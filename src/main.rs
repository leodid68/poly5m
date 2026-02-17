mod chainlink;
mod polymarket;
mod strategy;

use alloy::primitives::Address;
use alloy::providers::ProviderBuilder;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time;

// --- Config ---

#[derive(Deserialize)]
struct Config {
    chainlink: ChainlinkConfig,
    polymarket: PolymarketConfig,
    strategy: StrategyToml,
}

#[derive(Deserialize)]
struct ChainlinkConfig {
    rpc_urls: Vec<String>,
    btc_usd_feed: String,
    poll_interval_ms: u64,
}

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
    #[serde(default)]
    dry_run: bool,
}

impl From<StrategyToml> for strategy::StrategyConfig {
    fn from(s: StrategyToml) -> Self {
        Self {
            max_bet_usdc: s.max_bet_usdc,
            min_edge_pct: s.min_edge_pct,
            entry_seconds_before_end: s.entry_seconds_before_end,
            session_profit_target_usdc: s.session_profit_target_usdc,
            session_loss_limit_usdc: s.session_loss_limit_usdc,
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
    let strat_config = strategy::StrategyConfig::from(config.strategy);
    let feed: Address = config.chainlink.btc_usd_feed.parse().context("Invalid feed address")?;

    // Providers Chainlink (un par RPC URL, pour racing)
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
            tracing::warn!("[DRY-RUN] Utilisation d'un prix marché simulé (0.50)");
            None
        }
        Err(e) => return Err(e),
    };

    tracing::info!("poly5m — Bot d'arbitrage Polymarket 5min BTC{}",
        if dry_run { " [DRY-RUN]" } else { "" });
    tracing::info!("Config: max_bet=${} min_edge={}% entry={}s avant fin",
        strat_config.max_bet_usdc, strat_config.min_edge_pct, strat_config.entry_seconds_before_end);

    let mut session = strategy::Session::default();
    let mut interval = time::interval(Duration::from_millis(config.chainlink.poll_interval_ms));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    let mut current_window = 0u64;
    let mut start_price = 0.0f64;
    let mut traded_this_window = false;

    loop {
        interval.tick().await;

        // Sample time once per iteration (avoid TOCTOU)
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let window = (now / 300) * 300;
        let window_end = window + 300;
        let remaining = window_end.saturating_sub(now);

        // Détecter un nouvel intervalle 5min
        if window != current_window {
            current_window = window;
            traded_this_window = false;
            start_price = 0.0;
            tracing::info!("--- Nouvel intervalle 5min (window={window}) ---");
        }

        // Fetch prix Chainlink (fallback séquentiel)
        let price = match fetch_with_fallback(&providers, feed).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Chainlink fetch error: {e:#}");
                continue;
            }
        };

        // Staleness check — BTC/USD feed heartbeat = 3600s, seuil = 3700s
        if now > price.updated_at + 3700 {
            tracing::warn!("Chainlink stale: updated {}s ago", now - price.updated_at);
            continue;
        }

        // Enregistrer le prix de début d'intervalle
        if start_price == 0.0 {
            start_price = price.price_usd;
            tracing::info!("Prix début intervalle: ${:.2}", start_price);
        }

        // Skip si déjà tradé ce window
        if traded_this_window {
            continue;
        }

        // Pas dans la fenêtre d'entrée ? Skip
        if remaining > strat_config.entry_seconds_before_end {
            continue;
        }

        // Session limits — graceful shutdown
        if session.pnl_usdc >= strat_config.session_profit_target_usdc
            || session.pnl_usdc <= -strat_config.session_loss_limit_usdc
        {
            tracing::info!("Session limit atteint (PnL: ${:.2}). Arrêt.", session.pnl_usdc);
            break;
        }

        // Chercher le marché et le prix mid
        let (market, market_up_price) = if let Some(ref poly) = poly {
            let market = match poly.find_5min_btc_market().await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("Marché introuvable: {e:#}");
                    continue;
                }
            };
            let mid = match poly.get_midpoint(&market.token_id_yes).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Midpoint error: {e:#}");
                    continue;
                }
            };
            (Some(market), mid)
        } else {
            // Dry-run sans credentials: prix marché simulé à 0.50
            (None, 0.50)
        };

        // Évaluer la stratégie
        let signal = match strategy::evaluate(
            start_price,
            price.price_usd,
            market_up_price,
            remaining,
            &session,
            &strat_config,
        ) {
            Some(s) => s,
            None => continue,
        };

        // Déterminer le token à acheter
        let dummy_token = "dry-run-token".to_string();
        let token_id = match &market {
            Some(m) if signal.side == polymarket::Side::Buy => &m.token_id_yes,
            Some(m) => &m.token_id_no,
            None => &dummy_token,
        };

        let side_label = if signal.side == polymarket::Side::Buy { "BUY UP" } else { "BUY DOWN" };
        tracing::info!(
            "{}Placement ordre: {} {} ${:.2} @ {:.4}",
            if dry_run { "[DRY-RUN] " } else { "" },
            side_label, token_id, signal.size_usdc, signal.price,
        );

        if dry_run {
            // Dry-run: assume matched, track PnL potentiel
            let potential_pnl = signal.size_usdc * (1.0 / signal.price - 1.0);
            session.record_trade(potential_pnl);
            tracing::info!(
                "[DRY-RUN] Trade #{} | {} | PnL potentiel: ${:.2} | Session: ${:.2} | WR: {:.0}%",
                session.trades, side_label, potential_pnl, session.pnl_usdc, session.win_rate() * 100.0,
            );
            traded_this_window = true;
        } else if let Some(ref poly) = poly {
            // Toujours BUY — on varie le token (UP ou DOWN)
            match poly.place_order(token_id, polymarket::Side::Buy, signal.size_usdc, signal.price).await {
                Ok(result) => {
                    tracing::info!("Ordre placé: {} (status: {})", result.order_id, result.status);
                    if result.status == "matched" {
                        let potential_pnl = signal.size_usdc * (1.0 / signal.price - 1.0);
                        session.record_trade(potential_pnl);
                        tracing::info!(
                            "Trade #{} | PnL potentiel: ${:.2} | Session: ${:.2} | WR: {:.0}%",
                            session.trades, potential_pnl, session.pnl_usdc, session.win_rate() * 100.0,
                        );
                    } else {
                        tracing::warn!("Ordre non matched (status: {}), aucune position", result.status);
                    }
                    traded_this_window = true;
                }
                Err(e) => {
                    tracing::error!("Erreur placement ordre: {e:#}");
                }
            }
        }
    }

    Ok(())
}

/// Fetch prix Chainlink avec fallback séquentiel sur les providers.
async fn fetch_with_fallback(
    providers: &[impl alloy::providers::Provider + Sync],
    feed: Address,
) -> Result<chainlink::PriceData> {
    let mut last_err = None;
    for provider in providers {
        match chainlink::fetch_price(provider, feed).await {
            Ok(data) => return Ok(data),
            Err(e) => {
                tracing::debug!("RPC provider failed: {e:#}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("No providers")))
}
