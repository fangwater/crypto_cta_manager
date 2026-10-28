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
        return Ok((
            NO_STORE,
            Json(state.nav_monitor.read(&query.source_id, unix_now_ms())),
        )
            .into_response());
    }
    for provider in &state.external_overviews {
        if !provider.owns(&query.source_id) {
            continue;
        }
        // Reuse every host-defined and database visibility constraint, including
        // administrator exclusions, before reading the persisted snapshot.
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
        let report = state.nav_monitor.read(&query.source_id, unix_now_ms());
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
    Ok((
        NO_STORE,
        Json(state.nav_monitor.read(&query.source_id, unix_now_ms())),
    )
        .into_response())
}

async fn calculate(state: WebState, id: String, end: i64) -> Result<Monitor> {
    let allocation = !state.funding.settings(&id).enabled;
    {
        let cache = state.cache.read().await;
        anyhow::ensure!(
            cache.dashboard.generated_at_us / 1000 >= end,
            "dashboard has not reached the four-hour boundary"
        );
        anyhow::ensure!(
            cache
                .nav_timelines
                .start_ts_us(&id, end * 1000, allocation)
                .is_some(),
            "no reconstructable monitoring history"
        );
    }
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
    .await
    .map_err(|error| error.0)?
    {
        Ok(value) => value,
        Err(_) => anyhow::bail!("four-hour NAV pricing is not ready"),
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
    Ok(result)
}

/// Startup loads persisted snapshots; this single worker owns reconstruction.
/// Browser and integration requests never replay fills or contact providers.
pub(super) fn spawn(state: WebState) {
    tokio::spawn(async move {
        let mut attempts = BTreeMap::<String, (i64, String, std::time::Instant)>::new();
        loop {
            let cutoff = crate::account_overview::nav::grid_end(unix_now_ms());
            let mut sources = state
                .config
                .sources
                .iter()
                .filter(|s| s.enabled)
                .map(|s| {
                    (
                        s.id.clone(),
                        format!("funding:{}", state.funding.settings(&s.id).enabled),
                        None,
                    )
                })
                .collect::<Vec<_>>();
            for (index, provider) in state.external_overviews.iter().enumerate() {
                sources.extend(
                    provider
                        .monitor_sources()
                        .into_iter()
                        .map(|id| (id, String::new(), Some(index))),
                );
            }
            for (id, input_key, provider) in sources {
                if state.nav_monitor.is_current(&id, cutoff, &input_key) {
                    continue;
                }
                if attempts.get(&id).is_some_and(|(at, key, retry)| {
                    *at == cutoff && *key == input_key && std::time::Instant::now() < *retry
                }) {
                    continue;
                }
                let result = match provider {
                    Some(index) => state.external_overviews[index].nav_monitor(&id).await,
                    None => calculate(state.clone(), id.clone(), cutoff).await,
                };
                let result = match result {
                    Ok(report) => {
                        let store = state.nav_monitor.clone();
                        let key = input_key.clone();
                        tokio::task::spawn_blocking(move || store.save(report, key, cutoff))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))
                            .and_then(|r| r)
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    warn!(source_id=%id, %error, "four-hour NAV deferred; retaining saved history");
                }
                attempts.insert(
                    id,
                    (
                        cutoff,
                        input_key,
                        std::time::Instant::now() + Duration::from_secs(30),
                    ),
                );
            }
            // Re-evaluate the UTC/Shanghai aligned boundary and discover new
            // catalog entries. Failed/pending sources retry independently.
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
