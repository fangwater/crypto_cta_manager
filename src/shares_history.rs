//! Actual account allocations after Virtual expansion, grouped by leaf strategy.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Postgres, Row, Transaction, postgres::PgPool};
use std::collections::{BTreeMap, BTreeSet};

use crate::auth::AuthUser;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeActor {
    pub user_id: i64,
    pub username: String,
}

pub struct BeforeAllocation {
    source_id: String,
    shares: BTreeMap<String, f64>,
}

/// Share the configuration transaction lock with Virtual materialization. Take
/// the baseline before any binding mutation, including edits to several aliases.
pub async fn capture(
    transaction: &mut Transaction<'_, Postgres>,
    source_id: &str,
) -> Result<BeforeAllocation> {
    let connection = &mut **transaction;
    sqlx::query("SELECT pg_advisory_xact_lock(740913211)")
        .execute(&mut *connection)
        .await?;
    let shares = totals(connection, &[source_id.to_string()])
        .await?
        .into_iter()
        .map(|entry| (entry.strategy_name, entry.shares))
        .collect();
    Ok(BeforeAllocation {
        source_id: source_id.to_string(),
        shares,
    })
}

/// Record once after all the account's bindings have been materialized. Equal
/// final totals, even with different Virtual paths/aliases, are not changes.
pub async fn record(
    transaction: &mut Transaction<'_, Postgres>,
    before: BeforeAllocation,
    at_us: i64,
    actor: Option<&AuthUser>,
    origin: &str,
) -> Result<()> {
    let connection = &mut **transaction;
    let after: BTreeMap<_, _> = totals(connection, std::slice::from_ref(&before.source_id))
        .await?
        .into_iter()
        .map(|entry| (entry.strategy_name, entry.shares))
        .collect();
    let names: BTreeSet<_> = before.shares.keys().chain(after.keys()).cloned().collect();
    let actors = sqlx::types::Json(
        actor
            .map(|user| ChangeActor {
                user_id: user.user_id,
                username: user.username.clone(),
            })
            .into_iter()
            .collect::<Vec<_>>(),
    );
    for name in names {
        let initial = before.shares.get(&name).copied().unwrap_or(0.0);
        let final_shares = after.get(&name).copied().unwrap_or(0.0);
        if initial == final_shares {
            continue;
        }
        let previous: Option<(i64, f64, i64)> = sqlx::query_as(
            "SELECT change_id, final_shares, last_changed_at_us
             FROM cta_account_strategy_shares_history WHERE source_id = $1 AND strategy_name = $2
             ORDER BY change_id DESC LIMIT 1 FOR UPDATE",
        )
        .bind(&before.source_id)
        .bind(&name)
        .fetch_optional(&mut *connection)
        .await?;
        let at_us = at_us.max(previous.map_or(at_us, |(_, _, at)| at));
        if let Some((id, previous_shares, last_at)) = previous
            && previous_shares == initial
            && at_us.saturating_sub(last_at) <= 60_000_000
        {
            sqlx::query(
                "UPDATE cta_account_strategy_shares_history SET final_shares = $2,
                    last_changed_at_us = $3, change_count = change_count + 1,
                    actors = CASE WHEN actors @> $4::jsonb THEN actors ELSE actors || $4::jsonb END,
                    origins = CASE WHEN $5 = ANY(origins) THEN origins ELSE array_append(origins, $5) END
                 WHERE change_id = $1",
            ).bind(id).bind(final_shares).bind(at_us).bind(&actors).bind(origin)
                .execute(&mut *connection).await?;
        } else {
            sqlx::query(
                "INSERT INTO cta_account_strategy_shares_history
                    (source_id, strategy_name, initial_shares, final_shares,
                     first_changed_at_us, last_changed_at_us, actors, origins)
                 VALUES ($1,$2,$3,$4,$5,$5,$6,ARRAY[$7]::text[])",
            )
            .bind(&before.source_id)
            .bind(&name)
            .bind(initial)
            .bind(final_shares)
            .bind(at_us)
            .bind(&actors)
            .bind(origin)
            .execute(&mut *connection)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct AccountStrategyShares {
    pub source_id: String,
    pub strategy_name: String,
    pub shares: f64,
}

async fn totals(
    connection: &mut PgConnection,
    source_ids: &[String],
) -> Result<Vec<AccountStrategyShares>> {
    let rows = sqlx::query(
        "SELECT source_id, position_strategy_name, shares FROM cta_account_strategy_bindings
         WHERE source_id = ANY($1) ORDER BY source_id, position_strategy_name, binding_name",
    )
    .bind(source_ids)
    .fetch_all(connection)
    .await?;
    let mut totals = BTreeMap::<(String, String), f64>::new();
    for row in rows {
        let source_id = row.try_get("source_id")?;
        let strategy_name = row.try_get("position_strategy_name")?;
        let shares: f64 = row.try_get("shares")?;
        let total = totals.entry((source_id, strategy_name)).or_default();
        *total += shares;
        crate::strategy_catalog::validate_nonnegative_multiplier(*total, "account strategy shares")
            .map_err(anyhow::Error::msg)?;
    }
    Ok(totals
        .into_iter()
        .map(
            |((source_id, strategy_name), shares)| AccountStrategyShares {
                source_id,
                strategy_name,
                shares,
            },
        )
        .collect())
}

#[derive(Debug, Serialize)]
pub struct SharesChange {
    pub change_id: i64,
    pub source_id: String,
    pub strategy_name: String,
    pub initial_shares: f64,
    pub final_shares: f64,
    pub first_changed_at_us: i64,
    pub last_changed_at_us: i64,
    pub change_count: i64,
    pub actors: Vec<ChangeActor>,
    pub origins: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub at_us: i64,
    pub change_id: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    pub strategy_name: Option<String>,
    pub before_at_us: Option<i64>,
    pub before_id: Option<i64>,
    pub limit: Option<i64>,
}

impl HistoryQuery {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.before_at_us.is_some() == self.before_id.is_some(),
            "before_at_us and before_id must be supplied together"
        );
        ensure!(
            (1..=200).contains(&self.limit.unwrap_or(50)),
            "limit must be between 1 and 200"
        );
        if let Some(name) = &self.strategy_name {
            crate::order_config::validate_strategy_name(name).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct SharesHistory {
    pub current: Vec<AccountStrategyShares>,
    pub entries: Vec<SharesChange>,
    pub next_cursor: Option<HistoryCursor>,
}

pub async fn list(
    pool: &PgPool,
    source_ids: &[String],
    query: &HistoryQuery,
) -> Result<SharesHistory> {
    query.validate()?;
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let current = totals(&mut tx, source_ids)
        .await?
        .into_iter()
        .filter(|entry| {
            entry.shares > 0.0
                && query
                    .strategy_name
                    .as_ref()
                    .is_none_or(|name| name == &entry.strategy_name)
        })
        .collect();
    let limit = query.limit.unwrap_or(50) as usize;
    let rows = sqlx::query(
        "SELECT change_id, source_id, strategy_name,
                initial_shares, final_shares, first_changed_at_us, last_changed_at_us,
                change_count, actors, origins
         FROM cta_account_strategy_shares_history
         WHERE source_id = ANY($1)
           AND initial_shares IS DISTINCT FROM final_shares
           AND ($2::text IS NULL OR strategy_name = $2)
           AND ($3::bigint IS NULL OR (last_changed_at_us, change_id) < ($3, $4))
         ORDER BY last_changed_at_us DESC, change_id DESC LIMIT $5",
    )
    .bind(source_ids)
    .bind(&query.strategy_name)
    .bind(query.before_at_us)
    .bind(query.before_id)
    .bind(limit as i64 + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut entries = rows
        .into_iter()
        .map(|row| {
            Ok(SharesChange {
                change_id: row.try_get("change_id")?,
                source_id: row.try_get("source_id")?,
                strategy_name: row.try_get("strategy_name")?,
                initial_shares: row.try_get("initial_shares")?,
                final_shares: row.try_get("final_shares")?,
                first_changed_at_us: row.try_get("first_changed_at_us")?,
                last_changed_at_us: row.try_get("last_changed_at_us")?,
                change_count: row.try_get("change_count")?,
                actors: row
                    .try_get::<sqlx::types::Json<Vec<ChangeActor>>, _>("actors")?
                    .0,
                origins: row.try_get("origins")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let has_more = entries.len() > limit;
    entries.truncate(limit);
    let next_cursor = entries
        .last()
        .filter(|_| has_more)
        .map(|entry| HistoryCursor {
            at_us: entry.last_changed_at_us,
            change_id: entry.change_id,
        });
    Ok(SharesHistory {
        current,
        entries,
        next_cursor,
    })
}
