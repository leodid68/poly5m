# Bet Sizing Improvements — Design

## Goal

Replace hardcoded half-Kelly with configurable fractional Kelly that:
- Uses session bankroll (not max_bet) for sizing
- Integrates fees into the Kelly formula (b_net)
- Supports configurable kelly_fraction (0.25 for validation, 0.5 for cruise)
- Always enforces min_shares floor (5 shares minimum)

## Changes

### 1. StrategyConfig — new fields
- `kelly_fraction: f64` (default 0.25)
- `initial_bankroll_usdc: f64` (default 40.0)

### 2. Session — bankroll tracking
- Add `initial_bankroll: f64` field
- Add `Session::new(initial_bankroll)` constructor
- Add `Session::bankroll() -> f64` = initial + pnl

### 3. fractional_kelly() replaces half_kelly()
```
fee = dynamic_fee(price, fee_rate)
b_net = (1-price)/price - fee
kelly = (b_net * p - q) / b_net
size = kelly * kelly_fraction * bankroll
clamped to [0, max_bet]
```

### 4. evaluate() — uses bankroll from session
- Pass `session.bankroll()` and `config.kelly_fraction` to fractional_kelly
- Min floor: max(min_shares * price, min_bet_usdc)
- If kelly_size < min_floor * 0.1 → skip (too marginal)
- If kelly_size < min_floor → bump to min_floor

### 5. main.rs — Session::new()
- Initialize with `Session::new(config.initial_bankroll_usdc)`

### 6. Tests
- fractional_kelly proportional to kelly_fraction
- fee-adjusted b_net reduces sizing
- bankroll decrease reduces bet size
- 5 shares minimum always respected
- existing tests adapted
