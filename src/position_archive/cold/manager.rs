//! Manager's bounded, idle-gated hot-to-cold worker. No exchange/SQL/Redis calls.
use super::*;
use crate::manager_db::{ManagerDb, POSITION_UPDATES_CF};
use crate::position_archive::{PositionArchive, storage};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LAST_ATTEMPT: &[u8] = b"position_cold_last_attempt_us";
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub root: PathBuf,
    pub interval_secs: u64,
    pub quiet_secs: u64,
    pub max_cpu_percent: f64,
    pub max_io_pressure_percent: f64,
    pub limits: BatchLimits,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            interval_secs: 3600,
            quiet_secs: 120,
            max_cpu_percent: 35.,
            max_io_pressure_percent: 1.,
            limits: BatchLimits::default(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.limits.validate()?;
        ensure!(
            self.root.is_absolute()
                && self
                    .root
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
            "cold.root must be an absolute path without parent components"
        );
        ensure!(
            (3600..=31 * 86400).contains(&self.interval_secs)
                && (30..=3600).contains(&self.quiet_secs),
            "cold interval must be hourly or slower, with a bounded quiet period"
        );
        ensure!(
            self.max_cpu_percent.is_finite()
                && (0.0..=100.0).contains(&self.max_cpu_percent)
                && self.max_io_pressure_percent.is_finite()
                && (0.0..=100.0).contains(&self.max_io_pressure_percent),
            "invalid cold load thresholds"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub running: bool,
    pub last_attempt_us: Option<i64>,
    pub last_completed_us: Option<i64>,
    pub moved_records: u64,
    pub last_batch_records: u64,
    pub last_batch_bytes: u64,
    pub oldest_hot_received_at_us: Option<i64>,
    pub expired_hot_age_secs: Option<i64>,
    pub hot_estimated_records: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub io_pressure_percent: Option<f64>,
    pub deferred_reason: Option<String>,
    pub last_error: Option<String>,
}
pub(crate) struct State {
    pub settings: Settings,
    pub root: ColdRoot,
    pub status: Mutex<Status>,
}
impl State {
    pub fn open(db: &ManagerDb, settings: Settings) -> Result<Self> {
        settings.validate()?;
        let hot = db.path().canonicalize()?;
        let parent = settings
            .root
            .parent()
            .context("cold root parent missing")?
            .canonicalize()?;
        let resolved = parent.join(
            settings
                .root
                .file_name()
                .context("cold root name missing")?,
        );
        ensure!(
            !resolved.starts_with(&hot) && !hot.starts_with(&resolved),
            "cold root overlaps hot DB through a symlink"
        );
        let identity = std::fs::read_to_string(db.path().join("IDENTITY"))?;
        let root = ColdRoot::create(&settings.root, identity.trim())?;
        let last = db
            .db()
            .get(LAST_ATTEMPT)?
            .map(|b| -> Result<i64> {
                Ok(i64::from_be_bytes(
                    b.as_slice()
                        .try_into()
                        .context("invalid cold attempt marker")?,
                ))
            })
            .transpose()?;
        Ok(Self {
            settings,
            root,
            status: Mutex::new(Status {
                last_attempt_us: last,
                ..Default::default()
            }),
        })
    }
}

fn successor(k: Cursor) -> Result<Cursor> {
    if k.1 == u32::MAX {
        Ok((k.0.checked_add(1).context("cold cursor overflow")?, 0))
    } else {
        Ok((k.0, k.1 + 1))
    }
}

impl PositionArchive {
    /// At most one cold DB transaction; no forced hot compaction.
    pub fn archive_cold_once(&self, now_us: i64) -> Result<Option<Segment>> {
        let state = self
            .cold
            .as_ref()
            .context("cold archival is not configured")?;
        let mut status = state.status.lock().unwrap();
        if status.running {
            status.deferred_reason = Some("cold attempt already running".into());
            return Ok(None);
        }
        if status.last_attempt_us.is_some_and(|t| {
            now_us.saturating_sub(t) < state.settings.interval_secs as i64 * 1_000_000
        }) {
            status.deferred_reason = Some("hourly interval".into());
            return Ok(None);
        }
        self.db
            .db()
            .put_opt(LAST_ATTEMPT, now_us.to_be_bytes(), &sync_options())?;
        status.last_attempt_us = Some(now_us);
        status.running = true;
        drop(status);
        let result = self.archive_cold_inner(now_us);
        let cf = self.db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
        let oldest = self
            .db
            .db()
            .iterator_cf(&cf, IteratorMode::Start)
            .next()
            .and_then(|row| row.ok())
            .and_then(|(key, _)| decode_seq_key(&key))
            .map(|key| key.0);
        let estimated = self
            .db
            .db()
            .property_int_value_cf(&cf, "rocksdb.estimate-num-keys")
            .ok()
            .flatten();
        let mut status = state.status.lock().unwrap();
        status.running = false;
        status.oldest_hot_received_at_us = oldest;
        status.expired_hot_age_secs =
            oldest.map(|t| retention::cutoff(now_us).0.saturating_sub(t).max(0) / 1_000_000);
        status.hot_estimated_records = estimated;
        match &result {
            Ok(Some(s)) => {
                status.last_completed_us = Some(now_us);
                status.last_batch_records = s.count;
                status.last_batch_bytes = s.encoded_bytes;
                status.moved_records += s.count;
                status.last_error = None;
                status.deferred_reason = None;
            }
            Ok(None) => {
                status.last_error = None;
                status.deferred_reason = Some("no expired hot records".into());
            }
            Err(e) => status.last_error = Some(e.to_string()),
        }
        result
    }
    fn archive_cold_inner(&self, now_us: i64) -> Result<Option<Segment>> {
        let cold = self.cold.as_ref().unwrap();
        if let Some(pending) = cold.root.pending()? {
            ensure!(
                verify_segment(&cold.root.segment_path(&pending.id)?)? == pending,
                "pending cold data failed independent verification; hot originals retained"
            );
            ensure!(
                pending.last < retention::cutoff(now_us),
                "pending cold batch exceeds retention"
            );
            self.finish_cold_hot_commit(&pending)?;
            cold.root.confirm(&pending)?;
            return Ok(Some(pending));
        }
        let original = self.retention.lock().unwrap().clone();
        let before = retention::cutoff(now_us);
        if original.floor >= before {
            return Ok(None);
        }
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut day = None;
        let mut boundary = original.boundary.clone();
        let budget = cold.settings.limits.max_bytes / 2;
        self.visit_range(original.floor, before, |key, value| {
            let k = decode_seq_key(key).context("invalid hot key")?;
            let d = super::super::storage::day_key(k.0);
            if day.is_some_and(|old| old != d) || rows.len() >= cold.settings.limits.max_records {
                return Ok(false);
            }
            let add = key.len() + value.len() + 32;
            if bytes + add > budget {
                ensure!(
                    !rows.is_empty(),
                    "oldest hot record exceeds cold byte budget; original retained"
                );
                return Ok(false);
            }
            let msg = codec::decode(value)?;
            boundary.ingest(&msg)?;
            day = Some(d);
            bytes += add;
            rows.push((k, value.to_vec()));
            Ok(true)
        })?;
        if rows.is_empty() {
            return Ok(None);
        }
        // No delayed factual enrichment or historical insertion may change this prefix.
        let to = successor(rows.last().unwrap().0)?;
        {
            let head = self.last_key.lock().unwrap();
            let mut current = self.retention.lock().unwrap();
            ensure!(
                current.floor == original.floor,
                "hot retention changed while selecting cold batch"
            );
            let mut next = current.clone();
            next.sealed = next.sealed.max(to);
            next.head = next.head.max(*head);
            self.db
                .db()
                .put_opt(retention::META, rmp_serde::to_vec(&next)?, &sync_options())?;
            *current = next;
        }
        // Freeze preceded this readback: an enrichment racing initial selection
        // must never leave an irreconcilable pending cold segment.
        let mut expected = Sha256::new();
        for (key, value) in &rows {
            hash_row(&mut expected, &encode_seq_key(key.0, key.1)?, value);
        }
        let mut frozen = Sha256::new();
        let mut frozen_count = 0;
        self.visit_range(original.floor, to, |key, value| {
            hash_row(&mut frozen, key, value);
            frozen_count += 1;
            Ok(true)
        })?;
        ensure!(
            frozen_count == rows.len() && expected.finalize() == frozen.finalize(),
            "selected prefix changed before sealing; originals retained, retry the frozen prefix"
        );
        let segment =
            cold.root
                .write_batch(&rows, &original.boundary, &boundary, cold.settings.limits)?;
        self.finish_cold_hot_commit(&segment)?;
        cold.root.confirm(&segment)?;
        Ok(Some(segment))
    }
    fn finish_cold_hot_commit(&self, s: &Segment) -> Result<()> {
        let state = self.retention.lock().unwrap().clone();
        let to = successor(s.last)?;
        if state.floor >= to {
            return Ok(());
        }
        ensure!(
            state.floor <= s.first
                && state.sealed >= to
                && rmp_serde::to_vec(&state.boundary)? == s.boundary_before,
            "pending cold source boundary mismatch"
        );
        let mut hash = Sha256::new();
        let mut count = 0;
        let mut bytes = 0;
        self.visit_range(state.floor, to, |key, value| {
            hash_row(&mut hash, key, value);
            count += 1;
            bytes += key.len() as u64 + value.len() as u64;
            Ok(true)
        })?;
        ensure!(
            count == s.count
                && bytes == s.encoded_bytes
                && hex::encode(hash.finalize()) == s.payload_sha256,
            "hot prefix changed; no originals removed"
        );
        let _gate = self.retention_gate.write().unwrap();
        let mut current = self.retention.lock().unwrap();
        ensure!(
            current.floor == state.floor,
            "hot prefix changed during cold confirmation"
        );
        let mut next = current.clone();
        next.floor = to;
        next.boundary = rmp_serde::from_slice(&s.boundary_after)?;
        let mut batch = WriteBatch::default();
        if self.legacy_json {
            let cf = self
                .db
                .db()
                .cf_handle(POSITION_UPDATES_CF)
                .context("missing legacy hot CF")?;
            batch.delete_range_cf(
                &cf,
                encode_seq_key(state.floor.0, state.floor.1)?,
                encode_seq_key(to.0, to.1)?,
            );
        } else {
            storage::Storage::delete_prefix(&self.db, &mut batch, state.floor, to)?;
        }
        let encoded = rmp_serde::to_vec(&next)?;
        batch.put(retention::META, &encoded);
        let result = self.db.db().write_opt(batch, &sync_options());
        if self.db.db().get(retention::META)?.as_deref() == Some(encoded.as_slice()) {
            *current = next;
            let mut writes = self.target_writes.lock().unwrap();
            writes.revision = writes
                .revision
                .checked_add(1)
                .context("archive revision exhausted")?;
            writes.retention_revision = writes.revision;
        }
        result?;
        self.storage.clear_cache();
        Ok(())
    }
}

#[derive(Clone)]
struct CpuSample {
    ticks: BTreeMap<u32, (u64, u64)>,
}
impl CpuSample {
    fn read() -> Result<Self> {
        let status = std::fs::read_to_string("/proc/self/status")?;
        let allowed = status
            .lines()
            .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
            .context("missing actual CPU affinity")?
            .trim();
        let mut ids = BTreeSet::new();
        for part in allowed.split(',') {
            let ends = part
                .split('-')
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            match ends.as_slice() {
                [n] => {
                    ids.insert(*n);
                }
                [a, b] => {
                    ensure!(b >= a && b - a < 100_000, "invalid affinity range");
                    ids.extend(*a..=*b);
                }
                _ => anyhow::bail!("invalid affinity list"),
            }
        }
        let mut ticks = BTreeMap::new();
        for line in std::fs::read_to_string("/proc/stat")?.lines() {
            let mut words = line.split_whitespace();
            let Some(label) = words.next() else { continue };
            let Some(id) = label
                .strip_prefix("cpu")
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if !ids.contains(&id) {
                continue;
            }
            let v = words
                .take(8)
                .map(str::parse::<u64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(v.len() >= 4, "incomplete CPU counters");
            ticks.insert(id, (v.iter().sum(), v[3]));
        }
        ensure!(ticks.len() == ids.len(), "incomplete CPU-affinity sample");
        Ok(Self { ticks })
    }
    fn busy_percent(&self, old: &Self) -> Option<f64> {
        let mut max: f64 = 0.;
        for (id, (total, idle)) in &self.ticks {
            let (ot, oi) = old.ticks.get(id)?;
            let d = total.checked_sub(*ot)?;
            if d == 0 {
                return None;
            };
            let di = idle.checked_sub(*oi)?;
            max = max.max(100. * (d.saturating_sub(di)) as f64 / d as f64);
        }
        Some(max)
    }
}
fn io_pressure() -> Result<f64> {
    let s = std::fs::read_to_string("/proc/pressure/io")?;
    let v = s
        .lines()
        .find(|l| l.starts_with("some "))
        .and_then(|l| l.split_whitespace().find_map(|v| v.strip_prefix("avg10=")))
        .context("I/O pressure unavailable")?
        .parse::<f64>()?;
    ensure!(
        v.is_finite() && (0.0..=100.0).contains(&v),
        "invalid I/O pressure"
    );
    Ok(v)
}
pub fn spawn(
    archive: Arc<PositionArchive>,
    backlog: impl Fn() -> (usize, bool) + Send + Sync + 'static,
) {
    let Some(state) = &archive.cold else { return };
    let settings = state.settings.clone();
    tokio::spawn(async move {
        let mut previous = CpuSample::read().ok();
        let mut quiet = None;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let current = CpuSample::read().ok();
            let cpu = current
                .as_ref()
                .zip(previous.as_ref())
                .and_then(|(a, b)| a.busy_percent(b));
            previous = current;
            let io = io_pressure().ok();
            let (pending, failed) = backlog();
            let idle = cpu.is_some_and(|v| v <= settings.max_cpu_percent)
                && io.is_some_and(|v| v <= settings.max_io_pressure_percent)
                && pending == 0
                && !failed;
            {
                let mut s = archive.cold.as_ref().unwrap().status.lock().unwrap();
                s.cpu_percent = cpu;
                s.io_pressure_percent = io;
                if !idle {
                    s.deferred_reason =
                        Some("CPU/I/O/publication backlog or telemetry unavailable".into());
                }
            }
            if !idle {
                quiet = None;
                continue;
            }
            let start = *quiet.get_or_insert_with(Instant::now);
            if start.elapsed() < Duration::from_secs(settings.quiet_secs) {
                continue;
            }
            if archive
                .cold
                .as_ref()
                .unwrap()
                .status
                .lock()
                .unwrap()
                .last_attempt_us
                .is_some_and(|last| {
                    crate::kline::now_us().saturating_sub(last)
                        < settings.interval_secs as i64 * 1_000_000
                })
            {
                continue;
            }
            let job = archive.clone();
            let (send, receive) = tokio::sync::oneshot::channel();
            if let Err(e) = std::thread::Builder::new()
                .name("cold-archive".into())
                .spawn(move || {
                    lower_priority();
                    let _ = send.send(job.archive_cold_once(crate::kline::now_us()));
                })
            {
                tracing::warn!(error=%e,"cold archive thread creation failed");
                continue;
            }
            match receive.await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error=%e,"cold archive attempt failed; originals retained until verified")
                }
                Err(e) => {
                    let mut s = archive.cold.as_ref().unwrap().status.lock().unwrap();
                    s.running = false;
                    s.last_error = Some("cold worker exited unexpectedly".into());
                    tracing::warn!(error=%e,"cold archive worker exited");
                }
            }
        }
    });
}

pub fn lower_priority() {
    if let Ok(path) = std::fs::read_link("/proc/thread-self") {
        if let Some(tid) = path.file_name() {
            let _ = std::process::Command::new("renice")
                .args(["-n", "19", "-p"])
                .arg(tid)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let _ = std::process::Command::new("ionice")
                .args(["-c", "3", "-p"])
                .arg(tid)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const DAY: i64 = 86_400_000_000;
    const NOW: i64 = 100 * DAY;
    fn setup(legacy: bool) -> (tempfile::TempDir, PositionArchive, Vec<(Cursor, Vec<u8>)>) {
        let temp = tempfile::tempdir().unwrap();
        let db = ManagerDb::open(&temp.path().join("hot")).unwrap();
        let (mut rows, _, _) = super::super::tests::records(10 * DAY, 5);
        rows.extend(super::super::tests::records(NOW - DAY, 1).0);
        if legacy {
            let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
            for (key, value) in &rows {
                db.db()
                    .put_cf(
                        &cf,
                        encode_seq_key(key.0, key.1).unwrap(),
                        serde_json::to_vec(&codec::decode(value).unwrap()).unwrap(),
                    )
                    .unwrap();
            }
        } else {
            let a = PositionArchive::open(db.clone()).unwrap();
            for (_, value) in &rows {
                let m = codec::decode(value).unwrap();
                a.append(m.received_at_us, &m.strategy, vec![], m.published_accounts)
                    .unwrap();
            }
        }
        db.db()
            .put(b"position_current:protected", b"current target")
            .unwrap();
        db.ensure_column_family("klines_1m").unwrap();
        db.db()
            .put_cf(
                &db.db().cf_handle("klines_1m").unwrap(),
                b"protected",
                b"candle",
            )
            .unwrap();
        let a = PositionArchive::open_with_cold(
            db,
            Some(Settings {
                root: temp.path().join("cold"),
                limits: BatchLimits {
                    max_records: 2,
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .unwrap();
        let mut originals = vec![];
        a.visit_range((1, 0), (NOW, 0), |k, v| {
            originals.push((decode_seq_key(k).unwrap(), v.to_vec()));
            Ok(true)
        })
        .unwrap();
        (temp, a, originals)
    }
    fn stage_pending(a: &PositionArchive, rows: &[(Cursor, Vec<u8>)]) -> Segment {
        let before = a.retention.lock().unwrap().boundary.clone();
        let mut after = before.clone();
        for (_, v) in rows {
            after.ingest(&codec::decode(v).unwrap()).unwrap();
        }
        let mut state = a.retention.lock().unwrap();
        state.sealed = successor(rows.last().unwrap().0).unwrap();
        a.db.db()
            .put_opt(
                retention::META,
                rmp_serde::to_vec(&*state).unwrap(),
                &sync_options(),
            )
            .unwrap();
        drop(state);
        a.cold
            .as_ref()
            .unwrap()
            .root
            .write_batch(rows, &before, &after, BatchLimits::default())
            .unwrap()
    }
    #[test]
    fn json_and_pb_archive_bounded_oldest_prefix_with_durable_hourly_cadence() {
        for legacy in [true, false] {
            let (temp, a, originals) = setup(legacy);
            assert_eq!(a.hot_format(), if legacy { "json" } else { "protobuf" });
            let s = a.archive_cold_once(NOW).unwrap().unwrap();
            assert_eq!(
                (s.first, s.last, s.count),
                (originals[0].0, originals[1].0, 2)
            );
            let mut cold = vec![];
            a.cold
                .as_ref()
                .unwrap()
                .root
                .visit(&s, |k, v| {
                    cold.push((k, v));
                    Ok(())
                })
                .unwrap();
            assert_eq!(cold, originals[..2]);
            let settings = a.cold.as_ref().unwrap().settings.clone();
            drop(a);
            let a = PositionArchive::open_with_cold(
                ManagerDb::open(&temp.path().join("hot")).unwrap(),
                Some(settings),
            )
            .unwrap();
            assert!(a.archive_cold_once(NOW + 3_599_000_000).unwrap().is_none());
            let next = a.archive_cold_once(NOW + 3_600_000_000).unwrap().unwrap();
            assert_eq!(
                (next.first, next.last, next.count),
                (originals[2].0, originals[3].0, 2)
            );
            let mut hot = vec![];
            a.visit_range(a.retention_floor(), (NOW, 0), |k, v| {
                hot.push((decode_seq_key(k).unwrap(), v.to_vec()));
                Ok(true)
            })
            .unwrap();
            assert_eq!(hot, originals[4..]);
            assert_eq!(
                a.db.db()
                    .get(b"position_current:protected")
                    .unwrap()
                    .unwrap(),
                b"current target"
            );
            assert_eq!(
                a.db.db()
                    .get_cf(&a.db.db().cf_handle("klines_1m").unwrap(), b"protected")
                    .unwrap()
                    .unwrap(),
                b"candle"
            );
            // New hot writes and the target-only reader remain operational.
            let mut m = codec::decode(&originals.last().unwrap().1).unwrap();
            m.strategy.targets.get_mut("BTCUSDT").unwrap().qty = 0.12345678901234567;
            let expected_json = serde_json::to_vec(&PositionUpdateMsg::from_strategy(
                NOW,
                0,
                m.strategy.clone(),
                vec![],
                m.published_accounts.clone(),
            ))
            .unwrap();
            a.append(NOW, &m.strategy, vec![], m.published_accounts)
                .unwrap();
            if legacy {
                assert_eq!(
                    a.db.db()
                        .get_cf(
                            &a.db.db().cf_handle(POSITION_UPDATES_CF).unwrap(),
                            encode_seq_key(NOW, 0).unwrap()
                        )
                        .unwrap()
                        .unwrap(),
                    expected_json
                );
            }
            let mut count = 0;
            a.visit_target_updates_through(NOW, |_| {
                count += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(count, 3);
        }
    }
    #[test]
    fn pending_cold_is_reverified_before_hot_deletion_and_recovered_after_restart() {
        for hot_committed in [false, true] {
            let (temp, a, originals) = setup(true);
            let s = stage_pending(&a, &originals[..2]);
            if hot_committed {
                a.finish_cold_hot_commit(&s).unwrap();
            }
            let settings = a.cold.as_ref().unwrap().settings.clone();
            drop(a);
            let a = PositionArchive::open_with_cold(
                ManagerDb::open(&temp.path().join("hot")).unwrap(),
                Some(settings),
            )
            .unwrap();
            if !hot_committed {
                let marker = a
                    .cold
                    .as_ref()
                    .unwrap()
                    .root
                    .path
                    .join(&s.id)
                    .join(SEGMENT_MARKER);
                let original = std::fs::read(&marker).unwrap();
                let mut bad = s.clone();
                bad.count += 1;
                durable_json(&marker, &bad).unwrap();
                assert!(a.archive_cold_once(NOW).is_err());
                assert_eq!(a.scan_from(0).unwrap().len(), originals.len());
                std::fs::write(&marker, original).unwrap();
            }
            let now = NOW + if hot_committed { 0 } else { 3_600_000_000 };
            assert_eq!(a.archive_cold_once(now).unwrap(), Some(s.clone()));
            let mut retained = 0;
            a.visit_range(a.retention_floor(), (NOW, 0), |_, _| {
                retained += 1;
                Ok(true)
            })
            .unwrap();
            assert_eq!(retained, originals.len() - 2);
            assert_eq!(
                a.cold.as_ref().unwrap().root.list(now, true).unwrap(),
                vec![s]
            );
        }
    }
    #[test]
    fn blocked_retention_reader_does_not_block_new_target_append() {
        let (_temp, a, originals) = setup(false);
        let a = Arc::new(a);
        let guard = a.retention_read();
        let worker = a.clone();
        let handle = std::thread::spawn(move || worker.archive_cold_once(NOW));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(a.cold.as_ref().unwrap().root.pending(), Ok(Some(_))) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let m = codec::decode(&originals.last().unwrap().1).unwrap();
        a.append(NOW, &m.strategy, vec![], m.published_accounts)
            .unwrap();
        // Reuse the guard while a writer is pending; no nested read acquisition.
        a.visit_target_updates_guarded(&guard, None, NOW, |_| Ok(()))
            .unwrap();
        drop(guard);
        assert!(handle.join().unwrap().unwrap().is_some());
    }
}
