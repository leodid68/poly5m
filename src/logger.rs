use crate::macro_data::MacroData;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};

pub struct CsvLogger {
    writer: BufWriter<File>,
}

impl CsvLogger {
    pub fn new(path: &str) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context("Cannot create CSV log file")?;
        let needs_header = file.metadata().map(|m| m.len() == 0).unwrap_or(true);
        let mut writer = BufWriter::new(file);
        if needs_header {
            writeln!(writer, "timestamp,hour_utc,day_of_week,window,event,btc_start,btc_current,btc_resolution,price_change_pct,market_mid,implied_p_up,side,token,edge_brut_pct,edge_net_pct,fee_pct,size_usdc,entry_price,order_latency_ms,fill_type,remaining_s,num_ws_src,price_source,vol_pct,btc_1h_pct,btc_24h_pct,btc_24h_vol_m,funding_rate,spread,bid_depth,ask_depth,book_imbalance,best_bid,best_ask,mid_vs_entry_slippage_bps,bid_levels,ask_levels,micro_vol,momentum_ratio,sign_changes,max_drawdown_bps,time_above_start_s,ticks_count,result,pnl,session_pnl,session_trades,session_wr_pct,consecutive_wins,session_drawdown_pct,skip_reason")?;
            writer.flush()?;
        }
        Ok(Self { writer })
    }

    /// Log quand un trade est plac\u{e9}.
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
        time_above_start_s: u64,
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
            "{timestamp},{hour_utc},{day_of_week},{window},trade,\
             {btc_start:.2},{btc_current:.2},,{change_pct:.4},\
             {market_mid:.4},{implied_p_up:.4},{side},{token},\
             {edge_brut:.2},{edge_net:.2},{fee_pct:.2},{size_usdc:.2},{entry_price:.4},\
             {order_latency_ms},{fill_type},{remaining_s},{num_ws},{price_source},\
             {vol_pct:.4},{:.4},{:.4},{:.1},{:.8},\
             {spread:.4},{bid_depth:.2},{ask_depth:.2},{imbalance:.4},\
             {best_bid:.4},{best_ask:.4},{slippage_bps:.2},\
             {bid_levels},{ask_levels},{micro_vol:.4},{momentum_ratio:.4},\
             {sign_changes},{max_drawdown_bps:.2},{time_above_start_s},{ticks_count},\
             ,,{session_pnl:.4},{session_trades},{session_wr:.1},{consecutive_wins},{session_drawdown_pct:.2},",
            macro_data.btc_1h_pct, macro_data.btc_24h_pct, macro_data.btc_24h_vol_m, macro_data.funding_rate,
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }

    /// Log quand un bet est r\u{e9}solu (win/loss).
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
        // Fields: 1-timestamp, 2-hour, 3-dow, 4-window, 5-event, 6-btc_start, 7-(empty), 8-btc_resolution,
        // 9-change_pct, 10..43-(34 empty), 44-result, 45-pnl, 46-session_pnl, 47-session_trades,
        // 48-session_wr, 49-consecutive_wins, 50-session_drawdown_pct, 51-(empty skip_reason)
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{hour_utc},{day_of_week},{window},resolution,\
             {btc_start:.2},,{btc_resolution:.2},{change_pct:.4},\
             ,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,\
             {result},{pnl:.4},{session_pnl:.4},{session_trades},{session_wr:.1},{consecutive_wins},{session_drawdown_pct:.2},"
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }

    /// Log r\u{e9}sum\u{e9} de window sans trade (pour analyse des skips).
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
        // Fields: 1-timestamp, 2-hour, 3-dow, 4-window, 5-skip, 6-btc_start, 7-btc_end, 8-(empty),
        // 9-change_pct, 10-market_mid, 11-(empty), 12-reason(side), 13..18-(6 empty: token..entry_price),
        // 19..21-(3 empty: latency,fill,remaining), 22-num_ws, 23-price_source, 24-vol_pct,
        // 25..28-macro, 29..43-(15 empty: spread..ticks_count),
        // 44..50-(7 empty: result..session_drawdown), 51-skip_reason
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{hour_utc},{day_of_week},{window},skip,\
             {btc_start:.2},{btc_end:.2},,{change_pct:.4},\
             {market_mid:.4},,,\
             ,,,,,,,,,{num_ws},{price_source},\
             {vol_pct:.4},{:.4},{:.4},{:.1},{:.8},\
             ,,,,,,,,,,,,,,,\
             ,,,,,,,{reason}",
            macro_data.btc_1h_pct, macro_data.btc_24h_pct, macro_data.btc_24h_vol_m, macro_data.funding_rate,
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }
}

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
            writeln!(writer, "window,btc_start,btc_end,went_up,price_change_bps,delta_peak_pct,velocity_last_5s,ticks_in_window,mid_at_eval,reversal_detected")?;
            writer.flush()?;
        }
        Ok(Self { writer })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_outcome(
        &mut self,
        window: u64,
        btc_start: f64,
        btc_end: f64,
        delta_peak_pct: f64,
        velocity_last_5s: f64,
        ticks_in_window: u32,
        mid_at_eval: f64,
        reversal_detected: bool,
    ) {
        let went_up = btc_end >= btc_start;
        let change_bps = if btc_start > 0.0 { (btc_end - btc_start) / btc_start * 10000.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{window},{btc_start:.2},{btc_end:.2},{went_up},{change_bps:.2},{delta_peak_pct:.4},{velocity_last_5s:.6},{ticks_in_window},{mid_at_eval:.4},{reversal_detected}"
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("Outcome CSV write error: {e}");
        }
    }
}

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
        let _ = self.rotate_if_needed();
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
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self::date_from_epoch(secs)
    }

    fn date_from_epoch(secs: u64) -> String {
        let days = secs / 86400;
        let mut y = 1970u32;
        let mut remaining = days as u32;
        loop {
            let days_in_year = if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) { 366 } else { 365 };
            if remaining < days_in_year { break; }
            remaining -= days_in_year;
            y += 1;
        }
        let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn csv_header_and_trade_line() {
        let path = "/tmp/poly5m_test_trade3.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        let macro_data = MacroData { btc_1h_pct: -0.5, btc_24h_pct: 2.1, btc_24h_vol_m: 45000.0, funding_rate: 0.0001 };
        logger.log_trade(
            1700000000, 1699999800, 97150.50, 97155.0, 0.65, 0.62,
            "BUY_UP", "YES", 3.5, 2.9, 0.6, 2.0, 0.65,
            42, "FOK_filled", 10, 3, "CL", 0.12,
            &macro_data, 0.02, 500.0, 300.0, 0.625, 0.64, 0.66,
            5, 4, 0.0012, 0.85,
            3, 15.5, 120, 50,
            5.50, 3, 66.7, 2, 1.25,
        );
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("timestamp,hour_utc,day_of_week,window,event,"));
        assert!(lines[0].contains(",btc_1h_pct,btc_24h_pct,"));
        assert!(lines[0].contains(",micro_vol,momentum_ratio,"));
        assert!(lines[0].contains(",order_latency_ms,fill_type,"));
        assert!(lines[0].contains(",best_bid,best_ask,mid_vs_entry_slippage_bps,"));
        assert!(lines[0].contains(",sign_changes,max_drawdown_bps,time_above_start_s,ticks_count,"));
        assert!(lines[0].contains(",consecutive_wins,session_drawdown_pct,skip_reason"));
        assert!(lines[1].contains(",trade,"));
        assert!(lines[1].contains("BUY_UP"));
        assert!(lines[1].contains(",YES,"));
        assert!(lines[1].contains("FOK_filled"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_resolution_line() {
        let path = "/tmp/poly5m_test_resolution3.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        logger.log_resolution(1700000300, 1699999800, 97150.0, 97200.0, "WIN", 1.08, 5.50, 3, 66.7, 2, 1.25);
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains(",resolution,"));
        assert!(lines[1].contains("WIN"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_skip_line() {
        let path = "/tmp/poly5m_test_skip3.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        let macro_data = MacroData { btc_1h_pct: 1.2, btc_24h_pct: -3.0, btc_24h_vol_m: 50000.0, funding_rate: -0.0002 };
        logger.log_skip(1700000000, 1699999800, 97150.50, 97160.0, 0.95, 3, "CL", 0.12, &macro_data, "mid>0.90");
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains(",skip,"));
        assert!(lines[1].contains("mid>0.90"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_all_events_same_field_count() {
        let path = "/tmp/poly5m_test_alignment.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        let macro_data = MacroData::default();
        logger.log_trade(1700000000, 1699999800, 97150.0, 97155.0, 0.65, 0.62,
            "BUY_UP", "YES", 3.5, 2.9, 0.6, 2.0, 0.65,
            42, "FOK_filled", 10, 3, "CL", 0.12,
            &macro_data, 0.02, 500.0, 300.0, 0.625, 0.64, 0.66,
            5, 4, 0.001, 0.9,
            3, 15.5, 120, 50,
            5.50, 3, 66.7, 2, 1.25);
        logger.log_resolution(1700000300, 1699999800, 97150.0, 97200.0, "WIN", 1.08, 5.50, 3, 66.7, 0, 0.0);
        logger.log_skip(1700000000, 1699999800, 97150.0, 97160.0, 0.95, 3, "CL", 0.12, &macro_data, "test");
        drop(logger);

        let content = std::fs::read_to_string(path).unwrap();
        for (i, line) in content.trim().lines().enumerate() {
            assert_eq!(line.split(',').count(), 51,
                "Line {} has {} fields instead of 51: {}",
                i, line.split(',').count(), &line[..line.len().min(80)]);
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn outcome_logger_writes_header_and_row() {
        let path = "/tmp/poly5m_test_outcomes.csv";
        let _ = std::fs::remove_file(path);
        let mut logger = super::OutcomeLogger::new(path).unwrap();
        logger.log_outcome(1699999800, 97150.0, 97155.0, 0.01, 0.001, 50, 0.55, false);
        drop(logger);

        let content = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("window,btc_start,"));
        assert!(lines[0].contains("delta_peak_pct"));
        assert!(lines[1].contains("1699999800"));
        assert!(lines[1].contains("true"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn outcome_logger_appends_without_duplicate_header() {
        let path = "/tmp/poly5m_test_outcomes_append.csv";
        let _ = std::fs::remove_file(path);
        {
            let mut logger = super::OutcomeLogger::new(path).unwrap();
            logger.log_outcome(1699999800, 97150.0, 97155.0, 0.01, 0.001, 50, 0.55, false);
        }
        {
            let mut logger = super::OutcomeLogger::new(path).unwrap();
            logger.log_outcome(1700000100, 97155.0, 97140.0, 0.02, -0.001, 60, 0.45, true);
        }
        let content = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 3, "1 header + 2 data rows, got: {lines:?}");
        assert!(lines[2].contains("false"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tick_logger_writes_ticks() {
        let dir = "/tmp/poly5m_test_ticks";
        let _ = std::fs::remove_dir_all(dir);
        let mut logger = super::TickLogger::new(dir).unwrap();
        logger.log_tick(1700000000000, "RTDS", 97150.50, 1699999800);
        logger.log_tick(1700000000100, "WS", 97150.80, 1699999800);
        drop(logger);

        let entries: Vec<_> = std::fs::read_dir(dir).unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 ticks
        assert!(lines[0].starts_with("timestamp_ms,"));
        assert!(lines[1].contains("RTDS"));
        assert!(lines[2].contains("WS"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn csv_logger_appends_without_duplicate_header() {
        let path = "/tmp/poly5m_test_csv_append.csv";
        let _ = std::fs::remove_file(path);
        let macro_data = MacroData::default();
        {
            let mut logger = CsvLogger::new(path).unwrap();
            logger.log_trade(
                1700000000, 1699999800, 97150.0, 97155.0, 0.65, 0.62,
                "BUY_UP", "YES", 3.5, 2.9, 0.6, 2.0, 0.65,
                42, "FOK_filled", 10, 3, "CL", 0.12,
                &macro_data, 0.02, 500.0, 300.0, 0.625, 0.64, 0.66,
                5, 4, 0.001, 0.9,
                3, 15.5, 120, 50,
                5.50, 3, 66.7, 2, 1.25,
            );
        }
        {
            let mut logger = CsvLogger::new(path).unwrap();
            logger.log_trade(
                1700000300, 1700000100, 97155.0, 97160.0, 0.60, 0.58,
                "BUY_DOWN", "NO", 2.5, 1.9, 0.5, 1.5, 0.60,
                38, "GTC_filled", 8, 3, "WS", 0.10,
                &macro_data, 0.01, 400.0, 250.0, 0.615, 0.59, 0.61,
                4, 3, 0.002, 0.8,
                2, 10.0, 100, 40,
                4.00, 4, 75.0, 3, 0.50,
            );
        }
        let content = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 3, "1 header + 2 data rows, got: {lines:?}");
        assert!(lines[0].starts_with("timestamp,"));
        assert!(lines[1].contains("BUY_UP"));
        assert!(lines[2].contains("BUY_DOWN"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_skip_no_reason_in_side_column() {
        let path = "/tmp/poly5m_test_skip_side.csv";
        let _ = std::fs::remove_file(path);
        let macro_data = MacroData::default();
        let mut logger = CsvLogger::new(path).unwrap();
        logger.log_skip(1700000000, 1699999800, 97150.50, 97160.0, 0.95, 3, "CL", 0.12, &macro_data, "mid>0.90");
        drop(logger);
        let content = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        let fields: Vec<&str> = lines[1].split(',').collect();
        // side column (index 11) should be empty, skip_reason (index 50) should have the reason
        assert_eq!(fields[11], "", "side column should be empty for skips, got: {}", fields[11]);
        assert_eq!(fields[50], "mid>0.90", "skip_reason should contain reason");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tick_logger_date_from_epoch() {
        // 2026-02-20 00:00:00 UTC = 1771545600
        assert_eq!(super::TickLogger::date_from_epoch(1771545600), "20260220");
        // 2024-02-29 (leap year)
        assert_eq!(super::TickLogger::date_from_epoch(1709164800), "20240229");
        // 1970-01-01
        assert_eq!(super::TickLogger::date_from_epoch(0), "19700101");
    }
}
