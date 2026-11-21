use super::*;
use rocksdb::{DB, Options, WriteBatch, WriteOptions};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct Owner {
    pub kind: String,
    pub source_id: String,
}

pub(super) fn open_owned(path: &Path, kind: &str, source: &str, cfs: &[&str]) -> Result<DB> {
    let mut options = Options::default();
    options.set_max_open_files(64);
    if path.join("CURRENT").exists() {
        let existing = DB::list_cf(&options, path)?;
        let view = DB::open_cf_for_read_only(&options, path, &existing, false)?;
        let owner: Owner = serde_json::from_slice(
            &view
                .get(OWNER)?
                .context("existing database has no native sync ownership marker")?,
        )?;
        ensure!(
            owner.kind == kind && owner.source_id == source,
            "existing database belongs to another owner or source"
        );
        ensure!(
            existing.len() == cfs.len() + 1
                && existing
                    .iter()
                    .all(|n| n == "default" || cfs.contains(&n.as_str())),
            "unexpected native sync column families"
        );
        drop(view);
    } else {
        ensure!(
            !path.exists() || fs::read_dir(path)?.next().is_none(),
            "native sync refuses an existing nonempty, unmarked directory"
        );
    }
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let db = DB::open_cf(&options, path, cfs)?;
    let mut batch = WriteBatch::default();
    batch.put(
        OWNER,
        serde_json::to_vec(&Owner {
            kind: kind.into(),
            source_id: source.into(),
        })?,
    );
    write(&db, batch)?;
    Ok(db)
}
pub(super) fn write(db: &DB, batch: WriteBatch) -> Result<()> {
    let mut options = WriteOptions::default();
    options.set_sync(true);
    db.write_opt(batch, &options)
        .context("commit native synchronization WAL")
}
pub(super) fn seq_key(prefix: u8, seq: u64) -> Vec<u8> {
    let mut key = vec![prefix];
    key.extend_from_slice(&seq.to_be_bytes());
    key
}
pub(super) fn hash_key(cf: NativeCf, key: &[u8]) -> Vec<u8> {
    let mut out = vec![b'H', cf.index() as u8];
    out.extend_from_slice(key);
    out
}
pub(super) fn get<T: serde::de::DeserializeOwned>(db: &DB, key: &[u8]) -> Result<Option<T>> {
    db.get(key)?
        .map(|v| serde_json::from_slice(&v).map_err(Into::into))
        .transpose()
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaStatus {
    pub store_id: Option<String>,
    pub initialized: bool,
    pub applying: bool,
    pub committed_cursor: u64,
    pub committed_sha256: Option<String>,
    pub received_cursor: u64,
    pub received_sha256: Option<String>,
    pub target_cursor: Option<u64>,
    pub target_sha256: Option<String>,
    pub source_captured_at_us: Option<i64>,
    pub target_captured_at_us: Option<i64>,
    pub last_synced_at_us: Option<i64>,
    pub last_attempt_at_us: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct ReplicaBusy;
impl std::fmt::Display for ReplicaBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "本地执行副本正在初始化或提交同步批次，请稍后重试")
    }
}
impl std::error::Error for ReplicaBusy {}

pub fn replica_status(db: &DB, source: &str) -> Result<ReplicaStatus> {
    let owner: Owner = get(db, OWNER)?.context("native replica ownership marker is missing")?;
    ensure!(
        owner.kind == "cn_futures_replica" && owner.source_id == source,
        "native replica ownership mismatch"
    );
    get(db, STATE)?.context("native replica synchronization state is missing")
}
pub(super) fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("sync.lock")
}
pub fn replica_read_lock(path: &Path) -> Result<File> {
    let file = File::open(lock_path(path)).context("native replica lock is missing")?;
    file.try_lock_shared().map_err(|_| ReplicaBusy)?;
    Ok(file)
}
pub(super) fn writer_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(path))?;
    file.lock()?;
    Ok(file)
}
