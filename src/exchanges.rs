use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const STALE_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct AggregatedPrice {
    pub median_price: f64,
    pub num_sources: u8,
    pub last_update_ms: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    price: f64,
    updated_ms: u64,
}

type Slots = Arc<RwLock<[Option<Slot>; 3]>>;

pub struct ExchangeFeed {
    slots: Slots,
}

impl ExchangeFeed {
    /// Démarre les 3 connexions WS en background. Non-bloquant.
    pub async fn start(binance: &str, coinbase: &str, kraken: &str) -> Self {
        let slots: Slots = Arc::new(RwLock::new([None; 3]));
        tokio::spawn(ws_loop(Exchange::Binance, binance.to_string(), Arc::clone(&slots)));
        tokio::spawn(ws_loop(Exchange::Coinbase, coinbase.to_string(), Arc::clone(&slots)));
        tokio::spawn(ws_loop(Exchange::Kraken, kraken.to_string(), Arc::clone(&slots)));
        Self { slots }
    }

    /// Dernier prix agrégé (médiane des sources fraîches, non-bloquant).
    pub fn latest(&self) -> AggregatedPrice {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        let mut prices = Vec::with_capacity(3);
        let mut last = 0u64;
        for slot in slots.iter().flatten() {
            if now.saturating_sub(slot.updated_ms) < STALE_MS {
                prices.push(slot.price);
                last = last.max(slot.updated_ms);
            }
        }
        if prices.is_empty() {
            return AggregatedPrice::default();
        }
        prices.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
        let median = match prices.len() {
            1 => prices[0],
            2 => (prices[0] + prices[1]) / 2.0,
            _ => prices[1],
        };
        AggregatedPrice { median_price: median, num_sources: prices.len() as u8, last_update_ms: last }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

#[derive(Clone, Copy)]
enum Exchange { Binance, Coinbase, Kraken }

impl Exchange {
    fn label(self) -> &'static str {
        match self { Exchange::Binance => "Binance", Exchange::Coinbase => "Coinbase", Exchange::Kraken => "Kraken" }
    }
    fn idx(self) -> usize {
        match self { Exchange::Binance => 0, Exchange::Coinbase => 1, Exchange::Kraken => 2 }
    }
}

/// Boucle de reconnexion automatique pour chaque exchange.
async fn ws_loop(ex: Exchange, url: String, slots: Slots) {
    loop {
        if let Err(e) = match ex {
            Exchange::Binance => run_binance(&url, &slots).await,
            Exchange::Coinbase => run_coinbase(&url, &slots).await,
            Exchange::Kraken => run_kraken(&url, &slots).await,
        } {
            tracing::warn!("[{}] WS error: {e:#}", ex.label());
        }
        // Clear slot on disconnect
        slots.write().unwrap_or_else(|e| e.into_inner())[ex.idx()] = None;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// --- Binance: wss://stream.binance.com:9443/ws/btcusdt@trade ---

#[derive(Deserialize)]
struct BinanceTrade { p: String, #[serde(rename = "T")] ts: u64 }

async fn run_binance(url: &str, slots: &Slots) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("connect")?;
    tracing::info!("[Binance] WS connected");
    while let Some(msg) = ws.next().await {
        if let Message::Text(ref text) = msg? {
            if let Ok(t) = serde_json::from_str::<BinanceTrade>(text) {
                if let Ok(p) = t.p.parse::<f64>() {
                    slots.write().unwrap_or_else(|e| e.into_inner())[0] = Some(Slot { price: p, updated_ms: t.ts });
                }
            }
        }
    }
    Ok(())
}

// --- Coinbase: wss://ws-feed.exchange.coinbase.com ---

#[derive(Deserialize)]
struct CoinbaseTicker {
    #[serde(rename = "type")]
    msg_type: String,
    price: Option<String>,
}

async fn run_coinbase(url: &str, slots: &Slots) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("connect")?;
    tracing::info!("[Coinbase] WS connected");
    let sub = serde_json::json!({
        "type": "subscribe",
        "channels": ["ticker"],
        "product_ids": ["BTC-USD"]
    });
    ws.send(Message::Text(sub.to_string().into())).await?;
    while let Some(msg) = ws.next().await {
        if let Message::Text(ref text) = msg? {
            if let Ok(t) = serde_json::from_str::<CoinbaseTicker>(text) {
                if t.msg_type == "ticker" {
                    if let Some(ref ps) = t.price {
                        if let Ok(p) = ps.parse::<f64>() {
                            slots.write().unwrap_or_else(|e| e.into_inner())[1] = Some(Slot { price: p, updated_ms: now_ms() });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// --- Kraken v2: wss://ws.kraken.com/v2 ---

#[derive(Deserialize)]
struct KrakenMsg { channel: Option<String>, data: Option<Vec<KrakenTicker>> }

#[derive(Deserialize)]
struct KrakenTicker { last: Option<f64> }

async fn run_kraken(url: &str, slots: &Slots) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("connect")?;
    tracing::info!("[Kraken] WS connected");
    let sub = serde_json::json!({
        "method": "subscribe",
        "params": { "channel": "ticker", "symbol": ["BTC/USD"] }
    });
    ws.send(Message::Text(sub.to_string().into())).await?;
    while let Some(msg) = ws.next().await {
        if let Message::Text(ref text) = msg? {
            if let Ok(m) = serde_json::from_str::<KrakenMsg>(text) {
                if m.channel.as_deref() == Some("ticker") {
                    if let Some(ref data) = m.data {
                        if let Some(t) = data.first() {
                            if let Some(p) = t.last {
                                slots.write().unwrap_or_else(|e| e.into_inner())[2] = Some(Slot { price: p, updated_ms: now_ms() });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_three_sources() {
        let slots: Slots = Arc::new(RwLock::new([None; 3]));
        let now = now_ms();
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            s[0] = Some(Slot { price: 97100.0, updated_ms: now });
            s[1] = Some(Slot { price: 97200.0, updated_ms: now });
            s[2] = Some(Slot { price: 97150.0, updated_ms: now });
        }
        let feed = ExchangeFeed { slots };
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 3);
        assert!((agg.median_price - 97150.0).abs() < 0.01);
    }

    #[test]
    fn median_two_sources() {
        let slots: Slots = Arc::new(RwLock::new([None; 3]));
        let now = now_ms();
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            s[0] = Some(Slot { price: 97100.0, updated_ms: now });
            s[1] = Some(Slot { price: 97200.0, updated_ms: now });
        }
        let feed = ExchangeFeed { slots };
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 2);
        assert!((agg.median_price - 97150.0).abs() < 0.01);
    }

    #[test]
    fn median_one_source() {
        let slots: Slots = Arc::new(RwLock::new([None; 3]));
        let now = now_ms();
        slots.write().unwrap_or_else(|e| e.into_inner())[0] = Some(Slot { price: 97100.0, updated_ms: now });
        let feed = ExchangeFeed { slots };
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 1);
        assert!((agg.median_price - 97100.0).abs() < 0.01);
    }

    #[test]
    fn stale_sources_excluded() {
        let slots: Slots = Arc::new(RwLock::new([None; 3]));
        let now = now_ms();
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            s[0] = Some(Slot { price: 97100.0, updated_ms: now });
            s[1] = Some(Slot { price: 97200.0, updated_ms: now.saturating_sub(10_000) });
        }
        let feed = ExchangeFeed { slots };
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 1);
    }

    #[test]
    fn no_sources_returns_default() {
        let slots: Slots = Arc::new(RwLock::new([None; 3]));
        let feed = ExchangeFeed { slots };
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 0);
        assert_eq!(agg.median_price, 0.0);
    }

    #[test]
    fn parse_binance_trade() {
        let json = r#"{"e":"trade","E":1234567890123,"s":"BTCUSDT","t":12345,"p":"97150.50","q":"0.001","b":88,"a":50,"T":1234567890123,"m":true,"M":true}"#;
        let t: BinanceTrade = serde_json::from_str(json).unwrap();
        assert_eq!(t.p, "97150.50");
        assert_eq!(t.ts, 1234567890123);
    }

    #[test]
    fn parse_coinbase_ticker() {
        let json = r#"{"type":"ticker","sequence":123,"product_id":"BTC-USD","price":"97150.50","open_24h":"96000","volume_24h":"1234","time":"2026-02-18T12:00:00.000000Z"}"#;
        let t: CoinbaseTicker = serde_json::from_str(json).unwrap();
        assert_eq!(t.msg_type, "ticker");
        assert_eq!(t.price.unwrap(), "97150.50");
    }

    #[test]
    fn parse_kraken_ticker() {
        let json = r#"{"channel":"ticker","type":"update","data":[{"symbol":"BTC/USD","bid":97100.0,"ask":97200.0,"last":97150.0,"volume":1234.5}]}"#;
        let m: KrakenMsg = serde_json::from_str(json).unwrap();
        assert_eq!(m.channel.unwrap(), "ticker");
        assert_eq!(m.data.unwrap()[0].last.unwrap(), 97150.0);
    }
}
