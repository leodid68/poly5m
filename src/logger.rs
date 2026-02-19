use crate::macro_data::MacroData;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};

pub struct CsvLogger {
    writer: BufWriter<File>,
}

impl CsvLogger {
    pub fn new(path: &str) -> Result<Self> {
        let file = File::create(path).context("Cannot create CSV log file")?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "timestamp,window,event,btc_start,btc_current,btc_resolution,price_change_pct,market_mid,implied_p_up,side,token,edge_brut_pct,edge_net_pct,fee_pct,size_usdc,entry_price,remaining_s,num_ws_src,vol_pct,btc_1h_pct,btc_24h_pct,btc_24h_vol_m,funding_rate,spread,bid_depth,ask_depth,book_imbalance,bid_levels,ask_levels,result,pnl,session_pnl,session_trades,session_wr_pct")?;
        writer.flush()?;
        Ok(Self { writer })
    }

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
        remaining_s: u64,
        num_ws: u8,
        vol_pct: f64,
        macro_data: &MacroData,
        spread: f64,
        bid_depth: f64,
        ask_depth: f64,
        imbalance: f64,
        bid_levels: u32,
        ask_levels: u32,
    ) {
        let change_pct = if btc_start > 0.0 { (btc_current - btc_start) / btc_start * 100.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{window},trade,{btc_start:.2},{btc_current:.2},,{change_pct:.4},{market_mid:.4},{implied_p_up:.4},{side},{token},{edge_brut:.2},{edge_net:.2},{fee_pct:.2},{size_usdc:.2},{entry_price:.4},{remaining_s},{num_ws},{vol_pct:.4},{:.4},{:.4},{:.1},{:.8},{spread:.4},{bid_depth:.2},{ask_depth:.2},{imbalance:.4},{bid_levels},{ask_levels},,,,,",
            macro_data.btc_1h_pct, macro_data.btc_24h_pct, macro_data.btc_24h_vol_m, macro_data.funding_rate,
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }

    /// Log quand un bet est résolu (win/loss).
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
    ) {
        let change_pct = if btc_start > 0.0 { (btc_resolution - btc_start) / btc_start * 100.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{window},resolution,{btc_start:.2},,{btc_resolution:.2},{change_pct:.4},,,,,,,,,,,,,,,,,,,,,{result},{pnl:.4},{session_pnl:.4},{session_trades},{session_wr:.1}"
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }

    /// Log résumé de window sans trade (pour analyse des skips).
    pub fn log_skip(
        &mut self,
        timestamp: u64,
        window: u64,
        btc_start: f64,
        btc_end: f64,
        market_mid: f64,
        num_ws: u8,
        vol_pct: f64,
        macro_data: &MacroData,
        reason: &str,
    ) {
        let change_pct = if btc_start > 0.0 { (btc_end - btc_start) / btc_start * 100.0 } else { 0.0 };
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{window},skip,{btc_start:.2},{btc_end:.2},,{change_pct:.4},{market_mid:.4},,{reason},,,,,,,,{num_ws},{vol_pct:.4},{:.4},{:.4},{:.1},{:.8},,,,,,,,,,",
            macro_data.btc_1h_pct, macro_data.btc_24h_pct, macro_data.btc_24h_vol_m, macro_data.funding_rate,
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
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
            "BUY_UP", "YES", 3.5, 2.9, 0.6, 2.0, 0.65, 10, 3, 0.12,
            &macro_data, 0.02, 500.0, 300.0, 0.625, 5, 4,
        );
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("timestamp,window,event,"));
        assert!(lines[0].contains(",btc_1h_pct,btc_24h_pct,"));
        assert!(lines[1].contains(",trade,"));
        assert!(lines[1].contains("BUY_UP"));
        assert!(lines[1].contains(",YES,"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_resolution_line() {
        let path = "/tmp/poly5m_test_resolution3.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        logger.log_resolution(1700000300, 1699999800, 97150.0, 97200.0, "WIN", 1.08, 5.50, 3, 66.7);
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
        logger.log_skip(1700000000, 1699999800, 97150.50, 97160.0, 0.95, 3, 0.12, &macro_data, "mid>0.90");
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains(",skip,"));
        assert!(lines[1].contains("mid>0.90"));
        std::fs::remove_file(path).ok();
    }
}
