mod chainlink;
mod exchanges;
mod logger;
mod polymarket;
mod strategy;

use alloy::primitives::Address;
use alloy::providers::ProviderBuilder;
use anyhow::{Context, Result};
use futures::future::select_ok;
use serde::Deserialize;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time;

// --- Config ---

#[derive(Deserialize)]
struct Config {
    chainlink: ChainlinkConfig,
    polymarket: PolymarketConfig,
    strategy: StrategyToml,
    #[serde(default)]
    exchanges: ExchangesConfig,
    #[serde(default)]
    logging: LoggingConfig,
}

#[derive(Deserialize, Default)]
struct LoggingConfig {
    #[serde(default)]
    csv_path: String,
}

#[derive(Deserialize)]
struct ChainlinkConfig {
    rpc_urls: Vec<String>,
    btc_usd_feed: String,
    poll_interval_ms: u64,
    #[serde(default = "default_poll_interval_ws")]
    poll_interval_ms_with_ws: u64,
}

fn default_poll_interval_ws() -> u64 { 1000 }

#[derive(Deserialize, Default)]
struct ExchangesConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_binance_ws")]
    binance_ws: String,
    #[serde(default = "default_coinbase_ws")]
    coinbase_ws: String,
    #[serde(default = "default_kraken_ws")]
    kraken_ws: String,
}

fn default_binance_ws() -> String { "wss://stream.binance.com:9443/ws/btcusdt@trade".into() }
fn default_coinbase_ws() -> String { "wss://ws-feed.exchange.coinbase.com".into() }
fn default_kraken_ws() -> String { "wss://ws.kraken.com/v2".into() }

#[derive(Deserialize)]
struct PolymarketConfig {
    api_key: String,
    api_secret: String,
    passphrase: String,
    private_key: String,
}

#[derive(Deserialize)]
struct StrategyToml {
    max_bet_usdc: f64,
    min_edge_pct: f64,
    entry_seconds_before_end: u64,
    session_profit_target_usdc: f64,
    session_loss_limit_usdc: f64,
    #[serde(default = "default_fee_rate_bps")]
    fee_rate_bps: u32,
    #[serde(default = "default_min_market_price")]
    min_market_price: f64,
    #[serde(default = "default_max_market_price")]
    max_market_price: f64,
    #[serde(default)]
    dry_run: bool,
    #[serde(default = "default_vol_lookback")]
    vol_lookback_intervals: usize,
    #[serde(default = "default_vol_pct")]
    default_vol_pct: f64,
}

fn default_fee_rate_bps() -> u32 { 1000 }
fn default_min_market_price() -> f64 { 0.15 }
fn default_max_market_price() -> f64 { 0.85 }
fn default_vol_lookback() -> usize { 20 }
fn default_vol_pct() -> f64 { 0.12 }

impl From<StrategyToml> for strategy::StrategyConfig {
    fn from(s: StrategyToml) -> Self {
        Self {
            max_bet_usdc: s.max_bet_usdc,
            min_edge_pct: s.min_edge_pct,
            entry_seconds_before_end: s.entry_seconds_before_end,
            session_profit_target_usdc: s.session_profit_target_usdc,
            session_loss_limit_usdc: s.session_loss_limit_usdc,
            fee_rate_bps: s.fee_rate_bps,
            min_market_price: s.min_market_price,
            max_market_price: s.max_market_price,
        }
    }
}

fn load_config() -> Result<Config> {
    let text = std::fs::read_to_string("config.toml").context("config.toml introuvable")?;
    toml::from_str(&text).context("Erreur de parsing config.toml")
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("poly5m=info".parse().unwrap()),
        )
        .init();

    let config = load_config()?;
    let dry_run = config.strategy.dry_run;
    let poll_ms_base = config.chainlink.poll_interval_ms;
    let poll_ms_ws = config.chainlink.poll_interval_ms_with_ws;
    let vol_lookback = config.strategy.vol_lookback_intervals;
    let default_vol = config.strategy.default_vol_pct;
    let strat_config = strategy::StrategyConfig::from(config.strategy);
    let feed: Address = config.chainlink.btc_usd_feed.parse().context("Invalid feed address")?;

    // Providers Chainlink — timeouts serrés pour le racing
    let providers = config.chainlink.rpc_urls.iter()
        .map(|url| {
            let url: reqwest::Url = url.parse().context("Invalid RPC URL in config")?;
            Ok(ProviderBuilder::new().connect_http(url))
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!providers.is_empty(), "Au moins un rpc_url requis");

    // Client Polymarket (optionnel en dry-run si credentials manquants)
    let poly = match polymarket::PolymarketClient::new(
        config.polymarket.api_key,
        config.polymarket.api_secret,
        config.polymarket.passphrase,
        &config.polymarket.private_key,
    ) {
        Ok(c) => Some(c),
        Err(e) if dry_run => {
            tracing::warn!("[DRY-RUN] Client Polymarket non initialisé: {e:#}");
            None
        }
        Err(e) => return Err(e),
    };

    // Exchange WebSocket feed (optionnel)
    let exchange_feed = if config.exchanges.enabled {
        let ef = exchanges::ExchangeFeed::start(
            &config.exchanges.binance_ws,
            &config.exchanges.coinbase_ws,
            &config.exchanges.kraken_ws,
        ).await;
        tracing::info!("Exchange WS feed démarré (Binance + Coinbase + Kraken)");
        Some(ef)
    } else {
        None
    };

    // Poll Chainlink moins souvent si les exchanges WS sont actifs (fallback only)
    let poll_ms = if exchange_feed.is_some() { poll_ms_ws } else { poll_ms_base };

    tracing::info!("poly5m — Bot d'arbitrage Polymarket 5min BTC{}{}",
        if dry_run { " [DRY-RUN]" } else { "" },
        if exchange_feed.is_some() { " [WS]" } else { "" });
    tracing::info!("Config: max_bet=${} min_edge={}% entry={}s avant fin | {} RPCs | poll={}ms",
        strat_config.max_bet_usdc, strat_config.min_edge_pct,
        strat_config.entry_seconds_before_end, providers.len(), poll_ms);

    // --- Pre-warm : établit TCP+TLS vers tous les endpoints ---
    tracing::info!("Pre-warming connections...");
    let warmup_t = Instant::now();
    let _ = fetch_racing(&providers, feed).await; // Chainlink RPC
    if let Some(ref p) = poly {
        let _ = p.get_midpoint("0").await; // Polymarket CLOB (force TCP+TLS)
    }
    tracing::info!("Pre-warm done in {}ms", warmup_t.elapsed().as_millis());

    // CSV logger (optionnel)
    let mut csv = if !config.logging.csv_path.is_empty() {
        let l = logger::CsvLogger::new(&config.logging.csv_path)?;
        tracing::info!("CSV logging → {}", config.logging.csv_path);
        Some(l)
    } else {
        None
    };

    let mut session = strategy::Session::default();
    let mut vol_tracker = strategy::VolTracker::new(vol_lookback, default_vol);
    let mut interval = time::interval(Duration::from_millis(poll_ms));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    let mut current_window = 0u64;
    let mut start_price = 0.0f64;
    let mut traded_this_window = false;
    let mut cached_market: Option<polymarket::Market> = None;
    let mut pending_bet: Option<(f64, polymarket::Side, f64, f64)> = None; // (start_price, side, size, price)

    loop {
        interval.tick().await;

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let window = (now / 300) * 300;
        let window_end = window + 300;
        let remaining = window_end.saturating_sub(now);

        // Fetch Chainlink AVANT détection window (nécessaire pour résolution)
        let fetch_t = Instant::now();
        let price = match fetch_racing(&providers, feed).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Chainlink fetch error: {e:#}");
                continue;
            }
        };
        let fetch_ms = fetch_t.elapsed().as_millis();

        if now > price.updated_at + 300 {
            tracing::warn!("Chainlink stale: updated {}s ago", now - price.updated_at);
            continue;
        }

        // Nouvel intervalle 5min — résoudre le bet précédent
        if window != current_window {
            if let Some((bet_start, bet_side, bet_size, bet_price)) = pending_bet.take() {
                let went_up = price.price_usd > bet_start;
                let won = (went_up && bet_side == polymarket::Side::Buy)
                    || (!went_up && bet_side != polymarket::Side::Buy);
                let pnl = if won {
                    bet_size * (1.0 / bet_price - 1.0)
                } else {
                    -bet_size
                };
                session.record_trade(pnl);
                let result_str = if won { "WIN" } else { "LOSS" };
                tracing::info!(
                    "Résolution: {} | PnL: ${:.2} | Session: ${:.2} | WR: {:.0}%",
                    result_str, pnl, session.pnl_usdc, session.win_rate() * 100.0,
                );
                if let Some(ref mut csv) = csv {
                    csv.log_resolution(now, current_window, result_str, pnl, session.pnl_usdc);
                }
            }

            // Enregistrer le mouvement de l'intervalle précédent pour la vol dynamique
            if current_window > 0 && start_price > 0.0 {
                vol_tracker.record_move(start_price, price.price_usd);
            }

            current_window = window;
            traded_this_window = false;
            start_price = price.price_usd;
            cached_market = None;
            tracing::info!("--- Nouvel intervalle 5min (window={window}) | BTC: ${:.2} | vol: {:.3}% (fetch: {}ms) ---",
                start_price, vol_tracker.current_vol(), fetch_ms);

            if session.pnl_usdc >= strat_config.session_profit_target_usdc
                || session.pnl_usdc <= -strat_config.session_loss_limit_usdc
            {
                tracing::info!("Session limit atteint (PnL: ${:.2}). Arrêt.", session.pnl_usdc);
                break;
            }
            continue;
        }

        if traded_this_window { continue; }
        if remaining > strat_config.entry_seconds_before_end { continue; }

        // Fenêtre d'entrée : fetch marché (cache) + midpoint + fee rate
        let (market, market_up_price, fee_rate_bps) = if let Some(ref poly) = poly {
            if cached_market.is_none() {
                match poly.find_5min_btc_market(current_window).await {
                    Ok(m) => cached_market = Some(m),
                    Err(e) => {
                        tracing::warn!("Marché introuvable: {e:#}");
                        continue;
                    }
                }
            }
            let market = cached_market.as_ref().unwrap();
            let mid = match poly.get_midpoint(&market.token_id_yes).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Midpoint error: {e:#}");
                    continue;
                }
            };
            let fee = poly.get_fee_rate(&market.token_id_yes).await
                .unwrap_or(strat_config.fee_rate_bps);
            (Some(market.clone()), mid, fee)
        } else {
            (None, 0.50, strat_config.fee_rate_bps)
        };

        // Prix exchange WS (plus frais que Chainlink) si disponible et non-stale
        let ex_price = exchange_feed.as_ref()
            .map(|ef| ef.latest())
            .filter(|agg| agg.num_sources > 0)
            .map(|agg| agg.median_price);

        let signal = match strategy::evaluate(
            start_price, price.price_usd, ex_price, market_up_price,
            remaining, &session, &strat_config, fee_rate_bps, vol_tracker.current_vol(),
        ) {
            Some(s) => s,
            None => continue,
        };

        let dummy_token = "dry-run-token".to_string();
        let token_id = match &market {
            Some(m) if signal.side == polymarket::Side::Buy => &m.token_id_yes,
            Some(m) => &m.token_id_no,
            None => &dummy_token,
        };

        let side_label = if signal.side == polymarket::Side::Buy { "BUY_UP" } else { "BUY_DOWN" };
        tracing::info!(
            "{}Placement ordre: {} ${:.2} @ {:.4} (fetch: {}ms)",
            if dry_run { "[DRY-RUN] " } else { "" },
            side_label, signal.size_usdc, signal.price, fetch_ms,
        );
        if let Some(ref mut csv) = csv {
            csv.log_trade(
                now, current_window, price.price_usd, ex_price,
                market_up_price, side_label, signal.edge_pct, signal.size_usdc, signal.price,
            );
        }

        if dry_run {
            pending_bet = Some((start_price, signal.side, signal.size_usdc, signal.price));
            traded_this_window = true;
        } else if let Some(ref poly) = poly {
            let order_t = Instant::now();
            // Toujours Side::Buy : on achète le token UP ou DOWN, jamais de vente
            match poly.place_order(token_id, polymarket::Side::Buy, signal.size_usdc, signal.price).await {
                Ok(result) => {
                    let order_ms = order_t.elapsed().as_millis();
                    tracing::info!("Ordre placé: {} (status: {}) en {}ms",
                        result.order_id, result.status, order_ms);
                    if result.status == "matched" {
                        pending_bet = Some((start_price, signal.side, signal.size_usdc, signal.price));
                    } else {
                        tracing::warn!("Ordre non matched (status: {})", result.status);
                    }
                    traded_this_window = true;
                }
                Err(e) => {
                    tracing::error!("Erreur ordre: {e:#} ({}ms)", order_t.elapsed().as_millis());
                    traded_this_window = true; // éviter de spammer l'API en boucle
                }
            }
        }
    }

    Ok(())
}

/// Fetch prix Chainlink en RACING parallèle — prend la 1ère réponse.
async fn fetch_racing(
    providers: &[impl alloy::providers::Provider + Sync],
    feed: Address,
) -> Result<chainlink::PriceData> {
    if providers.len() == 1 {
        return chainlink::fetch_price(&providers[0], feed).await;
    }
    let futures: Vec<_> = providers.iter()
        .map(|p| Box::pin(chainlink::fetch_price(p, feed)))
        .collect();
    let (result, _remaining) = select_ok(futures).await
        .map_err(|e| anyhow::anyhow!("All RPC providers failed: {e}"))?;
    Ok(result)
}
