//! Background-only daily equity reconstruction using the NAV page's default account view.
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
        "waiting for equity after Shanghai midnight"
    );
    // Match the NAV page default: actual funding uses account mode; otherwise
    // include the existing strategy allocation snapshot in the account total.
    // Starting from zero when only a strategy snapshot exists can invert PnL.
    let strategy_allocation = !state.funding.settings(&source.id).enabled;
    let earliest = timelines
        .start_ts_us(
            &source.id,
            observation.equity_at_ms * 1000,
            strategy_allocation,
        )
        .context("account NAV history unavailable")?;
    // The public funding cache and overview cover the most recent month. Existing
    // daily database records outside this moving rebuild window are retained.
    let first_boundary_ms = first_settlement_boundary_ms(earliest);
    let start = first_boundary_ms.max(day - 29 * DAY_MS);
    anyhow::ensure!(
        start <= day,
        "account has not crossed its first Shanghai settlement boundary"
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
            interval_hours: None,
        },
        strategy_allocation,
        BTreeSet::from([source.id.clone()]),
    )
    .await
    .map_err(|error| error.0)?
    .map_err(|_| anyhow::anyhow!("account NAV range is not ready"))?;
    // The interactive endpoint can use trade-price estimates beyond retention;
    // persisted daily audit anchors must still require complete minute marks.
    anyhow::ensure!(
        snapshot.report.valuation == "quantity_fifo_closed_1m_window_delta",
        "daily equity reconstruction requires complete minute-close valuation"
    );
    anyhow::ensure!(
        strategy_allocation == !state.funding.settings(&source.id).enabled,
        "funding selection changed during reconstruction"
    );
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
            strategy_allocation,
            valuation: snapshot.report.valuation.into(),
            computed_at_ms: unix_now_ms(),
            drawdown_15d_percent: None,
        },
        &values,
    )
}

/// Never extend the available NAV backwards across an incomplete first local day.
fn first_settlement_boundary_ms(earliest_us: i64) -> i64 {
    let opening = day_start(earliest_us.div_euclid(1000));
    if opening * 1000 < earliest_us {
        opening + DAY_MS
    } else {
        opening
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shanghai_first_boundary_respects_microsecond_history_start() {
        let midnight = crate::account_overview::selected_day("2026-10-19").unwrap();
        assert_eq!(first_settlement_boundary_ms(midnight * 1000 - 1), midnight);
        assert_eq!(first_settlement_boundary_ms(midnight * 1000), midnight);
        assert_eq!(
            first_settlement_boundary_ms(midnight * 1000 + 1),
            midnight + DAY_MS
        );
        // UTC midnight is already 08:00 Shanghai: the next complete boundary is tomorrow.
        assert_eq!(
            first_settlement_boundary_ms((midnight + 8 * 3_600_000) * 1000),
            midnight + DAY_MS
        );
    }
}
