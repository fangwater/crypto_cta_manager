//! Explicit offline JSON replacement. No startup conversion or mixed live reader.
use super::*;
use anyhow::ensure;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

const BACKUP_PROOF: &str = "POSITION_JSON_BACKUP.json";
const REPORT: &str = "POSITION_STORAGE_MIGRATION.json";

#[derive(Clone, Serialize, Deserialize)]
struct Progress {
    identity: String,
    backup: String,
    cursor: Option<(i64, u32)>,
    imported: u64,
}
#[derive(Serialize, Deserialize)]
struct BackupProof {
    identity: String,
    cf_hashes: BTreeMap<String, String>,
}

pub(super) fn decode_json(key: &[u8], bytes: &[u8]) -> Result<PositionUpdateMsg> {
    let message: PositionUpdateMsg =
        serde_json::from_slice(bytes).context("decode explicit legacy JSON input")?;
    ensure!(
        manager_db::decode_seq_key(key) == Some((message.received_at_us, message.seq))
            && message.msg_type == MSG_TYPE,
        "legacy key/message mismatch"
    );
    message.strategy.validate().map_err(anyhow::Error::msg)?;
    Ok(message)
}
fn atomic_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("missing report parent")?)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    std::fs::File::open(path.parent().context("missing report parent")?)?.sync_all()?;
    Ok(())
}
fn sync() -> rocksdb::WriteOptions {
    let mut o = rocksdb::WriteOptions::default();
    o.set_sync(true);
    o
}
fn hashes(
    db: &crate::manager_db::ManagerRocksDb,
    names: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for name in names {
        let cf = db.cf_handle(name).context("missing backup CF")?;
        let mut hash = Sha256::new();
        for row in db.iterator_cf(&cf, IteratorMode::Start) {
            let (key, value) = row?;
            if name == "default"
                && [
                    storage::FORMAT_KEY.as_bytes(),
                    storage::MIGRATING_KEY.as_bytes(),
                ]
                .contains(&key.as_ref())
            {
                continue;
            }
            hash.update((key.len() as u64).to_be_bytes());
            hash.update(&key);
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(&value);
        }
        out.insert(name.clone(), hex::encode(hash.finalize()));
    }
    Ok(out)
}
fn readonly(path: &Path) -> Result<crate::manager_db::ManagerRocksDb> {
    let mut options = rocksdb::Options::default();
    options.set_max_open_files(128);
    let names = crate::manager_db::ManagerRocksDb::list_cf(&options, path)?;
    crate::manager_db::ManagerRocksDb::open_cf_for_read_only(&options, path, names, false)
        .map_err(Into::into)
}
/// Read-only deployment gate. A legacy or interrupted store must not have its
/// running owner replaced by an incompatible binary before planned migration.
pub fn check_storage(path: &Path) -> Result<serde_json::Value> {
    check_storage_inner(path, false, false)
}

/// Explicit cold mode keeps the entire hot store JSON and converts only expired
/// records into independent PB segments. This check never initializes either DB.
pub fn check_storage_with_cold(path: &Path, cold_root: &Path) -> Result<serde_json::Value> {
    super::cold::manager::Settings {
        root: cold_root.to_owned(),
        ..Default::default()
    }
    .validate()?;
    let hot = crate::config::resolve_storage_path(path)?;
    let cold = crate::config::resolve_storage_path(cold_root)?;
    ensure!(
        !hot.starts_with(&cold) && !cold.starts_with(&hot),
        "cold root overlaps hot DB"
    );
    if cold_root.exists() {
        ensure!(
            super::cold::ColdRoot::open(cold_root)?.identity()
                == std::fs::read_to_string(path.join("IDENTITY"))?.trim(),
            "cold source identity mismatch"
        );
    }
    check_storage_inner(path, true, false)
}

pub fn check_storage_with_hot_conversion(
    path: &Path,
    cold_root: &Path,
) -> Result<serde_json::Value> {
    super::cold::manager::Settings {
        root: cold_root.to_owned(),
        ..Default::default()
    }
    .validate()?;
    // Reuse path/identity checks, but the active transition needs its own opt-in.
    let hot = crate::config::resolve_storage_path(path)?;
    let cold = crate::config::resolve_storage_path(cold_root)?;
    ensure!(
        cold_root.is_absolute() && !hot.starts_with(&cold) && !cold.starts_with(&hot),
        "invalid cold path"
    );
    if cold_root.exists() {
        ensure!(
            super::cold::ColdRoot::open(cold_root)?.identity()
                == std::fs::read_to_string(path.join("IDENTITY"))?.trim(),
            "cold source identity mismatch"
        );
    }
    check_storage_inner(path, true, true)
}

fn check_storage_inner(
    path: &Path,
    allow_json: bool,
    allow_conversion: bool,
) -> Result<serde_json::Value> {
    ensure!(path.is_absolute(), "Manager DB path must be absolute");
    if !path.exists() {
        return Ok(serde_json::json!({"storage_format":"empty","database_exists":false}));
    }
    let db = readonly(path)?;
    ensure!(
        !["uniform_orders", "order_updates", "trade_updates"]
            .iter()
            .any(|n| db.cf_handle(n).is_some()),
        "refuse Exec store"
    );
    ensure!(
        db.get(storage::MIGRATING_KEY)?.is_none(),
        "signal migration is incomplete; finish the explicit migration before deployment"
    );
    let marked = db.get(storage::FORMAT_KEY)?;
    let conversion = super::online::inspect_progress(
        db.get(super::online::META)?.as_deref(),
        marked.as_deref(),
        std::fs::read_to_string(path.join("IDENTITY"))?.trim(),
    )?;
    ensure!(
        conversion
            .as_ref()
            .is_none_or(|p| p.completed || allow_conversion),
        "active hot conversion requires explicit archive.cold.hot_conversion before deployment"
    );
    if let Some(marker) = marked {
        ensure!(marker == storage::FORMAT, "unknown signal storage format");
        ensure!(
            [
                POSITION_UPDATES_CF,
                storage::OBSERVATIONS_CF,
                storage::SNAPSHOTS_CF
            ]
            .iter()
            .all(|n| db.cf_handle(n).is_some()),
            "compact archive CF is missing"
        );
        return Ok(
            serde_json::json!({"storage_format":if conversion.as_ref().is_some_and(|p| !p.completed) {"converting_json"} else {"protobuf"},"database_exists":true}),
        );
    }
    for name in [
        POSITION_UPDATES_CF,
        storage::OBSERVATIONS_CF,
        storage::SNAPSHOTS_CF,
    ] {
        if let Some(cf) = db.cf_handle(name) {
            if allow_json && name == POSITION_UPDATES_CF {
                for mode in [IteratorMode::Start, IteratorMode::End] {
                    if let Some(row) = db.iterator_cf(&cf, mode).next() {
                        let (key, value) = row?;
                        decode_json(&key, &value)?;
                    }
                }
                continue;
            }
            ensure!(
                db.iterator_cf(&cf, IteratorMode::Start)
                    .next()
                    .transpose()?
                    .is_none(),
                "legacy or unmarked signals require explicit migrate-json before deployment; running Manager has not been changed"
            );
        }
    }
    if allow_json
        && let Some(cf) = db.cf_handle(POSITION_UPDATES_CF)
        && db
            .iterator_cf(&cf, IteratorMode::Start)
            .next()
            .transpose()?
            .is_some()
    {
        return Ok(
            serde_json::json!({"storage_format":"json","database_exists":true,"cold_mode":true}),
        );
    }
    Ok(serde_json::json!({"storage_format":"empty","database_exists":true}))
}
fn validate_paths(db: &Path, backup: &Path) -> Result<()> {
    ensure!(
        db.is_absolute() && db.join("IDENTITY").is_file(),
        "use an existing absolute Manager DB path"
    );
    ensure!(
        backup.is_absolute() && !backup.starts_with(db) && !db.starts_with(backup),
        "backup and database must be separate absolute paths"
    );
    let primary = db.canonicalize()?;
    let parent = backup
        .parent()
        .context("backup parent missing")?
        .canonicalize()?;
    let backup = parent.join(backup.file_name().context("backup name missing")?);
    ensure!(
        !backup.starts_with(&primary) && !primary.starts_with(&backup),
        "backup overlaps database through a symlink"
    );
    ensure!(
        !backup.exists() || !std::fs::symlink_metadata(&backup)?.file_type().is_symlink(),
        "backup cannot be a symlink"
    );
    Ok(())
}

/// Acquires the database's exclusive RocksDB lock. The operator must stop only
/// Manager before running this against a live path; Exec stores are refused.
/// A complete, verified original checkpoint precedes every source mutation.
pub fn migrate_json(path: &Path, backup: &Path, compact: bool) -> Result<serde_json::Value> {
    migrate_with(path, backup, compact, |_| Ok(()))
}
/// Prepare the original DB backup without converting a single signal. This
/// allows its verified copy to reach powerleader NVMe before remote replacement.
pub fn backup_json(path: &Path, backup: &Path) -> Result<serde_json::Value> {
    migrate_impl(path, backup, false, true, |_| Ok(()))
}
/// Independently verify an original checkpoint after transferring it elsewhere.
pub fn verify_json_backup(path: &Path) -> Result<serde_json::Value> {
    let proof: BackupProof =
        serde_json::from_reader(std::fs::File::open(path.join(BACKUP_PROOF))?)?;
    ensure!(
        std::fs::read_to_string(path.join("IDENTITY"))?.trim() == proof.identity,
        "original backup identity changed"
    );
    let db = readonly(path)?;
    ensure!(
        db.get(storage::FORMAT_KEY)?.is_none() && db.get(storage::MIGRATING_KEY)?.is_none(),
        "expected an untouched original JSON checkpoint"
    );
    let names = proof.cf_hashes.keys().cloned().collect::<Vec<_>>();
    let mut actual =
        crate::manager_db::ManagerRocksDb::list_cf(&rocksdb::Options::default(), path)?;
    actual.sort();
    ensure!(
        actual == names,
        "original backup column-family catalog changed"
    );
    ensure!(
        hashes(&db, &names)? == proof.cf_hashes,
        "original backup readback differs"
    );
    Ok(
        serde_json::json!({"original_backup_verified":true,"identity":proof.identity,"cf_hashes":proof.cf_hashes}),
    )
}
fn migrate_with(
    path: &Path,
    backup: &Path,
    compact: bool,
    after_write: impl Fn(u64) -> Result<()>,
) -> Result<serde_json::Value> {
    migrate_impl(path, backup, compact, false, after_write)
}
fn migrate_impl(
    path: &Path,
    backup: &Path,
    compact: bool,
    backup_only: bool,
    after_write: impl Fn(u64) -> Result<()>,
) -> Result<serde_json::Value> {
    validate_paths(path, backup)?;
    let names = crate::manager_db::ManagerRocksDb::list_cf(&rocksdb::Options::default(), path)?;
    ensure!(
        names.iter().any(|f| f == POSITION_UPDATES_CF)
            && !names.iter().any(|f| matches!(
                f.as_str(),
                "uniform_orders" | "order_updates" | "trade_updates"
            )),
        "migration requires Manager, never an Exec store"
    );
    let db = ManagerDb::open(path)
        .context("migration needs exclusive Manager DB ownership; do not remove LOCK")?;
    let identity = std::fs::read_to_string(path.join("IDENTITY"))?
        .trim()
        .to_owned();
    let progress = db
        .db()
        .get(storage::MIGRATING_KEY)?
        .map(|v| serde_json::from_slice::<Progress>(&v))
        .transpose()?;
    let already_current = db.db().get(storage::FORMAT_KEY)?.as_deref() == Some(storage::FORMAT);
    if backup_only {
        ensure!(
            progress.is_none() && !already_current,
            "backup-json requires an original, unmigrated JSON store"
        );
    }
    ensure!(
        already_current || db.db().get(storage::FORMAT_KEY)?.is_none(),
        "unknown archive storage marker"
    );
    if progress.is_none() && !already_current && !backup.exists() {
        db.db().flush_wal(true)?;
        rocksdb::checkpoint::Checkpoint::new(db.db())?.create_checkpoint(backup)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(backup, std::fs::Permissions::from_mode(0o700))?;
        // Restore the original identity too: checkpoint() omits this file.
        std::fs::write(
            backup.join("IDENTITY"),
            std::fs::read(path.join("IDENTITY"))?,
        )?;
        let expected = hashes(db.db(), &names)?;
        let copy = readonly(backup)?;
        ensure!(
            hashes(&copy, &names)? == expected,
            "original database backup readback differs"
        );
        drop(copy);
        for entry in std::fs::read_dir(backup)? {
            let p = entry?.path();
            if p.is_file() {
                std::fs::File::open(p)?.sync_all()?;
            }
        }
        atomic_file(
            &backup.join(BACKUP_PROOF),
            &BackupProof {
                identity: identity.clone(),
                cf_hashes: expected,
            },
        )?;
    }
    let proof: BackupProof = serde_json::from_reader(
        std::fs::File::open(backup.join(BACKUP_PROOF))
            .context("verified original JSON backup is required")?,
    )?;
    ensure!(
        proof.identity == identity,
        "backup belongs to another database"
    );
    let original = readonly(backup)?;
    let original_names = proof.cf_hashes.keys().cloned().collect::<Vec<_>>();
    let mut actual =
        crate::manager_db::ManagerRocksDb::list_cf(&rocksdb::Options::default(), backup)?;
    actual.sort();
    ensure!(
        actual == original_names,
        "original backup column-family catalog changed"
    );
    ensure!(
        hashes(&original, &original_names)? == proof.cf_hashes,
        "original backup was changed or corrupted"
    );
    if progress.is_none() && !already_current {
        ensure!(
            hashes(db.db(), &original_names)? == proof.cf_hashes,
            "database changed since the verified backup"
        );
    }
    if backup_only {
        return Ok(
            serde_json::json!({"original_backup_verified":true,"identity":identity,"cf_hashes":proof.cf_hashes,"database_unchanged":true,"original_backup":backup}),
        );
    }
    let mut progress = progress.unwrap_or(Progress {
        identity: identity.clone(),
        backup: backup.to_string_lossy().into_owned(),
        cursor: None,
        imported: 0,
    });
    ensure!(
        progress.identity == identity && Path::new(&progress.backup) == backup,
        "resume requires the exact original backup path and identity"
    );
    if !already_current {
        db.db().put_opt(
            storage::MIGRATING_KEY,
            serde_json::to_vec(&progress)?,
            &sync(),
        )?;
        storage::families(&db)?;
        let storage = storage::Storage::default();
        let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
        let start = manager_db::encode_seq_key(
            progress.cursor.map_or(1, |p| p.0),
            progress.cursor.map_or(0, |p| p.1),
        )?;
        let mut since_sync = 0;
        for row in db
            .db()
            .iterator_cf(&cf, IteratorMode::From(&start, Direction::Forward))
        {
            let (key, value) = row?;
            let cursor = manager_db::decode_seq_key(&key).context("invalid JSON archive key")?;
            if progress.cursor.is_some_and(|p| cursor <= p) {
                continue;
            }
            let message = decode_json(&key, &value)?;
            let mut batch = rocksdb::WriteBatch::default();
            let pending = storage.stage(&db, &message, &mut batch, true)?;
            progress.cursor = Some(cursor);
            progress.imported += 1;
            batch.put(storage::MIGRATING_KEY, serde_json::to_vec(&progress)?);
            db.db().write(batch)?;
            storage.published(pending);
            after_write(progress.imported)?;
            since_sync += value.len();
            if since_sync >= 4 * 1024 * 1024 {
                db.db().flush_wal(true)?;
                since_sync = 0;
            }
        }
        db.db().flush_wal(true)?;
    }
    // Verify every field and f64 bit after a cold storage-reader pass, not just
    // codec roundtrips; also prove all unrelated CFs/current targets unchanged.
    let storage = storage::Storage::default();
    let old_cf = original.cf_handle(POSITION_UPDATES_CF).unwrap();
    let new_cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let mut typed = Sha256::new();
    let mut count = 0;
    for row in original.iterator_cf(&old_cf, IteratorMode::Start) {
        let (key, value) = row?;
        let old = decode_json(&key, &value)?;
        let target = db
            .db()
            .get_cf(&new_cf, &key)?
            .context("migrated target is missing")?;
        let decoded = storage.complete(&db, &key, &target)?;
        let before = rmp_serde::to_vec(&old)?;
        let after = rmp_serde::to_vec(&decoded)?;
        ensure!(
            before == after,
            "migration changed typed history at {}:{}",
            old.received_at_us,
            old.seq
        );
        typed.update(&key);
        typed.update((before.len() as u64).to_be_bytes());
        typed.update(before);
        count += 1;
    }
    ensure!(
        db.db()
            .iterator_cf(&new_cf, IteratorMode::Start)
            .try_fold(0u64, |n, row| row.map(|_| n + 1))?
            == count,
        "migration changed record count"
    );
    let unrelated = original_names
        .iter()
        .filter(|n| n.as_str() != POSITION_UPDATES_CF)
        .cloned()
        .collect::<Vec<_>>();
    let after = hashes(db.db(), &unrelated)?;
    ensure!(
        unrelated
            .iter()
            .all(|n| after.get(n) == proof.cf_hashes.get(n)),
        "migration changed unrelated data/current targets"
    );
    let mut batch = rocksdb::WriteBatch::default();
    batch.put(storage::FORMAT_KEY, storage::FORMAT);
    batch.delete(storage::MIGRATING_KEY);
    db.db().write_opt(batch, &sync())?;
    if compact {
        for name in [
            POSITION_UPDATES_CF,
            storage::OBSERVATIONS_CF,
            storage::SNAPSHOTS_CF,
        ] {
            let cf = db.db().cf_handle(name).unwrap();
            db.db().flush_cf(&cf)?;
            db.db().compact_range_cf(&cf, None::<&[u8]>, None::<&[u8]>);
        }
    }
    let report = serde_json::json!({"format":"protobuf","records":count,"typed_history_sha256":hex::encode(typed.finalize()),
        "typed_history_equal":true,"unrelated_data_equal":true,"original_backup":backup,"compacted":compact});
    atomic_file(&path.join(REPORT), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_requires_owner_backup_and_resumes_without_changing_history_or_other_data() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("manager");
        let backup = root.path().join("original");
        let db = ManagerDb::open(&path).unwrap();
        assert_eq!(check_storage(&path).unwrap()["storage_format"], "empty");
        db.ensure_column_family("klines_1m").unwrap();
        let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
        let mut expected = Vec::new();
        let mut original_bytes = Vec::new();
        for i in 0..7 {
            let mut m = super::super::storage_tests::message(
                1_700_000_000_000_001 + i,
                0.12345678901234567 + i as f64 * 1e-17,
            );
            m.factual_positions[0].positions[0].qty = i as f64 * 1e-17;
            let key = manager_db::encode_seq_key(m.received_at_us, m.seq).unwrap();
            let bytes = serde_json::to_vec(&m).unwrap();
            expected.push(decode_json(&key, &bytes).unwrap());
            original_bytes.push((key, bytes.clone()));
            db.db().put_cf(&cf, key, bytes).unwrap();
        }
        db.db()
            .put("position_current:alpha", b"unchanged current target")
            .unwrap();
        assert!(check_storage(&path).is_err());
        assert_eq!(
            check_storage_with_cold(&path, &root.path().join("cold")).unwrap()["storage_format"],
            "json"
        );
        assert!(check_storage_with_cold(&path, &path.join("cold")).is_err());
        let kline = db.db().cf_handle("klines_1m").unwrap();
        db.db()
            .put_cf(&kline, b"sentinel", b"unchanged candles")
            .unwrap();
        drop(kline);
        assert!(migrate_json(&path, &backup, false).is_err());
        assert!(!backup.exists());
        assert!(PositionArchive::open(db.clone()).is_err());
        assert!(db.db().get(storage::FORMAT_KEY).unwrap().is_none());
        drop(cf);
        drop(db);
        assert_eq!(
            backup_json(&path, &backup).unwrap()["database_unchanged"],
            true
        );
        assert_eq!(
            verify_json_backup(&backup).unwrap()["original_backup_verified"],
            true
        );
        let copied = root.path().join("powerleader-copy");
        std::fs::create_dir(&copied).unwrap();
        for entry in std::fs::read_dir(&backup).unwrap() {
            let entry = entry.unwrap();
            assert!(entry.path().is_file());
            std::fs::copy(entry.path(), copied.join(entry.file_name())).unwrap();
        }
        assert_eq!(
            verify_json_backup(&copied).unwrap()["cf_hashes"],
            verify_json_backup(&backup).unwrap()["cf_hashes"]
        );
        let interrupted = migrate_with(&path, &backup, false, |n| {
            ensure!(n < 3, "injected interruption after committed prefix");
            Ok(())
        });
        assert!(interrupted.is_err());
        assert!(PositionArchive::open(ManagerDb::open(&path).unwrap()).is_err());
        assert!(check_storage(&path).is_err());
        let report = migrate_json(&path, &backup, true).unwrap();
        assert_eq!(report["records"], 7);
        assert_eq!(report["typed_history_equal"], true);
        assert_eq!(check_storage(&path).unwrap()["storage_format"], "protobuf");
        let source = PositionArchive::open(ManagerDb::open(&path).unwrap()).unwrap();
        assert_eq!(
            rmp_serde::to_vec(&source.scan_from(1).unwrap()).unwrap(),
            rmp_serde::to_vec(&expected).unwrap()
        );
        assert_eq!(
            source
                .db
                .db()
                .get("position_current:alpha")
                .unwrap()
                .unwrap(),
            b"unchanged current target"
        );
        drop(source);
        let copy = readonly(&backup).unwrap();
        let cf = copy.cf_handle(POSITION_UPDATES_CF).unwrap();
        for (key, bytes) in original_bytes {
            assert_eq!(copy.get_cf(&cf, key).unwrap().unwrap(), bytes);
        }
        drop(cf);
        drop(copy);
        assert_eq!(migrate_json(&path, &backup, false).unwrap()["records"], 7);
    }
}
