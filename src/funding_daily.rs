//! Daily estimated fees, independent of immutable position and daily equity anchors.
use crate::manager_db::ManagerDb;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const CF: &str = "funding_daily";
pub const DAY_US: i64 = 86_400_000_000;
pub fn day_start(ts: i64) -> i64 {
    (ts + 8 * 3_600_000_000).div_euclid(DAY_US) * DAY_US - 8 * 3_600_000_000
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailyEstimate {
    pub source_id: String,
    pub scope_start_us: i64,
    pub day_us: i64,
    pub from_us: i64,
    pub through_us: i64,
    pub input_fingerprint: String,
    pub complete: bool,
    pub income_quote: Option<f64>,
    pub strategies: BTreeMap<String, f64>,
    pub symbols: BTreeMap<String, f64>,
}

pub struct FundingDailyStore(ManagerDb);
impl FundingDailyStore {
    pub fn open(db: ManagerDb) -> Result<Self> {
        // Creating this Manager-owned CF is independent of PostgreSQL schema maintenance.
        db.ensure_column_family(CF)?;
        Ok(Self(db))
    }
    fn key(source: &str, day: i64) -> Vec<u8> {
        format!("{source}\n{day:020}").into_bytes()
    }
    pub fn load(&self, source: &str, day: i64) -> Result<Option<DailyEstimate>> {
        let cf = self
            .0
            .db()
            .cf_handle(CF)
            .context("daily funding store unavailable")?;
        let Some(bytes) = self.0.db().get_cf(&cf, Self::key(source, day))? else {
            return Ok(None);
        };
        let value: DailyEstimate = serde_json::from_slice(&bytes)?;
        ensure!(
            value.source_id == source
                && value.day_us == day
                && day_start(day) == day
                && value.from_us >= day
                && value.through_us < day + DAY_US
                && value.through_us >= value.from_us
                && value.income_quote.is_none_or(f64::is_finite)
                && value
                    .strategies
                    .values()
                    .chain(value.symbols.values())
                    .all(|v| v.is_finite()),
            "invalid daily funding snapshot"
        );
        Ok(Some(value))
    }
    pub fn save(&self, value: &DailyEstimate) -> Result<bool> {
        if self.load(&value.source_id, value.day_us)?.as_ref() == Some(value) {
            return Ok(false);
        }
        let cf = self
            .0
            .db()
            .cf_handle(CF)
            .context("daily funding store unavailable")?;
        let mut options = rocksdb::WriteOptions::default();
        options.set_sync(true);
        self.0.db().put_cf_opt(
            &cf,
            Self::key(&value.source_id, value.day_us),
            serde_json::to_vec(value)?,
            &options,
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daily_fees_are_idempotent_durable_and_source_isolated() {
        let temp = tempfile::TempDir::new().unwrap();
        let day = day_start(1_790_000_000_000_000);
        let value = DailyEstimate {
            source_id: "a".into(),
            scope_start_us: day,
            day_us: day,
            from_us: day,
            through_us: day + DAY_US - 1,
            input_fingerprint: "fills".into(),
            complete: true,
            income_quote: Some(-2.5),
            strategies: BTreeMap::from([("s".into(), -2.5)]),
            symbols: BTreeMap::from([("BTCUSDT".into(), -2.5)]),
        };
        {
            let store = FundingDailyStore::open(ManagerDb::open(temp.path()).unwrap()).unwrap();
            assert!(store.save(&value).unwrap());
            assert!(!store.save(&value).unwrap());
            assert!(store.load("b", day).unwrap().is_none());
        }
        let store = FundingDailyStore::open(ManagerDb::open(temp.path()).unwrap()).unwrap();
        assert_eq!(store.load("a", day).unwrap(), Some(value));
    }
}
