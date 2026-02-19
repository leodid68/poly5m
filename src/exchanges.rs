use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const STALE_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
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

pub struct ExchangeFeed {
    rx: [watch::Receiver<Option<Slot>>; 3],
}

impl ExchangeFeed {
    /// Démarre les 3 connexions WS en background. Non-bloquant.
    pub async fn start(binance: &str, coinbase: &str, kraken: &str) -> Self {
        let (tx0, rx0) = watch::channel(None);
        let (tx1, rx1) = watch::channel(None);
        let (tx2, rx2) = watch::channel(None);
        tokio::spawn(ws_loop(Exchange::Binance, binance.to_string(), tx0));
        tokio::spawn(ws_loop(Exchange::Coinbase, coinbase.to_string(), tx1));
        tokio::spawn(ws_loop(Exchange::Kraken, kraken.to_string(), tx2));
        Self { rx: [rx0, rx1, rx2] }
    }

    /// Dernier prix agrégé (médiane des sources fraîches, non-bloquant).
    pub fn latest(&self) -> AggregatedPrice {
        let now = now_ms();
        let mut prices = Vec::with_capacity(3);
        let mut last = 0u64;
        for rx in &self.rx {
            if let Some(slot) = *rx.borrow() {
                if now.saturating_sub(slot.updated_ms) < STALE_MS {
                    prices.push(slot.price);
                    last = last.max(slot.updated_ms);
                }
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
}

/// Boucle de reconnexion automatique pour chaque exchange.
/// Exponential backoff: 2s → 4s → 8s → … → 30s max. Reset on clean disconnect.
async fn ws_loop(ex: Exchange, url: String, tx: watch::Sender<Option<Slot>>) {
    let mut backoff_s = 2u64;
    let mut reconnects = 0u32;
    loop {
        let result = match ex {
            Exchange::Binance => run_binance(&url, &tx).await,
            Exchange::Coinbase => run_coinbase(&url, &tx).await,
            Exchange::Kraken => run_kraken(&url, &tx).await,
        };
        // Clear slot on disconnect
        let _ = tx.send(None);

        match result {
            Ok(()) => {
                // Clean disconnect — reset backoff
                tracing::info!("[{}] WS disconnected cleanly", ex.label());
                backoff_s = 2;
            }
            Err(e) => {
                reconnects += 1;
                tracing::warn!("[{}] WS error (reconnect #{}): {e:#}", ex.label(), reconnects);
                backoff_s = (backoff_s * 2).min(30);
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff_s)).await;
    }
}

// --- Binance: wss://stream.binance.com:9443/ws/btcusdt@trade ---

#[derive(Deserialize)]
struct BinanceTrade { p: String, #[serde(rename = "T")] ts: u64 }

async fn run_binance(url: &str, tx: &watch::Sender<Option<Slot>>) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("connect")?;
    tracing::info!("[Binance] WS connected");
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    ping_interval.tick().await; // skip immediate first tick

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(ref text))) => {
                        if let Ok(t) = serde_json::from_str::<BinanceTrade>(text) {
                            if let Ok(p) = t.p.parse::<f64>() {
                                let _ = tx.send(Some(Slot { price: p, updated_ms: t.ts }));
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws.send(Message::Pong(data)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }
            _ = ping_interval.tick() => {
                ws.send(Message::Ping(vec![].into())).await.context("ping failed")?;
            }
        }
    }
}

// --- Coinbase: wss://ws-feed.exchange.coinbase.com ---

#[derive(Deserialize)]
struct CoinbaseTicker {
    #[serde(rename = "type")]
    msg_type: String,
    price: Option<String>,
}

async fn run_coinbase(url: &str, tx: &watch::Sender<Option<Slot>>) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("connect")?;
    tracing::info!("[Coinbase] WS connected");
    let sub = serde_json::json!({
        "type": "subscribe",
        "channels": ["ticker"],
        "product_ids": ["BTC-USD"]
    });
    ws.send(Message::Text(sub.to_string().into())).await?;
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    ping_interval.tick().await; // skip immediate first tick

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(ref text))) => {
                        if let Ok(t) = serde_json::from_str::<CoinbaseTicker>(text) {
                            if t.msg_type == "ticker" {
                                if let Some(ref ps) = t.price {
                                    if let Ok(p) = ps.parse::<f64>() {
                                        let _ = tx.send(Some(Slot { price: p, updated_ms: now_ms() }));
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws.send(Message::Pong(data)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }
            _ = ping_interval.tick() => {
                ws.send(Message::Ping(vec![].into())).await.context("ping failed")?;
            }
        }
    }
}

// --- Kraken v2: wss://ws.kraken.com/v2 ---

#[derive(Deserialize)]
struct KrakenMsg { channel: Option<String>, data: Option<Vec<KrakenTicker>> }

#[derive(Deserialize)]
struct KrakenTicker { last: Option<f64> }

async fn run_kraken(url: &str, tx: &watch::Sender<Option<Slot>>) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("connect")?;
    tracing::info!("[Kraken] WS connected");
    let sub = serde_json::json!({
        "method": "subscribe",
        "params": { "channel": "ticker", "symbol": ["BTC/USD"] }
    });
    ws.send(Message::Text(sub.to_string().into())).await?;
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    ping_interval.tick().await; // skip immediate first tick

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(ref text))) => {
                        if let Ok(m) = serde_json::from_str::<KrakenMsg>(text) {
                            if m.channel.as_deref() == Some("ticker") {
                                if let Some(ref data) = m.data {
                                    if let Some(t) = data.first() {
                                        if let Some(p) = t.last {
                                            let _ = tx.send(Some(Slot { price: p, updated_ms: now_ms() }));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws.send(Message::Pong(data)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }
            _ = ping_interval.tick() => {
                ws.send(Message::Ping(vec![].into())).await.context("ping failed")?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_feed(slots: [Option<Slot>; 3]) -> ExchangeFeed {
        let (tx0, rx0) = watch::channel(slots[0]);
        let (tx1, rx1) = watch::channel(slots[1]);
        let (tx2, rx2) = watch::channel(slots[2]);
        // Keep senders alive for the duration of the test
        std::mem::forget(tx0);
        std::mem::forget(tx1);
        std::mem::forget(tx2);
        ExchangeFeed { rx: [rx0, rx1, rx2] }
    }

    #[test]
    fn median_three_sources() {
        let now = now_ms();
        let feed = make_feed([
            Some(Slot { price: 97100.0, updated_ms: now }),
            Some(Slot { price: 97200.0, updated_ms: now }),
            Some(Slot { price: 97150.0, updated_ms: now }),
        ]);
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 3);
        assert!((agg.median_price - 97150.0).abs() < 0.01);
    }

    #[test]
    fn median_two_sources() {
        let now = now_ms();
        let feed = make_feed([
            Some(Slot { price: 97100.0, updated_ms: now }),
            Some(Slot { price: 97200.0, updated_ms: now }),
            None,
        ]);
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 2);
        assert!((agg.median_price - 97150.0).abs() < 0.01);
    }

    #[test]
    fn median_one_source() {
        let now = now_ms();
        let feed = make_feed([
            Some(Slot { price: 97100.0, updated_ms: now }),
            None,
            None,
        ]);
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 1);
        assert!((agg.median_price - 97100.0).abs() < 0.01);
    }

    #[test]
    fn stale_sources_excluded() {
        let now = now_ms();
        let feed = make_feed([
            Some(Slot { price: 97100.0, updated_ms: now }),
            Some(Slot { price: 97200.0, updated_ms: now.saturating_sub(10_000) }),
            None,
        ]);
        let agg = feed.latest();
        assert_eq!(agg.num_sources, 1);
    }

    #[test]
    fn no_sources_returns_default() {
        let feed = make_feed([None, None, None]);
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
