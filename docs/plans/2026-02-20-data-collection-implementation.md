# Data Collection Enhancement — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend CSV logging with 14 new columns, enrich WindowTicks with 4 analytics methods, add outcome logger for all windows, tick-level logger with daily rotation, and session stats tracking.

**Architecture:** Three layers of changes: (1) Extend `CsvLogger` header + all three log methods with new fields, plumbing new args from `main.rs`. (2) Enhance `WindowTicks` in `strategy.rs` with timestamps and 4 new analysis methods. (3) Add `OutcomeLogger` and `TickLogger` structs in `logger.rs`, wire them into the main loop. Session gets `consecutive_wins` and `min_pnl` tracking.

**Tech Stack:** Rust, existing CsvLogger/WindowTicks/Session structs, std::fs for file I/O.

---

### Task 1: Session stats — consecutive_wins + min_pnl

**Files:**
- Modify: `src/strategy.rs:56-99` (Session struct + Default + record_trade)

**Step 1: Add fields to Session struct**

In `src/strategy.rs`, add two new fields to the `Session` struct (after `consecutive_losses: u32` at line 66):

```rust
    /// Current consecutive win count (resets on any loss).
    pub consecutive_wins: u32,
    /// Minimum PnL reached during session (for drawdown tracking).
    pub min_pnl: f64,
```

**Step 2: Update Default impl**

In the `Default` impl (line 69-76), add the new fields:

```rust
    consecutive_wins: 0,
    min_pnl: 0.0,
```

**Step 3: Update record_trade()**

In `record_trade()` (line 88-99), add consecutive_wins tracking (mirror of consecutive_losses) and min_pnl tracking:

```rust
    pub fn record_trade(&mut self, pnl: f64) {
        self.pnl_usdc += pnl;
        self.trades += 1;
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
```

**Step 4: Add session_drawdown_pct() helper**

After `is_circuit_broken()` (line 133), add:

```rust
    /// Session drawdown as percentage of initial bankroll.
    pub fn session_drawdown_pct(&self) -> f64 {
        if self.initial_bankroll <= 0.0 {
            return 0.0;
        }
        (-self.min_pnl / self.initial_bankroll * 100.0).max(0.0)
    }
```

**Step 5: Run tests**

Run: `cargo test`
Expected: All tests pass (existing tests don't reference the new fields directly).

**Step 6: Commit**

```bash
git add src/strategy.rs
git commit -m "feat: add consecutive_wins and min_pnl tracking to Session"
```

---

### Task 2: WindowTicks enhancements — timestamps + 4 new methods

**Files:**
- Modify: `src/strategy.rs:177-227` (WindowTicks struct + impl)

**Step 1: Add timestamps_ms field to WindowTicks**

Replace the struct definition (lines 180-182):

```rust
pub struct WindowTicks {
    prices: Vec<f64>,
    timestamps_ms: Vec<u64>,
}
```

**Step 2: Update new(), tick(), and clear()**

Replace the implementations (lines 184-195):

```rust
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
```

**Step 3: Add 4 new methods after momentum_ratio() (after line 226)**

```rust
    /// Number of ticks collected in this window.
    pub fn ticks_count(&self) -> u32 {
        self.prices.len() as u32
    }

    /// Number of sign changes in consecutive tick deltas.
    /// High = choppy/oscillating, low = trending.
    pub fn sign_changes(&self) -> u32 {
        if self.prices.len() < 3 {
            return 0;
        }
        let mut changes = 0u32;
        let mut prev_sign = 0i8; // 0 = no previous delta
        for w in self.prices.windows(2) {
            let delta = w[1] - w[0];
            let sign = if delta > 0.0 { 1i8 } else if delta < 0.0 { -1i8 } else { 0i8 };
            if sign != 0 {
                if prev_sign != 0 && sign != prev_sign {
                    changes += 1;
                }
                prev_sign = sign;
            }
        }
        changes
    }

    /// Worst intra-window drawdown from peak, in basis points.
    pub fn max_drawdown_bps(&self) -> f64 {
        if self.prices.len() < 2 {
            return 0.0;
        }
        let mut peak = self.prices[0];
        let mut max_dd = 0.0f64;
        for &p in &self.prices[1..] {
            if p > peak {
                peak = p;
            }
            let dd = (peak - p) / peak * 10000.0;
            if dd > max_dd {
                max_dd = dd;
            }
        }
        max_dd
    }

    /// Seconds the price spent above start_price (for UP signal strength).
    /// Requires start_price as parameter (from the window's opening price).
    pub fn time_at_extreme_s(&self, start_price: f64) -> u64 {
        if self.timestamps_ms.len() < 2 {
            return 0;
        }
        let mut above_ms = 0u64;
        for i in 1..self.timestamps_ms.len() {
            if self.prices[i] >= start_price {
                above_ms += self.timestamps_ms[i].saturating_sub(self.timestamps_ms[i - 1]);
            }
        }
        above_ms / 1000
    }
```

**Step 4: Update existing tests that call tick()**

Search for all `window_ticks.tick(` or `.tick(` calls in tests within strategy.rs. Each call currently passes 1 arg — add a dummy timestamp. For example, if a test does `wt.tick(100.0)`, change to `wt.tick(100.0, 0)`.

Also update `main.rs` line 416 where `window_ticks.tick(current_btc)` is called — change to pass the current timestamp in milliseconds:

```rust
window_ticks.tick(current_btc, now * 1000);
```

**Step 5: Add tests for the new methods**

Add at the bottom of `mod tests` in `strategy.rs`:

```rust
#[test]
fn window_ticks_sign_changes() {
    let mut wt = WindowTicks::new();
    // Up, up, down, down, up = 2 sign changes (up→down, down→up)
    for (i, &p) in [100.0, 101.0, 102.0, 101.0, 100.0, 101.0].iter().enumerate() {
        wt.tick(p, i as u64 * 100);
    }
    assert_eq!(wt.sign_changes(), 2);
}

#[test]
fn window_ticks_max_drawdown_bps() {
    let mut wt = WindowTicks::new();
    // Peak at 100.10, drops to 99.90 = 20 bps drawdown
    for (i, &p) in [100.0, 100.10, 100.05, 99.90, 100.0].iter().enumerate() {
        wt.tick(p, i as u64 * 100);
    }
    let dd = wt.max_drawdown_bps();
    assert!(dd > 19.0 && dd < 21.0, "drawdown should be ~20 bps: {dd}");
}

#[test]
fn window_ticks_time_at_extreme() {
    let mut wt = WindowTicks::new();
    // Start at 100.0, above for 2 intervals, below for 1
    wt.tick(100.0, 0);
    wt.tick(100.5, 1000); // above, +1s
    wt.tick(100.3, 2000); // above, +1s
    wt.tick(99.8, 3000);  // below, +0s
    assert_eq!(wt.time_at_extreme_s(100.0), 2);
}

#[test]
fn window_ticks_ticks_count() {
    let mut wt = WindowTicks::new();
    wt.tick(100.0, 0);
    wt.tick(101.0, 100);
    wt.tick(102.0, 200);
    assert_eq!(wt.ticks_count(), 3);
}
```

**Step 6: Run tests**

Run: `cargo test`
Expected: All tests pass.

**Step 7: Commit**

```bash
git add src/strategy.rs src/main.rs
git commit -m "feat: enrich WindowTicks with sign_changes, max_drawdown_bps, time_at_extreme_s, ticks_count"
```

---

### Task 3: CsvLogger — add 14 new columns

**Files:**
- Modify: `src/logger.rs:1-193` (CsvLogger header, log_trade, log_resolution, log_skip, tests)

This task changes the CSV from 37 columns to 51 columns. The new columns are inserted at logical positions in the header.

**Step 1: Update the CSV header (line 14)**

Replace the header string with:

```
timestamp,hour_utc,day_of_week,window,event,btc_start,btc_current,btc_resolution,price_change_pct,market_mid,implied_p_up,side,token,edge_brut_pct,edge_net_pct,fee_pct,size_usdc,entry_price,order_latency_ms,fill_type,remaining_s,num_ws_src,price_source,vol_pct,btc_1h_pct,btc_24h_pct,btc_24h_vol_m,funding_rate,spread,bid_depth,ask_depth,book_imbalance,best_bid,best_ask,mid_vs_entry_slippage_bps,bid_levels,ask_levels,micro_vol,momentum_ratio,sign_changes,max_drawdown_bps,time_at_extreme_s,ticks_count,result,pnl,session_pnl,session_trades,session_wr_pct,consecutive_wins,session_drawdown_pct,skip_reason
```

That's 51 columns. New columns: `day_of_week` (3), `order_latency_ms` (19), `fill_type` (20), `price_source` (23), `best_bid` (33), `best_ask` (34), `mid_vs_entry_slippage_bps` (35), `sign_changes` (40), `max_drawdown_bps` (41), `time_at_extreme_s` (42), `ticks_count` (43), `consecutive_wins` (49), `session_drawdown_pct` (50), `skip_reason` (51).

**Step 2: Update log_trade() signature and body**

Replace the entire `log_trade` method (lines 19-58) with:

```rust
    /// Log quand un trade est placé.
    #[allow(clippy::too_many_arguments)]
    pub fn log_trade(
        &mut self,
        timestamp: u64,
        window: u64,
        btc_start: f64,
        btc_current: f64,
        market_mid: f64,
        implied_p_up: f64,
        side: &str,
        token: &str,
        edge_brut: f64,
        edge_net: f64,
        fee_pct: f64,
        size_usdc: f64,
        entry_price: f64,
        order_latency_ms: u64,
        fill_type: &str,
        remaining_s: u64,
        num_ws: u8,
        price_source: &str,
        vol_pct: f64,
        macro_data: &MacroData,
        spread: f64,
        bid_depth: f64,
        ask_depth: f64,
        imbalance: f64,
        best_bid: f64,
        best_ask: f64,
        bid_levels: u32,
        ask_levels: u32,
        micro_vol: f64,
        momentum_ratio: f64,
        sign_changes: u32,
        max_drawdown_bps: f64,
        time_at_extreme_s: u64,
        ticks_count: u32,
        session_pnl: f64,
        session_trades: u32,
        session_wr: f64,
        consecutive_wins: u32,
        session_drawdown_pct: f64,
    ) {
        let hour_utc = (timestamp % 86400) / 3600;
        let day_of_week = ((timestamp / 86400) + 4) % 7;
        let change_pct = if btc_start > 0.0 { (btc_current - btc_start) / btc_start * 100.0 } else { 0.0 };
        let slippage_bps = if market_mid > 0.0 { (entry_price - market_mid) / market_mid * 10000.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{hour_utc},{day_of_week},{window},trade,{btc_start:.2},{btc_current:.2},,{change_pct:.4},{market_mid:.4},{implied_p_up:.4},{side},{token},{edge_brut:.2},{edge_net:.2},{fee_pct:.2},{size_usdc:.2},{entry_price:.4},{order_latency_ms},{fill_type},{remaining_s},{num_ws},{price_source},{vol_pct:.4},{:.4},{:.4},{:.1},{:.8},{spread:.4},{bid_depth:.2},{ask_depth:.2},{imbalance:.4},{best_bid:.4},{best_ask:.4},{slippage_bps:.2},{bid_levels},{ask_levels},{micro_vol:.4},{momentum_ratio:.4},{sign_changes},{max_drawdown_bps:.2},{time_at_extreme_s},{ticks_count},,,,{session_pnl:.4},{session_trades},{session_wr:.1},{consecutive_wins},{session_drawdown_pct:.2},",
            macro_data.btc_1h_pct, macro_data.btc_24h_pct, macro_data.btc_24h_vol_m, macro_data.funding_rate,
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }
```

**Step 3: Update log_resolution()**

Replace the entire `log_resolution` method (lines 60-82) with:

```rust
    /// Log quand un bet est résolu (win/loss).
    #[allow(clippy::too_many_arguments)]
    pub fn log_resolution(
        &mut self,
        timestamp: u64,
        window: u64,
        btc_start: f64,
        btc_resolution: f64,
        result: &str,
        pnl: f64,
        session_pnl: f64,
        session_trades: u32,
        session_wr: f64,
        consecutive_wins: u32,
        session_drawdown_pct: f64,
    ) {
        let hour_utc = (timestamp % 86400) / 3600;
        let day_of_week = ((timestamp / 86400) + 4) % 7;
        let change_pct = if btc_start > 0.0 { (btc_resolution - btc_start) / btc_start * 100.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{hour_utc},{day_of_week},{window},resolution,{btc_start:.2},,{btc_resolution:.2},{change_pct:.4},,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,{result},{pnl:.4},{session_pnl:.4},{session_trades},{session_wr:.1},{consecutive_wins},{session_drawdown_pct:.2},"
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }
```

**Step 4: Update log_skip()**

Replace the entire `log_skip` method (lines 84-107) with:

```rust
    /// Log résumé de window sans trade (pour analyse des skips).
    #[allow(clippy::too_many_arguments)]
    pub fn log_skip(
        &mut self,
        timestamp: u64,
        window: u64,
        btc_start: f64,
        btc_end: f64,
        market_mid: f64,
        num_ws: u8,
        price_source: &str,
        vol_pct: f64,
        macro_data: &MacroData,
        reason: &str,
    ) {
        let hour_utc = (timestamp % 86400) / 3600;
        let day_of_week = ((timestamp / 86400) + 4) % 7;
        let change_pct = if btc_start > 0.0 { (btc_end - btc_start) / btc_start * 100.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{hour_utc},{day_of_week},{window},skip,{btc_start:.2},{btc_end:.2},,{change_pct:.4},{market_mid:.4},,{reason},,,,,,,,{num_ws},{price_source},{vol_pct:.4},{:.4},{:.4},{:.1},{:.8},,,,,,,,,,,,,,,,,,,,,,{reason}",
            macro_data.btc_1h_pct, macro_data.btc_24h_pct, macro_data.btc_24h_vol_m, macro_data.funding_rate,
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }
```

**Step 5: Update all tests**

Replace the entire `#[cfg(test)] mod tests` block with updated tests that match the new 51-column format. Key changes:
- `log_trade` calls need the new arguments: `order_latency_ms`, `fill_type`, `price_source`, `best_bid`, `best_ask`, `sign_changes`, `max_drawdown_bps`, `time_at_extreme_s`, `ticks_count`, `session_pnl`, `session_trades`, `session_wr`, `consecutive_wins`, `session_drawdown_pct`.
- `log_resolution` calls need `consecutive_wins`, `session_drawdown_pct`.
- `log_skip` calls need `price_source`.
- Field count assertion changes from 37 to 51.
- Add assertions for new header columns (`day_of_week`, `price_source`, `order_latency_ms`, etc.).

**Step 6: Run tests**

Run: `cargo test`
Expected: All tests pass.

**Step 7: Commit**

```bash
git add src/logger.rs
git commit -m "feat: extend CSV logger with 14 new columns (P0+P1+P2 data collection)"
```

---

### Task 4: Wire new logger args in main.rs

**Files:**
- Modify: `src/main.rs:416,459,591-600,621-623,789-845` (tick call, price_source tracking, log_trade args, log_skip args, resolve_pending_bet)

**Step 1: Track price_source as a variable**

The `price_source` is already computed at line 459 (`let src = ...`). Make it available later by storing it in a variable that persists across the loop. Add at line 365 (near the other state vars):

```rust
let mut price_source = "CL";
```

Then at line 459 (window transition block), update to also set the variable:

```rust
price_source = if rtds_price.is_some() { "RTDS" } else if ws_price.is_some() { "WS" } else { "CL" };
```

But actually, `price_source` should reflect which source provided `current_btc` on each tick, not just at window transition. Move the computation to after `current_btc` is determined (around line 414, after the price source selection block):

```rust
let price_source = if rtds_price.is_some() { "RTDS" } else if ws_price.is_some() { "WS" } else { "CL" };
```

This makes `price_source` a fresh `let` each iteration, which is fine.

**Step 2: Update window_ticks.tick() call (line 416)**

Replace:
```rust
window_ticks.tick(current_btc);
```
With:
```rust
window_ticks.tick(current_btc, now * 1000);
```

**Step 3: Measure order latency + fill_type**

Around the trade execution block (lines 603-627), we need to track `order_latency_ms` and `fill_type`. Add timing around the execute_order call and track fill_type.

Before the `if dry_run {` block at line 603, add:

```rust
let order_start = Instant::now();
```

For the dry_run case:
```rust
let order_latency_ms = 0u64;
let fill_type = "dry_run";
```

For the live case, after execute_order returns:
```rust
let order_latency_ms = order_start.elapsed().as_millis() as u64;
let fill_type = if order_type == "GTC" { "GTC_filled" } else { "FOK_filled" };
```

For failed orders (the `else` branch where reason is assigned):
```rust
let order_latency_ms = order_start.elapsed().as_millis() as u64;
```

**Step 4: Update log_trade() call (lines 591-600)**

Add the new arguments to the `csv.log_trade(...)` call. After the existing args, add:
- `order_latency_ms` (after `entry_price`)
- `fill_type` (after `order_latency_ms`)
- `price_source` (after `num_ws`)
- `book.best_bid, book.best_ask` (after `imbalance`)
- `window_ticks.sign_changes(), window_ticks.max_drawdown_bps(), window_ticks.time_at_extreme_s(start_price), window_ticks.ticks_count()` (after `momentum_ratio`)
- `session.pnl_usdc, session.trades, session.win_rate() * 100.0, session.consecutive_wins, session.session_drawdown_pct()` (session stats)

**Step 5: Update log_skip() calls**

There are two `csv.log_skip()` calls:
1. At window transition (line 423): add `price_source`
2. In the failed order block (line 622): add `price_source`

**Step 6: Update log_resolution() call in resolve_pending_bet (line 810-811)**

Add `session.consecutive_wins` and `session.session_drawdown_pct()` as the last two args.

**Step 7: Run tests**

Run: `cargo test`
Expected: All tests pass.

**Step 8: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings.

**Step 9: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire new CSV columns into main loop (latency, fill_type, price_source, session stats)"
```

---

### Task 5: OutcomeLogger

**Files:**
- Modify: `src/logger.rs` (add OutcomeLogger struct after CsvLogger)
- Modify: `src/main.rs` (create + call at window transition)

**Step 1: Add OutcomeLogger struct in logger.rs**

After the `CsvLogger` impl block (after line 107, before `#[cfg(test)]`), add:

```rust
/// Logs the outcome of every 5-min window (even without a trade).
/// Enables offline backtesting on all windows.
pub struct OutcomeLogger {
    writer: BufWriter<File>,
}

impl OutcomeLogger {
    pub fn new(path: &str) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context("Cannot create outcomes CSV")?;
        let needs_header = file.metadata().map(|m| m.len() == 0).unwrap_or(true);
        let mut writer = BufWriter::new(file);
        if needs_header {
            writeln!(writer, "window,btc_start,btc_end,went_up,price_change_bps")?;
            writer.flush()?;
        }
        Ok(Self { writer })
    }

    pub fn log_outcome(&mut self, window: u64, btc_start: f64, btc_end: f64) {
        let went_up = btc_end >= btc_start;
        let change_bps = if btc_start > 0.0 { (btc_end - btc_start) / btc_start * 10000.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{window},{btc_start:.2},{btc_end:.2},{went_up},{change_bps:.2}"
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("Outcome CSV write error: {e}");
        }
    }
}
```

**Step 2: Wire OutcomeLogger into main.rs**

After the CsvLogger creation (around line 342-348), add:

```rust
let mut outcome_csv = if !config.logging.csv_path.is_empty() {
    let outcome_path = config.logging.csv_path.replace(".csv", "_outcomes.csv");
    match logger::OutcomeLogger::new(&outcome_path) {
        Ok(l) => {
            tracing::info!("Outcome logging → {outcome_path}");
            Some(l)
        }
        Err(e) => {
            tracing::warn!("Failed to create outcome logger: {e:#}");
            None
        }
    }
} else {
    None
};
```

**Step 3: Call OutcomeLogger at window transition**

In the window transition block (around line 419-435), after `vol_tracker.record_move(start_price, current_btc)` and before `current_window = window`, add:

```rust
if let Some(ref mut oc) = outcome_csv {
    oc.log_outcome(current_window, start_price, current_btc);
}
```

**Step 4: Add test for OutcomeLogger**

In the `mod tests` block in logger.rs, add:

```rust
#[test]
fn outcome_logger_writes_header_and_row() {
    let path = "/tmp/poly5m_test_outcomes.csv";
    let _ = std::fs::remove_file(path);
    let mut logger = super::OutcomeLogger::new(path).unwrap();
    logger.log_outcome(1699999800, 97150.0, 97155.0);
    drop(logger);

    let content = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("window,btc_start,"));
    assert!(lines[1].contains("1699999800"));
    assert!(lines[1].contains("true")); // went_up
    std::fs::remove_file(path).ok();
}

#[test]
fn outcome_logger_appends_without_duplicate_header() {
    let path = "/tmp/poly5m_test_outcomes_append.csv";
    let _ = std::fs::remove_file(path);
    {
        let mut logger = super::OutcomeLogger::new(path).unwrap();
        logger.log_outcome(1699999800, 97150.0, 97155.0);
    }
    {
        let mut logger = super::OutcomeLogger::new(path).unwrap();
        logger.log_outcome(1700000100, 97155.0, 97140.0);
    }
    let content = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(lines.len(), 3, "1 header + 2 data rows, got: {lines:?}");
    assert!(lines[2].contains("false")); // went_down
    std::fs::remove_file(path).ok();
}
```

**Step 5: Run tests**

Run: `cargo test`
Expected: All tests pass.

**Step 6: Commit**

```bash
git add src/logger.rs src/main.rs
git commit -m "feat: add OutcomeLogger — log all window outcomes for offline backtesting"
```

---

### Task 6: TickLogger with daily rotation

**Files:**
- Modify: `src/logger.rs` (add TickLogger struct)
- Modify: `src/main.rs` (create + call on each tick)

**Step 1: Add TickLogger struct in logger.rs**

After `OutcomeLogger` (and before `#[cfg(test)]`), add:

```rust
/// Logs every price tick for granular analysis.
/// Rotates to a new file each day (ticks_YYYYMMDD.csv).
pub struct TickLogger {
    writer: BufWriter<File>,
    base_dir: String,
    current_date: String,
}

impl TickLogger {
    pub fn new(base_dir: &str) -> Result<Self> {
        std::fs::create_dir_all(base_dir).context("Cannot create ticks directory")?;
        let date = Self::today_str();
        let path = format!("{base_dir}/ticks_{date}.csv");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .context("Cannot create tick log file")?;
        let needs_header = file.metadata().map(|m| m.len() == 0).unwrap_or(true);
        let mut writer = BufWriter::new(file);
        if needs_header {
            writeln!(writer, "timestamp_ms,source,price,window")?;
            writer.flush()?;
        }
        Ok(Self { writer, base_dir: base_dir.to_string(), current_date: date })
    }

    pub fn log_tick(&mut self, timestamp_ms: u64, source: &str, price: f64, window: u64) {
        if let Err(_) = self.rotate_if_needed() {
            return;
        }
        if let Err(e) = writeln!(self.writer, "{timestamp_ms},{source},{price:.2},{window}")
            .and_then(|_| self.writer.flush())
        {
            tracing::warn!("Tick CSV write error: {e}");
        }
    }

    fn rotate_if_needed(&mut self) -> Result<()> {
        let today = Self::today_str();
        if today != self.current_date {
            let path = format!("{}/ticks_{today}.csv", self.base_dir);
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .context("Cannot create new tick log file")?;
            let needs_header = file.metadata().map(|m| m.len() == 0).unwrap_or(true);
            self.writer = BufWriter::new(file);
            if needs_header {
                writeln!(self.writer, "timestamp_ms,source,price,window")?;
                self.writer.flush()?;
            }
            self.current_date = today;
        }
        Ok(())
    }

    fn today_str() -> String {
        let secs = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let days = secs / 86400;
        // Convert days since epoch to YYYYMMDD
        // Simple algorithm: iterate years/months (good enough for 2020-2099)
        let mut y = 1970u32;
        let mut remaining = days as u32;
        loop {
            let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
            if remaining < days_in_year { break; }
            remaining -= days_in_year;
            y += 1;
        }
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let mut m = 1u32;
        for &days_in_month in &months {
            if remaining < days_in_month { break; }
            remaining -= days_in_month;
            m += 1;
        }
        let d = remaining + 1;
        format!("{y}{m:02}{d:02}")
    }
}
```

**Step 2: Wire TickLogger into main.rs**

After the outcome_csv creation, add:

```rust
let mut tick_csv = if !config.logging.csv_path.is_empty() {
    let tick_dir = std::path::Path::new(&config.logging.csv_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("ticks");
    match logger::TickLogger::new(tick_dir.to_str().unwrap_or("ticks")) {
        Ok(l) => {
            tracing::info!("Tick logging → {}", tick_dir.display());
            Some(l)
        }
        Err(e) => {
            tracing::warn!("Failed to create tick logger: {e:#}");
            None
        }
    }
} else {
    None
};
```

**Step 3: Call TickLogger on each price tick**

After `window_ticks.tick(current_btc, now * 1000);` (line 416), add:

```rust
if let Some(ref mut tl) = tick_csv {
    tl.log_tick(now * 1000, price_source, current_btc, current_window);
}
```

Note: `price_source` must be defined before this line. Move the `let price_source = ...` line to before the `window_ticks.tick()` call.

**Step 4: Add a use statement for SystemTime in logger.rs**

At the top of `logger.rs`, the existing imports include `std::io::{BufWriter, Write}`. Add:

```rust
use std::time::SystemTime;
```

**Step 5: Add test for TickLogger**

In the `mod tests` block in logger.rs, add:

```rust
#[test]
fn tick_logger_writes_ticks() {
    let dir = "/tmp/poly5m_test_ticks";
    let _ = std::fs::remove_dir_all(dir);
    let mut logger = super::TickLogger::new(dir).unwrap();
    logger.log_tick(1700000000000, "RTDS", 97150.50, 1699999800);
    logger.log_tick(1700000000100, "WS", 97150.80, 1699999800);
    drop(logger);

    // Find the ticks file
    let entries: Vec<_> = std::fs::read_dir(dir).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let content = std::fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(lines.len(), 3); // header + 2 ticks
    assert!(lines[0].starts_with("timestamp_ms,"));
    assert!(lines[1].contains("RTDS"));
    assert!(lines[2].contains("WS"));
    std::fs::remove_dir_all(dir).ok();
}
```

**Step 6: Run tests**

Run: `cargo test`
Expected: All tests pass.

**Step 7: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings.

**Step 8: Commit**

```bash
git add src/logger.rs src/main.rs
git commit -m "feat: add TickLogger with daily rotation for tick-level price data"
```

---

### Task 7: Final verification

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 0 warnings.

**Step 3: Build release**

Run: `cargo build --release`
Expected: Build succeeds.

**Step 4: Review changes**

Run: `git log --oneline -8` and `git diff HEAD~6..HEAD --stat`
Expected: 6 new commits touching `src/strategy.rs`, `src/logger.rs`, and `src/main.rs`.

---

## Summary of changes

| Task | File | What |
|------|------|------|
| 1 | `src/strategy.rs` | Add `consecutive_wins`, `min_pnl`, `session_drawdown_pct()` to Session |
| 2 | `src/strategy.rs`, `src/main.rs` | Enrich WindowTicks: timestamps, sign_changes, max_drawdown_bps, time_at_extreme_s, ticks_count |
| 3 | `src/logger.rs` | Extend CSV from 37→51 columns (14 new fields across all 3 log methods) |
| 4 | `src/main.rs` | Wire new args: price_source, order_latency_ms, fill_type, best_bid/ask, session stats |
| 5 | `src/logger.rs`, `src/main.rs` | OutcomeLogger for all windows (outcomes.csv) |
| 6 | `src/logger.rs`, `src/main.rs` | TickLogger with daily rotation (ticks_YYYYMMDD.csv) |
| 7 | — | Final verification: tests, clippy, release build |
