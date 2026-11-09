//! Business accounts resolve to execution sources. Stored fills, snapshots,
//! ownership and risk always retain their original source identity.
use crate::{config::AppConfig, nav};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub source_ids: Vec<String>,
    /// Business-account inception; never a position or equity snapshot.
    pub created_at_us: i64,
}

pub fn validate(config: &AppConfig) -> Result<()> {
    let mut owned = BTreeSet::new();
    for (id, account) in &config.accounts {
        ensure!(
            account.created_at_us > 0,
            "managed account inception must be positive: {id}"
        );
        ensure!(
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "invalid managed account ID"
        );
        ensure!(
            !config.sources.iter().any(|s| &s.id == id),
            "account ID conflicts with execution source: {id}"
        );
        ensure!(
            !account.name.trim().is_empty()
                && account.name.chars().count() <= 128
                && !account.name.chars().any(char::is_control),
            "invalid managed account name: {id}"
        );
        ensure!(
            account.source_ids.len() >= 2,
            "managed account {id} requires multiple execution sources"
        );
        for source_id in &account.source_ids {
            ensure!(
                owned.insert(source_id),
                "execution source belongs to multiple accounts or is duplicated: {source_id}"
            );
            let source = config.sources.iter().find(|s| &s.id == source_id);
            ensure!(source.is_some(), "unknown execution source: {source_id}");
            ensure!(
                matches!(
                    source.unwrap().venue.as_str(),
                    "okex-futures" | "binance-futures"
                ),
                "managed spread accounts require linear futures execution sources"
            );
        }
    }
    Ok(())
}

pub fn parent_id<'a>(config: &'a AppConfig, source_id: &str) -> Option<&'a str> {
    config
        .accounts
        .iter()
        .find(|(_, a)| a.source_ids.iter().any(|id| id == source_id))
        .map(|(id, _)| id.as_str())
}

/// An account is accessible only with access to every execution source. The
/// same boundary applies to direct leg APIs and gateways, including admins.
pub fn scope_access(config: &AppConfig, allowed: &mut BTreeSet<String>) {
    for (id, account) in &config.accounts {
        if account.source_ids.iter().all(|s| allowed.contains(s))
            && account
                .source_ids
                .iter()
                .all(|id| config.sources.iter().any(|s| &s.id == id && s.enabled))
        {
            allowed.insert(id.clone());
        } else {
            allowed.remove(id);
            for source in &account.source_ids {
                allowed.remove(source);
            }
        }
    }
}

pub fn public_ids(config: &AppConfig, allowed: &BTreeSet<String>) -> Vec<String> {
    config
        .sources
        .iter()
        .filter(|s| s.enabled && allowed.contains(&s.id) && parent_id(config, &s.id).is_none())
        .map(|s| s.id.clone())
        .chain(
            config
                .accounts
                .keys()
                .filter(|id| allowed.contains(*id))
                .cloned(),
        )
        .collect()
}

/// Resolve once at the API boundary, before FIFO/valuation. Overlapping public
/// account and leg selections are rejected to prevent double counting.
pub fn expand(config: &AppConfig, ids: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut selected = BTreeSet::new();
    for id in ids {
        let members = config
            .accounts
            .get(id)
            .map(|a| a.source_ids.as_slice())
            .unwrap_or(std::slice::from_ref(id));
        for member in members {
            let source = config
                .sources
                .iter()
                .find(|s| &s.id == member)
                .ok_or_else(|| format!("unknown account or execution source: {id}"))?;
            if !source.enabled {
                return Err(format!("execution source is disabled: {member}"));
            }
            if !selected.insert(member.clone()) {
                return Err("account selections overlap or contain duplicates".into());
            }
        }
    }
    Ok(selected.into_iter().collect())
}

pub fn project_report(config: &AppConfig, report: &nav::NavReport) -> nav::NavReport {
    let mut reports = report
        .sources
        .iter()
        .filter(|s| parent_id(config, &s.source_id).is_none())
        .cloned()
        .collect::<Vec<_>>();
    for (id, account) in &config.accounts {
        let members = account
            .source_ids
            .iter()
            .map(|id| report.sources.iter().find(|s| &s.source_id == id).cloned())
            .collect::<Option<Vec<_>>>();
        let Some(members) = members else { continue };
        let mut combined = members[0].clone();
        combined.source_id = id.clone();
        combined.account = account.name.clone();
        combined.configured_venue = "spread".into();
        combined.estimated_fee_rate = None;
        combined.maker_fee_rate = None;
        combined.taker_fee_rate = None;
        combined.quote_fee_rates.clear();
        combined.initial_position_snapshot_ts_us = members
            .iter()
            .filter_map(|s| s.initial_position_snapshot_ts_us)
            .min();
        combined.initial_position_count = members.iter().map(|s| s.initial_position_count).sum();
        combined.order_event_count = members.iter().map(|s| s.order_event_count).sum();
        combined.ignored_at_or_before_snapshot_event_count = members
            .iter()
            .map(|s| s.ignored_at_or_before_snapshot_event_count)
            .sum();
        combined.ignored_non_fill_event_count =
            members.iter().map(|s| s.ignored_non_fill_event_count).sum();
        combined.first_fill_ts_us = members.iter().filter_map(|s| s.first_fill_ts_us).min();
        combined.last_fill_ts_us = members.iter().filter_map(|s| s.last_fill_ts_us).max();
        combined.totals = nav::NavTotals::default();
        let mut symbols = BTreeMap::<String, nav::SymbolNavReport>::new();
        for member in &members {
            combined.totals.add(member.totals);
            for s in &member.symbols {
                if let Some(total) = symbols.get_mut(&s.symbol) {
                    total.venue_count += s.venue_count;
                    total.initial_net_quantity += s.initial_net_quantity;
                    total.long_quantity += s.long_quantity;
                    total.short_quantity += s.short_quantity;
                    total.net_quantity += s.net_quantity;
                    total.long_position_value_quote += s.long_position_value_quote;
                    total.short_position_value_quote += s.short_position_value_quote;
                    total.net_position_value_quote += s.net_position_value_quote;
                    total.totals.add(s.totals);
                    total.venues.extend(s.venues.clone());
                } else {
                    symbols.insert(s.symbol.clone(), s.clone());
                }
            }
        }
        combined.symbols = symbols.into_values().collect();
        combined.execution_sources = members;
        reports.push(combined);
    }
    nav::aggregate_source_reports(reports)
}

pub fn project_overview(
    config: &AppConfig,
    report: &mut crate::account_overview::OverviewResponse,
) {
    use crate::account_overview::OverviewRow;
    let original = std::mem::take(&mut report.accounts);
    report.accounts = original
        .iter()
        .filter(|r| parent_id(config, &r.source_id).is_none())
        .cloned()
        .collect();
    for (id, account) in &config.accounts {
        let rows = account
            .source_ids
            .iter()
            .map(|id| original.iter().find(|r| &r.source_id == id))
            .collect::<Option<Vec<_>>>();
        let Some(rows) = rows else { continue };
        let sum = |field: fn(&OverviewRow) -> Option<f64>| {
            rows.iter()
                .map(|r| field(r))
                .collect::<Option<Vec<_>>>()
                .map(|v| v.iter().sum::<f64>())
        };
        let equity = sum(|r| r.equity);
        let yesterday = sum(|r| r.yesterday_equity);
        let long = sum(|r| r.long_position);
        let short = sum(|r| r.short_position);
        let margin_kind = rows[0]
            .margin_kind
            .clone()
            .filter(|kind| rows.iter().all(|r| r.margin_kind.as_ref() == Some(kind)));
        let margin = margin_kind.as_ref().and_then(|_| sum(|r| r.margin));
        let ratio = |v: Option<f64>| v.zip(equity).filter(|(_, e)| *e > 0.0).map(|(v, e)| v / e);
        let profit = sum(|r| r.today_profit);
        // Venue observations arrive independently. Daily PnL requires the same
        // Shanghai trading day, not identical millisecond timestamps.
        let same_day = rows[0].date_ms.is_some_and(|first| {
            rows.iter().all(|r| {
                r.date_ms.is_some_and(|ts| {
                    crate::account_overview::day_start(ts)
                        == crate::account_overview::day_start(first)
                })
            })
        });
        let oldest_date = rows
            .iter()
            .map(|r| r.date_ms)
            .collect::<Option<Vec<_>>>()
            .and_then(|dates| dates.into_iter().min());
        report.accounts.push(OverviewRow {
            source_id: id.clone(),
            account: account.name.clone(),
            venue: "spread".into(),
            equity,
            yesterday_equity: yesterday,
            today_profit: if same_day { profit } else { None },
            profit_percent: if same_day {
                profit
                    .zip(yesterday)
                    .filter(|(_, e)| *e > 0.0)
                    .map(|(p, e)| p / e * 100.0)
            } else {
                None
            },
            long_position: long,
            short_position: short,
            net_position: sum(|r| r.net_position),
            long_leverage: ratio(long),
            short_leverage: ratio(short),
            margin,
            margin_kind,
            margin_percent: ratio(margin).map(|v| v * 100.0),
            // Drawdown requires the combined history, never a sum of ratios.
            max_drawdown_15d_percent: None,
            position_unpriced_assets: rows
                .iter()
                .flat_map(|r| r.position_unpriced_assets.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            date_ms: oldest_date,
            queried_at_ms: rows
                .iter()
                .map(|r| r.queried_at_ms)
                .min()
                .unwrap_or_default(),
            leverage_alert: if rows.iter().any(|r| r.leverage_alert == Some(true)) {
                Some(true)
            } else if rows.iter().all(|r| r.leverage_alert == Some(false)) {
                Some(false)
            } else {
                None
            },
            timeout_alert: if rows.iter().any(|r| r.timeout_alert == Some(true)) {
                Some(true)
            } else if rows.iter().all(|r| r.timeout_alert == Some(false)) {
                Some(false)
            } else {
                None
            },
            history_start_ms: rows.iter().filter_map(|r| r.history_start_ms).min(),
            history_rebuilt_at_ms: rows
                .iter()
                .map(|r| r.history_rebuilt_at_ms)
                .collect::<Option<Vec<_>>>()
                .and_then(|v| v.into_iter().min()),
            account_kind: "cta".into(),
            detail_url: Some(format!("/manager/account/?source={id}")),
            collection_status: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> AppConfig {
        toml::from_str(
            r#"
[database]
url_env = "TEST_DATABASE_URL"
[accounts.spread]
name = "OKX / Binance Spread"
source_ids = ["okx", "bn"]
created_at_us = 1000000
[[sources]]
id = "okx"
account = "okx"
venue = "okex-futures"
rocksdb_path = "/account/okx/data/persist_manager"
[[sources]]
id = "bn"
account = "bn"
venue = "binance-futures"
rocksdb_path = "/account/bn/data/persist_manager"
[[sources]]
id = "other"
account = "other"
venue = "binance-futures"
rocksdb_path = "/account/other/data/persist_manager"
"#,
        )
        .unwrap()
    }

    #[test]
    fn one_public_account_resolves_to_both_execution_sources() {
        let config = config();
        validate(&config).unwrap();
        let mut allowed = BTreeSet::from(["okx".into(), "bn".into(), "other".into()]);
        scope_access(&config, &mut allowed);
        assert_eq!(public_ids(&config, &allowed), ["other", "spread"]);
        assert_eq!(expand(&config, &["spread".into()]).unwrap(), ["bn", "okx"]);
        for ids in [
            vec!["spread", "okx"],
            vec!["spread", "spread"],
            vec!["okx", "okx"],
            vec!["missing"],
        ] {
            assert!(
                expand(
                    &config,
                    &ids.into_iter().map(str::to_string).collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        assert_eq!(expand(&config, &["other".into()]).unwrap(), ["other"]);
    }

    #[test]
    fn partial_permission_or_disabled_leg_hides_the_entire_account() {
        let mut config = config();
        let mut allowed = BTreeSet::from(["okx".into(), "other".into()]);
        scope_access(&config, &mut allowed);
        assert_eq!(allowed, BTreeSet::from(["other".into()]));
        config.sources[1].enabled = false;
        let mut allowed = BTreeSet::from(["okx".into(), "bn".into(), "other".into()]);
        scope_access(&config, &mut allowed);
        assert_eq!(public_ids(&config, &allowed), ["other"]);
        assert!(expand(&config, &["spread".into()]).is_err());
    }

    #[test]
    fn identity_and_execution_ownership_are_unambiguous() {
        let mut config = config();
        let original = config.accounts["spread"].clone();
        for members in [vec!["okx"], vec!["okx", "okx"], vec!["okx", "missing"]] {
            config.accounts.get_mut("spread").unwrap().source_ids =
                members.into_iter().map(str::to_string).collect();
            assert!(validate(&config).is_err());
        }
        config.accounts.insert("spread".into(), original.clone());
        config.accounts.insert("another".into(), original.clone());
        assert!(validate(&config).is_err());
        config.accounts.remove("another");
        config.accounts.insert("okx".into(), original);
        assert!(validate(&config).is_err());
    }

    #[test]
    fn composite_daily_profit_accepts_independent_updates_within_one_shanghai_day() {
        use crate::account_overview::{OverviewResponse, OverviewRow, selected_day};
        let config = config();
        let day = selected_day("2026-11-04").unwrap();
        for (left, right, same_day) in [
            (Some(day + 1000), Some(day + 17_000), true),
            // UTC midnight is inside one Shanghai trading day.
            (
                Some(day + 8 * 3_600_000 - 1),
                Some(day + 8 * 3_600_000 + 1),
                true,
            ),
            (Some(day + 86_400_000 - 1), Some(day + 86_400_000), false),
            (Some(day), None, false),
            (None, None, false),
        ] {
            for (left_profit, right_profit) in [
                (Some(-3.0), Some(7.5)),
                (Some(0.0), Some(0.0)),
                (Some(1.0), None),
            ] {
                let mut report = OverviewResponse {
                    accounts: [
                        ("okx", left, 100.0, left_profit),
                        ("bn", right, 200.0, right_profit),
                    ]
                    .into_iter()
                    .map(|(id, date, baseline, profit)| OverviewRow {
                        source_id: id.into(),
                        date_ms: date,
                        yesterday_equity: Some(baseline),
                        today_profit: profit,
                        ..Default::default()
                    })
                    .collect(),
                    dates: vec![],
                    generated_at_ms: day,
                    alerts: Default::default(),
                    loading: false,
                    external_status: vec![],
                };
                project_overview(&config, &mut report);
                let expected = same_day
                    .then(|| left_profit.zip(right_profit).map(|(a, b)| a + b))
                    .flatten();
                assert_eq!(report.accounts[0].today_profit, expected);
                assert_eq!(
                    report.accounts[0].profit_percent,
                    expected.map(|v| v / 300.0 * 100.0)
                );
                assert_eq!(
                    report.accounts[0].date_ms,
                    left.zip(right).map(|(a, b)| a.min(b))
                );
            }
        }
    }

    #[test]
    fn composite_margin_requires_the_same_meaning_on_every_leg() {
        use crate::account_overview::{OverviewResponse, OverviewRow};
        let config = config();
        for (other_kind, expected) in [("ltp_frozen", Some(30.0)), ("initial", None)] {
            let mut report = OverviewResponse {
                accounts: [("okx", "ltp_frozen", 10.0), ("bn", other_kind, 20.0)]
                    .into_iter()
                    .map(|(id, kind, margin)| OverviewRow {
                        source_id: id.into(),
                        equity: Some(100.0),
                        margin: Some(margin),
                        margin_kind: Some(kind.into()),
                        ..Default::default()
                    })
                    .collect(),
                dates: vec![],
                generated_at_ms: 1,
                alerts: Default::default(),
                loading: false,
                external_status: vec![],
            };
            project_overview(&config, &mut report);
            assert_eq!(report.accounts[0].margin, expected);
            assert_eq!(
                report.accounts[0].margin_kind.as_deref(),
                expected.map(|_| "ltp_frozen")
            );
            assert_eq!(
                report.accounts[0].margin_percent,
                expected.map(|v| v / 200.0 * 100.0)
            );
        }
    }

    #[test]
    fn missing_overview_values_remain_unknown_and_ratios_use_combined_equity() {
        use crate::account_overview::{OverviewResponse, OverviewRow};
        let config = config();
        let row = |id: &str, equity, long| OverviewRow {
            source_id: id.into(),
            equity,
            long_position: Some(long),
            long_leverage: Some(999.0),
            max_drawdown_15d_percent: Some(30.0),
            ..Default::default()
        };
        let mut report = OverviewResponse {
            accounts: vec![
                row("okx", Some(100.0), 300.0),
                row("bn", Some(200.0), 100.0),
            ],
            dates: vec![],
            generated_at_ms: 1,
            alerts: Default::default(),
            loading: false,
            external_status: vec![],
        };
        project_overview(&config, &mut report);
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(report.accounts[0].source_id, "spread");
        assert_eq!(report.accounts[0].equity, Some(300.0));
        assert_eq!(report.accounts[0].long_leverage, Some(400.0 / 300.0));
        assert_eq!(report.accounts[0].max_drawdown_15d_percent, None);
        let mut report = OverviewResponse {
            accounts: vec![row("okx", Some(100.0), 300.0), row("bn", None, 100.0)],
            ..report
        };
        project_overview(&config, &mut report);
        assert_eq!(report.accounts[0].equity, None);
        assert_eq!(report.accounts[0].long_leverage, None);
    }
}
