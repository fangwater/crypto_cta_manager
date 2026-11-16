//! Reclaim already-archived hot data, at most one CF/range per maintenance tick.
//! Whole obsolete SST deletion avoids rewrite; manual compaction is conservative.
use super::*;
use crate::position_archive::{PositionArchive, storage};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::time::Instant;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub max_input_bytes: u64,
    pub max_files: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_files: 4,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (4096..=64 * 1024 * 1024).contains(&self.max_input_bytes)
                && (1..=16).contains(&self.max_files),
            "invalid small hot reclamation budget"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub action: String,
    pub column_family: Option<String>,
    pub files: usize,
    pub observed_sst_bytes: u64,
    pub observed_memtable_bytes: u64,
    pub duration_ms: u64,
    pub deferred_reason: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug)]
enum Plan {
    Drop {
        start: Vec<u8>,
        end: Vec<u8>,
        files: usize,
        bytes: u64,
    },
    Compact {
        start: Vec<u8>,
        end: Vec<u8>,
        files: usize,
        bytes: u64,
    },
}
fn keys(file: &rocksdb::LiveFile) -> Option<(&[u8], &[u8])> {
    Some((file.start_key.as_deref()?, file.end_key.as_deref()?))
}

fn plan(
    files: &[&rocksdb::LiveFile],
    boundary: &[u8],
    memtable: u64,
    limits: &Settings,
) -> Option<Plan> {
    let mut eligible = files
        .iter()
        .filter(|f| keys(f).is_some_and(|(a, b)| a <= b && a < boundary))
        .copied()
        .collect::<Vec<_>>();
    eligible.sort_by(|a, b| a.start_key.cmp(&b.start_key));
    // First prefer metadata-only whole-file removal. Never touch L0 or files
    // straddling the retained boundary; DeleteFilesInRange uses inclusive end.
    for file in eligible.iter().filter(|f| f.level > 0).take(16) {
        let (start, end) = keys(file)?;
        if end >= boundary {
            continue;
        }
        let contained = files
            .iter()
            .filter(|f| keys(f).is_some_and(|(a, b)| a <= end && b >= start))
            .collect::<Vec<_>>();
        // A range-tombstone file must not uncover older overlapping versions.
        if contained.len() <= limits.max_files
            && contained
                .iter()
                .all(|f| f.level > 0 && keys(f).is_some_and(|(a, b)| a >= start && b <= end))
        {
            return Some(Plan::Drop {
                start: start.to_vec(),
                end: end.to_vec(),
                files: contained.len(),
                bytes: contained.iter().map(|f| f.size as u64).sum(),
            });
        }
    }
    if memtable >= limits.max_input_bytes {
        return None;
    }
    // Range alone does not bound work. Include full overlapping SST spans across
    // ALL levels and any memtable flush in the preflight budget. Large/shared
    // boundary files are left to ordinary background compaction.
    for file in eligible.iter().take(16) {
        let (start, end) = keys(file)?;
        let mut low = start.to_vec();
        let mut high = end.to_vec();
        let mut seen = BTreeSet::new();
        let mut bytes = 0u64;
        let mut too_large = false;
        loop {
            let previous = seen.len();
            for (n, other) in files.iter().enumerate() {
                let (a, b) = keys(other)?;
                if a <= high.as_slice() && b >= low.as_slice() && seen.insert(n) {
                    bytes = bytes.saturating_add(other.size as u64);
                    if seen.len() > limits.max_files
                        || bytes.saturating_add(memtable) > limits.max_input_bytes
                    {
                        too_large = true;
                        break;
                    }
                    if a < low.as_slice() {
                        low = a.to_vec();
                    }
                    if b > high.as_slice() {
                        high = b.to_vec();
                    }
                }
            }
            if too_large || seen.len() == previous {
                break;
            }
        }
        if !too_large {
            let mut end = high;
            end.push(0);
            end = end.min(boundary.to_vec());
            if low < end {
                return Some(Plan::Compact {
                    start: low,
                    end,
                    files: seen.len(),
                    bytes,
                });
            }
        }
    }
    None
}

impl PositionArchive {
    pub(super) fn reclaim_hot_once(&self) -> Report {
        let start = Instant::now();
        let mut report = Report {
            action: "deferred".into(),
            ..Default::default()
        };
        if let Err(error) = self.reclaim_hot_inner(&mut report) {
            report.error = Some(error.to_string());
        }
        report.duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        report
    }
    fn reclaim_hot_inner(&self, report: &mut Report) -> Result<()> {
        let cold = self.cold.as_ref().context("cold mode missing")?;
        let floor = self.retention_floor();
        if floor == (1, 0) {
            report.deferred_reason = Some("no verified archived prefix".into());
            return Ok(());
        }
        let all = self.db.db().live_files()?;
        let names: &[&str] = if self.legacy_json {
            &[crate::manager_db::POSITION_UPDATES_CF]
        } else {
            &[
                crate::manager_db::POSITION_UPDATES_CF,
                storage::OBSERVATIONS_CF,
                storage::SNAPSHOTS_CF,
            ]
        };
        let index = cold.reclaim_cursor.fetch_add(1, Ordering::Relaxed) % names.len();
        let name = names[index];
        report.column_family = Some(name.into());
        let cf = self.db.db().cf_handle(name).context("missing signal CF")?;
        if self
            .db
            .db()
            .property_int_value("rocksdb.num-running-compactions")?
            != Some(0)
        {
            report.deferred_reason = Some("background compaction active".into());
            return Ok(());
        }
        let memtable = self
            .db
            .db()
            .property_int_value_cf(&cf, "rocksdb.cur-size-all-mem-tables")?
            .context("memtable size unavailable")?;
        let files = all
            .iter()
            .filter(|f| f.column_family_name == name)
            .collect::<Vec<_>>();
        if files.iter().any(|f| keys(f).is_none()) {
            report.deferred_reason = Some("SST key bounds unavailable".into());
            return Ok(());
        }
        let boundary = if name == storage::SNAPSHOTS_CF {
            storage::day_key(floor.0).to_vec()
        } else {
            encode_seq_key(floor.0, floor.1)?.to_vec()
        };
        let Some(plan) = plan(&files, &boundary, memtable, &cold.settings.reclaim) else {
            report.deferred_reason = Some(
                "no small reclaimed SST range; defer large files to background compaction".into(),
            );
            return Ok(());
        };
        // Keep existing readers consistent, but never wait for a long theory
        // rebuild just to reclaim space. Publication does not take this gate.
        let Ok(_gate) = self.retention_gate.try_write() else {
            report.deferred_reason = Some("archive readers active".into());
            return Ok(());
        };
        ensure!(
            floor <= self.retention_floor(),
            "retention boundary moved backwards"
        );
        self.db.db().flush_wal(true)?;
        match plan {
            Plan::Drop {
                start,
                end,
                files,
                bytes,
            } => {
                self.db.db().delete_file_in_range_cf(&cf, start, end)?;
                report.action = "drop_obsolete_sst".into();
                report.files = files;
                report.observed_sst_bytes = bytes;
            }
            Plan::Compact {
                start,
                end,
                files,
                bytes,
            } => {
                if self
                    .db
                    .db()
                    .property_int_value("rocksdb.background-errors")?
                    != Some(0)
                {
                    report.deferred_reason =
                        Some("RocksDB background errors present or unknown".into());
                    return Ok(());
                }
                if self
                    .db
                    .db()
                    .property_int_value("rocksdb.num-running-compactions")?
                    != Some(0)
                {
                    report.deferred_reason = Some("background compaction active".into());
                    return Ok(());
                }
                space::require(
                    self.db.path(),
                    cold.settings.min_free_bytes,
                    cold.settings.reclaim.max_input_bytes.saturating_mul(4),
                )?;
                let mut options = rocksdb::CompactOptions::default();
                options.set_exclusive_manual_compaction(false);
                options.set_change_level(false);
                options.set_bottommost_level_compaction(
                    rocksdb::BottommostLevelCompaction::ForceOptimized,
                );
                self.db
                    .db()
                    .compact_range_cf_opt(&cf, Some(start), Some(end), &options);
                ensure!(
                    self.db
                        .db()
                        .property_int_value("rocksdb.background-errors")?
                        == Some(0),
                    "RocksDB reported background errors during small compaction"
                );
                report.action = "compact_small_range".into();
                report.files = files;
                report.observed_sst_bytes = bytes;
                report.observed_memtable_bytes = memtable;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(name: &str, level: i32, low: u8, high: u8, size: usize) -> rocksdb::LiveFile {
        rocksdb::LiveFile {
            column_family_name: "position_updates".into(),
            name: name.into(),
            size,
            level,
            start_key: Some(vec![low]),
            end_key: Some(vec![high]),
            num_entries: 1,
            num_deletions: 0,
        }
    }
    #[test]
    fn whole_file_reclaim_requires_an_expired_closed_overlap_group() {
        let old = file("old", 6, 10, 20, 128 << 20);
        let retained = file("recent", 0, 80, 90, 128 << 20);
        assert!(matches!(
            plan(&[&old, &retained], &[50], u64::MAX, &Settings::default()),
            Some(Plan::Drop { files: 1, .. })
        ));
        let overlapping = file("overlap", 5, 15, 60, 128 << 20);
        assert!(plan(&[&old, &overlapping], &[50], 0, &Settings::default()).is_none());
        let level0 = file("l0", 0, 10, 20, 128 << 20);
        assert!(plan(&[&old, &level0], &[50], 0, &Settings::default()).is_none());
        assert!(plan(&[&old], &[20], 0, &Settings::default()).is_none());
    }
    #[test]
    fn range_budget_includes_transitive_overlap_memtables_and_file_count() {
        let a = file("a", 0, 10, 20, 1 << 20);
        let b = file("b", 1, 15, 30, 1 << 20);
        let huge = file("c", 2, 25, 40, 32 << 20);
        assert!(matches!(
            plan(&[&a, &b], &[25], 1 << 20, &Settings::default()),
            Some(Plan::Compact { files: 2, .. })
        ));
        assert!(plan(&[&a, &b, &huge], &[25], 0, &Settings::default()).is_none());
        assert!(plan(&[&a], &[25], 16 << 20, &Settings::default()).is_none());
        let limits = Settings {
            max_files: 1,
            ..Default::default()
        };
        assert!(plan(&[&a, &b], &[25], 0, &limits).is_none());
    }
    #[test]
    fn real_small_compaction_preserves_retained_json_and_protected_cfs() {
        let (_temp, archive, rows) = super::super::manager::tests::setup(true);
        let cf = archive
            .db
            .db()
            .cf_handle(crate::manager_db::POSITION_UPDATES_CF)
            .unwrap();
        archive
            .db
            .db()
            .set_options_cf(&cf, &[("disable_auto_compactions", "true")])
            .unwrap();
        archive.db.db().flush_cf(&cf).unwrap();
        archive
            .archive_cold_once(super::super::manager::tests::NOW)
            .unwrap()
            .unwrap();
        let report = archive.cold_status().unwrap().hot_reclaim.unwrap();
        assert_eq!(report.action, "compact_small_range", "{report:?}");
        assert!(report.error.is_none(), "{report:?}");
        assert!(report.observed_sst_bytes + report.observed_memtable_bytes <= 16 << 20);
        assert_eq!(
            archive
                .scan_from(archive.retention_floor().0)
                .unwrap()
                .len(),
            rows.len() - 2
        );
        assert_eq!(
            archive
                .db
                .db()
                .get_cf(
                    &archive.db.db().cf_handle("klines_1m").unwrap(),
                    b"protected"
                )
                .unwrap()
                .unwrap(),
            b"candle"
        );
        assert_eq!(
            archive
                .db
                .db()
                .get(b"position_current:protected")
                .unwrap()
                .unwrap(),
            b"current target"
        );
    }

    #[test]
    fn real_whole_sst_reclamation_removes_only_archived_file_and_survives_restart() {
        use crate::manager_db::{ManagerDb, POSITION_UPDATES_CF};
        let root = tempfile::tempdir().unwrap();
        let db = ManagerDb::open(&root.path().join("hot")).unwrap();
        let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
        db.db()
            .set_options_cf(&cf, &[("disable_auto_compactions", "true")])
            .unwrap();
        let (old, _, _) = super::super::tests::records(10 * 86_400_000_000, 3);
        for (key, value) in &old {
            db.db()
                .put_cf(
                    &cf,
                    encode_seq_key(key.0, key.1).unwrap(),
                    serde_json::to_vec(&codec::decode(value).unwrap()).unwrap(),
                )
                .unwrap();
        }
        db.db().flush_cf(&cf).unwrap();
        let mut opts = rocksdb::CompactOptions::default();
        opts.set_change_level(true);
        opts.set_target_level(6);
        opts.set_bottommost_level_compaction(rocksdb::BottommostLevelCompaction::Force);
        db.db()
            .compact_range_cf_opt(&cf, None::<&[u8]>, None::<&[u8]>, &opts);
        let file = db
            .db()
            .live_files()
            .unwrap()
            .into_iter()
            .find(|f| f.column_family_name == POSITION_UPDATES_CF && f.level > 0)
            .expect("isolated old SST reached a nonzero level")
            .name;
        drop(cf);
        let settings = manager::Settings {
            root: root.path().join("cold"),
            ..Default::default()
        };
        let archive = PositionArchive::open_with_cold(db, Some(settings.clone())).unwrap();
        let recent =
            codec::decode(&super::super::tests::records(99 * 86_400_000_000, 1).0[0].1).unwrap();
        archive
            .append(
                recent.received_at_us,
                &recent.strategy,
                vec![],
                recent.published_accounts,
            )
            .unwrap();
        let s = archive
            .archive_cold_once(100 * 86_400_000_000)
            .unwrap()
            .unwrap();
        assert_eq!(s.count, 3);
        let report = archive.cold_status().unwrap().hot_reclaim.unwrap();
        assert_eq!(report.action, "drop_obsolete_sst", "{report:?}");
        assert!(report.error.is_none(), "{report:?}");
        assert!(
            !archive
                .db
                .db()
                .live_files()
                .unwrap()
                .iter()
                .any(|f| f.name == file)
        );
        assert_eq!(
            archive
                .scan_from(archive.retention_floor().0)
                .unwrap()
                .len(),
            1
        );
        drop(archive);
        let archive = PositionArchive::open_with_cold(
            ManagerDb::open(&root.path().join("hot")).unwrap(),
            Some(settings),
        )
        .unwrap();
        assert_eq!(
            archive.latest().unwrap().unwrap().received_at_us,
            recent.received_at_us
        );
        assert_eq!(
            archive
                .scan_from(archive.retention_floor().0)
                .unwrap()
                .len(),
            1
        );
    }
}
