//! HTTP contract for a single source's cumulative change since its monitoring anchor.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const INTERVAL_MS: i64 = 4 * 3_600_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Point {
    pub ts_ms: i64,
    pub pnl_quote: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub source_id: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub generated_at_ms: i64,
    pub interval_hours: u32,
    pub valuation: String,
    pub funding: String,
    pub complete: bool,
    pub loading: bool,
    pub unavailable_reason: Option<String>,
    pub points: Vec<Point>,
}

impl Monitor {
    pub fn pending(source: &str, reason: &str) -> Self {
        Self {
            source_id: source.into(),
            start_ms: None,
            end_ms: None,
            generated_at_ms: super::now_ms(),
            interval_hours: 4,
            valuation: String::new(),
            funding: String::new(),
            complete: false,
            loading: true,
            unavailable_reason: Some(reason.into()),
            points: vec![],
        }
    }

    pub fn validate(&self, source: &str, now: i64) -> Result<()> {
        ensure!(
            self.source_id == source && self.interval_hours == 4,
            "NAV identity or interval mismatch"
        );
        ensure!(
            self.generated_at_ms > 0 && self.generated_at_ms <= now + 30_000,
            "invalid NAV generation timestamp"
        );
        ensure!(self.points.len() <= 100_000, "NAV history too large");
        if self.points.is_empty() {
            ensure!(
                !self.complete && self.unavailable_reason.is_some(),
                "missing NAV history reason"
            );
            return Ok(());
        }
        let first = &self.points[0];
        let last = self.points.last().unwrap();
        ensure!(
            Some(first.ts_ms) == self.start_ms
                && Some(last.ts_ms) == self.end_ms
                && first.ts_ms > 0
                && last.ts_ms <= self.generated_at_ms + 30_000
                && first.pnl_quote.abs() < 1e-8,
            "invalid NAV endpoints"
        );
        ensure!(
            self.points.iter().all(|p| p.pnl_quote.is_finite())
                && self.points.windows(2).all(|w| w[0].ts_ms < w[1].ts_ms),
            "invalid NAV points"
        );
        ensure!(
            self.points
                .iter()
                .skip(1)
                .take(self.points.len().saturating_sub(2))
                .all(|p| p.ts_ms.rem_euclid(INTERVAL_MS) == 0),
            "unaligned NAV samples"
        );
        if self.complete {
            ensure!(
                self.points.windows(2).all(|w| {
                    let next = (w[0].ts_ms.div_euclid(INTERVAL_MS) + 1) * INTERVAL_MS;
                    w[1].ts_ms == next.min(last.ts_ms)
                }),
                "missing NAV samples"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_curves_require_identity_zero_origin_and_complete_grid() {
        let mut report = Monitor::pending("account", "loading");
        report.start_ms = Some(1);
        report.end_ms = Some(2 * INTERVAL_MS);
        report.generated_at_ms = 2 * INTERVAL_MS;
        report.points = vec![
            Point {
                ts_ms: 1,
                pnl_quote: 0.0,
            },
            Point {
                ts_ms: INTERVAL_MS,
                pnl_quote: 5.0,
            },
            Point {
                ts_ms: 2 * INTERVAL_MS,
                pnl_quote: 3.0,
            },
        ];
        report.complete = true;
        assert!(report.validate("account", 2 * INTERVAL_MS).is_ok());
        assert!(report.validate("another", 2 * INTERVAL_MS).is_err());
        report.points.remove(1);
        assert!(report.validate("account", 2 * INTERVAL_MS).is_err());
        report.complete = false;
        assert!(report.validate("account", 2 * INTERVAL_MS).is_ok());
        report.points[0].pnl_quote = 5.0;
        assert!(report.validate("account", 2 * INTERVAL_MS).is_err());
    }
}
