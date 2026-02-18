use crate::polymarket::Side;
use std::collections::VecDeque;

/// Configuration de la stratégie (chargée depuis config.toml).
#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub max_bet_usdc: f64,
    pub min_edge_pct: f64,
    pub entry_seconds_before_end: u64,
    pub session_profit_target_usdc: f64,
    pub session_loss_limit_usdc: f64,
    pub fee_rate_bps: u32,
    pub min_market_price: f64,
    pub max_market_price: f64,
}

/// Signal de trade émis par la stratégie.
#[derive(Debug)]
pub struct Signal {
    pub side: Side,
    pub edge_pct: f64,
    pub size_usdc: f64,
    pub price: f64,
}

/// État de la session (P&L, nombre de trades).
#[derive(Debug, Default)]
pub struct Session {
    pub pnl_usdc: f64,
    pub trades: u32,
    pub wins: u32,
}

impl Session {
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

/// Évalue si on doit trader sur cet intervalle.
/// `exchange_price` : prix WS exchanges (plus frais), fallback sur `chainlink_price`.
pub fn evaluate(
    start_price: f64,
    chainlink_price: f64,
    exchange_price: Option<f64>,
    market_up_price: f64,
    seconds_remaining: u64,
    session: &Session,
    config: &StrategyConfig,
    fee_rate_bps: u32,
    vol_5min_pct: f64,
) -> Option<Signal> {
    // 1. Session limits
    if session.pnl_usdc >= config.session_profit_target_usdc {
        return None;
    }
    if session.pnl_usdc <= -config.session_loss_limit_usdc {
        return None;
    }

    // 2. Fenêtre d'entrée
    if seconds_remaining > config.entry_seconds_before_end {
        return None;
    }

    // 3. Validation inputs + filtre zone de prix
    if start_price <= 0.0 || !(0.01..=0.99).contains(&market_up_price) {
        return None;
    }
    if market_up_price < config.min_market_price || market_up_price > config.max_market_price {
        return None;
    }

    // 4. Cohérence Chainlink / exchanges — skip si divergence directionnelle
    //    Tolérance : si Chainlink est quasi-flat (<0.001%), on fait confiance aux exchanges
    if let Some(ex_price) = exchange_price {
        let cl_move_pct = ((chainlink_price - start_price) / start_price).abs();
        if cl_move_pct > 0.00001 {
            let chainlink_up = chainlink_price > start_price;
            let exchange_up = ex_price > start_price;
            if chainlink_up != exchange_up {
                tracing::debug!("Skip: divergence CL/WS (CL={chainlink_price:.2}, WS={ex_price:.2}, start={start_price:.2})");
                return None;
            }
        }
    }

    // 5. Direction et probabilité estimée (time-aware)
    // Préfère le prix exchange (100-200ms plus frais) si disponible
    let current_price = exchange_price.unwrap_or(chainlink_price);
    let price_change_pct = (current_price - start_price) / start_price * 100.0;
    let true_up_prob = price_change_to_probability(price_change_pct, seconds_remaining, vol_5min_pct);
    let true_down_prob = 1.0 - true_up_prob;
    let market_down_price = 1.0 - market_up_price;

    // 6. Edge — edge_up = -edge_down toujours, on check juste le signe
    let edge_up = true_up_prob - market_up_price;
    let edge_down = true_down_prob - market_down_price;

    let (side, edge, market_price, true_prob) = if edge_up > 0.0 {
        (Side::Buy, edge_up, market_up_price, true_up_prob)
    } else if edge_down > 0.0 {
        (Side::Sell, edge_down, market_down_price, true_down_prob)
    } else {
        return None;
    };

    let edge_pct = edge * 100.0;
    let fee = dynamic_fee(market_price, fee_rate_bps);
    let net_edge_pct = edge_pct - (fee * 100.0);

    if net_edge_pct < config.min_edge_pct {
        return None;
    }

    // 7. Half-Kelly sizing (max_bet sert de cap, pas de bankroll)
    let size = half_kelly(true_prob, market_price, config.max_bet_usdc);
    if size < 0.01 {
        return None;
    }

    tracing::info!(
        "SIGNAL: {} | Edge: {:.1}% (brut {:.1}%, fee {:.2}%) | Δ prix: {:.4}% | Size: ${:.2} | {}s restantes | src: {}",
        if side == Side::Buy { "BUY UP" } else { "BUY DOWN" },
        net_edge_pct, edge_pct, fee * 100.0, price_change_pct, size, seconds_remaining,
        if exchange_price.is_some() { "WS" } else { "CL" },
    );

    Some(Signal { side, edge_pct: net_edge_pct, size_usdc: size, price: market_price })
}

/// Calcule les frais dynamiques Polymarket.
/// fee_rate_bps = 1000 pour les marchés crypto 5min/15min, exponent = 2.
pub fn dynamic_fee(price: f64, fee_rate_bps: u32) -> f64 {
    let p_q = price * (1.0 - price);
    (fee_rate_bps as f64 / 10000.0) * p_q.powi(2)
}

/// Probabilité UP time-aware basée sur un modèle de volatilité.
/// Utilise la vol résiduelle pour pondérer la confiance selon le temps restant.
fn price_change_to_probability(pct_change: f64, seconds_remaining: u64, vol_5min_pct: f64) -> f64 {
    let remaining_vol = vol_5min_pct * ((seconds_remaining as f64) / 300.0).sqrt();

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

/// Half-Kelly Criterion : mise conservatrice.
/// Kelly fraction × max_bet / 2, plafonné à max_bet.
fn half_kelly(p: f64, price: f64, max_bet: f64) -> f64 {
    if price <= 0.0 || price >= 1.0 || p <= 0.0 || p >= 1.0 {
        return 0.0;
    }
    let b = (1.0 - price) / price;
    let q = 1.0 - p;
    let kelly = (b * p - q) / b;
    ((kelly / 2.0) * max_bet).clamp(0.0, max_bet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> StrategyConfig {
        StrategyConfig {
            max_bet_usdc: 2.0,
            min_edge_pct: 1.0,
            entry_seconds_before_end: 30,
            session_profit_target_usdc: 100.0,
            session_loss_limit_usdc: 50.0,
            fee_rate_bps: 1000,
            min_market_price: 0.15,
            max_market_price: 0.85,
        }
    }

    // --- price_change_to_probability ---

    const DEFAULT_VOL: f64 = 0.12;

    #[test]
    fn prob_positive_move_low_time() {
        let p = price_change_to_probability(0.05, 5, DEFAULT_VOL);
        assert!(p > 0.95, "got {p}");
    }

    #[test]
    fn prob_positive_move_high_time() {
        let p = price_change_to_probability(0.05, 60, DEFAULT_VOL);
        assert!(p > 0.5 && p < 0.95, "got {p}");
    }

    #[test]
    fn prob_flat() {
        let p = price_change_to_probability(0.0, 30, DEFAULT_VOL);
        assert!((p - 0.5).abs() < 0.001, "got {p}");
    }

    #[test]
    fn prob_negative_move() {
        let p = price_change_to_probability(-0.05, 10, DEFAULT_VOL);
        assert!(p < 0.1, "got {p}");
    }

    #[test]
    fn prob_zero_time_locks_direction() {
        assert!(price_change_to_probability(0.01, 0, DEFAULT_VOL) == 1.0);
        assert!(price_change_to_probability(-0.01, 0, DEFAULT_VOL) == 0.0);
        assert!(price_change_to_probability(0.0, 0, DEFAULT_VOL) == 0.5);
    }

    // --- half_kelly ---

    #[test]
    fn kelly_positive_edge() {
        let size = half_kelly(0.7, 0.5, 2.0);
        assert!(size > 0.0 && size <= 2.0, "got {size}");
    }

    #[test]
    fn kelly_no_edge() {
        let size = half_kelly(0.5, 0.5, 2.0);
        assert!(size.abs() < 0.001, "got {size}");
    }

    #[test]
    fn kelly_bad_odds() {
        let size = half_kelly(0.3, 0.5, 2.0);
        assert!(size.abs() < 0.001, "got {size}");
    }

    // --- evaluate ---

    #[test]
    fn evaluate_buy_up_signal() {
        let config = test_config();
        let session = Session::default();
        // BTC +0.05% avec 10s restantes, marché à 50/50
        let signal = evaluate(100_000.0, 100_050.0, None, 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_some());
        let s = signal.unwrap();
        assert_eq!(s.side, Side::Buy);
        assert!(s.edge_pct > 1.0);
    }

    #[test]
    fn evaluate_buy_down_signal() {
        let config = test_config();
        let session = Session::default();
        // BTC -0.05% avec 10s restantes, marché à 50/50
        let signal = evaluate(100_000.0, 99_950.0, None, 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_some());
        let s = signal.unwrap();
        assert_eq!(s.side, Side::Sell); // Sell = buy DOWN token
    }

    #[test]
    fn evaluate_no_signal_outside_window() {
        let config = test_config();
        let session = Session::default();
        // 60s restantes > entry_seconds_before_end (30)
        let signal = evaluate(100_000.0, 100_050.0, None, 0.50, 60, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_no_signal_profit_target() {
        let config = test_config();
        let mut session = Session::default();
        session.pnl_usdc = 100.0; // target atteint
        let signal = evaluate(100_000.0, 100_050.0, None, 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_no_signal_loss_limit() {
        let config = test_config();
        let mut session = Session::default();
        session.pnl_usdc = -50.0; // limit atteint
        let signal = evaluate(100_000.0, 100_050.0, None, 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_no_signal_low_edge() {
        let config = test_config();
        let session = Session::default();
        // Marché déjà ajusté à 0.99 → edge < 1% (min_edge_pct)
        let signal = evaluate(100_000.0, 100_050.0, None, 0.99, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_rejects_bad_market_price() {
        let config = test_config();
        let session = Session::default();
        assert!(evaluate(100_000.0, 100_050.0, None, 1.5, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL).is_none());
        assert!(evaluate(100_000.0, 100_050.0, None, 0.0, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL).is_none());
    }

    // --- dynamic_fee ---

    #[test]
    fn dynamic_fee_at_50_50() {
        let fee = dynamic_fee(0.50, 1000);
        assert!((fee - 0.00625).abs() < 0.001, "got {fee}");
    }

    #[test]
    fn dynamic_fee_at_80_20() {
        let fee = dynamic_fee(0.80, 1000);
        // 0.8*0.2 = 0.16, 0.16^2 = 0.0256, * 0.1 = 0.00256
        assert!(fee < 0.003, "got {fee}");
    }

    #[test]
    fn dynamic_fee_at_95_05() {
        let fee = dynamic_fee(0.95, 1000);
        assert!(fee < 0.0003, "got {fee}");
    }

    #[test]
    fn evaluate_rejects_when_fee_exceeds_edge() {
        let config = test_config();
        let session = Session::default();
        // BTC +0.0005% avec 10s restantes, marché à 50/50
        // Edge brut ~0.9%, fee ~0.625% → net edge ~0.28% < min_edge 1%
        let signal = evaluate(100_000.0, 100_000.5, None, 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    // --- price zone filter ---

    #[test]
    fn evaluate_rejects_below_min_market_price() {
        let config = test_config(); // min=0.15
        let session = Session::default();
        // Marché à 0.10 → en dessous de min_market_price
        let signal = evaluate(100_000.0, 100_050.0, None, 0.10, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_rejects_above_max_market_price() {
        let config = test_config(); // max=0.85
        let session = Session::default();
        // Marché à 0.90 → au dessus de max_market_price
        let signal = evaluate(100_000.0, 100_050.0, None, 0.90, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_accepts_70_30() {
        let config = test_config();
        let session = Session::default();
        // Marché à 0.70, dans la zone autorisée, +0.05% avec 10s restantes
        let signal = evaluate(100_000.0, 100_050.0, None, 0.70, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_some());
    }

    // --- exchange_price integration ---

    #[test]
    fn evaluate_uses_exchange_price_when_provided() {
        let config = test_config();
        let session = Session::default();
        // Les deux UP, mais exchange montre un mouvement plus large → signal basé sur exchange
        let signal = evaluate(100_000.0, 100_010.0, Some(100_050.0), 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
        assert!(signal.is_some());
        assert_eq!(signal.unwrap().side, Side::Buy);
    }

    #[test]
    fn evaluate_falls_back_to_chainlink_when_no_exchange() {
        let config = test_config();
        let session = Session::default();
        // exchange_price = None → utilise chainlink_price (+0.05%)
        let signal = evaluate(100_000.0, 100_050.0, None, 0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL);
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
        let session = Session::default();
        // Chainlink dit DOWN (-0.05%), exchanges dit UP (+0.05%) → divergence → None
        let signal = evaluate(
            100_000.0, 99_950.0, Some(100_050.0),
            0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL,
        );
        assert!(signal.is_none());
    }

    #[test]
    fn evaluate_ok_when_both_agree() {
        let config = test_config();
        let session = Session::default();
        // Les deux disent UP → pas de divergence
        let signal = evaluate(
            100_000.0, 100_030.0, Some(100_050.0),
            0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL,
        );
        assert!(signal.is_some());
    }

    #[test]
    fn evaluate_no_divergence_when_chainlink_flat() {
        let config = test_config();
        let session = Session::default();
        // Chainlink flat (== start), exchange UP → tolérance, pas de divergence
        let signal = evaluate(
            100_000.0, 100_000.0, Some(100_050.0),
            0.50, 10, &session, &config, config.fee_rate_bps, DEFAULT_VOL,
        );
        assert!(signal.is_some());
    }
}
