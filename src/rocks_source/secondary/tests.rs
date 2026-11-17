use super::*;
use crate::rocks_source::visit_from;
use std::sync::atomic::Ordering;

fn primary(path: &Path, families: &[&str]) -> DB {
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    DB::open_cf(&options, path, families).unwrap()
}

fn pool(root: &Path) -> ReaderPool {
    ReaderPool {
        registry: Mutex::new(Registry {
            root: Some(root.to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn rows(pool: &ReaderPool, path: &Path, family: &str) -> BTreeMap<Vec<u8>, Vec<u8>> {
    pool.read(path, &[family.into()], |db| {
        let Some(cf) = db.cf_handle(family) else {
            return Ok(BTreeMap::new());
        };
        db.iterator_cf(cf, rocksdb::IteratorMode::Start)
            .map(|row| {
                let (key, value) = row?;
                Ok((key.into_vec(), value.into_vec()))
            })
            .collect()
    })
    .unwrap()
}

#[test]
fn catches_up_live_wal_and_late_keys_without_reopening() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let db = primary(&path, &["fills", "unused"]);
    let cf = db.cf_handle("fills").unwrap();
    db.put_cf(cf, b"200", b"first").unwrap();
    let pool = pool(&tmp.path().join("readers"));
    assert_eq!(rows(&pool, &path, "fills")[b"200".as_slice()], b"first");
    for i in 0..3 {
        db.put_cf(cf, b"100", format!("late-{i}")).unwrap();
        db.put_cf(cf, b"300", format!("tail-{i}")).unwrap();
        let actual = rows(&pool, &path, "fills");
        assert_eq!(actual[b"100".as_slice()], format!("late-{i}").as_bytes());
        assert_eq!(actual[b"300".as_slice()], format!("tail-{i}").as_bytes());
    }
    assert_eq!(pool.opens.load(Ordering::Relaxed), 1);
    let registry = pool.registry.lock().unwrap();
    let slot = registry
        .entries
        .values()
        .next()
        .unwrap()
        .reader
        .lock()
        .unwrap();
    assert_eq!(
        slot.as_ref().unwrap().families,
        BTreeSet::from(["default".into(), "fills".into()])
    );
    // Primary remains writable; secondary holds no primary write lock.
    db.put_cf(cf, b"400", b"live").unwrap();
}

#[test]
fn unchanged_refreshes_do_not_reread_the_retained_wal() {
    fn thread_read_bytes() -> u64 {
        std::fs::read_to_string("/proc/thread-self/io")
            .unwrap()
            .lines()
            .find_map(|line| {
                line.strip_prefix("rchar:")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap()
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let db = primary(&path, &["fills"]);
    let payload = vec![7_u8; 64 * 1024];
    for _ in 0..128 {
        db.put_cf(db.cf_handle("fills").unwrap(), b"key", &payload)
            .unwrap();
    }
    let pool = pool(&tmp.path().join("readers"));
    let before = thread_read_bytes();
    pool.read(&path, &["fills".into()], |_| Ok(())).unwrap();
    let first = thread_read_bytes() - before;
    let before = thread_read_bytes();
    for _ in 0..4 {
        pool.read(&path, &["fills".into()], |_| Ok(())).unwrap();
    }
    let steady = thread_read_bytes() - before;
    eprintln!("secondary WAL I/O: initial_read_bytes={first}, four_refresh_read_bytes={steady}");
    assert!(
        first >= 4 * 1024 * 1024,
        "fixture must exercise WAL recovery"
    );
    assert!(
        steady < first / 2,
        "unchanged refreshes reread retained WAL"
    );
    assert_eq!(pool.opens.load(Ordering::Relaxed), 1);
}

#[test]
fn survives_flush_compaction_and_primary_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let pool = pool(&tmp.path().join("readers"));
    {
        let db = primary(&path, &["fills"]);
        db.put_cf(db.cf_handle("fills").unwrap(), b"key", b"old")
            .unwrap();
        rows(&pool, &path, "fills");
        db.flush_cf(db.cf_handle("fills").unwrap()).unwrap();
        db.put_cf(db.cf_handle("fills").unwrap(), b"key", b"new")
            .unwrap();
        db.flush_cf(db.cf_handle("fills").unwrap()).unwrap();
        db.compact_range_cf(db.cf_handle("fills").unwrap(), None::<&[u8]>, None::<&[u8]>);
        assert_eq!(rows(&pool, &path, "fills")[b"key".as_slice()], b"new");
    }
    let db = primary(&path, &["fills"]);
    db.put_cf(db.cf_handle("fills").unwrap(), b"key", b"restarted")
        .unwrap();
    assert_eq!(rows(&pool, &path, "fills")[b"key".as_slice()], b"restarted");
    // Primary restart publishes a new OPTIONS file. Reopen once at that boundary.
    assert_eq!(pool.opens.load(Ordering::Relaxed), 2);
    rows(&pool, &path, "fills");
    assert_eq!(pool.opens.load(Ordering::Relaxed), 2);
}

#[test]
fn rebuilds_after_source_replacement_even_with_copied_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let pool = pool(&tmp.path().join("readers"));
    let db = primary(&path, &["fills"]);
    db.put_cf(db.cf_handle("fills").unwrap(), b"old", b"old")
        .unwrap();
    rows(&pool, &path, "fills");
    let identity = std::fs::read(path.join("IDENTITY")).unwrap();
    drop(db);
    std::fs::rename(&path, tmp.path().join("old-source")).unwrap();
    let db = primary(&path, &["fills"]);
    std::fs::write(path.join("IDENTITY"), identity).unwrap();
    db.put_cf(db.cf_handle("fills").unwrap(), b"new", b"new")
        .unwrap();
    assert_eq!(
        rows(&pool, &path, "fills"),
        BTreeMap::from([(b"new".to_vec(), b"new".to_vec())])
    );
    assert_eq!(pool.opens.load(Ordering::Relaxed), 2);
}

#[test]
fn adds_requested_families_once_and_detects_new_source_families() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let mut db = primary(&path, &["fills", "roles"]);
    let pool = pool(&tmp.path().join("readers"));
    rows(&pool, &path, "fills");
    rows(&pool, &path, "roles");
    rows(&pool, &path, "fills");
    assert_eq!(pool.opens.load(Ordering::Relaxed), 2);
    assert!(rows(&pool, &path, "future").is_empty());
    assert_eq!(pool.opens.load(Ordering::Relaxed), 2);
    db.create_cf("future", &Options::default()).unwrap();
    db.put_cf(db.cf_handle("future").unwrap(), b"key", b"new-cf")
        .unwrap();
    assert_eq!(rows(&pool, &path, "future")[b"key".as_slice()], b"new-cf");
    rows(&pool, &path, "fills");
    assert_eq!(pool.opens.load(Ordering::Relaxed), 3);
}

#[test]
fn does_not_reuse_a_dropped_and_recreated_family() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let mut db = primary(&path, &["fills"]);
    db.put_cf(db.cf_handle("fills").unwrap(), b"old", b"old")
        .unwrap();
    let pool = pool(&tmp.path().join("readers"));
    rows(&pool, &path, "fills");
    db.drop_cf("fills").unwrap();
    db.create_cf("fills", &Options::default()).unwrap();
    db.put_cf(db.cf_handle("fills").unwrap(), b"new", b"new")
        .unwrap();
    let first = pool.read(&path, &["fills".into()], |reader| {
        let cf = reader.cf_handle("fills").unwrap();
        reader
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .map(|row| {
                let (k, v) = row?;
                Ok((k.into_vec(), v.into_vec()))
            })
            .collect::<Result<BTreeMap<_, _>>>()
    });
    // Some RocksDB builds report the dropped handle during catch-up; others
    // report it from the iterator. Either way it must never yield stale data.
    if let Ok(ref actual) = first {
        assert_eq!(
            actual,
            &BTreeMap::from([(b"new".to_vec(), b"new".to_vec())])
        );
    }
    assert_eq!(
        rows(&pool, &path, "fills"),
        BTreeMap::from([(b"new".to_vec(), b"new".to_vec())])
    );
    assert_eq!(pool.opens.load(Ordering::Relaxed), 2);
}

#[test]
fn secondary_native_catch_up_after_historical_cf_recreation() {
    for _ in 0..10 {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("primary");
        let mut db = primary(&path, &["fills"]);
        db.drop_cf("fills").unwrap();
        db.create_cf("fills", &Options::default()).unwrap();
        db.put_cf(db.cf_handle("fills").unwrap(), b"new", b"new")
            .unwrap();
        let mut options = Options::default();
        options.set_max_open_files(-1);
        let secondary =
            DB::open_cf_as_secondary(&options, &path, &tmp.path().join("secondary"), ["fills"])
                .unwrap();
        secondary.try_catch_up_with_primary().unwrap();
        assert_eq!(
            secondary
                .get_cf(secondary.cf_handle("fills").unwrap(), b"new")
                .unwrap()
                .unwrap(),
            b"new"
        );
    }
}

#[test]
fn serializes_catch_up_and_multi_cf_scans() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let db = primary(&path, &["fills", "roles"]);
    let families = vec!["fills".to_owned(), "roles".to_owned()];
    for name in &families {
        db.put_cf(db.cf_handle(name).unwrap(), b"key", b"before")
            .unwrap();
        db.flush_cf(db.cf_handle(name).unwrap()).unwrap();
    }
    let pool = pool(&tmp.path().join("readers"));
    let (inside_tx, inside_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let pool_ref = &pool;
        let path_ref = &path;
        let families_ref = &families;
        let first = scope.spawn(move || {
            pool_ref
                .read(path_ref, families_ref, |reader| {
                    let a = reader
                        .get_cf(reader.cf_handle("fills").unwrap(), b"key")?
                        .unwrap();
                    inside_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    let b = reader
                        .get_cf(reader.cf_handle("roles").unwrap(), b"key")?
                        .unwrap();
                    Ok((a, b))
                })
                .unwrap()
        });
        inside_rx.recv().unwrap();
        let mut batch = rocksdb::WriteBatch::default();
        for name in &families {
            batch.put_cf(db.cf_handle(name).unwrap(), b"key", b"after");
        }
        db.write(batch).unwrap();
        for name in &families {
            db.flush_cf(db.cf_handle(name).unwrap()).unwrap();
            db.compact_range_cf(db.cf_handle(name).unwrap(), None::<&[u8]>, None::<&[u8]>);
        }
        let second = scope.spawn(|| rows(&pool, &path, "fills"));
        resume_tx.send(()).unwrap();
        let (a, b) = first.join().unwrap();
        assert_eq!(a, b"before");
        assert_eq!(b, b"before");
        assert_eq!(second.join().unwrap()[b"key".as_slice()], b"after");
    });
    assert_eq!(pool.opens.load(Ordering::Relaxed), 1);
}

#[test]
fn scans_inclusive_overlap_and_preserves_heads_and_missing_families() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let db = primary(&path, &["fills", "roles"]);
    for key in [b"100", b"200", b"300"] {
        db.put_cf(db.cf_handle("fills").unwrap(), key, key).unwrap();
    }
    let pool = pool(&tmp.path().join("readers"));
    let requested = vec!["fills".into(), "roles".into(), "absent".into()];
    let requests = vec![
        ("fills".into(), Some(b"200".to_vec())),
        ("roles".into(), None),
        ("absent".into(), None),
    ];
    let mut found = Vec::new();
    let heads = pool
        .read(&path, &requested, |reader| {
            visit_from(reader, &requests, |_, k, _| {
                found.push(k.to_vec());
                Ok(())
            })
        })
        .unwrap();
    assert_eq!(found, [b"200".to_vec(), b"300".to_vec()]);
    assert_eq!(heads["fills"], Some(b"300".to_vec()));
    assert_eq!(heads["roles"], None);
    assert!(!heads.contains_key("absent"));
}

#[test]
fn rejects_removed_or_replaced_sources_during_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let _db = primary(&path, &["fills"]);
    let pool = pool(&tmp.path().join("readers"));
    let error = pool
        .read(&path, &["fills".into()], |_| {
            std::fs::rename(&path, tmp.path().join("retired"))?;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("source changed"));
    assert!(!path.exists());
    assert!(pool.read(&path, &["fills".into()], |_| Ok(())).is_err());
    assert!(!path.exists());
}

#[test]
fn visitor_failure_does_not_hide_repaired_records_or_reopen_reader() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let db = primary(&path, &["fills"]);
    let pool = pool(&tmp.path().join("readers"));
    assert!(
        pool.read(&path, &["fills".into()], |_| -> Result<()> {
            anyhow::bail!("bad payload")
        })
        .is_err()
    );
    db.put_cf(db.cf_handle("fills").unwrap(), b"fixed", b"ok")
        .unwrap();
    assert_eq!(rows(&pool, &path, "fills")[b"fixed".as_slice()], b"ok");
    assert_eq!(pool.opens.load(Ordering::Relaxed), 1);
}

#[test]
fn secondary_does_not_mutate_primary_files_and_removes_its_scratch() {
    fn files(path: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(path)
            .unwrap()
            .map(|p| {
                let p = p.unwrap().path();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(p).unwrap(),
                )
            })
            .collect()
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let db = primary(&path, &["fills"]);
    db.put_cf(db.cf_handle("fills").unwrap(), b"key", b"value")
        .unwrap();
    let before = files(&path);
    let root = tmp.path().join("readers");
    {
        let pool = pool(&root);
        rows(&pool, &path, "fills");
        rows(&pool, &path, "fills");
        assert_eq!(files(&path), before);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn refuses_scratch_inside_source_before_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("primary");
    let _db = primary(&path, &["fills"]);
    let root = path.join("forbidden");
    let pool = pool(&root);
    assert!(pool.read(&path, &["fills".into()], |_| Ok(())).is_err());
    assert!(!root.exists());
}
