use super::*;
use crate::{
    native_futures::nav::comparison,
    nav::{NavTimelinePoint, NavTotals},
    web::timeline_proto::{self, CurveKind, View},
};

const BROWSER_STEP_US: i64 = 15 * 60 * 1_000_000;
pub(super) const MONITOR_STEP_US: i64 = 4 * 3600 * 1_000_000;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct TimelineQuery {
    source_ids: Option<String>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    max_points: Option<usize>,
    symbols: Option<String>,
    view: Option<View>,
    include_theoretical: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MonitorQuery {
    source_id: String,
}

pub(super) async fn nav_monitor(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Query(query): Query<MonitorQuery>,
) -> ApiResult {
    get_source(&s, &access, &query.source_id)?;
    let id = &query.source_id;
    let now = crate::native_futures::now_us() / 1000;
    let sample = s.overview.sample(id);
    let anchor = nav::store::first(&s.pool, id).await?;
    let start = anchor
        .as_ref()
        .map(|anchor| anchor.snapshot_us / 1000 + i64::from(anchor.snapshot_us % 1000 != 0));
    let end = sample
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.through_us)
        .map(|time| time.div_euclid(MONITOR_STEP_US) * (MONITOR_STEP_US / 1000));
    let (report, reason, loading) = match (start, end) {
        (None, _) => (None, Some("该账户尚无原生净值起始快照"), false),
        (Some(_), None) => (
            None,
            Some("执行记录尚未同步"),
            sample.collection() == "loading",
        ),
        (Some(start), Some(end)) if start > end => {
            (None, Some("起始快照之后尚无完整的四小时观测点"), false)
        }
        (Some(start), Some(end)) if end - start > nav::MAX_WINDOW_US / 1000 => (
            None,
            Some("从起始快照到最近观测点的区间超过 31 天计价范围"),
            false,
        ),
        (Some(start), Some(end)) => {
            let report = account_nav_report(
                &s,
                id,
                NavQuery {
                    start_ms: start,
                    end_ms: end,
                    max_points: Some(600),
                },
                Some(BrowserNav {
                    symbols: &[],
                    symbol_curves: false,
                    interval_us: MONITOR_STEP_US,
                }),
            )
            .await?;
            // A newer anchor may support short interactive windows, but the
            // cumulative monitor always retains its original observed baseline.
            (Some(report), None, false)
        }
    };
    let complete = report
        .as_ref()
        .is_some_and(|r| r.ready && r.fees.missing_count == 0);
    let reason = report.as_ref().and_then(|r| r.reason.as_deref()).or(reason);
    body(json!({
        "source_id": id, "start_ms": start, "end_ms": report.as_ref().map(|r| r.end_us / 1000),
        "generated_at_ms": now, "interval_hours": 4, "valuation": "native_lot_fifo_completed_minute_close",
        "funding": "not_applicable", "complete": complete, "loading": loading, "unavailable_reason": reason,
        "points": report.iter().flat_map(|r| &r.points).filter_map(|p| Some(json!({
            "ts_ms": p.timestamp_us / 1000, "pnl_quote": p.nav_after_fees_cny?,
        }))).collect::<Vec<_>>(),
    }))
}

pub(super) async fn account(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Query(query): Query<TimelineQuery>,
) -> Result<Response, ApiError> {
    let sources = selected(&s, &access, query.source_ids.as_deref())?;
    let id = only_source(&sources)?;
    let view = query.view.unwrap_or_default();
    if view == View::Strategies {
        return Err(unavailable("请通过分策略 PnL 查看可核验的策略归因"));
    }
    account_view(&s, &user, id, query, view).await
}

pub(super) async fn strategy(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Query(mut query): Query<TimelineQuery>,
) -> Result<Response, ApiError> {
    let sources = selected(&s, &access, query.source_ids.as_deref())?;
    let id = only_source(&sources)?;
    let view = query.view.unwrap_or_default();
    let Some(ledger) = crate::native_futures::netting::store::load(&s.pool, id).await? else {
        return Err(unavailable("该账户尚无原生策略归因起始快照"));
    };
    if !can_view_targets(&s, &user, &ledger).await? {
        return Err(forbidden());
    }
    let revision = ledger.revision;
    if ledger.baseline_revision > 0 {
        query.restrict_to_baseline(ledger.anchor.snapshot_us)?;
    }
    let symbols = query.symbols();
    let window = query.nav_window(Some(ledger.anchor.snapshot_us))?;
    let (start, end, max_points) = window.window()?;
    if start < ledger.anchor.snapshot_us {
        return Err(unavailable(
            "所选起点早于策略归因快照；请选择其后的时间范围",
        ));
    }
    if end - start > nav::MAX_WINDOW_US {
        return Err(unavailable("当前分钟行情计价仅支持最长 31 天的区间"));
    }
    let replay = nav::attribution::Replay::load(&s.pool, ledger).await.map_err(|error| {
        tracing::warn!(source_id=id,error=?error,"native strategy NAV receipt history unavailable");
        unavailable("策略归因历史记录不完整，无法计算所选区间的净值")
    })?;
    let frozen_symbols = symbols.clone();
    let cut = with_nav_source(&s, id, move |source, db, through| {
        let mut replay = replay;
        let effective_end = end.min(through);
        ensure!(effective_end >= start, "策略归因的执行记录尚未覆盖所选区间");
        replay.read_records(source, db, start, effective_end, &frozen_symbols)?;
        Ok((replay, effective_end, through))
    })
    .await?;
    let (replay, effective_end, through) = cut.map_err(|error| {
        tracing::warn!(source_id=id,error=?error,"native strategy NAV evidence cut unavailable");
        unavailable("策略归因或执行证据未覆盖所选区间；请核对归因快照、未归因成交和同步进度")
    })?;
    let result = replay
        .timeline(
            s.market_data.as_deref(),
            s.reference_service.as_deref(),
            nav::service::Query {
                start_us: start,
                end_us: effective_end,
                through_us: through,
                max_points,
                symbols: &symbols,
                symbol_curves: view == View::Symbols,
                interval_us: Some(BROWSER_STEP_US),
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(source_id=id,error=?error,"native strategy NAV valuation unavailable");
            unavailable("策略持仓证据或已完成分钟行情覆盖不足；无法形成完整策略净值曲线")
        })?;
    let references = if query.wants_comparison(view) {
        comparisons(&s, &result.report, revision, !symbols.is_empty()).await
    } else {
        vec![]
    };
    project(result.report, result.strategies, references, end, view)
}

async fn can_view_targets(
    s: &Service,
    user: &AuthUser,
    ledger: &crate::native_futures::netting::Portfolio,
) -> Result<bool, ApiError> {
    if user.is_admin() {
        return Ok(true);
    }
    let visible = strategy_catalog::list_position_visibility(&s.pool).await?;
    // Retired members remain in the strategy NAV history.
    Ok(ledger.targets.keys().chain(&ledger.retired).all(|name| {
        visible
            .iter()
            .any(|v| v.strategy_name == *name && v.user_can_view(user.user_id))
    }))
}

async fn comparisons(
    s: &Service,
    report: &nav::Report,
    revision: i64,
    filtered: bool,
) -> Vec<comparison::Reference> {
    if !report.ready {
        return comparison::unavailable(
            report
                .reason
                .as_deref()
                .unwrap_or("所选区间的实际仓位基准不可用"),
        );
    }
    match comparison::timeline(
        &s.pool,
        s.market_data.as_deref(),
        report,
        revision,
        filtered,
    )
    .await
    {
        Ok(references) => references,
        Err(error) => {
            tracing::warn!(source_id=%report.source_id,error=?error,"native reference NAV unavailable");
            comparison::unavailable("无法读取完整的目标批次或合约分钟行情")
        }
    }
}

async fn account_view(
    s: &Service,
    user: &AuthUser,
    id: &str,
    mut query: TimelineQuery,
    view: View,
) -> Result<Response, ApiError> {
    if let Some(start) = nav::store::fresh_start(&s.pool, id).await? {
        query.restrict_to_baseline(start)?;
    }
    let end = query
        .end_ms
        .unwrap_or_else(|| crate::native_futures::now_us() / 1000);
    let anchor = if query.start_ms.is_none() {
        let at = end.checked_mul(1000).ok_or_else(|| bad("时间范围溢出"))?;
        nav::store::load(&s.pool, id, at)
            .await?
            .map(|anchor| anchor.snapshot_us)
    } else {
        None
    };
    let window = query.nav_window(anchor)?;
    let (_, requested_end, _) = window.window()?;
    let symbols = query.symbols();
    let report = account_nav_report(
        s,
        id,
        window,
        Some(BrowserNav {
            symbols: &symbols,
            symbol_curves: view == View::Symbols,
            interval_us: BROWSER_STEP_US,
        }),
    )
    .await?;
    let references = if query.wants_comparison(view) {
        match crate::native_futures::netting::store::load(&s.pool, id).await {
            Ok(Some(ledger)) if report.start_us < ledger.anchor.snapshot_us => {
                comparison::unavailable("所选起点早于策略目标账本")
            }
            Ok(Some(ledger)) => match can_view_targets(s, user, &ledger).await {
                Ok(true) => comparisons(s, &report, ledger.revision, !symbols.is_empty()).await,
                _ => comparison::unavailable("查看参考曲线需要该账户的策略目标查看权限"),
            },
            Ok(None) => comparison::unavailable("该账户尚无原生策略目标账本"),
            Err(error) => {
                tracing::warn!(source_id=id,error=?error,"native reference NAV target ledger unavailable");
                comparison::unavailable("策略目标账本暂不可用")
            }
        }
    } else {
        vec![]
    };
    project(report, vec![], references, requested_end, view)
}

fn only_source(sources: &[String]) -> Result<&str, ApiError> {
    match sources {
        [] => Err(forbidden()),
        [id] => Ok(id),
        _ => Err(bad("请选择一个账户查询净值")),
    }
}

impl TimelineQuery {
    fn restrict_to_baseline(&mut self, snapshot_us: i64) -> Result<(), ApiError> {
        let baseline_ms = snapshot_us / 1000 + i64::from(snapshot_us % 1000 != 0);
        if self.end_ms.is_some_and(|end| end < baseline_ms) {
            return Err(unavailable("所选时段早于新净值起点"));
        }
        if let Some(start) = &mut self.start_ms {
            *start = (*start).max(baseline_ms);
        }
        Ok(())
    }

    fn wants_comparison(&self, view: View) -> bool {
        view == View::Theoretical || (view == View::Portfolio && self.include_theoretical)
    }

    fn nav_window(&self, anchor_us: Option<i64>) -> Result<NavQuery, ApiError> {
        let end = self
            .end_ms
            .unwrap_or_else(|| crate::native_futures::now_us() / 1000);
        let start = self.start_ms.unwrap_or_else(|| {
            let since = end.saturating_sub(86_400_000);
            anchor_us.map_or(since, |anchor| {
                (anchor / 1000 + i64::from(anchor % 1000 != 0)).max(since)
            })
        });
        let query = NavQuery {
            start_ms: start,
            end_ms: end,
            max_points: self.max_points,
        };
        query.window()?;
        Ok(query)
    }
    fn symbols(&self) -> Vec<String> {
        self.symbols
            .iter()
            .flat_map(|symbols| symbols.split(','))
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

fn unavailable(message: impl ToString) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.to_string())
}

fn metadata(report: &nav::Report, references: &[comparison::Reference]) -> Value {
    let fee_basis = if report.fees.estimated_count > 0 {
        "reported_and_estimated_total_cny"
    } else {
        "actual_total_cny"
    };
    json!({
        "generated_at_us": crate::native_futures::now_us(), "generation_duration_ms": 0,
        "theoretical": references.iter().map(|row| json!({
            "valuation": "native_target_baseline_minute_reference", "execution_model": row.model.key(),
            "loading": false, "available_from_us": report.snapshot_us,
            "execution_window_secs": row.model.window_secs(),
            "price_basis": "baseline_data_1min", "fee_basis": fee_basis,
            "latest_point_ts_us": row.points.last().map(|point| point.ts_us),
            "unavailable_reason": row.unavailable, "sampled": report.sampled,
            "points": [],
        })).collect::<Vec<_>>(),
        "execution_slippage": [],
    })
}

fn totals(point: &nav::Point, fills: &nav::Fees) -> Result<NavTotals, ApiError> {
    let fee = point.fees_cny.ok_or_else(|| {
        unavailable("所选区间存在缺少 CNY 实收费用或历史费率依据的成交，费后净值不可用")
    })?;
    Ok(NavTotals {
        fill_count: fills.fill_count as u64,
        volume_quote: fills.volume_cny,
        unknown_liquidity_fill_count: fills.fill_count as u64,
        unknown_liquidity_volume_quote: fills.volume_cny,
        realized_pnl_before_fee_quote: point.realized_cny,
        estimated_trading_fee_quote: fee,
        realized_pnl_after_fee_quote: point.realized_cny - fee,
        floating_pnl_quote: point.floating_cny,
        nav_change_before_fee_quote: point.nav_before_fees_cny,
        nav_change_after_fee_quote: point
            .nav_after_fees_cny
            .context("native fee totals inconsistent")?,
        ..Default::default()
    })
}

type ActualCurve = (CurveKind, String, Vec<NavTimelinePoint>);

fn curve(kind: CurveKind, key: String, points: &[nav::Point]) -> Result<ActualCurve, ApiError> {
    let no_fill_statistics = nav::Fees::default();
    let points = points
        .iter()
        .map(|point| {
            Ok(NavTimelinePoint {
                ts_us: point.timestamp_us,
                gross_position_value_quote: point.gross_position_cny,
                net_position_value_quote: point.net_position_cny,
                totals: totals(point, &no_fill_statistics)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok((kind, key, points))
}

fn project(
    report: nav::Report,
    strategy: Vec<nav::attribution::StrategySummary>,
    references: Vec<comparison::Reference>,
    requested_end: i64,
    view: View,
) -> Result<Response, ApiError> {
    let mut metadata = metadata(&report, &references);
    if view == View::Theoretical {
        return encode(metadata, vec![], &references);
    }
    let summary = report
        .summary
        .as_ref()
        .ok_or_else(|| unavailable(report.reason.as_deref().unwrap_or("暂无可计价的账户净值")))?;
    let mut curves = vec![curve(CurveKind::Portfolio, String::new(), &report.points)?];
    let mut markets = Vec::with_capacity(report.markets.len());
    for market in &report.markets {
        let name = format!("{}:{}", market.venue, market.symbol);
        if !market.points.is_empty() {
            curves.push(curve(CurveKind::Symbol, name.clone(), &market.points)?);
        }
        let mut row = serde_json::to_value(totals(&market.summary, &market.fees)?)
            .map_err(anyhow::Error::from)?;
        row["symbol"] = json!(name);
        row["source_count"] = json!(1);
        row["venue_count"] = json!(1);
        row["initial_net_quantity"] = json!(market.initial_net_lots);
        row["long_quantity"] = json!(market.long_lots);
        row["short_quantity"] = json!(market.short_lots);
        row["net_quantity"] = json!(market.long_lots - market.short_lots);
        row["long_position_value_quote"] = json!(market.long_value_cny);
        row["short_position_value_quote"] = json!(market.short_value_cny);
        row["net_position_value_quote"] = json!(market.summary.net_position_cny);
        markets.push(row);
    }
    let mut strategies = Vec::with_capacity(strategy.len());
    for row in &strategy {
        if view == View::Strategies {
            curves.push(curve(CurveKind::Strategy, row.name.clone(), &row.points)?);
        }
        strategies.push(json!({
            "strategy": row.name, "symbol_count": row.market_count, "has_activity": true,
            "gross_position_value_quote": row.summary.gross_position_cny,
            "net_position_value_quote": row.summary.net_position_cny,
            "summary": totals(&row.summary, &row.fees)?, "points": [],
        }));
    }
    metadata["report"] = json!({
        "valuation": report.valuation, "earliest_start_ts_us": report.snapshot_us,
        "start_ts_us": report.start_us, "end_ts_us": report.end_us,
        "requested_end_ts_us": requested_end, "source_through_ts_us": report.source_through_us,
        "selected_source_ids": [report.source_id], "quote_currency": report.currency,
        "available_symbols": report.available_symbols, "selected_symbols": report.selected_symbols,
        "available_strategies": strategy.iter().map(|row| &row.name).collect::<Vec<_>>(),
        "summary": totals(summary, &report.fees)?, "symbols": markets,
        "fee_evidence": report.fees,
        "trading_segments_ms": report.trading_segments_us.iter()
            .map(|&(start, end)| [start / 1000, end / 1000]).collect::<Vec<_>>(),
        "points": [], "symbol_points": [], "strategy_points": strategies, "sampled": report.sampled,
    });
    metadata["estimated_funding"] = json!({
        "income_quote": null, "loading": false, "strategies": [], "points": [],
        "unavailable_reason": "国内期货不适用加密永续合约的资金费",
    });
    encode(metadata, curves, &references)
}

fn encode(
    metadata: Value,
    curves: Vec<ActualCurve>,
    references: &[comparison::Reference],
) -> Result<Response, ApiError> {
    let bytes = timeline_proto::encode_native_curves(metadata, curves, references)?;
    Ok((
        [
            (header::CONTENT_TYPE, timeline_proto::CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}
