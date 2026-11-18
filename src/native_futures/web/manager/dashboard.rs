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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NavCut {
    at_us: i64,
    sequence: i64,
    revision: u64,
    historical: bool,
    stopped: bool,
}

impl NavCut {
    fn observed(snapshot: &reader::Snapshot) -> Option<Self> {
        if snapshot.projection_errors != 0
            || snapshot
                .replication
                .as_ref()
                .is_some_and(|state| state.last_error.is_some())
        {
            return None;
        }
        Some(Self {
            at_us: snapshot.nav_anchor_observed_us?,
            sequence: snapshot.nav_anchor_evidence_sequence?,
            revision: snapshot.nav_evidence_revision,
            historical: snapshot.status != "observing"
                || snapshot.replication_stale
                || !snapshot.snapshot_ready
                || !snapshot.snapshot_current_runtime
                || snapshot.positions_may_lag,
            stopped: snapshot.status == "stopped",
        })
    }

    fn accepts(self, previous: Self) -> bool {
        self.revision == previous.revision
            && self.historical == previous.historical
            && self.stopped == previous.stopped
            && self.sequence >= previous.sequence
            && self.at_us >= previous.at_us
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

#[derive(Clone, Default)]
pub(super) struct Sample {
    pub(super) snapshot: Option<Arc<reader::Snapshot>>,
    attempted_at_us: Option<i64>,
    collection: Option<&'static str>,
    failed: bool,
    notionals: Option<(f64, f64)>,
    nav_cut: Option<NavCut>,
    nav: Option<CachedNav>,
}
impl Sample {
    pub(super) fn collection(&self) -> &'static str {
        self.collection.unwrap_or("loading")
    }

    fn current(&self) -> Option<&reader::Snapshot> {
        self.snapshot.as_deref().filter(|snapshot| {
            self.collection() == "live"
                && snapshot.snapshot_ready
                && snapshot.snapshot_current_runtime
                && !snapshot.positions_may_lag
        })
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
                let (notionals, nav) = tokio::join!(
                    tokio::time::timeout(budget, position_notionals(&service, &sample)),
                    refresh_cumulative_nav(&service, &id, &sample, budget)
                );
                sample.notionals = notionals.ok().flatten();
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
            sample.collection = Some(match (sample.snapshot.is_some(), error.0) {
                (true, _) => "stale",
                (false, StatusCode::SERVICE_UNAVAILABLE) => "loading",
                (false, _) => "unavailable",
            });
            sample.failed = error.0 != StatusCode::SERVICE_UNAVAILABLE;
            sample.notionals = None;
            sample.nav_cut = None;
            sample.nav = None;
            return sample;
        }
    };
    sample.failed = snapshot.projection_errors != 0
        || snapshot
            .replication
            .as_ref()
            .is_some_and(|s| s.last_error.is_some());
    sample.collection = Some(
        if snapshot.replication_stale || snapshot.status == "stale" {
            "stale"
        } else if sample.failed {
            "unavailable"
        } else if snapshot.last_event_us.is_none() || snapshot.status == "recovering" {
            "loading"
        } else if snapshot.status == "stopped" || snapshot.status == "disconnected" {
            "stale"
        } else {
            "live"
        },
    );
    let same_position = sample.current().is_some_and(|previous| {
        previous.state_observed_us == snapshot.state_observed_us
            && previous.positions == snapshot.positions
            && previous.total_fills == snapshot.total_fills
    }) && snapshot.snapshot_ready
        && snapshot.snapshot_current_runtime
        && !snapshot.positions_may_lag;
    if !same_position {
        sample.notionals = None;
    }
    sample.nav_cut = NavCut::observed(&snapshot);
    sample.nav = sample
        .nav
        .filter(|entry| sample.nav_cut.is_some_and(|cut| cut.can_reuse(entry.cut)));
    sample.snapshot = Some(Arc::new(snapshot));
    sample
}

fn positions(snapshot: &reader::Snapshot) -> Option<BTreeMap<(&str, &str), (i64, i64)>> {
    let mut result = BTreeMap::new();
    for row in &snapshot.positions {
        let venue = row["venue"].as_str()?;
        let contract = row["symbol"].as_str()?;
        let lots = row["position"]["today"]
            .as_i64()?
            .checked_add(row["position"]["yesterday"].as_i64()?)?;
        if lots > 0 {
            let sides = result.entry((venue, contract)).or_insert((0, 0));
            match row["side"].as_str()? {
                "buy" => sides.0 = lots,
                "sell" => sides.1 = lots,
                _ => return None,
            }
        }
    }
    Some(result)
}

async fn position_notionals(s: &Service, sample: &Sample) -> Option<(f64, f64)> {
    if let Some(values) = sample.notionals {
        return Some(values);
    }
    let snapshot = sample.current()?;
    let at = snapshot.state_observed_us?;
    let (mut long_cny, mut short_cny) = (0.0, 0.0);
    for ((venue, contract), (long, short)) in positions(snapshot)? {
        let prices = s
            .market_data
            .as_ref()?
            .nav_prices(venue, contract, at, at)
            .await
            .ok()?;
        let lot_value = prices.mark(at).ok()? * prices.multiplier;
        long_cny += long as f64 * lot_value;
        short_cny += short as f64 * lot_value;
    }
    finite_notionals(long_cny, short_cny)
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
    // Publish totals only for the same evidence cut that requested them.
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
    let now_ms = crate::native_futures::now_us() / 1000;
    let mut accounts = Vec::new();
    let mut dates = BTreeSet::new();
    for source in visible(&s, &access) {
        let sample = s.overview.sample(&source.id);
        let anchors = nav::store::list(&s.pool, &source.id).await?;
        dates.extend(anchors.iter().filter_map(|a| shanghai_date(a.snapshot_us)));
        let mut row = OverviewRow {
            source_id: source.id.clone(),
            account: aliases.get(&source.id).unwrap_or(&source.account).clone(),
            venue: source.adapter.clone(),
            account_kind: "cta".into(),
            queried_at_ms: sample.attempted_at_us.map_or(now_ms, |at| at / 1000),
            history_start_ms: anchors.last().map(|anchor| anchor.snapshot_us / 1000),
            ..Default::default()
        };
        let (status, notionals) = if let Some(day) = selected_day {
            let anchor = anchors
                .iter()
                .find(|a| shanghai_date(a.snapshot_us) == Some(day));
            row.date_ms = anchor.map(|a| a.snapshot_us / 1000);
            row.queried_at_ms = now_ms;
            (
                if anchor.is_some() {
                    "historical"
                } else {
                    "unavailable"
                },
                anchor.and_then(anchor_notionals),
            )
        } else {
            row.date_ms = sample
                .current()
                .and_then(|snapshot| snapshot.state_observed_us)
                .map(|at| at / 1000);
            (sample.collection(), sample.current().and(sample.notionals))
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
        let position_count = sample
            .current()
            .and_then(positions)
            .map(|positions| positions.len());
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
            .and_then(|fill| fill["received_us"].as_i64());
        let first_fill = observed
            .filter(|snapshot| snapshot.fills.len() == snapshot.total_fills)
            .and_then(|snapshot| snapshot.fills.last())
            .and_then(|fill| fill["received_us"].as_i64());
        // Account configure grants govern aliases and access lists. The
        // domestic allocator uses its own native execution plan interface.
        accounts.push(json!({
            "source_id": id, "account": account, "venue": source.adapter, "enabled": true,
            "gateway_prefix": null, "configurable": false, "target_proxy_enabled": false,
            "access_level": if can_configure { "configure" } else { "view" },
            "can_edit_alias": can_configure, "display_alias": aliases.get(id),
            "open_position_symbol_count": position_count, "account_pnl_start_ts_us": first_anchor_us,
            "strategy_pnl_start_ts_us": strategy_anchor,
            "account_pnl_only": strategy_anchor.is_none(),
            "quote_currency": "CNY", "collection_status": sample.collection(),
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
mod tests {
    use super::*;

    #[test]
    fn cached_cumulative_nav_keeps_stopped_cut_without_live_positions() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let primary = tmp.path().join("exec");
        let mut options = rocksdb::Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db = rocksdb::DB::open_cf(
            &options,
            &primary,
            ["cn_trading_events", "cn_uniform_orders", "cn_trade_facts"],
        )?;
        let source = Source {
            id: "account".into(),
            account: "test".into(),
            adapter: "yida".into(),
            rocksdb_path: primary.clone(),
            enabled: true,
            sync: None,
        };
        let now = crate::native_futures::now_us();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/yida_native_records.json"
        ))?;
        let mut state = fixture["state"].clone();
        state["received_us"] = json!(now);
        let events = db
            .cf_handle("cn_trading_events")
            .context("test events CF")?;
        db.put_cf(events, 100_i64.to_be_bytes(), serde_json::to_vec(&state)?)?;
        let secondary =
            crate::rocks_source::ReaderPool::new(&tmp.path().join("secondary"), &[&primary])?;
        let mut cache = reader::Cache::default();
        let mut read =
            || reader::with_db(&secondary, &source, |db| cache.read(&source, db, 10, 30));

        let mut sample = observation(Sample::default(), Ok(read()?), now);
        assert!(sample.current().is_some());
        let active = sample.nav_cut.context("reconciled cutoff")?;
        assert!(!active.historical);
        sample.nav = Some(CachedNav {
            cut: active,
            anchor_fingerprint: "initial-anchor".into(),
            report: None,
            retry_at_us: Some(now + NAV_RETRY_US),
        });
        sample.notionals = Some((1.0, 0.0));

        let stop = json!({"sequence":101,"received_us":now + 1,"value":{"type":"runtime_stopped"}});
        db.put_cf(events, 101_i64.to_be_bytes(), serde_json::to_vec(&stop)?)?;
        let mut sample = observation(sample, Ok(read()?), now + 1);
        let stopped = sample
            .nav_cut
            .context("stopped account's historical cutoff")?;
        assert_eq!(stopped.at_us, active.at_us);
        assert_eq!(stopped.sequence, active.sequence);
        assert_eq!(stopped.revision, active.revision);
        assert!(stopped.historical && stopped.stopped);
        assert!(sample.current().is_none());
        assert!(sample.notionals.is_none());
        assert!(sample.nav.is_none());

        let saved = CachedNav {
            cut: stopped,
            anchor_fingerprint: "initial-anchor".into(),
            report: None,
            retry_at_us: Some(now + NAV_RETRY_US),
        };
        assert!(saved.reusable(stopped, "initial-anchor", now + 1));
        assert!(!saved.reusable(stopped, "new-anchor", now + 1));
        assert!(!saved.reusable(stopped, "initial-anchor", now + NAV_RETRY_US));
        assert!(!saved.reusable(
            NavCut {
                revision: stopped.revision + 1,
                ..stopped
            },
            "initial-anchor",
            now + 1
        ));
        sample.nav = Some(saved);

        let mut failed = read()?;
        failed.replication = Some(crate::native_futures::sync::ReplicaStatus {
            last_error: Some("test synchronization failure".into()),
            ..Default::default()
        });
        let failed = observation(sample, Ok(failed), now + 2);
        assert!(failed.current().is_none());
        assert!(failed.nav_cut.is_none());
        assert!(failed.nav.is_none());
        Ok(())
    }
}
