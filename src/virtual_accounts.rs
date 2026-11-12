//! Configuration-only accounts. Effective real bindings remain in the existing
//! catalog so every publish/archive path freezes the same multiplied shares.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};

use crate::strategy_catalog::{SaveBindingRequest, validate_nonnegative_multiplier};

mod graph;

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
    pub children: Vec<AccountFollow>,
    pub expanded_bindings: Vec<VirtualBinding>,
    pub parents: Vec<String>,
    pub updated_at_us: i64,
    pub followers: Vec<VirtualFollower>,
    pub created_by_user_id: Option<i64>,
    pub owner_username: Option<String>,
    pub managers: Vec<VirtualGrantee>,
    pub can_configure: bool,
    pub can_manage_grants: bool,
}

#[derive(Debug, Serialize)]
pub struct VirtualGrantee {
    pub user_id: i64,
    pub username: String,
}

impl VirtualAccount {
    fn user_can_configure(&self, user: &crate::auth::AuthUser) -> bool {
        user.is_admin()
            || self.created_by_user_id == Some(user.user_id)
            || self
                .managers
                .iter()
                .any(|manager| manager.user_id == user.user_id)
    }

    pub fn user_can_view(
        &self,
        user: &crate::auth::AuthUser,
        visibility: &[crate::strategy_catalog::PositionVisibility],
    ) -> bool {
        self.user_can_configure(user)
            || self.expanded_bindings.iter().all(|binding| {
                visibility.iter().any(|v| {
                    v.strategy_name == binding.position_strategy_name
                        && v.user_can_view(user.user_id)
                })
            })
    }

    pub fn permissions_for(&mut self, user: &crate::auth::AuthUser) {
        self.can_manage_grants = user.is_admin() || self.created_by_user_id == Some(user.user_id);
        self.can_configure = self.user_can_configure(user);
    }
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
    #[serde(default)]
    pub children: Option<Vec<AccountFollow>>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AccountConfiguration {
    #[default]
    Independent,
    Follow {
        follows: Vec<AccountFollow>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountFollow {
    pub virtual_id: String,
    pub multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingPublish {
    pub binding_name: String,
    pub error: Option<String>,
}

pub async fn configuration(pool: &PgPool, source_id: &str) -> Result<AccountConfiguration> {
    let follows = load_follows(&mut *pool.acquire().await?, source_id).await?;
    Ok(if follows.is_empty() {
        AccountConfiguration::Independent
    } else {
        AccountConfiguration::Follow { follows }
    })
}

async fn load_follows(conn: &mut PgConnection, source: &str) -> Result<Vec<AccountFollow>> {
    sqlx::query("SELECT virtual_id, multiplier FROM cta_account_follows WHERE source_id = $1 ORDER BY virtual_id")
        .bind(source).fetch_all(conn).await?.into_iter().map(|row| Ok(AccountFollow {
            virtual_id: row.try_get("virtual_id")?, multiplier: row.try_get("multiplier")?,
        })).collect()
}

pub async fn require_independent(pool: &PgPool, source_id: &str) -> Result<()> {
    ensure!(
        matches!(
            configuration(pool, source_id).await?,
            AccountConfiguration::Independent
        ),
        "account is following virtual accounts; switch to independent mode before editing bindings"
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
    let definitions = graph::load(&mut *pool.acquire().await?).await?;
    let expanded = graph::expand(&definitions)?;
    let mut accounts = Vec::new();
    for row in sqlx::query(
        "SELECT v.virtual_id, v.name, v.updated_at_us, v.created_by_user_id, u.username AS owner_username FROM cta_virtual_accounts v LEFT JOIN cta_users u ON u.user_id = v.created_by_user_id ORDER BY v.virtual_id",
    )
    .fetch_all(pool)
    .await?
    {
        let id: String = row.try_get("virtual_id")?;
        let definition = &definitions[&id];
        let bindings = definition.bindings.clone();
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
            virtual_id: id.clone(),
            name: row.try_get("name")?,
            children: definition.children.clone(),
            expanded_bindings: expanded[&id].clone(),
            parents: definitions.iter().filter(|(_, definition)| definition.children.iter().any(|child| child.virtual_id == id)).map(|(parent, _)| parent.clone()).collect(),
            bindings,
            updated_at_us: row.try_get("updated_at_us")?,
            followers,
            created_by_user_id: row.try_get("created_by_user_id")?,
            owner_username: row.try_get("owner_username")?,
            managers: sqlx::query("SELECT u.user_id, u.username FROM cta_virtual_account_managers m JOIN cta_users u USING (user_id) WHERE m.virtual_id = $1 AND NOT u.disabled ORDER BY u.username")
                .bind(&id).fetch_all(pool).await?.into_iter().map(|row| Ok(VirtualGrantee {
                    user_id: row.try_get("user_id")?, username: row.try_get("username")?,
                })).collect::<Result<Vec<_>>>()?,
            can_configure: false,
            can_manage_grants: false,
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

fn combined_bindings(
    follows: &[AccountFollow],
    templates: &BTreeMap<String, Vec<VirtualBinding>>,
) -> Result<Vec<VirtualBinding>> {
    ensure!(
        !follows.is_empty(),
        "select at least one virtual account or use independent mode"
    );
    let mut ids = BTreeSet::new();
    let mut combined = BTreeMap::<String, VirtualBinding>::new();
    // Canonical order keeps sums stable when a browser reorders follow rows.
    let mut ordered: Vec<_> = follows.iter().collect();
    ordered.sort_by(|a, b| a.virtual_id.cmp(&b.virtual_id));
    for follow in ordered {
        ensure!(
            ids.insert(&follow.virtual_id),
            "duplicate virtual account: {}",
            follow.virtual_id
        );
        let bindings = templates
            .get(&follow.virtual_id)
            .ok_or_else(|| anyhow::anyhow!("unknown virtual account: {}", follow.virtual_id))?;
        for b in effective_bindings(bindings, follow.multiplier)? {
            graph::merge(&mut combined, b)?;
        }
    }
    Ok(combined.into_values().collect())
}

async fn load_combined_bindings(
    conn: &mut PgConnection,
    follows: &[AccountFollow],
    replacement: Option<(&str, &SaveVirtualAccount)>,
) -> Result<Vec<VirtualBinding>> {
    let mut definitions = graph::load(conn).await?;
    if let Some((id, request)) = replacement {
        validate_request(request)?;
        definitions
            .entry(id.to_owned())
            .or_default()
            .replace(request);
    }
    combined_bindings(follows, &graph::expand(&definitions)?)
}

// Preview the complete account target, including an unsaved nested template edit.
pub async fn preview_bindings(
    pool: &PgPool,
    follows: &[AccountFollow],
    replacement: Option<(&str, &SaveVirtualAccount)>,
) -> Result<Vec<VirtualBinding>> {
    load_combined_bindings(&mut *pool.acquire().await?, follows, replacement).await
}

pub async fn preview_virtual(
    pool: &PgPool,
    id: Option<&str>,
    request: &SaveVirtualAccount,
) -> Result<Vec<VirtualBinding>> {
    validate_request(request)?;
    let mut definitions = graph::load(&mut *pool.acquire().await?).await?;
    let id = match id {
        Some(id) => id.to_owned(),
        None => next_virtual_id(&definitions.keys().cloned().collect::<Vec<_>>())?,
    };
    definitions.entry(id.clone()).or_default().replace(request);
    Ok(graph::expand(&definitions)?.remove(&id).unwrap_or_default())
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

fn validate_request(request: &SaveVirtualAccount) -> Result<()> {
    ensure!(
        !request.name.trim().is_empty() && request.name.len() <= 200,
        "virtual account name must contain 1–200 bytes"
    );
    effective_bindings(&request.bindings, 1.0)?;
    let mut ids = BTreeSet::new();
    for child in request.children.iter().flatten() {
        crate::order_config::validate_strategy_name(&child.virtual_id)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            ids.insert(&child.virtual_id),
            "duplicate child virtual account: {}",
            child.virtual_id
        );
        validate_nonnegative_multiplier(child.multiplier, "child multiplier")
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

fn next_virtual_id(ids: &[String]) -> Result<String> {
    let mut highest = 0_u64;
    for id in ids {
        if let Some(number) = id
            .strip_prefix("virtual")
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            highest = highest.max(number.parse::<u64>()?);
        }
    }
    let next = highest
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("virtual account number exceeds the supported range"))?;
    Ok(format!("virtual{next:02}"))
}

pub async fn create(
    pool: &PgPool,
    request: &SaveVirtualAccount,
    owner: Option<i64>,
    at: i64,
) -> Result<String> {
    validate_request(request)?;
    // Allocate and insert under the same configuration lock/transaction as saves.
    let mut tx = begin(pool).await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT virtual_id FROM cta_virtual_accounts")
        .fetch_all(&mut *tx)
        .await?;
    let id = next_virtual_id(&ids)?;
    save_in_transaction(&mut tx, &id, request, at).await?;
    sqlx::query("UPDATE cta_virtual_accounts SET created_by_user_id = $2 WHERE virtual_id = $1")
        .bind(&id)
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn grantees(pool: &PgPool) -> Result<Vec<VirtualGrantee>> {
    sqlx::query("SELECT user_id, username FROM cta_users WHERE NOT disabled ORDER BY username")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| {
            Ok(VirtualGrantee {
                user_id: row.try_get("user_id")?,
                username: row.try_get("username")?,
            })
        })
        .collect()
}

pub async fn set_managers(pool: &PgPool, id: &str, user_ids: &[i64]) -> Result<()> {
    let ids: BTreeSet<i64> = user_ids.iter().copied().collect();
    ensure!(ids.len() == user_ids.len(), "duplicate Virtual manager");
    let mut tx = begin(pool).await?;
    let valid: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cta_users WHERE user_id = ANY($1) AND NOT disabled",
    )
    .bind(user_ids)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        valid as usize == ids.len(),
        "unknown or disabled Virtual manager"
    );
    sqlx::query("DELETE FROM cta_virtual_account_managers WHERE virtual_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for user_id in ids {
        sqlx::query(
            "INSERT INTO cta_virtual_account_managers (virtual_id, user_id) VALUES ($1, $2)",
        )
        .bind(id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn save(pool: &PgPool, id: &str, request: &SaveVirtualAccount, at: i64) -> Result<()> {
    crate::order_config::validate_strategy_name(id).map_err(anyhow::Error::msg)?;
    validate_request(request)?;
    let mut tx = begin(pool).await?;
    save_in_transaction(&mut tx, id, request, at).await?;
    tx.commit().await?;
    Ok(())
}

async fn save_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    request: &SaveVirtualAccount,
    at: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO cta_virtual_accounts (virtual_id,name,updated_at_us) VALUES ($1,$2,$3) ON CONFLICT (virtual_id) DO UPDATE SET name = EXCLUDED.name, updated_at_us = EXCLUDED.updated_at_us")
        .bind(id).bind(request.name.trim()).bind(at).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM cta_virtual_account_bindings WHERE virtual_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    for b in &request.bindings {
        sqlx::query("INSERT INTO cta_virtual_account_bindings (virtual_id,binding_name,position_strategy_name,order_strategy_name,shares) VALUES ($1,$2,$3,$4,$5)")
            .bind(id).bind(&b.binding_name).bind(&b.position_strategy_name).bind(&b.order_strategy_name).bind(b.shares).execute(&mut **tx).await?;
    }
    // Older clients that do not edit children must not silently remove a nested portfolio.
    if let Some(children) = &request.children {
        sqlx::query("DELETE FROM cta_virtual_account_children WHERE parent_virtual_id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        for child in children {
            sqlx::query("INSERT INTO cta_virtual_account_children (parent_virtual_id, child_virtual_id, multiplier) VALUES ($1,$2,$3)")
                .bind(id).bind(&child.virtual_id).bind(child.multiplier).execute(&mut **tx).await?;
        }
    }
    // Validate the entire DAG inside the configuration transaction, including zero edges.
    let definitions = graph::load(tx).await?;
    let expanded = graph::expand(&definitions)?;
    let followers = followers_on(tx, id).await?;
    for source in followers {
        let follows = load_follows(tx, &source).await?;
        let combined = combined_bindings(&follows, &expanded)?;
        materialize(tx, &source, &combined, 1.0, at, false).await?;
    }
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
        AccountConfiguration::Follow { follows } => {
            let bindings = load_combined_bindings(&mut tx, follows, None).await?;
            materialize(&mut tx, source, &bindings, 1.0, at, true).await?;
            sqlx::query("DELETE FROM cta_account_follows WHERE source_id = $1")
                .bind(source)
                .execute(&mut *tx)
                .await?;
            for follow in follows {
                sqlx::query("INSERT INTO cta_account_follows (source_id,virtual_id,multiplier,updated_at_us) VALUES ($1,$2,$3,$4)")
                    .bind(source).bind(&follow.virtual_id).bind(follow.multiplier).bind(at).execute(&mut *tx).await?;
            }
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
    let parent: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cta_virtual_account_children WHERE child_virtual_id = $1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        !parent,
        "virtual account is still referenced by another Virtual; remove the child reference first"
    );
    if followed {
        bail!(
            "virtual account still has followers; remove this virtual from their follow lists first"
        );
    }
    let result = sqlx::query("DELETE FROM cta_virtual_accounts WHERE virtual_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

pub async fn enqueue_order_followers(pool: &PgPool, order: &str) -> Result<()> {
    sqlx::query("INSERT INTO cta_follow_publish_queue (source_id,binding_name,revision) SELECT b.source_id,b.binding_name,1 FROM cta_account_strategy_bindings b JOIN cta_position_strategies p ON p.strategy_name = b.position_strategy_name WHERE b.shares > 0 AND EXISTS (SELECT 1 FROM cta_account_follows f WHERE f.source_id = b.source_id) AND (b.order_strategy_name = $1 OR EXISTS (SELECT 1 FROM jsonb_each_text(p.symbol_order_strategy_overrides) o WHERE o.value = $1)) ON CONFLICT (source_id,binding_name) DO UPDATE SET revision = cta_follow_publish_queue.revision + 1, archived = false, error = NULL")
        .bind(order).execute(pool).await?;
    Ok(())
}

async fn followers_on(conn: &mut PgConnection, id: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE ancestors(virtual_id) AS (SELECT $1::text UNION SELECT c.parent_virtual_id FROM cta_virtual_account_children c JOIN ancestors a ON c.child_virtual_id = a.virtual_id) SELECT DISTINCT f.source_id FROM cta_account_follows f JOIN ancestors a USING (virtual_id) ORDER BY f.source_id",
    ).bind(id).fetch_all(conn).await?)
}

pub async fn followers(pool: &PgPool, id: &str) -> Result<Vec<String>> {
    followers_on(&mut *pool.acquire().await?, id).await
}

// Used by validation before a template is changed or attached to a live account.
pub async fn validate_source_bindings(
    pool: &PgPool,
    source: &crate::config::SourceConfig,
    bindings: &[VirtualBinding],
    positions: &[crate::strategy_catalog::PositionStrategy],
) -> Result<()> {
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
            crate::exec_routing::validate_target_symbol(source, symbol)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_leaf_visibility_and_virtual_delegation_preserve_permissions() {
        let mut account = VirtualAccount {
            virtual_id: "virtual01".into(),
            name: "model".into(),
            bindings: vec![],
            children: vec![],
            expanded_bindings: vec![],
            parents: vec![],
            updated_at_us: 1,
            followers: vec![],
            created_by_user_id: Some(1),
            owner_username: Some("owner".into()),
            managers: vec![],
            can_configure: false,
            can_manage_grants: false,
        };
        let owner = crate::auth::AuthUser {
            user_id: 1,
            username: "owner".into(),
            role: "user".into(),
        };
        let guest = crate::auth::AuthUser {
            user_id: 2,
            username: "guest".into(),
            role: "user".into(),
        };
        account.expanded_bindings = ["visible", "private"]
            .into_iter()
            .map(|name| VirtualBinding {
                binding_name: name.into(),
                position_strategy_name: name.into(),
                order_strategy_name: "maker".into(),
                shares: 0.0,
            })
            .collect();
        let mut visibility = vec![crate::strategy_catalog::PositionVisibility {
            strategy_name: "visible".into(),
            created_by_user_id: None,
            open_visibility: true,
            manager_user_ids: vec![],
            grants: BTreeMap::new(),
        }];
        // An empty direct-binding list cannot bypass private inherited leaves, even at zero weight.
        assert!(!account.user_can_view(&guest, &visibility));
        assert!(account.user_can_view(&owner, &[]));
        visibility.push(crate::strategy_catalog::PositionVisibility {
            strategy_name: "private".into(),
            created_by_user_id: None,
            open_visibility: false,
            manager_user_ids: vec![],
            grants: BTreeMap::from([(guest.user_id, "view".into())]),
        });
        assert!(account.user_can_view(&guest, &visibility));
        account.permissions_for(&owner);
        assert!(account.can_configure && account.can_manage_grants);
        account.permissions_for(&guest);
        assert!(!account.can_configure && !account.can_manage_grants);
        account.managers.push(VirtualGrantee {
            user_id: 2,
            username: "guest".into(),
        });
        account.permissions_for(&guest);
        assert!(account.can_configure && !account.can_manage_grants);
        account.managers.clear();
        account.permissions_for(&guest);
        assert!(!account.can_configure && !account.can_manage_grants);
        account.created_by_user_id = None;
        account.permissions_for(&owner);
        assert!(!account.can_configure && !account.can_manage_grants);
        account.permissions_for(&crate::auth::AuthUser {
            role: "admin".into(),
            ..guest
        });
        assert!(account.can_configure && account.can_manage_grants);
    }
    #[test]
    fn virtual_numbers_skip_existing_ids_and_leave_aliases_independent() {
        assert_eq!(next_virtual_id(&[]).unwrap(), "virtual01");
        assert_eq!(next_virtual_id(&["virtual01".into()]).unwrap(), "virtual02");
        assert_eq!(
            next_virtual_id(&["model".into(), "virtual02".into(), "virtual09".into()]).unwrap(),
            "virtual10"
        );
        assert_eq!(
            next_virtual_id(&["virtual99".into()]).unwrap(),
            "virtual100"
        );
        assert_eq!(
            next_virtual_id(&["virtual".into(), "virtual-01".into(), "virtualVIP".into()]).unwrap(),
            "virtual01"
        );
        assert!(next_virtual_id(&[format!("virtual{}", u64::MAX)]).is_err());
    }

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
    #[test]
    fn multiple_virtuals_sum_matching_bindings_and_keep_distinct_publication_names() {
        let binding = |name: &str, shares| VirtualBinding {
            binding_name: name.into(),
            position_strategy_name: "position".into(),
            order_strategy_name: "order".into(),
            shares,
        };
        let templates = BTreeMap::from([
            (
                "a".into(),
                vec![binding("shared", 2.0), binding("only-a", 4.0)],
            ),
            (
                "b".into(),
                vec![binding("shared", 5.0), binding("only-b", 6.0)],
            ),
        ]);
        let follows = vec![
            AccountFollow {
                virtual_id: "a".into(),
                multiplier: 3.0,
            },
            AccountFollow {
                virtual_id: "b".into(),
                multiplier: 0.5,
            },
        ];
        let combined = combined_bindings(&follows, &templates).unwrap();
        assert_eq!(
            combined
                .iter()
                .map(|b| (b.binding_name.as_str(), b.shares))
                .collect::<Vec<_>>(),
            vec![("only-a", 12.0), ("only-b", 3.0), ("shared", 8.5)]
        );
        let mut reversed = follows.clone();
        reversed.reverse();
        assert_eq!(combined, combined_bindings(&reversed, &templates).unwrap());
        let mut paused = follows.clone();
        paused[0].multiplier = 0.0;
        assert_eq!(
            combined_bindings(&paused, &templates).unwrap()[2].shares,
            2.5
        );
        assert_eq!(
            combined_bindings(&follows[1..], &templates).unwrap()[1].shares,
            2.5
        );
    }

    #[test]
    fn multiple_virtuals_reject_conflicts_duplicates_empty_follows_and_sum_overflow() {
        let binding = VirtualBinding {
            binding_name: "shared".into(),
            position_strategy_name: "position".into(),
            order_strategy_name: "order".into(),
            shares: f64::MAX,
        };
        let follows = vec![
            AccountFollow {
                virtual_id: "a".into(),
                multiplier: 1.0,
            },
            AccountFollow {
                virtual_id: "b".into(),
                multiplier: 1.0,
            },
        ];
        let mut templates = BTreeMap::from([
            ("a".into(), vec![binding.clone()]),
            ("b".into(), vec![binding]),
        ]);
        assert!(combined_bindings(&follows, &templates).is_err());
        templates.get_mut("b").unwrap()[0].shares = 0.0;
        assert!(combined_bindings(&follows, &templates).is_ok());
        templates.get_mut("b").unwrap()[0].order_strategy_name = "other".into();
        assert!(combined_bindings(&follows, &templates).is_err());
        templates.get_mut("b").unwrap()[0].order_strategy_name = "order".into();
        templates.get_mut("b").unwrap()[0].position_strategy_name = "other".into();
        assert!(combined_bindings(&follows, &templates).is_err());
        assert!(combined_bindings(&[follows[0].clone(), follows[0].clone()], &templates).is_err());
        assert!(combined_bindings(&[], &templates).is_err());
        assert!(combined_bindings(&follows, &BTreeMap::new()).is_err());
    }
}
