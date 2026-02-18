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
        writeln!(writer, "timestamp,window,event,chainlink_price,exchange_price,market_up_price,side,edge_pct,size_usdc,price,result,pnl,session_pnl")?;
        writer.flush()?;
        Ok(Self { writer })
    }

    /// Log quand un trade est placé.
    pub fn log_trade(
        &mut self,
        timestamp: u64,
        window: u64,
        chainlink_price: f64,
        exchange_price: Option<f64>,
        market_up_price: f64,
        side: &str,
        edge_pct: f64,
        size_usdc: f64,
        price: f64,
    ) {
        let ex = exchange_price.map_or(String::new(), |p| format!("{p:.2}"));
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{window},trade,{chainlink_price:.2},{ex},{market_up_price:.4},{side},{edge_pct:.2},{size_usdc:.2},{price:.4},,,",
        ).and_then(|_| self.writer.flush()) {
            tracing::warn!("CSV write error: {e}");
        }
    }

    /// Log quand un bet est résolu (win/loss).
    pub fn log_resolution(
        &mut self,
        timestamp: u64,
        window: u64,
        result: &str,
        pnl: f64,
        session_pnl: f64,
    ) {
        if let Err(e) = writeln!(
            self.writer,
            "{timestamp},{window},resolution,,,,,,,,{result},{pnl:.4},{session_pnl:.4}",
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
        let path = "/tmp/poly5m_test_trade.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        logger.log_trade(1700000000, 1699999800, 97150.50, Some(97155.0), 0.65, "BUY_UP", 3.5, 2.0, 0.65);
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("timestamp,window,event,"));
        assert!(lines[1].contains(",trade,"));
        assert!(lines[1].contains("BUY_UP"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_resolution_line() {
        let path = "/tmp/poly5m_test_resolution.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        logger.log_resolution(1700000300, 1699999800, "WIN", 1.08, 5.50);
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
    fn csv_trade_without_exchange_price() {
        let path = "/tmp/poly5m_test_no_ex.csv";
        let mut logger = CsvLogger::new(path).unwrap();
        logger.log_trade(1700000000, 1699999800, 97150.50, None, 0.50, "BUY_DOWN", 2.1, 1.50, 0.50);
        drop(logger);

        let mut content = String::new();
        File::open(path).unwrap().read_to_string(&mut content).unwrap();
        let line = content.lines().nth(1).unwrap();
        // exchange_price should be empty between two commas
        assert!(line.contains(",97150.50,,0.5000,"));
        std::fs::remove_file(path).ok();
    }
}
