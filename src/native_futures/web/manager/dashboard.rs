use super::*;
use crate::{
    account_overview::{OverviewAlertSettings, OverviewRow},
    native_futures::funds::{self, BrokerFunds, DailyEquity},
    postgres,
};
use axum::extract::Query;
use chrono::NaiveDate;
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
    /// Latest validated broker funds relay; kept when a later query fails.
    funds: Option<funds::Observation>,
    /// Trading day and query time of the last persisted daily observation.
    funds_saved: Option<(String, i64)>,
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("native funds HTTP client");
    for (id, updates) in &service.overview.0 {
        let service = service.clone();
        let id = id.clone();
        let updates = updates.clone();
        let client = client.clone();
        let mut shutdown = service.plan_shutdown.subscribe();
        tokio::spawn(async move {
            loop {
                let result = read_snapshot(&service, &id).await;
                let attempted_at_us = crate::native_futures::now_us();
                let mut sample = observation(updates.borrow().clone(), result, attempted_at_us);
                updates.send_replace(sample.clone());
                let budget = Duration::from_secs(REFRESH_SECS);
                let (holdings, nav, broker) = tokio::join!(
                    tokio::time::timeout(budget, holdings(&service, &id, &sample)),
                    refresh_cumulative_nav(&service, &id, &sample, budget),
                    broker_funds(&service, &client, &id)
                );
                if let Ok(Some(holdings)) = holdings {
                    sample.holdings = Some(holdings);
                }
                sample.nav = nav;
                if let Some(broker) = broker {
                    sample.funds = Some(broker);
                }
                save_funds(&service, &id, &mut sample).await;
                updates.send_replace(sample);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(REFRESH_SECS)) => {},
                    _ = shutdown.changed() => break,
                }
            }
        });
    }
}

/// Broker funds exist only for replica sources whose exporter relays them.
async fn broker_funds(
    s: &Service,
    client: &reqwest::Client,
    id: &str,
) -> Option<funds::Observation> {
    let source = s
        .config
        .sources
        .iter()
        .find(|source| source.id == id && source.sync.is_some())?;
    match crate::native_futures::sync::account_funds(client, source).await {
        Ok(observation) => observation,
        Err(error) => {
            tracing::warn!(source_id=%id,error=?error,"native broker funds unavailable");
            None
        }
    }
}

const SAVE_EVERY_US: i64 = 60_000_000;

/// Persists the latest observation of each trading day: at most once a minute
/// while live, and immediately once the counter stops answering so the final
/// observation of the day is kept.
async fn save_funds(s: &Service, id: &str, sample: &mut Sample) {
    let now = crate::native_futures::now_us();
    let Some(observation) = sample.funds.as_ref() else {
        return;
    };
    let Some(account) = observation.account.as_ref() else {
        return;
    };
    let due = match &sample.funds_saved {
        None => true,
        Some((day, at)) => {
            account.observed_us > *at
                && (*day != account.trading_day
                    || account.observed_us - at >= SAVE_EVERY_US
                    || observation.historical_reason(now).is_some())
        }
    };
    if !due {
        return;
    }
    let notionals = sample
        .holdings
        .as_ref()
        .and_then(|h| h.notionals.map(|n| (n, h.cut.at_us)));
    let row = DailyEquity {
        source_id: id.into(),
        funds: account.clone(),
        long_notional: notionals.map(|((long, _), _)| long),
        short_notional: notionals.map(|((_, short), _)| short),
        positions_at_us: notionals.map(|(_, at)| at),
    };
    match funds::store::upsert(&s.pool, &row).await {
        Ok(()) => sample.funds_saved = Some((account.trading_day.clone(), account.observed_us)),
        Err(error) => tracing::warn!(source_id=%id,error=?error,"native daily equity not saved"),
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
    // Concurrent per contract; the feed's own permits bound the provider load.
    let values = futures_util::future::join_all(rows.keys().map(|(venue, contract)| async {
        let prices = s
            .market_data
            .as_ref()?
            .nav_prices(venue, contract, cut.at_us, cut.at_us, false)
            .await
            .ok()?;
        Some(prices.mark(cut.at_us).ok()? * prices.multiplier)
    }))
    .await;
    for ((long, short), value) in rows.values().zip(values) {
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

/// The funds shown for "now": the live relay, else the latest saved day.
#[derive(Clone)]
struct CurrentFunds {
    funds: BrokerFunds,
    reason: Option<&'static str>,
}
impl CurrentFunds {
    fn status(&self) -> &'static str {
        if self.reason.is_some() {
            "historical"
        } else {
            "live"
        }
    }
}

fn current_funds(sample: &Sample, saved: &[DailyEquity], now_us: i64) -> Option<CurrentFunds> {
    if let Some(observation) = &sample.funds
        && let Some(account) = &observation.account
        && saved
            .last()
            .is_none_or(|row| account.observed_us >= row.funds.observed_us)
    {
        return Some(CurrentFunds {
            funds: account.clone(),
            reason: observation.historical_reason(now_us),
        });
    }
    saved.last().map(|row| CurrentFunds {
        funds: row.funds.clone(),
        reason: Some("未取得柜台实时资金，显示最近一次保存的观察"),
    })
}

/// Saved trading days with the current observation replacing its own day.
fn with_current(mut rows: Vec<DailyEquity>, current: Option<DailyEquity>) -> Vec<DailyEquity> {
    let Some(current) = current else {
        return rows;
    };
    match rows.last_mut() {
        Some(last) if last.funds.trading_day == current.funds.trading_day => {
            if current.funds.observed_us >= last.funds.observed_us {
                *last = current;
            }
        }
        Some(last) if last.funds.trading_day > current.funds.trading_day => {}
        _ => rows.push(current),
    }
    rows
}

fn apply_funds(row: &mut OverviewRow, funds: &BrokerFunds, drawdown: Option<f64>) {
    row.date_ms = Some(funds.observed_us / 1000);
    row.yesterday_equity = Some(funds.pre_balance);
    row.equity = Some(funds.balance);
    row.today_profit = Some(funds.profit());
    row.profit_percent = funds.profit_percent();
    row.margin = Some(funds.curr_margin);
    row.margin_kind = Some("initial".into());
    row.margin_percent = funds.margin_percent();
    row.long_leverage = funds.leverage(row.long_position);
    row.short_leverage = funds.leverage(row.short_position);
    row.max_drawdown_15d_percent = drawdown;
}

fn dashed(trading_day: &str) -> Option<String> {
    funds::day(trading_day).map(|day| day.to_string())
}

/// Saved daily equity, or none until the explicit schema maintenance ran.
async fn saved_equity(s: &Service, id: &str) -> Vec<DailyEquity> {
    funds::store::list(&s.pool, id)
        .await
        .inspect_err(
            |error| tracing::warn!(source_id=%id,error=?error,"native daily equity unavailable"),
        )
        .unwrap_or_default()
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
    funds_status: Option<&'static str>,
    funds_as_of_ms: Option<i64>,
    funds_historical_reason: Option<&'static str>,
    funds_trading_day: Option<String>,
}

/// Dates are native trading days: a night session belongs to the next day,
/// and yesterday's equity is the broker's settled previous-day equity.
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
        .map_err(|_| bad("日期须为 YYYY-MM-DD"))?
        .map(|day| day.format("%Y%m%d").to_string());
    let aliases = postgres::load_source_aliases(&s.pool).await?;
    let now_us = crate::native_futures::now_us();
    let now_ms = now_us / 1000;
    let mut accounts = Vec::new();
    let mut dates = BTreeSet::new();
    for source in visible(&s, &access) {
        let sample = s.overview.sample(&source.id);
        let position = sample.position();
        let anchors = nav::store::list(&s.pool, &source.id).await?;
        let saved = saved_equity(&s, &source.id).await;
        let current = current_funds(&sample, &saved, now_us);
        let live_notionals = position.and_then(|obs| obs.holdings.notionals);
        let days = with_current(
            saved,
            current.as_ref().map(|current| DailyEquity {
                source_id: source.id.clone(),
                funds: current.funds.clone(),
                long_notional: live_notionals.map(|(long, _)| long),
                short_notional: live_notionals.map(|(_, short)| short),
                positions_at_us: position.map(|obs| obs.holdings.cut.at_us),
            }),
        );
        dates.extend(anchors.iter().filter_map(|a| dashed(&a.trading_day)));
        dates.extend(days.iter().filter_map(|d| dashed(&d.funds.trading_day)));
        let mut row = OverviewRow {
            source_id: source.id.clone(),
            account: aliases.get(&source.id).unwrap_or(&source.account).clone(),
            venue: source.adapter.clone(),
            account_kind: "cta".into(),
            queried_at_ms: sample.attempted_at_us.map_or(now_ms, |at| at / 1000),
            history_start_ms: anchors.last().map(|anchor| anchor.snapshot_us / 1000),
            ..Default::default()
        };
        let mut native = NativeOverviewRow {
            row: OverviewRow::default(),
            quote_currency: "CNY",
            collection_status_reason: None,
            position_status: None,
            position_as_of_ms: None,
            position_historical_reason: None,
            funds_status: None,
            funds_as_of_ms: None,
            funds_historical_reason: None,
            funds_trading_day: None,
        };
        if let Some(day) = &selected_day {
            row.queried_at_ms = now_ms;
            let through = days.partition_point(|d| d.funds.trading_day <= *day);
            let daily = days[..through]
                .last()
                .filter(|d| d.funds.trading_day == *day);
            let anchor = anchors.iter().find(|a| a.trading_day == *day);
            let (notionals, at) = match (daily, anchor) {
                (Some(d), _) if d.positions_at_us.is_some() => {
                    (d.long_notional.zip(d.short_notional), d.positions_at_us)
                }
                (_, Some(anchor)) => (anchor_notionals(anchor), Some(anchor.snapshot_us)),
                _ => (None, None),
            };
            if let Some((long, short)) = notionals {
                row.long_position = Some(long);
                row.short_position = Some(short);
                row.net_position = Some(long - short);
            }
            row.date_ms = at.map(|at| at / 1000);
            if let Some(d) = daily {
                apply_funds(&mut row, &d.funds, funds::drawdown(&days[..through]));
                native.funds_status = Some("historical");
                native.funds_as_of_ms = Some(d.funds.observed_us / 1000);
                native.funds_historical_reason = Some("所选交易日最后一次柜台资金观察");
                native.funds_trading_day = Some(d.funds.trading_day.clone());
            }
            let found = daily.is_some() || at.is_some();
            row.collection_status = Some(if found { "historical" } else { "unavailable" }.into());
            native.position_status = at.map(|_| "historical");
            native.position_as_of_ms = at.map(|at| at / 1000);
            native.position_historical_reason = Some(if at.is_some() {
                "所选交易日的持仓观测"
            } else {
                "所选交易日没有可验证的持仓"
            });
        } else {
            if let Some((long, short)) = live_notionals {
                row.long_position = Some(long);
                row.short_position = Some(short);
                row.net_position = Some(long - short);
            }
            row.date_ms = position.map(|obs| obs.holdings.cut.at_us / 1000);
            if let Some(current) = &current {
                apply_funds(&mut row, &current.funds, funds::drawdown(&days));
                native.funds_status = Some(current.status());
                native.funds_as_of_ms = Some(current.funds.observed_us / 1000);
                native.funds_historical_reason = current.reason;
                native.funds_trading_day = Some(current.funds.trading_day.clone());
            }
            row.collection_status = Some(sample.collection().into());
            native.collection_status_reason = sample.collection_reason;
            native.position_status = position.map(PositionDisplay::status);
            native.position_as_of_ms = position.map(|obs| obs.holdings.cut.at_us / 1000);
            native.position_historical_reason = sample.position_reason(position);
        }
        native.row = row;
        accounts.push(native);
    }
    let loading = !accounts.is_empty()
        && accounts
            .iter()
            .all(|row| row.row.collection_status.as_deref() == Some("loading"));
    body(json!({
        "loading": loading,
        "accounts": accounts,
        "dates": dates.into_iter().rev().collect::<Vec<_>>(),
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
        let saved = saved_equity(&s, id).await;
        let funds = current_funds(&sample, &saved, now);
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
            "live_equity_quote": funds.as_ref().map(|f| f.funds.balance),
            "live_equity_status": funds.as_ref().map_or("unavailable", |f| {
                if f.reason.is_some() { "stale" } else { "ok" }
            }),
            "account_funds": funds.as_ref().map(|f| json!({
                "source": if source.adapter == "yida" { "yida_extended_account" } else { "ctp_trading_account" },
                "status": f.status(), "historical_reason": f.reason,
                "observed_us": f.funds.observed_us, "trading_day": f.funds.trading_day,
                "currency": f.funds.currency, "available": f.funds.available,
                "previous_balance": f.funds.pre_balance,
                "trading_day_commission_cny": f.funds.commission,
            })),
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
