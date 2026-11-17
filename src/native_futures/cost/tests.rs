use super::*;
use crate::native_futures::{
    config::{Config, Source},
    reference,
};
use serde_json::json;

fn pack() -> Pack {
    serde_json::from_str(include_str!(
        "../../../config/cn-futures-reference.example.json"
    ))
    .unwrap()
}
fn facts(pack: &Pack) -> CostFacts {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/yida_native_records.json"
    ))
    .unwrap();
    let link = &pack.order_links[0];
    let signal = &pack.signals[0];
    let mut order = fixture["order"].clone();
    order["identity"] = json!({"trading_day":pack.trading_day,"venue":link.venue,"native":{"session":link.session,"order_ref":link.order_ref}});
    order["order"]["request"]["side"] = json!(signal.side);
    order["order"]["request"]["offset"] = json!(signal.offset);
    order["first_received_us"] = json!(reference::timestamp(&signal.received_at).unwrap() + 1);
    let mut fill = fixture["fill"].clone();
    fill["fill"]["identity"]["trading_day"] = json!(pack.trading_day);
    fill["fill"]["identity"]["side"] = json!(signal.side);
    fill["fill"]["offset"] = json!(signal.offset);
    fill["fill"]["order_key"] = order["identity"]["native"].clone();
    fill["fill"]["lots"] = json!(2);
    fill["fill"]["price"] = json!(120);
    fill["commission"] = json!(5);
    fill["received_us"] = json!(reference::timestamp(&signal.received_at).unwrap() + 2);
    CostFacts {
        replication: None,
        orders: vec![order],
        fills: vec![fill],
    }
}
fn report(p: &Pack, f: CostFacts) -> Report {
    build(
        &p.source_id,
        &p.trading_day,
        Some(p),
        f,
        reference::timestamp("2026-11-14T00:00:00Z").unwrap(),
    )
    .unwrap()
}
fn near(actual: Option<f64>, expected: f64) {
    assert!(
        (actual.unwrap() - expected).abs() < 1e-8,
        "{actual:?} != {expected}"
    );
}

#[test]
fn quantity_multiplier_twap_vwap_and_fee_costs_use_real_fill_lots() {
    let mut p = pack();
    let r = report(&p, facts(&p));
    near(r.signals[0].theoretical_price, 105.0);
    near(r.price_shortfall_cny, 150.0);
    near(r.fee_shortfall_cny, 1.0);
    near(r.total_shortfall_cny, 151.0);
    assert!(r.complete);
    assert_eq!(r.total_lots, 2);
    assert_eq!(r.signals[0].priced_lots, 2);
    assert_eq!(
        r.fills[0].order_ref.as_deref(),
        Some("18446744073709551614")
    );
    p.signals[0].model = Model::Vwap;
    let r = report(&p, facts(&p));
    near(r.signals[0].theoretical_price, 107.5);
    assert_eq!(r.signals[0].sample_weights, vec![0.25, 0.75]);
    near(r.price_shortfall_cny, 125.0);
    p.signals[0].side = "sell".into();
    let r = report(&p, facts(&p));
    near(r.price_shortfall_cny, -125.0);
}

#[test]
fn per_notional_and_close_today_fees_are_independent_and_missing_fees_remain_unknown() {
    let mut p = pack();
    p.signals[0].offset = "close_today".into();
    p.contracts[0]
        .fees
        .get_mut("close_today")
        .unwrap()
        .notional_rate = 0.001;
    let r = report(&p, facts(&p));
    near(r.fills[0].theoretical_commission_cny, 9.05);
    near(r.fee_shortfall_cny, -4.05);
    let mut f = facts(&p);
    f.fills[0]["commission"] = Value::Null;
    let r = report(&p, f);
    near(r.price_shortfall_cny, 150.0);
    assert!(r.fee_shortfall_cny.is_none() && r.total_shortfall_cny.is_none() && !r.complete);
    p.signals[0].offset = "close".into();
    let r = report(&p, facts(&p));
    assert!(r.fills[0].theoretical_commission_cny.is_none());
    assert!(r.fills[0].reason.as_deref().unwrap().contains("手续费规则"));
}

#[test]
fn explicit_night_day_sessions_skip_breaks_and_never_cross_into_another_trading_day() {
    let mut p = pack();
    p.sessions[0].end = "2026-11-12T21:01:00+08:00".into();
    p.sessions.push(reference::Session {
        venue: "SHFE".into(),
        symbol: "cu2612".into(),
        start: "2026-11-13T09:00:00+08:00".into(),
        end: "2026-11-13T09:01:00+08:00".into(),
    });
    p.bars[1].open_time = "2026-11-13T09:00:00+08:00".into();
    let r = report(&p, facts(&p));
    near(r.signals[0].theoretical_price, 105.0);
    assert_eq!(
        r.signals[0].sample_open_us[1],
        reference::timestamp(&p.bars[1].open_time).unwrap()
    );
    p.signals[0].received_at = "2026-11-12T21:00:01+08:00".into();
    p.signals[0].window_minutes = 1;
    near(report(&p, facts(&p)).signals[0].theoretical_price, 110.0);
    p.signals[0].window_minutes = 2;
    assert!(
        report(&p, facts(&p)).signals[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("分钟不足")
    );
}

#[test]
fn incomplete_missing_and_zero_volume_windows_never_return_partial_prices() {
    let mut p = pack();
    let r = build(
        &p.source_id,
        &p.trading_day,
        Some(&p),
        facts(&p),
        reference::timestamp("2026-11-12T21:01:59+08:00").unwrap(),
    )
    .unwrap();
    assert!(
        r.signals[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("尚未全部收盘")
    );
    p.bars.pop();
    let r = report(&p, facts(&p));
    assert!(r.signals[0].theoretical_price.is_none() && r.price_shortfall_cny.is_none());
    assert!(
        r.signals[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("缺少执行窗口分钟线")
    );
    p = pack();
    p.bars[0].volume_lots = 0.0;
    p.bars[0].turnover = 0.0;
    assert!(
        report(&p, facts(&p)).signals[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("零成交")
    );
}

#[test]
fn full_native_group_identity_and_explicit_links_are_required_without_guessing() {
    let p = pack();
    let mut f = facts(&p);
    let mut unlinked = f.fills[0].clone();
    unlinked["fill"]["identity"]["trade_id"] = json!("another");
    unlinked["fill"]["order_key"]["session"] = json!("yida:20261113:group:2");
    f.fills.push(unlinked);
    let r = report(&p, f);
    assert_eq!(r.fill_count, 2);
    assert_eq!(r.priced_fill_count, 1);
    assert_eq!(r.total_lots, 4);
    assert_eq!(r.priced_lots, 2);
    assert!(r.price_shortfall_cny.is_none());
    near(r.covered_price_shortfall_cny, 150.0);
    let mut f = facts(&p);
    f.fills[0]["fill"]["order_key"] = Value::Null;
    assert!(report(&p, f).fills[0].theoretical_price.is_none());
    let mut f = facts(&p);
    f.orders[0]["instrument"]["contract_multiple"] = json!(10);
    assert!(
        report(&p, f).fills[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("规则冲突")
    );
    let mut f = facts(&p);
    f.orders[0]["first_received_us"] = json!(1);
    assert!(
        report(&p, f).fills[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("早于参考信号")
    );
}

#[test]
fn absent_reference_duplicate_facts_and_account_mismatches_do_not_invent_costs() {
    let p = pack();
    let r = build(&p.source_id, &p.trading_day, None, facts(&p), 0).unwrap();
    assert!(!r.reference_available && r.fills[0].theoretical_price.is_none());
    let mut f = facts(&p);
    f.fills.push(f.fills[0].clone());
    assert!(build(&p.source_id, &p.trading_day, Some(&p), f, 0).is_err());
    assert!(build("other", &p.trading_day, Some(&p), facts(&p), 0).is_err());
}

#[test]
fn pack_validation_rejects_duplicates_bad_units_invalid_times_and_unknown_fields() {
    let mut p = pack();
    p.bars[0].turnover *= 2.0;
    assert!(p.validate().is_err());
    p = pack();
    p.bars.push(p.bars[0].clone());
    assert!(p.validate().is_err());
    p = pack();
    p.sessions.push(p.sessions[0].clone());
    assert!(p.validate().is_err());
    p = pack();
    p.bars[0].open_time = "2026-11-12T21:00:00".into();
    assert!(p.validate().is_err());
    p = pack();
    p.bars[0].open_time = "2026-11-12T21:00:01+08:00".into();
    assert!(p.validate().is_err());
    p = pack();
    p.trading_day = "20260230".into();
    assert!(p.validate().is_err());
    let mut value = serde_json::to_value(pack()).unwrap();
    value["silently_ignored"] = json!(true);
    assert!(serde_json::from_value::<Pack>(value).is_err());
}

#[test]
fn atomic_maintenance_preserves_revisions_requires_explicit_replace_and_rejects_store_overlap() {
    let temp = tempfile::tempdir().unwrap();
    let mut config: Config = toml::from_str(include_str!(
        "../../../config/cn-futures-manager.example.toml"
    ))
    .unwrap();
    config.secondary_root = temp.path().join("secondaries");
    config.reference_root = Some(temp.path().join("references"));
    config.sources = vec![Source {
        id: "yida_sim01".into(),
        account: "test".into(),
        adapter: "yida".into(),
        rocksdb_path: temp.path().join("exec"),
        enabled: true,
        sync: None,
    }];
    let mut p = pack();
    let old = reference::install(&config, &p, false).unwrap();
    assert_eq!(old, reference::install(&config, &p, false).unwrap());
    p.provenance = "reviewed correction".into();
    assert!(reference::install(&config, &p, false).is_err());
    let new = reference::install(&config, &p, true).unwrap();
    assert_ne!(old, new);
    let root = config.reference_root.as_ref().unwrap();
    assert!(
        root.join("yida_sim01/history/20261113")
            .join(format!("{old}.json"))
            .exists()
    );
    assert_eq!(
        reference::load(&config, "yida_sim01", "20261113")
            .unwrap()
            .unwrap()
            .provenance,
        "reviewed correction"
    );
    config.reference_root = Some(config.sources[0].rocksdb_path.join("bad"));
    assert!(config.validate().is_err());
    config.reference_root = Some(config.secondary_root.join("bad"));
    assert!(config.validate().is_err());
}
