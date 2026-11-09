//! Account projection is applied after native-source reads and permissions.
use super::*;

pub(super) fn project(
    config: &AppConfig,
    dashboard: &mut DashboardSnapshot,
    allowed: &BTreeSet<String>,
) {
    let original = std::mem::take(&mut dashboard.accounts);
    dashboard.accounts = original
        .iter()
        .filter(|s| crate::managed_accounts::parent_id(config, &s.source_id).is_none())
        .cloned()
        .collect();
    for (id, spec) in &config.accounts {
        if !allowed.contains(id) {
            continue;
        }
        let Some(legs) = spec
            .source_ids
            .iter()
            .map(|id| original.iter().find(|a| &a.source_id == id).cloned())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let mut account = legs[0].clone();
        account.source_id = id.clone();
        account.account = spec.name.clone();
        account.venue = "spread".into();
        account.gateway_prefix = None;
        account.can_edit_alias = true;
        account.display_alias = None;
        account.configurable = legs.iter().all(|a| a.configurable);
        account.target_proxy_enabled = false;
        account.access_level = Some(
            if legs.iter().all(|a| a.access_level == Some("configure")) {
                "configure"
            } else {
                "view"
            },
        );
        account.account_pnl_start_ts_us = legs
            .iter()
            .filter_map(|a| a.account_pnl_start_ts_us)
            .min()
            .or(Some(spec.created_at_us));
        account.strategy_pnl_start_ts_us = legs
            .iter()
            .filter_map(|a| a.strategy_pnl_start_ts_us)
            .min()
            .or(Some(spec.created_at_us));
        account.live_equity_usdt = legs
            .iter()
            .map(|a| a.live_equity_usdt)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.iter().sum());
        account.live_equity_status =
            Some(if legs.iter().all(|a| a.live_equity_status == Some("ok")) {
                "ok"
            } else {
                "stale"
            });
        // Margin ratios and funding switches remain execution-source settings.
        account.unified_account = false;
        account.uni_mmr = None;
        account.uni_mmr_status = None;
        account.funding = Default::default();
        account.total_nav_before_fee_quote = legs
            .iter()
            .map(|a| a.total_nav_before_fee_quote)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.iter().sum());
        account.total_nav_after_fee_quote = legs
            .iter()
            .map(|a| a.total_nav_after_fee_quote)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.iter().sum());
        account.cumulative_funding = if legs.iter().all(|a| a.cumulative_funding.is_some()) {
            let funding = legs
                .iter()
                .filter_map(|a| a.cumulative_funding.as_ref())
                .collect::<Vec<_>>();
            Some(cumulative_funding::CumulativeFunding {
                kind: if funding.iter().all(|f| f.kind == funding[0].kind) {
                    funding[0].kind.clone()
                } else {
                    "mixed".into()
                },
                income_quote: funding
                    .iter()
                    .map(|f| f.income_quote)
                    .collect::<Option<Vec<_>>>()
                    .map(|v| v.iter().sum()),
                complete: funding.iter().all(|f| f.complete),
                loading: funding.iter().any(|f| f.loading),
                refreshing: funding.iter().any(|f| f.refreshing),
                unavailable_reason: funding.iter().find_map(|f| f.unavailable_reason.clone()),
                start_ts_us: funding
                    .iter()
                    .map(|f| f.start_ts_us)
                    .min()
                    .unwrap_or_default(),
                end_ts_us: funding
                    .iter()
                    .map(|f| f.end_ts_us)
                    .min()
                    .unwrap_or_default(),
                generation_us: dashboard.generated_at_us,
            })
        } else {
            None
        };
        account.execution_sources = legs;
        dashboard.accounts.push(account);
    }
    dashboard.report = crate::managed_accounts::project_report(config, &dashboard.report);
}

/// Apply display metadata after account projection, without changing identity.
pub(super) fn apply_aliases(dashboard: &mut DashboardSnapshot, aliases: &BTreeMap<String, String>) {
    for account in &mut dashboard.accounts {
        account.display_alias = aliases.get(&account.source_id).cloned();
        if let Some(alias) = &account.display_alias {
            account.account.clone_from(alias);
        }
    }
    for report in &mut dashboard.report.sources {
        if let Some(alias) = aliases.get(&report.source_id) {
            report.account.clone_from(alias);
        }
    }
}
