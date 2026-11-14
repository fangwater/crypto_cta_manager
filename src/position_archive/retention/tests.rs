use super::*;
use crate::order_config::TargetPosition;
const NOW: i64 = 100 * 86_400_000_000;
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
    assert!(archive.export_chunk(&mut output, NOW).unwrap());
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
    archive.compact_archived().unwrap();
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
    assert!(!reopened.export_chunk(&mut Vec::new(), NOW).unwrap());
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
    archive.export_chunk(&mut bytes, NOW).unwrap();
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
    archive.export_chunk(&mut bytes, NOW).unwrap();
    let first = verify_restore(bytes.as_slice(), &backup.path().join("db")).unwrap();
    assert_eq!(first.count, 0);
    archive.commit_receipt(&first, NOW).unwrap();
    let later = NOW + 86_400_000_000;
    let mut bytes = Vec::new();
    archive.export_chunk(&mut bytes, later).unwrap();
    let second = verify_restore(bytes.as_slice(), &backup.path().join("db")).unwrap();
    assert_eq!(second.previous, Some(first.sha256.clone()));
    let missing_chain = tempfile::tempdir().unwrap();
    assert!(verify_restore(bytes.as_slice(), &missing_chain.path().join("db")).is_err());
    archive.commit_receipt(&second, later).unwrap();
    assert!(archive.commit_receipt(&first, later).is_err());
    assert_eq!(successor((100, u32::MAX)).unwrap(), (101, 0));
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
    let archive = PositionArchive::open(db).unwrap();
    assert!(archive.export_chunk(&mut Vec::new(), NOW).is_err());
    assert_eq!(archive.retention_status().sealed, (1, 0));
    assert!(archive.db.db().get(META).unwrap().is_none());
}
