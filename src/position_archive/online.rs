//! Explicit, resumable JSON replacement in the Manager-owned hot store.
//! No database/file-format guessing outside a durable conversion transition.
use super::*;
use anyhow::ensure;
use std::time::Instant;

pub const META: &[u8] = b"position_storage_conversion";

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub max_records: usize,
    pub max_bytes: usize,
    pub max_work_millis: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_records: 1000,
            max_bytes: 8 * 1024 * 1024,
            max_work_millis: 50,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=10000).contains(&self.max_records)
                && (8192..=64 * 1024 * 1024).contains(&self.max_bytes)
                && (1..=1000).contains(&self.max_work_millis),
            "invalid hot conversion limits"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub identity: String,
    pub original_head: (i64, u32),
    pub cursor: Option<(i64, u32)>,
    pub converted_records: u64,
    pub scanned_records: u64,
    pub completed: bool,
    pub last_batch_records: usize,
    pub last_batch_bytes: usize,
}
pub(super) fn load(db: &ManagerDb) -> Result<Option<Progress>> {
    inspect_progress(
        db.db().get(META)?.as_deref(),
        db.db().get(storage::FORMAT_KEY)?.as_deref(),
        std::fs::read_to_string(db.path().join("IDENTITY"))?.trim(),
    )
}

/// The same marker validation for read-only diagnostic owners.
pub fn inspect_progress(
    bytes: Option<&[u8]>,
    format: Option<&[u8]>,
    identity: &str,
) -> Result<Option<Progress>> {
    let Some(bytes) = bytes else { return Ok(None) };
    ensure!(bytes.len() <= 8192, "invalid hot conversion metadata size");
    let progress: Progress = serde_json::from_slice(bytes)?;
    ensure!(
        progress.identity == identity
            && progress.original_head.0 > 0
            && progress
                .cursor
                .is_none_or(|c| c.0 > 0 && c <= progress.original_head)
            && progress.converted_records <= progress.scanned_records,
        "invalid hot conversion identity/progress"
    );
    ensure!(
        format == Some(storage::FORMAT),
        "hot conversion requires the PB destination marker"
    );
    Ok(Some(progress))
}

pub(super) fn start(db: &ManagerDb) -> Result<()> {
    ensure!(
        db.db().get(storage::MIGRATING_KEY)?.is_none(),
        "cannot online-convert an interrupted offline migration"
    );
    ensure!(
        db.db().get(storage::FORMAT_KEY)?.is_none(),
        "unexpected existing hot format"
    );
    let upper = latest_key(db)?.context("cannot convert an empty JSON archive")?;
    let cf = db
        .db()
        .cf_handle(POSITION_UPDATES_CF)
        .context("missing target CF")?;
    for mode in [IteratorMode::Start, IteratorMode::End] {
        let (key, value) = db
            .db()
            .iterator_cf(&cf, mode)
            .next()
            .transpose()?
            .context("missing JSON row")?;
        migration::decode_json(&key, &value)?;
    }
    storage::families(db)?;
    let progress = Progress {
        identity: std::fs::read_to_string(db.path().join("IDENTITY"))?
            .trim()
            .into(),
        original_head: upper,
        cursor: None,
        converted_records: 0,
        scanned_records: 0,
        completed: false,
        last_batch_records: 0,
        last_batch_bytes: 0,
    };
    let mut batch = rocksdb::WriteBatch::default();
    batch.put(storage::FORMAT_KEY, storage::FORMAT);
    batch.put(META, serde_json::to_vec(&progress)?);
    db.db().write_opt(batch, &sync())?;
    Ok(())
}

pub(super) fn is_json(bytes: &[u8]) -> bool {
    // Original Manager JSON is serialized without leading whitespace. PB starts
    // with tag 0x0a (also ASCII LF); never trim whitespace from binary bytes.
    bytes.first() == Some(&b'{')
}
fn sync() -> rocksdb::WriteOptions {
    let mut options = rocksdb::WriteOptions::default();
    options.set_sync(true);
    options
}

#[derive(Default)]
struct Operations {
    values: BTreeMap<(u32, Vec<u8>), Option<Vec<u8>>>,
    invalid: bool,
}
impl rocksdb::WriteBatchIteratorCf for Operations {
    fn put_cf(&mut self, cf: u32, key: &[u8], value: &[u8]) {
        self.values.insert((cf, key.to_vec()), Some(value.to_vec()));
    }
    fn delete_cf(&mut self, cf: u32, key: &[u8]) {
        self.values.insert((cf, key.to_vec()), None);
    }
    fn merge_cf(&mut self, _: u32, _: &[u8], _: &[u8]) {
        self.invalid = true;
    }
}
fn families(db: &ManagerDb) -> Result<BTreeMap<u32, String>> {
    // Obtain stable CF IDs through an unsubmitted probe; no FFI or DB writes.
    let mut probe = rocksdb::WriteBatch::default();
    for name in [
        POSITION_UPDATES_CF,
        storage::OBSERVATIONS_CF,
        storage::SNAPSHOTS_CF,
    ] {
        probe.put_cf(
            &db.db().cf_handle(name).context("missing conversion CF")?,
            name,
            b"",
        );
    }
    let mut ids = Operations::default();
    probe.iterate_cf(&mut ids);
    ids.values
        .into_keys()
        .map(|(id, name)| Ok((id, String::from_utf8(name)?)))
        .collect()
}

impl PositionArchive {
    pub fn hot_conversion_status(&self) -> Result<Option<Progress>> {
        load(&self.db)
    }

    /// One bounded atomic batch; no flush/compaction and no network/SQL work.
    pub(super) fn convert_hot_once(&self, now: i64) -> Result<Option<Progress>> {
        self.convert_hot_with(now, || Ok(()))
    }

    fn convert_hot_with(
        &self,
        now: i64,
        after_commit: impl FnOnce() -> Result<()>,
    ) -> Result<Option<Progress>> {
        let Some(settings) = self
            .cold
            .as_ref()
            .and_then(|c| c.settings.hot_conversion.as_ref())
        else {
            return Ok(None);
        };
        let Some(mut progress) = load(&self.db)?.filter(|p| !p.completed) else {
            return Ok(None);
        };
        if self
            .db
            .db()
            .property_int_value("rocksdb.num-running-compactions")?
            != Some(0)
        {
            return Ok(None);
        }
        let cold = self.cold.as_ref().unwrap();
        cold::space::require(
            self.db.path(),
            cold.settings.min_free_bytes,
            (settings.max_bytes as u64).saturating_mul(4),
        )?;
        // Never wait behind an interactive scan or the normal archive writer.
        let Ok(_readers) = self.retention_gate.try_write() else {
            return Ok(None);
        };
        let Ok(_writer) = self.last_key.try_lock() else {
            return Ok(None);
        };
        let started = Instant::now();
        let from = progress
            .cursor
            .unwrap_or((1, 0))
            .max(self.retention_floor());
        let cf = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("missing conversion target CF")?;
        let mut read = rocksdb::ReadOptions::default();
        read.fill_cache(false);
        let ids = families(&self.db)?;
        let mut operations = Operations::default();
        let mut expected = Vec::new();
        let mut input_bytes = 0usize;
        let mut bytes = 4096usize; // Reserve more than the bounded progress record.
        let mut scanned = 0usize;
        let mut reached_end = true;
        for row in self.db.db().iterator_cf_opt(
            &cf,
            read,
            IteratorMode::From(
                &manager_db::encode_seq_key(from.0, from.1)?,
                Direction::Forward,
            ),
        ) {
            let (key, value) = row?;
            let cursor = manager_db::decode_seq_key(&key).context("invalid conversion key")?;
            if progress.cursor.is_some_and(|old| cursor <= old) {
                continue;
            }
            if cursor > progress.original_head {
                break;
            }
            if scanned >= settings.max_records
                || (scanned > 0
                    && started.elapsed().as_millis() >= u128::from(settings.max_work_millis))
            {
                reached_end = false;
                break;
            }
            if is_json(&value) {
                // Expired JSON belongs in cold, not in a second hot rewrite.
                if cursor < retention::cutoff(now) {
                    reached_end = false;
                    break;
                }
                ensure!(
                    value.len() <= settings.max_bytes,
                    "oldest JSON exceeds conversion decode budget; original retained"
                );
                if input_bytes.saturating_add(value.len()) > settings.max_bytes {
                    reached_end = false;
                    break;
                }
                let message = migration::decode_json(&key, &value)?;
                let mut row_batch = rocksdb::WriteBatch::default();
                let _staged = self
                    .storage
                    .stage(&self.db, &message, &mut row_batch, true)?;
                let added = row_batch.size_in_bytes();
                if bytes.saturating_add(added) > settings.max_bytes {
                    ensure!(
                        scanned > 0,
                        "oldest JSON exceeds hot conversion byte limit; original retained"
                    );
                    reached_end = false;
                    break;
                }
                bytes += added;
                input_bytes += value.len();
                row_batch.iterate_cf(&mut operations);
                expected.push((key.to_vec(), message));
            } else {
                ensure!(
                    value.len() <= settings.max_bytes,
                    "PB scan record exceeds conversion decode budget"
                );
                let target = codec::decode(&value)?;
                ensure!(
                    (target.received_at_us, target.seq) == cursor
                        && target.factual_positions.is_empty(),
                    "invalid PB target during conversion"
                );
            }
            progress.cursor = Some(cursor);
            scanned += 1;
        }
        if scanned == 0 && !reached_end {
            return Ok(None);
        }
        ensure!(!operations.invalid, "unexpected merge in hot conversion");
        let get = |name: &str, key: &[u8]| -> Result<Option<Vec<u8>>> {
            let id = ids
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(id, _)| *id)
                .context("unknown conversion CF")?;
            if let Some(value) = operations.values.get(&(id, key.to_vec())) {
                return Ok(value.clone());
            }
            Ok(self.db.db().get_cf(
                &self
                    .db
                    .db()
                    .cf_handle(name)
                    .context("missing conversion CF")?,
                key,
            )?)
        };
        // Fresh independent reader checks the exact bytes that will be submitted.
        let verifier = storage::Storage::default();
        for (key, original) in &expected {
            let target = get(POSITION_UPDATES_CF, key)?.context("missing staged PB target")?;
            let decoded = verifier.complete_with(&get, key, &target)?;
            ensure!(
                rmp_serde::to_vec(original)? == rmp_serde::to_vec(&decoded)?,
                "hot conversion changed typed history; originals retained"
            );
        }
        let mut batch = rocksdb::WriteBatch::default();
        for ((id, key), value) in operations.values {
            let name = ids.get(&id).context("unexpected conversion CF ID")?;
            let handle = self
                .db
                .db()
                .cf_handle(name)
                .context("missing conversion CF")?;
            match value {
                Some(value) => batch.put_cf(&handle, key, value),
                None => batch.delete_cf(&handle, key),
            }
        }
        progress.scanned_records = progress
            .scanned_records
            .checked_add(scanned as u64)
            .context("conversion count overflow")?;
        progress.converted_records = progress
            .converted_records
            .checked_add(expected.len() as u64)
            .context("conversion count overflow")?;
        progress.completed = reached_end;
        progress.last_batch_records = expected.len();
        progress.last_batch_bytes = batch.size_in_bytes();
        batch.put(META, serde_json::to_vec(&progress)?);
        ensure!(
            batch.size_in_bytes() <= settings.max_bytes,
            "hot conversion batch exceeds byte cap"
        );
        let result = self.db.db().write_opt(batch, &sync());
        self.storage.clear_cache();
        result?;
        after_commit()?;
        // No logical target revision: this only changes its physical encoding.
        Ok(Some(progress))
    }
}

#[cfg(test)]
mod tests;
