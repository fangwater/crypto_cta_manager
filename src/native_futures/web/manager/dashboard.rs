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
    collection_reason: Option<&'static str>,
    failed: bool,
    notionals: Option<(f64, f64)>,
    nav_cut: Option<NavCut>,
    nav: Option<CachedNav>,
}
impl Sample {
    pub(super) fn collection(&self) -> &'static str {
        self.collection.unwrap_or("loading")
    }

    fn execution_status(&self) -> Option<&str> {
        self.snapshot.as_ref().map(|snapshot| {
            if snapshot.status == "stale" {
                "quiet"
            } else {
                snapshot.status.as_str()
            }
        })
    }

    fn position(&self) -> Option<PositionDisplay<'_>> {
        let observation = self.snapshot.as_ref()?.position_observation.as_ref()?;
        let reason = match self.execution_status() {
            Some("stopped") => Some("执行已停止"),
            Some("disconnected") => Some("柜台已断线"),
            _ if self.collection() != "live" => self.collection_reason.or(Some("当前未能核验持仓")),
            _ if !observation.current_runtime => Some("尚未收到本次连接的完整持仓对账"),
            _ if observation.later_fills => Some("该持仓对账之后已有成交，等待新的完整观测"),
            _ => None,
        };
        Some(PositionDisplay {
            observation,
            reason,
        })
    }

    fn position_reason(&self, position: Option<PositionDisplay<'_>>) -> Option<&'static str> {
        match position {
            Some(position) => position.reason,
            None => Some(self.collection_reason.unwrap_or("尚无可验证的完整持仓对账")),
        }
    }

    fn funds(&self, now_us: i64, stale_secs: u64) -> Option<FundsObservation<'_>> {
        let snapshot = self.snapshot.as_deref()?;
        let funds = snapshot.funds.as_ref()?;
        let stale_us = i64::try_from(stale_secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000_000);
        let historical_reason = if snapshot.status == "stopped" {
            Some("执行已停止")
        } else if snapshot.status == "disconnected" {
            Some("柜台已断线")
        } else if self.collection() != "live" {
            self.collection_reason
                .or(Some("当前未收到可验证的账户资金"))
        } else if !snapshot.funds_current_runtime {
            Some("当前运行尚未收到可验证的账户资金")
        } else if funds.observed_us > now_us {
            Some("资金回报时间待核验")
        } else if funds.source == reader::AccountFundsSource::CtpTradingAccount
            && now_us.saturating_sub(funds.observed_us) > stale_us
        {
            Some("CTP 周期资金查询逾期")
        } else if snapshot.funds_stale {
            Some("最近资金回报未通过校验")
        } else {
            None
        };
        Some(FundsObservation {
            funds,
            historical_reason,
        })
    }
}

#[derive(Clone, Copy)]
struct PositionDisplay<'a> {
    observation: &'a reader::PositionObservation,
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
}

#[derive(Clone, Copy)]
struct FundsObservation<'a> {
    funds: &'a reader::AccountFunds,
    historical_reason: Option<&'static str>,
}

impl FundsObservation<'_> {
    fn status(self) -> &'static str {
        if self.historical_reason.is_none() {
            "live"
        } else {
            "historical"
        }
    }

    fn with_historical_date(self) -> Self {
        Self {
            historical_reason: Some("所选日期的资金观察"),
            ..self
        }
    }

    fn sdk_market_value(self) -> Option<f64> {
        self.funds
            .native_fields
            .get("sdk_market_value")
            .copied()
            .filter(|_| {
                self.funds.valuation_basis.as_deref() == Some("sdk_mark_time_unverified")
                    && self.funds.currency.as_deref() == Some("CNY")
            })
    }

    fn card(self) -> Value {
        let fields = &self.funds.native_fields;
        json!({
            "source": self.funds.source, "status": self.status(), "historical_reason": self.historical_reason,
            "observed_us": self.funds.observed_us, "trading_day": self.funds.trading_day,
            "currency": self.funds.currency, "valuation_basis": self.funds.valuation_basis,
            "previous_balance": self.funds.previous_balance,
            "available": self.funds.available, "usable_cash": self.funds.usable_cash,
            "sdk_cash_balance": fields.get("sdk_cash_balance"), "sdk_market_value": self.sdk_market_value(),
        })
    }

    fn apply_to(self, row: &mut OverviewRow) {
        row.date_ms = Some(self.funds.observed_us / 1000);
        if self.funds.currency.as_deref() != Some("CNY") {
            return;
        }
        row.equity = self.funds.balance;
        row.margin = self.funds.margin;
        row.margin_kind = row.margin.map(|_| "initial".into());
        row.margin_percent = self
            .funds
            .balance
            .filter(|balance| *balance > 0.0)
            .zip(self.funds.margin)
            .map(|(balance, margin)| margin / balance * 100.0)
            .filter(|fraction| fraction.is_finite());
        // The broker's previous settlement balance uses the trading day.
        // The shared dashboard's previous equity and profit use Shanghai midnight.
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
    sample.failed = snapshot.projection_errors != 0
        || snapshot
            .replication
            .as_ref()
            .is_some_and(|s| s.last_error.is_some());
    let (status, reason) = collection(&snapshot);
    sample.collection = Some(status);
    sample.collection_reason = reason;
    let same_position = sample
        .snapshot
        .as_ref()
        .and_then(|old| old.position_observation.as_ref())
        .zip(snapshot.position_observation.as_ref())
        .is_some_and(|(old, current)| {
            old.evidence_sequence == current.evidence_sequence
                && old.observed_us == current.observed_us
                && old.rows == current.rows
        });
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

fn collection(snapshot: &reader::Snapshot) -> (&'static str, Option<&'static str>) {
    if snapshot.projection_errors != 0 {
        return ("unavailable", Some("执行证据校验异常"));
    }
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
    if snapshot.last_event_us.is_none() {
        return ("loading", Some("尚无执行事件"));
    }
    ("live", None)
}

fn positions(rows: &[Value]) -> Option<BTreeMap<(&str, &str), (i64, i64)>> {
    let mut result = BTreeMap::new();
    for row in rows {
        let venue = row["venue"].as_str()?;
        let contract = row["symbol"].as_str().filter(|symbol| !symbol.is_empty())?;
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
    if sample.collection() != "live" {
        return None;
    }
    let position = sample.position()?.observation;
    let at = position.observed_us;
    let (mut long_cny, mut short_cny) = (0.0, 0.0);
    for ((venue, contract), (long, short)) in positions(&position.rows)? {
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
    collection_status_reason: Option<&'static str>,
    execution_status: Option<String>,
    funds_status: Option<&'static str>,
    funds_as_of_ms: Option<i64>,
    funds_historical_reason: Option<&'static str>,
    funds_trading_day: Option<String>,
    sdk_market_value: Option<f64>,
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
        let mut funds = sample.funds(now_us, s.config.stale_secs);
        let position = sample.position();
        let anchors = nav::store::list(&s.pool, &source.id).await?;
        dates.extend(anchors.iter().filter_map(|a| shanghai_date(a.snapshot_us)));
        dates.extend(funds.and_then(|obs| shanghai_date(obs.funds.observed_us)));
        dates.extend(position.and_then(|obs| shanghai_date(obs.observation.observed_us)));
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
                    position.filter(|obs| shanghai_date(obs.observation.observed_us) == Some(day));
                funds = funds
                    .filter(|obs| shanghai_date(obs.funds.observed_us) == Some(day))
                    .map(FundsObservation::with_historical_date);
                let at = anchor
                    .map(|a| a.snapshot_us)
                    .or_else(|| observed.map(|obs| obs.observation.observed_us));
                row.date_ms = at.map(|at| at / 1000);
                row.queried_at_ms = now_ms;
                (
                    if at.is_some() || funds.is_some() {
                        "historical"
                    } else {
                        "unavailable"
                    },
                    match anchor {
                        Some(anchor) => anchor_notionals(anchor),
                        None => observed.and(sample.notionals),
                    },
                    at.map(|_| "historical"),
                    at,
                    Some(if at.is_some() {
                        "所选日期的持仓观测"
                    } else {
                        "所选日期没有可验证的完整持仓对账"
                    }),
                )
            } else {
                let at = position.map(|obs| obs.observation.observed_us);
                row.date_ms = at.map(|at| at / 1000);
                (
                    sample.collection(),
                    position.and(sample.notionals),
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
        if let Some(obs) = funds {
            obs.apply_to(&mut row);
        }
        let (collection_status_reason, execution_status) = if selected_day.is_none() {
            (
                sample.collection_reason,
                sample.execution_status().map(str::to_owned),
            )
        } else {
            (None, None)
        };
        accounts.push(NativeOverviewRow {
            row,
            quote_currency: "CNY",
            collection_status_reason,
            execution_status,
            funds_status: funds.map(FundsObservation::status),
            funds_as_of_ms: funds.map(|obs| obs.funds.observed_us / 1000),
            funds_historical_reason: funds.and_then(|obs| obs.historical_reason),
            funds_trading_day: funds.and_then(|obs| obs.funds.trading_day.clone()),
            sdk_market_value: funds.and_then(FundsObservation::sdk_market_value),
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
        let funds = sample.funds(now, s.config.stale_secs);
        let equity = funds
            .filter(|obs| obs.funds.currency.as_deref() == Some("CNY"))
            .and_then(|obs| obs.funds.balance);
        let equity_status = funds
            .filter(|_| equity.is_some())
            .map(|obs| {
                if obs.status() == "live" {
                    "ok"
                } else {
                    "stale"
                }
            })
            .unwrap_or("unavailable");
        let observed = sample.snapshot.as_deref();
        let account = aliases.get(id).unwrap_or(&source.account);
        let anchor = nav::store::first(&s.pool, id).await?;
        let first_anchor_us = anchor.as_ref().map(|anchor| anchor.snapshot_us);
        let can_configure = access.configure.contains(id);
        let position = sample.position();
        let position_count = position
            .and_then(|obs| positions(&obs.observation.rows))
            .map(|set| set.len());
        let current_position_count = position
            .filter(|obs| obs.reason.is_none())
            .and(position_count);
        let recent_position_count = position
            .filter(|obs| obs.reason.is_some())
            .and(position_count);
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
            "open_position_symbol_count": current_position_count,
            "recent_position_symbol_count": recent_position_count,
            "position_status": position.map(PositionDisplay::status),
            "position_observed_ts_us": position.map(|obs| obs.observation.observed_us),
            "position_historical_reason": position_reason,
            "account_pnl_start_ts_us": first_anchor_us,
            "strategy_pnl_start_ts_us": strategy_anchor,
            "account_pnl_only": strategy_anchor.is_none(),
            "live_equity_quote": equity, "live_equity_status": equity_status,
            "account_funds": funds.map(FundsObservation::card),
            "quote_currency": "CNY", "collection_status": sample.collection(),
            "collection_status_reason": sample.collection_reason,
            "execution_status": sample.execution_status(),
            "execution_observed_ts_us": observed.and_then(|snapshot| snapshot.last_event_us),
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
    fn funds_keep_broker_time_and_basis_when_settlement_blocks_execution_or_runtime_stops()
    -> Result<()> {
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
            id: "test-ctp".into(),
            account: "test".into(),
            adapter: "ctp".into(),
            rocksdb_path: primary.clone(),
            enabled: true,
            sync: None,
        };
        let now = crate::native_futures::now_us();
        let observed = now - 2_000_000;
        let events = db
            .cf_handle("cn_trading_events")
            .context("test events CF")?;
        let put = |sequence: i64, at: i64, value: Value| -> Result<()> {
            db.put_cf(
                events,
                sequence.to_be_bytes(),
                serde_json::to_vec(&json!({"sequence":sequence,"received_us":at,"value":value}))?,
            )?;
            Ok(())
        };
        put(
            10,
            observed - 1,
            json!({"type":"reconciled","state":{
                "ready":false,"connected":true,"positions":[],"available_local_budget":99.0,
                "reason":"settlement not confirmed","trading_day":"20261117"
            }}),
        )?;
        let mut amounts = vec![0.0; 44];
        amounts[3] = 1200.0;
        amounts[12] = 250.0;
        amounts[17] = 1250.0;
        amounts[18] = 800.0;
        put(
            11,
            observed + 10,
            json!({"type":"ctp_event","event":{
                "received_us":observed,"kind":7,"exchange":"CNY","trading_day":"20261117",
                "request_id":4,"error_code":0,"present":1,"last":1,"available":800.0,
                "metadata":{"account_values":amounts}
            }}),
        )?;
        let secondary =
            crate::rocks_source::ReaderPool::new(&tmp.path().join("secondary"), &[&primary])?;
        let mut cache = reader::Cache::default();
        let mut read =
            || reader::with_db(&secondary, &source, |db| cache.read(&source, db, 10, 30));
        let sample = observation(Sample::default(), Ok(read()?), now);
        assert_eq!(
            sample
                .position()
                .context("read-only reconciliation")?
                .status(),
            "live"
        );
        let funds = sample.funds(now, 30).context("fresh broker observation")?;
        assert_eq!(funds.status(), "live");
        assert_eq!(funds.funds.observed_us, observed);
        let mut row = OverviewRow::default();
        funds.apply_to(&mut row);
        assert_eq!(row.date_ms, Some(observed / 1000));
        assert_eq!(row.equity, Some(1250.0));
        assert_eq!(row.margin, Some(250.0));
        assert_eq!(row.margin_percent, Some(20.0));
        assert_eq!(funds.card()["previous_balance"], 1200.0);
        assert!(row.yesterday_equity.is_none());
        assert!(row.today_profit.is_none());
        let late = sample.funds(now + 30_000_000, 30).context("aged query")?;
        assert_eq!(late.status(), "historical");
        assert_eq!(late.historical_reason, Some("CTP 周期资金查询逾期"));

        put(12, now, json!({"type":"runtime_stopped"}))?;
        let stopped = observation(sample, Ok(read()?), now);
        let last = stopped
            .funds(now, 30)
            .context("retained broker observation")?;
        assert_eq!(last.status(), "historical");
        assert_eq!(last.historical_reason, Some("执行已停止"));
        let mut row = OverviewRow::default();
        last.apply_to(&mut row);
        assert_eq!(row.date_ms, Some(observed / 1000));
        assert_eq!(row.equity, Some(1250.0));
        Ok(())
    }

    #[test]
    fn yida_sdk_asset_indications_remain_separate_from_verified_equity() {
        let funds = reader::AccountFunds {
            source: reader::AccountFundsSource::YidaExtendedAccount,
            evidence_sequence: 1,
            observed_us: 2000,
            trading_day: Some("20261118".into()),
            currency: Some("CNY".into()),
            valuation_basis: Some("sdk_mark_time_unverified".into()),
            balance: None,
            previous_balance: Some(1200.0),
            margin: None,
            available: Some(0.0),
            usable_cash: Some(0.0),
            deposit: None,
            withdraw: None,
            native_fields: BTreeMap::from([
                ("sdk_cash_balance", 1250.0),
                ("sdk_market_value", 1300.0),
            ]),
        };
        let obs = FundsObservation {
            funds: &funds,
            historical_reason: None,
        };
        let card = obs.card();
        assert_eq!(card["sdk_market_value"], 1300.0);
        assert_eq!(card["sdk_cash_balance"], 1250.0);
        assert_eq!(card["usable_cash"], 0.0);
        assert_eq!(card["valuation_basis"], "sdk_mark_time_unverified");
        let mut row = OverviewRow::default();
        obs.apply_to(&mut row);
        assert!(row.equity.is_none());
        assert!(row.yesterday_equity.is_none());
        assert!(row.today_profit.is_none());
    }

    #[test]
    fn quiet_replica_keeps_yida_funds_and_positions_until_evidence_becomes_unverifiable()
    -> Result<()> {
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
            rocksdb_path: primary,
            enabled: true,
            sync: None,
        };
        let now = crate::native_futures::now_us();
        let old = now - 600_000_000;
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/yida_native_records.json"
        ))?;
        let mut state = fixture["state"].clone();
        state["received_us"] = json!(old);
        let cf = db
            .cf_handle("cn_trading_events")
            .context("test events CF")?;
        db.put_cf(cf, 100_i64.to_be_bytes(), serde_json::to_vec(&state)?)?;
        let mut snapshot = reader::read(&source, &tmp.path().join("secondary"), 10, 30)?;
        assert_eq!(
            snapshot.status, "stale",
            "execution evidence still applies its own age rule"
        );
        snapshot.replication = Some(crate::native_futures::sync::ReplicaStatus {
            initialized: true,
            committed_cursor: 100,
            received_cursor: 100,
            source_captured_at_us: Some(old),
            last_synced_at_us: Some(now),
            ..Default::default()
        });
        snapshot.funds = Some(reader::AccountFunds {
            source: reader::AccountFundsSource::YidaExtendedAccount,
            evidence_sequence: 99,
            observed_us: old,
            trading_day: Some("20261118".into()),
            currency: None,
            valuation_basis: None,
            balance: None,
            previous_balance: None,
            margin: None,
            available: None,
            usable_cash: Some(500.0),
            deposit: None,
            withdraw: None,
            native_fields: BTreeMap::new(),
        });
        snapshot.funds_current_runtime = true;
        let mut quiet = observation(Sample::default(), Ok(snapshot.clone()), now);
        assert_eq!(quiet.collection(), "live");
        assert_eq!(quiet.execution_status(), Some("quiet"));
        assert!(quiet.nav_cut.context("retained NAV evidence")?.historical);
        let funds = quiet.funds(now, 30).context("broker funds")?;
        assert_eq!(funds.status(), "live");
        assert_eq!(funds.funds.observed_us, old);
        let position = quiet.position().context("complete position observation")?;
        assert_eq!(position.status(), "live");
        assert_eq!(position.observation.observed_us, old);
        assert_eq!(
            positions(&position.observation.rows)
                .context("valid position rows")?
                .len(),
            2
        );
        quiet.notionals = Some((123.0, 0.0));

        let busy = observation(
            quiet.clone(),
            Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "test barrier".into(),
            )),
            now,
        );
        assert_eq!(busy.collection(), "loading");
        assert_eq!(
            busy.position().context("last known observation")?.status(),
            "historical"
        );
        assert_eq!(busy.funds(now, 30).unwrap().status(), "historical");
        assert_eq!(busy.notionals, Some((123.0, 0.0)));
        assert!(busy.nav_cut.is_none());
        let failed = observation(
            quiet.clone(),
            Err(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "test failure".into(),
            )),
            now,
        );
        assert_eq!(failed.collection(), "unavailable");
        assert_eq!(failed.collection_reason, Some("执行记录读取失败"));

        let replica = snapshot.replication.as_mut().unwrap();
        replica.target_cursor = Some(101);
        let catching_up = observation(quiet.clone(), Ok(snapshot.clone()), now);
        assert_eq!(catching_up.collection(), "loading");
        assert_eq!(catching_up.position().unwrap().status(), "historical");
        snapshot.replication_stale = true;
        let delayed = observation(quiet.clone(), Ok(snapshot.clone()), now);
        assert_eq!(delayed.collection(), "stale");
        assert_eq!(delayed.collection_reason, Some("远端副本同步逾期"));

        snapshot.replication_stale = false;
        snapshot.replication.as_mut().unwrap().target_cursor = None;
        snapshot.position_observation.as_mut().unwrap().later_fills = true;
        let later_fill = observation(quiet, Ok(snapshot), now);
        assert_eq!(later_fill.collection(), "live");
        assert_eq!(later_fill.position().unwrap().status(), "historical");
        assert_eq!(
            later_fill.position().unwrap().reason,
            Some("该持仓对账之后已有成交，等待新的完整观测")
        );
        assert_eq!(later_fill.notionals, Some((123.0, 0.0)));
        Ok(())
    }

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
        assert_eq!(
            sample.position().context("current positions")?.status(),
            "live"
        );
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
        assert_eq!(
            sample.position().context("retained positions")?.status(),
            "historical"
        );
        assert_eq!(sample.notionals, Some((1.0, 0.0)));
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
        assert_eq!(
            failed.position().context("retained positions")?.status(),
            "historical"
        );
        assert!(failed.nav_cut.is_none());
        assert!(failed.nav.is_none());
        Ok(())
    }
}
