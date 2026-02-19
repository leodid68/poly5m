use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const STALE_MS: u64 = 5_000;
const PING_INTERVAL_S: u64 = 5;

#[derive(Debug, Clone, Copy, Default)]
pub struct RtdsPrice {
    pub price: f64,
    pub timestamp_ms: u64,
}

/// Feed RTDS Polymarket — prix Chainlink Data Streams utilisé pour le settlement.
pub struct RtdsFeed {
    rx: watch::Receiver<Option<RtdsPrice>>,
}

impl RtdsFeed {
    /// Démarre la connexion WS au RTDS en background. Non-bloquant.
    pub async fn start(ws_url: &str, symbol: &str) -> Self {
        let (tx, rx) = watch::channel(None);
        tokio::spawn(ws_loop(ws_url.to_string(), symbol.to_string(), tx));
        Self { rx }
    }

    /// Dernier prix RTDS si frais (<5s), sinon None.
    pub fn latest(&self) -> Option<f64> {
        let slot = (*self.rx.borrow())?;
        let now = now_ms();
        if now.saturating_sub(slot.timestamp_ms) < STALE_MS {
            Some(slot.price)
        } else {
            None
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Boucle de reconnexion automatique.
/// Exponential backoff: 2s → 4s → 8s → … → 30s max.
async fn ws_loop(url: String, symbol: String, tx: watch::Sender<Option<RtdsPrice>>) {
    let mut backoff_s = 2u64;
    let mut reconnects = 0u32;
    loop {
        let result = run_rtds(&url, &symbol, &tx).await;
        let _ = tx.send(None); // Clear on disconnect

        match result {
            Ok(()) => {
                tracing::info!("[RTDS] WS disconnected cleanly");
                backoff_s = 2;
            }
            Err(e) => {
                reconnects += 1;
                tracing::warn!("[RTDS] WS error (reconnect #{}): {e:#}", reconnects);
                backoff_s = (backoff_s * 2).min(30);
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff_s)).await;
    }
}

#[derive(Deserialize)]
struct RtdsMsg {
    topic: Option<String>,
    #[serde(rename = "type")]
    msg_type: Option<String>,
    payload: Option<RtdsPayload>,
}

#[derive(Deserialize)]
struct RtdsPayload {
    symbol: String,
    timestamp: u64,
    value: f64,
}

async fn run_rtds(url: &str, symbol: &str, tx: &watch::Sender<Option<RtdsPrice>>) -> Result<()> {
    let (mut ws, _) = connect_async(url).await.context("RTDS connect")?;
    tracing::info!("[RTDS] WS connected to {url}");

    // Subscribe to crypto_prices_chainlink for the symbol
    let sub = serde_json::json!({
        "action": "subscribe",
        "subscriptions": [{
            "topic": "crypto_prices_chainlink",
            "type": "*",
            "filters": {"symbol": symbol}
        }]
    });
    ws.send(Message::Text(sub.to_string().into())).await?;
    tracing::info!("[RTDS] Subscribed to crypto_prices_chainlink/{symbol}");

    let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_S));
    ping_interval.tick().await; // skip immediate first tick

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(ref text))) => {
                        if let Ok(m) = serde_json::from_str::<RtdsMsg>(text) {
                            if m.msg_type.as_deref() == Some("subscribed") {
                                tracing::info!("[RTDS] Subscription confirmed: {}",
                                    m.topic.as_deref().unwrap_or("unknown"));
                            }
                            if m.topic.as_deref() == Some("crypto_prices_chainlink")
                                && m.msg_type.as_deref() == Some("update")
                            {
                                if let Some(p) = m.payload {
                                    if p.symbol == symbol && p.value > 0.0 {
                                        let _ = tx.send(Some(RtdsPrice {
                                            price: p.value,
                                            timestamp_ms: now_ms(),
                                        }));
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
                ws.send(Message::Ping(vec![].into())).await.context("RTDS ping failed")?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rtds_update() {
        let json = r#"{"topic":"crypto_prices_chainlink","type":"update","timestamp":1753314064237,"payload":{"symbol":"btc/usd","timestamp":1753314064213,"value":97150.50}}"#;
        let m: RtdsMsg = serde_json::from_str(json).unwrap();
        assert_eq!(m.topic.unwrap(), "crypto_prices_chainlink");
        assert_eq!(m.msg_type.unwrap(), "update");
        let p = m.payload.unwrap();
        assert_eq!(p.symbol, "btc/usd");
        assert_eq!(p.timestamp, 1753314064213);
        assert!((p.value - 97150.50).abs() < 0.01);
    }

    #[test]
    fn parse_rtds_non_update_ignored() {
        // Subscription ack messages don't have payload
        let json = r#"{"topic":"crypto_prices_chainlink","type":"subscribed"}"#;
        let m: RtdsMsg = serde_json::from_str(json).unwrap();
        assert_eq!(m.msg_type.unwrap(), "subscribed");
        assert!(m.payload.is_none());
    }

    #[test]
    fn rtds_feed_returns_fresh_price() {
        let (tx, rx) = watch::channel(Some(RtdsPrice {
            price: 97150.0,
            timestamp_ms: now_ms(),
        }));
        std::mem::forget(tx);
        let feed = RtdsFeed { rx };
        let price = feed.latest();
        assert!(price.is_some());
        assert!((price.unwrap() - 97150.0).abs() < 0.01);
    }

    #[test]
    fn rtds_feed_returns_none_when_stale() {
        let (tx, rx) = watch::channel(Some(RtdsPrice {
            price: 97150.0,
            timestamp_ms: now_ms().saturating_sub(10_000), // 10s old
        }));
        std::mem::forget(tx);
        let feed = RtdsFeed { rx };
        assert!(feed.latest().is_none());
    }

    #[test]
    fn rtds_feed_returns_none_when_empty() {
        let (tx, rx) = watch::channel(None);
        std::mem::forget(tx);
        let feed = RtdsFeed { rx };
        assert!(feed.latest().is_none());
    }
}
