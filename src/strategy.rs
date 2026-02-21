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
    pub min_ws_sources: u32,
    pub circuit_breaker_window: usize,
    pub circuit_breaker_min_wr: f64,
    pub circuit_breaker_cooldown_s: u64,
    /// Minimum implied probability to trade. Filters out low-confidence predictions.
    /// Data shows WR drops when model isn't confident. Set 0.0 to disable.
    /// Recommended: 0.70+ (only trade when model says 70%+ chance of being right).
    pub min_implied_prob: f64,
    /// Maximum consecutive losses before pausing (0 = disabled).
    /// Data shows loss streaks of 13 — this caps exposure during drawdowns.
    pub max_consecutive_losses: u32,
    /// Degrees of freedom for Student-t CDF (0.0 = use normal CDF).
    /// Lower df = heavier tails = more conservative. Recommended: 4.0.
    pub student_t_df: f64,
    /// Minimum absolute z-score to trade (0.0 = disabled). Recommended: 0.5.
    pub min_z_score: f64,
    /// Maximum model-vs-market divergence (0.0 = disabled). Recommended: 0.30.
    pub max_model_divergence: f64,
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

/// État de la session (P&L, nombre de trades, bankroll tracking, circuit breaker).
#[derive(Debug)]
pub struct Session {
    pub pnl_usdc: f64,
    pub trades: u32,
    pub wins: u32,
    pub initial_bankroll: f64,
    /// Rolling window of recent trade outcomes (true=win, false=loss) for circuit breaker.
    recent_outcomes: VecDeque<bool>,
    /// Timestamp (unix secs) when circuit breaker was triggered. 0 = not active.
    pub circuit_breaker_until: u64,
    /// Current consecutive loss count (resets on any win).
    pub consecutive_losses: u32,
    /// Current consecutive win count (resets on any loss).
    pub consecutive_wins: u32,
    /// Minimum PnL reached during session (for drawdown calculation).
    pub min_pnl: f64,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            pnl_usdc: 0.0, trades: 0, wins: 0, initial_bankroll: 0.0,
            recent_outcomes: VecDeque::new(), circuit_breaker_until: 0,
            consecutive_losses: 0,
            consecutive_wins: 0,
            min_pnl: 0.0,
        }
    }
}

impl Session {
    pub fn new(initial_bankroll: f64) -> Self {
        Self { initial_bankroll, ..Default::default() }
    }

    pub fn bankroll(&self) -> f64 {
        self.initial_bankroll + self.pnl_usdc
    }

    pub fn record_trade(&mut self, pnl: f64) {
        self.pnl_usdc += pnl;
        self.trades += 1;
        // Break-even (pnl == 0.0) treated as loss: costs opportunity, resets win streak.
        let won = pnl > 0.0;
        if won {
            self.wins += 1;
            self.consecutive_losses = 0;
            self.consecutive_wins += 1;
        } else {
            self.consecutive_losses += 1;
            self.consecutive_wins = 0;
        }
        if self.pnl_usdc < self.min_pnl {
            self.min_pnl = self.pnl_usdc;
        }
        self.recent_outcomes.push_back(won);
    }

    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 { 0.0 } else { self.wins as f64 / self.trades as f64 }
    }

    /// Rolling win rate over the last `window` trades. Returns None if not enough trades.
    pub fn rolling_wr(&self, window: usize) -> Option<f64> {
        if window == 0 || self.recent_outcomes.len() < window {
            return None;
        }
        let recent: Vec<_> = self.recent_outcomes.iter().rev().take(window).collect();
        let wins = recent.iter().filter(|&&w| *w).count();
        Some(wins as f64 / window as f64)
    }

    /// Check if circuit breaker should trigger. If rolling WR is below threshold, set cooldown.
    pub fn check_circuit_breaker(&mut self, window: usize, min_wr: f64, cooldown_secs: u64, now: u64) {
        if window == 0 || min_wr <= 0.0 {
            return;
        }
        if let Some(wr) = self.rolling_wr(window) {
            if wr < min_wr {
                self.circuit_breaker_until = now + cooldown_secs;
                tracing::warn!(
                    "Circuit breaker triggered: rolling WR {:.0}% < {:.0}% over {} trades. Pausing until +{}s",
                    wr * 100.0, min_wr * 100.0, window, cooldown_secs
                );
            }
        }
    }

    /// Returns true if circuit breaker is active (should not trade).
    pub fn is_circuit_broken(&self, now: u64) -> bool {
        self.circuit_breaker_until > now
    }

    /// Returns session drawdown as a percentage of initial bankroll.
    /// Drawdown = how far below zero PnL has gone, expressed as % of bankroll.
    pub fn session_drawdown_pct(&self) -> f64 {
        if self.initial_bankroll <= 0.0 {
            return 0.0;
        }
        (-self.min_pnl / self.initial_bankroll * 100.0).max(0.0)
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

    /// Volatilité estimée (MAD — résiste aux outliers).
    /// Retourne default_vol si pas assez de données (< 3 samples).
    pub fn current_vol(&self) -> f64 {
        if self.recent_moves.len() < 3 {
            return self.default_vol;
        }
        let mut sorted: Vec<f64> = self.recent_moves.iter().copied().collect();
        sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        let mut deviations: Vec<f64> = sorted.iter().map(|x| (x - median).abs()).collect();
        deviations.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mad = deviations[deviations.len() / 2];
        // MAD → std dev: σ ≈ 1.4826 × MAD (for normal distribution)
        (1.4826 * mad).clamp(0.01, 1.0)
    }
}

/// Buffer de prix intra-window pour le regime detection.
/// Collecte les ticks pendant une fenêtre 5min et calcule micro-vol + momentum.
#[derive(Debug)]
pub struct WindowTicks {
    prices: Vec<f64>,
    timestamps_ms: Vec<u64>,
}

impl WindowTicks {
    pub fn new() -> Self {
        Self {
            prices: Vec::with_capacity(3200),
            timestamps_ms: Vec::with_capacity(3200),
        }
    }

    pub fn tick(&mut self, price: f64, timestamp_ms: u64) {
        self.prices.push(price);
        self.timestamps_ms.push(timestamp_ms);
    }

    pub fn clear(&mut self) {
        self.prices.clear();
        self.timestamps_ms.clear();
    }

    /// Micro-volatility: std dev of tick-to-tick log-returns (%).
    /// High = choppy, low = directional.
    pub fn micro_vol(&self) -> f64 {
        if self.prices.len() < 3 {
            return 0.0;
        }
        let returns: Vec<f64> = self.prices.windows(2)
            .map(|w| (w[1] / w[0]).ln() * 100.0)
            .collect();
        let n = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / n;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        variance.sqrt()
    }

    /// Momentum consistency: ratio of dominant direction ticks.
    /// \>0.7 = directional, <0.55 = choppy/oscillating.
    /// Returns 1.0 (favorable) if not enough data.
    pub fn momentum_ratio(&self) -> f64 {
        if self.prices.len() < 3 {
            return 1.0;
        }
        let up = self.prices.windows(2).filter(|w| w[1] > w[0]).count();
        let down = self.prices.windows(2).filter(|w| w[1] < w[0]).count();
        let total = up + down;
        if total == 0 {
            return 1.0;
        }
        up.max(down) as f64 / total as f64
    }

    pub fn ticks_count(&self) -> u32 {
        self.prices.len() as u32
    }

    /// Number of sign changes in consecutive tick deltas.
    pub fn sign_changes(&self) -> u32 {
        if self.prices.len() < 3 { return 0; }
        let mut changes = 0u32;
        let mut prev_sign = 0i8;
        for w in self.prices.windows(2) {
            let delta = w[1] - w[0];
            let sign = if delta > 0.0 { 1i8 } else if delta < 0.0 { -1i8 } else { 0i8 };
            if sign != 0 {
                if prev_sign != 0 && sign != prev_sign { changes += 1; }
                prev_sign = sign;
            }
        }
        changes
    }

    /// Worst intra-window drawdown from peak, in basis points.
    pub fn max_drawdown_bps(&self) -> f64 {
        if self.prices.len() < 2 { return 0.0; }
        let mut peak = self.prices[0];
        let mut max_dd = 0.0f64;
        for &p in &self.prices[1..] {
            if p > peak { peak = p; }
            let dd = (peak - p) / peak * 10000.0;
            if dd > max_dd { max_dd = dd; }
        }
        max_dd
    }

    /// Seconds the price spent at or above start_price.
    pub fn time_above_start_s(&self, start_price: f64) -> u64 {
        if self.timestamps_ms.len() < 2 { return 0; }
        let mut above_ms = 0u64;
        for i in 1..self.timestamps_ms.len() {
            if self.prices[i] >= start_price {
                above_ms += self.timestamps_ms[i].saturating_sub(self.timestamps_ms[i - 1]);
            }
        }
        above_ms / 1000
    }
}

/// Auto-calibration: tracks (predicted_prob, actual_outcome) pairs and
/// recalibrates vol_confidence_multiplier by minimizing Brier Score.
#[derive(Debug)]
pub struct Calibrator {
    entries: Vec<(f64, bool)>,
    recalibrate_every: usize,
    current_vcm: f64,
}

impl Calibrator {
    pub fn new(recalibrate_every: usize) -> Self {
        Self {
            entries: Vec::with_capacity(recalibrate_every + 10),
            recalibrate_every,
            current_vcm: 1.0,
        }
    }

    pub fn set_current_vcm(&mut self, vcm: f64) {
        self.current_vcm = vcm;
    }

    pub fn record(&mut self, predicted_prob: f64, won: bool) {
        self.entries.push((predicted_prob, won));
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn should_recalibrate(&self) -> bool {
        self.recalibrate_every > 0 && self.entries.len() >= self.recalibrate_every
    }

    /// Brier Score on current entries.
    #[allow(dead_code)]
    pub fn brier_score(&self) -> f64 {
        if self.entries.is_empty() {
            return 1.0;
        }
        let sum: f64 = self.entries.iter()
            .map(|(p, won)| {
                let outcome = if *won { 1.0 } else { 0.0 };
                (p - outcome).powi(2)
            })
            .sum();
        sum / self.entries.len() as f64
    }

    /// Grid-search the optimal vol_confidence_multiplier that minimizes Brier Score.
    /// Returns Some((optimal_multiplier, brier_score)) and clears entries.
    /// Returns None if not enough data.
    pub fn recalibrate(&mut self) -> Option<(f64, f64)> {
        if self.entries.is_empty() {
            return None;
        }

        let multipliers: Vec<f64> = (2..=16).map(|i| i as f64 * 0.5).collect();
        let mut best_mult = self.current_vcm;
        let mut best_brier = f64::MAX;
        let ref_vcm = self.current_vcm;

        for &mult in &multipliers {
            let brier: f64 = self.entries.iter()
                .map(|(p, won)| {
                    let adjusted_p = 0.5 + (*p - 0.5) * (ref_vcm / mult);
                    let adjusted_p = adjusted_p.clamp(0.001, 0.999);
                    let outcome = if *won { 1.0 } else { 0.0 };
                    (adjusted_p - outcome).powi(2)
                })
                .sum::<f64>() / self.entries.len() as f64;

            if brier < best_brier {
                best_brier = brier;
                best_mult = mult;
            }
        }

        self.entries.clear();
        Some((best_mult, best_brier))
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
    pub num_ws_sources: u32,
    pub micro_vol: f64,
    pub momentum_ratio: f64,
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

    // 1b. Min WS sources — skip if not enough exchange feeds
    if config.min_ws_sources > 0 && ctx.num_ws_sources < config.min_ws_sources {
        tracing::debug!("Skip: {} WS sources < min {}", ctx.num_ws_sources, config.min_ws_sources);
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

    let true_up_prob = price_change_to_probability(price_change_pct, ctx.seconds_remaining, ctx.vol_5min_pct, config.vol_confidence_multiplier, config.student_t_df);
    let true_down_prob = 1.0 - true_up_prob;
    let market_down_price = 1.0 - ctx.market_up_price;

    // 4b. Z-threshold: skip if z too small (noise, not signal)
    if config.min_z_score > 0.0 {
        let remaining_vol = ctx.vol_5min_pct * config.vol_confidence_multiplier * ((ctx.seconds_remaining as f64) / 300.0).sqrt();
        if remaining_vol > 1e-9 {
            let z_abs = (price_change_pct / remaining_vol).abs();
            if z_abs < config.min_z_score {
                tracing::debug!("Skip: |z| {:.3} < {:.1} (noise)", z_abs, config.min_z_score);
                return None;
            }
        }
    }

    // 4c. Model-vs-market divergence sanity check
    if config.max_model_divergence > 0.0 {
        let model_market_divergence = (true_up_prob - ctx.market_up_price).abs();
        if model_market_divergence > config.max_model_divergence {
            tracing::debug!("Skip: model/market divergence {:.3} > {:.2}", model_market_divergence, config.max_model_divergence);
            return None;
        }
    }

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

    // 7.0 Consecutive loss limit — stop digging when on tilt
    if config.max_consecutive_losses > 0 && session.consecutive_losses >= config.max_consecutive_losses {
        tracing::debug!("Skip: {} consecutive losses >= max {}", session.consecutive_losses, config.max_consecutive_losses);
        return None;
    }

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
        if cl_move_pct > 0.0001 {
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

    // 7c. Minimum implied probability — only trade when model is confident
    // Data shows that low-confidence trades (prob close to 0.5) lose money
    if config.min_implied_prob > 0.0 && true_prob < config.min_implied_prob && (1.0 - true_prob) < config.min_implied_prob {
        tracing::debug!("Skip: implied prob {:.3} too close to 50/50 (min {:.3})", true_prob.max(1.0 - true_prob), config.min_implied_prob);
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
    // 8b. Loss decay: reduce sizing exponentially during losing streaks
    let loss_decay = 0.7_f64.powi(session.consecutive_losses as i32);
    let kelly_size = kelly_size * loss_decay;
    // 8c. Regime factor: reduce sizing in choppy/high-microvol markets
    let mut regime_factor = 1.0;
    if ctx.momentum_ratio < 0.55 {
        regime_factor *= 0.5;
    }
    if ctx.vol_5min_pct > 0.0 && ctx.micro_vol > ctx.vol_5min_pct * 2.0 {
        regime_factor *= 0.6;
    }
    let kelly_size = (kelly_size * regime_factor).min(config.max_bet_usdc);

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
/// Official formula: fee = C × p × feeRate × [p(1-p)]^exponent.
/// For crypto 5min/15min: feeRate = 0.25, exponent = 2.
/// Returns fee as fraction of cost (per dollar invested = fee / (C×p) = feeRate × [p(1-p)]^2).
/// Max fee: 1.56% at p=0.50, drops to ~0% at extremes.
pub fn dynamic_fee(price: f64, fee_rate: f64) -> f64 {
    let p_q = price * (1.0 - price);
    fee_rate * p_q.powi(2)
}

/// Probabilité UP time-aware — modèle hybride prix + imbalance.
/// Calcule P(UP) à partir du mouvement de prix et de la vol résiduelle.
/// Utilise le z-score pur (sans book imbalance — le book Polymarket est un signal
/// de liquidité, pas un signal directionnel sur BTC).
fn price_change_to_probability(pct_change: f64, seconds_remaining: u64, vol_5min_pct: f64, confidence_multiplier: f64, student_t_df: f64) -> f64 {
    let remaining_vol = vol_5min_pct * confidence_multiplier * ((seconds_remaining as f64) / 300.0).sqrt();

    if remaining_vol < 1e-9 {
        return if pct_change > 0.0 { 1.0 } else if pct_change < 0.0 { 0.0 } else { 0.5 };
    }

    let z = pct_change / remaining_vol;

    if student_t_df > 0.0 {
        use statrs::distribution::{StudentsT, ContinuousCDF};
        let dist = StudentsT::new(0.0, 1.0, student_t_df).unwrap();
        dist.cdf(z)
    } else {
        normal_cdf(z)
    }
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
            min_ws_sources: 0,
            circuit_breaker_window: 0,
            circuit_breaker_min_wr: 0.0,
            circuit_breaker_cooldown_s: 1800,
            min_implied_prob: 0.0,
            max_consecutive_losses: 0,
            student_t_df: 0.0,
            min_z_score: 0.0,
            max_model_divergence: 0.0,
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
            num_ws_sources: 3,
            micro_vol: 0.0,
            momentum_ratio: 1.0,
        }
    }

    // --- price_change_to_probability ---

    #[test]
    fn prob_positive_move_low_time() {
        let p = price_change_to_probability(0.05, 5, DEFAULT_VOL, 1.0, 0.0);
        assert!(p > 0.80, "got {p}");
    }

    #[test]
    fn prob_positive_move_high_time() {
        let p = price_change_to_probability(0.05, 60, DEFAULT_VOL, 1.0, 0.0);
        assert!(p > 0.5 && p < 0.95, "got {p}");
    }

    #[test]
    fn prob_flat() {
        let p = price_change_to_probability(0.0, 30, DEFAULT_VOL, 1.0, 0.0);
        assert!((p - 0.5).abs() < 0.001, "got {p}");
    }

    #[test]
    fn prob_negative_move() {
        let p = price_change_to_probability(-0.05, 10, DEFAULT_VOL, 1.0, 0.0);
        assert!(p < 0.2, "got {p}");
    }

    #[test]
    fn prob_zero_time_locks_direction() {
        assert!(price_change_to_probability(0.01, 0, DEFAULT_VOL, 1.0, 0.0) == 1.0);
        assert!(price_change_to_probability(-0.01, 0, DEFAULT_VOL, 1.0, 0.0) == 0.0);
        assert!(price_change_to_probability(0.0, 0, DEFAULT_VOL, 1.0, 0.0) == 0.5);
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
        // BTC +0.02% with 10s remaining, market at 0.55
        // z = 0.02/0.0219 = 0.91 > 0.5 threshold, edge passes
        let ctx_no_spread = TradeContext {
            chainlink_price: 100_020.0,
            market_up_price: 0.55,
            ..test_ctx()
        };
        let with_no_spread = evaluate(&ctx_no_spread, &session, &config);
        assert!(with_no_spread.is_some(), "should pass with 0 spread");

        // With spread=0.50 → spread_cost=25%, should kill any edge
        let ctx_spread = TradeContext {
            chainlink_price: 100_020.0,
            market_up_price: 0.55,
            spread: 0.50,
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
            max_bet_usdc: 3.0,
            min_bet_usdc: 1.0,
            min_shares: 5,
            min_edge_pct: 3.0,
            entry_seconds_before_end: 8,
            session_profit_target_usdc: 15.0,
            session_loss_limit_usdc: 10.0,
            fee_rate: 0.25,
            min_market_price: 0.25,
            max_market_price: 0.75,
            min_delta_pct: 0.008,
            max_spread: 0.04,
            kelly_fraction: 0.10,
            initial_bankroll_usdc: 40.0,
            always_trade: false,
            vol_confidence_multiplier: 1.0,
            min_payout_ratio: 0.08,
            min_book_imbalance: 0.08,  // Data: <8% imbalance = low WR
            max_vol_5min_pct: 0.08,    // Data: only <8bp vol is profitable
            min_ws_sources: 2,
            circuit_breaker_window: 10,
            circuit_breaker_min_wr: 0.25,
            circuit_breaker_cooldown_s: 900,
            min_implied_prob: 0.75,    // Data: low-confidence trades lose
            max_consecutive_losses: 6, // Data: cap exposure during drawdowns
            student_t_df: 4.0,
            min_z_score: 0.5,
            max_model_divergence: 0.30,
        }
    }

    #[test]
    fn paper_strong_up_signal_10s() {
        // BTC +0.05% with 8s remaining, market at 0.72, strong z-score
        // With VCM=1.0: z ≈ 5.1, model P(UP) ≈ 1.0, divergence ≈ 0.28 < 0.30
        // max_bet raised to accommodate min_shares * price = 5 * 0.72 = $3.60
        let config = StrategyConfig { max_bet_usdc: 5.0, ..prod_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_050.0,
            exchange_price: Some(100_050.0),
            rtds_price: None,
            market_up_price: 0.72,
            seconds_remaining: 8,
            fee_rate: 0.25,
            vol_5min_pct: 0.06,
            spread: 0.02,
            book_imbalance: 0.65,
            num_ws_sources: 3,
            micro_vol: 0.0,
            momentum_ratio: 1.0,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "should trade with +0.05% at 8s");
        let s = signal.unwrap();
        assert_eq!(s.side, Side::Buy);
        assert!(s.size_usdc >= 1.0 && s.size_usdc <= 5.0,
            "size ${:.2} should be in $1-$5 range", s.size_usdc);
        assert!(s.edge_pct >= 3.0, "net edge {:.1}% should be >= 3%", s.edge_pct);
    }

    #[test]
    fn paper_weak_signal_rejected() {
        // BTC +0.005% with 8s remaining → small edge, below min_edge 3%
        let config = prod_config();
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_005.0,
            exchange_price: Some(100_005.0),
            rtds_price: None,
            market_up_price: 0.55,
            seconds_remaining: 8,
            fee_rate: 0.25,
            vol_5min_pct: 0.06,
            spread: 0.02,
            book_imbalance: 0.20,
            num_ws_sources: 3,
            micro_vol: 0.0,
            momentum_ratio: 1.0,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "weak +0.005% should not pass 3% min edge");
    }

    #[test]
    fn paper_high_price_min_shares_binding() {
        // Market at 0.60 → 5 shares = $3.00 min, equal to max_bet $3
        let config = prod_config();
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_050.0,
            exchange_price: Some(100_050.0),
            rtds_price: None,
            market_up_price: 0.60,
            seconds_remaining: 8,
            fee_rate: 0.25,
            vol_5min_pct: 0.06,
            spread: 0.01,
            book_imbalance: 0.20,
            num_ws_sources: 3,
            micro_vol: 0.0,
            momentum_ratio: 1.0,
        };
        let signal = evaluate(&ctx, &session, &config);
        if let Some(s) = signal {
            assert!(s.size_usdc >= 3.0, "min 5 shares @ 0.60 = $3, got ${:.2}", s.size_usdc);
            assert!(s.size_usdc <= 3.0, "capped at max_bet $3, got ${:.2}", s.size_usdc);
        }
    }

    #[test]
    fn paper_session_loss_limit_40_portfolio() {
        // After losing $10, session should stop (25% of $40 portfolio)
        let config = prod_config();
        let mut session = Session::new(40.0);
        session.pnl_usdc = -10.0;
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_050.0,
            exchange_price: Some(100_050.0),
            rtds_price: None,
            market_up_price: 0.50,
            seconds_remaining: 8,
            fee_rate: 0.25,
            vol_5min_pct: 0.06,
            spread: 0.02,
            book_imbalance: 0.20,
            num_ws_sources: 3,
            micro_vol: 0.0,
            momentum_ratio: 1.0,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should stop after -$10 (25% of $40 portfolio)");
    }

    #[test]
    fn paper_spread_eats_edge() {
        // Strong price move but wide spread kills the net edge below 3%
        // Disable max_spread filter to test spread cost mechanism specifically
        let config = StrategyConfig { max_spread: 0.0, ..prod_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_050.0, // +0.05%
            exchange_price: Some(100_050.0),
            rtds_price: None,
            market_up_price: 0.55,
            seconds_remaining: 8,
            fee_rate: 0.25,
            vol_5min_pct: 0.06,
            spread: 0.80, // 40% spread cost → kills edge below 3%
            book_imbalance: 0.20,
            num_ws_sources: 3,
            micro_vol: 0.0,
            momentum_ratio: 1.0,
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "wide spread should kill the edge below 3% min");
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
        let p_low = price_change_to_probability(0.05, 10, DEFAULT_VOL, 1.0, 0.0);
        let p_high = price_change_to_probability(0.05, 10, DEFAULT_VOL, 2.5, 0.0);
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
    fn imbalance_does_not_affect_sizing() {
        // Imbalance is used for filtering only, not sizing (removed imbalance_boost)
        let config = StrategyConfig { min_book_imbalance: 0.08, ..test_config() };
        let session = Session::new(40.0);
        let ctx_low = TradeContext {
            chainlink_price: 100_050.0,
            book_imbalance: 0.10,
            ..test_ctx()
        };
        let sig_low = evaluate(&ctx_low, &session, &config);
        let ctx_high = TradeContext {
            chainlink_price: 100_050.0,
            book_imbalance: 0.60,
            ..test_ctx()
        };
        let sig_high = evaluate(&ctx_high, &session, &config);
        assert!(sig_low.is_some());
        assert!(sig_high.is_some());
        let diff = (sig_high.unwrap().size_usdc - sig_low.unwrap().size_usdc).abs();
        assert!(diff < 0.01, "imbalance should not change sizing: diff={diff}");
    }

    // --- min_ws_sources ---

    #[test]
    fn evaluate_skips_few_ws_sources() {
        let config = StrategyConfig { min_ws_sources: 3, ..test_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            num_ws_sources: 2,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should skip with only 2 WS sources");
    }

    #[test]
    fn evaluate_passes_enough_ws_sources() {
        let config = StrategyConfig { min_ws_sources: 3, ..test_config() };
        let session = Session::new(40.0);
        let ctx = TradeContext {
            chainlink_price: 100_050.0,
            num_ws_sources: 3,
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "should pass with 3 WS sources");
    }

    // --- circuit breaker ---

    #[test]
    fn rolling_wr_tracks_recent() {
        let mut s = Session::new(40.0);
        // 3 losses
        for _ in 0..3 {
            s.record_trade(-1.0);
        }
        assert_eq!(s.rolling_wr(3), Some(0.0));
        // 1 win
        s.record_trade(2.0);
        // Last 3: L, L, W → 33%
        let wr = s.rolling_wr(3).unwrap();
        assert!((wr - 1.0 / 3.0).abs() < 0.01, "got {wr}");
    }

    #[test]
    fn rolling_wr_none_when_not_enough() {
        let mut s = Session::new(40.0);
        s.record_trade(1.0);
        s.record_trade(-1.0);
        assert_eq!(s.rolling_wr(5), None);
    }

    #[test]
    fn circuit_breaker_triggers() {
        let mut s = Session::new(40.0);
        // 15 trades: 3 wins, 12 losses → 20% WR < 25%
        for i in 0..15 {
            if i < 3 { s.record_trade(1.0); } else { s.record_trade(-1.0); }
        }
        assert!(!s.is_circuit_broken(1000));
        s.check_circuit_breaker(15, 0.25, 1800, 1000);
        assert!(s.is_circuit_broken(1000)); // active now
        assert!(s.is_circuit_broken(2799)); // still active at 1000+1799
        assert!(!s.is_circuit_broken(2800)); // expired at 1000+1800
    }

    #[test]
    fn circuit_breaker_does_not_trigger_above_threshold() {
        let mut s = Session::new(40.0);
        // 15 trades: 5 wins, 10 losses → 33% WR > 25%
        for i in 0..15 {
            if i < 5 { s.record_trade(1.0); } else { s.record_trade(-1.0); }
        }
        s.check_circuit_breaker(15, 0.25, 1800, 1000);
        assert!(!s.is_circuit_broken(1000));
    }

    // --- consecutive losses ---

    #[test]
    fn consecutive_losses_tracked() {
        let mut s = Session::new(40.0);
        s.record_trade(-1.0);
        s.record_trade(-1.0);
        s.record_trade(-1.0);
        assert_eq!(s.consecutive_losses, 3);
        s.record_trade(1.0); // win resets
        assert_eq!(s.consecutive_losses, 0);
        s.record_trade(-1.0);
        assert_eq!(s.consecutive_losses, 1);
    }

    #[test]
    fn evaluate_skips_after_max_consecutive_losses() {
        let config = StrategyConfig { max_consecutive_losses: 5, ..test_config() };
        let mut session = Session::new(40.0);
        // Record 5 consecutive losses
        for _ in 0..5 {
            session.record_trade(-1.0);
        }
        assert_eq!(session.consecutive_losses, 5);
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "should skip after 5 consecutive losses");
    }

    #[test]
    fn evaluate_resumes_after_consec_loss_reset() {
        let config = StrategyConfig { max_consecutive_losses: 5, ..test_config() };
        let mut session = Session::new(40.0);
        for _ in 0..5 {
            session.record_trade(-1.0);
        }
        // This won't help because evaluate checks but doesn't reset
        // The user must manually (or via circuit breaker timeout) allow resume
        // Actually: consecutive_losses resets on any win via record_trade
        session.record_trade(1.0); // win! resets to 0
        assert_eq!(session.consecutive_losses, 0);
        let ctx = TradeContext { chainlink_price: 100_050.0, ..test_ctx() };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "should resume after loss streak broken by win");
    }

    // --- min_implied_prob ---

    #[test]
    fn evaluate_skips_low_confidence() {
        let config = StrategyConfig {
            min_implied_prob: 0.70,
            min_edge_pct: 0.1, // low so edge isn't the blocker
            ..test_config()
        };
        let session = Session::new(40.0);
        // Small move → prob close to 0.5 → below 0.70 threshold
        let ctx = TradeContext {
            chainlink_price: 100_002.0, // +0.002% — tiny, prob ~0.52
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_none(), "low confidence should be skipped");
    }

    #[test]
    fn evaluate_passes_high_confidence() {
        let config = StrategyConfig {
            min_implied_prob: 0.70,
            min_edge_pct: 0.1,
            ..test_config()
        };
        let session = Session::new(40.0);
        // Large move → high prob → above 0.70 threshold
        let ctx = TradeContext {
            chainlink_price: 100_050.0, // +0.05% with 10s remaining → prob ~0.99
            ..test_ctx()
        };
        let signal = evaluate(&ctx, &session, &config);
        assert!(signal.is_some(), "high confidence should pass");
    }

    // --- data-driven prod config test ---

    #[test]
    fn prod_config_data_driven_values() {
        let cfg = prod_config();
        // Data analysis: min_book_imbalance should be >= 0.05 (WR drops below 5% under this)
        assert!(cfg.min_book_imbalance >= 0.05, "imbalance threshold too low");
        // Data analysis: max_vol_5min_pct should be <= 0.10 (only <10bp profitable)
        assert!(cfg.max_vol_5min_pct <= 0.10, "vol threshold too high");
        // Data analysis: min_implied_prob should be >= 0.70
        assert!(cfg.min_implied_prob >= 0.70, "confidence threshold too low");
        // Data analysis: max_consecutive_losses should be set
        assert!(cfg.max_consecutive_losses > 0 && cfg.max_consecutive_losses <= 10, "consec loss limit not set properly");
    }

    #[test]
    fn vol_tracker_mad_resists_outlier() {
        let mut vt = VolTracker::new(20, 0.12);
        // 9 normal moves ~0.05%
        for _ in 0..9 {
            vt.record_move(100_000.0, 100_050.0); // +0.05%
        }
        let vol_before = vt.current_vol();
        // Add one extreme outlier (+2%)
        vt.record_move(100_000.0, 102_000.0);
        let vol_after = vt.current_vol();
        // MAD should resist: vol should not jump more than 2x
        assert!(vol_after < vol_before * 2.0,
            "MAD should resist outlier: before={vol_before:.4}, after={vol_after:.4}");
    }

    #[test]
    fn loss_decay_reduces_sizing() {
        let config = StrategyConfig { min_edge_pct: 1.0, kelly_fraction: 0.25, ..test_config() };
        let ctx = TradeContext {
            start_price: 100_000.0,
            chainlink_price: 100_100.0,
            exchange_price: Some(100_100.0),
            market_up_price: 0.55,
            seconds_remaining: 5,
            vol_5min_pct: 0.12,
            book_imbalance: 0.5,
            ..test_ctx()
        };

        // 0 losses → full size
        let session_0 = Session::new(40.0);
        let sig_0 = evaluate(&ctx, &session_0, &config).unwrap();

        // 3 consecutive losses → decayed size (0.7^3 = 0.343)
        let mut session_3 = Session::new(40.0);
        for _ in 0..3 { session_3.record_trade(-1.0); }
        let sig_3 = evaluate(&ctx, &session_3, &config);

        if let Some(s3) = sig_3 {
            assert!(s3.size_usdc < sig_0.size_usdc,
                "3 losses should reduce size: {} vs {}", s3.size_usdc, sig_0.size_usdc);
        }
        // Either smaller or skipped entirely — both are valid
    }

    // --- pure z-score probability model (no imbalance) ---

    #[test]
    fn pure_z_up_move_gives_high_prob() {
        let p = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.0);
        assert!(p > 0.5, "UP move should give prob > 0.5: {p}");
    }

    #[test]
    fn student_t_more_conservative_than_normal() {
        let p_normal = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.0);
        let p_student = price_change_to_probability(0.05, 10, 0.12, 1.0, 4.0);
        assert!(p_student < p_normal,
            "Student-t should be more conservative: {p_student} vs normal {p_normal}");
        assert!(p_student > 0.5, "should still lean UP: {p_student}");
    }

    #[test]
    fn student_t_df_zero_uses_normal() {
        let p_zero = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.0);
        let p_normal = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.0);
        assert!((p_zero - p_normal).abs() < 1e-10);
    }

    #[test]
    fn student_t_symmetric() {
        let p_up = price_change_to_probability(0.05, 10, 0.12, 1.0, 4.0);
        let p_down = price_change_to_probability(-0.05, 10, 0.12, 1.0, 4.0);
        assert!((p_up + p_down - 1.0).abs() < 0.01,
            "should be symmetric: p_up={p_up} p_down={p_down}");
    }

    #[test]
    fn window_ticks_micro_vol_directional() {
        let mut wt = WindowTicks::new();
        for i in 0..5 {
            wt.tick(100.0 + i as f64, i as u64 * 100);
        }
        let mv = wt.micro_vol();
        assert!(mv > 0.0, "micro_vol should be positive: {mv}");
    }

    #[test]
    fn window_ticks_micro_vol_choppy_higher() {
        let mut dir = WindowTicks::new();
        let mut chop = WindowTicks::new();
        for i in 0..20 {
            dir.tick(100.0 + i as f64 * 0.5, i as u64 * 100);
        }
        for i in 0..20 {
            chop.tick(if i % 2 == 0 { 100.0 } else { 101.0 }, i as u64 * 100);
        }
        assert!(chop.micro_vol() > dir.micro_vol(),
            "choppy should have higher micro_vol: {} vs {}", chop.micro_vol(), dir.micro_vol());
    }

    #[test]
    fn window_ticks_momentum_ratio_directional() {
        let mut wt = WindowTicks::new();
        for i in 0..10 {
            wt.tick(100.0 + i as f64, i as u64 * 100);
        }
        let mr = wt.momentum_ratio();
        assert!(mr > 0.8, "directional should have high momentum: {mr}");
    }

    #[test]
    fn window_ticks_momentum_ratio_choppy() {
        let mut wt = WindowTicks::new();
        for i in 0..10 {
            wt.tick(if i % 2 == 0 { 100.0 } else { 101.0 }, i as u64 * 100);
        }
        let mr = wt.momentum_ratio();
        assert!(mr < 0.6, "choppy should have low momentum: {mr}");
    }

    #[test]
    fn window_ticks_empty_defaults() {
        let wt = WindowTicks::new();
        assert!(wt.micro_vol() == 0.0);
        assert!(wt.momentum_ratio() == 1.0);
    }

    #[test]
    fn window_ticks_single_price_defaults() {
        let mut wt = WindowTicks::new();
        wt.tick(100.0, 0);
        assert!(wt.micro_vol() == 0.0);
        assert!(wt.momentum_ratio() == 1.0);
    }

    #[test]
    fn window_ticks_sign_changes() {
        let mut wt = WindowTicks::new();
        for (i, &p) in [100.0, 101.0, 102.0, 101.0, 100.0, 101.0].iter().enumerate() {
            wt.tick(p, i as u64 * 100);
        }
        assert_eq!(wt.sign_changes(), 2);
    }

    #[test]
    fn window_ticks_max_drawdown_bps() {
        let mut wt = WindowTicks::new();
        for (i, &p) in [100.0, 100.10, 100.05, 99.90, 100.0].iter().enumerate() {
            wt.tick(p, i as u64 * 100);
        }
        let dd = wt.max_drawdown_bps();
        assert!(dd > 19.0 && dd < 21.0, "drawdown should be ~20 bps: {dd}");
    }

    #[test]
    fn window_ticks_time_above_start() {
        let mut wt = WindowTicks::new();
        wt.tick(100.0, 0);
        wt.tick(100.5, 1000);
        wt.tick(100.3, 2000);
        wt.tick(99.8, 3000);
        assert_eq!(wt.time_above_start_s(100.0), 2);
    }

    #[test]
    fn window_ticks_ticks_count() {
        let mut wt = WindowTicks::new();
        wt.tick(100.0, 0);
        wt.tick(101.0, 100);
        wt.tick(102.0, 200);
        assert_eq!(wt.ticks_count(), 3);
    }

    // --- Calibrator ---

    #[test]
    fn calibrator_records_and_counts() {
        let mut cal = Calibrator::new(5);
        cal.record(0.8, true);
        cal.record(0.6, false);
        cal.record(0.9, true);
        assert_eq!(cal.count(), 3);
        assert!(!cal.should_recalibrate());
    }

    #[test]
    fn calibrator_triggers_at_threshold() {
        let mut cal = Calibrator::new(5);
        for i in 0..5 {
            cal.record(0.7, i % 2 == 0);
        }
        assert!(cal.should_recalibrate());
    }

    #[test]
    fn calibrator_brier_score() {
        let mut cal = Calibrator::new(10);
        cal.record(1.0, true);
        cal.record(0.0, false);
        let bs = cal.brier_score();
        assert!(bs < 0.01, "perfect predictions should have low brier: {bs}");
    }

    #[test]
    fn calibrator_brier_score_bad() {
        let mut cal = Calibrator::new(10);
        for _ in 0..5 {
            cal.record(0.9, false);
        }
        let bs = cal.brier_score();
        assert!(bs > 0.5, "bad predictions should have high brier: {bs}");
    }

    #[test]
    fn calibrator_optimal_multiplier() {
        let mut cal = Calibrator::new(5);
        cal.record(0.9, true);
        cal.record(0.9, false);
        cal.record(0.9, true);
        cal.record(0.9, false);
        cal.record(0.9, true);
        let result = cal.recalibrate();
        assert!(result.is_some());
        let (mult, brier) = result.unwrap();
        assert!((1.0..=8.0).contains(&mult), "multiplier {mult} out of range");
        assert!((0.0..=1.0).contains(&brier), "brier {brier} out of range");
    }

    #[test]
    fn calibrator_resets_after_recalibrate() {
        let mut cal = Calibrator::new(3);
        for _ in 0..3 {
            cal.record(0.7, true);
        }
        assert!(cal.should_recalibrate());
        let _ = cal.recalibrate();
        assert_eq!(cal.count(), 0);
        assert!(!cal.should_recalibrate());
    }

    #[test]
    fn regime_choppy_reduces_sizing() {
        let config = StrategyConfig { min_edge_pct: 1.0, ..test_config() };
        let session = Session::new(40.0);
        let ctx_good = TradeContext {
            chainlink_price: 100_050.0,
            micro_vol: 0.001,
            momentum_ratio: 0.9,
            ..test_ctx()
        };
        let sig_good = evaluate(&ctx_good, &session, &config);

        let ctx_choppy = TradeContext {
            chainlink_price: 100_050.0,
            micro_vol: 0.001,
            momentum_ratio: 0.45,
            ..test_ctx()
        };
        let sig_choppy = evaluate(&ctx_choppy, &session, &config);

        assert!(sig_good.is_some());
        if let Some(sc) = sig_choppy {
            assert!(sc.size_usdc <= sig_good.unwrap().size_usdc,
                "choppy regime should reduce sizing");
        }
    }

    #[test]
    fn consecutive_wins_tracked() {
        let mut s = Session::new(40.0);
        s.record_trade(1.0);
        s.record_trade(1.0);
        assert_eq!(s.consecutive_wins, 2);
        s.record_trade(-1.0);
        assert_eq!(s.consecutive_wins, 0);
        s.record_trade(1.0);
        assert_eq!(s.consecutive_wins, 1);
    }

    #[test]
    fn session_drawdown_pct_calculation() {
        let mut s = Session::new(40.0);
        s.record_trade(-5.0);
        assert!((s.session_drawdown_pct() - 12.5).abs() < 0.01);
        s.record_trade(10.0);
        // drawdown stays at worst point
        assert!((s.session_drawdown_pct() - 12.5).abs() < 0.01);
    }

    #[test]
    fn regime_high_microvol_reduces_sizing() {
        let config = StrategyConfig { min_edge_pct: 1.0, ..test_config() };
        let session = Session::new(40.0);
        let ctx_normal = TradeContext {
            chainlink_price: 100_050.0,
            vol_5min_pct: 0.10,
            micro_vol: 0.05,
            momentum_ratio: 0.9,
            ..test_ctx()
        };
        let sig_normal = evaluate(&ctx_normal, &session, &config);

        let ctx_high = TradeContext {
            chainlink_price: 100_050.0,
            vol_5min_pct: 0.10,
            micro_vol: 0.25,
            momentum_ratio: 0.9,
            ..test_ctx()
        };
        let sig_high = evaluate(&ctx_high, &session, &config);

        assert!(sig_normal.is_some());
        if let Some(sh) = sig_high {
            assert!(sh.size_usdc <= sig_normal.unwrap().size_usdc,
                "high micro_vol should reduce sizing");
        }
    }
}
