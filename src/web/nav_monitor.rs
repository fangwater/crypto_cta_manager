//! Account-scoped monitoring facade. External NAV is always consumed over HTTP.
use super::*;
use crate::account_overview::nav::{Monitor, Point};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueryParams {
    source_id: String,
}

pub(super) async fn report(
    State(state): State<WebState>,
    Query(query): Query<QueryParams>,
    Extension(visible): Extension<VisibleSources>,
    Extension(user): Extension<AuthUser>,
) -> Result<Response, ApiError> {
    if !query.source_id.starts_with("external:") {
        if !visible.0.contains(&query.source_id) {
            return Ok(forbidden("account is not visible"));
        }
        return local(state, query.source_id).await;
    }
    for provider in &state.external_overviews {
        if !provider.owns(&query.source_id) {
            continue;
        }
        // Reuse every host-defined and database visibility constraint, including
        // administrator exclusions, before making an upstream request.
        let mut overview = state
            .account_overview
            .report(&[], None, &Default::default())?;
        provider
            .merge(&state.pool, &user, None, &Default::default(), &mut overview)
            .await?;
        if !overview
            .accounts
            .iter()
            .any(|a| a.source_id == query.source_id)
        {
            return Ok(forbidden("account is not visible"));
        }
        let report = provider
            .nav_monitor(&query.source_id)
            .await
            .unwrap_or_else(|_| {
                let mut value =
                    Monitor::pending(&query.source_id, "外部净值接口暂不可用，请稍后重试");
                value.loading = false;
                value
            });
        return Ok((NO_STORE, Json(report)).into_response());
    }
    Ok(forbidden("account is not visible"))
}

pub(super) async fn export(
    State(state): State<WebState>,
    Query(query): Query<QueryParams>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !state.overview_export.authorized(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok()),
    ) {
        return Ok(unauthorized("integration token required"));
    }
    if !state.overview_export.allows_source(&query.source_id) {
        return Ok(forbidden("account is not exported"));
    }
    local(state, query.source_id).await
}

async fn local(state: WebState, id: String) -> Result<Response, ApiError> {
    let allocation = !state.funding.settings(&id).enabled;
    let end = {
        let cache = state.cache.read().await;
        let end = cache.dashboard.generated_at_us / 1000;
        if cache
            .nav_timelines
            .start_ts_us(&id, end * 1000, allocation)
            .is_none()
        {
            return Ok((
                NO_STORE,
                Json(Monitor::pending(&id, "尚无可重建的监控历史")),
            )
                .into_response());
        }
        end
    };
    let snapshot = match rebuild_timeline_snapshot(
        state,
        TimelineQuery {
            source_ids: Some(id.clone()),
            end_ms: Some(end),
            include_theoretical: Some(false),
            interval_hours: Some(4),
            max_points: Some(100_000),
            ..Default::default()
        },
        allocation,
        BTreeSet::from([id.clone()]),
    )
    .await?
    {
        Ok(value) => value,
        Err(response) => return Ok(response),
    };
    let actual = &snapshot.funding;
    let estimate = &snapshot.estimated_funding;
    let actual_available = actual.summary.record_count > 0
        || actual.summary.symbols.iter().any(|s| {
            s.start_ts_us <= snapshot.report.end_ts_us
                && s.synced_through_ts_us
                    .is_some_and(|ts| ts >= snapshot.report.start_ts_us)
        });
    let use_actual = actual_available
        && (actual.summary.enabled
            || estimate.income_quote.is_none()
            || estimate.unavailable_reason.is_some());
    let (funding, points, reason, loading) = if use_actual {
        (
            "actual",
            actual
                .points
                .iter()
                .map(|p| Point {
                    ts_ms: p.ts_us / 1000,
                    pnl_quote: p.total_nav_after_fee_quote,
                })
                .collect::<Vec<_>>(),
            (actual.summary.status != "ok"
                || actual
                    .summary
                    .start_ts_us
                    .is_none_or(|ts| ts > snapshot.report.start_ts_us))
            .then(|| "实际资金费覆盖尚不完整".into()),
            false,
        )
    } else if estimate.income_quote.is_some() {
        (
            "estimated",
            estimate
                .points
                .iter()
                .map(|p| Point {
                    ts_ms: p.ts_us / 1000,
                    pnl_quote: p.total_nav_after_fee_quote,
                })
                .collect(),
            estimate.unavailable_reason.clone(),
            estimate.loading,
        )
    } else {
        (
            "unavailable",
            snapshot
                .report
                .points
                .iter()
                .map(|p| Point {
                    ts_ms: p.ts_us / 1000,
                    pnl_quote: p.totals.nav_change_after_fee_quote,
                })
                .collect(),
            Some("资金费尚不可用；当前仅展示交易费后净值变化".into()),
            estimate.loading,
        )
    };
    // Funding APIs share factual sample times. Rebase explicitly so no opening
    // funding balance can turn the monitoring origin into a nonzero principal.
    let mut points = points;
    if let Some(baseline) = points.first().map(|p| p.pnl_quote) {
        for point in &mut points {
            point.pnl_quote -= baseline;
        }
    }
    let grid_complete = points.windows(2).all(|w| {
        let next = (w[0]
            .ts_ms
            .div_euclid(crate::account_overview::nav::INTERVAL_MS)
            + 1)
            * crate::account_overview::nav::INTERVAL_MS;
        w[1].ts_ms == next.min(snapshot.report.end_ts_us / 1000)
    });
    let reason = if !grid_complete {
        Some("历史点数超过上限，曲线已降采样".into())
    } else if unix_now_ms() - snapshot.generated_at_us / 1000 > 180_000 {
        Some("本地净值数据已过期，当前保留上次结果".into())
    } else {
        reason
    };
    let result = Monitor {
        source_id: id,
        start_ms: points.first().map(|p| p.ts_ms),
        end_ms: points.last().map(|p| p.ts_ms),
        generated_at_ms: snapshot.generated_at_us / 1000,
        interval_hours: 4,
        valuation: snapshot.report.valuation.into(),
        funding: funding.into(),
        complete: reason.is_none() && !loading,
        loading,
        unavailable_reason: reason,
        points,
    };
    Ok((NO_STORE, Json(result)).into_response())
}
