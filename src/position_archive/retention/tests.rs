use super::*;
use crate::order_config::TargetPosition;
const NOW: i64 = 100 * 86_400_000_000;

#[test]
fn thirty_day_analysis_keeps_three_additional_days_before_reclamation() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-16T04:00:00Z")
        .unwrap()
        .timestamp_micros();
    let safe = date_boundary("2026-10-14").unwrap();
    let too_recent = date_boundary("2026-10-15").unwrap();
    assert!(validate_before(safe, now).is_ok());
    assert!(validate_before(too_recent, now).is_err());
    assert!(validate_before(cutoff(now), now).is_ok());
    assert!(cutoff(now).0 <= now - 33 * 86_400_000_000 - 600_000_000);

    let live = tempfile::tempdir().unwrap();
    let archive = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    assert!(
        archive
            .export_prefix(&mut Vec::new(), too_recent, now)
            .is_err()
    );
    assert_eq!(archive.retention_status().sealed, (1, 0));
    assert!(archive.db.db().get(META).unwrap().is_none());
    assert!(inspect(live.path(), too_recent, now).is_err());
}
fn strategy(ts: i64, qty: f64) -> PositionStrategy {
    PositionStrategy {
        strategy_name: "alpha".into(),
        updated_at_us: ts,
        targets: BTreeMap::from([(
            "BTCUSDT".into(),
            TargetPosition {
                clear: false,
                qty,
                signal: 1,
            },
        )]),
        symbol_order_strategy_overrides: BTreeMap::new(),
    }
}
#[test]
fn verified_backup_precedes_prune_and_restart_preserves_head_and_current() {
    let live = tempfile::tempdir().unwrap();
    let backup = tempfile::tempdir().unwrap();
    let archive = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    archive.db.ensure_column_family("klines_1m").unwrap();
    archive
        .db
        .db()
        .put_cf(
            &archive.db.db().cf_handle("klines_1m").unwrap(),
            b"protected",
            b"candle",
        )
        .unwrap();
    let ts = NOW - 40 * 86_400_000_000;
    let message = archive
        .append_target(
            &strategy(ts, 0.12345678901234567),
            7,
            vec![published_account("source", "binding", 2.0)],
        )
        .unwrap();
    archive
        .append(ts, &strategy(ts, 0.7), vec![], vec![])
        .unwrap();
    let mut output = Vec::new();
    assert!(
        archive
            .export_prefix(&mut output, cutoff(NOW), NOW)
            .unwrap()
    );
    assert_eq!(archive.scan_from(0).unwrap().len(), 2);
    let receipt = verify_restore(output.as_slice(), &backup.path().join("db")).unwrap();
    assert_eq!(
        verify_restore(output.as_slice(), &backup.path().join("db")).unwrap(),
        receipt
    );
    assert_eq!(receipt.count, 2);
    let mut wrong = receipt.clone();
    wrong.sha256 = "bad".into();
    assert!(archive.commit_receipt(&wrong, NOW).is_err());
    wrong = receipt.clone();
    wrong.identity = "another-store".into();
    assert!(archive.commit_receipt(&wrong, NOW).is_err());
    assert_eq!(archive.scan_from(0).unwrap().len(), 2);
    archive.commit_receipt(&receipt, NOW).unwrap();
    archive.commit_receipt(&receipt, NOW).unwrap();
    archive.compact_archived(cutoff(NOW)).unwrap();
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
    assert!(archive.latest().unwrap().is_none());
    archive.enrich_factual_positions(message, vec![]).unwrap();
    assert!(archive.latest().unwrap().is_none());
    assert!(
        archive
            .append(ts, &strategy(ts, 3.0), vec![], vec![])
            .is_err()
    );
    assert_eq!(
        archive.current_target("alpha", 7).unwrap().unwrap().targets["BTCUSDT"].qty,
        0.12345678901234567
    );
    assert_eq!(archive.latest_cursor(), (ts, 1));
    drop(archive);
    let reopened = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    assert_eq!(reopened.latest_cursor(), (ts, 1));
    assert_eq!(reopened.retention_status().last_receipt, Some(receipt));
    assert!(
        !reopened
            .export_prefix(&mut Vec::new(), cutoff(NOW), NOW)
            .unwrap()
    );
    reopened
        .append(NOW, &strategy(NOW, 3.0), vec![], vec![])
        .unwrap();
}
#[test]
fn truncated_corrupt_trailing_and_conflicting_backups_cannot_issue_receipts() {
    let live = tempfile::tempdir().unwrap();
    let backup = tempfile::tempdir().unwrap();
    let archive = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    archive
        .append(100, &strategy(100, 1.0), vec![], vec![])
        .unwrap();
    let mut bytes = Vec::new();
    archive.export_prefix(&mut bytes, cutoff(NOW), NOW).unwrap();
    for length in [0, 15, bytes.len() / 2, bytes.len() - 1] {
        assert!(verify_restore(&bytes[..length], &backup.path().join("truncated")).is_err());
    }
    let mut corrupt = bytes.clone();
    let at = corrupt.len() - 8;
    corrupt[at] ^= 1;
    assert!(verify_restore(corrupt.as_slice(), &backup.path().join("corrupt")).is_err());
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(b"extra");
    assert!(verify_restore(trailing.as_slice(), &backup.path().join("trailing")).is_err());
    let dbpath = backup.path().join("valid");
    verify_restore(bytes.as_slice(), &dbpath).unwrap();
    {
        let db = ManagerDb::open(&dbpath).unwrap();
        let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
        db.db()
            .put_cf(
                &cf,
                manager_db::encode_seq_key(100, 0).unwrap(),
                b"conflict",
            )
            .unwrap();
    }
    assert!(verify_restore(bytes.as_slice(), &dbpath).is_err());
    assert!(verify_restore(bytes.as_slice(), live.path()).is_err());
    assert_eq!(archive.scan_from(0).unwrap().len(), 1);
}
#[test]
fn empty_prefix_advances_only_with_verified_receipt_and_chain_is_enforced() {
    let live = tempfile::tempdir().unwrap();
    let backup = tempfile::tempdir().unwrap();
    let archive = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    let mut bytes = Vec::new();
    archive.export_prefix(&mut bytes, cutoff(NOW), NOW).unwrap();
    let first = verify_restore(bytes.as_slice(), &backup.path().join("db")).unwrap();
    assert_eq!(first.count, 0);
    archive.commit_receipt(&first, NOW).unwrap();
    let later = NOW + 86_400_000_000;
    let mut bytes = Vec::new();
    archive
        .export_prefix(&mut bytes, cutoff(later), later)
        .unwrap();
    let second = verify_restore(bytes.as_slice(), &backup.path().join("db")).unwrap();
    assert_eq!(second.previous, Some(first.sha256.clone()));
    let missing_chain = tempfile::tempdir().unwrap();
    assert!(verify_restore(bytes.as_slice(), &missing_chain.path().join("db")).is_err());
    archive.commit_receipt(&second, later).unwrap();
    assert!(archive.commit_receipt(&first, later).is_err());
}
#[test]
fn owner_socket_refuses_shared_directory_and_occupied_file() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let archive =
        Arc::new(PositionArchive::open(ManagerDb::open(&root.path().join("db")).unwrap()).unwrap());
    let control = root.path().join("control");
    std::fs::create_dir(&control).unwrap();
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(start_socket(archive.clone(), &control.join("socket"), |_| Ok(())).is_err());
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(control.join("socket"), b"must survive").unwrap();
    assert!(start_socket(archive, &control.join("socket"), |_| Ok(())).is_err());
    assert_eq!(
        std::fs::read(control.join("socket")).unwrap(),
        b"must survive"
    );
}

#[test]
fn exec_column_family_blocks_sealing_before_any_mutation() {
    let root = tempfile::tempdir().unwrap();
    let db = ManagerDb::open(root.path()).unwrap();
    db.ensure_column_family("uniform_orders").unwrap();
    assert!(PositionArchive::open(db.clone()).is_err());
    assert!(db.db().get(META).unwrap().is_none());
    assert!(db.db().get(storage::FORMAT_KEY).unwrap().is_none());
}

#[test]
fn manual_date_is_exclusive_and_retained_month_cannot_be_removed() {
    let date = date_boundary("2026-10-01").unwrap();
    assert_eq!(
        date.0,
        chrono::DateTime::parse_from_rfc3339("2026-09-30T16:00:00Z")
            .unwrap()
            .timestamp_micros()
    );
    for value in [
        "2026-1-1",
        "2026-02-30",
        "2026-10-01T00:00:00Z",
        "1969-12-31",
    ] {
        assert!(date_boundary(value).is_err());
    }
    let live = tempfile::tempdir().unwrap();
    let archive = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    assert!(
        archive
            .export_prefix(&mut Vec::new(), (NOW, 0), NOW)
            .is_err()
    );
    assert_eq!(archive.retention_status().sealed, (1, 0));
    assert!(archive.db.db().get(META).unwrap().is_none());
    assert!(archive.compact_archived((200, 0)).is_err());
}

#[test]
fn complete_manual_prefix_is_backed_up_before_one_commit_and_boundary_records_survive() {
    let live = tempfile::tempdir().unwrap();
    let backup = tempfile::tempdir().unwrap();
    let archive = PositionArchive::open(ManagerDb::open(live.path()).unwrap()).unwrap();
    for ts in 100..400 {
        archive
            .append(ts, &strategy(ts, ts as f64), vec![], vec![])
            .unwrap();
    }
    archive
        .append(400, &strategy(400, 400.), vec![], vec![])
        .unwrap();
    archive
        .append(400, &strategy(400, 401.), vec![], vec![])
        .unwrap();
    let mut output = Vec::new();
    archive.export_prefix(&mut output, (400, 0), NOW).unwrap();
    assert_eq!(archive.scan_from(0).unwrap().len(), 302);
    let receipt = verify_restore(output.as_slice(), &backup.path().join("db")).unwrap();
    assert_eq!(receipt.count, 300);
    assert_eq!(receipt.to, (400, 0));
    assert!(receipt.raw_bytes > 16 * 1024);
    archive.commit_receipt(&receipt, NOW).unwrap();
    assert_eq!(archive.retention_floor(), (400, 0));
    assert_eq!(archive.scan_from(400).unwrap().len(), 2);
    assert!(archive.compact_archived((401, 0)).is_err());
    archive.compact_archived((400, 0)).unwrap();
    assert_eq!(archive.scan_from(400).unwrap().len(), 2);
}

#[test]
fn live_owner_holds_write_lock_but_manual_planning_can_open_read_only() {
    let live = tempfile::tempdir().unwrap();
    let db = ManagerDb::open(live.path()).unwrap();
    let archive = PositionArchive::open(db.clone()).unwrap();
    archive
        .append(100, &strategy(100, 1.), vec![], vec![])
        .unwrap();
    assert!(ManagerDb::open(live.path()).is_err());
    let plan = inspect(live.path(), (200, 0), NOW).unwrap();
    assert_eq!(plan["requested_before"], serde_json::json!([200, 0]));
    assert_eq!(plan["floor"], serde_json::json!([1, 0]));
    assert!(db.db().get(META).unwrap().is_none());
    assert_eq!(archive.scan_from(0).unwrap().len(), 1);
}

#[test]
fn manual_socket_exports_backup_then_owner_commits_and_compacts_once() {
    use std::os::unix::net::UnixListener;
    let live = tempfile::tempdir().unwrap();
    let backup = tempfile::tempdir().unwrap();
    let archive =
        Arc::new(PositionArchive::open(ManagerDb::open(&live.path().join("db")).unwrap()).unwrap());
    let before = cutoff(crate::kline::now_us());
    archive
        .append(before.0 - 1, &strategy(before.0 - 1, 1.), vec![], vec![])
        .unwrap();
    archive
        .append(before.0, &strategy(before.0, 2.), vec![], vec![])
        .unwrap();
    let socket = live.path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let owner = archive.clone();
    let serving = std::thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            control::serve_connection(&owner, &mut stream, &|_| Ok(())).unwrap();
        }
    });
    let mut compressed = Vec::new();
    client(
        &socket,
        &Command::Export {
            before_us: before.0,
        },
        &mut compressed,
    )
    .unwrap();
    let receipt = verify_restore(compressed.as_slice(), &backup.path().join("restored")).unwrap();
    assert_eq!(receipt.count, 1);
    let mut status = Vec::new();
    client(&socket, &Command::Status, &mut status).unwrap();
    let status: Reply = serde_json::from_slice(&status).unwrap();
    assert_eq!(status.status.unwrap().floor, (1, 0));
    // New publications after the backup survive the subsequent prefix deletion.
    archive
        .append(before.0 + 1, &strategy(before.0 + 1, 3.), vec![], vec![])
        .unwrap();
    client(&socket, &Command::Commit { receipt }, &mut Vec::new()).unwrap();
    client(
        &socket,
        &Command::Compact {
            before_us: before.0,
        },
        &mut Vec::new(),
    )
    .unwrap();
    serving.join().unwrap();
    assert_eq!(archive.retention_floor(), before);
    assert_eq!(archive.scan_from(before.0).unwrap().len(), 2);
}

#[test]
fn real_record_rehearsal_preserves_retained_inventory_and_never_mutates_source() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let archive = PositionArchive::open(ManagerDb::open(&primary).unwrap()).unwrap();
    let now = crate::kline::now_us();
    let day = 86_400_000_000;
    for (age, qty) in [(60, 1.), (20, 2.), (1, 3.)] {
        let ts = now - age * day;
        archive
            .append(
                ts,
                &strategy(ts, qty),
                vec![],
                vec![published_account("source", "binding", 1.)],
            )
            .unwrap();
    }
    let mut sample = Vec::new();
    rehearsal::sample(&primary, &root.path().join("secondary"), &mut sample).unwrap();
    assert_eq!(archive.retention_status().sealed, (1, 0));
    assert_eq!(archive.retention_floor(), (1, 0));
    assert!(rehearsal::serve(&primary, &root.path().join("bad.sock")).is_err());
    let file = root.path().join("sample.gz");
    std::fs::write(&file, &sample).unwrap();
    let prepared = rehearsal::prepare(&file, &root.path().join("isolated")).unwrap();
    let sampled_receipt: Receipt = serde_json::from_value(prepared["receipt"].clone()).unwrap();
    assert!(archive.commit_receipt(&sampled_receipt, now).is_err());
    let working = std::path::PathBuf::from(prepared["working"].as_str().unwrap());
    let before = rehearsal::proof(&working, now - 30 * day, now).unwrap();
    assert_eq!(before["retained_signals"], serde_json::json!(2));
    assert_eq!(before["source_bindings"], serde_json::json!(1));
    let lab = PositionArchive::open(ManagerDb::open(&working).unwrap()).unwrap();
    let cutoff = (now - 40 * day, 0);
    let mut backup = Vec::new();
    lab.export_prefix(&mut backup, cutoff, now).unwrap();
    let receipt = verify_restore(backup.as_slice(), &root.path().join("verified-backup")).unwrap();
    lab.commit_receipt(&receipt, now).unwrap();
    lab.compact_archived(cutoff).unwrap();
    drop(lab);
    let after = rehearsal::proof(&working, now - 30 * day, now).unwrap();
    assert_eq!(before, after);
    assert_eq!(archive.scan_from(1).unwrap().len(), 3);
    let mut tail = Vec::new();
    rehearsal::sample_with_options(
        &primary,
        &root.path().join("tail-secondary"),
        &mut tail,
        false,
        2,
    )
    .unwrap();
    let tail_path = root.path().join("tail-restored");
    let tail_receipt = verify_restore(tail.as_slice(), &tail_path).unwrap();
    assert_eq!(tail_receipt.count, 2);
    let tail_archive = PositionArchive::open(ManagerDb::open(&tail_path).unwrap()).unwrap();
    assert_eq!(
        tail_archive.scan_from(1).unwrap(),
        archive.scan_from(1).unwrap()[1..]
    );
}

#[test]
fn explicit_json_backup_restore_then_migration_can_continue_the_current_binary_receipt_chain() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("legacy");
    let db = ManagerDb::open(&source).unwrap();
    let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let message = super::super::storage_tests::message(1_700_000_000_000_001, 0.12345678901234567);
    let key = manager_db::encode_seq_key(message.received_at_us, 0).unwrap();
    let json = serde_json::to_vec(&message).unwrap();
    let parsed: PositionUpdateMsg = serde_json::from_slice(&json).unwrap();
    db.db().put_cf(&cf, key, &json).unwrap();
    let mut legacy = Vec::new();
    rehearsal::sample_with_format(&source, &root.path().join("secondary"), &mut legacy, true)
        .unwrap();
    let restored = root.path().join("restored");
    let old = verify_json_restore(legacy.as_slice(), &restored).unwrap();
    assert!(verify_restore(legacy.as_slice(), &root.path().join("wrong-reader")).is_err());
    super::super::migration::migrate_json(&restored, &root.path().join("original-json"), false)
        .unwrap();
    assert!(verify_json_restore(legacy.as_slice(), &restored).is_err());
    let next = super::super::storage_tests::message(message.received_at_us + 10, 0.25);
    let header = Header {
        identity: old.identity.clone(),
        from: old.to,
        to: (next.received_at_us + 1, 0),
        previous: Some(old.sha256.clone()),
        boundary: Boundary::default(),
    };
    let bytes = encode_header(&header).unwrap();
    let key = manager_db::encode_seq_key(next.received_at_us, 0).unwrap();
    let value = codec::encode(&next).unwrap();
    let mut hash = Sha256::new();
    hash.update(&bytes);
    hash_frame(&mut hash, &key, &value);
    let receipt = Receipt {
        identity: header.identity,
        from: header.from,
        to: header.to,
        previous: header.previous,
        count: 1,
        raw_bytes: 20 + value.len() as u64,
        sha256: hex::encode(hash.finalize()),
    };
    let mut out = GzEncoder::new(Vec::new(), Compression::fast());
    out.write_all(MAGIC).unwrap();
    write_blob(&mut out, &bytes).unwrap();
    out.write_all(&key).unwrap();
    write_blob(&mut out, &value).unwrap();
    out.write_all(&[0; 12]).unwrap();
    write_blob(&mut out, &[]).unwrap();
    write_blob(&mut out, &serde_json::to_vec(&receipt).unwrap()).unwrap();
    let chunk = out.finish().unwrap();
    assert_eq!(
        verify_restore(chunk.as_slice(), &restored).unwrap(),
        receipt
    );
    let archive = PositionArchive::open(ManagerDb::open(&restored).unwrap()).unwrap();
    assert_eq!(
        rmp_serde::to_vec(&archive.scan_from(1).unwrap()).unwrap(),
        rmp_serde::to_vec(&vec![parsed, next]).unwrap()
    );
}
