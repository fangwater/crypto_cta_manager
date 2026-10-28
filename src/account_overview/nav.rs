//! HTTP contract for a single source's cumulative change since its monitoring anchor.
use anyhow::{Context, Result, ensure};
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

/// The Manager owns only these derived display snapshots, never an Exec store.
/// Each source is atomically persisted, then published to the HTTP read cache.
pub struct Store {
    db: crate::manager_db::ManagerDb,
    values: std::sync::RwLock<std::collections::BTreeMap<String, Saved>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub input_key: String,
    pub through_ms: i64,
    pub report: Monitor,
}

pub fn grid_end(now_ms: i64) -> i64 {
    // Shanghai's UTC+8 offset is exactly two four-hour intervals.
    now_ms.div_euclid(INTERVAL_MS) * INTERVAL_MS
}

impl Monitor {
    /// Remove the moving latest point from upstream interfaces. The factual
    /// origin is the sole allowed exception to the fixed wall-clock grid.
    pub fn closed_grid(mut self, cutoff: i64) -> Result<Self> {
        self.validate(&self.source_id, super::now_ms())?;
        self.points.retain(|p| p.ts_ms <= cutoff);
        while self.points.len() > 1
            && self.points.last().unwrap().ts_ms.rem_euclid(INTERVAL_MS) != 0
        {
            self.points.pop();
        }
        self.start_ms = self.points.first().map(|p| p.ts_ms);
        self.end_ms = self.points.last().map(|p| p.ts_ms);
        ensure!(self.end_ms == Some(cutoff), "four-hour NAV point not ready");
        ensure!(
            self.points
                .windows(2)
                .all(|w| { w[1].ts_ms == (w[0].ts_ms.div_euclid(INTERVAL_MS) + 1) * INTERVAL_MS }),
            "four-hour NAV history has missing samples"
        );
        Ok(self)
    }
}

impl Store {
    const CF: &str = "nav_monitor";
    pub fn open(db: crate::manager_db::ManagerDb) -> Result<Self> {
        db.ensure_column_family(Self::CF)?;
        let mut values = std::collections::BTreeMap::new();
        let cf = db.db().cf_handle(Self::CF).unwrap();
        for entry in db.db().iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (key, bytes) = entry?;
            let saved = (|| -> Result<Saved> {
                let saved: Saved = serde_json::from_slice(&bytes)?;
                saved
                    .report
                    .validate(&saved.report.source_id, super::now_ms())?;
                ensure!(
                    key.as_ref() == saved.report.source_id.as_bytes(),
                    "NAV key mismatch"
                );
                saved.report.clone().closed_grid(saved.through_ms)?;
                ensure!(
                    saved.report.end_ms == Some(saved.through_ms)
                        && grid_end(saved.through_ms) == saved.through_ms,
                    "invalid stored NAV grid"
                );
                Ok(saved)
            })();
            match saved {
                Ok(saved) => {
                    values.insert(saved.report.source_id.clone(), saved);
                }
                Err(error) => {
                    tracing::warn!(%error, "invalid derived NAV cache; rebuilding source in background")
                }
            }
        }
        drop(cf);
        Ok(Self {
            db,
            values: std::sync::RwLock::new(values),
        })
    }
    pub fn is_current(&self, source: &str, cutoff: i64, input_key: &str) -> bool {
        self.values.read().unwrap().get(source).is_some_and(|s| {
            s.through_ms == cutoff && s.input_key == input_key && !s.report.loading
        })
    }
    pub fn saved(&self, source: &str) -> Option<Saved> {
        self.values.read().unwrap().get(source).cloned()
    }
    pub fn read(&self, source: &str, now: i64) -> Monitor {
        let Some(saved) = self.saved(source) else {
            return Monitor::pending(source, "四小时净值历史正在后台生成");
        };
        let mut report = saved.report;
        if saved.through_ms < grid_end(now) {
            report.complete = false;
            report.loading = true;
            report.unavailable_reason = Some("新一期四小时净值正在后台生成，保留已保存历史".into());
        }
        report
    }
    pub fn save(&self, report: Monitor, input_key: String, through_ms: i64) -> Result<()> {
        let report = report.closed_grid(through_ms)?;
        let saved = Saved {
            input_key,
            through_ms,
            report,
        };
        let cf = self
            .db
            .db()
            .cf_handle(Self::CF)
            .context("NAV display store unavailable")?;
        let mut options = rocksdb::WriteOptions::default();
        options.set_sync(true);
        self.db.db().put_cf_opt(
            &cf,
            saved.report.source_id.as_bytes(),
            serde_json::to_vec(&saved)?,
            &options,
        )?;
        self.values
            .write()
            .unwrap()
            .insert(saved.report.source_id.clone(), saved);
        Ok(())
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    fn curve() -> Monitor {
        let mut report = Monitor::pending("account:a", "initializing");
        report.points = vec![
            Point {
                ts_ms: 1,
                pnl_quote: 0.0,
            },
            Point {
                ts_ms: INTERVAL_MS,
                pnl_quote: 2.5,
            },
            Point {
                ts_ms: 2 * INTERVAL_MS,
                pnl_quote: -1.5,
            },
            Point {
                ts_ms: 2 * INTERVAL_MS + 1,
                pnl_quote: 100.0,
            },
        ];
        report.start_ms = Some(1);
        report.end_ms = Some(2 * INTERVAL_MS + 1);
        report.generated_at_ms = 2 * INTERVAL_MS + 1;
        report.loading = false;
        report.complete = true;
        report.unavailable_reason = None;
        report
    }
    #[test]
    fn persists_closed_grid_and_serves_after_restart_without_reconstruction() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(crate::manager_db::ManagerDb::open(dir.path()).unwrap()).unwrap();
        store
            .save(curve(), "funding:false".into(), 2 * INTERVAL_MS)
            .unwrap();
        drop(store);
        let store = Store::open(crate::manager_db::ManagerDb::open(dir.path()).unwrap()).unwrap();
        let value = store.read("account:a", 3 * INTERVAL_MS - 1);
        assert_eq!(value.points.len(), 3);
        assert_eq!(value.end_ms, Some(2 * INTERVAL_MS));
        assert_eq!(value.points.last().unwrap().pnl_quote, -1.5);
        assert!(value.complete);
        assert!(!value.loading);
        let pending = store.read("account:a", 3 * INTERVAL_MS);
        assert!(pending.loading);
        assert!(!pending.complete);
        assert_eq!(pending.points.len(), 3);
        assert!(
            store
                .read("another-account", 3 * INTERVAL_MS)
                .points
                .is_empty()
        );
    }
    #[test]
    fn incomplete_or_invalid_update_cannot_replace_saved_history() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(crate::manager_db::ManagerDb::open(dir.path()).unwrap()).unwrap();
        store.save(curve(), "old".into(), 2 * INTERVAL_MS).unwrap();
        let mut missing = curve();
        missing.complete = false;
        missing.points.remove(1);
        assert!(store.save(missing, "bad".into(), 2 * INTERVAL_MS).is_err());
        assert!(
            store
                .save(curve(), "future".into(), 3 * INTERVAL_MS)
                .is_err()
        );
        assert_eq!(store.saved("account:a").unwrap().input_key, "old");
    }
    #[test]
    fn cutoffs_are_shanghai_wall_clock_hours_and_do_not_include_moving_tail() {
        // 2026-10-28 12:59:59 Shanghai -> 12:00:00, then 16:00:00.
        let noon = 1_792_123_200_000i64;
        assert_eq!((grid_end(noon) / 3_600_000 + 8).rem_euclid(4), 0);
        assert_eq!(grid_end(2 * INTERVAL_MS + 3_599_999), 2 * INTERVAL_MS);
        assert_eq!(grid_end(3 * INTERVAL_MS), 3 * INTERVAL_MS);
        let closed = curve().closed_grid(2 * INTERVAL_MS).unwrap();
        assert!(
            closed
                .points
                .iter()
                .skip(1)
                .all(|p| p.ts_ms % INTERVAL_MS == 0)
        );
    }
}
