#![allow(dead_code)]

use crate::strategy::StrategyConfig;

/// Returns the "Sniper Conservateur" preset.
/// GTC maker, edge>=3%, kelly=0.10, vol<0.08%.
pub fn sniper() -> StrategyConfig {
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
        vol_confidence_multiplier: 4.0,
        min_payout_ratio: 0.10,
        min_book_imbalance: 0.08,
        max_vol_5min_pct: 0.08,
        min_ws_sources: 2,
        circuit_breaker_window: 10,
        circuit_breaker_min_wr: 0.30,
        circuit_breaker_cooldown_s: 900,
        min_implied_prob: 0.75,
        max_consecutive_losses: 6,
    }
}

/// Returns the "High Conviction Only" preset.
/// Very few trades, extreme conviction. For limited capital.
pub fn conviction() -> StrategyConfig {
    StrategyConfig {
        max_bet_usdc: 5.0,
        min_bet_usdc: 1.0,
        min_shares: 5,
        min_edge_pct: 5.0,
        entry_seconds_before_end: 6,
        session_profit_target_usdc: 20.0,
        session_loss_limit_usdc: 8.0,
        fee_rate: 0.25,
        min_market_price: 0.30,
        max_market_price: 0.70,
        min_delta_pct: 0.015,
        max_spread: 0.03,
        kelly_fraction: 0.15,
        initial_bankroll_usdc: 40.0,
        always_trade: false,
        vol_confidence_multiplier: 3.5,
        min_payout_ratio: 0.15,
        min_book_imbalance: 0.15,
        max_vol_5min_pct: 0.07,
        min_ws_sources: 2,
        circuit_breaker_window: 8,
        circuit_breaker_min_wr: 0.30,
        circuit_breaker_cooldown_s: 1200,
        min_implied_prob: 0.80,
        max_consecutive_losses: 5,
    }
}

/// Returns the "Extreme Zones Scalper" preset.
/// Trade only at 85-95% where fees are near-zero. FOK taker OK.
pub fn scalper() -> StrategyConfig {
    StrategyConfig {
        max_bet_usdc: 4.0,
        min_bet_usdc: 1.0,
        min_shares: 5,
        min_edge_pct: 1.0,
        entry_seconds_before_end: 10,
        session_profit_target_usdc: 12.0,
        session_loss_limit_usdc: 8.0,
        fee_rate: 0.25,
        min_market_price: 0.10,
        max_market_price: 0.90,
        min_delta_pct: 0.003,
        max_spread: 0.05,
        kelly_fraction: 0.20,
        initial_bankroll_usdc: 40.0,
        always_trade: false,
        vol_confidence_multiplier: 3.0,
        min_payout_ratio: 0.05,
        min_book_imbalance: 0.05,
        max_vol_5min_pct: 0.10,
        min_ws_sources: 2,
        circuit_breaker_window: 15,
        circuit_breaker_min_wr: 0.40,
        circuit_breaker_cooldown_s: 600,
        min_implied_prob: 0.85,
        max_consecutive_losses: 6,
    }
}

/// Returns the "Data Farm" preset (dry-run only).
/// Relaxed filters, collects maximum data for analysis.
pub fn farm() -> StrategyConfig {
    StrategyConfig {
        max_bet_usdc: 2.0,
        min_bet_usdc: 1.0,
        min_shares: 5,
        min_edge_pct: 0.5,
        entry_seconds_before_end: 12,
        session_profit_target_usdc: 100.0,
        session_loss_limit_usdc: 100.0,
        fee_rate: 0.25,
        min_market_price: 0.05,
        max_market_price: 0.95,
        min_delta_pct: 0.003,
        max_spread: 0.10,
        kelly_fraction: 0.25,
        initial_bankroll_usdc: 40.0,
        always_trade: false,
        vol_confidence_multiplier: 2.5,
        min_payout_ratio: 0.0,
        min_book_imbalance: 0.0,
        max_vol_5min_pct: 0.0,
        min_ws_sources: 1,
        circuit_breaker_window: 0,
        circuit_breaker_min_wr: 0.0,
        circuit_breaker_cooldown_s: 0,
        min_implied_prob: 0.0,
        max_consecutive_losses: 0,
    }
}

/// Display the interactive profile menu and return the selected profile name.
/// Returns None if stdin is not a TTY or user chose Custom (option 5).
pub fn interactive_menu() -> Option<&'static str> {
    use std::io::{self, BufRead, Write};

    if !atty::is(atty::Stream::Stdin) {
        return None;
    }

    println!();
    println!("poly5m — Sélection du profil de trading");
    println!();
    println!("  1. Sniper Conservateur   (GTC maker, edge>=3%, kelly=0.10, vol<0.08%)");
    println!("  2. High Conviction Only  (GTC maker, edge>=5%, kelly=0.15, imbal>=0.15)");
    println!("  3. Extreme Zones Scalper (FOK taker, edge>=1%, mid 0.10-0.90, prob>=0.85)");
    println!("  4. Data Farm [dry-run]   (FOK, filtres relâchés, collecte de données)");
    println!("  5. Custom (config.toml)  (utiliser la config [strategy] du fichier)");
    println!();
    print!("Choix [1-5]: ");
    io::stdout().flush().ok();

    let stdin = io::stdin();
    let line = stdin.lock().lines().next()?.ok()?;
    match line.trim() {
        "1" => Some("sniper"),
        "2" => Some("conviction"),
        "3" => Some("scalper"),
        "4" => Some("farm"),
        _ => None,
    }
}

/// Get a preset StrategyConfig by name. Returns None for unknown names.
pub fn get(name: &str) -> Option<StrategyConfig> {
    match name {
        "sniper" => Some(sniper()),
        "conviction" => Some(conviction()),
        "scalper" => Some(scalper()),
        "farm" => Some(farm()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_have_valid_config() {
        for (name, config) in [
            ("sniper", sniper()),
            ("conviction", conviction()),
            ("scalper", scalper()),
            ("farm", farm()),
        ] {
            assert!(config.max_bet_usdc > 0.0, "{name}: max_bet must be positive");
            assert!(config.min_market_price < config.max_market_price,
                "{name}: min_market must be < max_market");
            assert!(config.kelly_fraction > 0.0 && config.kelly_fraction <= 1.0,
                "{name}: kelly_fraction must be in (0, 1]");
            assert!(config.fee_rate > 0.0, "{name}: fee_rate must be positive");
        }
    }

    #[test]
    fn get_returns_preset_by_name() {
        assert!(get("sniper").is_some());
        assert!(get("conviction").is_some());
        assert!(get("scalper").is_some());
        assert!(get("farm").is_some());
        assert!(get("unknown").is_none());
    }

    #[test]
    fn farm_has_relaxed_filters() {
        let f = farm();
        assert_eq!(f.min_book_imbalance, 0.0);
        assert_eq!(f.max_vol_5min_pct, 0.0);
        assert_eq!(f.circuit_breaker_window, 0);
        assert_eq!(f.max_consecutive_losses, 0);
    }

    #[test]
    fn sniper_is_conservative() {
        let s = sniper();
        assert!(s.kelly_fraction <= 0.10);
        assert!(s.min_edge_pct >= 3.0);
        assert!(s.vol_confidence_multiplier >= 4.0);
    }
}
