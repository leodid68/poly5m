use serde::Deserialize;

#[derive(Debug, Clone, Default)]
pub struct MacroData {
    pub btc_1h_pct: f64,
    pub btc_24h_pct: f64,
    pub btc_24h_vol_m: f64, // volume 24h en millions USD
    pub funding_rate: f64,  // taux de financement perps
}

#[derive(Deserialize)]
struct TickerResp {
    #[serde(rename = "priceChangePercent")]
    price_change_percent: String,
}

#[derive(Deserialize)]
struct Ticker24hResp {
    #[serde(rename = "priceChangePercent")]
    price_change_percent: String,
    #[serde(rename = "quoteVolume")]
    quote_volume: String,
}

#[derive(Deserialize)]
struct FundingRateResp {
    #[serde(rename = "fundingRate")]
    funding_rate: String,
}

/// Fetch macro data depuis Binance (public, no auth).
/// Ne fail jamais — retourne defaults en cas d'erreur.
pub async fn fetch(http: &reqwest::Client) -> MacroData {
    let (r1h, r24h, rfund) = tokio::join!(
        fetch_1h(http),
        fetch_24h(http),
        fetch_funding(http),
    );

    let mut data = MacroData::default();
    if let Some(pct) = r1h {
        data.btc_1h_pct = pct;
    }
    if let Some((pct, vol)) = r24h {
        data.btc_24h_pct = pct;
        data.btc_24h_vol_m = vol / 1_000_000.0;
    }
    if let Some(rate) = rfund {
        data.funding_rate = rate;
    }
    data
}

async fn fetch_1h(http: &reqwest::Client) -> Option<f64> {
    let resp = http.get("https://api.binance.com/api/v3/ticker")
        .query(&[("symbol", "BTCUSDT"), ("windowSize", "1h")])
        .send().await.ok()?;
    let t: TickerResp = resp.json().await.ok()?;
    t.price_change_percent.parse().ok()
}

async fn fetch_24h(http: &reqwest::Client) -> Option<(f64, f64)> {
    let resp = http.get("https://api.binance.com/api/v3/ticker/24hr")
        .query(&[("symbol", "BTCUSDT")])
        .send().await.ok()?;
    let t: Ticker24hResp = resp.json().await.ok()?;
    Some((t.price_change_percent.parse().ok()?, t.quote_volume.parse().ok()?))
}

async fn fetch_funding(http: &reqwest::Client) -> Option<f64> {
    let resp = http.get("https://fapi.binance.com/fapi/v1/fundingRate")
        .query(&[("symbol", "BTCUSDT"), ("limit", "1")])
        .send().await.ok()?;
    let rates: Vec<FundingRateResp> = resp.json().await.ok()?;
    rates.first()?.funding_rate.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ticker_resp() {
        let json = r#"{"priceChangePercent": "-1.234"}"#;
        let t: TickerResp = serde_json::from_str(json).unwrap();
        assert_eq!(t.price_change_percent, "-1.234");
    }

    #[test]
    fn parse_ticker_24h_resp() {
        let json = r#"{"priceChangePercent": "2.5", "quoteVolume": "45000000000.12"}"#;
        let t: Ticker24hResp = serde_json::from_str(json).unwrap();
        assert_eq!(t.price_change_percent, "2.5");
        let vol: f64 = t.quote_volume.parse().unwrap();
        assert!(vol > 4e10);
    }

    #[test]
    fn parse_funding_rate_resp() {
        let json = r#"[{"fundingRate": "0.00012345", "fundingTime": 1700000000}]"#;
        let rates: Vec<FundingRateResp> = serde_json::from_str(json).unwrap();
        let rate: f64 = rates[0].funding_rate.parse().unwrap();
        assert!((rate - 0.00012345).abs() < 1e-10);
    }

    #[test]
    fn default_macro_data() {
        let d = MacroData::default();
        assert_eq!(d.btc_1h_pct, 0.0);
        assert_eq!(d.btc_24h_pct, 0.0);
        assert_eq!(d.btc_24h_vol_m, 0.0);
        assert_eq!(d.funding_rate, 0.0);
    }
}
