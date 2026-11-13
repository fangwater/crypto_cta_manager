use super::{
    config::{Config, Source},
    reader,
    state::{self, OrderPhase},
};
use rocksdb::{ColumnFamilyDescriptor, DB, Options};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/yida_native_records.json"
    ))
    .unwrap()
}
fn source(path: &std::path::Path) -> Source {
    Source {
        id: "sim01".into(),
        account: "仿真".into(),
        adapter: "yida".into(),
        rocksdb_path: path.to_owned(),
        enabled: true,
    }
}
fn db(path: &std::path::Path) -> DB {
    let mut o = Options::default();
    o.create_if_missing(true);
    o.create_missing_column_families(true);
    DB::open_cf_descriptors(
        &o,
        path,
        ["cn_trading_events", "cn_uniform_orders", "cn_trade_facts"]
            .into_iter()
            .map(|cf| ColumnFamilyDescriptor::new(cf, Options::default())),
    )
    .unwrap()
}
fn put(db: &DB, cf: &str, key: &str, value: &Value) {
    db.put_cf(
        db.cf_handle(cf).unwrap(),
        key,
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

#[test]
fn distinguishes_submission_cancellation_and_factual_settlement() {
    let mut record = fixture()["order"].clone();
    record["order"]["status"] = json!("working");
    record["order"]["filled"] = json!(0);
    record["order"]["reported_filled"] = json!(0);
    record["order"]["cancelled"] = json!(0);
    record["send_state"] = json!("submitted_locally");
    record["order"]["cancel_pending"] = json!(true);
    assert_eq!(
        state::observe(record.clone()).unwrap().phase,
        OrderPhase::CancelPending
    );
    record["send_state"] = json!("uncertain");
    assert_eq!(
        state::observe(record.clone()).unwrap().phase,
        OrderPhase::Uncertain
    );
    record["send_state"] = json!("observed");
    record["order"]["status"] = json!("filled");
    record["order"]["reported_filled"] = json!(1);
    let before = state::observe(record.clone()).unwrap();
    assert_eq!(before.phase, OrderPhase::Settling);
    assert!(before.exchange_terminal && !before.facts_complete);
    record["order"]["filled"] = json!(1);
    assert_eq!(state::observe(record).unwrap().phase, OrderPhase::Filled);
}

#[test]
fn partial_cancel_waits_for_actual_fills_and_never_invents_the_remaining_lot() {
    let mut record = fixture()["order"].clone();
    record["order"]["request"]["lots"] = json!(2);
    record["order"]["status"] = json!("cancelled");
    record["order"]["filled"] = json!(0);
    record["order"]["reported_filled"] = json!(1);
    record["order"]["cancelled"] = json!(1);
    assert_eq!(
        state::observe(record.clone()).unwrap().phase,
        OrderPhase::Settling
    );
    record["order"]["filled"] = json!(1);
    assert_eq!(
        state::observe(record.clone()).unwrap().phase,
        OrderPhase::Cancelled
    );
    record["order"]["filled"] = json!(2);
    assert!(state::observe(record).is_err());
}

#[test]
fn live_primary_remains_writable_and_late_fills_mark_snapshot_lag() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let secondary = tmp.path().join("secondary");
    let db = db(&primary);
    let f = fixture();
    put(&db, "cn_uniform_orders", "one", &f["order"]);
    put(&db, "cn_trade_facts", "one", &f["fill"]);
    let mut event = f["state"].clone();
    event["sequence"] = json!(100);
    event["received_us"] = json!(super::now_us());
    put(&db, "cn_trading_events", "00000000000000000100", &event);
    let view = reader::read(&source(&primary), &secondary, 100, 30).unwrap();
    assert_eq!(view.total_orders, 1);
    assert_eq!(view.total_fills, 1);
    assert_eq!(view.status, "observing");
    assert!(view.reported_can_trade.is_none());
    assert_eq!(
        view.positions,
        f["state"]["value"]["state"]["positions"]
            .as_array()
            .unwrap()
            .clone()
    );
    assert!(!view.positions_may_lag);
    let mut fill = f["fill"].clone();
    fill["evidence_seq"] = json!(101);
    fill["fill"]["identity"]["trade_id"] = json!("18446744073709551614");
    put(&db, "cn_trade_facts", "late", &fill);
    let after = reader::read(&source(&primary), &secondary, 1, 30).unwrap();
    assert_eq!(after.total_fills, 2);
    assert_eq!(after.fills.len(), 1);
    assert!(after.positions_may_lag);
    assert_eq!(after.positions, view.positions);
    assert!(primary.join("CURRENT").exists());
}

#[test]
fn new_runtime_and_stopped_process_do_not_claim_current_trading_readiness() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let f = fixture();
    put(&db, "cn_trading_events", "001", &f["state"]);
    put(
        &db,
        "cn_trading_events",
        "002",
        &json!({"sequence":1000,"received_us":super::now_us(),"projection_error":null,"value":{"type":"runtime_start","execute":false}}),
    );
    let view = reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).unwrap();
    assert_eq!(view.status, "recovering");
    put(
        &db,
        "cn_trading_events",
        "003",
        &json!({"sequence":1001,"received_us":super::now_us(),"projection_error":null,"value":{"type":"runtime_stopped"}}),
    );
    assert_eq!(
        reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30)
            .unwrap()
            .status,
        "stopped"
    );
}

#[test]
fn preserves_market_group_and_64_bit_order_identities() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let f = fixture();
    for (key, venue, group) in [("one", "SHFE", 1), ("two", "SHFE", 2), ("three", "INE", 1)] {
        let mut r = f["order"].clone();
        r["identity"]["venue"] = json!(venue);
        r["identity"]["native"]["session"] = json!(format!("yida:20261112:group:{group}"));
        r["order"]["system_id"] = json!("18446744073709551614");
        put(&db, "cn_uniform_orders", key, &r);
    }
    let s = reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).unwrap();
    assert_eq!(s.orders.len(), 3);
    assert!(
        s.orders
            .iter()
            .all(|o| o.record["order"]["system_id"] == "18446744073709551614")
    );
}

#[test]
fn malformed_evidence_and_projection_faults_are_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let mut event = fixture()["state"].clone();
    event["projection_error"] = json!("conflicting trade fact");
    put(&db, "cn_trading_events", "001", &event);
    let s = reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).unwrap();
    assert_eq!(s.projection_errors, 1);
    assert_eq!(s.status, "blocked");
    db.put_cf(
        db.cf_handle("cn_trading_events").unwrap(),
        "002",
        b"bad-json",
    )
    .unwrap();
    assert!(reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).is_err());
}

#[test]
fn rejects_overlapping_stores_before_creating_a_secondary() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let _db = db(&primary);
    let nested = primary.join("must-not-create");
    assert!(reader::read(&source(&primary), &nested, 5, 30).is_err());
    assert!(!nested.exists());
    let mut config = Config {
        database: crate::config::DatabaseConfig {
            url_env: "CN_TEST_DB".into(),
            max_connections: 1,
        },
        secondary_root: tmp.path().join("secondary"),
        stale_secs: 30,
        recent_rows: 10,
        source_viewers: Default::default(),
        sources: vec![source(&primary)],
    };
    assert!(config.validate().is_ok());
    config.sources.push(source(&primary));
    assert!(config.validate().is_err());
}

#[test]
fn native_strategy_targets_keep_contract_case_and_integer_lots() {
    let request = |target: &str, qty: f64| {
        serde_json::from_value(
            json!({"strategy_name":"alpha","targets":{target:{"qty":qty,"signal":0}}}),
        )
        .unwrap()
    };
    assert!(web::validate_test_strategy(&request("SHFE:cu2612", 1.0)).is_ok());
    assert!(web::validate_test_strategy(&request("INE:sc2612P640", -1.0)).is_ok());
    assert!(web::validate_test_strategy(&request("SHFE:cu2612", 0.5)).is_err());
    assert!(web::validate_test_strategy(&request("SHFE:CU2612", 1.0)).is_err());
    assert!(web::validate_test_strategy(&request("binance-futures:BTCUSDT", 1.0)).is_err());
    assert!(super::validate_market("SHFE:cu2613").is_err());
}

use super::web;

#[test]
fn later_block_disconnect_and_transport_events_override_a_ready_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    put(&db, "cn_trading_events", "001", &fixture()["state"]);
    put(
        &db,
        "cn_trading_events",
        "002",
        &json!({"sequence":1000,"received_us":super::now_us(),"projection_error":null,"value":{"type":"trading_blocked","error":"cancel response unknown; reservation retained"}}),
    );
    let view = reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).unwrap();
    assert!(view.snapshot_ready);
    assert_eq!(view.status, "blocked");
    assert!(view.reason.contains("reservation retained"));
    put(
        &db,
        "cn_trading_events",
        "003",
        &json!({"sequence":1001,"received_us":super::now_us(),"projection_error":null,"value":{"type":"yida_event","event":{"kind":2,"replay":0}}}),
    );
    assert_eq!(
        reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30)
            .unwrap()
            .status,
        "disconnected"
    );
    let mut state = fixture()["state"].clone();
    state["sequence"] = json!(1002);
    state["received_us"] = json!(super::now_us());
    put(&db, "cn_trading_events", "004", &state);
    put(
        &db,
        "cn_trading_events",
        "005",
        &json!({"sequence":1003,"received_us":super::now_us(),"projection_error":null,"value":{"type":"transport_status","ready":false}}),
    );
    let view = reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).unwrap();
    assert_eq!(view.status, "transport_unavailable");
    assert_eq!(view.transport_ready, Some(false));
}
