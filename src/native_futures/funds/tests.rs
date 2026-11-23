use super::*;

pub(crate) fn funds(day: &str, pre_balance: f64, balance: f64) -> BrokerFunds {
    BrokerFunds {
        observed_us: 1_795_415_513_000_000,
        trading_day: day.into(),
        currency: "CNY".into(),
        pre_balance,
        deposit: 0.0,
        withdraw: 0.0,
        balance,
        available: balance / 2.0,
        withdraw_quota: balance / 2.0,
        curr_margin: balance / 4.0,
        frozen_margin: 0.0,
        frozen_cash: 0.0,
        frozen_commission: 0.0,
        position_profit: 0.0,
        close_profit: 0.0,
        commission: 0.0,
    }
}

fn row(day: &str, pre_balance: f64, balance: f64) -> DailyEquity {
    DailyEquity {
        source_id: "s".into(),
        funds: funds(day, pre_balance, balance),
        long_notional: None,
        short_notional: None,
        positions_at_us: None,
    }
}

/// Consecutive weekdays from 2026-11-02 (a Monday).
fn weekdays(count: usize) -> Vec<String> {
    let mut day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
    let mut out = vec![];
    while out.len() < count {
        out.push(day.format("%Y%m%d").to_string());
        day = next_weekday(day);
    }
    out
}

#[test]
fn profit_excludes_cash_movements_and_uses_settled_previous_equity() {
    let mut f = funds("20261123", 1000.0, 1250.0);
    f.deposit = 300.0;
    f.withdraw = 100.0;
    assert_eq!(f.profit(), 50.0);
    assert_eq!(f.profit_percent(), Some(5.0));
    assert_eq!(f.margin_percent(), Some(1250.0 / 4.0 / 1250.0 * 100.0));
    assert_eq!(f.leverage(Some(2500.0)), Some(2.0));
    assert_eq!(f.leverage(None), None);
    f.pre_balance = 0.0;
    f.balance = 0.0;
    assert_eq!(f.profit_percent(), None);
    assert_eq!(f.margin_percent(), None);
    assert_eq!(f.leverage(Some(1.0)), None);
}

#[test]
fn funds_require_cny_finite_values_and_a_native_trading_day() {
    funds("20261123", 1.0, 1.0).validate().unwrap();
    for bad in [
        BrokerFunds {
            currency: "USD".into(),
            ..funds("20261123", 1.0, 1.0)
        },
        BrokerFunds {
            balance: f64::NAN,
            ..funds("20261123", 1.0, 1.0)
        },
        funds("2026-11-23", 1.0, 1.0),
        funds("20261332", 1.0, 1.0),
        BrokerFunds {
            observed_us: 0,
            ..funds("20261123", 1.0, 1.0)
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}

#[test]
fn observation_identity_and_freshness() {
    let account = funds("20261123", 1.0, 1.0);
    let now = account.observed_us + 1_000_000;
    let mut obs = Observation {
        manager_source_id: "s".into(),
        adapter: "ctp".into(),
        error: None,
        observed_us: Some(account.observed_us + 10),
        runtime_status: Some("running".into()),
        connected: Some(true),
        transport_ready: Some(true),
        account: Some(account.clone()),
    };
    obs.validate("s", "ctp").unwrap();
    assert!(obs.validate("other", "ctp").is_err());
    assert!(obs.validate("s", "yida").is_err());
    assert_eq!(obs.historical_reason(now), None);
    assert!(obs.historical_reason(now + FRESH_US).is_some());
    obs.connected = Some(false);
    assert!(obs.historical_reason(now).is_some());
    obs.connected = Some(true);
    obs.error = Some("等待持仓 IPC".into());
    assert!(obs.historical_reason(now).is_some());
    obs.observed_us = Some(account.observed_us - 1);
    assert!(obs.validate("s", "ctp").is_err());
    obs.account = None;
    assert_eq!(obs.historical_reason(now), None);
    obs.validate("s", "ctp").unwrap();
}

#[test]
fn drawdown_chains_settled_returns_over_fifteen_consecutive_trading_days() {
    let days = weekdays(16);
    // Flat days at 100, a rise to 120, then a fall to 90 and a deposit-only day.
    let mut rows: Vec<_> = days[..15].iter().map(|d| row(d, 100.0, 100.0)).collect();
    rows[3] = row(&days[3], 100.0, 120.0);
    rows[4] = row(&days[4], 120.0, 90.0);
    rows[5].funds.deposit = 1000.0;
    rows[5].funds.balance = 1100.0;
    let value = drawdown(&rows).unwrap();
    assert!((value - 25.0).abs() < 1e-9, "{value}");
    // Fourteen rows are not enough.
    assert_eq!(drawdown(&rows[1..]), None);
    // A later day keeps only the last fifteen rows.
    rows.push(row(&days[15], 100.0, 100.0));
    let later = drawdown(&rows[1..]).unwrap();
    assert!((later - 25.0).abs() < 1e-9);
}

#[test]
fn drawdown_is_unknown_across_a_missing_weekday_or_nonpositive_equity() {
    let days = weekdays(16);
    let mut rows: Vec<_> = days.iter().map(|d| row(d, 100.0, 100.0)).collect();
    assert_eq!(drawdown(&rows), Some(0.0));
    rows.remove(7);
    assert_eq!(drawdown(&rows), None);
    let mut rows: Vec<_> = days[..15].iter().map(|d| row(d, 100.0, 100.0)).collect();
    rows[2].funds.pre_balance = 0.0;
    assert_eq!(drawdown(&rows), None);
    // Friday to Monday is adjacent.
    assert_eq!(
        next_weekday(day("20261120").unwrap()),
        day("20261123").unwrap()
    );
}
