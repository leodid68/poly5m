# Phase 3: Sophistication — Design Document

> Approved design for QUANT_REVIEW Phase 3 (items 11-15).

## Overview

Five improvements to transform the bot from a "calibrated sniper" (Phase 1-2) into a regime-aware, self-calibrating system with better execution and a more robust probability model.

## 1. Regime Detection (Item 11)

**Goal:** Reduce sizing in choppy/unfavorable market conditions instead of trading blind.

**Architecture:**
- New `WindowTicks` buffer in `main.rs` collects every polled price during the 5-min window (~3000 ticks at 100ms polling).
- At entry window, compute two metrics:
  - **Micro-vol**: std dev of tick-to-tick log-returns over last N ticks. High micro-vol = choppy.
  - **Momentum consistency**: `max(up_ticks, down_ticks) / total_ticks`. >0.7 = directional. <0.55 = oscillating.
- New fields in `TradeContext`: `micro_vol: f64`, `momentum_ratio: f64`.
- **Soft filter** in `evaluate()`: `regime_factor` (0.5 to 1.0) scales Kelly sizing.
  - `momentum_ratio < 0.55` → `regime_factor *= 0.5`
  - `micro_vol > 2 × vol_5min` → `regime_factor *= 0.6`
- CSV logger: add `micro_vol` and `momentum_ratio` columns.

**Files:** `main.rs` (WindowTicks buffer + TradeContext), `strategy.rs` (regime_factor in evaluate), `logger.rs` (new columns).

## 2. Maker Pricing Intelligent (Item 12)

**Goal:** GTC orders at a better price than best_ask → improved fill price, more edge.

**Architecture:**
- For GTC orders, compute: `maker_price = best_bid + spread × 0.25`
- Fallback: if spread < 0.02, use `best_bid + 0.01`. If spread = 0, use `best_bid`.
- Change only in `main.rs` where `entry_price` is computed before `place_limit_order()`.
- No changes to `polymarket.rs`.

**Files:** `main.rs` (entry_price calculation for GTC).

## 3. Hourly Analysis (Item 13)

**Goal:** Enable offline analysis of profitability by hour.

**Architecture:**
- Add `hour_utc` column to CSV logger: `(timestamp % 86400) / 3600`.
- No runtime filtering — analysis done offline on CSV data.

**Files:** `logger.rs` (add hour_utc field).

## 4. Student-t Distribution (Item 14)

**Goal:** Heavier tails than normal CDF → more conservative probabilities at high z-scores.

**Architecture:**
- Add `statrs = "0.17"` dependency.
- In `price_change_to_probability()`, replace `normal_cdf(z_combined)` with `StudentsT::new(0.0, 1.0, df).cdf(z_combined)`.
- New config field: `student_t_df: f64` (default 4.0). If 0.0, fall back to normal CDF.
- df=4: at z=2 gives ~0.95 instead of ~0.977 (normal). More conservative.

**Files:** `Cargo.toml` (statrs dep), `strategy.rs` (replace normal_cdf, add student_t_df config), `main.rs` (default fn), `presets.rs` (add field to all presets).

## 5. Auto-calibration (Item 15)

**Goal:** Automatically tune `vol_confidence_multiplier` based on actual trade outcomes.

**Architecture:**
- New `Calibrator` struct in `strategy.rs`:
  - Buffer of `(predicted_prob, actual_outcome)` pairs.
  - After each resolved trade, `calibrator.record(predicted_p, won)`.
  - When `count >= 200`: grid search multipliers [1.0, 1.5, ..., 8.0] (15 values), pick the one minimizing Brier Score.
  - Update `strat_config.vol_confidence_multiplier` in place.
- Persistence: `calibration.json` file.
  - Written after each recalibration: `{"vol_confidence_multiplier": 4.2, "trades_used": 200, "brier_score": 0.18, "timestamp": ...}`.
  - Read at startup (unless a preset overrides it).
- Grid search is O(15 × 200) = 3000 operations — instantaneous.

**Files:** `strategy.rs` (Calibrator struct), `main.rs` (wire calibrator into resolve loop + startup load), `calibration.json` (runtime artifact, .gitignore).

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Student-t impl | `statrs` crate | Battle-tested, avoids numerical bugs |
| Calibration persistence | `calibration.json` | Simple, safe (no config.toml corruption risk) |
| Regime filter severity | Soft (reduce sizing) | Keeps collecting data while reducing risk |
| Scope | All 5 items | User requested full Phase 3 |
