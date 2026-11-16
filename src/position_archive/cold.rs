//! Closed, bounded cold segments. No process keeps a writable cold DB cached.
use super::{PositionUpdateMsg, codec, retention};
use crate::manager_db::{decode_seq_key, encode_seq_key};
use anyhow::{Context, Result, ensure};
use rocksdb::{DB, DBCompressionType, IteratorMode, Options, WriteBatch, WriteOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub mod manager;
pub mod package;
pub mod pull;
pub mod reclaim;
pub mod rehearsal;
pub mod space;
pub mod wire;

pub type Cursor = (i64, u32);
const ROWS: &str = "position_updates";
const META: &[u8] = b"position_cold_segment";
const ROOT_MARKER: &str = "COLD_ARCHIVE.json";
const SEGMENT_MARKER: &str = "COLD_SEGMENT.json";
const READY: &str = "READY";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Segment {
    pub source_identity: String,
    pub id: String,
    pub first: Cursor,
    pub last: Cursor,
    pub count: u64,
    pub encoded_bytes: u64,
    pub payload_sha256: String,
    /// Exact MessagePack retention boundary; part of the verified segment proof.
    pub boundary_before: Vec<u8>,
    pub boundary_after: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatchLimits {
    pub max_records: usize,
    pub max_bytes: usize,
}
impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            max_records: 1_000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}
impl BatchLimits {
    pub fn validate(self) -> Result<()> {
        ensure!(
            (1..=1_000_000).contains(&self.max_records),
            "invalid cold batch record limit"
        );
        ensure!(
            (4096..=256 * 1024 * 1024).contains(&self.max_bytes),
            "invalid cold batch byte limit"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ColdRoot {
    path: PathBuf,
    identity: String,
}

fn options(create: bool) -> Options {
    let mut o = Options::default();
    o.create_if_missing(create);
    o.create_missing_column_families(create);
    o.set_max_open_files(128);
    o.set_compression_type(DBCompressionType::Lz4);
    o
}
fn sync_options() -> WriteOptions {
    let mut o = WriteOptions::default();
    o.set_sync(true);
    o
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn durable_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("missing metadata parent")?;
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut f, data)?;
    f.write_all(b"\n")?;
    f.as_file().sync_all()?;
    f.persist(path).map_err(|e| e.error)?;
    sync_dir(parent)
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    ensure!(path.is_absolute(), "cold/archive path must be absolute");
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.mode() & 0o077 == 0,
        "cold/archive directory must be private (0700)"
    );
    Ok(())
}
pub(crate) struct ArchiveLock(File);
impl Drop for ArchiveLock {
    fn drop(&mut self) {
        // Explicitly unlock the open-file description before close: a concurrent
        // child spawn may briefly inherit CLOEXEC descriptors before exec.
        let _ = self.0.unlock();
    }
}
pub(crate) fn lock(path: &Path, shared: bool) -> Result<ArchiveLock> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    if shared {
        f.try_lock_shared().context("archive is busy")?;
    } else {
        f.try_lock().context("archive is busy")?;
    }
    Ok(ArchiveLock(f))
}
fn json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "metadata must be a regular file"
    );
    ensure!(
        path.metadata()?.len() <= MAX_MANIFEST_BYTES,
        "metadata exceeds limit"
    );
    Ok(serde_json::from_reader(File::open(path)?)?)
}
pub(crate) fn leaf(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 160
            && !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid archive filename"
    );
    ensure!(
        matches!(
            Path::new(value).components().next(),
            Some(Component::Normal(_))
        ) && Path::new(value).components().count() == 1,
        "archive filename must be a leaf"
    );
    Ok(())
}
fn hash_row(h: &mut Sha256, key: &[u8], value: &[u8]) {
    h.update(key);
    h.update((value.len() as u64).to_be_bytes());
    h.update(value);
}
fn validate_record(key: &[u8], value: &[u8]) -> Result<PositionUpdateMsg> {
    let msg = codec::decode(value)?;
    ensure!(
        decode_seq_key(key) == Some((msg.received_at_us, msg.seq)),
        "cold record/key mismatch"
    );
    ensure!(
        codec::encode(&msg)? == value,
        "cold record is not canonical PB"
    );
    Ok(msg)
}
fn validate_segment(s: &Segment) -> Result<()> {
    leaf(&s.id)?;
    ensure!(
        !s.source_identity.is_empty()
            && s.source_identity.len() <= 256
            && s.count > 0
            && s.first.0 > 0
            && s.first <= s.last,
        "invalid cold segment range/identity"
    );
    ensure!(
        s.payload_sha256.len() == 64 && s.payload_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid cold digest"
    );
    ensure!(
        super::storage::day_key(s.first.0) == super::storage::day_key(s.last.0),
        "cold segment crosses Shanghai day"
    );
    let _: retention::Boundary = rmp_serde::from_slice(&s.boundary_before)?;
    let _: retention::Boundary = rmp_serde::from_slice(&s.boundary_after)?;
    Ok(())
}

impl ColdRoot {
    pub fn create(path: &Path, identity: &str) -> Result<Self> {
        ensure!(
            !identity.is_empty() && identity.len() <= 256,
            "invalid cold source identity"
        );
        private_dir(path)?;
        let marker = path.join(ROOT_MARKER);
        if !marker.exists() {
            ensure!(
                std::fs::read_dir(path)?
                    .all(|e| e.is_ok_and(|e| e.file_name() == ".maintenance.lock")),
                "refuse foreign cold root before opening a write handle"
            );
        }
        let _guard = lock(&path.join(".maintenance.lock"), false)?;
        if marker.exists() {
            ensure!(
                json_file::<String>(&marker)? == identity,
                "cold root identity mismatch"
            );
        } else {
            ensure!(
                std::fs::read_dir(path)?
                    .all(|e| e.is_ok_and(|e| e.file_name() == ".maintenance.lock")),
                "refuse to initialize a nonempty foreign cold root"
            );
            durable_json(&marker, &identity)?;
        }
        Ok(Self {
            path: path.canonicalize()?,
            identity: identity.to_owned(),
        })
    }
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute() && !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
            "cold root must be an absolute real directory"
        );
        let identity: String = json_file(&path.join(ROOT_MARKER))?;
        ensure!(!identity.is_empty(), "missing cold root identity");
        Ok(Self {
            path: path.canonicalize()?,
            identity,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn pending(&self) -> Result<Option<Segment>> {
        let _guard = lock(&self.path.join(".maintenance.lock"), true)?;
        let mut pending = None;
        for e in std::fs::read_dir(&self.path)? {
            let e = e?;
            if !e.file_type()?.is_dir()
                || e.file_name().to_string_lossy().starts_with('.')
                || e.path().join(READY).exists()
            {
                continue;
            }
            let s: Segment = json_file(&e.path().join(SEGMENT_MARKER))?;
            validate_segment(&s)?;
            ensure!(
                s.source_identity == self.identity && pending.is_none(),
                "foreign/multiple pending cold batches"
            );
            pending = Some(s);
        }
        Ok(pending)
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    fn segment_path(&self, id: &str) -> Result<PathBuf> {
        leaf(id)?;
        Ok(self.path.join(id))
    }
    pub fn list(&self, now_us: i64, expired_only: bool) -> Result<Vec<Segment>> {
        let _guard = lock(&self.path.join(".maintenance.lock"), true)?;
        self.list_unlocked(now_us, expired_only)
    }
    pub fn list_after(
        &self,
        now_us: i64,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Segment>> {
        ensure!((1..=100).contains(&limit), "invalid cold inventory limit");
        if let Some(id) = after {
            leaf(id)?;
        }
        let mut segments = self.list(now_us, true)?;
        // Fixed-width original (timestamp, sequence) prefixes preserve event order.
        segments.retain(|s| after.is_none_or(|id| s.id.as_str() > id));
        segments.truncate(limit);
        Ok(segments)
    }
    fn list_unlocked(&self, now_us: i64, expired_only: bool) -> Result<Vec<Segment>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.path)? {
            let e = e?;
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if !e.file_type()?.is_dir() || e.file_type()?.is_symlink() {
                continue;
            }
            let p = e.path();
            if !p.join(READY).is_file() {
                continue;
            }
            let s: Segment = json_file(&p.join(SEGMENT_MARKER))?;
            validate_segment(&s)?;
            ensure!(
                s.id == e.file_name().to_string_lossy() && s.source_identity == self.identity,
                "cold segment/root mismatch"
            );
            if !expired_only || s.last < retention::cutoff(now_us) {
                out.push(s);
            }
        }
        out.sort_by_key(|s| s.first);
        for pair in out.windows(2) {
            ensure!(pair[0].last < pair[1].first, "overlapping cold segments");
        }
        Ok(out)
    }
    /// Build exactly one cold transaction. Source removal is a separate owner operation.
    pub fn write_batch(
        &self,
        rows: &[(Cursor, Vec<u8>)],
        before: &retention::Boundary,
        after: &retention::Boundary,
        limits: BatchLimits,
    ) -> Result<Segment> {
        limits.validate()?;
        ensure!(
            !rows.is_empty() && rows.len() <= limits.max_records,
            "cold batch record limit exceeded"
        );
        let _guard = lock(&self.path.join(".maintenance.lock"), false)?;
        let mut hash = Sha256::new();
        let mut encoded_bytes = 0u64;
        let mut previous = None;
        for (cursor, value) in rows {
            ensure!(
                previous.is_none_or(|p| p < *cursor),
                "cold batch is not oldest-first"
            );
            let key = encode_seq_key(cursor.0, cursor.1)?;
            validate_record(&key, value)?;
            hash_row(&mut hash, &key, value);
            encoded_bytes += 12 + value.len() as u64;
            previous = Some(*cursor);
        }
        let digest = hex::encode(hash.finalize());
        let first = rows[0].0;
        let last = rows.last().unwrap().0;
        let s = Segment {
            source_identity: self.identity.clone(),
            id: format!("{:020}-{:010}-{}", first.0, first.1, &digest[..16]),
            first,
            last,
            count: rows.len() as u64,
            encoded_bytes,
            payload_sha256: digest,
            boundary_before: rmp_serde::to_vec(before)?,
            boundary_after: rmp_serde::to_vec(after)?,
        };
        validate_segment(&s)?;
        let final_path = self.segment_path(&s.id)?;
        if final_path.exists() {
            ensure!(
                verify_segment(&final_path)? == s,
                "conflicting retry of cold batch"
            );
            return Ok(s);
        }
        for e in std::fs::read_dir(&self.path)? {
            let e = e?;
            if !e.file_type()?.is_dir() || e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let previous: Segment = json_file(&e.path().join(SEGMENT_MARKER))?;
            ensure!(
                e.path().join(READY).exists() && previous.last < first,
                "previous cold batch is pending or would overlap"
            );
        }
        let staging = tempfile::Builder::new()
            .prefix(".writing-")
            .tempdir_in(&self.path)?;
        let db = DB::open_cf(&options(true), staging.path(), ["default", ROWS])?;
        let cf = db.cf_handle(ROWS).unwrap();
        let mut batch = WriteBatch::default();
        for (cursor, value) in rows {
            batch.put_cf(cf, encode_seq_key(cursor.0, cursor.1)?, value);
        }
        batch.put(META, serde_json::to_vec(&s)?);
        ensure!(
            batch.size_in_bytes() <= limits.max_bytes,
            "encoded cold WriteBatch byte limit exceeded; source must remain unchanged"
        );
        db.write_opt(batch, &sync_options())?;
        db.flush_cf(cf)?;
        db.flush()?;
        db.flush_wal(true)?;
        drop(db);
        durable_json(&staging.path().join(SEGMENT_MARKER), &s)?;
        ensure!(
            verify_segment(staging.path())? == s,
            "cold batch readback differs"
        );
        for e in std::fs::read_dir(staging.path())? {
            let e = e?;
            if e.file_type()?.is_file() {
                File::open(e.path())?.sync_all()?;
            }
        }
        sync_dir(staging.path())?;
        std::fs::rename(staging.path(), &final_path)?;
        sync_dir(&self.path)?;
        Ok(s)
    }
    /// Called only AFTER the source owner durably commits hot removal/progress.
    pub fn confirm(&self, s: &Segment) -> Result<()> {
        let _guard = lock(&self.path.join(".maintenance.lock"), false)?;
        ensure!(
            s.source_identity == self.identity,
            "foreign cold confirmation"
        );
        let path = self.segment_path(&s.id)?;
        ensure!(
            verify_segment(&path)? == *s,
            "cold confirmation readback failed"
        );
        durable_json(&path.join(READY), &s.payload_sha256)?;
        Ok(())
    }
    pub fn visit(
        &self,
        s: &Segment,
        mut f: impl FnMut(Cursor, Vec<u8>) -> Result<()>,
    ) -> Result<()> {
        ensure!(s.source_identity == self.identity, "foreign cold read");
        let path = self.segment_path(&s.id)?;
        let _lease = lock(&self.path.join(format!(".lease-{}", s.id)), true)?;
        let db = readonly_segment(&path)?;
        let cf = db.cf_handle(ROWS).unwrap();
        for item in db.iterator_cf(cf, IteratorMode::Start) {
            let (k, v) = item?;
            f(
                decode_seq_key(&k).context("invalid cold key")?,
                v.into_vec(),
            )?;
        }
        Ok(())
    }
    pub fn delete_verified(&self, s: &Segment, now_us: i64) -> Result<()> {
        validate_segment(s)?;
        ensure!(
            s.source_identity == self.identity && s.last < retention::cutoff(now_us),
            "cold deletion would remove retained 33-day history"
        );
        let _guard = lock(&self.path.join(".maintenance.lock"), false)?;
        let _lease = lock(&self.path.join(format!(".lease-{}", s.id)), false)?;
        let path = self.segment_path(&s.id)?;
        let retired = self.path.join(format!(".retired-{}.json", s.id));
        let receipt_hash = hex::encode(Sha256::digest(serde_json::to_vec(s)?));
        if !path.exists() {
            ensure!(
                json_file::<String>(&retired)? == receipt_hash,
                "missing unconfirmed cold segment"
            );
            let removing = self.path.join(format!(".removing-{}", s.id));
            if removing.exists() {
                // remove_dir_all may have partially completed before a crash.
                // The exact retired receipt was persisted before the rename;
                // requiring an intact DB here would prevent recovery forever.
                let metadata = std::fs::symlink_metadata(&removing)?;
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "retired staging is not a real directory"
                );
                std::fs::remove_dir_all(removing)?;
                sync_dir(&self.path)?;
            }
            package::remove(self, s)?;
            return Ok(());
        }
        ensure!(
            self.list_unlocked(now_us, false)?.first() == Some(s),
            "cold segments must be retired oldest-first"
        );
        ensure!(
            path.join(READY).is_file() && verify_segment(&path)? == *s,
            "source cold segment changed or is unconfirmed"
        );
        // The checkpoint remains after retiring original data, preserving carried inventory.
        durable_json(&self.path.join("RETAINED_BOUNDARY.json"), s)?;
        durable_json(&retired, &receipt_hash)?;
        let removing = self.path.join(format!(".removing-{}", s.id));
        std::fs::rename(&path, &removing)?;
        sync_dir(&self.path)?;
        std::fs::remove_dir_all(&removing)?;
        sync_dir(&self.path)?;
        package::remove(self, s)
    }
}

fn readonly_segment(path: &Path) -> Result<DB> {
    ensure!(
        !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "cold segment cannot be a symlink"
    );
    let names = DB::list_cf(&options(false), path)?;
    ensure!(
        names.len() == 2 && names.iter().all(|n| n == "default" || n == ROWS),
        "not a cold signal segment; refuse foreign/Exec store"
    );
    Ok(DB::open_cf_for_read_only(
        &options(false),
        path,
        names,
        false,
    )?)
}
pub fn verify_segment(path: &Path) -> Result<Segment> {
    let expected: Segment = json_file(&path.join(SEGMENT_MARKER))?;
    validate_segment(&expected)?;
    let db = readonly_segment(path)?;
    let stored: Segment =
        serde_json::from_slice(&db.get(META)?.context("missing cold DB marker")?)?;
    ensure!(stored == expected, "cold manifest/DB marker mismatch");
    let cf = db.cf_handle(ROWS).unwrap();
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut bytes = 0;
    let mut first = None;
    let mut last = None;
    let mut boundary: retention::Boundary = rmp_serde::from_slice(&expected.boundary_before)?;
    for row in db.iterator_cf(cf, IteratorMode::Start) {
        let (k, v) = row?;
        let message = validate_record(&k, &v)?;
        boundary.ingest(&message)?;
        let cursor = decode_seq_key(&k).unwrap();
        first.get_or_insert(cursor);
        last = Some(cursor);
        count += 1;
        bytes += 12 + v.len() as u64;
        hash_row(&mut hash, &k, &v);
    }
    ensure!(
        first == Some(expected.first)
            && last == Some(expected.last)
            && count == expected.count
            && bytes == expected.encoded_bytes
            && hex::encode(hash.finalize()) == expected.payload_sha256,
        "cold segment checksum/count/range mismatch"
    );
    ensure!(
        rmp_serde::to_vec(&boundary)? == expected.boundary_after,
        "cold inventory boundary mismatch"
    );
    Ok(expected)
}

#[cfg(test)]
mod tests;
