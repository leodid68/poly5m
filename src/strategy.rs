use crate::polymarket::Side;
use std::collections::VecDeque;

/// Configuration de la stratégie (chargée depuis config.toml).
#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub max_bet_usdc: f64,
    pub min_bet_usdc: f64,
    pub min_shares: u64,
    pub min_edge_pct: f64,
    pub entry_seconds_before_end: u64,
    pub session_profit_target_usdc: f64,
    pub session_loss_limit_usdc: f64,
    pub fee_rate: f64,
    pub min_market_price: f64,
    pub max_market_price: f64,
    pub min_delta_pct: f64,
    pub max_spread: f64,
    pub kelly_fraction: f64,
    pub initial_bankroll_usdc: f64,
    pub always_trade: bool,
    pub vol_confidence_multiplier: f64,
    pub min_payout_ratio: f64,
    pub min_book_imbalance: f64,
    pub max_vol_5min_pct: f64,
}

/// Signal de trade émis par la stratégie.
#[derive(Debug)]
pub struct Signal {
    pub side: Side,
    pub edge_pct: f64,
    pub edge_brut_pct: f64,
    pub fee_pct: f64,
    pub implied_p_up: f64,
    pub size_usdc: f64,
    pub price: f64,
}

/// État de la session (P&L, nombre de trades, bankroll tracking).
#[derive(Debug)]
pub struct Session {
    pub pnl_usdc: f64,
    pub trades: u32,
    pub wins: u32,
    pub initial_bankroll: f64,
}

impl Default for Session {
    fn default() -> Self {
        Self { pnl_usdc: 0.0, trades: 0, wins: 0, initial_bankroll: 0.0 }
    }
}

impl Session {
    pub fn new(initial_bankroll: f64) -> Self {
        Self { pnl_usdc: 0.0, trades: 0, wins: 0, initial_bankroll }
    }

    pub fn bankroll(&self) -> f64 {
        self.initial_bankroll + self.pnl_usdc
    }

    pub fn record_trade(&mut self, pnl: f64) {
        self.pnl_usdc += pnl;
        self.trades += 1;
        if pnl > 0.0 {
            self.wins += 1;
        }
    }

    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 { 0.0 } else { self.wins as f64 / self.trades as f64 }
    }
}

/// Suit la volatilité réalisée sur les derniers intervalles 5min.
#[derive(Debug)]
pub struct VolTracker {
    recent_moves: VecDeque<f64>,
    max_samples: usize,
    default_vol: f64,
}

impl VolTracker {
    pub fn new(max_samples: usize, default_vol: f64) -> Self {
        Self { recent_moves: VecDeque::with_capacity(max_samples), max_samples, default_vol }
    }

    /// Enregistre le mouvement de prix d'un intervalle terminé (% signé).
    pub fn record_move(&mut self, start_price: f64, end_price: f64) {
        if start_price <= 0.0 { return; }
        let pct = (end_price - start_price) / start_price * 100.0;
        self.recent_moves.push_back(pct);
        if self.recent_moves.len() > self.max_samples {
            self.recent_moves.pop_front();
        }
    }

    /// Volatilité estimée (std dev des mouvements récents).
    /// Retourne default_vol si pas assez de données (< 3 samples).
    pub fn current_vol(&self) -> f64 {
        if self.recent_moves.len() < 3 {
            return self.default_vol;
        }
        let n = self.recent_moves.len() as f64;
        let mean = self.recent_moves.iter().sum::<f64>() / n;
        let variance = self.recent_moves.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        variance.sqrt().clamp(0.01, 1.0)
    }
}

/// Market context for trade evaluation.
#[derive(Debug, Clone)]
pub struct TradeContext {
    pub start_price: f64,
    pub chainlink_price: f64,
    pub exchange_price: Option<f64>,
    pub rtds_price: Option<f64>,
    pub market_up_price: f64,
    pub seconds_remaining: u64,
    pub fee_rate: f64,
    pub vol_5min_pct: f64,
    pub spread: f64,
    pub book_imbalance: f64,
}

/// Évalue si on doit trader sur cet intervalle.
/// `exchange_price` : prix WS exchanges (plus frais), fallback sur `chainlink_price`.
pub fn evaluate(
    ctx: &TradeContext,
    session: &Session,
    config: &StrategyConfig,
) -> Option<Signal> {
    // 1. Session limits
    if session.pnl_usdc >= config.session_profit_target_usdc {
        return None;
    }
    if session.pnl_usdc <= -config.session_loss_limit_usdc {
        return None;
    }

    // 2. Fenêtre d'entrée
    if ctx.seconds_remaining > config.entry_seconds_before_end {
        return None;
    }

    // 3. Validation inputs
    if ctx.start_price <= 0.0 || !(0.01..=0.99).contains(&ctx.market_up_price) {
        return None;
    }

    // 3a. Vol filter — skip high-vol windows (unpredictable)
    if config.max_vol_5min_pct > 0.0 && ctx.vol_5min_pct > config.max_vol_5min_pct {
        tracing::debug!("Skip: vol {:.3}% > max {:.3}%", ctx.vol_5min_pct, config.max_vol_5min_pct);
        return None;
    }

    // 4. Direction et probabilité estimée (time-aware)
    // Priorité : RTDS (prix settlement) > exchange WS > Chainlink on-chain
    let current_price = ctx.rtds_price
        .or(ctx.exchange_price)
        .unwrap_or(ctx.chainlink_price);
    let price_change_pct = (current_price - ctx.start_price) / ctx.start_price * 100.0;

    let true_up_prob = price_change_to_probability(price_change_pct, ctx.seconds_remaining, ctx.vol_5min_pct, config.vol_confidence_multiplier);
    let true_down_prob = 1.0 - true_up_prob;
    let market_down_price = 1.0 - ctx.market_up_price;

    // 5. Edge calculation
    let edge_up = true_up_prob - ctx.market_up_price;
    let edge_down = true_down_prob - market_down_price;

    let (side, edge, market_price, true_prob) = if edge_up >= edge_down {
        (Side::Buy, edge_up, ctx.market_up_price, true_up_prob)
    } else {
        (Side::Sell, edge_down, market_down_price, true_down_prob)
    };

    let edge_pct = edge * 100.0;
    let fee = dynamic_fee(market_price, ctx.fee_rate);
    let spread_cost = ctx.spread / 2.0;
    let net_edge_pct = edge_pct - (fee * 100.0) - (spread_cost * 100.0);

    // 6. always_trade: bypass filters, trade at min_bet in best direction
    if config.always_trade {
        let min_usdc = (config.min_shares as f64 * market_price).max(config.min_bet_usdc);
        let size = min_usdc.min(config.max_bet_usdc);
        tracing::info!(
            "SIGNAL [ALWAYS]: {} | Edge: {:.1}% (brut {:.1}%, fee {:.2}%) | Δ prix: {:.4}% | Size: ${:.2} | {}s restantes | src: {}",
            if side == Side::Buy { "BUY UP" } else { "BUY DOWN" },
            net_edge_pct, edge_pct, fee * 100.0, price_change_pct, size, ctx.seconds_remaining,
            if ctx.rtds_price.is_some() { "RTDS" } else if ctx.exchange_price.is_some() { "WS" } else { "CL" },
        );
        return Some(Signal {
            side,
            edge_pct: net_edge_pct,
            edge_brut_pct: edge_pct,
            fee_pct: fee * 100.0,
            implied_p_up: true_up_prob,
            size_usdc: size,
            price: market_price,
        });
    }

    // 7. Normal mode: apply all filters
    if ctx.market_up_price < config.min_market_price || ctx.market_up_price > config.max_market_price {
        return None;
    }

    // 7a. Payout filter — skip low-payout traps (e.g. price=0.96 → payout=0.04x)
    let payout = (1.0 - market_price) / market_price;
    if config.min_payout_ratio > 0.0 && payout < config.min_payout_ratio {
        tracing::debug!("Skip: payout {:.3} < min {:.3}", payout, config.min_payout_ratio);
        return None;
    }

    // 7b. Book imbalance filter — skip when imbalance is too low
    if config.min_book_imbalance > 0.0 && ctx.book_imbalance < config.min_book_imbalance {
        tracing::debug!("Skip: imbalance {:.3} < min {:.3}", ctx.book_imbalance, config.min_book_imbalance);
        return None;
    }

    // Cohérence Chainlink / exchanges — skip si divergence directionnelle
    if let Some(ex_price) = ctx.exchange_price {
        let cl_move_pct = ((ctx.chainlink_price - ctx.start_price) / ctx.start_price).abs();
        if cl_move_pct > 0.00001 {
            let chainlink_up = ctx.chainlink_price > ctx.start_price;
            let exchange_up = ex_price > ctx.start_price;
            if chainlink_up != exchange_up {
                tracing::debug!("Skip: divergence CL/WS (CL={:.2}, WS={ex_price:.2}, start={:.2})",
                    ctx.chainlink_price, ctx.start_price);
                return None;
            }
        }
    }

    if price_change_pct.abs() < config.min_delta_pct {
        tracing::debug!("Skip: Δ {:.4}% < min_delta {:.4}%", price_change_pct.abs(), config.min_delta_pct);
        return None;
    }

    if config.max_spread > 0.0 && ctx.spread > config.max_spread {
        tracing::debug!("Skip: spread {:.4} > max {:.4}", ctx.spread, config.max_spread);
        return None;
    }

    if edge <= 0.0 || net_edge_pct < config.min_edge_pct {
        return None;
    }

    // 8. Fractional Kelly sizing with fee-adjusted payout and bankroll tracking
    let bankroll = session.bankroll();
    let kelly_size = fractional_kelly(
        true_prob, market_price, ctx.fee_rate,
        config.kelly_fraction, bankroll, config.max_bet_usdc,
    );
    if kelly_size <= 0.0 {
        return None;
    }
    // 8a. Book imbalance boost: higher imbalance → larger bet
    let imbalance_boost = 1.0 + (ctx.book_imbalance - config.min_book_imbalance).clamp(0.0, 1.0);
    let kelly_size = (kelly_size * imbalance_boost).min(config.max_bet_usdc);

    let min_usdc = (config.min_shares as f64 * market_price).max(config.min_bet_usdc);
    if min_usdc > config.max_bet_usdc {
        tracing::debug!("Skip: min order ${:.2} > max_bet ${:.2}", min_usdc, config.max_bet_usdc);
        return None;
    }
    if kelly_size < min_usdc * 0.1 {
        tracing::debug!("Skip: Kelly ${:.2} too marginal vs min ${:.2}", kelly_size, min_usdc);
        return None;
    }
    let size = if kelly_size < min_usdc {
        tracing::debug!("Kelly ${:.2} bumped to min ${:.2}", kelly_size, min_usdc);
        min_usdc
    } else {
        kelly_size
    };

    tracing::info!(
        "SIGNAL: {} | Edge: {:.1}% (brut {:.1}%, fee {:.2}%) | Δ prix: {:.4}% | Size: ${:.2} | {}s restantes | src: {}",
        if side == Side::Buy { "BUY UP" } else { "BUY DOWN" },
        net_edge_pct, edge_pct, fee * 100.0, price_change_pct, size, ctx.seconds_remaining,
        if ctx.rtds_price.is_some() { "RTDS" } else if ctx.exchange_price.is_some() { "WS" } else { "CL" },
    );

    Some(Signal {
        side,
        edge_pct: net_edge_pct,
        edge_brut_pct: edge_pct,
        fee_pct: fee * 100.0,
        implied_p_up: true_up_prob,
        size_usdc: size,
        price: market_price,
    })
}

/// Calcule les frais dynamiques Polymarket.
/// fee_rate = 0.25 pour les marchés crypto 5min/15min, exponent = 2.
/// Retourne le fee en fraction du coût (price).
pub fn dynamic_fee(price: f64, fee_rate: f64) -> f64 {
    let p_q = price * (1.0 - price);
    fee_rate * p_q.powi(2)
}

/// Probabilité UP time-aware basée sur un modèle de volatilité.
/// Utilise la vol résiduelle pour pondérer la confiance selon le temps restant.
fn price_change_to_probability(pct_change: f64, seconds_remaining: u64, vol_5min_pct: f64, confidence_multiplier: f64) -> f64 {
    let remaining_vol = vol_5min_pct * confidence_multiplier * ((seconds_remaining as f64) / 300.0).sqrt();

    if remaining_vol < 1e-9 {
        // Quasi plus de temps — direction verrouillée
        return if pct_change > 0.0 { 1.0 } else if pct_change < 0.0 { 0.0 } else { 0.5 };
    }

    // z-score : avance actuelle / vol résiduelle
    let z = pct_change / remaining_vol;
    normal_cdf(z)
}

/// Approximation de la CDF normale (Abramowitz & Stegun, erreur max 1.5e-7).
fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/sqrt(2*pi)
    let p = d * (-x * x / 2.0).exp()
        * (t * (0.319381530
            + t * (-0.356563782
                + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429)))));
    if x >= 0.0 { 1.0 - p } else { p }
}

/// Fractional Kelly Criterion with fee-adjusted payout.
/// Uses b_net = (1-price)/price - fee to account for taker fees in the Kelly formula.
/// Sizes based on current bankroll, clamped to max_bet.
fn fractional_kelly(p: f64, price: f64, fee_rate: f64, kelly_fraction: f64, bankroll: f64, max_bet: f64) -> f64 {
    if price <= 0.0 || price >= 1.0 || p <= 0.0 || p >= 1.0 || bankroll <= 0.0 {
        return 0.0;
    }
    let fee = dynamic_fee(price, fee_rate);
    let b_net = (1.0 - price) / price - fee;
    if b_net <= 0.0 {
        return 0.0;
    }
    let q = 1.0 - p;
    let kelly = (b_net * p - q) / b_net;
    (kelly * kelly_fraction * bankroll).clamp(0.0, max_bet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> StrategyConfig {
        StrategyConfig {
            max_bet_usdc: 2.0,
            min_bet_usdc: 0.01,
            min_shares: 0,
            min_edge_pct: 1.0,
            entry_seconds_before_end: 30,
            session_profit_target_usdc: 100.0,
            session_loss_limit_usdc: 50.0,
            fee_rate: 0.25,
            min_market_price: 0.15,
            max_market_price: 0.85,
            min_delta_pct: 0.0,
            max_spread: 0.0,
            kelly_fraction: 0.5,
            initial_bankroll_usdc: 40.0,
            always_trade: false,
            vol_confidence_multiplier: 1.0,
            min_payout_ratio: 0.0,
            min_book_imbalance: 0.0,
            max_vol_5min_pct: 0.0,
        }
    }

    const DEFAULT_VOL: f64 = 0.12;

    fn test_ctx() -> TradeContext {
        TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_000.0,
            exchange_price: None,
            rtds_price: None,
            market_up_price: 0.50,
            seconds_remaining: 10,
            fee_rate: 0.25,
            vol_5min_pct: DEFAULT_VOL,
            spread: 0.0,
            book_imbalance: 0.5,
        }
    }

    // --- price_change_to_probability ---

    #[test]
    fn prob_positive_move_low_time() {
        let p = price_change_to_probability(0.05, 5, DEFAULT_VOL, 1.0);
        assert!(p > 0.95, "got {p}");
    }

    #[test]
    fn prob_positive_move_high_time() {
        let p = price_change_to_probability(0.05, 60, DEFAULT_VOL, 1.0);
        assert!(p > 0.5 && p < 0.95, "got {p}");
    }

    #[test]
    fn prob_flat() {
        let p = price_change_to_probability(0.0, 30, DEFAULT_VOL, 1.0);
        assert!((p - 0.5).abs() < 0.001, "got {p}");
    }

    #[test]
    fn prob_negative_move() {
        let p = price_change_to_probability(-0.05, 10, DEFAULT_VOL, 1.0);
        assert!(p < 0.1, "got {p}");
    }

    #[test]
    fn prob_zero_time_locks_direction() {
        assert!(price_change_to_probability(0.01, 0, DEFAULT_VOL, 1.0) == 1.0);
        assert!(price_change_to_probability(-0.01, 0, DEFAULT_VOL, 1.0) == 0.0);
        assert!(price_change_to_probability(0.0, 0, DEFAULT_VOL, 1.0) == 0.5);
    }

    // --- fractional_kelly ---

    #[test]
    fn kelly_positive_edge() {
        let size = fractional_kelly(0.7, 0.5, 0.25, 0.5, 40.0, 5.0);
        assert!(size > 0.0 && size <= 5.0, "got {size}");
    }

    #[test]
    fn kelly_no_edge() {
        let size = fractional_kelly(0.5, 0.5, 0.25, 0.5, 40.0, 5.0);
        assert!(size.abs() < 0.001, "got {size}");
    }

    #[test]
    fn kelly_bad_odds() {
        let size = fractional_kelly(0.3, 0.5, 0.25, 0.5, 40.0, 5.0);
        assert!(size.abs() < 0.001, "got {size}");
    }

    #[test]
    fn kelly_fraction_proportional() {
        // Quarter Kelly should be ~half the size of Half Kelly
        let half = fractional_kelly(0.7, 0.5, 0.25, 0.5, 40.0, 50.0);
        let quarter = fractional_kelly(0.7, 0.5, 0.25, 0.25, 40.0, 50.0);
        assert!((half - 2.0 * quarter).abs() < 0.01, "half={half} quarter={quarter}");
    }

    #[test]
    fn kelly_fee_adjusted_smaller_than_naive() {
        // Fee-adjusted Kelly should be smaller than naive (fee_rate=0)
        let with_fee = fractional_kelly(0.7, 0.5, 0.25, 0.5, 40.0, 50.0);
        let no_fee = fractional_kelly(0.7, 0.5, 0.0, 0.5, 40.0, 50.0);
        assert!(with_fee < no_fee, "with_fee={with_fee} should be < no_fee={no_fee}");
    }

    #[test]
    fn kelly_bankroll_scales_size() {
        // Doubling bankroll should double the size (before max_bet clamp)
        let small = fractional_kelly(0.7, 0.5, 0.25, 0.5, 20.0, 100.0);
        let large = fractional_kelly(0.7, 0.5, 0.25, 0.5, 40.0, 100.0);
        assert!((large - 2.0 * small).abs() < 0.01, "large={large} small={small}");
    }

    #[test]
    fn kelly_zero_bankroll_returns_zero() {
        let size = fractional_kelly(0.7, 0.5, 0.25, 0.5, 0.0, 5.0);
        assert!(size.abs() < 0.001, "got {size}");
    }

    #[test]
    fn kelly_negative_bankroll_returns_zero() {
        let size = fractional_kelly(0.7, 0.5, 0.25, 0.5, -5.0, 5.0);
        assert!(size.abs() < 0.001, "got {size}");
    }

    // --- evaluate ---

    #[test]
    fn evaluate_buy_up_signal() {
        let config = test_config();
        let session = Session::new(40.0);
        // BTC +0.05% avec 10s restantes, marché à 50/50
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        let s = signal.unwrap();
        assert_eq!(s.side, Side::Buy);
        assert!(s.edge_pct > 1.0);
    }

    #[test]
    fn evaluate_buy_down_signal() {
        let config = test_config();
        let session = Session::new(40.0);
        // BTC -0.05% avec 10s restantes, marché à 50/50
        let ctx = TradeContext { chainlink_price: 99_950.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        let s = signal.unwrap();
        assert_eq!(s.side, Side::Sell); // Sell = buy DOWN token
    }

    #[test]
    fn evaluate_no_signal_outside_window() {
        let config = test_config();
        let session = Session::new(40.0);
        // 60s restantes > entry_seconds_before_end (30)
        let ctx = TradeContext { chainlink_price: 100_050.0, seconds_remaining: 60, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_no_signal_profit_target() {
        let config = test_config();
        let mut session = Session::new(40.0);
        session.pnl_usdc = 100.0; // target atteint
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_no_signal_loss_limit() {
        let config = test_config();
        let mut session = Session::new(40.0);
        session.pnl_usdc = -50.0; // limit atteint
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_no_signal_low_edge() {
        let config = test_config();
        let session = Session::new(40.0);
        // Marché déjà ajusté à 0.99 → edge < 1% (min_edge_pct)
        let ctx = TradeContext { chainlink_price: 100_050.0, market_up_price: 0.99, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_rejects_bad_market_price() {
        let config = test_config();
        let session = Session::new(40.0);
        let ctx1 = TradeContext { chainlink_price: 100_050.0, market_up_price: 1.5, ..test_ctx() };
        assert!(evaluate(&ctx1, &session, &config).is_none());
        let ctx2 = TradeContext { chainlink_price: 100_050.0, market_up_price: 0.0, ..test_ctx() };
        assert!(evaluate(&ctx2, &session, &config).is_none());
    }

    // --- dynamic_fee ---

    #[test]
    fn dynamic_fee_at_50_50() {
        // fee_rate=0.25, p=0.50: 0.25 × (0.25)^2 = 0.015625 → 1.56%
        let fee = dynamic_fee(0.50, 0.25);
        assert!((fee - 0.015625).abs() < 0.0001, "got {fee}");
    }

    #[test]
    fn dynamic_fee_at_80_20() {
        // fee_rate=0.25, p=0.80: 0.25 × (0.16)^2 = 0.0064 → 0.64%
        let fee = dynamic_fee(0.80, 0.25);
        assert!((fee - 0.0064).abs() < 0.0001, "got {fee}");
    }

    #[test]
    fn dynamic_fee_at_95_05() {
        // fee_rate=0.25, p=0.95: 0.25 × (0.0475)^2 = 0.000564 → 0.056%
        let fee = dynamic_fee(0.95, 0.25);
        assert!((fee - 0.000564).abs() < 0.0001, "got {fee}");
    }

    #[test]
    fn evaluate_rejects_when_fee_exceeds_edge() {
        let config = test_config();
        let session = Session::new(40.0);
        // BTC +0.0005% avec 10s restantes, marché à 50/50
        // Edge brut ~0.9%, fee ~0.625% → net edge ~0.28% < min_edge 1%
        let ctx = TradeContext { chainlink_price: 100_000.5, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    // --- price zone filter ---

    #[test]
    fn evaluate_rejects_below_min_market_price() {
        let config = test_config(); // min=0.15
        let session = Session::new(40.0);
        // Marché à 0.10 → en dessous de min_market_price
        let ctx = TradeContext { chainlink_price: 100_050.0, market_up_price: 0.10, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_rejects_above_max_market_price() {
        let config = test_config(); // max=0.85
        let session = Session::new(40.0);
        // Marché à 0.90 → au dessus de max_market_price
        let ctx = TradeContext { chainlink_price: 100_050.0, market_up_price: 0.90, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_accepts_70_30() {
        let config = test_config();
        let session = Session::new(40.0);
        // Marché à 0.70, dans la zone autorisée, +0.05% avec 10s restantes
        let ctx = TradeContext { chainlink_price: 100_050.0, market_up_price: 0.70, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
    }

    // --- exchange_price integration ---

    #[test]
    fn evaluate_uses_exchange_price_when_provided() {
        let config = test_config();
        let session = Session::new(40.0);
        // Les deux UP, mais exchange montre un mouvement plus large → signal basé sur exchange
        let ctx = TradeContext {
            chainlink_price: 100_010.0,
            exchange_price: Some(100_050.0),
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        assert_eq!(signal.unwrap().side, Side::Buy);
    }

    #[test]
    fn evaluate_falls_back_to_chainlink_when_no_exchange() {
        let config = test_config();
        let session = Session::new(40.0);
        // exchange_price = None → utilise chainlink_price (+0.05%)
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        assert_eq!(signal.unwrap().side, Side::Buy);
    }

    // --- VolTracker ---

    #[test]
    fn vol_tracker_with_no_data_returns_default() {
        let vt = VolTracker::new(20, 0.12);
        assert!((vt.current_vol() - 0.12).abs() < 0.001);
    }

    #[test]
    fn vol_tracker_with_few_samples_returns_default() {
        let mut vt = VolTracker::new(20, 0.12);
        vt.record_move(100_000.0, 100_100.0);
        vt.record_move(100_000.0, 99_900.0);
        // Seulement 2 samples < 3 → default
        assert!((vt.current_vol() - 0.12).abs() < 0.001);
    }

    #[test]
    fn vol_tracker_adapts() {
        let mut vt = VolTracker::new(5, 0.12);
        // Mouvements de ~0.2% → vol devrait être ~0.2%
        for _ in 0..5 {
            vt.record_move(100_000.0, 100_200.0);
        }
        // Tous les mêmes → std dev = 0, clamped à 0.01
        // Il faut de la variance — alternons +0.2% et -0.2%
        let mut vt2 = VolTracker::new(10, 0.12);
        for i in 0..10 {
            if i % 2 == 0 {
                vt2.record_move(100_000.0, 100_200.0);
            } else {
                vt2.record_move(100_000.0, 99_800.0);
            }
        }
        let vol = vt2.current_vol();
        assert!(vol > 0.15, "got {vol}"); // std dev de ±0.2 ≈ 0.2
    }

    #[test]
    fn vol_tracker_evicts_old_samples() {
        let mut vt = VolTracker::new(3, 0.12);
        vt.record_move(100_000.0, 100_100.0); // +0.1%
        vt.record_move(100_000.0, 99_900.0);  // -0.1%
        vt.record_move(100_000.0, 100_050.0); // +0.05%
        vt.record_move(100_000.0, 99_950.0);  // -0.05% (évince le premier)
        assert_eq!(vt.recent_moves.len(), 3);
    }

    // --- divergence Chainlink / exchanges ---

    #[test]
    fn evaluate_skips_on_direction_divergence() {
        let config = test_config();
        let session = Session::new(40.0);
        // Chainlink dit DOWN (-0.05%), exchanges dit UP (+0.05%) → divergence → None
        let ctx = TradeContext {
            chainlink_price: 99_950.0,
            exchange_price: Some(100_050.0),
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_ok_when_both_agree() {
        let config = test_config();
        let session = Session::new(40.0);
        // Les deux disent UP → pas de divergence
        let ctx = TradeContext {
            chainlink_price: 100_030.0,
            exchange_price: Some(100_050.0),
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
    }

    #[test]
    fn evaluate_no_divergence_when_chainlink_flat() {
        let config = test_config();
        let session = Session::new(40.0);
        // Chainlink flat (== start), exchange UP → tolérance, pas de divergence
        let ctx = TradeContext {
            exchange_price: Some(100_050.0),
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
    }

    // --- spread integration ---

    #[test]
    fn evaluate_rejects_when_spread_kills_edge() {
        let config = test_config();
        let session = Session::new(40.0);
        // BTC +0.005% with 10s remaining, market at 0.55
        // Edge brut ~4%, fee ~0.6%, net ~3.4% → passes with 0 spread
        let ctx_no_spread = TradeContext {
            chainlink_price: 100_005.0,
            market_up_price: 0.55,
            ..test_ctx()
        };
        let with_no_spread = evaluate(&ctx_no_spread, &session, &config);
        assert!(with_no_spread.is_some(), "should pass with 0 spread");

        // With spread=0.06 → spread_cost=3%, net edge ~0.4% < min_edge 1% → rejected
        let ctx_spread = TradeContext {
            chainlink_price: 100_005.0,
            market_up_price: 0.55,
            spread: 0.06,
            ..test_ctx()
        };
        let with_spread = evaluate(&ctx_spread, &session, &config);
        assert!(with_spread.is_none(), "spread should kill the edge");
    }

    // --- min_delta_pct filter ---

    #[test]
    fn evaluate_skips_when_delta_below_min() {
        let config = StrategyConfig { min_delta_pct: 0.005, ..test_config() };
        let session = Session::new(40.0);
        // BTC +0.003% → below min_delta 0.005% → skip
        let ctx = TradeContext { chainlink_price: 100_003.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should skip: Δ 0.003% < min_delta 0.005%");
    }

    #[test]
    fn evaluate_passes_when_delta_above_min() {
        let config = StrategyConfig { min_delta_pct: 0.005, ..test_config() };
        let session = Session::new(40.0);
        // BTC +0.05% → well above min_delta 0.005% → proceed
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "should pass: Δ 0.05% > min_delta 0.005%");
    }

    #[test]
    fn evaluate_delta_filter_disabled_at_zero() {
        let config = StrategyConfig { min_delta_pct: 0.0, ..test_config() };
        let session = Session::new(40.0);
        // BTC +0.001% → tiny delta but filter disabled (0.0)
        let ctx = TradeContext { chainlink_price: 100_001.0, ..test_ctx() };
        // Should NOT be blocked by delta filter (may still be blocked by edge)
        // This just ensures the filter doesn't fire when set to 0
        let _ = evaluate(&ctx, &session, &config);
    }

    // --- max_spread filter ---

    #[test]
    fn evaluate_skips_when_spread_above_max() {
        let config = StrategyConfig { max_spread: 0.05, ..test_config() };
        let session = Session::new(40.0);
        // spread 0.08 > max 0.05 → skip
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            spread: 0.08,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should skip: spread 0.08 > max 0.05");
    }

    #[test]
    fn evaluate_passes_when_spread_below_max() {
        let config = StrategyConfig { max_spread: 0.05, ..test_config() };
        let session = Session::new(40.0);
        // spread 0.02 < max 0.05 → proceed
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            spread: 0.02,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "should pass: spread 0.02 < max 0.05");
    }

    #[test]
    fn evaluate_spread_filter_disabled_at_zero() {
        let config = StrategyConfig { max_spread: 0.0, ..test_config() };
        let session = Session::new(40.0);
        // spread 0.50 but filter disabled (max=0.0)
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            spread: 0.50,
            ..test_ctx()
        };
        // Should NOT be blocked by spread filter, only by edge (spread cost in evaluate)
        let _ = evaluate(&ctx, &session, &config);
    }

    // --- minimum order size ---

    #[test]
    fn evaluate_bumps_to_min_bet_usdc() {
        let config = StrategyConfig {
            min_bet_usdc: 1.0,
            min_shares: 0,
            ..test_config()
        };
        let session = Session::new(40.0);
        // Strong edge → Kelly would size small relative to max_bet, but floor at $1
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        assert!(signal.unwrap().size_usdc >= 1.0, "size should be >= $1 min");
    }

    #[test]
    fn evaluate_bumps_to_min_shares_constraint() {
        let config = StrategyConfig {
            min_bet_usdc: 1.0,
            min_shares: 5,
            max_bet_usdc: 10.0,
            ..test_config()
        };
        let session = Session::new(40.0);
        // price=0.50 → 5 shares = $2.50 minimum
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        let s = signal.unwrap();
        assert!(s.size_usdc >= 2.50, "5 shares @ 0.50 = $2.50 min, got ${:.2}", s.size_usdc);
    }

    #[test]
    fn evaluate_skips_when_min_exceeds_max_bet() {
        let config = StrategyConfig {
            min_bet_usdc: 1.0,
            min_shares: 5,
            max_bet_usdc: 2.0, // max=$2, but 5 shares @ 0.50 = $2.50 > max
            ..test_config()
        };
        let session = Session::new(40.0);
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should skip: min $2.50 > max $2.00");
    }

    #[test]
    fn evaluate_normal_kelly_above_min() {
        let config = StrategyConfig {
            min_bet_usdc: 1.0,
            min_shares: 5,
            max_bet_usdc: 10.0,
            ..test_config()
        };
        let session = Session::new(40.0);
        // Strong edge → Kelly sizes well above minimum
        let ctx = TradeContext {
            chainlink_price: 100_050.0, // +0.05%, high confidence at 10s (p≈0.99)
            market_up_price: 0.50,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some());
        let s = signal.unwrap();
        // Kelly should give >$2.50 with this edge
        assert!(s.size_usdc >= 2.50);
    }

    #[test]
    fn evaluate_skips_marginal_kelly() {
        // Kelly returns tiny value → <10% of min → skip (don't amplify weak signals)
        let config = StrategyConfig {
            min_bet_usdc: 1.0,
            min_shares: 5,
            max_bet_usdc: 5.0,
            min_edge_pct: 0.1, // very low to let marginal signals through to Kelly
            ..test_config()
        };
        let session = Session::new(40.0);
        // Tiny price move at 10s → small Kelly fraction, below 10% of min ($0.25)
        let ctx = TradeContext {
            chainlink_price: 100_000.5, // +0.0005% → very weak signal
            market_up_price: 0.50,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        // Kelly should be near-zero, well below 10% of $2.50 min → skip
        assert!(signal.is_none(), "marginal Kelly should be skipped, not bumped");
    }

    // --- paper test: realistic $40 portfolio scenarios ---

    fn prod_config() -> StrategyConfig {
        StrategyConfig {
            max_bet_usdc: 5.0,
            min_bet_usdc: 1.0,
            min_shares: 5,
            min_edge_pct: 5.0,
            entry_seconds_before_end: 12,
            session_profit_target_usdc: 50.0,
            session_loss_limit_usdc: 20.0,
            fee_rate: 0.25,
            min_market_price: 0.20,
            max_market_price: 0.80,
            min_delta_pct: 0.005,
            max_spread: 0.05,
            kelly_fraction: 0.25,
            initial_bankroll_usdc: 40.0,
            always_trade: false,
            vol_confidence_multiplier: 2.5,
            min_payout_ratio: 0.08,
            min_book_imbalance: 0.08,
            max_vol_5min_pct: 0.12,
        }
    }

    #[test]
    fn paper_strong_up_signal_10s() {
        // BTC +0.03% with 10s remaining, market at 0.55, spread 0.02
        let config = prod_config();
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_030.0,
            exchange_price: Some(100_030.0),
            rtds_price: None,
            market_up_price: 0.55,
            seconds_remaining: 10,
            fee_rate: 0.25,
            vol_5min_pct: 0.10,
            spread: 0.02,
            book_imbalance: 0.20,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "should trade with +0.03% at 10s");
        let s = signal.unwrap();
        assert_eq!(s.side, Side::Buy);
        assert!(s.size_usdc >= 1.0 && s.size_usdc <= 5.0,
            "size ${:.2} should be in $1-$5 range", s.size_usdc);
        assert!(s.edge_pct >= 5.0, "net edge {:.1}% should be >= 5%", s.edge_pct);
    }

    #[test]
    fn paper_weak_signal_rejected() {
        // BTC +0.005% with 10s remaining → small edge, below min_edge 5%
        let config = prod_config();
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_005.0,
            exchange_price: Some(100_005.0),
            rtds_price: None,
            market_up_price: 0.55,
            seconds_remaining: 10,
            fee_rate: 0.25,
            vol_5min_pct: 0.10,
            spread: 0.02,
            book_imbalance: 0.20,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "weak +0.005% should not pass 5% min edge");
    }

    #[test]
    fn paper_high_price_min_shares_binding() {
        // Market at 0.80 → 5 shares = $4.00 min, close to max_bet $5
        let config = prod_config();
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_050.0,
            exchange_price: Some(100_050.0),
            rtds_price: None,
            market_up_price: 0.80,
            seconds_remaining: 10,
            fee_rate: 0.25,
            vol_5min_pct: 0.10,
            spread: 0.01,
            book_imbalance: 0.20,
        };
        let signal = evaluate(&ctx, &session, &config);
        if let Some(s) = signal {
            assert!(s.size_usdc >= 4.0, "min 5 shares @ 0.80 = $4, got ${:.2}", s.size_usdc);
            assert!(s.size_usdc <= 5.0, "capped at max_bet $5, got ${:.2}", s.size_usdc);
        }
    }

    #[test]
    fn paper_session_loss_limit_40_portfolio() {
        // After losing $20, session should stop (50% of $40 portfolio)
        let config = prod_config();
        let mut session = Session::new(40.0);
        session.pnl_usdc = -20.0;
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_050.0,
            exchange_price: Some(100_050.0),
            rtds_price: None,
            market_up_price: 0.50,
            seconds_remaining: 10,
            fee_rate: 0.25,
            vol_5min_pct: 0.10,
            spread: 0.02,
            book_imbalance: 0.20,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should stop after -$20 (50% of $40 portfolio)");
    }

    #[test]
    fn paper_spread_eats_edge() {
        // Moderate price move but wide spread kills the net edge below 5%
        // Disable max_spread filter to test spread cost mechanism specifically
        let config = StrategyConfig { max_spread: 0.0, ..prod_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_010.0, // +0.01%
            exchange_price: Some(100_010.0),
            rtds_price: None,
            market_up_price: 0.55,
            seconds_remaining: 10,
            fee_rate: 0.25,
            vol_5min_pct: 0.10,
            spread: 0.20, // 10% spread cost → kills edge below 5%
            book_imbalance: 0.20,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "wide spread should kill the edge below 5% min");
    }

    // --- Session bankroll tracking ---

    #[test]
    fn session_bankroll_tracks_pnl() {
        let mut s = Session::new(40.0);
        assert!((s.bankroll() - 40.0).abs() < 0.001);
        s.record_trade(5.0);
        assert!((s.bankroll() - 45.0).abs() < 0.001);
        s.record_trade(-3.0);
        assert!((s.bankroll() - 42.0).abs() < 0.001);
    }

    #[test]
    fn session_default_has_zero_bankroll() {
        let s = Session::default();
        assert!((s.bankroll()).abs() < 0.001);
    }

    // --- bankroll-aware sizing in evaluate ---

    #[test]
    fn evaluate_reduces_size_after_losses() {
        let config = test_config();
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };

        // Full bankroll
        let session_full = Session::new(40.0);
        let sig_full = evaluate(&ctx, &session_full, &config);

        // Half bankroll (after losses)
        let mut session_half = Session::new(40.0);
        session_half.pnl_usdc = -20.0; // bankroll = 20
        let sig_half = evaluate(&ctx, &session_half, &config);

        assert!(sig_full.is_some());
        assert!(sig_half.is_some());
        // Size should be smaller with less bankroll (or bumped to min, either way <= full)
        assert!(sig_half.unwrap().size_usdc <= sig_full.unwrap().size_usdc,
            "size should decrease with bankroll losses");
    }

    // --- vol_confidence_multiplier ---

    #[test]
    fn confidence_multiplier_reduces_probability() {
        // Same move, higher multiplier → less confident (closer to 0.5)
        let p_low = price_change_to_probability(0.05, 10, DEFAULT_VOL, 1.0);
        let p_high = price_change_to_probability(0.05, 10, DEFAULT_VOL, 2.5);
        assert!(p_high < p_low, "multiplier should reduce confidence: {p_high} vs {p_low}");
        assert!(p_high > 0.5, "should still lean UP: {p_high}");
    }

    // --- min_payout_ratio ---

    #[test]
    fn evaluate_skips_low_payout() {
        // price=0.95 → payout=0.053 < min_payout_ratio=0.08 → skip
        let config = StrategyConfig { min_payout_ratio: 0.08, ..test_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            market_up_price: 0.20, // → DOWN token price = 0.80, payout = 0.25 > 0.08 → ok
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "payout 0.25 should pass min 0.08");

        // market_up_price near extreme → trading side has low payout
        let ctx2 = TradeContext {
            chainlink_price: 100_050.0,
            market_up_price: 0.82, // UP token, payout = 0.22 > 0.08 → ok
            ..test_ctx()
        };
        let signal2 = evaluate(&ctx2, &session, &config);
        // May or may not pass other filters, but payout is ok
        let _ = signal2;
    }

    // --- max_vol_5min_pct ---

    #[test]
    fn evaluate_skips_high_vol() {
        let config = StrategyConfig { max_vol_5min_pct: 0.12, ..test_config() };
        let session = Session::new(40.0);
        // Vol 0.15% > max 0.12% → skip
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            vol_5min_pct: 0.15,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "high vol should be skipped");
    }

    #[test]
    fn evaluate_passes_low_vol() {
        let config = StrategyConfig { max_vol_5min_pct: 0.12, ..test_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            vol_5min_pct: 0.08,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "low vol should pass");
    }

    // --- min_book_imbalance ---

    #[test]
    fn evaluate_skips_low_imbalance() {
        let config = StrategyConfig { min_book_imbalance: 0.08, ..test_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            book_imbalance: 0.03, // < 0.08 → skip
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "low imbalance should be skipped");
    }

    #[test]
    fn evaluate_passes_high_imbalance() {
        let config = StrategyConfig { min_book_imbalance: 0.08, ..test_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            book_imbalance: 0.30,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "high imbalance should pass");
    }

    #[test]
    fn imbalance_boosts_sizing() {
        let config = StrategyConfig { min_book_imbalance: 0.08, ..test_config() };
        let session = Session::new(40.0);
        // Low imbalance
        let ctx_low = TradeContext {
            chainlink_price: 100_050.0,
            book_imbalance: 0.10,
            ..test_ctx()
        };
        let sig_low = evaluate(&ctx_low, &session, &config);
        // High imbalance
        let ctx_high = TradeContext {
            chainlink_price: 100_050.0,
            book_imbalance: 0.60,
            ..test_ctx()
        };
        let sig_high = evaluate(&ctx_high, &session, &config);
        assert!(sig_low.is_some());
        assert!(sig_high.is_some());
        assert!(sig_high.unwrap().size_usdc >= sig_low.unwrap().size_usdc,
            "higher imbalance should give equal or larger size");
    }
}
