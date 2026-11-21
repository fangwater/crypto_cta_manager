use super::*;
use crate::{
    account_overview::{OverviewAlertSettings, OverviewRow},
    postgres,
};
use axum::extract::Query;
use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Deserialize;
use std::{collections::BTreeSet, time::Duration};

const REFRESH_SECS: u64 = 15;
const NAV_RETRY_US: i64 = 60_000_000;
const NAV_RECHECK_US: i64 = 5 * NAV_RETRY_US;

/// Coverage of the Exec logs; both are append-only, so a later cut only adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NavCut {
    at_us: i64,
    historical: bool,
}

impl NavCut {
    fn observed(snapshot: &reader::Snapshot) -> Option<Self> {
        if snapshot
            .replication
            .as_ref()
            .is_some_and(|state| state.last_error.is_some())
        {
            return None;
        }
        Some(Self {
            at_us: snapshot.through_us?,
            historical: snapshot.replication_stale
                || snapshot
                    .replication
                    .as_ref()
                    .is_some_and(|replica| !reader::replica_caught_up(replica)),
        })
    }

    fn accepts(self, previous: Self) -> bool {
        self.historical == previous.historical && self.at_us >= previous.at_us
    }

    fn can_reuse(self, previous: Self) -> bool {
        self.accepts(previous)
            && (self.at_us == previous.at_us
                || (!self.historical && self.at_us / 60_000_000 == previous.at_us / 60_000_000))
    }
}

#[derive(Clone)]
struct CachedNav {
    cut: NavCut,
    anchor_fingerprint: String,
    report: Option<Arc<nav::Report>>,
    retry_at_us: Option<i64>,
}

impl CachedNav {
    fn reusable(&self, cut: NavCut, fingerprint: &str, now: i64) -> bool {
        cut.can_reuse(self.cut)
            && self.anchor_fingerprint == fingerprint
            && self.retry_at_us.is_none_or(|retry| now < retry)
    }
}

/// Account inventory replayed from the latest NAV anchor and the trade log.
#[derive(Clone, Debug, PartialEq)]
struct Holdings {
    cut: NavCut,
    anchor_us: i64,
    rows: BTreeMap<nav::Market, (i64, i64)>,
    notionals: Option<(f64, f64)>,
}

#[derive(Clone, Default)]
pub(super) struct Sample {
    pub(super) snapshot: Option<Arc<reader::Snapshot>>,
    attempted_at_us: Option<i64>,
    collection: Option<&'static str>,
    collection_reason: Option<&'static str>,
    failed: bool,
    holdings: Option<Holdings>,
    nav_cut: Option<NavCut>,
    nav: Option<CachedNav>,
}
impl Sample {
    pub(super) fn collection(&self) -> &'static str {
        self.collection.unwrap_or("loading")
    }

    fn position(&self) -> Option<PositionDisplay<'_>> {
        let holdings = self.holdings.as_ref()?;
        let reason = if self.collection() != "live" {
            self.collection_reason.or(Some("当前未能核验持仓"))
        } else if holdings.cut.historical || self.nav_cut != Some(holdings.cut) {
            Some("持仓为最近一次已同步执行记录的回放结果")
        } else {
            None
        };
        Some(PositionDisplay { holdings, reason })
    }

    fn position_reason(&self, position: Option<PositionDisplay<'_>>) -> Option<&'static str> {
        match position {
            Some(position) => position.reason,
            None => Some(
                self.collection_reason
                    .unwrap_or("尚无原生净值起始快照，无法由执行记录回放持仓"),
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct PositionDisplay<'a> {
    holdings: &'a Holdings,
    reason: Option<&'static str>,
}

impl PositionDisplay<'_> {
    fn status(self) -> &'static str {
        if self.reason.is_some() {
            "historical"
        } else {
            "live"
        }
    }
    fn count(self) -> usize {
        self.holdings.rows.len()
    }
}

pub struct Observations(BTreeMap<String, watch::Sender<Sample>>);

impl Observations {
    pub(in super::super) fn new(config: &Config) -> Self {
        Self(
            config
                .sources
                .iter()
                .filter(|s| s.enabled)
                .map(|s| (s.id.clone(), watch::channel(Sample::default()).0))
                .collect(),
        )
    }

    pub(super) fn sample(&self, id: &str) -> Sample {
        self.0[id].borrow().clone()
    }
}

pub(in super::super) fn start_observing(service: &Service) {
    for (id, updates) in &service.overview.0 {
        let service = service.clone();
        let id = id.clone();
        let updates = updates.clone();
        let mut shutdown = service.plan_shutdown.subscribe();
        tokio::spawn(async move {
            loop {
                let result = read_snapshot(&service, &id).await;
                let attempted_at_us = crate::native_futures::now_us();
                let mut sample = observation(updates.borrow().clone(), result, attempted_at_us);
                updates.send_replace(sample.clone());
                let budget = Duration::from_secs(REFRESH_SECS);
                let (holdings, nav) = tokio::join!(
                    tokio::time::timeout(budget, holdings(&service, &id, &sample)),
                    refresh_cumulative_nav(&service, &id, &sample, budget)
                );
                if let Ok(Some(holdings)) = holdings {
                    sample.holdings = Some(holdings);
                }
                sample.nav = nav;
                updates.send_replace(sample);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(REFRESH_SECS)) => {},
                    _ = shutdown.changed() => break,
                }
            }
        });
    }
}

fn observation(previous: Sample, result: Result<reader::Snapshot, ApiError>, now: i64) -> Sample {
    let mut sample = Sample {
        attempted_at_us: Some(now),
        ..previous
    };
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            sample.failed = error.0 != StatusCode::SERVICE_UNAVAILABLE;
            let (status, reason) = if sample.failed {
                ("unavailable", "执行记录读取失败")
            } else {
                ("loading", "副本正在初始化或提交同步")
            };
            sample.collection = Some(status);
            sample.collection_reason = Some(reason);
            sample.nav_cut = None;
            sample.nav = None;
            return sample;
        }
    };
    sample.failed = snapshot
        .replication
        .as_ref()
        .is_some_and(|s| s.last_error.is_some());
    let (status, reason) = collection(&snapshot);
    sample.collection = Some(status);
    sample.collection_reason = reason;
    sample.nav_cut = NavCut::observed(&snapshot);
    sample.nav = sample
        .nav
        .filter(|entry| sample.nav_cut.is_some_and(|cut| cut.can_reuse(entry.cut)));
    sample.snapshot = Some(Arc::new(snapshot));
    sample
}

fn collection(snapshot: &reader::Snapshot) -> (&'static str, Option<&'static str>) {
    if let Some(replica) = &snapshot.replication {
        if replica.last_error.is_some() {
            return ("stale", Some("远端副本同步失败"));
        }
        if snapshot.replication_stale {
            return ("stale", Some("远端副本同步逾期"));
        }
        if !reader::replica_caught_up(replica) {
            return ("loading", Some("执行记录正在同步"));
        }
    }
    ("live", None)
}

/// Replays the latest anchor's inventory with the factual fills after it.
async fn holdings(s: &Service, id: &str, sample: &Sample) -> Option<Holdings> {
    let cut = sample.nav_cut?;
    let anchor = nav::store::load(&s.pool, id, cut.at_us).await.ok()??;
    if let Some(previous) = sample
        .holdings
        .as_ref()
        .filter(|h| h.cut == cut && h.anchor_us == anchor.snapshot_us)
    {
        return Some(previous.clone());
    }
    let read = anchor.clone();
    let rows = with_nav_source(s, id, move |source, db, _| {
        let fills = nav::source::fills(source, db, &read, cut.at_us)?;
        nav::inventory(&read, &fills)
    })
    .await
    .ok()?
    .ok()?;
    let mut notionals = Some((0.0, 0.0));
    for ((venue, contract), (long, short)) in &rows {
        let value = async {
            let prices = s
                .market_data
                .as_ref()?
                .nav_prices(venue, contract, cut.at_us, cut.at_us, false)
                .await
                .ok()?;
            Some(prices.mark(cut.at_us).ok()? * prices.multiplier)
        }
        .await;
        notionals = notionals
            .zip(value)
            .map(|((l, s), lot)| (l + *long as f64 * lot, s + *short as f64 * lot));
    }
    Some(Holdings {
        cut,
        anchor_us: anchor.snapshot_us,
        rows,
        notionals: notionals.and_then(|(long, short)| finite_notionals(long, short)),
    })
}

fn finite_notionals(long: f64, short: f64) -> Option<(f64, f64)> {
    (long.is_finite() && short.is_finite()).then_some((long, short))
}

async fn refresh_cumulative_nav(
    s: &Service,
    id: &str,
    sample: &Sample,
    budget: Duration,
) -> Option<CachedNav> {
    let cut = sample.nav_cut?;
    let anchor = nav::store::first(&s.pool, id).await.ok()??;
    let anchor_fingerprint = crate::native_futures::netting::fingerprint(&anchor).ok()?;
    let now = crate::native_futures::now_us();
    if let Some(entry) = sample
        .nav
        .as_ref()
        .filter(|entry| entry.reusable(cut, &anchor_fingerprint, now))
    {
        return Some(entry.clone());
    }
    let report = tokio::time::timeout(budget, cumulative_nav(s, id, &anchor, cut))
        .await
        .ok()
        .flatten();
    let delay = match report.as_ref() {
        None => Some(NAV_RETRY_US),
        Some(r) if r.fees.missing_count > 0 => Some(NAV_RETRY_US),
        Some(r) if r.end_us / 1000 < cut.at_us / 1000 || r.fees.estimated_count > 0 => {
            Some(NAV_RECHECK_US)
        }
        Some(_) => None,
    };
    let retry = delay.map(|delay| crate::native_futures::now_us() + delay);
    Some(CachedNav {
        cut,
        anchor_fingerprint,
        report: report.map(Arc::new),
        retry_at_us: retry,
    })
}

async fn cumulative_nav(
    s: &Service,
    id: &str,
    anchor: &nav::Anchor,
    cut: NavCut,
) -> Option<nav::Report> {
    let start_ms = anchor.snapshot_us / 1000 + i64::from(anchor.snapshot_us % 1000 != 0);
    let end_ms = cut.at_us / 1000;
    if end_ms <= start_ms || (end_ms - start_ms) * 1000 > nav::MAX_WINDOW_US {
        return None;
    }
    let request = |end_ms| {
        nav_report(
            s,
            id,
            NavQuery {
                start_ms,
                end_ms,
                max_points: Some(2),
            },
        )
    };
    let mut report = request(end_ms).await.ok()?;
    // Retain the last covered monitor interval when the final partial
    // interval lacks finalized marks or complete fees.
    let step_ms = timeline::MONITOR_STEP_US / 1000;
    let closed_ms = end_ms.div_euclid(step_ms) * step_ms;
    if cut.historical
        && closed_ms > start_ms
        && closed_ms < end_ms
        && (!report.ready || report.fees.missing_count > 0)
        && let Ok(closed) = request(closed_ms).await
        && closed.ready
        && (!report.ready || closed.fees.missing_count == 0)
    {
        report = closed;
    }
    if !report.ready || report.snapshot_us != Some(anchor.snapshot_us) {
        return None;
    }
    // The async valuation may overlap a replica commit or source replacement.
    // Publish totals only for the same coverage state that requested them.
    let after = read_snapshot(s, id).await.ok()?;
    NavCut::observed(&after)
        .is_some_and(|latest| latest.accepts(cut))
        .then_some(report)
}

pub(super) async fn health(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
) -> ApiResult {
    let samples: Vec<_> = visible(&s, &access)
        .map(|src| s.overview.sample(&src.id))
        .collect();
    let healthy = samples.iter().all(|sample| sample.collection() == "live");
    let loading = samples
        .iter()
        .all(|sample| sample.collection() == "loading");
    body(json!({
        "status": if healthy { "ok" } else if loading { "loading" } else { "degraded" },
        "source_count": samples.len(), "generated_at_us": crate::native_futures::now_us(),
        "last_attempt_at_us": samples.iter().filter_map(|sample| sample.attempted_at_us).max(),
        "refresh_interval_secs": REFRESH_SECS,
        "last_refresh_error": samples.iter().any(|sample| sample.failed).then_some("部分账户执行记录暂不可读"),
    }))
}

#[derive(Deserialize)]
pub(super) struct OverviewQuery {
    date: Option<String>,
}

fn shanghai_date(at_us: i64) -> Option<NaiveDate> {
    let local = FixedOffset::east_opt(8 * 3600).unwrap();
    Some(
        DateTime::from_timestamp_micros(at_us)?
            .with_timezone(&local)
            .date_naive(),
    )
}

fn anchor_notionals(anchor: &nav::Anchor) -> Option<(f64, f64)> {
    let (mut long, mut short) = (0.0, 0.0);
    for position in &anchor.positions {
        let value = position.today.checked_add(position.yesterday)? as f64
            * position.reference_price
            * position.contract_multiplier;
        match position.side {
            nav::Side::Buy => long += value,
            nav::Side::Sell => short += value,
        }
    }
    finite_notionals(long, short)
}

#[derive(Serialize)]
struct NativeOverviewRow {
    #[serde(flatten)]
    row: OverviewRow,
    quote_currency: &'static str,
    collection_status_reason: Option<&'static str>,
    position_status: Option<&'static str>,
    position_as_of_ms: Option<i64>,
    position_historical_reason: Option<&'static str>,
}

pub(super) async fn overview(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Query(query): Query<OverviewQuery>,
) -> ApiResult {
    let selected_day = query
        .date
        .as_deref()
        .filter(|date| !date.is_empty())
        .map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| bad("日期须为 YYYY-MM-DD"))?;
    let aliases = postgres::load_source_aliases(&s.pool).await?;
    let now_us = crate::native_futures::now_us();
    let now_ms = now_us / 1000;
    let mut accounts = Vec::new();
    let mut dates = BTreeSet::new();
    for source in visible(&s, &access) {
        let sample = s.overview.sample(&source.id);
        let position = sample.position();
        let anchors = nav::store::list(&s.pool, &source.id).await?;
        dates.extend(anchors.iter().filter_map(|a| shanghai_date(a.snapshot_us)));
        dates.extend(position.and_then(|obs| shanghai_date(obs.holdings.cut.at_us)));
        let mut row = OverviewRow {
            source_id: source.id.clone(),
            account: aliases.get(&source.id).unwrap_or(&source.account).clone(),
            venue: source.adapter.clone(),
            account_kind: "cta".into(),
            queried_at_ms: sample.attempted_at_us.map_or(now_ms, |at| at / 1000),
            history_start_ms: anchors.last().map(|anchor| anchor.snapshot_us / 1000),
            ..Default::default()
        };
        let (status, notionals, position_status, position_at, position_reason) =
            if let Some(day) = selected_day {
                let anchor = anchors
                    .iter()
                    .find(|a| shanghai_date(a.snapshot_us) == Some(day));
                let observed =
                    position.filter(|obs| shanghai_date(obs.holdings.cut.at_us) == Some(day));
                let at = anchor
                    .map(|a| a.snapshot_us)
                    .or_else(|| observed.map(|obs| obs.holdings.cut.at_us));
                row.date_ms = at.map(|at| at / 1000);
                row.queried_at_ms = now_ms;
                (
                    if at.is_some() {
                        "historical"
                    } else {
                        "unavailable"
                    },
                    match anchor {
                        Some(anchor) => anchor_notionals(anchor),
                        None => observed.and_then(|obs| obs.holdings.notionals),
                    },
                    at.map(|_| "historical"),
                    at,
                    Some(if at.is_some() {
                        "所选日期的持仓观测"
                    } else {
                        "所选日期没有可验证的持仓"
                    }),
                )
            } else {
                let at = position.map(|obs| obs.holdings.cut.at_us);
                row.date_ms = at.map(|at| at / 1000);
                (
                    sample.collection(),
                    position.and_then(|obs| obs.holdings.notionals),
                    position.map(PositionDisplay::status),
                    at,
                    sample.position_reason(position),
                )
            };
        row.collection_status = Some(status.into());
        if let Some((long, short)) = notionals {
            row.long_position = Some(long);
            row.short_position = Some(short);
            row.net_position = Some(long - short);
        }
        accounts.push(NativeOverviewRow {
            row,
            quote_currency: "CNY",
            collection_status_reason: if selected_day.is_none() {
                sample.collection_reason
            } else {
                None
            },
            position_status,
            position_as_of_ms: position_at.map(|at| at / 1000),
            position_historical_reason: position_reason,
        });
    }
    let loading = !accounts.is_empty()
        && accounts
            .iter()
            .all(|row| row.row.collection_status.as_deref() == Some("loading"));
    body(json!({
        "loading": loading,
        "accounts": accounts,
        "dates": dates.into_iter().rev().map(|day| day.to_string()).collect::<Vec<_>>(),
        "generated_at_ms": now_ms,
        "alerts": OverviewAlertSettings::default(),
        "external_status": [],
    }))
}

pub(super) async fn dashboard(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Extension(user): Extension<AuthUser>,
) -> ApiResult {
    let aliases = postgres::load_source_aliases(&s.pool).await?;
    let scope = catalog::Scope::load(&s, &access, &user).await?;
    let now = crate::native_futures::now_us();
    let mut accounts = Vec::new();
    let mut reports = Vec::new();
    for source in visible(&s, &access) {
        let id = &source.id;
        let sample = s.overview.sample(id);
        let observed = sample.snapshot.as_deref();
        let account = aliases.get(id).unwrap_or(&source.account);
        let anchor = nav::store::first(&s.pool, id).await?;
        let first_anchor_us = anchor.as_ref().map(|anchor| anchor.snapshot_us);
        let can_configure = access.configure.contains(id);
        let position = sample.position();
        let current_position_count = position
            .filter(|obs| obs.reason.is_none())
            .map(PositionDisplay::count);
        let recent_position_count = position
            .filter(|obs| obs.reason.is_some())
            .map(PositionDisplay::count);
        let position_reason = sample.position_reason(position);
        let portfolio = crate::native_futures::netting::store::load(&s.pool, id).await?;
        let strategy_anchor = portfolio
            .filter(|p| p.targets.keys().all(|name| scope.can_view(name)))
            .map(|p| p.anchor.snapshot_us);
        let anchor_fingerprint = anchor
            .as_ref()
            .map(crate::native_futures::netting::fingerprint)
            .transpose()?;
        let cached = sample.nav.as_ref().filter(|entry| {
            sample.nav_cut.is_some_and(|cut| cut.can_reuse(entry.cut))
                && Some(entry.anchor_fingerprint.as_str()) == anchor_fingerprint.as_deref()
        });
        let report = cached.and_then(|entry| entry.report.as_deref());
        let summary = report.and_then(|r| r.summary.as_ref());
        let nav_as_of = summary.map(|point| point.timestamp_us);
        let nav_is_historical = cached.zip(nav_as_of).is_some_and(|(entry, as_of)| {
            entry.cut.historical || as_of / 1000 < entry.cut.at_us / 1000
        });
        let fees = report.map(|r| &r.fees);
        let last_fill = observed
            .and_then(|snapshot| snapshot.fills.first())
            .map(|fill| fill.received_us);
        let first_fill = observed
            .filter(|snapshot| snapshot.fills.len() == snapshot.total_fills)
            .and_then(|snapshot| snapshot.fills.last())
            .map(|fill| fill.received_us);
        // Account configure grants govern aliases and access lists. The
        // domestic allocator uses its own native execution plan interface.
        accounts.push(json!({
            "source_id": id, "account": account, "venue": source.adapter, "enabled": true,
            "gateway_prefix": null, "configurable": false, "target_proxy_enabled": false,
            "execution_monitor_url": s.config.execution_monitor_urls.get(id),
            "access_level": if can_configure { "configure" } else { "view" },
            "can_edit_alias": can_configure, "display_alias": aliases.get(id),
            "open_position_symbol_count": current_position_count,
            "recent_position_symbol_count": recent_position_count,
            "position_status": position.map(PositionDisplay::status),
            "position_observed_ts_us": position.map(|obs| obs.holdings.cut.at_us),
            "position_historical_reason": position_reason,
            "account_pnl_start_ts_us": first_anchor_us,
            "strategy_pnl_start_ts_us": strategy_anchor,
            "account_pnl_only": strategy_anchor.is_none(),
            "live_equity_quote": null, "live_equity_status": "unavailable",
            "quote_currency": "CNY", "collection_status": sample.collection(),
            "collection_status_reason": sample.collection_reason,
            "execution_observed_ts_us": observed.and_then(|snapshot| snapshot.last_record_us),
        }));
        reports.push(json!({
            "source_id": id, "account": account, "configured_venue": source.adapter,
            "estimated_fee_rate": null, "maker_fee_rate": null, "taker_fee_rate": null, "quote_fee_rates": {},
            "initial_position_snapshot_ts_us": first_anchor_us,
            "initial_position_count": anchor.as_ref().map(|anchor| anchor.positions.len()),
            "order_event_count": observed.map(|snapshot| snapshot.total_orders),
            "fill_count": observed.map(|snapshot| snapshot.total_fills),
            "volume_quote": fees.map(|f| f.volume_cny),
            "unknown_liquidity_fill_count": fees.map(|f| f.fill_count),
            "unknown_liquidity_volume_quote": fees.map(|f| f.volume_cny),
            "first_fill_ts_us": first_fill, "last_fill_ts_us": last_fill,
            "realized_pnl_before_fee_quote": summary.map(|point| point.realized_cny),
            "realized_pnl_after_fee_quote": summary.and_then(|point| {
                point.fees_cny.map(|fees| point.realized_cny - fees)
            }),
            "floating_pnl_quote": summary.map(|point| point.floating_cny),
            "estimated_trading_fee_quote": summary.and_then(|point| point.fees_cny),
            "nav_change_before_fee_quote": summary.map(|point| point.nav_before_fees_cny),
            "nav_change_after_fee_quote": summary.and_then(|point| point.nav_after_fees_cny),
            "nav_as_of_ts_us": nav_as_of, "nav_is_historical": nav_is_historical,
            "symbols": [],
        }));
    }
    body(json!({
        "generated_at_us": now, "generation_duration_ms": (crate::native_futures::now_us() - now) / 1000,
        "refresh_interval_secs": REFRESH_SECS, "accounts": accounts,
        "report": { "valuation": "native_lot_fifo_completed_minute_close", "source_count": reports.len(), "aggregate": { "symbols": [] }, "sources": reports },
    }))
}

#[cfg(test)]
mod tests;
