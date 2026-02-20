mod chainlink;
mod exchanges;
mod logger;
mod macro_data;
mod polymarket;
mod presets;
mod rtds;
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
    rtds: RtdsConfig,
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
struct RtdsConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_rtds_ws")]
    ws_url: String,
    #[serde(default = "default_rtds_symbol")]
    symbol: String,
}

fn default_rtds_ws() -> String { "wss://ws-live-data.polymarket.com".into() }
fn default_rtds_symbol() -> String { "btc/usd".into() }

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
    #[serde(default = "default_min_bet_usdc")]
    min_bet_usdc: f64,
    #[serde(default = "default_min_shares")]
    min_shares: u64,
    min_edge_pct: f64,
    entry_seconds_before_end: u64,
    session_profit_target_usdc: f64,
    session_loss_limit_usdc: f64,
    #[serde(default = "default_fee_rate")]
    fee_rate: f64,
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
    #[serde(default = "default_order_type")]
    order_type: String,
    #[serde(default = "default_maker_timeout")]
    maker_timeout_s: u64,
    #[serde(default)]
    min_delta_pct: f64,
    #[serde(default = "default_max_spread")]
    max_spread: f64,
    #[serde(default = "default_kelly_fraction")]
    kelly_fraction: f64,
    #[serde(default = "default_initial_bankroll")]
    initial_bankroll_usdc: f64,
    #[serde(default)]
    always_trade: bool,
    #[serde(default = "default_vol_confidence_multiplier")]
    vol_confidence_multiplier: f64,
    #[serde(default)]
    min_payout_ratio: f64,
    #[serde(default)]
    min_book_imbalance: f64,
    #[serde(default)]
    max_vol_5min_pct: f64,
    #[serde(default)]
    min_ws_sources: u32,
    #[serde(default = "default_circuit_breaker_window")]
    circuit_breaker_window: usize,
    #[serde(default)]
    circuit_breaker_min_wr: f64,
    #[serde(default = "default_circuit_breaker_cooldown")]
    circuit_breaker_cooldown_s: u64,
    #[serde(default)]
    min_implied_prob: f64,
    #[serde(default)]
    max_consecutive_losses: u32,
    #[serde(default = "default_student_t_df")]
    student_t_df: f64,
}

fn default_min_bet_usdc() -> f64 { 1.0 }
fn default_min_shares() -> u64 { 5 }
fn default_fee_rate() -> f64 { 0.25 }
fn default_fee_rate_bps() -> u32 { 1000 }
fn default_min_market_price() -> f64 { 0.25 }
fn default_max_market_price() -> f64 { 0.75 }
fn default_vol_lookback() -> usize { 20 }
fn default_vol_pct() -> f64 { 0.12 }
fn default_order_type() -> String { "FOK".into() }
fn default_maker_timeout() -> u64 { 5 }
fn default_max_spread() -> f64 { 0.0 }
fn default_kelly_fraction() -> f64 { 0.10 }
fn default_initial_bankroll() -> f64 { 40.0 }
fn default_vol_confidence_multiplier() -> f64 { 4.0 }
fn default_circuit_breaker_window() -> usize { 0 }
fn default_circuit_breaker_cooldown() -> u64 { 1800 }
fn default_student_t_df() -> f64 { 4.0 }

impl From<StrategyToml> for strategy::StrategyConfig {
    fn from(s: StrategyToml) -> Self {
        Self {
            max_bet_usdc: s.max_bet_usdc,
            min_bet_usdc: s.min_bet_usdc,
            min_shares: s.min_shares,
            min_edge_pct: s.min_edge_pct,
            entry_seconds_before_end: s.entry_seconds_before_end,
            session_profit_target_usdc: s.session_profit_target_usdc,
            session_loss_limit_usdc: s.session_loss_limit_usdc,
            fee_rate: s.fee_rate,
            min_market_price: s.min_market_price,
            max_market_price: s.max_market_price,
            min_delta_pct: s.min_delta_pct,
            max_spread: s.max_spread,
            kelly_fraction: s.kelly_fraction,
            initial_bankroll_usdc: s.initial_bankroll_usdc,
            always_trade: s.always_trade,
            vol_confidence_multiplier: s.vol_confidence_multiplier,
            min_payout_ratio: s.min_payout_ratio,
            min_book_imbalance: s.min_book_imbalance,
            max_vol_5min_pct: s.max_vol_5min_pct,
            min_ws_sources: s.min_ws_sources,
            circuit_breaker_window: s.circuit_breaker_window,
            circuit_breaker_min_wr: s.circuit_breaker_min_wr,
            circuit_breaker_cooldown_s: s.circuit_breaker_cooldown_s,
            min_implied_prob: s.min_implied_prob,
            max_consecutive_losses: s.max_consecutive_losses,
            student_t_df: s.student_t_df,
        }
    }
}

fn load_config() -> Result<Config> {
    let text = std::fs::read_to_string("config.toml").context("config.toml introuvable")?;
    let mut config: Config = toml::from_str(&text).context("Erreur de parsing config.toml")?;

    // Override secrets from environment variables (takes precedence over config.toml)
    if let Ok(v) = std::env::var("POLY_API_KEY") { config.polymarket.api_key = v; }
    if let Ok(v) = std::env::var("POLY_API_SECRET") { config.polymarket.api_secret = v; }
    if let Ok(v) = std::env::var("POLY_PASSPHRASE") { config.polymarket.passphrase = v; }
    if let Ok(v) = std::env::var("POLY_PRIVATE_KEY") { config.polymarket.private_key = v; }

    Ok(config)
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
    let poll_ms_base = config.chainlink.poll_interval_ms;
    let poll_ms_ws = config.chainlink.poll_interval_ms_with_ws;
    let default_fee_rate_bps = config.strategy.fee_rate_bps;
    let feed: Address = config.chainlink.btc_usd_feed.parse().context("Invalid feed address")?;

    // Profile selection: --profile <name> or interactive menu or config.toml
    let profile_name = std::env::args()
        .skip_while(|a| a != "--profile")
        .nth(1);

    let (strat_config, dry_run, order_type, maker_timeout_s, vol_lookback, default_vol) =
        if let Some(ref name) = profile_name {
            let preset = presets::get(name)
                .unwrap_or_else(|| {
                    eprintln!("Profil inconnu: {name}. Disponibles: sniper, conviction, scalper, farm");
                    std::process::exit(1);
                });
            let dry_run = name == "farm";
            let order_type = match name.as_str() {
                "sniper" | "conviction" => "GTC".to_string(),
                _ => "FOK".to_string(),
            };
            let maker_timeout_s = if &order_type == "GTC" { 3 } else { config.strategy.maker_timeout_s };
            tracing::info!("Profil: {name}");
            (preset, dry_run, order_type, maker_timeout_s,
                config.strategy.vol_lookback_intervals, config.strategy.default_vol_pct)
        } else if let Some(name) = presets::interactive_menu() {
            let preset = presets::get(name).unwrap();
            let dry_run = name == "farm";
            let order_type = match name {
                "sniper" | "conviction" => "GTC".to_string(),
                _ => "FOK".to_string(),
            };
            let maker_timeout_s = if &order_type == "GTC" { 3 } else { config.strategy.maker_timeout_s };
            tracing::info!("Profil: {name}");
            (preset, dry_run, order_type, maker_timeout_s,
                config.strategy.vol_lookback_intervals, config.strategy.default_vol_pct)
        } else {
            let dry_run = config.strategy.dry_run;
            let order_type = config.strategy.order_type.clone();
            let maker_timeout_s = config.strategy.maker_timeout_s;
            let vol_lookback = config.strategy.vol_lookback_intervals;
            let default_vol = config.strategy.default_vol_pct;
            let strat_config = strategy::StrategyConfig::from(config.strategy);
            (strat_config, dry_run, order_type, maker_timeout_s, vol_lookback, default_vol)
        };

    let mut strat_config = strat_config;

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

    // RTDS feed (Polymarket settlement price, optionnel)
    let rtds_feed = if config.rtds.enabled {
        let rf = rtds::RtdsFeed::start(&config.rtds.ws_url, &config.rtds.symbol).await;
        tracing::info!("RTDS feed démarré ({} / {})", config.rtds.ws_url, config.rtds.symbol);
        Some(rf)
    } else {
        None
    };

    // Poll Chainlink moins souvent si les exchanges WS sont actifs (fallback only)
    let poll_ms = if exchange_feed.is_some() || rtds_feed.is_some() { poll_ms_ws } else { poll_ms_base };

    tracing::info!("poly5m — Bot d'arbitrage Polymarket 5min BTC{}{}{}",
        if dry_run { " [DRY-RUN]" } else { "" },
        if rtds_feed.is_some() { " [RTDS]" } else { "" },
        if exchange_feed.is_some() { " [WS]" } else { "" });
    tracing::info!("Config: max_bet=${} min_edge={}% entry={}s | {} RPCs | poll={}ms | order_type={}",
        strat_config.max_bet_usdc, strat_config.min_edge_pct,
        strat_config.entry_seconds_before_end, providers.len(), poll_ms, order_type);

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

    let macro_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let mut session = strategy::Session::new(strat_config.initial_bankroll_usdc);
    let mut vol_tracker = strategy::VolTracker::new(vol_lookback, default_vol);
    let mut interval = time::interval(Duration::from_millis(poll_ms));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    let mut current_window = 0u64;
    let mut start_price = 0.0f64;
    let mut traded_this_window = false;
    let mut cached_market: Option<polymarket::Market> = None;
    let mut pending_bet: Option<PendingBet> = None;
    let mut last_mid = 0.0f64;
    let mut skip_reason = String::from("startup");
    let mut macro_ctx = macro_data::MacroData::default();
    let mut window_ticks = strategy::WindowTicks::new();
    let mut calibrator = strategy::Calibrator::new(200);

    // Load saved calibration if available (not when using a preset)
    if profile_name.is_none() {
        if let Ok(content) = std::fs::read_to_string("calibration.json") {
            if let Ok(cal_data) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(mult) = cal_data["vol_confidence_multiplier"].as_f64() {
                    tracing::info!("Loaded calibration: vcm={mult:.2} (brier={:.4})",
                        cal_data["brier_score"].as_f64().unwrap_or(0.0));
                    strat_config.vol_confidence_multiplier = mult;
                }
            }
        }
    }

    loop {
        interval.tick().await;

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let window = (now / 300) * 300;
        let window_end = window + 300;
        let remaining = window_end.saturating_sub(now);

        // Prix BTC : RTDS (settlement, primaire) > WS exchanges > Chainlink on-chain (fallback)
        let rtds_price = rtds_feed.as_ref().and_then(|rf| rf.latest());
        let ws_agg = exchange_feed.as_ref().map(|ef| ef.latest());
        let ws_price = ws_agg.filter(|a| a.num_sources > 0).map(|a| a.median_price);
        let num_ws = ws_agg.map_or(0, |a| a.num_sources);

        let current_btc = if let Some(p) = rtds_price {
            p
        } else if let Some(p) = ws_price {
            p
        } else {
            // Fallback Chainlink on-chain
            match fetch_racing(&providers, feed).await {
                Ok(p) if now <= p.updated_at + 3700 => p.price_usd,
                Ok(p) => {
                    tracing::warn!("No RTDS/WS, Chainlink stale ({}s ago)", now - p.updated_at);
                    continue;
                }
                Err(e) => {
                    tracing::warn!("No price: RTDS/WS offline, CL error: {e:#}");
                    continue;
                }
            }
        };

        let price_source = if rtds_price.is_some() { "RTDS" } else if ws_price.is_some() { "WS" } else { "CL" };

        window_ticks.tick(current_btc, now * 1000);

        // Nouvel intervalle 5min — résoudre le bet précédent
        if window != current_window {
            // Log skip si le window précédent n'a pas donné de trade
            if current_window > 0 && !traded_this_window {
                if let Some(ref mut csv) = csv {
                    csv.log_skip(now, current_window, start_price, current_btc, last_mid, num_ws, price_source, vol_tracker.current_vol(), &macro_ctx, &skip_reason);
                }
            }

            if let Some(bet) = pending_bet.take() {
                resolve_pending_bet(bet, current_btc, now, current_window,
                    &mut session, &mut csv, &mut strat_config, &mut calibrator);
            }

            // Enregistrer le mouvement de l'intervalle précédent pour la vol dynamique
            if current_window > 0 && start_price > 0.0 {
                vol_tracker.record_move(start_price, current_btc);
            }

            current_window = window;
            traded_this_window = false;
            start_price = current_btc;
            window_ticks.clear();
            // Pre-fetch market for the new window (saves ~200ms during entry)
            cached_market = if let Some(ref poly) = poly {
                match poly.find_5min_btc_market(window).await {
                    Ok(m) => {
                        tracing::debug!("Pre-fetched market for window {window}: {}", m.question);
                        Some(m)
                    }
                    Err(e) => {
                        tracing::debug!("Pre-fetch failed (will retry): {e:#}");
                        None // fetch_market_data() will retry during entry window
                    }
                }
            } else {
                None
            };
            last_mid = 0.0;
            skip_reason = String::from("no_entry");
            macro_ctx = macro_data::fetch(&macro_http).await;
            let src = if rtds_price.is_some() { "RTDS" } else if ws_price.is_some() { "WS" } else { "CL" };
            tracing::info!("--- Nouvel intervalle 5min (window={window}) | BTC: ${:.2} ({src}, {num_ws} src) | vol: {:.3}% | 1h: {:.2}% | 24h: {:.2}% | fund: {:.6} ---",
                start_price, vol_tracker.current_vol(), macro_ctx.btc_1h_pct, macro_ctx.btc_24h_pct, macro_ctx.funding_rate);

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

        // Circuit breaker — skip trading during cooldown
        if session.is_circuit_broken(now) {
            if skip_reason == "no_entry" {
                skip_reason = String::from("circuit_breaker");
            }
            continue;
        }

        // Fetch Chainlink independently for divergence check (even if WS is primary)
        let cl_price = match fetch_racing(&providers, feed).await {
            Ok(p) if now <= p.updated_at + 3700 => Some(p.price_usd),
            Ok(_) => None,
            Err(_) => None,
        };

        // Fenêtre d'entrée : fetch marché + midpoint + book + fee rate (parallèle)
        let market_data = if let Some(ref poly) = poly {
            match fetch_market_data(poly, &mut cached_market, current_window, default_fee_rate_bps).await {
                Ok(data) => data,
                Err(reason) => {
                    skip_reason = reason;
                    continue;
                }
            }
        } else {
            MarketData {
                market: polymarket::Market {
                    condition_id: String::new(),
                    token_id_yes: String::new(),
                    token_id_no: String::new(),
                    question: String::new(),
                },
                mid_price: 0.50,
                fee_rate_bps: default_fee_rate_bps,
                book: polymarket::BookData::default(),
            }
        };

        let market_up_price = market_data.mid_price;
        let fee_rate_bps = market_data.fee_rate_bps;
        let spread_book = market_data.book;

        last_mid = market_up_price;

        tracing::debug!(
            "Fee check: API bps={} | calc={:.4}% | mid={:.4}",
            fee_rate_bps, strategy::dynamic_fee(market_up_price, strat_config.fee_rate) * 100.0, market_up_price
        );

        // evaluate() : RTDS for probability model (settlement price), CL/WS for divergence
        let ctx = strategy::TradeContext {
            start_price,
            chainlink_price: cl_price.unwrap_or(current_btc),
            exchange_price: ws_price,
            rtds_price,
            market_up_price,
            seconds_remaining: remaining,
            fee_rate: strat_config.fee_rate,
            vol_5min_pct: vol_tracker.current_vol(),
            spread: spread_book.spread,
            book_imbalance: spread_book.imbalance,
            num_ws_sources: u32::from(num_ws),
            micro_vol: window_ticks.micro_vol(),
            momentum_ratio: window_ticks.momentum_ratio(),
        };
        let signal = match strategy::evaluate(&ctx, &session, &strat_config) {
            Some(s) => s,
            None => {
                let price_change_pct = ((current_btc - start_price) / start_price * 100.0).abs();
                skip_reason = infer_skip_reason(
                    &strat_config, &session, market_up_price, price_change_pct,
                    vol_tracker.current_vol(), num_ws, spread_book.spread,
                    spread_book.imbalance, ws_price, cl_price, start_price,
                );
                continue;
            }
        };

        let (token_id, token_label) = if signal.side == polymarket::Side::Buy {
            (&market_data.market.token_id_yes, "YES")
        } else {
            (&market_data.market.token_id_no, "NO")
        };

        // Reuse spread_book if trading YES token, otherwise fetch NO token book
        let book = if signal.side == polymarket::Side::Buy {
            spread_book.clone() // YES token — already fetched for spread
        } else if let Some(ref poly) = poly {
            poly.get_book(token_id).await.unwrap_or_default()
        } else {
            polymarket::BookData::default()
        };

        // Maker pricing for GTC: bid + 25% of spread (better than best_ask)
        // Taker (FOK): use best_ask as usual
        let entry_price = if order_type == "GTC" && book.best_bid > 0.0 && book.best_ask > 0.0 {
            let spread = book.best_ask - book.best_bid;
            if spread >= 0.02 {
                let maker_price = book.best_bid + spread * 0.25;
                (maker_price * 100.0).round() / 100.0
            } else {
                (book.best_bid + 0.01).min(book.best_ask)
            }
        } else if book.best_ask > 0.0 && book.best_ask <= 1.0 {
            book.best_ask
        } else {
            signal.price
        };

        let side_label = if signal.side == polymarket::Side::Buy { "BUY_UP" } else { "BUY_DOWN" };
        tracing::info!(
            "{}Placement ordre: {} {} ${:.2} @ {:.4} | BTC=${:.2} ({num_ws} src) | spread={:.4} imbal={:.2}",
            if dry_run { "[DRY-RUN] " } else { "" },
            side_label, token_label, signal.size_usdc, entry_price, current_btc,
            book.spread, book.imbalance,
        );
        if let Some(ref mut csv) = csv {
            csv.log_trade(
                now, current_window, start_price, current_btc,
                market_up_price, signal.implied_p_up, side_label, token_label,
                signal.edge_brut_pct, signal.edge_pct, signal.fee_pct,
                signal.size_usdc, entry_price,
                0, if dry_run { "dry_run" } else if order_type == "GTC" { "GTC_filled" } else { "FOK_filled" },
                remaining, num_ws, price_source, vol_tracker.current_vol(),
                &macro_ctx, book.spread, book.bid_depth_usdc, book.ask_depth_usdc,
                book.imbalance, book.best_bid, book.best_ask,
                book.num_bid_levels, book.num_ask_levels,
                window_ticks.micro_vol(), window_ticks.momentum_ratio(),
                window_ticks.sign_changes(), window_ticks.max_drawdown_bps(),
                window_ticks.time_at_extreme_s(start_price), window_ticks.ticks_count(),
                session.pnl_usdc, session.trades, session.win_rate() * 100.0,
                session.consecutive_wins, session.session_drawdown_pct(),
            );
        }

        if dry_run {
            pending_bet = Some(PendingBet {
                start_price,
                side: signal.side,
                size_usdc: signal.size_usdc,
                entry_price,
                fee_pct: signal.fee_pct,
            });
            traded_this_window = true;
        } else if let Some(ref poly) = poly {
            if let Some(bet) = execute_order(
                poly, token_id, &signal, entry_price, start_price,
                fee_rate_bps, &order_type, maker_timeout_s,
            ).await {
                pending_bet = Some(bet);
            } else {
                let reason = if order_type == "GTC" { "gtc_not_filled" } else { "fok_rejected" };
                tracing::warn!("Ordre {reason} — loggé comme skip");
                if let Some(ref mut csv) = csv {
                    csv.log_skip(now, current_window, start_price, current_btc,
                        market_up_price, num_ws, price_source, vol_tracker.current_vol(), &macro_ctx, reason);
                }
            }
            traded_this_window = true;
        }
    }

    // Résumé de session
    tracing::info!("=== SESSION TERMINÉE ===");
    tracing::info!("Trades: {} | Wins: {} | WR: {:.0}% | PnL: ${:.2}",
        session.trades, session.wins, session.win_rate() * 100.0, session.pnl_usdc);

    Ok(())
}

struct PendingBet {
    start_price: f64,
    side: polymarket::Side,
    size_usdc: f64,
    entry_price: f64,
    fee_pct: f64,
}

/// Resolve whether the 5-min window outcome is UP.
/// Polymarket rule: end_price >= start_price → UP wins (equality = UP).
fn resolve_up(start_price: f64, end_price: f64) -> bool {
    end_price >= start_price
}

/// Compute PnL for a resolved bet. Taker fee is paid at entry regardless of outcome.
fn compute_pnl(won: bool, size: f64, price: f64, fee_pct: f64) -> f64 {
    let fee_cost = size * fee_pct / 100.0;
    if won {
        size * (1.0 / price - 1.0) - fee_cost
    } else {
        -size - fee_cost
    }
}

/// Infer why evaluate() returned None (mirrors evaluate() filter order for CSV logging).
#[allow(clippy::too_many_arguments)]
fn infer_skip_reason(
    config: &strategy::StrategyConfig,
    session: &strategy::Session,
    market_up_price: f64,
    price_change_pct: f64,
    vol: f64,
    num_ws: u8,
    spread: f64,
    imbalance: f64,
    ws_price: Option<f64>,
    cl_price: Option<f64>,
    start_price: f64,
) -> String {
    if config.max_consecutive_losses > 0 && session.consecutive_losses >= config.max_consecutive_losses {
        format!("consec_loss>={}", config.max_consecutive_losses)
    } else if config.min_ws_sources > 0 && u32::from(num_ws) < config.min_ws_sources {
        format!("ws_src<{}", config.min_ws_sources)
    } else if config.max_vol_5min_pct > 0.0 && vol > config.max_vol_5min_pct {
        format!("vol>{:.3}%", config.max_vol_5min_pct)
    } else if market_up_price < config.min_market_price {
        format!("mid<{:.2}", config.min_market_price)
    } else if market_up_price > config.max_market_price {
        format!("mid>{:.2}", config.max_market_price)
    } else if config.min_payout_ratio > 0.0 && {
        let mp = if price_change_pct >= 0.0 { market_up_price } else { 1.0 - market_up_price };
        (1.0 - mp) / mp < config.min_payout_ratio
    } {
        format!("payout<{:.2}", config.min_payout_ratio)
    } else if config.min_book_imbalance > 0.0 && imbalance < config.min_book_imbalance {
        format!("imbal<{:.2}", config.min_book_imbalance)
    } else if ws_price.is_some() && cl_price.is_some() && {
        let cl = cl_price.unwrap();
        let cl_move = ((cl - start_price) / start_price).abs();
        cl_move > 0.0001 && (cl > start_price) != (ws_price.unwrap() > start_price)
    } {
        String::from("divergence")
    } else if config.min_delta_pct > 0.0 && price_change_pct < config.min_delta_pct {
        format!("delta<{:.4}%", config.min_delta_pct)
    } else if config.max_spread > 0.0 && spread > config.max_spread {
        format!("spread>{:.2}", config.max_spread)
    } else {
        String::from("no_edge")
    }
}

/// Execute a FOK or GTC order via the Polymarket API.
/// Returns Some(PendingBet) if the order was filled, None if it failed or wasn't filled.
#[allow(clippy::too_many_arguments)]
async fn execute_order(
    poly: &polymarket::PolymarketClient,
    token_id: &str,
    signal: &strategy::Signal,
    entry_price: f64,
    start_price: f64,
    fee_rate_bps: u32,
    order_type: &str,
    maker_timeout_s: u64,
) -> Option<PendingBet> {
    let order_t = Instant::now();
    let mut gtc_immediate_fill = false;

    let order_result = if order_type == "GTC" {
        match poly.place_limit_order(token_id, polymarket::Side::Buy, signal.size_usdc, entry_price, fee_rate_bps).await {
            Ok(result) => {
                let order_ms = order_t.elapsed().as_millis();
                tracing::info!("[MAKER] Ordre GTC placé: {} en {}ms", result.order_id, order_ms);
                if result.status == "matched" {
                    gtc_immediate_fill = true;
                    Some(result)
                } else {
                    tokio::time::sleep(Duration::from_secs(maker_timeout_s)).await;
                    let filled = match poly.get_order_status(&result.order_id).await {
                        Ok(status) => {
                            tracing::info!("[MAKER] Order {} status after {}s: {}", result.order_id, maker_timeout_s, status);
                            status == "matched"
                        }
                        Err(e) => {
                            tracing::warn!("[MAKER] Status check failed: {e:#}");
                            false
                        }
                    };
                    if filled {
                        Some(result)
                    } else {
                        tracing::info!("[MAKER] Not filled — cancelling {}", result.order_id);
                        if let Err(e) = poly.cancel_order(&result.order_id).await {
                            tracing::warn!("[MAKER] Cancel failed: {e:#}");
                        }
                        None
                    }
                }
            }
            Err(e) => {
                tracing::error!("Erreur ordre GTC: {e:#} ({}ms)", order_t.elapsed().as_millis());
                None
            }
        }
    } else {
        match poly.place_order(token_id, polymarket::Side::Buy, signal.size_usdc, entry_price, fee_rate_bps).await {
            Ok(result) => {
                let order_ms = order_t.elapsed().as_millis();
                tracing::info!("Ordre FOK: {} (status: {}) en {}ms", result.order_id, result.status, order_ms);
                if result.status == "matched" { Some(result) } else { None }
            }
            Err(e) => {
                tracing::error!("Erreur ordre FOK: {e:#} ({}ms)", order_t.elapsed().as_millis());
                None
            }
        }
    };

    order_result.map(|_| {
        let pays_taker_fee = order_type != "GTC" || gtc_immediate_fill;
        PendingBet {
            start_price,
            side: signal.side,
            size_usdc: signal.size_usdc,
            entry_price,
            fee_pct: if pays_taker_fee { signal.fee_pct } else { 0.0 },
        }
    })
}

/// Resolve a pending bet: compute PnL, log to CSV, calibrate, check circuit breaker.
#[allow(clippy::too_many_arguments)]
fn resolve_pending_bet(
    bet: PendingBet,
    current_btc: f64,
    now: u64,
    current_window: u64,
    session: &mut strategy::Session,
    csv: &mut Option<logger::CsvLogger>,
    strat_config: &mut strategy::StrategyConfig,
    calibrator: &mut strategy::Calibrator,
) {
    let went_up = resolve_up(bet.start_price, current_btc);
    let won = (went_up && bet.side == polymarket::Side::Buy)
        || (!went_up && bet.side != polymarket::Side::Buy);
    let pnl = compute_pnl(won, bet.size_usdc, bet.entry_price, bet.fee_pct);
    session.record_trade(pnl);
    let result_str = if won { "WIN" } else { "LOSS" };
    tracing::info!(
        "Résolution: {} | PnL: ${:.2} | Session: ${:.2} | WR: {:.0}%",
        result_str, pnl, session.pnl_usdc, session.win_rate() * 100.0,
    );
    if let Some(ref mut csv) = csv {
        csv.log_resolution(now, current_window, bet.start_price, current_btc,
            result_str, pnl, session.pnl_usdc, session.trades, session.win_rate() * 100.0,
            session.consecutive_wins, session.session_drawdown_pct());
    }

    // Auto-calibration: record prediction and check if recalibration is due
    let predicted_p = if bet.side == polymarket::Side::Buy {
        1.0 - bet.entry_price
    } else {
        bet.entry_price
    };
    calibrator.record(predicted_p, won);

    if calibrator.should_recalibrate() {
        if let Some((new_mult, brier)) = calibrator.recalibrate() {
            tracing::info!("Auto-calibration: vcm {:.2} → {:.2} (brier={:.4})",
                strat_config.vol_confidence_multiplier, new_mult, brier);
            strat_config.vol_confidence_multiplier = new_mult;
            let cal_json = serde_json::json!({
                "vol_confidence_multiplier": new_mult,
                "brier_score": brier,
                "trades_used": 200,
                "timestamp": now,
            });
            if let Err(e) = std::fs::write("calibration.json", cal_json.to_string()) {
                tracing::warn!("Failed to save calibration.json: {e}");
            }
        }
    }

    session.check_circuit_breaker(
        strat_config.circuit_breaker_window,
        strat_config.circuit_breaker_min_wr,
        strat_config.circuit_breaker_cooldown_s,
        now,
    );
}

struct MarketData {
    market: polymarket::Market,
    mid_price: f64,
    fee_rate_bps: u32,
    book: polymarket::BookData,
}

/// Fetch market, midpoint, book, and fee rate from Polymarket (with cache).
/// Midpoint, book, and fee rate are fetched concurrently via `tokio::join!`.
async fn fetch_market_data(
    poly: &polymarket::PolymarketClient,
    cached_market: &mut Option<polymarket::Market>,
    current_window: u64,
    default_fee_rate_bps: u32,
) -> Result<MarketData, String> {
    if cached_market.is_none() {
        match poly.find_5min_btc_market(current_window).await {
            Ok(m) => *cached_market = Some(m),
            Err(e) => {
                tracing::warn!("Marché introuvable: {e:#}");
                return Err(format!("market_err:{e}"));
            }
        }
    }
    let market = cached_market.as_ref().unwrap();
    let token = market.token_id_yes.as_str();

    // Parallel fetch: midpoint + book + fee rate
    let (mid_res, book_res, fee_res) = tokio::join!(
        poly.get_midpoint(token),
        poly.get_book(token),
        poly.get_fee_rate(token),
    );

    let mid = match mid_res {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Midpoint error: {e:#}");
            return Err(format!("midpoint_err:{e}"));
        }
    };
    let book = book_res.unwrap_or_default();
    let fee = fee_res.unwrap_or(default_fee_rate_bps);

    Ok(MarketData {
        market: market.clone(),
        mid_price: mid,
        fee_rate_bps: fee,
        book,
    })
}

/// Fetch prix Chainlink en RACING parallèle — prend la 1ère réponse.
async fn fetch_racing(
    providers: &[impl alloy::providers::Provider],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_up_price_higher() {
        assert!(resolve_up(100_000.0, 100_001.0));
    }

    #[test]
    fn resolve_up_price_equal() {
        // Polymarket rule: equality → UP wins
        assert!(resolve_up(100_000.0, 100_000.0));
    }

    #[test]
    fn resolve_up_price_lower() {
        assert!(!resolve_up(100_000.0, 99_999.0));
    }

    #[test]
    fn pnl_win_subtracts_fee() {
        let size = 2.0;
        let price = 0.65;
        let fee_pct = 0.52;
        let pnl = compute_pnl(true, size, price, fee_pct);
        let expected = size * (1.0 / price - 1.0) - size * 0.0052;
        assert!((pnl - expected).abs() < 1e-10, "pnl={pnl} expected={expected}");
    }

    #[test]
    fn pnl_loss_includes_fee() {
        let size = 2.0;
        let price = 0.65;
        let fee_pct = 0.52;
        let pnl = compute_pnl(false, size, price, fee_pct);
        let expected = -size - size * 0.0052;
        assert!((pnl - expected).abs() < 1e-10, "loss pnl should be -size-fee, got {pnl}");
    }

    #[test]
    fn pnl_win_zero_fee_maker() {
        // GTC maker case: fee_pct = 0.0
        let size = 2.0;
        let price = 0.65;
        let pnl = compute_pnl(true, size, price, 0.0);
        let expected = size * (1.0 / price - 1.0); // ~1.077
        assert!((pnl - expected).abs() < 1e-10, "pnl={pnl} expected={expected}");
    }

    #[test]
    fn pnl_loss_zero_fee_maker() {
        // GTC maker case: fee_pct = 0.0
        let size = 2.0;
        let pnl = compute_pnl(false, size, 0.65, 0.0);
        assert!((pnl - (-size)).abs() < 1e-10, "loss pnl should be -size, got {pnl}");
    }
}
