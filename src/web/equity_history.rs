//! Background-only daily equity reconstruction using the account NAV API path.
use super::*;
use crate::account_overview::{daily, day_start};

const DAY_MS: i64 = 86_400_000;

pub(super) fn spawn(state: WebState) {
    tokio::spawn(async move {
        let mut loaded = BTreeSet::new();
        let mut retry_after = BTreeMap::new();
        let mut timer = tokio::time::interval(Duration::from_secs(30));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            for source in state.config.sources.iter().filter(|source| source.enabled) {
                if retry_after
                    .get(&source.id)
                    .is_some_and(|at| Instant::now() < *at)
                {
                    continue;
                }
                let result = async {
                    if !loaded.contains(&source.id) {
                        state.account_overview.set_daily(&source.id, daily::load(&state.pool, &source.id).await?)?;
                        loaded.insert(source.id.clone());
                    }
                    let day = day_start(unix_now_ms());
                    if state.account_overview.rebuilt_day(&source.id).is_some_and(|last| last >= day) { return Ok(()); }
                    let rows = rebuild(&state, source).await?;
                    let saved = daily::save(&state.pool, &rows).await?;
                    let rows = if saved { rows } else { daily::load(&state.pool, &source.id).await? };
                    let count = rows.len();
                    state.account_overview.set_daily(&source.id, rows)?;
                    info!(source_id = %source.id, daily_points = count, "account equity daily reconstruction ready");
                    anyhow::Ok(())
                }.await;
                if let Err(error) = result {
                    warn!(source_id = %source.id, %error, "daily account equity reconstruction deferred; retaining prior result");
                    retry_after.insert(source.id.clone(), Instant::now() + Duration::from_secs(60));
                } else {
                    retry_after.remove(&source.id);
                }
            }
        }
    });
}

async fn rebuild(state: &WebState, source: &SourceConfig) -> Result<Vec<daily::DailyEquity>> {
    anyhow::ensure!(
        source.venue == "binance-futures",
        "account NAV and equity currency must be compatible"
    );
    let (through_ms, timelines) = {
        let cache = state.cache.read().await;
        (
            cache.dashboard.generated_at_us / 1000 - cache.dashboard.generation_duration_ms as i64,
            cache.nav_timelines.clone(),
        )
    };
    let max_age = state
        .refresh_interval_secs
        .saturating_mul(2)
        .saturating_mul(1000)
        .saturating_add(45_000)
        .min(i64::MAX as u64) as i64;
    let observation = state
        .account_overview
        .reconstruction_anchor(&source.id, through_ms, max_age)
        .context("waiting for matching fresh equity and NAV generation")?;
    let day = day_start(unix_now_ms());
    anyhow::ensure!(
        observation.equity_at_ms >= day,
        "waiting for equity after UTC midnight"
    );
    let earliest = timelines
        .account_start_ts_us(&source.id, observation.equity_at_ms * 1000)
        .context("account NAV history unavailable")?;
    // The public funding cache and overview cover the most recent month. Existing
    // daily database records outside this moving rebuild window are retained.
    let first_boundary_ms = ((earliest + DAY_MS * 1000 - 1) / (DAY_MS * 1000)) * DAY_MS;
    let start = first_boundary_ms.max(day - 29 * DAY_MS);
    anyhow::ensure!(
        start <= day,
        "account has not crossed its first UTC settlement boundary"
    );
    let snapshot = rebuild_timeline_snapshot(
        state.clone(),
        TimelineQuery {
            start_ms: Some(start),
            end_ms: Some(observation.equity_at_ms),
            source_ids: Some(source.id.clone()),
            symbols: None,
            max_points: Some(10_000),
            include_theoretical: Some(false),
        },
        false,
        BTreeSet::from([source.id.clone()]),
    )
    .await
    .map_err(|error| error.0)?
    .map_err(|_| anyhow::anyhow!("account NAV range is not ready"))?;
    let (funding_mode, values) = if snapshot.funding.summary.enabled {
        anyhow::ensure!(
            snapshot.funding.summary.status == "ok",
            "actual funding history is not ready"
        );
        (
            "actual",
            snapshot
                .funding
                .points
                .iter()
                .map(|point| (point.ts_us / 1000, point.total_nav_after_fee_quote))
                .collect::<Vec<_>>(),
        )
    } else {
        let estimate = &snapshot.estimated_funding;
        anyhow::ensure!(
            !estimate.loading && estimate.income_quote.is_some(),
            "estimated funding history is not ready"
        );
        let hour_start_us = observation.equity_at_ms.div_euclid(3_600_000) * 3_600_000_000;
        anyhow::ensure!(
            estimate
                .synced_through_ts_us
                .is_none_or(|ts| ts >= hour_start_us),
            "waiting for current funding settlement coverage"
        );
        (
            "estimated",
            estimate
                .points
                .iter()
                .map(|point| (point.ts_us / 1000, point.total_nav_after_fee_quote))
                .collect::<Vec<_>>(),
        )
    };
    daily::reconstruct(
        &source.id,
        daily::EquityAnchor {
            day_ms: day,
            ts_ms: observation.equity_at_ms,
            equity: observation.equity,
            nav_change_quote: 0.0,
            coverage_start_ms: start,
            nav_generated_at_us: snapshot.generated_at_us,
            funding_mode: funding_mode.into(),
            computed_at_ms: unix_now_ms(),
            drawdown_15d_percent: None,
        },
        &values,
    )
}
