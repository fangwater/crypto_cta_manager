//! Daily equity reconstructed from the existing account NAV, anchored to live equity.
//! These derived records are separate from immutable exchange observations.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use super::{DAY_MS, day_start, maximum_drawdown};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EquityAnchor {
    pub day_ms: i64,
    pub ts_ms: i64,
    pub equity: f64,
    pub nav_change_quote: f64,
    pub coverage_start_ms: i64,
    pub nav_generated_at_us: i64,
    pub funding_mode: String,
    /// False also describes records created before allocation provenance was saved.
    #[serde(default)]
    pub strategy_allocation: bool,
    pub computed_at_ms: i64,
    pub drawdown_15d_percent: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DailyEquity {
    pub source_id: String,
    pub boundary_ts_ms: i64,
    pub equity: f64,
    pub previous_equity: Option<f64>,
    pub nav_change_quote: f64,
    pub drawdown_15d_percent: Option<f64>,
    pub anchor: EquityAnchor,
}

/// Each input is the fee-after, funding-selected account NAV at its timestamp.
/// Preserve the NAV engine's samples and exact Shanghai ticks, never interpolate across fills.
pub fn reconstruct(
    source_id: &str,
    mut anchor: EquityAnchor,
    nav: &[(i64, f64)],
) -> Result<Vec<DailyEquity>> {
    ensure!(!source_id.is_empty(), "missing equity source");
    ensure!(anchor.equity.is_finite(), "invalid equity anchor");
    ensure!(
        day_start(anchor.ts_ms) == anchor.day_ms,
        "anchor is outside rebuild day"
    );
    ensure!(
        anchor.computed_at_ms >= anchor.ts_ms,
        "future equity anchor"
    );
    ensure!(
        matches!(
            anchor.funding_mode.as_str(),
            "actual" | "estimated" | "excluded"
        ),
        "invalid funding mode"
    );
    ensure!(!nav.is_empty(), "empty account NAV");
    ensure!(
        nav.iter().all(|(ts, value)| *ts >= 0 && value.is_finite()),
        "invalid account NAV"
    );
    ensure!(
        nav.windows(2).all(|w| w[0].0 < w[1].0),
        "unordered account NAV"
    );
    let &(end, current_nav) = nav.last().unwrap();
    ensure!(end == anchor.ts_ms, "equity and NAV anchors differ");
    ensure!(
        nav[0].0 == anchor.coverage_start_ms,
        "NAV coverage start differs"
    );
    anchor.nav_change_quote = current_nav;
    let equities = nav
        .iter()
        .map(|(ts, value)| (*ts, anchor.equity - (current_nav - value)))
        .collect::<Vec<_>>();
    ensure!(
        equities.iter().all(|(_, value)| value.is_finite()),
        "invalid reconstructed equity"
    );
    anchor.drawdown_15d_percent = drawdown(&equities, anchor.ts_ms);
    let mut rows = Vec::new();
    let mut previous: Option<(i64, f64)> = None;
    for ((ts, value), (_, equity)) in nav.iter().zip(&equities) {
        if day_start(*ts) != *ts {
            continue;
        }
        rows.push(DailyEquity {
            source_id: source_id.into(),
            boundary_ts_ms: *ts,
            equity: *equity,
            previous_equity: previous
                .filter(|(prev, _)| *prev + DAY_MS == *ts)
                .map(|(_, value)| value),
            nav_change_quote: *value,
            drawdown_15d_percent: drawdown(&equities, *ts),
            anchor: anchor.clone(),
        });
        previous = Some((*ts, *equity));
    }
    ensure!(
        rows.last()
            .is_some_and(|row| row.boundary_ts_ms == anchor.day_ms),
        "missing current Shanghai baseline"
    );
    ensure!(
        rows.windows(2)
            .all(|w| w[0].boundary_ts_ms + DAY_MS == w[1].boundary_ts_ms),
        "missing Shanghai NAV boundary"
    );
    Ok(rows)
}

fn drawdown(equities: &[(i64, f64)], end: i64) -> Option<f64> {
    let start = end - 15 * DAY_MS;
    let first = equities
        .partition_point(|(ts, _)| *ts <= start)
        .checked_sub(1)?;
    maximum_drawdown(
        equities[first..]
            .iter()
            .take_while(|(ts, _)| *ts <= end)
            .map(|(_, equity)| *equity),
    )
    .map(|value| value * 100.0)
}

pub async fn load(pool: &PgPool, source_id: &str) -> Result<Vec<DailyEquity>> {
    let rows = sqlx::query("SELECT source_id,boundary_ts_ms,equity,previous_equity,nav_change_quote,drawdown_15d_percent,anchor FROM cta_account_equity_daily WHERE source_id=$1 ORDER BY boundary_ts_ms")
        .bind(source_id).fetch_all(pool).await.context("daily equity table unavailable; apply explicit equity schema maintenance")?;
    rows.into_iter()
        .map(|row| {
            let sqlx::types::Json(anchor) = row.try_get("anchor")?;
            Ok(DailyEquity {
                source_id: row.try_get("source_id")?,
                boundary_ts_ms: row.try_get("boundary_ts_ms")?,
                equity: row.try_get("equity")?,
                previous_equity: row.try_get("previous_equity")?,
                nav_change_quote: row.try_get("nav_change_quote")?,
                drawdown_15d_percent: row.try_get("drawdown_15d_percent")?,
                anchor,
            })
        })
        .collect()
}

/// One complete account generation per Shanghai day. Failed transactions retain the old generation.
/// A per-source transaction lock also prevents duplicate daily commits from overlapping workers.
pub async fn save(pool: &PgPool, rows: &[DailyEquity]) -> Result<bool> {
    let first = rows.first().context("empty daily equity batch")?;
    ensure!(
        rows.iter()
            .all(|row| row.source_id == first.source_id && row.anchor == first.anchor),
        "mixed equity reconstruction batch"
    );
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('account-equity:' || $1, 0))")
        .bind(&first.source_id)
        .execute(&mut *tx)
        .await?;
    let last: Option<i64> = sqlx::query_scalar(
        "SELECT max((anchor->>'day_ms')::bigint) FROM cta_account_equity_daily WHERE source_id=$1",
    )
    .bind(&first.source_id)
    .fetch_one(&mut *tx)
    .await?;
    if last.is_some_and(|day| day >= first.anchor.day_ms) {
        return Ok(false);
    }
    for row in rows {
        sqlx::query("INSERT INTO cta_account_equity_daily(source_id,boundary_ts_ms,equity,previous_equity,nav_change_quote,drawdown_15d_percent,anchor) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(source_id,boundary_ts_ms) DO UPDATE SET equity=EXCLUDED.equity,previous_equity=EXCLUDED.previous_equity,nav_change_quote=EXCLUDED.nav_change_quote,drawdown_15d_percent=EXCLUDED.drawdown_15d_percent,anchor=EXCLUDED.anchor")
            .bind(&row.source_id).bind(row.boundary_ts_ms).bind(row.equity).bind(row.previous_equity)
            .bind(row.nav_change_quote).bind(row.drawdown_15d_percent).bind(sqlx::types::Json(&row.anchor))
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn anchor(ts: i64, equity: f64, start: i64) -> EquityAnchor {
        EquityAnchor {
            day_ms: day_start(ts),
            ts_ms: ts,
            equity,
            nav_change_quote: 0.0,
            coverage_start_ms: start,
            nav_generated_at_us: ts * 1000,
            funding_mode: "estimated".into(),
            strategy_allocation: true,
            computed_at_ms: ts,
            drawdown_15d_percent: None,
        }
    }
    #[test]
    fn reverse_nav_preserves_signed_daily_changes_and_anchor() {
        let start = day_start(20 * DAY_MS);
        let end = start + 2 * DAY_MS + 3_600_000;
        let nav = [
            (start, 0.0),
            (start + DAY_MS, 30.0),
            (start + 2 * DAY_MS, 10.0),
            (end, 25.0),
        ];
        let rows = reconstruct("one", anchor(end, 125.0, start), &nav).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.equity).collect::<Vec<_>>(),
            [100.0, 130.0, 110.0]
        );
        assert_eq!(rows[1].previous_equity, Some(100.0));
        assert_eq!(rows[2].equity - rows[2].previous_equity.unwrap(), -20.0);
        assert_eq!(rows[2].anchor.nav_change_quote, 25.0);
        assert_eq!(rows[2].anchor.drawdown_15d_percent, None);
        let shifted = reconstruct("two", anchor(end, 225.0, start), &nav).unwrap();
        assert_eq!(shifted[0].equity, 200.0);
        assert_eq!(rows[0].source_id, "one");
    }
    #[test]
    fn drawdown_includes_intraday_nav_and_requires_full_coverage() {
        let start = day_start(20 * DAY_MS);
        let end = start + 15 * DAY_MS;
        let mut nav = (0..=15)
            .map(|d| (start + d * DAY_MS, 0.0))
            .collect::<Vec<_>>();
        nav.push((end - DAY_MS / 2, 50.0));
        nav.sort_by_key(|p| p.0);
        let rows = reconstruct("one", anchor(end, 100.0, start), &nav).unwrap();
        assert!((rows.last().unwrap().drawdown_15d_percent.unwrap() - 100.0 / 3.0).abs() < 1e-10);
        assert!(rows[14].drawdown_15d_percent.is_none());
    }
    #[test]
    fn missing_boundary_invalid_nav_and_misaligned_anchor_are_rejected() {
        let start = day_start(20 * DAY_MS);
        let end = start + 2 * DAY_MS;
        assert!(
            reconstruct(
                "one",
                anchor(end, 100.0, start),
                &[(start, 0.0), (end, 10.0)]
            )
            .is_err()
        );
        assert!(
            reconstruct(
                "one",
                anchor(end, 100.0, start),
                &[(start, 0.0), (end - 1, 10.0)]
            )
            .is_err()
        );
        assert!(
            reconstruct(
                "one",
                anchor(end, 100.0, start),
                &[(start, f64::NAN), (end, 10.0)]
            )
            .is_err()
        );
    }
}
