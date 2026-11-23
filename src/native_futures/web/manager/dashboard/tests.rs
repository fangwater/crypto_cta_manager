use super::*;
use crate::native_futures::{
    nav::{Offset, Side},
    records::fixture,
};

fn snapshot(tmp: &std::path::Path) -> Result<reader::Snapshot> {
    let primary = tmp.join("exec");
    let db = fixture::open(&primary);
    let order = fixture::Order::new(
        "SHFE:cu2612",
        "20261117",
        "yida:20261117:group:1",
        "7",
        Side::Buy,
        Offset::Open,
        1,
        100.0,
    );
    let now = crate::native_futures::now_us();
    fixture::put_order(&db, now, &order.lifecycle(fixture::NEW, "exec", now, now));
    let source = Source {
        id: "account".into(),
        account: "test".into(),
        adapter: "yida".into(),
        rocksdb_path: primary.clone(),
        enabled: true,
        sync: None,
    };
    let pool = crate::rocks_source::ReaderPool::new(&tmp.join("secondary"), &[&primary])?;
    let mut cache = reader::Cache::default();
    reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30))
}

#[test]
fn coverage_cut_follows_records_and_replication_state() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let observed = snapshot(tmp.path())?;
    let now = observed.read_at_us;
    let sample = observation(Sample::default(), Ok(observed.clone()), now);
    assert_eq!(sample.collection(), "live");
    let cut = sample.nav_cut.context("local coverage")?;
    assert_eq!(cut.at_us, observed.through_us.context("through")?);
    assert!(!cut.historical);
    assert!(sample.position().is_none());
    assert!(
        sample
            .position_reason(None)
            .is_some_and(|reason| reason.contains("起始快照"))
    );

    let mut stale = observed.clone();
    stale.replication = Some(crate::native_futures::sync::ReplicaStatus {
        initialized: true,
        source_captured_at_us: Some(now - 1),
        last_synced_at_us: Some(now - 60_000_000),
        ..Default::default()
    });
    stale.through_us = Some(now - 1);
    stale.replication_stale = true;
    let delayed = observation(sample.clone(), Ok(stale), now);
    assert_eq!(delayed.collection(), "stale");
    let historical = delayed.nav_cut.context("replica coverage")?;
    assert!(historical.historical && historical.at_us == now - 1);
    assert!(!historical.accepts(cut));

    let mut failed = observed.clone();
    failed.replication = Some(crate::native_futures::sync::ReplicaStatus {
        last_error: Some("test synchronization failure".into()),
        ..Default::default()
    });
    let failed = observation(sample, Ok(failed), now);
    assert!(failed.nav_cut.is_none() && failed.nav.is_none());
    assert_eq!(failed.collection(), "stale");
    Ok(())
}

#[test]
fn holdings_and_cached_nav_are_current_only_for_the_same_cut() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let observed = snapshot(tmp.path())?;
    let now = observed.read_at_us;
    let mut sample = observation(Sample::default(), Ok(observed.clone()), now);
    let cut = sample.nav_cut.context("coverage")?;
    sample.holdings = Some(Holdings {
        cut,
        anchor_us: now - 1,
        rows: BTreeMap::from([(("SHFE".into(), "cu2612".into()), (1, 0))]),
        notionals: Some((500.0, 0.0)),
    });
    let position = sample.position().context("holdings")?;
    assert_eq!((position.status(), position.count()), ("live", 1));

    let later = Sample {
        nav_cut: Some(NavCut {
            at_us: cut.at_us + 1,
            ..cut
        }),
        ..sample.clone()
    };
    assert_eq!(
        later.position().context("previous holdings")?.status(),
        "historical"
    );

    let saved = CachedNav {
        cut,
        anchor_fingerprint: "initial-anchor".into(),
        report: None,
        retry_at_us: Some(now + NAV_RETRY_US),
    };
    assert!(saved.reusable(cut, "initial-anchor", now));
    assert!(!saved.reusable(cut, "new-anchor", now));
    assert!(!saved.reusable(cut, "initial-anchor", now + NAV_RETRY_US));
    assert!(!saved.reusable(
        NavCut {
            historical: true,
            ..cut
        },
        "initial-anchor",
        now
    ));
    assert!(!saved.reusable(
        NavCut {
            at_us: cut.at_us + 120_000_000,
            ..cut
        },
        "initial-anchor",
        now
    ));
    Ok(())
}

fn broker(day: &str, observed_us: i64, pre_balance: f64, balance: f64) -> BrokerFunds {
    BrokerFunds {
        observed_us,
        trading_day: day.into(),
        currency: "CNY".into(),
        pre_balance,
        deposit: 0.0,
        withdraw: 0.0,
        balance,
        available: balance,
        withdraw_quota: balance,
        curr_margin: 100_000.0,
        frozen_margin: 0.0,
        frozen_cash: 0.0,
        frozen_commission: 0.0,
        position_profit: 0.0,
        close_profit: 0.0,
        commission: 12.5,
    }
}
fn daily(funds: BrokerFunds) -> DailyEquity {
    DailyEquity {
        source_id: "s".into(),
        funds,
        long_notional: None,
        short_notional: None,
        positions_at_us: None,
    }
}

#[test]
fn current_funds_prefer_the_live_relay_and_fall_back_to_the_saved_day() {
    let now = crate::native_futures::now_us();
    let saved = vec![daily(broker(
        "20261120",
        now - 3 * 86_400_000_000,
        1.0,
        1.0,
    ))];
    let mut sample = Sample::default();
    let fallback = current_funds(&sample, &saved, now).unwrap();
    assert_eq!(fallback.status(), "historical");
    assert_eq!(fallback.funds.trading_day, "20261120");
    sample.funds = Some(funds::Observation {
        manager_source_id: "s".into(),
        adapter: "ctp".into(),
        error: None,
        observed_us: Some(now - 1),
        runtime_status: Some("running".into()),
        connected: Some(true),
        transport_ready: Some(true),
        account: Some(broker("20261123", now - 2, 500_000.0, 510_000.0)),
    });
    let live = current_funds(&sample, &saved, now).unwrap();
    assert_eq!(
        (live.status(), live.funds.trading_day.as_str()),
        ("live", "20261123")
    );
    assert!(current_funds(&Sample::default(), &[], now).is_none());
}

#[test]
fn trading_day_rows_are_replaced_only_by_newer_observations_of_the_same_day() {
    let rows = vec![
        daily(broker("20261120", 10, 1.0, 1.0)),
        daily(broker("20261123", 20, 1.0, 2.0)),
    ];
    let merged = with_current(rows.clone(), Some(daily(broker("20261123", 30, 1.0, 3.0))));
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[1].funds.balance, 3.0);
    let older = with_current(rows.clone(), Some(daily(broker("20261123", 15, 1.0, 9.0))));
    assert_eq!(older[1].funds.balance, 2.0);
    // A night session already belongs to the next trading day.
    let next = with_current(rows.clone(), Some(daily(broker("20261124", 40, 2.0, 2.5))));
    assert_eq!(next.len(), 3);
    assert_eq!(with_current(rows.clone(), None).len(), 2);
}

#[test]
fn overview_fields_come_from_broker_funds_and_replayed_notionals() {
    let mut row = OverviewRow {
        long_position: Some(432_720.0),
        short_position: Some(301_700.0),
        ..Default::default()
    };
    let mut funds = broker("20261123", 1_795_415_513_000_000, 500_000.0, 512_000.0);
    funds.deposit = 10_000.0;
    apply_funds(&mut row, &funds, Some(1.5));
    assert_eq!(row.yesterday_equity, Some(500_000.0));
    assert_eq!(row.equity, Some(512_000.0));
    assert_eq!(row.today_profit, Some(2_000.0));
    assert_eq!(row.profit_percent, Some(0.4));
    assert_eq!(row.margin, Some(100_000.0));
    assert_eq!(row.margin_kind.as_deref(), Some("initial"));
    assert_eq!(row.margin_percent, Some(100_000.0 / 512_000.0 * 100.0));
    assert_eq!(row.long_leverage, Some(432_720.0 / 512_000.0));
    assert_eq!(row.short_leverage, Some(301_700.0 / 512_000.0));
    assert_eq!(row.max_drawdown_15d_percent, Some(1.5));
    assert_eq!(row.date_ms, Some(1_795_415_513_000));
    assert_eq!(dashed("20261123").as_deref(), Some("2026-11-23"));
}
