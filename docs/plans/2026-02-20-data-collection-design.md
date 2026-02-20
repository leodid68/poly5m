# Data Collection Enhancement — Design Document

> Approved design for DATA_COLLECTION.md implementation (P0 + P1 + P2).

## Overview

Extend the bot's logging infrastructure to capture more exploitable data for pattern mining, calibration, and backtesting. Builds on Phase 3's WindowTicks and hour_utc additions.

## Scope

- **P0**: 7 new CSV columns in trades.csv (day_of_week, price_source, order_latency_ms, fill_type, best_bid, best_ask, mid_vs_entry_slippage_bps)
- **P1**: 4 new WindowTicks methods (sign_changes, max_drawdown_bps, time_at_extreme_s, ticks_count) + OutcomeLogger for all windows
- **P2**: TickLogger with daily rotation + 2 session stat columns (consecutive_wins, session_drawdown_pct)

## 1. P0 — New CSV Columns (37 → 47 fields)

**New fields:**

| Field | Position | Source |
|-------|----------|--------|
| `day_of_week` | after `hour_utc` | `(timestamp / 86400 + 4) % 7` (0=Mon, 6=Sun) |
| `price_source` | after `num_ws_src` | `"RTDS"/"WS"/"CL"` from main.rs |
| `order_latency_ms` | after `entry_price` | `Instant::now()` around execute_order() |
| `fill_type` | after `order_latency_ms` | `"FOK_filled"/"GTC_immediate"/"GTC_waited"/"GTC_cancelled"/"dry_run"` |
| `best_bid` | after `book_imbalance` | `book.best_bid` |
| `best_ask` | after `best_bid` | `book.best_ask` |
| `mid_vs_entry_slippage_bps` | after `best_ask` | `(entry_price - market_mid) / market_mid * 10000` |

**Files:** `logger.rs` (header + log_trade + log_skip + log_resolution), `main.rs` (pass new args).

## 2. P1 — WindowTicks Enhancements

**New struct field:** `timestamps_ms: Vec<u64>` (for time_at_extreme_s).

**New methods:**

| Method | Returns | Logic |
|--------|---------|-------|
| `sign_changes()` | `u32` | Count sign flips in consecutive deltas |
| `max_drawdown_bps()` | `f64` | Running peak, worst drop in bps from peak |
| `time_at_extreme_s()` | `u64` | Seconds price spent above start_price (needs start_price param) |
| `ticks_count()` | `u32` | `self.prices.len()` |

**Breaking change:** `tick(price, timestamp_ms)` — add timestamp parameter.

**CSV:** 4 new columns after momentum_ratio: `sign_changes`, `max_drawdown_bps`, `time_at_extreme_s`, `ticks_count`.

**Files:** `strategy.rs` (WindowTicks methods), `logger.rs` (4 new columns), `main.rs` (pass timestamp to tick, pass new stats to logger).

## 3. P1 — Outcome Logger

New `OutcomeLogger` struct in `logger.rs`:
- File: `outcomes.csv` (derived from trades.csv path)
- Columns: `window,btc_start,btc_end,went_up,price_change_bps`
- Called at every window transition (whether traded or not)
- Enables offline backtesting on ALL windows

**Files:** `logger.rs` (OutcomeLogger struct), `main.rs` (create + call at window transition).

## 4. P2 — Tick Logger

New `TickLogger` struct in `logger.rs`:
- Files: `ticks_YYYYMMDD.csv` with daily rotation
- Columns: `timestamp_ms,source,price,window`
- Called on every price tick in main loop
- ~20K-30K lines/day, ~2-3 MB

**Files:** `logger.rs` (TickLogger struct), `main.rs` (create + call on each tick).

## 5. P2 — Session Stats

2 new CSV columns (after session_wr_pct):
- `consecutive_wins`: mirrors existing `consecutive_losses` in Session
- `session_drawdown_pct`: `min_pnl / initial_bankroll * 100`

New Session fields: `consecutive_wins: u32`, `min_pnl: f64`.

**Files:** `strategy.rs` (Session fields), `logger.rs` (2 new columns), `main.rs` (pass to logger).

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Architecture | All in logger.rs | Simple CSV writers don't justify separate modules |
| day_of_week | Epoch math, no chrono | Avoids new dependency for a trivial calculation |
| Outcome logger | No mid_at_tN columns | Would require extra API calls every window; add later if needed |
| Tick logger rotation | Daily by date string | Prevents multi-GB files, simple to implement |
| CSV field count | 37 → ~51 | 10 new trade columns + 4 WindowTicks = 14 new fields |
