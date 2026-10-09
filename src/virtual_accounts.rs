//! Configuration-only accounts. Effective real bindings remain in the existing
//! catalog so every publish/archive path freezes the same multiplied shares.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::strategy_catalog::{SaveBindingRequest, validate_nonnegative_multiplier};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VirtualBinding {
    pub binding_name: String,
    pub position_strategy_name: String,
    pub order_strategy_name: String,
    pub shares: f64,
}

#[derive(Debug, Serialize)]
pub struct VirtualAccount {
    pub virtual_id: String,
    pub name: String,
    pub bindings: Vec<VirtualBinding>,
    pub updated_at_us: i64,
    pub followers: Vec<VirtualFollower>,
}

#[derive(Debug, Serialize)]
pub struct VirtualFollower {
    pub source_id: String,
    pub multiplier: f64,
    pub pending_publishes: Vec<PendingPublish>,
}

#[derive(Debug, Deserialize)]
pub struct SaveVirtualAccount {
    pub name: String,
    pub bindings: Vec<VirtualBinding>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AccountConfiguration {
    #[default]
    Independent,
    Follow {
        virtual_id: String,
        multiplier: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingPublish {
    pub binding_name: String,
    pub error: Option<String>,
}

pub async fn configuration(pool: &PgPool, source_id: &str) -> Result<AccountConfiguration> {
    let row =
        sqlx::query("SELECT virtual_id, multiplier FROM cta_account_follows WHERE source_id = $1")
            .bind(source_id)
            .fetch_optional(pool)
            .await?;
    Ok(match row {
        Some(row) => AccountConfiguration::Follow {
            virtual_id: row.try_get("virtual_id")?,
            multiplier: row.try_get("multiplier")?,
        },
        None => AccountConfiguration::Independent,
    })
}

pub async fn require_independent(pool: &PgPool, source_id: &str) -> Result<()> {
    ensure!(
        matches!(
            configuration(pool, source_id).await?,
            AccountConfiguration::Independent
        ),
        "account is following a virtual account; switch to independent mode before editing bindings"
    );
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cta_follow_publish_queue WHERE source_id = $1)",
    )
    .bind(source_id)
    .fetch_one(pool)
    .await?;
    ensure!(
        !pending,
        "configuration publishes are still pending; retry after synchronization completes"
    );
    Ok(())
}

pub async fn pending(pool: &PgPool, source_id: &str) -> Result<Vec<PendingPublish>> {
    sqlx::query("SELECT binding_name, error FROM cta_follow_publish_queue WHERE source_id = $1 ORDER BY binding_name")
        .bind(source_id).fetch_all(pool).await?.into_iter().map(|row| Ok(PendingPublish {
            binding_name: row.try_get("binding_name")?, error: row.try_get("error")?,
        })).collect()
}

pub async fn list(pool: &PgPool) -> Result<Vec<VirtualAccount>> {
    let mut accounts = Vec::new();
    for row in sqlx::query(
        "SELECT virtual_id, name, updated_at_us FROM cta_virtual_accounts ORDER BY virtual_id",
    )
    .fetch_all(pool)
    .await?
    {
        let id: String = row.try_get("virtual_id")?;
        let bindings = sqlx::query("SELECT binding_name, position_strategy_name, order_strategy_name, shares FROM cta_virtual_account_bindings WHERE virtual_id = $1 ORDER BY binding_name")
            .bind(&id).fetch_all(pool).await?.into_iter().map(|row| Ok(VirtualBinding {
                binding_name: row.try_get("binding_name")?,
                position_strategy_name: row.try_get("position_strategy_name")?,
                order_strategy_name: row.try_get("order_strategy_name")?,
                shares: row.try_get("shares")?,
            })).collect::<Result<Vec<_>>>()?;
        let mut followers = Vec::new();
        for row in sqlx::query("SELECT source_id, multiplier FROM cta_account_follows WHERE virtual_id = $1 ORDER BY source_id")
            .bind(&id).fetch_all(pool).await? {
            let source_id: String = row.try_get("source_id")?;
            followers.push(VirtualFollower {
                pending_publishes: pending(pool, &source_id).await?,
                source_id,
                multiplier: row.try_get("multiplier")?,
            });
        }
        accounts.push(VirtualAccount {
            virtual_id: id,
            name: row.try_get("name")?,
            bindings,
            updated_at_us: row.try_get("updated_at_us")?,
            followers,
        });
    }
    Ok(accounts)
}

pub fn effective_bindings(
    bindings: &[VirtualBinding],
    multiplier: f64,
) -> Result<Vec<SaveBindingRequest>> {
    validate_nonnegative_multiplier(multiplier, "multiplier").map_err(anyhow::Error::msg)?;
    let mut names = BTreeSet::new();
    bindings
        .iter()
        .map(|binding| {
            for name in [
                &binding.binding_name,
                &binding.position_strategy_name,
                &binding.order_strategy_name,
            ] {
                crate::order_config::validate_strategy_name(name).map_err(anyhow::Error::msg)?;
            }
            ensure!(
                names.insert(&binding.binding_name),
                "duplicate binding name: {}",
                binding.binding_name
            );
            validate_nonnegative_multiplier(binding.shares, "shares")
                .map_err(anyhow::Error::msg)?;
            let shares = binding.shares * multiplier;
            validate_nonnegative_multiplier(shares, "effective shares")
                .map_err(anyhow::Error::msg)?;
            Ok(SaveBindingRequest {
                binding_name: binding.binding_name.clone(),
                position_strategy_name: binding.position_strategy_name.clone(),
                order_strategy_name: binding.order_strategy_name.clone(),
                shares,
            })
        })
        .collect()
}

async fn begin(pool: &PgPool) -> Result<Transaction<'_, Postgres>> {
    let mut tx = pool.begin().await?;
    // Coordinate configuration replacements across callers and Manager processes.
    sqlx::query("SELECT pg_advisory_xact_lock(740913211)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    binding: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO cta_follow_publish_queue (source_id, binding_name, revision) VALUES ($1, $2, 1) ON CONFLICT (source_id, binding_name) DO UPDATE SET revision = cta_follow_publish_queue.revision + 1, archived = false, error = NULL")
        .bind(source).bind(binding).execute(&mut **tx).await?;
    Ok(())
}

async fn materialize(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    bindings: &[VirtualBinding],
    multiplier: f64,
    at: i64,
    force_publish: bool,
) -> Result<()> {
    let desired = effective_bindings(bindings, multiplier)?
        .into_iter()
        .map(|b| (b.binding_name.clone(), b))
        .collect::<BTreeMap<_, _>>();
    let rows = sqlx::query("SELECT binding_name, position_strategy_name, order_strategy_name, shares FROM cta_account_strategy_bindings WHERE source_id = $1 FOR UPDATE")
        .bind(source).fetch_all(&mut **tx).await?;
    let mut existing = BTreeMap::new();
    for row in rows {
        let name: String = row.try_get("binding_name")?;
        existing.insert(
            name,
            (
                row.try_get::<String, _>("position_strategy_name")?,
                row.try_get::<String, _>("order_strategy_name")?,
                row.try_get::<f64, _>("shares")?,
            ),
        );
    }
    for (name, binding) in &desired {
        if let Some(old) = existing.get(name) {
            ensure!(
                old.0 == binding.position_strategy_name,
                "binding {name} cannot change its position strategy; use a different binding name"
            );
            if old.1 == binding.order_strategy_name && old.2 == binding.shares {
                if force_publish {
                    enqueue(tx, source, name).await?;
                }
                continue;
            }
        }
        sqlx::query("INSERT INTO cta_account_strategy_bindings (source_id, binding_name, position_strategy_name, order_strategy_name, shares, updated_at_us) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (source_id, binding_name) DO UPDATE SET order_strategy_name = EXCLUDED.order_strategy_name, shares = EXCLUDED.shares, updated_at_us = EXCLUDED.updated_at_us")
            .bind(source).bind(name).bind(&binding.position_strategy_name).bind(&binding.order_strategy_name).bind(binding.shares).bind(at).execute(&mut **tx).await?;
        enqueue(tx, source, name).await?;
    }
    for (name, (_, _, shares)) in existing {
        if !desired.contains_key(&name) && (shares != 0.0 || force_publish) {
            sqlx::query("UPDATE cta_account_strategy_bindings SET shares = 0, updated_at_us = $3 WHERE source_id = $1 AND binding_name = $2")
                .bind(source).bind(&name).bind(at).execute(&mut **tx).await?;
            enqueue(tx, source, &name).await?;
        }
    }
    Ok(())
}

pub async fn save(pool: &PgPool, id: &str, request: &SaveVirtualAccount, at: i64) -> Result<()> {
    crate::order_config::validate_strategy_name(id).map_err(anyhow::Error::msg)?;
    ensure!(
        !request.name.trim().is_empty() && request.name.len() <= 200,
        "virtual account name must contain 1–200 bytes"
    );
    effective_bindings(&request.bindings, 1.0)?;
    let mut tx = begin(pool).await?;
    sqlx::query("INSERT INTO cta_virtual_accounts (virtual_id,name,updated_at_us) VALUES ($1,$2,$3) ON CONFLICT (virtual_id) DO UPDATE SET name = EXCLUDED.name, updated_at_us = EXCLUDED.updated_at_us")
        .bind(id).bind(request.name.trim()).bind(at).execute(&mut *tx).await?;
    let followers =
        sqlx::query("SELECT source_id, multiplier FROM cta_account_follows WHERE virtual_id = $1")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    for row in followers {
        materialize(
            &mut tx,
            &row.try_get::<String, _>("source_id")?,
            &request.bindings,
            row.try_get("multiplier")?,
            at,
            false,
        )
        .await?;
    }
    sqlx::query("DELETE FROM cta_virtual_account_bindings WHERE virtual_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for b in &request.bindings {
        sqlx::query("INSERT INTO cta_virtual_account_bindings (virtual_id,binding_name,position_strategy_name,order_strategy_name,shares) VALUES ($1,$2,$3,$4,$5)")
            .bind(id).bind(&b.binding_name).bind(&b.position_strategy_name).bind(&b.order_strategy_name).bind(b.shares).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_configuration(
    pool: &PgPool,
    source: &str,
    config: &AccountConfiguration,
    at: i64,
) -> Result<()> {
    let mut tx = begin(pool).await?;
    match config {
        AccountConfiguration::Independent => {
            sqlx::query("DELETE FROM cta_account_follows WHERE source_id = $1")
                .bind(source)
                .execute(&mut *tx)
                .await?;
        }
        AccountConfiguration::Follow {
            virtual_id,
            multiplier,
        } => {
            validate_nonnegative_multiplier(*multiplier, "multiplier")
                .map_err(anyhow::Error::msg)?;
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM cta_virtual_accounts WHERE virtual_id = $1)",
            )
            .bind(virtual_id)
            .fetch_one(&mut *tx)
            .await?;
            ensure!(exists, "unknown virtual account: {virtual_id}");
            let rows = sqlx::query("SELECT binding_name, position_strategy_name, order_strategy_name, shares FROM cta_virtual_account_bindings WHERE virtual_id = $1")
                .bind(virtual_id).fetch_all(&mut *tx).await?;
            let bindings = rows
                .into_iter()
                .map(|row| {
                    Ok(VirtualBinding {
                        binding_name: row.try_get("binding_name")?,
                        position_strategy_name: row.try_get("position_strategy_name")?,
                        order_strategy_name: row.try_get("order_strategy_name")?,
                        shares: row.try_get("shares")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            materialize(&mut tx, source, &bindings, *multiplier, at, true).await?;
            sqlx::query("INSERT INTO cta_account_follows (source_id,virtual_id,multiplier,updated_at_us) VALUES ($1,$2,$3,$4) ON CONFLICT (source_id) DO UPDATE SET virtual_id = EXCLUDED.virtual_id, multiplier = EXCLUDED.multiplier, updated_at_us = EXCLUDED.updated_at_us")
                .bind(source).bind(virtual_id).bind(multiplier).bind(at).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete(pool: &PgPool, id: &str) -> Result<bool> {
    let mut tx = begin(pool).await?;
    let followed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cta_account_follows WHERE virtual_id = $1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if followed {
        bail!("virtual account still has followers; switch them to independent mode first");
    }
    let result = sqlx::query("DELETE FROM cta_virtual_accounts WHERE virtual_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

pub async fn enqueue_order_followers(pool: &PgPool, order: &str) -> Result<()> {
    sqlx::query("INSERT INTO cta_follow_publish_queue (source_id,binding_name,revision) SELECT b.source_id,b.binding_name,1 FROM cta_account_strategy_bindings b JOIN cta_account_follows f USING (source_id) JOIN cta_position_strategies p ON p.strategy_name = b.position_strategy_name WHERE b.shares > 0 AND (b.order_strategy_name = $1 OR EXISTS (SELECT 1 FROM jsonb_each_text(p.symbol_order_strategy_overrides) o WHERE o.value = $1)) ON CONFLICT (source_id,binding_name) DO UPDATE SET revision = cta_follow_publish_queue.revision + 1, archived = false, error = NULL")
        .bind(order).execute(pool).await?;
    Ok(())
}

pub async fn followers(pool: &PgPool, id: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT source_id FROM cta_account_follows WHERE virtual_id = $1 ORDER BY source_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?)
}

// Used by validation before a template is changed or attached to a live account.
pub async fn validate_source_bindings(
    pool: &PgPool,
    venue: &str,
    bindings: &[VirtualBinding],
) -> Result<()> {
    let positions = crate::strategy_catalog::list_position_strategies(pool).await?;
    let orders = crate::strategy_catalog::list_order_strategies(pool).await?;
    for binding in bindings {
        let position = positions
            .iter()
            .find(|p| p.strategy_name == binding.position_strategy_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown position strategy: {}",
                    binding.position_strategy_name
                )
            })?;
        ensure!(
            orders
                .iter()
                .any(|o| o.strategy_name == binding.order_strategy_name),
            "unknown order strategy: {}",
            binding.order_strategy_name
        );
        for symbol in position.targets.keys() {
            crate::exec_routing::symbol_market(venue, symbol)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follows_scale_shares_and_reject_invalid_or_overflowing_multipliers() {
        let b = VirtualBinding {
            binding_name: "s".into(),
            position_strategy_name: "s".into(),
            order_strategy_name: "o".into(),
            shares: 2.5,
        };
        assert_eq!(
            effective_bindings(&[b.clone()], 3.0).unwrap()[0].shares,
            7.5
        );
        assert_eq!(
            effective_bindings(&[b.clone()], 0.0).unwrap()[0].shares,
            0.0
        );
        for multiplier in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert!(effective_bindings(&[b.clone()], multiplier).is_err());
        }
        assert!(effective_bindings(&[b.clone(), b], 1.0).is_err());
    }
}
