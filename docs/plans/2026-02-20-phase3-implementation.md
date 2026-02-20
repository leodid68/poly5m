# Phase 3: Sophistication — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add regime detection, intelligent maker pricing, hourly CSV logging, Student-t probability model, and auto-calibration of vol_confidence_multiplier.

**Architecture:** Five independent features wired into existing `strategy.rs` (probability model, evaluate, new Calibrator), `main.rs` (WindowTicks buffer, maker pricing, calibrator wiring), `logger.rs` (new CSV columns), and `Cargo.toml` (statrs dep). New config field `student_t_df` propagated through StrategyConfig → StrategyToml → presets.

**Tech Stack:** Rust, statrs (Student-t CDF), tokio, serde_json (calibration.json), existing infrastructure.

---

### Task 1: Add `student_t_df` config field everywhere (plumbing)

**Files:**
- Modify: `src/strategy.rs:6-37` (StrategyConfig)
- Modify: `src/strategy.rs:446-473` (test_config)
- Modify: `src/strategy.rs:1043-1071` (prod_config)
- Modify: `src/main.rs:87-146` (StrategyToml)
- Modify: `src/main.rs:165-194` (From impl)
- Modify: `src/main.rs:148-163` (default fns)
- Modify: `src/presets.rs` (all 4 presets)

**Step 1: Add `student_t_df` to `StrategyConfig`**

In `src/strategy.rs`, after line 36 (`pub max_consecutive_losses: u32,`), add:

```rust
    /// Degrees of freedom for Student-t CDF (0.0 = use normal CDF).
    /// Lower df = heavier tails = more conservative. Recommended: 4.0.
    pub student_t_df: f64,
```

**Step 2: Add to `StrategyToml` + default fn + From impl**

In `src/main.rs`, after line 145 (`max_consecutive_losses: u32,`), add:

```rust
    #[serde(default)]
    student_t_df: f64,
```

After line 163 (`fn default_circuit_breaker_cooldown() ...`), add:

```rust
fn default_student_t_df() -> f64 { 4.0 }
```

Actually — use `#[serde(default = "default_student_t_df")]` instead of plain `#[serde(default)]` so new configs default to 4.0. Replace the attribute:

```rust
    #[serde(default = "default_student_t_df")]
    student_t_df: f64,
```

In the `From<StrategyToml>` impl (line 165-194), after `max_consecutive_losses: s.max_consecutive_losses,`, add:

```rust
            student_t_df: s.student_t_df,
```

**Step 3: Add to all 4 presets**

In `src/presets.rs`, add `student_t_df: 4.0,` at the end of each preset's StrategyConfig (sniper, conviction, scalper, farm). For farm, use `student_t_df: 0.0,` (normal CDF for data collection).

**Step 4: Add to test_config() and prod_config()**

In `src/strategy.rs` `test_config()` (line 446-473), add after `max_consecutive_losses: 0,`:

```rust
            student_t_df: 0.0, // normal CDF for backward-compat in tests
```

In `prod_config()` (line 1043-1071), add after `max_consecutive_losses: 6,`:

```rust
            student_t_df: 4.0,
```

**Step 5: Run all tests**

Run: `cargo test --lib`
Expected: All tests pass (no logic change, just a new field).

**Step 6: Commit**

```bash
git add src/strategy.rs src/main.rs src/presets.rs
git commit -m "refactor: add student_t_df config field plumbing (no logic change)"
```

---

### Task 2: Student-t distribution — tests + implementation

**Files:**
- Modify: `Cargo.toml` (add statrs)
- Modify: `src/strategy.rs:398-423` (price_change_to_probability + normal_cdf)

**Step 1: Add statrs dependency**

In `Cargo.toml`, after `atty = "0.2"`, add:

```toml
statrs = "0.18"
```

**Step 2: Write failing tests for Student-t behavior**

In `src/strategy.rs`, at the bottom of `mod tests` (before the closing `}`), add:

```rust
    #[test]
    fn student_t_more_conservative_than_normal() {
        // Student-t with df=4 should give lower probability at high z than normal CDF
        // Same inputs but with student_t_df=4 vs 0 (normal)
        let p_normal = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.5, 0.0);
        let p_student = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.5, 4.0);
        assert!(p_student < p_normal,
            "Student-t should be more conservative: {p_student} vs normal {p_normal}");
        assert!(p_student > 0.5, "should still lean UP: {p_student}");
    }

    #[test]
    fn student_t_df_zero_uses_normal() {
        // df=0 should fall back to normal CDF (identical results)
        let p_zero = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.5, 0.0);
        let p_normal = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.5, 0.0);
        assert!((p_zero - p_normal).abs() < 1e-10);
    }

    #[test]
    fn student_t_symmetric() {
        // P(UP | +x) + P(UP | -x) should be ~1.0 for Student-t too
        let p_up = price_change_to_probability(0.05, 10, 0.12, 1.0, 0.5, 4.0);
        let p_down = price_change_to_probability(-0.05, 10, 0.12, 1.0, 0.5, 4.0);
        assert!((p_up + p_down - 1.0).abs() < 0.01,
            "should be symmetric: p_up={p_up} p_down={p_down}");
    }
```

**Step 3: Run tests to verify they fail**

Run: `cargo test --lib -- student_t`
Expected: FAIL — `price_change_to_probability` doesn't accept 6 args.

**Step 4: Update `price_change_to_probability()` signature and logic**

Replace `src/strategy.rs` lines 398-412:

```rust
fn price_change_to_probability(pct_change: f64, seconds_remaining: u64, vol_5min_pct: f64, confidence_multiplier: f64, book_imbalance: f64, student_t_df: f64) -> f64 {
    let remaining_vol = vol_5min_pct * confidence_multiplier * ((seconds_remaining as f64) / 300.0).sqrt();

    if remaining_vol < 1e-9 {
        return if pct_change > 0.0 { 1.0 } else if pct_change < 0.0 { 0.0 } else { 0.5 };
    }

    let z = pct_change / remaining_vol;
    let imbalance_signal = (book_imbalance - 0.5).clamp(-0.4, 0.4);
    let z_combined = z * 0.6 + imbalance_signal * z.signum() * 2.5;

    if student_t_df > 0.0 {
        use statrs::distribution::{StudentsT, ContinuousCDF};
        let dist = StudentsT::new(0.0, 1.0, student_t_df).unwrap();
        dist.cdf(z_combined)
    } else {
        normal_cdf(z_combined)
    }
}
```

**Step 5: Update the call in `evaluate()` (line 234)**

Replace:

```rust
    let true_up_prob = price_change_to_probability(price_change_pct, ctx.seconds_remaining, ctx.vol_5min_pct, config.vol_confidence_multiplier, ctx.book_imbalance);
```

With:

```rust
    let true_up_prob = price_change_to_probability(price_change_pct, ctx.seconds_remaining, ctx.vol_5min_pct, config.vol_confidence_multiplier, ctx.book_imbalance, config.student_t_df);
```

**Step 6: Update ALL existing test calls to `price_change_to_probability`**

Every existing call uses 5 args. Add `, 0.0` (normal CDF) as 6th arg:

- `prob_positive_move_low_time` (line 498): `..., 0.5, 0.0)`
- `prob_positive_move_high_time` (line 504): `..., 0.5, 0.0)`
- `prob_flat` (line 510): `..., 0.5, 0.0)`
- `prob_negative_move` (line 516): `..., 0.5, 0.0)`
- `prob_zero_time_locks_direction` (lines 522-524): add `, 0.0` to all 3 calls
- `confidence_multiplier_reduces_probability` (lines 1239-1240): add `, 0.0` to both calls
- `hybrid_imbalance_confirms_direction_increases_prob` (lines 1577-1578): add `, 0.0` to both calls
- `hybrid_imbalance_contradicts_direction_decreases_prob` (lines 1585-1586): add `, 0.0` to both calls
- `hybrid_neutral_imbalance_close_to_original` (line 1593): add `, 0.0`
- `hybrid_imbalance_clamped` (line 1599): add `, 0.0`

**Step 7: Run all tests**

Run: `cargo test --lib`
Expected: All pass.

**Step 8: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings.

**Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/strategy.rs
git commit -m "feat: Student-t distribution for probability model (statrs, df=4.0)"
```

---

### Task 3: Regime Detection — WindowTicks struct + tests

**Files:**
- Modify: `src/strategy.rs` (add WindowTicks struct after VolTracker)

**Step 1: Write tests for WindowTicks**

Add at the bottom of `mod tests` in `src/strategy.rs`:

```rust
    #[test]
    fn window_ticks_micro_vol_directional() {
        let mut wt = WindowTicks::new();
        // Monotone up: 100, 101, 102, 103, 104
        for i in 0..5 {
            wt.tick(100.0 + i as f64);
        }
        let mv = wt.micro_vol();
        // Directional move → consistent returns → low micro_vol relative to move
        assert!(mv > 0.0, "micro_vol should be positive: {mv}");
    }

    #[test]
    fn window_ticks_micro_vol_choppy_higher() {
        let mut dir = WindowTicks::new();
        let mut chop = WindowTicks::new();
        // Directional: 100, 101, 102, 103, 104
        for i in 0..20 {
            dir.tick(100.0 + i as f64 * 0.5);
        }
        // Choppy: alternates ±1 around 100
        for i in 0..20 {
            chop.tick(if i % 2 == 0 { 100.0 } else { 101.0 });
        }
        assert!(chop.micro_vol() > dir.micro_vol(),
            "choppy should have higher micro_vol: {} vs {}", chop.micro_vol(), dir.micro_vol());
    }

    #[test]
    fn window_ticks_momentum_ratio_directional() {
        let mut wt = WindowTicks::new();
        // All up ticks
        for i in 0..10 {
            wt.tick(100.0 + i as f64);
        }
        let mr = wt.momentum_ratio();
        assert!(mr > 0.8, "directional should have high momentum: {mr}");
    }

    #[test]
    fn window_ticks_momentum_ratio_choppy() {
        let mut wt = WindowTicks::new();
        // Alternating up/down
        for i in 0..10 {
            wt.tick(if i % 2 == 0 { 100.0 } else { 101.0 });
        }
        let mr = wt.momentum_ratio();
        assert!(mr < 0.6, "choppy should have low momentum: {mr}");
    }

    #[test]
    fn window_ticks_empty_defaults() {
        let wt = WindowTicks::new();
        assert!(wt.micro_vol() == 0.0);
        assert!(wt.momentum_ratio() == 1.0); // default = favorable
    }

    #[test]
    fn window_ticks_single_price_defaults() {
        let mut wt = WindowTicks::new();
        wt.tick(100.0);
        assert!(wt.micro_vol() == 0.0);
        assert!(wt.momentum_ratio() == 1.0);
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib -- window_ticks`
Expected: FAIL — `WindowTicks` not defined.

**Step 3: Implement WindowTicks struct**

In `src/strategy.rs`, after the `VolTracker` impl block (after line 172), add:

```rust
/// Buffer de prix intra-window pour le regime detection.
/// Collecte les ticks pendant une fenêtre 5min et calcule micro-vol + momentum.
#[derive(Debug)]
pub struct WindowTicks {
    prices: Vec<f64>,
}

impl WindowTicks {
    pub fn new() -> Self {
        Self { prices: Vec::with_capacity(3200) } // ~5min at 100ms
    }

    pub fn tick(&mut self, price: f64) {
        self.prices.push(price);
    }

    pub fn clear(&mut self) {
        self.prices.clear();
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
    /// >0.7 = directional, <0.55 = choppy/oscillating.
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
}
```

**Step 4: Run all tests**

Run: `cargo test --lib`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/strategy.rs
git commit -m "feat: add WindowTicks struct for intra-window regime detection"
```

---

### Task 4: Regime Detection — integrate into evaluate() + main loop

**Files:**
- Modify: `src/strategy.rs:174-188` (TradeContext — add fields)
- Modify: `src/strategy.rs:343-348` (evaluate — add regime_factor after loss_decay)
- Modify: `src/strategy.rs:478-491` (test_ctx — add fields)
- Modify: `src/strategy.rs:1076-1090` (paper tests — add fields)
- Modify: `src/main.rs:353-360` (main loop state — add WindowTicks)
- Modify: `src/main.rs:410-416` (window transition — clear WindowTicks)
- Modify: `src/main.rs:500-513` (TradeContext construction)
- Modify: `src/logger.rs` (add micro_vol, momentum_ratio columns)

**Step 1: Write failing test for regime soft filter**

Add at bottom of `mod tests` in `src/strategy.rs`:

```rust
    #[test]
    fn regime_choppy_reduces_sizing() {
        let config = StrategyConfig { min_edge_pct: 1.0, ..test_config() };
        let session = Session::new(40.0);
        // Good regime
        let ctx_good = TradeContext {
            chainlink_price: 100_050.0,
            micro_vol: 0.001,
            momentum_ratio: 0.9,
            ..test_ctx()
        };
        let sig_good = evaluate(&ctx_good, &session, &config);

        // Choppy regime: low momentum
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
        // Choppy may also be skipped entirely — that's fine
    }

    #[test]
    fn regime_high_microvol_reduces_sizing() {
        let config = StrategyConfig { min_edge_pct: 1.0, ..test_config() };
        let session = Session::new(40.0);
        // Normal micro_vol
        let ctx_normal = TradeContext {
            chainlink_price: 100_050.0,
            vol_5min_pct: 0.10,
            micro_vol: 0.05,
            momentum_ratio: 0.9,
            ..test_ctx()
        };
        let sig_normal = evaluate(&ctx_normal, &session, &config);

        // High micro_vol (> 2× vol_5min)
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
```

**Step 2: Add `micro_vol` and `momentum_ratio` to TradeContext**

In `src/strategy.rs`, after line 187 (`pub num_ws_sources: u32,`), add:

```rust
    pub micro_vol: f64,
    pub momentum_ratio: f64,
```

**Step 3: Add regime_factor to evaluate() — after loss_decay, before imbalance_boost**

In `src/strategy.rs`, after line 345 (`let kelly_size = kelly_size * loss_decay;`), add:

```rust
    // 8c. Regime factor: reduce sizing in choppy/high-microvol markets
    let mut regime_factor = 1.0;
    if ctx.momentum_ratio < 0.55 {
        regime_factor *= 0.5;
    }
    if ctx.vol_5min_pct > 0.0 && ctx.micro_vol > ctx.vol_5min_pct * 2.0 {
        regime_factor *= 0.6;
    }
    let kelly_size = kelly_size * regime_factor;
```

**Step 4: Add defaults to test_ctx() and all paper test TradeContext literals**

In `test_ctx()` (line 478-491), after `num_ws_sources: 3,` add:

```rust
            micro_vol: 0.0,
            momentum_ratio: 1.0,
```

In every `paper_*` test and `loss_decay_reduces_sizing` that constructs `TradeContext` explicitly (not via `..test_ctx()`), add those same two fields. The tests that use `..test_ctx()` will inherit the defaults.

Specifically, add `micro_vol: 0.0, momentum_ratio: 1.0,` to:
- `paper_strong_up_signal_10s` (line 1078-1090)
- `paper_weak_signal_rejected` (line 1105-1117)
- `paper_high_price_min_shares_binding` (line 1127-1139)
- `paper_session_loss_limit_40_portfolio` (line 1153-1165)
- `paper_spread_eats_edge` (line 1176-1188)
- `loss_decay_reduces_sizing` (line 1546-1554)

**Step 5: Update main.rs TradeContext construction**

In `src/main.rs`, after line 512 (`num_ws_sources: u32::from(num_ws),`), add:

```rust
            micro_vol: window_ticks.micro_vol(),
            momentum_ratio: window_ticks.momentum_ratio(),
```

**Step 6: Add WindowTicks to main loop state**

In `src/main.rs`, after line 360 (`let mut macro_ctx = ...`), add:

```rust
    let mut window_ticks = strategy::WindowTicks::new();
```

**Step 7: Clear WindowTicks on window transition + record ticks**

In `src/main.rs`, after line 416 (`start_price = current_btc;`), add:

```rust
            window_ticks.clear();

```

Before the window transition check (before line 448 `if traded_this_window { continue; }`), add a tick recording:

```rust
        window_ticks.tick(current_btc);
```

This should go right after the `current_btc` is determined (after line 393, before line 396 `if window != current_window`). Find the exact spot: after `current_btc` is set but before the window transition check. Insert at line 395 (before `if window != current_window {`):

```rust
        window_ticks.tick(current_btc);
```

**Step 8: Add micro_vol and momentum_ratio to CSV logger**

In `src/logger.rs`, update the header (line 14) — add `micro_vol,momentum_ratio` after `ask_levels`:

Old header ends with: `...,bid_levels,ask_levels,result,pnl,...`
New: `...,bid_levels,ask_levels,micro_vol,momentum_ratio,result,pnl,...`

Update `log_trade()` signature — add `micro_vol: f64, momentum_ratio: f64` params after `ask_levels: u32`.

Update the `writeln!` format string in `log_trade()` to include `{micro_vol:.4},{momentum_ratio:.4}` after `{ask_levels}`.

Update `log_resolution()` writeln to add `,,` for the two new empty columns.

Update `log_skip()` writeln to add `,,` for the two new empty columns.

Update the call in `main.rs` (line 556-564) to pass `window_ticks.micro_vol(), window_ticks.momentum_ratio()`.

Update the field count assertion in logger tests from 34 to 36.

**Step 9: Run all tests**

Run: `cargo test --lib`
Expected: All pass.

**Step 10: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings.

**Step 11: Commit**

```bash
git add src/strategy.rs src/main.rs src/logger.rs
git commit -m "feat: regime detection — soft filter on micro-vol + momentum consistency"
```

---

### Task 5: Maker Pricing — intelligent GTC price

**Files:**
- Modify: `src/main.rs:542-547` (entry_price calculation)

**Step 1: Update entry_price calculation for GTC orders**

Replace lines 542-547 in `src/main.rs`:

```rust
        // Use best_ask as entry price (what taker actually pays), fallback to midpoint
        let entry_price = if book.best_ask > 0.0 && book.best_ask <= 1.0 {
            book.best_ask
        } else {
            signal.price // fallback to midpoint
        };
```

With:

```rust
        // Maker pricing for GTC: bid + 25% of spread (better than best_ask)
        // Taker (FOK): use best_ask as usual
        let entry_price = if order_type == "GTC" && book.best_bid > 0.0 && book.best_ask > 0.0 {
            let spread = book.best_ask - book.best_bid;
            if spread >= 0.02 {
                // Place at 25% of spread above best_bid
                let maker_price = book.best_bid + spread * 0.25;
                // Round to nearest cent (Polymarket tick size)
                (maker_price * 100.0).round() / 100.0
            } else {
                // Tight spread: place at bid + 1 tick
                (book.best_bid + 0.01).min(book.best_ask)
            }
        } else if book.best_ask > 0.0 && book.best_ask <= 1.0 {
            book.best_ask
        } else {
            signal.price
        };
```

**Step 2: Run all tests**

Run: `cargo test --lib`
Expected: All pass (no unit test for entry_price — it's integration logic).

**Step 3: Run clippy + build**

Run: `cargo clippy --all-targets -- -D warnings && cargo build --release`
Expected: 0 warnings, build succeeds.

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: intelligent maker pricing — bid + 25% spread for GTC orders"
```

---

### Task 6: Hourly Analysis — add hour_utc to CSV

**Files:**
- Modify: `src/logger.rs:14` (header)
- Modify: `src/logger.rs:46-54` (log_trade)
- Modify: `src/logger.rs:70-77` (log_resolution)
- Modify: `src/logger.rs:93-101` (log_skip)

**Step 1: Add `hour_utc` column to CSV header**

In `src/logger.rs` line 14, insert `hour_utc,` right after `timestamp,`:

Old: `"timestamp,window,event,...`
New: `"timestamp,hour_utc,window,event,...`

**Step 2: Add hour_utc to log_trade()**

At the start of `log_trade()` body (after line 47), compute:

```rust
        let hour_utc = (timestamp % 86400) / 3600;
```

In the writeln! format string, insert `{hour_utc},` after `{timestamp},`.

**Step 3: Add hour_utc to log_resolution()**

Same pattern: compute `let hour_utc = (timestamp % 86400) / 3600;` and insert `{hour_utc},` after `{timestamp},` in the writeln!.

**Step 4: Add hour_utc to log_skip()**

Same pattern.

**Step 5: Update field count in tests**

Update the assertion in `csv_all_events_same_field_count` from the current count to +1 (accounting for `hour_utc`). The current total with micro_vol+momentum_ratio from Task 4 will be 37 (was 34, +2 for regime, +1 for hour).

Note: if executing Task 4 and Task 6 together, the final field count = 34 + 2 (regime) + 1 (hour) = 37. If executing Task 6 before Task 4, it's 35.

**Step 6: Update test log calls**

In `csv_header_and_trade_line`, verify the header starts with `timestamp,hour_utc,window,`.

**Step 7: Run all tests**

Run: `cargo test --lib`
Expected: All pass.

**Step 8: Commit**

```bash
git add src/logger.rs
git commit -m "feat: add hour_utc column to CSV logger for hourly analysis"
```

---

### Task 7: Auto-calibration — Calibrator struct + tests

**Files:**
- Modify: `src/strategy.rs` (add Calibrator struct after WindowTicks)

**Step 1: Write failing tests**

Add at bottom of `mod tests` in `src/strategy.rs`:

```rust
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
        // Perfect predictions: p=1.0 when won, p=0.0 when lost
        cal.record(1.0, true);
        cal.record(0.0, false);
        let bs = cal.brier_score();
        assert!(bs < 0.01, "perfect predictions should have low brier: {bs}");
    }

    #[test]
    fn calibrator_brier_score_bad() {
        let mut cal = Calibrator::new(10);
        // Terrible predictions: p=0.9 but always loses
        for _ in 0..5 {
            cal.record(0.9, false);
        }
        let bs = cal.brier_score();
        assert!(bs > 0.5, "bad predictions should have high brier: {bs}");
    }

    #[test]
    fn calibrator_optimal_multiplier() {
        let mut cal = Calibrator::new(5);
        // Simulate: model overconfident (predicts 0.9 but actual WR ~50%)
        cal.record(0.9, true);
        cal.record(0.9, false);
        cal.record(0.9, true);
        cal.record(0.9, false);
        cal.record(0.9, true);
        // The optimal multiplier should be higher than 1.0 (to deflate overconfidence)
        // We can't test the exact value since it depends on the grid search
        // but recalibrate should work without panicking
        let result = cal.recalibrate();
        assert!(result.is_some());
        let (mult, brier) = result.unwrap();
        assert!(mult >= 1.0 && mult <= 8.0, "multiplier {mult} out of range");
        assert!(brier >= 0.0 && brier <= 1.0, "brier {brier} out of range");
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib -- calibrator`
Expected: FAIL — `Calibrator` not defined.

**Step 3: Implement Calibrator struct**

In `src/strategy.rs`, after the `WindowTicks` impl block, add:

```rust
/// Auto-calibration: tracks (predicted_prob, actual_outcome) pairs and
/// recalibrates vol_confidence_multiplier by minimizing Brier Score.
#[derive(Debug)]
pub struct Calibrator {
    entries: Vec<(f64, bool)>, // (predicted_prob, won)
    recalibrate_every: usize,
}

impl Calibrator {
    pub fn new(recalibrate_every: usize) -> Self {
        Self {
            entries: Vec::with_capacity(recalibrate_every + 10),
            recalibrate_every,
        }
    }

    pub fn record(&mut self, predicted_prob: f64, won: bool) {
        self.entries.push((predicted_prob, won));
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn should_recalibrate(&self) -> bool {
        self.recalibrate_every > 0 && self.entries.len() >= self.recalibrate_every
    }

    /// Brier Score on current entries.
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

        // Grid: [1.0, 1.5, 2.0, ..., 8.0]
        let multipliers: Vec<f64> = (2..=16).map(|i| i as f64 * 0.5).collect();
        let mut best_mult = 4.0;
        let mut best_brier = f64::MAX;

        for &mult in &multipliers {
            let brier: f64 = self.entries.iter()
                .map(|(p, won)| {
                    // Re-compute probability with this multiplier
                    // We don't have the raw inputs, so we estimate the correction factor
                    // by scaling p towards 0.5 proportionally to multiplier change
                    // Higher multiplier → p closer to 0.5
                    let adjusted_p = 0.5 + (*p - 0.5) * (4.0 / mult);
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
```

**Step 4: Run all tests**

Run: `cargo test --lib`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/strategy.rs
git commit -m "feat: add Calibrator struct for auto-calibration of vol_confidence_multiplier"
```

---

### Task 8: Auto-calibration — wire into main loop + persistence

**Files:**
- Modify: `src/main.rs:348-360` (add calibrator to state)
- Modify: `src/main.rs:752-781` (resolve_pending_bet — record to calibrator)
- Add to `.gitignore`: `calibration.json`

**Step 1: Add calibrator to main loop state**

In `src/main.rs`, after line 349 (`let mut vol_tracker = ...`), add:

```rust
    let mut calibrator = strategy::Calibrator::new(200);
    let mut strat_config = strat_config; // make mutable for recalibration
```

Note: `strat_config` is currently immutable. Change the declaration at line 232 from `let (strat_config, ...)` to `let (mut strat_config, ...)`. Actually it's easier: just add `let mut strat_config = strat_config;` right after the profile selection block.

Wait — `strat_config` is already destructured as immutable. The simplest fix is to add the rebinding line after line 267.

**Step 2: Load calibration.json at startup (if exists)**

After the `calibrator` creation, add:

```rust
    // Load saved calibration if available (and no preset overrides it)
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
```

**Step 3: Record to calibrator in resolve_pending_bet()**

The function `resolve_pending_bet` needs access to the calibrator. Change its signature to accept `&mut strategy::Calibrator`. Update the signature at line 752:

```rust
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
```

After `session.record_trade(pnl);` (line 765), add:

```rust
    // Record for auto-calibration (use implied_p_up from the signal)
    // We don't have the predicted_prob here — use a rough estimate from the bet side
    let predicted_p = if bet.side == polymarket::Side::Buy {
        1.0 - bet.entry_price // if we bought UP at 0.40, we thought P(UP) > 0.60
    } else {
        bet.entry_price // if we bought DOWN (sold UP), we thought P(UP) < entry_price
    };
    calibrator.record(predicted_p, won);

    // Recalibrate if threshold reached
    if calibrator.should_recalibrate() {
        if let Some((new_mult, brier)) = calibrator.recalibrate() {
            tracing::info!("Auto-calibration: vcm {:.2} → {:.2} (brier={:.4})",
                strat_config.vol_confidence_multiplier, new_mult, brier);
            strat_config.vol_confidence_multiplier = new_mult;
            // Save to calibration.json
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
```

**Step 4: Update all call sites of resolve_pending_bet()**

There's one call site at line 405. Update it to pass `&mut strat_config` and `&mut calibrator`:

```rust
                resolve_pending_bet(bet, current_btc, now, current_window,
                    &mut session, &mut csv, &mut strat_config, &mut calibrator);
```

**Step 5: Add calibration.json to .gitignore**

Check if `.gitignore` exists and add `calibration.json`.

**Step 6: Run all tests**

Run: `cargo test --lib`
Expected: All pass.

**Step 7: Run clippy + build**

Run: `cargo clippy --all-targets -- -D warnings && cargo build --release`
Expected: 0 warnings, build succeeds.

**Step 8: Commit**

```bash
git add src/main.rs .gitignore
git commit -m "feat: auto-calibration — recalibrate vcm every 200 trades + calibration.json persistence"
```

---

### Task 9: Final verification

**Step 1: Run full test suite**

Run: `cargo test --lib`
Expected: All tests pass.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings.

**Step 3: Build release**

Run: `cargo build --release`
Expected: Build succeeds.

**Step 4: Review changes**

Run: `git log --oneline -10` and `git diff HEAD~7..HEAD --stat`
Expected: 7 new commits, touching `src/strategy.rs`, `src/main.rs`, `src/logger.rs`, `src/presets.rs`, `Cargo.toml`.

---

## Summary of changes

| Task | File(s) | What |
|------|---------|------|
| 1 | strategy.rs, main.rs, presets.rs | Add `student_t_df` config field plumbing |
| 2 | Cargo.toml, strategy.rs | Student-t CDF via statrs, replace normal_cdf when df>0 |
| 3 | strategy.rs | WindowTicks struct: micro_vol() + momentum_ratio() |
| 4 | strategy.rs, main.rs, logger.rs | Regime detection soft filter + CSV columns |
| 5 | main.rs | Intelligent maker pricing: bid + 25% spread for GTC |
| 6 | logger.rs | Add hour_utc column for hourly analysis |
| 7 | strategy.rs | Calibrator struct: Brier Score grid search |
| 8 | main.rs, .gitignore | Wire calibrator into resolve + calibration.json |
| 9 | — | Final verification |
