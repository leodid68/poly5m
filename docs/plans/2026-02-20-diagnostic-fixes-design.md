# Diagnostic Fixes — Top 4 Critical Issues

Based on DIAGNOSTIC.md analysis of 84 paper trades (PnL -$15.23).

## Fix 1: Overconfident Probability Model

**Problem**: At 29s remaining, `remaining_vol = vol * sqrt(29/300) = 0.031%`. A 0.05% move gives z=1.6, P(UP)=95%. Market prices 50/50 and is right 80% of the time.

**Solution**: Add `vol_confidence_multiplier` (default 2.5) to config. Multiply residual vol in `price_change_to_probability()`:

```
remaining_vol = vol_5min_pct * confidence_multiplier * sqrt(seconds_remaining / 300)
```

Effect: z drops from 1.6 to 0.64, P(UP) drops from 95% to 74%.

**Files**: strategy.rs (StrategyConfig, evaluate, price_change_to_probability), main.rs (StrategyToml), config.toml

## Fix 3: Inverted Sizing (Low Payout Trap)

**Problem**: Kelly bets large at price=0.96 (payout=0.04x) because probability is high. Absolute gain doesn't cover risk.

**Solution**: Add `min_payout_ratio` (default 0.08) to config. Skip trade if `(1 - price) / price < min_payout_ratio`.

- price=0.95 -> payout=0.053 < 0.08 -> skip
- price=0.90 -> payout=0.111 > 0.08 -> OK

**Files**: strategy.rs (StrategyConfig, evaluate), main.rs (StrategyToml), config.toml

## Fix 4: Book Imbalance Signal

**Problem**: Imbalance > 0.20 -> 94% WR. Imbalance <= 0.05 -> 7% WR. Not used in decisions.

**Solution**:
1. Add `book_imbalance: f64` to TradeContext
2. Add `min_book_imbalance` (default 0.08) to config
3. Filter: skip if `book_imbalance < min_book_imbalance`
4. Sizing boost: `imbalance_boost = 1.0 + (book_imbalance - min_book_imbalance).clamp(0, 1)`. Kelly size multiplied by boost (capped by max_bet).

**Files**: strategy.rs (TradeContext, StrategyConfig, evaluate), main.rs (StrategyToml, context building), config.toml

## Fix 5: Volatility Filter

**Problem**: Vol > 0.10% -> 21% WR, PnL -$23.65. High vol = unpredictable in final seconds.

**Solution**: Add `max_vol_5min_pct` (default 0.12) to config. Skip if `vol_5min_pct > max_vol_5min_pct`.

**Files**: strategy.rs (StrategyConfig, evaluate), main.rs (StrategyToml), config.toml

## Summary

| Fix | New param | Default | Impact |
|-----|-----------|---------|--------|
| Vol multiplier | vol_confidence_multiplier | 2.5 | Deflates z-scores, less overconfident |
| Payout minimum | min_payout_ratio | 0.08 | Avoids low-payout traps at extreme prices |
| Book imbalance | min_book_imbalance | 0.08 | Filters + boosts based on strongest signal |
| Max vol | max_vol_5min_pct | 0.12 | Skips unpredictable high-vol windows |

Also: set `always_trade = false` in config.toml after implementation.
