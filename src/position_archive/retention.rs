//! Owner-only, receipt-gated retention of Manager position signals.
use super::*;
use crate::theoretical_nav::{LatestTargets, target_deltas, target_positions_changed};
use anyhow::ensure;
use flate2::{Compression, bufread::GzDecoder, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, RwLockReadGuard};

const META: &str = "position_archive_retention";
const MAGIC: &[u8; 16] = b"CTA-SIGNAL-GZIP\0";
#[cfg(not(test))]
const CHUNK_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(test)]
const CHUNK_BYTES: u64 = 16 * 1024;
const MAX_VALUE: u64 = 64 * 1024 * 1024;
const MAX_HEADER: usize = 64 * 1024 * 1024;
pub type Cursor = (i64, u32);

pub fn predecessor(cursor: Cursor) -> Option<Cursor> {
    if cursor.1 > 0 {
        Some((cursor.0, cursor.1 - 1))
    } else if cursor.0 > 1 {
        Some((cursor.0 - 1, u32::MAX))
    } else {
        None
    }
}
fn successor(cursor: Cursor) -> Result<Cursor> {
    Ok(if cursor.1 == u32::MAX {
        (cursor.0.checked_add(1).context("cursor overflow")?, 0)
    } else {
        (cursor.0, cursor.1 + 1)
    })
}
pub fn cutoff(now: i64) -> Cursor {
    const DAY: i64 = 86_400_000_000;
    (
        ((now - 30 * DAY).div_euclid(DAY) * DAY - 600_000_000).max(1),
        0,
    )
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Boundary {
    pub(crate) latest: BTreeMap<(String, String), LatestTargets>,
    pub(crate) cumulative: BTreeMap<(String, String), f64>,
}
impl Boundary {
    fn ingest(&mut self, message: &PositionUpdateMsg) -> Result<()> {
        for account in &message.published_accounts {
            let key = (account.source_id.clone(), account.binding_name.clone());
            let next = LatestTargets {
                position_strategy_name: message.strategy.strategy_name.clone(),
                // Source venue is constant for each historical source identity and
                // is resolved from configuration when the checkpoint is loaded.
                venue: String::new(),
                targets: super::normalized_account_targets(message, account)?,
                received_at_us: message.received_at_us,
                update_seq: message.seq,
            };
            if target_positions_changed(self.latest.get(&key), &next) {
                for (symbol, quantity) in target_deltas(self.latest.get(&key), &next) {
                    let total = self
                        .cumulative
                        .entry((account.source_id.clone(), symbol))
                        .or_default();
                    *total += quantity;
                    ensure!(total.is_finite(), "checkpoint quantity overflow");
                }
            }
            self.latest.insert(key, next);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub identity: String,
    pub from: Cursor,
    pub to: Cursor,
    pub previous: Option<String>,
    pub count: u64,
    pub raw_bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct RetentionState {
    identity: String,
    pub(super) head: Cursor,
    pub(super) sealed: Cursor,
    floor: Cursor,
    boundary: Boundary,
    receipt: Option<Receipt>,
}
impl RetentionState {
    pub(super) fn load(db: &ManagerDb) -> Result<Self> {
        let identity = std::fs::read_to_string(db.path().join("IDENTITY"))?
            .trim()
            .to_string();
        ensure!(!identity.is_empty(), "missing Manager store identity");
        if let Some(bytes) = db.db().get(META)? {
            let state: Self = rmp_serde::from_slice(&bytes)?;
            ensure!(
                state.identity == identity && state.floor <= state.sealed,
                "archive identity/range mismatch"
            );
            Ok(state)
        } else {
            Ok(Self {
                identity,
                head: (0, 0),
                sealed: (1, 0),
                floor: (1, 0),
                boundary: Boundary::default(),
                receipt: None,
            })
        }
    }
    fn header(&self, to: Cursor) -> Header {
        Header {
            identity: self.identity.clone(),
            from: self.floor,
            to,
            previous: self.receipt.as_ref().map(|r| r.sha256.clone()),
            boundary: self.boundary.clone(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Header {
    identity: String,
    from: Cursor,
    to: Cursor,
    previous: Option<String>,
    boundary: Boundary,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub identity: String,
    pub floor: Cursor,
    pub sealed: Cursor,
    pub eligible_before: Cursor,
    pub last_receipt: Option<Receipt>,
}

fn sync_options() -> rocksdb::WriteOptions {
    let mut options = rocksdb::WriteOptions::default();
    options.set_sync(true);
    options
}
fn encode_header(header: &Header) -> Result<Vec<u8>> {
    let bytes = rmp_serde::to_vec(header)?;
    ensure!(bytes.len() <= MAX_HEADER, "checkpoint header too large");
    Ok(bytes)
}
fn write_blob(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(&(bytes.len() as u64).to_be_bytes())?;
    writer.write_all(bytes)?;
    Ok(())
}
fn read_blob(reader: &mut impl Read, max: usize) -> Result<Vec<u8>> {
    let mut length = [0; 8];
    reader.read_exact(&mut length)?;
    let length = u64::from_be_bytes(length);
    ensure!(length <= max as u64, "archive frame exceeds size limit");
    let mut bytes = vec![0; length as usize];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn hash_frame(hash: &mut Sha256, key: &[u8], value: &[u8]) {
    hash.update(key);
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}
fn validate_message(key: &[u8], value: &[u8]) -> Result<PositionUpdateMsg> {
    let cursor = manager_db::decode_seq_key(key).context("invalid archived key")?;
    let message = decode_msg(value)?;
    ensure!(
        cursor.0 > 0
            && (message.received_at_us, message.seq) == cursor
            && message.msg_type == MSG_TYPE,
        "archive key/message identity mismatch"
    );
    Ok(message)
}

impl PositionArchive {
    pub fn retention_read(&self) -> RwLockReadGuard<'_, ()> {
        self.retention_gate.read().unwrap()
    }
    pub fn boundary(&self) -> (Cursor, Boundary) {
        let state = self.retention.lock().unwrap();
        (state.floor, state.boundary.clone())
    }
    pub fn retention_floor(&self) -> Cursor {
        self.retention.lock().unwrap().floor
    }
    pub fn retention_status(&self) -> Status {
        let state = self.retention.lock().unwrap();
        Status {
            identity: state.identity.clone(),
            floor: state.floor,
            sealed: state.sealed,
            eligible_before: cutoff(crate::kline::now_us()),
            last_receipt: state.receipt.clone(),
        }
    }
    fn seal(&self, now: i64) -> Result<RetentionState> {
        ensure!(
            !self.db.column_families()?.iter().any(|name| matches!(
                name.as_str(),
                "uniform_orders" | "order_updates" | "trade_updates"
            )),
            "refusing archive maintenance on an Exec store"
        );
        let head = self.last_key.lock().unwrap();
        let mut state = self.retention.lock().unwrap();
        let mut next = state.clone();
        next.sealed = next.sealed.max(cutoff(now));
        next.head = next.head.max(*head);
        self.db
            .db()
            .put_opt(META, rmp_serde::to_vec(&next)?, &sync_options())?;
        *state = next.clone();
        Ok(next)
    }
    fn visit_range(
        &self,
        from: Cursor,
        to: Cursor,
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<bool>,
    ) -> Result<()> {
        let cf = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("missing Manager archive CF")?;
        let start = manager_db::encode_seq_key(from.0, from.1)?;
        let mut options = rocksdb::ReadOptions::default();
        options.fill_cache(false);
        options.set_iterate_upper_bound(manager_db::encode_seq_key(to.0, to.1)?);
        for row in self.db.db().iterator_cf_opt(
            &cf,
            options,
            IteratorMode::From(&start, Direction::Forward),
        ) {
            let (key, value) = row?;
            let cursor = manager_db::decode_seq_key(&key).context("invalid archive key")?;
            ensure!(
                cursor >= from && cursor < to,
                "archive cursor outside range"
            );
            ensure!(
                !value.is_empty() && value.len() as u64 <= MAX_VALUE,
                "invalid archive record size"
            );
            if !visit(&key, &value)? {
                break;
            }
        }
        Ok(())
    }
    /// Returns false once the currently eligible prefix has been fully offloaded.
    pub fn export_chunk(&self, output: &mut impl Write, now: i64) -> Result<bool> {
        let state = self.seal(now)?;
        if state.floor >= state.sealed {
            return Ok(false);
        }
        let mut size = 0;
        let mut to = state.sealed;
        self.visit_range(state.floor, state.sealed, |key, value| {
            size += 20 + value.len() as u64;
            if size >= CHUNK_BYTES {
                to = successor(manager_db::decode_seq_key(key).unwrap())?;
                return Ok(false);
            }
            Ok(true)
        })?;
        let header = state.header(to);
        let bytes = encode_header(&header)?;
        let mut hash = Sha256::new();
        hash.update(&bytes);
        let mut writer = GzEncoder::new(output, Compression::fast());
        writer.write_all(MAGIC)?;
        write_blob(&mut writer, &bytes)?;
        let mut count = 0;
        let mut raw_bytes = 0;
        self.visit_range(header.from, header.to, |key, value| {
            validate_message(key, value)?;
            writer.write_all(key)?;
            write_blob(&mut writer, value)?;
            hash_frame(&mut hash, key, value);
            count += 1;
            raw_bytes += 20 + value.len() as u64;
            Ok(true)
        })?;
        writer.write_all(&[0; 12])?;
        write_blob(&mut writer, &[])?;
        let receipt = Receipt {
            identity: header.identity,
            from: header.from,
            to: header.to,
            previous: header.previous,
            count,
            raw_bytes,
            sha256: hex::encode(hash.finalize()),
        };
        write_blob(&mut writer, &serde_json::to_vec(&receipt)?)?;
        writer.finish()?.flush()?;
        Ok(true)
    }
    pub fn commit_receipt(&self, receipt: &Receipt, now: i64) -> Result<()> {
        let state = self.retention.lock().unwrap().clone();
        ensure!(
            receipt.identity == state.identity,
            "receipt belongs to another Manager store"
        );
        if state.receipt.as_ref() == Some(receipt) {
            self.db.db().flush_wal(true)?;
            return Ok(());
        }
        ensure!(
            receipt.from == state.floor
                && receipt.from < receipt.to
                && receipt.to <= state.sealed
                && receipt.to <= cutoff(now),
            "stale/unsafe archive receipt range"
        );
        ensure!(
            receipt.previous == state.receipt.as_ref().map(|r| r.sha256.clone()),
            "receipt chain mismatch"
        );
        let mut hash = Sha256::new();
        hash.update(encode_header(&state.header(receipt.to))?);
        let mut count = 0;
        let mut raw_bytes = 0;
        let mut boundary = state.boundary.clone();
        self.visit_range(receipt.from, receipt.to, |key, value| {
            boundary.ingest(&validate_message(key, value)?)?;
            hash_frame(&mut hash, key, value);
            count += 1;
            raw_bytes += 20 + value.len() as u64;
            Ok(true)
        })?;
        ensure!(
            count == receipt.count
                && raw_bytes == receipt.raw_bytes
                && hex::encode(hash.finalize()) == receipt.sha256,
            "receipt digest/count mismatch; no records removed"
        );
        // Heavy scanning/hash/replay is complete before acquiring these short locks.
        let _gate = self.retention_gate.write().unwrap();
        let mut current = self.retention.lock().unwrap();
        ensure!(
            current.floor == state.floor && current.receipt == state.receipt,
            "archive prefix changed; retry"
        );
        let mut next = current.clone();
        next.floor = receipt.to;
        next.boundary = boundary;
        next.receipt = Some(receipt.clone());
        let cf = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("missing archive CF")?;
        let mut batch = rocksdb::WriteBatch::default();
        batch.delete_range_cf(
            &cf,
            manager_db::encode_seq_key(receipt.from.0, receipt.from.1)?,
            manager_db::encode_seq_key(receipt.to.0, receipt.to.1)?,
        );
        batch.put(META, rmp_serde::to_vec(&next)?);
        let result = self.db.db().write_opt(batch, &sync_options());
        // A WAL sync error can follow a visible atomic commit. Keep RAM aligned
        // with the database, report the error, and require a durable retry.
        if self.db.db().get(META)?.as_deref() == Some(rmp_serde::to_vec(&next)?.as_slice()) {
            *current = next;
            let mut writes = self.target_writes.lock().unwrap();
            writes.revision = writes
                .revision
                .checked_add(1)
                .context("revision exhausted")?;
            writes.retention_revision = writes.revision;
        }
        result.context("archive retention WAL sync")?;
        Ok(())
    }
    pub fn compact_archived(&self) -> Result<()> {
        let floor = self.retention_floor();
        if floor == (1, 0) {
            return Ok(());
        }
        let cf = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("missing archive CF")?;
        let mut options = rocksdb::CompactOptions::default();
        options.set_exclusive_manual_compaction(false);
        options.set_bottommost_level_compaction(rocksdb::BottommostLevelCompaction::ForceOptimized);
        self.db.db().compact_range_cf_opt(
            &cf,
            Some(manager_db::encode_seq_key(1, 0)?),
            Some(manager_db::encode_seq_key(floor.0, floor.1)?),
            &options,
        );
        Ok(())
    }
}

/// Restore exact original bytes into a local, dedicated backup store and read
/// them back before issuing a receipt. No Exec store is accepted as a destination.
pub fn verify_restore(input: impl Read, destination: &Path) -> Result<Receipt> {
    ensure!(
        !destination.exists() || destination.join("POSITION_ARCHIVE_BACKUP").is_file(),
        "restore destination is not an archive backup store"
    );
    std::fs::create_dir_all(destination)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    let mut reader = GzDecoder::new(std::io::BufReader::new(input));
    let mut magic = [0; 16];
    reader.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "invalid archive magic");
    let bytes = read_blob(&mut reader, MAX_HEADER)?;
    let header: Header = rmp_serde::from_slice(&bytes)?;
    ensure!(
        header.from >= (1, 0) && header.from < header.to && !header.identity.is_empty(),
        "invalid archive header"
    );
    let marker = destination.join("POSITION_ARCHIVE_BACKUP");
    if marker.exists() {
        ensure!(
            std::fs::read_to_string(&marker)? == header.identity,
            "backup database identity mismatch"
        );
    } else {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)?;
        file.write_all(header.identity.as_bytes())?;
        file.sync_all()?;
    }
    let db = ManagerDb::open(destination)?;
    let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    if let Some(previous) = &header.previous {
        let saved = db
            .db()
            .get(format!("archive_receipt:{previous}"))?
            .context("preceding verified backup receipt is missing")?;
        let saved: Receipt = serde_json::from_slice(&saved)?;
        ensure!(
            saved.identity == header.identity
                && saved.to == header.from
                && saved.sha256 == *previous,
            "backup receipt chain/range mismatch"
        );
    } else {
        ensure!(
            header.from == (1, 0),
            "initial backup must begin at the original archive boundary"
        );
    }
    let mut hash = Sha256::new();
    hash.update(&bytes);
    let mut previous = None;
    let mut count = 0;
    let mut raw_bytes = 0;
    loop {
        let mut key = [0; 12];
        reader.read_exact(&mut key)?;
        let value = read_blob(&mut reader, MAX_VALUE as usize)?;
        if key == [0; 12] {
            ensure!(value.is_empty(), "invalid archive terminator");
            break;
        }
        let cursor = manager_db::decode_seq_key(&key).context("invalid archive key")?;
        ensure!(
            cursor >= header.from && cursor < header.to && previous.is_none_or(|p| cursor > p),
            "unordered archive records"
        );
        validate_message(&key, &value)?;
        if let Some(existing) = db.db().get_cf(&cf, key)? {
            ensure!(existing == value, "conflicting backup record");
        } else {
            db.db().put_cf(&cf, key, &value)?;
        }
        hash_frame(&mut hash, &key, &value);
        previous = Some(cursor);
        count += 1;
        raw_bytes += 20 + value.len() as u64;
    }
    let receipt: Receipt = serde_json::from_slice(&read_blob(&mut reader, 64 * 1024)?)?;
    let mut extra = [0; 1];
    ensure!(reader.read(&mut extra)? == 0, "trailing archive payload");
    // GzDecoder stops after one member. Reject any trailing compressed bytes too.
    ensure!(
        reader.into_inner().read(&mut extra)? == 0,
        "trailing gzip data"
    );
    let expected = Receipt {
        identity: header.identity,
        from: header.from,
        to: header.to,
        previous: header.previous,
        count,
        raw_bytes,
        sha256: hex::encode(hash.finalize()),
    };
    ensure!(receipt == expected, "archive checksum/count mismatch");
    db.db().put(
        format!("archive_boundary:{}", hex::encode(Sha256::digest(&bytes))),
        &bytes,
    )?;
    db.db().flush_wal(true)?;
    let mut readback = Sha256::new();
    readback.update(&bytes);
    let mut read_count = 0;
    let start = manager_db::encode_seq_key(header.from.0, header.from.1)?;
    for row in db
        .db()
        .iterator_cf(&cf, IteratorMode::From(&start, Direction::Forward))
    {
        let (key, value) = row?;
        if manager_db::decode_seq_key(&key).context("invalid backup key")? >= header.to {
            break;
        }
        hash_frame(&mut readback, &key, &value);
        read_count += 1;
    }
    ensure!(
        hex::encode(readback.finalize()) == receipt.sha256 && read_count == count,
        "restored backup readback mismatch"
    );
    db.db().put_opt(
        format!("archive_receipt:{}", receipt.sha256),
        serde_json::to_vec(&receipt)?,
        &sync_options(),
    )?;
    std::fs::File::open(destination)?.sync_all()?;
    Ok(receipt)
}

mod control;
pub use control::{Command, Reply, client, start_socket};

#[cfg(test)]
mod tests;
