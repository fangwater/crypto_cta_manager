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
        sync: None,
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
    let all_facts = reader::read_cost_facts(&source(&primary), &secondary, "20261112").unwrap();
    assert_eq!(
        all_facts.fills.len(),
        2,
        "cost reconstruction must not inherit the one-row UI limit"
    );
    assert_eq!(all_facts.orders.len(), 1);
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
        mode: crate::runtime_mode::RuntimeMode::CnFutures,
        database: crate::config::DatabaseConfig {
            url_env: "CN_TEST_DB".into(),
            max_connections: 1,
        },
        secondary_root: tmp.path().join("secondary"),
        reference_root: None,
        market_data: None,
        reference_service: None,
        sync: None,
        sync_export: None,
        stale_secs: 30,
        recent_rows: 10,
        source_viewers: Default::default(),
        ctaker_plan_readers: Default::default(),
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
    for market in [
        "DCE:m2701",
        "CZCE:SR701",
        "CFFEX:IF2612",
        "CFFEX:T2612",
        "GFEX:lc2701",
    ] {
        assert!(
            web::validate_test_strategy(&request(market, -2.0)).is_ok(),
            "{market}"
        );
    }
    for market in [
        "DCE:M2701",
        "CZCE:sr701",
        "CZCE:SR2701",
        "CZCE:SR700",
        "CFFEX:if2612",
        "CFFEX:XX2612",
        "GFEX:lc2713",
        "DCE:m2701-C-3000",
        "SHFE:铜2612",
    ] {
        assert!(super::validate_market(market).is_err(), "{market}");
    }
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

#[test]
fn ctp_sources_are_explicit_and_reject_unknown_adapters() {
    let temp = tempfile::tempdir().unwrap();
    let mut config: Config =
        toml::from_str(include_str!("../../config/cn-futures-manager.example.toml")).unwrap();
    config.secondary_root = temp.path().join("secondary");
    config.sources[0] = source(&temp.path().join("execution"));
    config.sources[0].adapter = "ctp".into();
    assert!(config.validate().is_ok());
    for adapter in ["CTP", "unknown", ""] {
        config.sources[0].adapter = adapter.into();
        assert!(config.validate().is_err());
    }
}

#[test]
fn ctp_reads_six_venues_without_changing_native_identities_offsets_or_unknown_fees() {
    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("execution");
    let db = db(&primary);
    let mut source = source(&primary);
    source.adapter = "ctp".into();
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/ctp_native_records.json")).unwrap();
    let mut state = fixture["state"].clone();
    state["received_us"] = json!(super::now_us());
    let mut positions = vec![];
    for (venue, symbol) in [
        ("SHFE", "sn2612"),
        ("INE", "sc2701"),
        ("DCE", "m2701"),
        ("CZCE", "SR701"),
        ("CFFEX", "IF2612"),
        ("GFEX", "lc2701"),
    ] {
        let mut position = state["value"]["state"]["positions"][0].clone();
        position["venue"] = json!(venue);
        position["symbol"] = json!(symbol);
        positions.push(position);
        let mut order = fixture["order"].clone();
        order["identity"]["venue"] = json!(venue);
        order["order"]["request"]["venue"] = json!(venue);
        order["order"]["request"]["symbol"] = json!(symbol);
        put(&db, "cn_uniform_orders", venue, &order);
        let mut fill = fixture["fill"].clone();
        fill["fill"]["identity"]["venue"] = json!(venue);
        fill["fill"]["identity"]["symbol"] = json!(symbol);
        put(&db, "cn_trade_facts", venue, &fill);
    }
    state["value"]["state"]["positions"] = json!(positions);
    put(&db, "cn_trading_events", "001", &state);
    let mut other_session = fixture["order"].clone();
    other_session["identity"]["native"]["session"] = json!("ctp:7:12");
    other_session["order"]["key"]["session"] = json!("ctp:7:12");
    put(&db, "cn_uniform_orders", "other-session", &other_session);
    let view = reader::read(&source, &temp.path().join("secondary"), 20, 30).unwrap();
    assert_eq!(view.status, "observing");
    assert_eq!(view.positions, positions);
    assert_eq!(view.total_orders, 7);
    assert_eq!(view.total_fills, 6);
    assert_eq!(view.trading_day.as_deref(), Some("20261117"));
    for fill in &view.fills {
        assert!(fill["commission"].is_null());
        assert!(fill["commission_currency"].is_null());
        assert!(fill["native_time"].is_null());
        assert_eq!(
            fill["fill"]["order_key"],
            fixture["fill"]["fill"]["order_key"]
        );
        assert_eq!(fill["fill"]["identity"]["trade_id"], "000000008888");
        assert_eq!(fill["fill"]["system_id"], "000000001234");
        assert_eq!(fill["fill"]["offset"], "close_yesterday");
        assert_eq!(fill["fill"]["lots"], 2);
    }
    assert!(
        view.orders
            .iter()
            .all(|order| order.phase == OrderPhase::Filled)
    );
    assert_eq!(
        view.orders
            .iter()
            .filter(|order| order.record["identity"]["native"]["session"] == "ctp:7:12")
            .count(),
        1
    );
    let facts =
        reader::read_cost_facts(&source, &temp.path().join("cost-secondary"), "20261117").unwrap();
    assert_eq!(facts.fills.len(), 6);
    assert_eq!(facts.orders.len(), 7);
}

#[test]
fn ctp_connection_recovery_and_query_events_follow_the_ctp_contract() {
    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("execution");
    let db = db(&primary);
    let mut source = source(&primary);
    source.adapter = "ctp".into();
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/ctp_native_records.json")).unwrap();
    // 14 is QUERY_TRADE; 16/error=90 is a retriable response, not Yida fatal.
    for (index, (value, expected)) in [
        (json!({"type":"runtime_start"}), "recovering"),
        (fixture["state"]["value"].clone(), "observing"),
        (json!({"type":"ctp_event","event":{"kind":14}}), "observing"),
        (
            json!({"type":"ctp_event","event":{"kind":16,"error_code":90}}),
            "observing",
        ),
        (
            json!({"type":"ctp_event","event":{"kind":2}}),
            "disconnected",
        ),
        (json!({"type":"ctp_event","event":{"kind":1}}), "recovering"),
        (
            json!({"type":"recovery_checkpoint","state":fixture["state"]["value"]["state"]}),
            "recovering",
        ),
        (fixture["state"]["value"].clone(), "observing"),
        (
            json!({"type":"restart_required","reason":"transport or control response timeout"}),
            "blocked",
        ),
        (json!({"type":"runtime_stopped"}), "stopped"),
    ]
    .into_iter()
    .enumerate()
    {
        put(
            &db,
            "cn_trading_events",
            &format!("{index:020}"),
            &json!({"sequence":index + 1,"received_us":super::now_us(),"projection_error":null,"value":value}),
        );
        let view = reader::read(&source, &temp.path().join("secondary"), 10, 30).unwrap();
        assert_eq!(view.status, expected, "event {index}");
        if index == 6 {
            assert_eq!(view.state_observed_us, last_reconciled_time(&db));
        }
        if index == 8 {
            assert!(view.reason.contains("timeout"));
        }
    }
}

fn last_reconciled_time(db: &DB) -> Option<i64> {
    let bytes = db
        .get_cf(
            db.cf_handle("cn_trading_events").unwrap(),
            format!("{:020}", 1),
        )
        .unwrap()
        .unwrap();
    serde_json::from_slice::<Value>(&bytes).unwrap()["received_us"].as_i64()
}

#[test]
fn cached_observation_matches_cold_reads_and_updates_only_changed_evidence_and_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let source = source(&primary);
    let f = fixture();
    let mut state = f["state"].clone();
    state["received_us"] = json!(super::now_us());
    put(&db, "cn_trading_events", "001", &state);
    put(&db, "cn_uniform_orders", "one", &f["order"]);
    put(&db, "cn_trade_facts", "one", &f["fill"]);
    let pool =
        crate::rocks_source::ReaderPool::new(&tmp.path().join("pooled"), &[&primary]).unwrap();
    let mut cache = reader::Cache::default();
    let mut read =
        || reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30)).unwrap();
    let first = read();
    assert_eq!(first.status, "observing");
    assert_eq!(read().total_fills, 1);
    put(
        &db,
        "cn_trading_events",
        "002",
        &json!({"sequence":101,"received_us":super::now_us(),"value":{"type":"market_tick","error":7,"reason":false,"payload":[1,2,3]}}),
    );
    let tick = read();
    assert_eq!(tick.status, "observing");
    assert_eq!(tick.total_orders, 1);
    let mut fill = f["fill"].clone();
    fill["commission"] = json!(123.5);
    put(&db, "cn_trade_facts", "one", &fill);
    assert_eq!(read().fills[0]["commission"], 123.5);
    put(
        &db,
        "cn_trading_events",
        "003",
        &json!({"sequence":102,"received_us":super::now_us(),"value":{"type":"yida_event","event":{"kind":2,"replay":0}}}),
    );
    let cached = read();
    let cold = reader::read(&source, &tmp.path().join("cold"), 10, 30).unwrap();
    let mut a = serde_json::to_value(cached).unwrap();
    let mut b = serde_json::to_value(cold).unwrap();
    a["read_at_us"] = json!(0);
    b["read_at_us"] = json!(0);
    assert_eq!(a, b);
}

#[test]
fn cached_snapshot_lag_includes_fills_outside_recent_rows_and_clears_after_a_new_state() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let source = source(&primary);
    let f = fixture();
    let mut state = f["state"].clone();
    state["received_us"] = json!(super::now_us());
    put(&db, "cn_trading_events", "001", &state);
    let mut late = f["fill"].clone();
    late["evidence_seq"] = json!(1000);
    late["received_us"] = json!(1);
    put(&db, "cn_trade_facts", "late", &late);
    let mut recent = f["fill"].clone();
    recent["received_us"] = json!(2);
    recent["fill"]["identity"]["trade_id"] = json!("second");
    put(&db, "cn_trade_facts", "recent", &recent);
    let pool =
        crate::rocks_source::ReaderPool::new(&tmp.path().join("pooled"), &[&primary]).unwrap();
    let mut cache = reader::Cache::default();
    let mut read = || reader::with_db(&pool, &source, |db| cache.read(&source, db, 1, 30)).unwrap();
    let first = read();
    assert_eq!(first.fills.len(), 1);
    assert!(first.positions_may_lag);
    state["sequence"] = json!(500);
    state["received_us"] = json!(super::now_us());
    put(&db, "cn_trading_events", "002", &state);
    assert!(read().positions_may_lag);
    state["sequence"] = json!(1001);
    put(&db, "cn_trading_events", "003", &state);
    assert!(!read().positions_may_lag);
}

#[test]
fn cached_observation_rebuilds_after_old_evidence_updates_and_directory_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let source = source(&primary);
    let mut state = fixture()["state"].clone();
    state["received_us"] = json!(super::now_us());
    put(&db, "cn_trading_events", "001", &state);
    let pool =
        crate::rocks_source::ReaderPool::new(&tmp.path().join("pooled"), &[&primary]).unwrap();
    let mut cache = reader::Cache::default();
    let mut read =
        || reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30)).unwrap();
    assert!(read().snapshot_ready);
    state["value"]["state"]["ready"] = json!(false);
    put(&db, "cn_trading_events", "001", &state);
    put(
        &db,
        "cn_trading_events",
        "002",
        &json!({"sequence":101,"received_us":super::now_us(),"value":{"type":"market_tick"}}),
    );
    assert!(!read().snapshot_ready);
    drop(db);
    std::fs::rename(&primary, tmp.path().join("old-exec")).unwrap();
    let fresh = super::tests::db(&primary);
    state["value"]["state"]["positions"] = json!([]);
    state["value"]["state"]["trading_day"] = json!("20261117");
    put(&fresh, "cn_trading_events", "001", &state);
    let next = read();
    assert_eq!(next.trading_day.as_deref(), Some("20261117"));
    assert!(next.positions.is_empty());
}

#[test]
fn readonly_reconciled_inventory_remains_a_historical_anchor_after_later_execution_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let source = source(&primary);
    let mut complete = fixture()["state"].clone();
    complete["sequence"] = json!(100);
    complete["value"]["type"] = json!("reconciled");
    complete["value"]["state"]["ready"] = json!(false);
    put(&db, "cn_trading_events", "001", &complete);
    let mut blocked = complete.clone();
    blocked["sequence"] = json!(101);
    blocked["received_us"] = json!(complete["received_us"].as_i64().unwrap() + 1);
    blocked["value"]["type"] = json!("state");
    put(&db, "cn_trading_events", "002", &blocked);
    let pool =
        crate::rocks_source::ReaderPool::new(&tmp.path().join("pooled"), &[&primary]).unwrap();
    let mut cache = reader::Cache::default();
    let read = reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30)).unwrap();
    assert!(!read.snapshot_ready);
    assert_eq!(read.state_evidence_sequence, Some(101));
    assert_eq!(read.nav_anchor_evidence_sequence, Some(100));
    blocked["sequence"] = json!(102);
    put(&db, "cn_trading_events", "003", &blocked);
    let read = reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30)).unwrap();
    assert_eq!(
        read.nav_anchor_observed_us,
        complete["received_us"].as_i64()
    );
    assert_eq!(read.nav_anchor_evidence_sequence, Some(100));
}
