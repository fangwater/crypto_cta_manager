use super::*;
fn at() -> i64 {
    reference::timestamp("2026-11-17T01:00:00Z").unwrap()
}
fn anchor() -> Anchor {
    Anchor {
        source_id: "account-01".into(),
        snapshot_us: at() - 3_600_000_000,
        trading_day: "20261117".into(),
        trades_key: 1,
        store_id: "native-source-01".into(),
        positions: vec![],
        note: "explicit fixture anchor".into(),
    }
}
fn position(side: Side, today: i64, yesterday: i64, price: f64) -> InitialPosition {
    InitialPosition {
        venue: "SHFE".into(),
        symbol: "cu2612".into(),
        side,
        today,
        yesterday,
        reference_price: price,
        contract_multiplier: 10.0,
    }
}
fn fill(seq: i64, side: Side, offset: Offset, lots: i64, price: f64, time: i64) -> Fill {
    Fill {
        key_us: seq,
        source_id: "account-01".into(),
        venue: "SHFE".into(),
        symbol: "cu2612".into(),
        trading_day: "20261117".into(),
        trade_id: seq.to_string(),
        side,
        offset,
        lots,
        price,
        received_us: time,
        commission_cny: Some(0.0),
    }
}
fn prices() -> BTreeMap<(String, String), MarketPrices> {
    BTreeMap::from([(
        ("SHFE".into(), "cu2612".into()),
        MarketPrices {
            multiplier: 10.0,
            closes: BTreeMap::from([
                (at(), 105.0),
                (at() + 60_000_000, 107.0),
                (at() + 120_000_000, 109.0),
                (at() + 180_000_000, 110.0),
            ]),
            sessions: vec![],
            ..Default::default()
        },
    )])
}
fn report(
    a: &Anchor,
    fills: &[Fill],
    p: &BTreeMap<(String, String), MarketPrices>,
    rates: &BTreeMap<(String, String), Vec<AccountRates>>,
) -> Result<Report> {
    build(Inputs {
        anchor: a,
        fills,
        prices: p,
        rates,
        start_us: at(),
        end_us: at() + 180_000_000,
        source_through_us: at() + 180_000_000,
        max_points: 1000,
    })
}
fn near(x: f64, y: f64) {
    assert!((x - y).abs() < 1e-8, "{x} != {y}");
}
fn rate(observed: i64, fee: f64) -> AccountRates {
    AccountRates {
        account_key: "reference-account-01".into(),
        venue: "SHFE".into(),
        symbol: "cu2612".into(),
        observed_at: "2026-11-17T00:00:00Z".into(),
        observed_at_us: observed,
        trading_day: "20261117".into(),
        contract_multiplier: 10.0,
        price_tick: 1.0,
        active: true,
        fees: ["open", "close", "close_today", "close_yesterday"]
            .into_iter()
            .map(|k| {
                (
                    k.into(),
                    reference::Fee {
                        per_lot: fee,
                        notional_rate: 0.0,
                    },
                )
            })
            .collect(),
        long_margin_rate: 0.1,
        short_margin_rate: 0.1,
        long_margin_per_lot: 0.0,
        short_margin_per_lot: 0.0,
        record_hash: "b".repeat(64),
        receipt_sha256: "c".repeat(64),
    }
}
#[test]
fn gross_long_short_fifo_offsets_multiplier_and_fee_totals_are_additive() {
    let mut a = anchor();
    a.positions = vec![
        position(Side::Buy, 2, 3, 100.0),
        position(Side::Sell, 2, 0, 100.0),
    ];
    let mut fills = vec![
        fill(2, Side::Buy, Offset::Open, 2, 108.0, at() + 60_000_000),
        fill(
            3,
            Side::Sell,
            Offset::CloseYesterday,
            2,
            109.0,
            at() + 120_000_000,
        ),
    ];
    fills[0].commission_cny = Some(2.0);
    fills[1].commission_cny = Some(3.0);
    let r = report(&a, &fills, &prices(), &BTreeMap::new()).unwrap();
    let s = r.summary.unwrap();
    near(s.realized_cny, 180.0);
    near(s.floating_cny, -10.0);
    near(s.nav_before_fees_cny, 170.0);
    near(s.nav_after_fees_cny.unwrap(), 165.0);
    assert_eq!((r.markets[0].long_lots, r.markets[0].short_lots), (5, 2));
    near(
        r.markets.iter().map(|m| m.nav_before_fees_cny).sum(),
        s.nav_before_fees_cny,
    );
    assert_eq!(r.points[0].nav_after_fees_cny, Some(0.0));
    assert_eq!(r.fees.actual_count, 2);
}
#[test]
fn close_today_consumes_today_fifo_without_using_yesterday_inventory() {
    let mut a = anchor();
    a.positions = vec![position(Side::Buy, 2, 3, 100.0)];
    let f = vec![
        fill(2, Side::Buy, Offset::Open, 2, 107.0, at() + 60_000_000),
        fill(
            3,
            Side::Sell,
            Offset::CloseToday,
            3,
            110.0,
            at() + 120_000_000,
        ),
    ];
    let r = report(&a, &f, &prices(), &BTreeMap::new()).unwrap();
    let s = r.summary.unwrap();
    near(s.realized_cny, 230.0);
    near(s.nav_before_fees_cny, 310.0);
    assert_eq!(r.markets[0].long_lots, 4);
    let excess = vec![fill(
        4,
        Side::Sell,
        Offset::CloseToday,
        3,
        110.0,
        at() + 60_000_000,
    )];
    assert!(report(&a, &excess, &prices(), &BTreeMap::new()).is_err());
}
#[test]
fn hedged_flat_net_account_keeps_independent_long_and_short_costs() {
    let mut a = anchor();
    a.positions = vec![
        position(Side::Buy, 2, 0, 100.0),
        position(Side::Sell, 2, 0, 120.0),
    ];
    let mut p = prices();
    p.values_mut()
        .next()
        .unwrap()
        .closes
        .insert(at() + 180_000_000, 115.0);
    let f = vec![fill(
        2,
        Side::Buy,
        Offset::CloseToday,
        1,
        113.0,
        at() + 120_000_000,
    )];
    let r = report(&a, &f, &p, &BTreeMap::new()).unwrap();
    near(r.summary.unwrap().nav_before_fees_cny, 20.0);
    assert_eq!((r.markets[0].long_lots, r.markets[0].short_lots), (2, 1));
}
#[test]
fn earlier_fills_establish_baseline_without_charging_old_fees_again() {
    let a = anchor();
    let mut f = fill(2, Side::Buy, Offset::Open, 1, 100.0, at() - 1_000_000);
    f.commission_cny = Some(5.0);
    let r = report(&a, &[f], &prices(), &BTreeMap::new()).unwrap();
    near(r.summary.unwrap().nav_after_fees_cny.unwrap(), 50.0);
    assert_eq!(r.fees.fill_count, 0);
}
#[test]
fn exact_start_fills_belong_to_window_and_zero_duration_keeps_terminal_execution() {
    let a = anchor();
    let mut f = fill(2, Side::Buy, Offset::Open, 1, 105.0, at());
    f.commission_cny = Some(1.0);
    let p = prices();
    let r = report(&a, &[f.clone()], &p, &BTreeMap::new()).unwrap();
    assert_eq!(r.points[0].nav_after_fees_cny, Some(0.0));
    near(r.summary.unwrap().nav_after_fees_cny.unwrap(), 49.0);
    let r = build(Inputs {
        anchor: &a,
        fills: &[f],
        prices: &p,
        rates: &BTreeMap::new(),
        start_us: at(),
        end_us: at(),
        source_through_us: at(),
        max_points: 10,
    })
    .unwrap();
    assert_eq!(r.points.len(), 1);
    near(r.summary.unwrap().nav_after_fees_cny.unwrap(), -1.0);
}
#[test]
fn missing_actual_fees_use_only_causal_contract_offset_rates_and_never_become_zero() {
    let a = anchor();
    let mut f = fill(2, Side::Buy, Offset::Open, 1, 105.0, at());
    f.commission_cny = None;
    let rates = BTreeMap::from([(
        ("SHFE".into(), "cu2612".into()),
        vec![rate(at() - 1, 2.0), rate(at() + 1, 99.0)],
    )]);
    let r = report(&a, &[f.clone()], &prices(), &rates).unwrap();
    assert_eq!(r.fees.estimated_count, 1);
    near(r.summary.unwrap().fees_cny.unwrap(), 2.0);
    let r = report(&a, &[f], &prices(), &BTreeMap::new()).unwrap();
    assert_eq!(r.fees.missing_count, 1);
    let s = r.summary.unwrap();
    assert!(s.nav_after_fees_cny.is_none());
    near(s.nav_before_fees_cny, 50.0);
}

#[test]
fn cross_account_cross_market_overclose_and_earlier_trading_day_fills_are_rejected() {
    let mut a = anchor();
    a.positions = vec![position(Side::Buy, 0, 1, 100.0)];
    let mut f = fill(2, Side::Sell, Offset::CloseYesterday, 1, 110.0, at());
    f.source_id = "other-account".into();
    assert!(report(&a, &[f.clone()], &prices(), &BTreeMap::new()).is_err());
    f.source_id = a.source_id.clone();
    f.trading_day = "20261116".into();
    assert!(report(&a, &[f.clone()], &prices(), &BTreeMap::new()).is_err());
    f.trading_day = a.trading_day.clone();
    f.lots = 2;
    assert!(report(&a, &[f], &prices(), &BTreeMap::new()).is_err());
    let mut f = fill(3, Side::Sell, Offset::CloseYesterday, 1, 110.0, at());
    f.venue = "DCE".into();
    f.symbol = "m2701".into();
    let mut p = prices();
    p.insert(
        (f.venue.clone(), f.symbol.clone()),
        p.values().next().unwrap().clone(),
    );
    assert!(report(&a, &[f], &p, &BTreeMap::new()).is_err());
}
#[test]
fn empty_anchor_empty_history_is_known_zero_and_unknown_times_serialize_as_null() {
    let r = report(&anchor(), &[], &BTreeMap::new(), &BTreeMap::new()).unwrap();
    assert_eq!(r.summary.unwrap().nav_after_fees_cny, Some(0.0));
    let u = Report::unavailable("a", 0, 1, 2, 0, "missing".into());
    let v = serde_json::to_value(u).unwrap();
    assert!(v["snapshot_us"].is_null() && v["source_through_us"].is_null());
}
#[test]
fn active_missing_minute_fails_while_scheduled_break_keeps_the_last_close() {
    let mut p = prices().into_values().next().unwrap();
    p.sessions = vec![(9 * 3600, 15 * 3600)];
    p.closes.remove(&(at() + 120_000_000));
    assert!(p.mark(at() + 120_000_000).is_err());
    let lunch = at() + 3 * 3600 * 1_000_000;
    p.sessions = vec![
        (9 * 3600, 11 * 3600 + 30 * 60),
        (13 * 3600 + 30 * 60, 15 * 3600),
    ];
    p.closes.insert(at() + 150 * 60_000_000, 110.0);
    assert!(p.mark(lunch).is_ok());
    assert!(p.mark(lunch + 86400 * 1_000_000).is_err());
}
#[test]
fn receive_key_is_a_stable_tie_breaker_and_new_trading_day_moves_today_lots_to_yesterday() {
    let a = anchor();
    let open = fill(2, Side::Buy, Offset::Open, 1, 100.0, at());
    let mut close = fill(
        3,
        Side::Sell,
        Offset::CloseYesterday,
        1,
        110.0,
        at() + 60_000_000,
    );
    close.trading_day = "20261118".into();
    let r = report(&a, &[close, open], &prices(), &BTreeMap::new()).unwrap();
    near(r.summary.unwrap().realized_cny, 100.0);
    assert_eq!(r.markets[0].long_lots, 0);
    // Same receipt time: the record key orders them.
    let open = fill(2, Side::Buy, Offset::Open, 1, 100.0, at());
    let close = fill(3, Side::Sell, Offset::CloseToday, 1, 110.0, at());
    assert!(report(&a, &[close, open], &prices(), &BTreeMap::new()).is_ok());
}

#[test]
fn complete_endpoints_do_not_hide_a_missing_active_minute() {
    let mut a = anchor();
    a.positions.push(position(Side::Buy, 1, 0, 100.0));
    let mut prices = prices();
    let market = prices.values_mut().next().unwrap();
    market.sessions = vec![(9 * 3600, 15 * 3600)];
    market.closes.remove(&(at() + 120_000_000));
    let result = build_selected(
        Inputs {
            anchor: &a,
            fills: &[],
            prices: &prices,
            rates: &BTreeMap::new(),
            start_us: at(),
            end_us: at() + 180_000_000,
            source_through_us: at() + 180_000_000,
            max_points: 2,
        },
        &[],
        false,
        Some(15 * 60_000_000),
    );
    assert!(result.is_err());
}

#[test]
fn inventory_replays_fills_after_the_cut_with_native_close_semantics() {
    let mut a = anchor();
    a.positions = vec![position(Side::Buy, 1, 2, 100.0)];
    let before_cut = fill(1, Side::Buy, Offset::Open, 5, 101.0, at());
    let close = fill(2, Side::Sell, Offset::CloseYesterday, 2, 102.0, at() + 1);
    let short = fill(3, Side::Sell, Offset::Open, 1, 102.0, at() + 2);
    let rows = inventory(&a, &[before_cut, close, short]).unwrap();
    assert_eq!(
        rows,
        BTreeMap::from([(("SHFE".into(), "cu2612".into()), (1, 1))])
    );
    let today = fill(4, Side::Sell, Offset::CloseYesterday, 1, 102.0, at() + 3);
    let close = fill(2, Side::Sell, Offset::CloseYesterday, 2, 102.0, at() + 1);
    assert!(inventory(&a, &[close, today]).is_err());
}

fn local(text: &str) -> i64 {
    reference::timestamp(text).unwrap()
}
/// Completed-minute closes ending in `(from, to]` (Shanghai wall-clock times).
fn closes(prices: &mut MarketPrices, from: &str, to: &str, price: f64) {
    let mut at = local(from) + MINUTE_US;
    while at <= local(to) {
        prices.closes.insert(at, price);
        at += MINUTE_US;
    }
}
fn sessions() -> MarketPrices {
    MarketPrices {
        multiplier: 10.0,
        sessions: vec![
            (21 * 3600, 23 * 3600),
            (9 * 3600, 11 * 3600 + 30 * 60),
            (13 * 3600 + 30 * 60, 15 * 3600),
        ],
        ..Default::default()
    }
}
fn window(a: &Anchor, fills: &[Fill], p: &MarketPrices, start: i64, end: i64) -> Result<Report> {
    build(Inputs {
        anchor: a,
        fills,
        prices: &BTreeMap::from([(("SHFE".into(), "cu2612".into()), p.clone())]),
        rates: &BTreeMap::new(),
        start_us: start,
        end_us: end,
        source_through_us: end,
        max_points: 1000,
    })
}
fn nav_at(a: &Anchor, fills: &[Fill], p: &MarketPrices, end: &str) -> f64 {
    window(a, fills, p, a.snapshot_us, local(end))
        .unwrap()
        .summary
        .unwrap()
        .nav_before_fees_cny
}

#[test]
fn friday_night_fill_belongs_to_mondays_trading_day_and_the_weekend_keeps_its_value() {
    use crate::native_futures::records::{self, fixture};
    let mut a = anchor();
    a.snapshot_us = local("2026-11-20T21:00:00+08:00");
    a.trading_day = "20261123".into();
    // The trading day comes from the record extension, not the receipt time.
    let night = local("2026-11-20T21:30:00+08:00");
    let order = fixture::Order::new(
        "SHFE:cu2612",
        "20261123",
        "ctp:1:2",
        "1",
        Side::Buy,
        Offset::Open,
        1,
        100.0,
    );
    let trade = fixture::Fill {
        trade_id: "night".into(),
        lots: 1,
        price: 100.0,
        commission: Some(0.0),
        order_id: 1,
    };
    let open = source::fill(
        &a.source_id,
        records::decode_trade(
            records::key(night).as_bytes(),
            &order.trade(&trade, 1, None, night),
        )
        .unwrap(),
    );
    assert_eq!(open.trading_day, "20261123");
    let mut p = sessions();
    closes(
        &mut p,
        "2026-11-20T21:00:00+08:00",
        "2026-11-20T23:00:00+08:00",
        105.0,
    );
    closes(
        &mut p,
        "2026-11-23T09:00:00+08:00",
        "2026-11-23T09:30:00+08:00",
        110.0,
    );
    let friday = nav_at(
        &a,
        std::slice::from_ref(&open),
        &p,
        "2026-11-20T23:00:00+08:00",
    );
    near(friday, 50.0);
    for weekend in [
        "2026-11-21T02:30:00+08:00",
        "2026-11-21T12:00:00+08:00",
        "2026-11-22T21:30:00+08:00",
        "2026-11-23T08:59:00+08:00",
    ] {
        near(nav_at(&a, std::slice::from_ref(&open), &p, weekend), friday);
    }
    // Monday's day session closes Friday night's lot as today's position.
    let mut close = fill(
        9,
        Side::Sell,
        Offset::CloseToday,
        1,
        110.0,
        local("2026-11-23T09:30:00+08:00"),
    );
    close.trading_day = "20261123".into();
    let monday = window(
        &a,
        &[open.clone(), close.clone()],
        &p,
        a.snapshot_us,
        close.received_us,
    )
    .unwrap();
    near(monday.summary.unwrap().realized_cny, 100.0);
    close.offset = Offset::CloseYesterday;
    assert!(
        window(
            &a,
            &[open, close.clone()],
            &p,
            a.snapshot_us,
            close.received_us
        )
        .is_err()
    );
    // Monday is a trading day: its missing session minutes are not a weekend.
    p.closes
        .retain(|at, _| *at < local("2026-11-23T09:00:00+08:00"));
    assert!(p.mark(local("2026-11-23T09:05:00+08:00")).is_err());
}

#[test]
fn a_holiday_after_a_trading_days_close_keeps_its_last_mark() {
    let mut a = anchor();
    a.snapshot_us = local("2026-12-31T14:00:00+08:00");
    a.trading_day = "20261231".into();
    a.positions = vec![position(Side::Buy, 1, 0, 100.0)];
    let mut p = sessions();
    // No night session before the New Year holiday; trading resumes Monday.
    closes(
        &mut p,
        "2026-12-31T13:59:00+08:00",
        "2026-12-31T14:59:00+08:00",
        100.0,
    );
    closes(
        &mut p,
        "2026-12-31T14:59:00+08:00",
        "2026-12-31T15:00:00+08:00",
        102.0,
    );
    closes(
        &mut p,
        "2027-01-04T09:00:00+08:00",
        "2027-01-04T09:01:00+08:00",
        120.0,
    );
    near(nav_at(&a, &[], &p, "2026-12-31T15:00:00+08:00"), 20.0);
    for holiday in [
        "2027-01-01T10:00:00+08:00",
        "2027-01-01T21:30:00+08:00",
        "2027-01-03T12:00:00+08:00",
        "2027-01-04T08:59:00+08:00",
    ] {
        near(nav_at(&a, &[], &p, holiday), 20.0);
    }
    near(nav_at(&a, &[], &p, "2027-01-04T09:01:00+08:00"), 200.0);
    // A gap inside the trading day itself is still missing data.
    p.closes.remove(&local("2026-12-31T14:30:00+08:00"));
    assert!(
        window(
            &a,
            &[],
            &p,
            a.snapshot_us,
            local("2026-12-31T15:00:00+08:00")
        )
        .is_err()
    );
}

#[test]
fn valuation_stops_at_the_published_minute_of_an_in_progress_day() {
    let mut p = prices();
    let end = at() + 180_000_000;
    // A completed day (or none in progress) does not limit the end.
    assert_eq!(service::covered_end(&p, end), end);
    // Minutes after the provider watermark are pending, not missing.
    for row in p.values_mut() {
        row.covered_through_us = Some(at() + 120_000_000);
    }
    let covered = service::covered_end(&p, end);
    assert_eq!(covered, at() + 120_000_000);
    assert_eq!(p.values().next().unwrap().mark(covered).unwrap(), 109.0);
}

#[test]
fn trading_segments_merge_published_minutes_and_leave_recesses_out() {
    let minute = 60_000_000i64;
    let at = |m: i64| 1_000 * minute + m * minute;
    let prices = |ends: &[i64]| MarketPrices {
        multiplier: 1.0,
        closes: ends.iter().map(|&m| (at(m), 1.0)).collect(),
        ..MarketPrices::default()
    };
    // Two contracts: one trades minutes 1-3 and 10-11, the other 3-4.
    let markets = BTreeMap::from([
        (("SHFE".to_owned(), "rb2701".to_owned()), prices(&[1, 2, 3, 10, 11])),
        (("SHFE".to_owned(), "hc2701".to_owned()), prices(&[3, 4])),
    ]);
    assert_eq!(
        trading_segments(&markets, at(0), at(20)),
        vec![(at(0), at(4)), (at(9), at(11))]
    );
    // The window bounds both the minutes and the first segment start.
    assert_eq!(trading_segments(&markets, at(2), at(10)), vec![(at(2), at(4)), (at(9), at(10))]);
}
