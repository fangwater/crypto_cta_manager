use super::{
    config::{Config, Source},
    nav::{Offset, Side},
    netting::UNALLOCATED,
    reader,
    records::{self, fixture},
    state::{self, OrderPhase},
};
use rocksdb::DB;
use serde_json::json;

const DAY: &str = "20261112";
const BASE: i64 = 1_794_877_200_000_000;

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
    fixture::open(path)
}
fn order(market: &str, session: &str, order_ref: &str) -> fixture::Order {
    fixture::Order::new(
        market,
        DAY,
        session,
        order_ref,
        Side::Sell,
        Offset::CloseYesterday,
        2,
        6000.0,
    )
}
fn yida() -> fixture::Order {
    order("SHFE:cu2612", "yida:20261112:group:1", "10")
}
fn fill(trade_id: &str, lots: i64, commission: Option<f64>) -> fixture::Fill {
    fixture::Fill {
        trade_id: trade_id.into(),
        lots,
        price: 6000.0,
        commission,
        order_id: 1234,
    }
}
fn lifecycle(order: &fixture::Order, status: u8, at: i64) -> records::Order {
    let records::Uniform::Order(order) = records::decode_uniform(
        records::key(at).as_bytes(),
        &order.lifecycle(status, "exec", BASE, at),
    )
    .unwrap() else {
        panic!("lifecycle record")
    };
    order
}

#[test]
fn cost_window_uses_inclusive_receipt_bounds_and_joins_orders_by_client_order_id() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let placed = yida();
    let other = order("SHFE:cu2612", "another-session", "10");
    fixture::put_order(
        &db,
        BASE,
        &placed.lifecycle(fixture::NEW, "exec", BASE, BASE),
    );
    fixture::put_order(
        &db,
        BASE + 1,
        &other.lifecycle(fixture::NEW, "exec", BASE, BASE + 1),
    );
    fixture::put_trade(
        &db,
        BASE + 101,
        &placed.trade(&fill("in-range", 1, None), 1, None, BASE + 101),
    );
    fixture::put_trade(
        &db,
        BASE + 102,
        &placed.trade(&fill("outside", 1, None), 2, None, BASE + 102),
    );
    // The latest lifecycle record is the order's current state.
    fixture::put_order(
        &db,
        BASE + 103,
        &placed.lifecycle(fixture::FILLED, "exec", BASE, BASE + 103),
    );

    let window = reader::read_cost_window(&source(&primary), &db, BASE + 101, BASE + 101).unwrap();
    assert_eq!(window.days.len(), 1);
    let facts = &window.days[DAY];
    assert_eq!(facts.fills.len(), 1);
    assert_eq!(facts.fills[0].trade_id, "in-range");
    assert_eq!(facts.orders.len(), 1);
    let joined = &facts.orders[0];
    assert_eq!(joined.order.order.session, "yida:20261112:group:1");
    assert_eq!(joined.order.key_us, BASE + 103);
    // Only the window's own fills join; the order still awaits its other lot.
    assert_eq!(joined.filled_lots, 1);
    assert_eq!(joined.phase, OrderPhase::Settling);

    let day = reader::read_cost_facts_from_db(&source(&primary), &db, DAY).unwrap();
    assert_eq!(day.fills.len(), 2);
    assert_eq!(day.orders.len(), 2);
    assert!(
        day.orders
            .iter()
            .any(|o| o.order.order.session == "yida:20261112:group:1"
                && o.phase == OrderPhase::Filled)
    );
}

#[test]
fn order_phase_joins_factual_fills_and_never_invents_lots() {
    let placed = yida();
    let observe =
        |status, filled| state::observe(lifecycle(&placed, status, BASE), filled).unwrap();
    assert_eq!(observe(fixture::NEW, 0).phase, OrderPhase::Working);
    assert_eq!(observe(fixture::NEW, 1).phase, OrderPhase::PartiallyFilled);
    assert_eq!(
        observe(fixture::PARTIALLY_FILLED, 1).phase,
        OrderPhase::PartiallyFilled
    );
    let settling = observe(fixture::FILLED, 1);
    assert_eq!(settling.phase, OrderPhase::Settling);
    assert!(settling.exchange_terminal && !settling.facts_complete);
    assert_eq!(observe(fixture::FILLED, 2).phase, OrderPhase::Filled);
    assert_eq!(observe(fixture::CANCELED, 1).phase, OrderPhase::Cancelled);
    assert_eq!(observe(fixture::EXPIRED, 0).phase, OrderPhase::Rejected);
    assert!(state::observe(lifecycle(&placed, fixture::FILLED, BASE), 3).is_err());
}

#[test]
fn live_primary_remains_writable_and_recent_rows_do_not_limit_cost_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let secondary = tmp.path().join("secondary");
    let db = db(&primary);
    let placed = yida();
    let first = fill("000000008888", 1, Some(1.5));
    fixture::put_order(
        &db,
        BASE,
        &placed.lifecycle(fixture::NEW, "exec", BASE, BASE),
    );
    fixture::put_trade(&db, BASE + 1, &placed.trade(&first, 1, None, BASE + 1));
    fixture::put_order(
        &db,
        BASE + 1,
        &placed.share(
            &first,
            UNALLOCATED,
            1,
            Some(1.5),
            fixture::PARTIALLY_FILLED,
            BASE + 1,
        ),
    );
    let view = reader::read(&source(&primary), &secondary, 100, 30).unwrap();
    assert_eq!(view.total_orders, 1, "fill shares are not order events");
    assert_eq!(view.total_fills, 1);
    assert_eq!(view.last_record_us, Some(BASE + 1));
    assert!(
        view.through_us
            .is_some_and(|at| at >= view.read_at_us - 1_000_000)
    );
    assert_eq!(view.orders[0].phase, OrderPhase::PartiallyFilled);
    assert_eq!(view.fills[0].commission_cny, Some(1.5));

    let late = fill("18446744073709551614", 1, None);
    fixture::put_trade(&db, BASE + 2, &placed.trade(&late, 2, None, BASE + 2));
    let after = reader::read(&source(&primary), &secondary, 1, 30).unwrap();
    assert_eq!(after.total_fills, 2);
    assert_eq!(after.fills.len(), 1);
    assert_eq!(after.fills[0].trade_id, "18446744073709551614");
    assert_eq!(after.orders[0].filled_lots, 2);
    let all_facts = reader::read_cost_facts(&source(&primary), &secondary, DAY).unwrap();
    assert_eq!(
        all_facts.fills.len(),
        2,
        "cost reconstruction must not inherit the one-row UI limit"
    );
    assert_eq!(all_facts.orders.len(), 1);
    assert!(primary.join("CURRENT").exists());
}

#[test]
fn preserves_market_group_and_native_order_identities() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    for (n, (market, group)) in [("SHFE:cu2612", 1), ("SHFE:cu2612", 2), ("INE:sc2701", 1)]
        .into_iter()
        .enumerate()
    {
        let placed = order(
            market,
            &format!("yida:20261112:group:{group}"),
            "18446744073709551614",
        );
        fixture::put_order(
            &db,
            BASE + n as i64,
            &placed.lifecycle(fixture::NEW, "exec", BASE, BASE + n as i64),
        );
    }
    let s = reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).unwrap();
    assert_eq!(s.orders.len(), 3);
    assert!(
        s.orders
            .iter()
            .all(|o| o.order.order.order_ref == "18446744073709551614")
    );
    let ids: std::collections::BTreeSet<_> =
        s.orders.iter().map(|o| o.order.client_order_id).collect();
    assert_eq!(ids.len(), 3);
}

#[test]
fn malformed_records_and_other_store_layouts_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    db.put_cf(
        db.cf_handle(records::TRADES).unwrap(),
        records::key(BASE),
        b"{\"json\":true}",
    )
    .unwrap();
    assert!(reader::read(&source(&primary), &tmp.path().join("secondary"), 5, 30).is_err());

    let legacy = tmp.path().join("legacy");
    let mut options = rocksdb::Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    drop(
        DB::open_cf(
            &options,
            &legacy,
            ["cn_trading_events", records::ORDERS, records::TRADES],
        )
        .unwrap(),
    );
    assert!(
        reader::read(
            &source(&legacy),
            &tmp.path().join("legacy-secondary"),
            5,
            30
        )
        .is_err()
    );
    let missing = tmp.path().join("missing");
    drop(DB::open_cf(&options, &missing, [records::ORDERS]).unwrap());
    assert!(
        reader::read(
            &source(&missing),
            &tmp.path().join("missing-secondary"),
            5,
            30
        )
        .is_err()
    );
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
        execution_monitor_urls: Default::default(),
        exec_plan_readers: Default::default(),
        sources: vec![source(&primary)],
    };
    assert!(config.validate().is_ok());
    let id = config.sources[0].id.clone();
    config
        .execution_monitor_urls
        .insert(id.clone(), "javascript:alert(1)".into());
    assert!(config.validate().is_err());
    config
        .execution_monitor_urls
        .insert(id, "https://viz.example.test/?source=paper".into());
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
    let markets = [
        "SHFE:sn2612",
        "INE:sc2701",
        "DCE:m2701",
        "CZCE:SR701",
        "CFFEX:IF2612",
        "GFEX:lc2701",
    ];
    for (n, market) in markets.into_iter().enumerate() {
        let at = BASE + 10 * n as i64;
        let placed = order(market, "ctp:7:11", "000000000042");
        let fact = fill("000000008888", 2, None);
        fixture::put_order(&db, at, &placed.lifecycle(fixture::NEW, "observed", at, at));
        fixture::put_trade(
            &db,
            at + 1,
            &placed.trade(&fact, 2, Some(fixture::FILLED), at + 1),
        );
        fixture::put_order(
            &db,
            at + 2,
            &placed.lifecycle(fixture::FILLED, "observed", at, at + 2),
        );
    }
    let other = order("SHFE:sn2612", "ctp:7:12", "000000000042");
    fixture::put_order(
        &db,
        BASE + 100,
        &other.lifecycle(fixture::NEW, "observed", BASE, BASE + 100),
    );
    let view = reader::read(&source, &temp.path().join("secondary"), 20, 30).unwrap();
    assert_eq!(view.total_orders, 13);
    assert_eq!(view.total_fills, 6);
    assert_eq!(view.orders.len(), 7);
    for fill in &view.fills {
        assert!(fill.commission_cny.is_none());
        assert_eq!(fill.order.session, "ctp:7:11");
        assert_eq!(fill.order.order_ref, "000000000042");
        assert_eq!(fill.trade_id, "000000008888");
        assert_eq!(fill.order_id, 1234);
        assert_eq!(fill.offset, Offset::CloseYesterday);
        assert_eq!(fill.lots, 2);
    }
    assert_eq!(
        view.fills
            .iter()
            .map(|fill| fill.market())
            .collect::<std::collections::BTreeSet<_>>(),
        markets.iter().map(|m| m.to_string()).collect()
    );
    assert_eq!(
        view.orders
            .iter()
            .filter(|order| order.phase == OrderPhase::Filled)
            .count(),
        6
    );
    assert_eq!(
        view.orders
            .iter()
            .filter(|order| order.order.order.session == "ctp:7:12")
            .count(),
        1
    );
    let facts = reader::read_cost_facts(&source, &temp.path().join("cost-secondary"), DAY).unwrap();
    assert_eq!(facts.fills.len(), 6);
    assert_eq!(facts.orders.len(), 7);
}

#[test]
fn cached_observation_matches_cold_reads_and_decodes_only_appended_records() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let source = source(&primary);
    let placed = yida();
    fixture::put_order(
        &db,
        BASE,
        &placed.lifecycle(fixture::NEW, "exec", BASE, BASE),
    );
    fixture::put_trade(
        &db,
        BASE + 1,
        &placed.trade(&fill("one", 1, None), 1, None, BASE + 1),
    );
    let pool =
        crate::rocks_source::ReaderPool::new(&tmp.path().join("pooled"), &[&primary]).unwrap();
    let mut cache = reader::Cache::default();
    let mut read =
        || reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30)).unwrap();
    let first = read();
    assert_eq!((first.total_orders, first.total_fills), (1, 1));
    let unchanged = read();
    assert_eq!(unchanged.last_record_us, first.last_record_us);
    fixture::put_trade(
        &db,
        BASE + 2,
        &placed.trade(&fill("two", 1, Some(123.5)), 2, None, BASE + 2),
    );
    fixture::put_order(
        &db,
        BASE + 3,
        &placed.lifecycle(fixture::FILLED, "exec", BASE, BASE + 3),
    );
    let appended = read();
    assert_eq!((appended.total_orders, appended.total_fills), (2, 2));
    assert_eq!(appended.fills[0].commission_cny, Some(123.5));
    assert_eq!(appended.orders[0].phase, OrderPhase::Filled);
    let cold = reader::read(&source, &tmp.path().join("cold"), 10, 30).unwrap();
    let mut a = serde_json::to_value(read()).unwrap();
    let mut b = serde_json::to_value(cold).unwrap();
    for value in [&mut a, &mut b] {
        value["read_at_us"] = json!(0);
        value["through_us"] = json!(0);
    }
    assert_eq!(a, b);
}

#[test]
fn cached_observation_rebuilds_after_directory_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("exec");
    let db = db(&primary);
    let source = source(&primary);
    let placed = yida();
    fixture::put_order(
        &db,
        BASE,
        &placed.lifecycle(fixture::NEW, "exec", BASE, BASE),
    );
    fixture::put_trade(
        &db,
        BASE + 1,
        &placed.trade(&fill("one", 1, None), 1, None, BASE + 1),
    );
    let pool =
        crate::rocks_source::ReaderPool::new(&tmp.path().join("pooled"), &[&primary]).unwrap();
    let mut cache = reader::Cache::default();
    let mut read =
        || reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30)).unwrap();
    assert_eq!(read().total_fills, 1);
    drop(db);
    std::fs::rename(&primary, tmp.path().join("old-exec")).unwrap();
    let fresh = self::db(&primary);
    fixture::put_order(
        &fresh,
        BASE + 5,
        &placed.lifecycle(fixture::CANCELED, "exec", BASE, BASE + 5),
    );
    let next = read();
    assert_eq!((next.total_orders, next.total_fills), (1, 0));
    assert_eq!(next.orders[0].phase, OrderPhase::Cancelled);
    assert_eq!(next.orders[0].filled_lots, 0);
}
