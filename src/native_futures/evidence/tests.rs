use super::*;
use serde_json::json;

fn database(path: &std::path::Path) -> DB {
    let mut options = rocksdb::Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    DB::open_cf(
        &options,
        path,
        ["cn_trading_events", "cn_uniform_orders", "cn_trade_facts"],
    )
    .unwrap()
}
fn source(path: &std::path::Path) -> Source {
    Source {
        id: "account".into(),
        account: "test".into(),
        adapter: "ctp".into(),
        rocksdb_path: path.into(),
        enabled: true,
        sync: None,
    }
}
fn put(db: &DB, sequence: i64, value: &Value) {
    db.put_cf(
        db.cf_handle(EVENTS).unwrap(),
        sequence.to_be_bytes(),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

#[test]
fn current_anchor_requires_the_exact_complete_snapshot_and_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("primary");
    let db = database(&primary);
    let src = source(&primary);
    let now = super::super::now_us();
    put(
        &db,
        1,
        &json!({"sequence":1,"received_us":now-1,"projection_error":null,"value":{"type":"runtime_start"}}),
    );
    put(
        &db,
        2,
        &json!({"sequence":2,"received_us":now,"projection_error":null,"value":{"type":"reconciled","state":{"ready":true,"reason":"","connected":true,"trading_day":"20261117","positions":[],"orders":[]}}}),
    );
    let snapshot = reader::read(&src, &temp.path().join("secondary"), 10, 30).unwrap();
    let anchor = capture_current(&src, &db, &snapshot, 2, "explicit flat account".into()).unwrap();
    validate_anchor(&src, &db, &anchor).unwrap();
    let mut changed = snapshot.clone();
    changed.positions.push(json!({"venue":"SHFE"}));
    assert!(capture_current(&src, &db, &changed, 2, "wrong position".into()).is_err());
    assert!(capture_current(&src, &db, &snapshot, 3, "stale revision".into()).is_err());
    drop(db);
    let reopened = database(&primary);
    validate_anchor(&src, &reopened, &anchor).unwrap();
    let mut wrong = src.clone();
    wrong.id = "other_account".into();
    assert!(validate_anchor(&wrong, &reopened, &anchor).is_err());
    let replacement = database(&temp.path().join("replacement"));
    assert!(validate_anchor(&src, &replacement, &anchor).is_err());
    put(&reopened, 2, &json!({"rewritten":true}));
    assert!(validate_anchor(&src, &reopened, &anchor).is_err());
}

#[test]
fn readonly_reconciliation_proves_holdings_without_enabling_trading() {
    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("primary");
    let db = database(&primary);
    let src = source(&primary);
    let now = super::super::now_us();
    put(
        &db,
        1,
        &json!({"sequence":1,"received_us":now-1,"projection_error":null,"value":{"type":"runtime_start"}}),
    );
    let mut event = json!({"sequence":2,"received_us":now,"projection_error":null,"value":{"type":"reconciled","state":{"ready":false,"connected":true,"reason":"settlement not confirmed; read-only queries remain available","trading_day":"20261118","positions":[],"orders":[]}}});
    put(&db, 2, &event);
    let snapshot = reader::read(&src, &temp.path().join("secondary"), 10, 30).unwrap();
    assert!(!snapshot.snapshot_ready);
    assert_eq!(snapshot.status, "blocked");
    let anchor = capture_current(
        &src,
        &db,
        &snapshot,
        2,
        "verified flat read-only account".into(),
    )
    .unwrap();
    validate_anchor(&src, &db, &anchor).unwrap();
    for status in ["stopped", "disconnected", "recovering", "stale"] {
        let mut invalid = snapshot.clone();
        invalid.status = status.into();
        assert!(capture_current(&src, &db, &invalid, 2, "incomplete account".into()).is_err());
    }
    event["value"]["type"] = json!("state");
    put(&db, 2, &event);
    assert!(
        capture_current(
            &src,
            &db,
            &snapshot,
            2,
            "unready state is not reconciliation".into()
        )
        .is_err()
    );
    event["value"]["type"] = json!("reconciled");
    event["value"]["state"]["connected"] = json!(false);
    put(&db, 2, &event);
    assert!(
        capture_current(
            &src,
            &db,
            &snapshot,
            2,
            "disconnected reconciliation".into()
        )
        .is_err()
    );
}

#[test]
fn live_fill_requires_matching_raw_identity_direction_and_live_origin() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(temp.path());
    let src = source(temp.path());
    let identity = json!({"venue":"SHFE","symbol":"sn2612","trading_day":"20261117","trade_id":"T1","side":"buy"});
    let record = json!({"received_us":10,"fill":{"identity":identity,"system_id":"S1","lots":1,"price":100}});
    let mut normalized = json!({"sequence":2,"value":{"type":"normalized_trade","identity":identity,"origin":"live_report"}});
    let mut raw = json!({"sequence":1,"value":{"type":"ctp_event","event":{"kind":10,"received_us":10,"exchange":"SHFE","symbol":"sn2612","trading_day":"20261117","trade_id":"T1","system_id":"S1","direction":48,"volume":1,"price":100}}});
    put(&db, 1, &raw);
    put(&db, 2, &normalized);
    assert!(live_origin(&src, &db, &record, 2).unwrap());
    normalized["value"]["origin"] = json!("recovery_query");
    put(&db, 2, &normalized);
    assert!(!live_origin(&src, &db, &record, 2).unwrap());
    normalized["value"]["origin"] = json!("live_report");
    put(&db, 2, &normalized);
    raw["value"]["event"]["direction"] = json!(49);
    put(&db, 1, &raw);
    assert!(!live_origin(&src, &db, &record, 2).unwrap());
    raw["value"]["event"]["direction"] = json!(48);
    raw["value"]["event"]["price"] = json!(101);
    put(&db, 1, &raw);
    assert!(!live_origin(&src, &db, &record, 2).unwrap());
}
