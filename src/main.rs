mod chainlink;
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
    let poll_ms = config.chainlink.poll_interval_ms;
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

    tracing::info!("poly5m — Bot d'arbitrage Polymarket 5min BTC{}",
        if dry_run { " [DRY-RUN]" } else { "" });
    tracing::info!("Config: max_bet=${} min_edge={}% entry={}s avant fin | {} RPCs",
        strat_config.max_bet_usdc, strat_config.min_edge_pct,
        strat_config.entry_seconds_before_end, providers.len());

    // --- Pre-warm : établit TCP+TLS vers tous les endpoints ---
    tracing::info!("Pre-warming connections...");
    let warmup_t = Instant::now();
    let _ = fetch_racing(&providers, feed).await; // Chainlink RPC
    if let Some(ref p) = poly {
        let _ = p.get_midpoint("0").await; // Polymarket CLOB (force TCP+TLS)
    }
    tracing::info!("Pre-warm done in {}ms", warmup_t.elapsed().as_millis());

    let mut session = strategy::Session::default();
    let mut interval = time::interval(Duration::from_millis(poll_ms));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    let mut current_window = 0u64;
    let mut start_price = 0.0f64;
    let mut traded_this_window = false;
    // Cache marché pour le window courant (évite un appel Gamma par tick)
    let mut cached_market: Option<polymarket::Market> = None;

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
            cached_market = None;
            tracing::info!("--- Nouvel intervalle 5min (window={window}) ---");
        }

        // Fetch prix Chainlink (RACING parallèle sur tous les RPCs)
        let fetch_t = Instant::now();
        let price = match fetch_racing(&providers, feed).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Chainlink fetch error: {e:#}");
                continue;
            }
        };
        let fetch_ms = fetch_t.elapsed().as_millis();

        // Staleness check — BTC/USD feed heartbeat = 3600s, seuil = 3700s
        if now > price.updated_at + 3700 {
            tracing::warn!("Chainlink stale: updated {}s ago", now - price.updated_at);
            continue;
        }

        // Enregistrer le prix de début d'intervalle
        if start_price == 0.0 {
            start_price = price.price_usd;
            tracing::info!("Prix début intervalle: ${:.2} (fetch: {}ms)", start_price, fetch_ms);
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

        // --- Fenêtre d'entrée : fetch marché (cache) + midpoint en parallèle ---
        let (market, market_up_price) = if let Some(ref poly) = poly {
            // Cache le marché pour tout le window (ne change pas intra-window)
            if cached_market.is_none() {
                match poly.find_5min_btc_market().await {
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
            (Some(market.clone()), mid)
        } else {
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
            "{}Placement ordre: {} ${:.2} @ {:.4} (fetch: {}ms)",
            if dry_run { "[DRY-RUN] " } else { "" },
            side_label, signal.size_usdc, signal.price, fetch_ms,
        );

        if dry_run {
            let potential_pnl = signal.size_usdc * (1.0 / signal.price - 1.0);
            session.record_trade(potential_pnl);
            tracing::info!(
                "[DRY-RUN] Trade #{} | {} | PnL: ${:.2} | Session: ${:.2} | WR: {:.0}%",
                session.trades, side_label, potential_pnl, session.pnl_usdc, session.win_rate() * 100.0,
            );
            traded_this_window = true;
        } else if let Some(ref poly) = poly {
            let order_t = Instant::now();
            match poly.place_order(token_id, polymarket::Side::Buy, signal.size_usdc, signal.price).await {
                Ok(result) => {
                    let order_ms = order_t.elapsed().as_millis();
                    tracing::info!("Ordre placé: {} (status: {}) en {}ms",
                        result.order_id, result.status, order_ms);
                    if result.status == "matched" {
                        let potential_pnl = signal.size_usdc * (1.0 / signal.price - 1.0);
                        session.record_trade(potential_pnl);
                        tracing::info!(
                            "Trade #{} | PnL: ${:.2} | Session: ${:.2} | WR: {:.0}%",
                            session.trades, potential_pnl, session.pnl_usdc, session.win_rate() * 100.0,
                        );
                    } else {
                        tracing::warn!("Ordre non matched (status: {})", result.status);
                    }
                    traded_this_window = true;
                }
                Err(e) => {
                    tracing::error!("Erreur ordre: {e:#} ({}ms)", order_t.elapsed().as_millis());
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
