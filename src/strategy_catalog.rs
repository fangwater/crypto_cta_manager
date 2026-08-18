use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgPool;

use crate::order_config::{OrderParameters, TargetPosition, validate_strategy_name};

pub const DEFAULT_POSITION_EQUITY_USDT: f64 = 10_000.0;
pub const DEFAULT_ACCOUNT_LEVERAGE: f64 = 1.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionStrategy {
    pub strategy_name: String,
    pub equity_usdt: f64,
    pub targets: BTreeMap<String, TargetPosition>,
    pub updated_at_us: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderStrategy {
    pub strategy_name: String,
    pub order_parameters: OrderParameters,
    pub updated_at_us: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountBinding {
    pub source_id: String,
    pub binding_name: String,
    pub position_strategy_name: String,
    pub order_strategy_name: String,
    pub shares: f64,
    pub position_equity_usdt: f64,
    pub allocation_ratio: f64,
    pub updated_at_us: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStudio {
    pub source_id: String,
    pub leverage: f64,
    pub bound_equity_usdt: f64,
    pub bindings: Vec<AccountBinding>,
    pub updated_at_us: i64,
}

#[derive(Debug, Deserialize)]
pub struct SavePositionStrategyRequest {
    pub strategy_name: String,
    #[serde(default = "default_position_equity")]
    pub equity_usdt: f64,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetPosition>,
}

#[derive(Debug, Deserialize)]
pub struct SaveOrderStrategyRequest {
    pub strategy_name: String,
    pub order_parameters: OrderParameters,
}

#[derive(Debug, Deserialize)]
pub struct SaveAccountSettingsRequest {
    pub leverage: f64,
}

#[derive(Debug, Deserialize)]
pub struct SaveBindingRequest {
    pub binding_name: String,
    pub position_strategy_name: String,
    pub order_strategy_name: String,
    #[serde(default = "default_shares")]
    pub shares: f64,
}

#[derive(Debug, Deserialize)]
pub struct SaveBindingSharesRequest {
    pub shares: f64,
}

#[derive(Debug, Deserialize)]
pub struct SaveAllocationsRequest {
    pub allocations: BTreeMap<String, f64>,
}

fn default_shares() -> f64 {
    1.0
}

fn default_position_equity() -> f64 {
    DEFAULT_POSITION_EQUITY_USDT
}

pub fn allocated_equity(binding: &AccountBinding) -> f64 {
    binding.shares * binding.position_equity_usdt
}

pub fn scale_targets(
    targets: &BTreeMap<String, TargetPosition>,
    shares: f64,
) -> BTreeMap<String, TargetPosition> {
    targets
        .iter()
        .map(|(symbol, target)| {
            (
                symbol.clone(),
                TargetPosition {
                    qty: target.qty * shares,
                    signal: target.signal,
                },
            )
        })
        .collect()
}

pub fn apply_allocation_ratios(
    bindings: &[AccountBinding],
    allocations: &BTreeMap<String, f64>,
) -> Result<Vec<(String, f64)>, String> {
    if bindings.is_empty() {
        return Err("no bindings to allocate".to_string());
    }
    if allocations.len() != bindings.len() {
        return Err("allocations must include every enabled strategy".to_string());
    }
    let mut ratio_sum = 0.0;
    for binding in bindings {
        let Some(ratio) = allocations.get(&binding.binding_name) else {
            return Err(format!("missing allocation for {}", binding.binding_name));
        };
        validate_equity(*ratio, "allocation")?;
        ratio_sum += *ratio;
    }
    if !ratio_sum.is_finite() || ratio_sum <= 0.0 {
        return Err("allocations must sum to a positive finite value".to_string());
    }
    if (ratio_sum - 1.0).abs() > 1e-4 {
        return Err("allocations must sum to 1 (100%)".to_string());
    }
    let total = bindings.iter().map(allocated_equity).sum::<f64>();
    if total <= 0.0 {
        return Err("bound equity must be greater than zero".to_string());
    }
    Ok(bindings
        .iter()
        .map(|binding| {
            let ratio = allocations[&binding.binding_name] / ratio_sum;
            (
                binding.binding_name.clone(),
                ratio * total / binding.position_equity_usdt,
            )
        })
        .collect())
}

pub fn validate_targets(targets: &BTreeMap<String, TargetPosition>) -> Result<(), String> {
    for (symbol, target) in targets {
        if symbol.is_empty()
            || !symbol
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(format!("invalid symbol: {symbol}"));
        }
        if !target.qty.is_finite() {
            return Err(format!("targets.{symbol}.qty must be finite"));
        }
        crate::order_config::validate_target_signal(target.signal)
            .map_err(|error| format!("targets.{symbol}.{error}"))?;
    }
    Ok(())
}

pub fn validate_equity(value: f64, field: &str) -> Result<(), String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{field} must be finite and greater than zero"));
    }
    Ok(())
}

impl PositionStrategy {
    pub fn validate(&self) -> Result<(), String> {
        validate_strategy_name(&self.strategy_name)?;
        validate_equity(self.equity_usdt, "equity_usdt")?;
        validate_targets(&self.targets)?;
        Ok(())
    }
}

impl AccountStudio {
    pub fn from_parts(
        source_id: String,
        leverage: f64,
        updated_at_us: i64,
        bindings: Vec<AccountBinding>,
    ) -> Result<Self, String> {
        validate_equity(leverage, "leverage")?;
        let bound_equity_usdt = bindings.iter().map(allocated_equity).sum();
        let bindings = bindings
            .into_iter()
            .map(|mut binding| {
                binding.allocation_ratio =
                    allocation_ratio(allocated_equity(&binding), bound_equity_usdt);
                binding
            })
            .collect();
        Ok(Self {
            source_id,
            leverage,
            bound_equity_usdt,
            bindings,
            updated_at_us,
        })
    }
}

fn allocation_ratio(part: f64, total: f64) -> f64 {
    if total > 0.0 { part / total } else { 0.0 }
}

pub async fn list_position_strategies(pool: &PgPool) -> Result<Vec<PositionStrategy>> {
    let rows = sqlx::query(
        r#"
        SELECT strategy_name, equity_usdt, targets, updated_at_us
        FROM cta_position_strategies
        ORDER BY strategy_name
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to list position strategies")?;
    rows.into_iter().map(decode_position_row).collect()
}

pub async fn upsert_position_strategy(
    pool: &PgPool,
    request: &SavePositionStrategyRequest,
    updated_at_us: i64,
) -> Result<PositionStrategy> {
    validate_strategy_name(&request.strategy_name).map_err(|error| anyhow::anyhow!(error))?;
    validate_equity(request.equity_usdt, "equity_usdt").map_err(|error| anyhow::anyhow!(error))?;
    validate_targets(&request.targets).map_err(|error| anyhow::anyhow!(error))?;
    let targets = serde_json::to_value(&request.targets)?;
    sqlx::query(
        r#"
        INSERT INTO cta_position_strategies (
            strategy_name, equity_usdt, targets, updated_at_us
        )
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (strategy_name) DO UPDATE SET
            equity_usdt = EXCLUDED.equity_usdt,
            targets = EXCLUDED.targets,
            updated_at_us = EXCLUDED.updated_at_us
        "#,
    )
    .bind(&request.strategy_name)
    .bind(request.equity_usdt)
    .bind(targets)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save position strategy {}", request.strategy_name))?;
    Ok(PositionStrategy {
        strategy_name: request.strategy_name.clone(),
        equity_usdt: request.equity_usdt,
        targets: request.targets.clone(),
        updated_at_us,
    })
}

pub async fn delete_position_strategy(pool: &PgPool, strategy_name: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM cta_position_strategies WHERE strategy_name = $1")
        .bind(strategy_name)
        .execute(pool)
        .await
        .with_context(|| format!("failed to delete position strategy {strategy_name}"))?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_order_strategies(pool: &PgPool) -> Result<Vec<OrderStrategy>> {
    let rows = sqlx::query(
        r#"
        SELECT strategy_name, single_order_usdt, orders_per_batch, maker_price_anchor,
               tick_spacing, batch_interval_ms, maker_timeout_ms, max_maker_requotes,
               target_tolerance_usdt, updated_at_us
        FROM cta_order_strategies
        ORDER BY strategy_name
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to list order strategies")?;
    rows.into_iter().map(decode_order_row).collect()
}

pub async fn upsert_order_strategy(
    pool: &PgPool,
    request: &SaveOrderStrategyRequest,
    updated_at_us: i64,
) -> Result<OrderStrategy> {
    validate_strategy_name(&request.strategy_name).map_err(|error| anyhow::anyhow!(error))?;
    request
        .order_parameters
        .validate()
        .map_err(|error| anyhow::anyhow!(error))?;
    sqlx::query(
        r#"
        INSERT INTO cta_order_strategies (
            strategy_name, single_order_usdt, orders_per_batch, maker_price_anchor,
            tick_spacing, batch_interval_ms, maker_timeout_ms, max_maker_requotes,
            target_tolerance_usdt, updated_at_us
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT (strategy_name) DO UPDATE SET
            single_order_usdt = EXCLUDED.single_order_usdt,
            orders_per_batch = EXCLUDED.orders_per_batch,
            maker_price_anchor = EXCLUDED.maker_price_anchor,
            tick_spacing = EXCLUDED.tick_spacing,
            batch_interval_ms = EXCLUDED.batch_interval_ms,
            maker_timeout_ms = EXCLUDED.maker_timeout_ms,
            max_maker_requotes = EXCLUDED.max_maker_requotes,
            target_tolerance_usdt = EXCLUDED.target_tolerance_usdt,
            updated_at_us = EXCLUDED.updated_at_us
        "#,
    )
    .bind(&request.strategy_name)
    .bind(request.order_parameters.single_order_usdt)
    .bind(i32::try_from(request.order_parameters.orders_per_batch)?)
    .bind(&request.order_parameters.maker_price_anchor)
    .bind(i32::try_from(request.order_parameters.tick_spacing)?)
    .bind(i32::try_from(request.order_parameters.batch_interval_ms)?)
    .bind(i32::try_from(request.order_parameters.maker_timeout_ms)?)
    .bind(i32::try_from(request.order_parameters.max_maker_requotes)?)
    .bind(request.order_parameters.target_tolerance_usdt)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save order strategy {}", request.strategy_name))?;
    Ok(OrderStrategy {
        strategy_name: request.strategy_name.clone(),
        order_parameters: request.order_parameters.clone(),
        updated_at_us,
    })
}

pub async fn delete_order_strategy(pool: &PgPool, strategy_name: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM cta_order_strategies WHERE strategy_name = $1")
        .bind(strategy_name)
        .execute(pool)
        .await
        .with_context(|| format!("failed to delete order strategy {strategy_name}"))?;
    Ok(result.rows_affected() > 0)
}

pub async fn load_account_studio(
    pool: &PgPool,
    source_id: &str,
    now_us: i64,
) -> Result<AccountStudio> {
    let settings = sqlx::query(
        r#"
        SELECT leverage, updated_at_us
        FROM cta_account_settings
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load account settings {source_id}"))?;
    let (leverage, updated_at_us) = if let Some(row) = settings {
        (row.try_get("leverage")?, row.try_get("updated_at_us")?)
    } else {
        sqlx::query(
            r#"
            INSERT INTO cta_account_settings (source_id, leverage, updated_at_us)
            VALUES ($1, $2, $3)
            ON CONFLICT (source_id) DO NOTHING
            "#,
        )
        .bind(source_id)
        .bind(DEFAULT_ACCOUNT_LEVERAGE)
        .bind(now_us)
        .execute(pool)
        .await
        .ok();
        (DEFAULT_ACCOUNT_LEVERAGE, now_us)
    };
    let bindings = list_bindings(pool, source_id).await?;
    AccountStudio::from_parts(source_id.to_string(), leverage, updated_at_us, bindings)
        .map_err(|error| anyhow::anyhow!(error))
}

pub async fn save_account_settings(
    pool: &PgPool,
    source_id: &str,
    request: &SaveAccountSettingsRequest,
    updated_at_us: i64,
) -> Result<AccountStudio> {
    validate_equity(request.leverage, "leverage").map_err(|error| anyhow::anyhow!(error))?;
    sqlx::query(
        r#"
        INSERT INTO cta_account_settings (source_id, leverage, updated_at_us)
        VALUES ($1, $2, $3)
        ON CONFLICT (source_id) DO UPDATE SET
            leverage = EXCLUDED.leverage,
            updated_at_us = EXCLUDED.updated_at_us
        "#,
    )
    .bind(source_id)
    .bind(request.leverage)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save account settings {source_id}"))?;
    load_account_studio(pool, source_id, updated_at_us).await
}

pub async fn save_binding(
    pool: &PgPool,
    source_id: &str,
    request: &SaveBindingRequest,
    updated_at_us: i64,
) -> Result<AccountStudio> {
    validate_strategy_name(&request.binding_name).map_err(|error| anyhow::anyhow!(error))?;
    validate_strategy_name(&request.position_strategy_name)
        .map_err(|error| anyhow::anyhow!(error))?;
    validate_strategy_name(&request.order_strategy_name).map_err(|error| anyhow::anyhow!(error))?;
    validate_equity(request.shares, "shares").map_err(|error| anyhow::anyhow!(error))?;
    if !list_position_strategies(pool)
        .await?
        .iter()
        .any(|strategy| strategy.strategy_name == request.position_strategy_name)
    {
        bail!(
            "position strategy is unknown: {}",
            request.position_strategy_name
        );
    }
    if !list_order_strategies(pool)
        .await?
        .iter()
        .any(|strategy| strategy.strategy_name == request.order_strategy_name)
    {
        bail!("order strategy is unknown: {}", request.order_strategy_name);
    }
    sqlx::query(
        r#"
        INSERT INTO cta_account_strategy_bindings (
            source_id, binding_name, position_strategy_name, order_strategy_name, shares, updated_at_us
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (source_id, binding_name) DO UPDATE SET
            position_strategy_name = EXCLUDED.position_strategy_name,
            order_strategy_name = EXCLUDED.order_strategy_name,
            shares = EXCLUDED.shares,
            updated_at_us = EXCLUDED.updated_at_us
        "#,
    )
    .bind(source_id)
    .bind(&request.binding_name)
    .bind(&request.position_strategy_name)
    .bind(&request.order_strategy_name)
    .bind(request.shares)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| {
        format!(
            "failed to bind {} + {} as {} on {source_id}",
            request.position_strategy_name, request.order_strategy_name, request.binding_name
        )
    })?;
    load_account_studio(pool, source_id, updated_at_us).await
}

pub async fn save_binding_shares(
    pool: &PgPool,
    source_id: &str,
    binding_name: &str,
    request: &SaveBindingSharesRequest,
    updated_at_us: i64,
) -> Result<AccountStudio> {
    validate_strategy_name(binding_name).map_err(|error| anyhow::anyhow!(error))?;
    validate_equity(request.shares, "shares").map_err(|error| anyhow::anyhow!(error))?;
    let result = sqlx::query(
        r#"
        UPDATE cta_account_strategy_bindings
        SET shares = $3, updated_at_us = $4
        WHERE source_id = $1 AND binding_name = $2
        "#,
    )
    .bind(source_id)
    .bind(binding_name)
    .bind(request.shares)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save shares for {binding_name} on {source_id}"))?;
    if result.rows_affected() == 0 {
        bail!("binding is unknown: {binding_name}");
    }
    load_account_studio(pool, source_id, updated_at_us).await
}

pub async fn save_allocations(
    pool: &PgPool,
    source_id: &str,
    request: &SaveAllocationsRequest,
    updated_at_us: i64,
) -> Result<AccountStudio> {
    let studio = load_account_studio(pool, source_id, updated_at_us).await?;
    let updates = apply_allocation_ratios(&studio.bindings, &request.allocations)
        .map_err(|error| anyhow::anyhow!(error))?;
    let mut tx = pool.begin().await?;
    for (binding_name, shares) in updates {
        let result = sqlx::query(
            r#"
            UPDATE cta_account_strategy_bindings
            SET shares = $3, updated_at_us = $4
            WHERE source_id = $1 AND binding_name = $2
            "#,
        )
        .bind(source_id)
        .bind(&binding_name)
        .bind(shares)
        .bind(updated_at_us)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to save allocation for {binding_name} on {source_id}"))?;
        if result.rows_affected() == 0 {
            bail!("binding is unknown: {binding_name}");
        }
    }
    tx.commit()
        .await
        .context("failed to commit allocation updates")?;
    load_account_studio(pool, source_id, updated_at_us).await
}

pub async fn delete_binding(pool: &PgPool, source_id: &str, binding_name: &str) -> Result<bool> {
    let result = sqlx::query(
        r#"
        DELETE FROM cta_account_strategy_bindings
        WHERE source_id = $1 AND binding_name = $2
        "#,
    )
    .bind(source_id)
    .bind(binding_name)
    .execute(pool)
    .await
    .with_context(|| format!("failed to delete binding {binding_name} on {source_id}"))?;
    Ok(result.rows_affected() > 0)
}

pub async fn load_binding_parts(
    pool: &PgPool,
    source_id: &str,
    binding_name: &str,
) -> Result<Option<(PositionStrategy, OrderStrategy, f64)>> {
    let row = sqlx::query(
        r#"
        SELECT
            p.strategy_name AS position_name,
            p.equity_usdt,
            p.targets,
            p.updated_at_us AS position_updated_at_us,
            o.strategy_name AS order_name,
            o.single_order_usdt,
            o.orders_per_batch,
            o.maker_price_anchor,
            o.tick_spacing,
            o.batch_interval_ms,
            o.maker_timeout_ms,
            o.max_maker_requotes,
            o.target_tolerance_usdt,
            o.updated_at_us AS order_updated_at_us,
            b.shares
        FROM cta_account_strategy_bindings b
        JOIN cta_position_strategies p ON p.strategy_name = b.position_strategy_name
        JOIN cta_order_strategies o ON o.strategy_name = b.order_strategy_name
        WHERE b.source_id = $1 AND b.binding_name = $2
        "#,
    )
    .bind(source_id)
    .bind(binding_name)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load binding {binding_name} on {source_id}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some((
        PositionStrategy {
            strategy_name: row.try_get("position_name")?,
            equity_usdt: row.try_get("equity_usdt")?,
            targets: serde_json::from_value(row.try_get("targets")?)?,
            updated_at_us: row.try_get("position_updated_at_us")?,
        },
        OrderStrategy {
            strategy_name: row.try_get("order_name")?,
            order_parameters: OrderParameters {
                single_order_usdt: row.try_get("single_order_usdt")?,
                orders_per_batch: u32::try_from(row.try_get::<i32, _>("orders_per_batch")?)?,
                maker_price_anchor: row.try_get("maker_price_anchor")?,
                tick_spacing: u32::try_from(row.try_get::<i32, _>("tick_spacing")?)?,
                batch_interval_ms: u32::try_from(row.try_get::<i32, _>("batch_interval_ms")?)?,
                maker_timeout_ms: u32::try_from(row.try_get::<i32, _>("maker_timeout_ms")?)?,
                max_maker_requotes: u32::try_from(row.try_get::<i32, _>("max_maker_requotes")?)?,
                target_tolerance_usdt: row.try_get("target_tolerance_usdt")?,
            },
            updated_at_us: row.try_get("order_updated_at_us")?,
        },
        row.try_get("shares")?,
    )))
}

async fn list_bindings(pool: &PgPool, source_id: &str) -> Result<Vec<AccountBinding>> {
    let rows = sqlx::query(
        r#"
        SELECT
            b.binding_name,
            b.position_strategy_name,
            b.order_strategy_name,
            b.shares,
            b.updated_at_us,
            p.equity_usdt
        FROM cta_account_strategy_bindings b
        JOIN cta_position_strategies p ON p.strategy_name = b.position_strategy_name
        WHERE b.source_id = $1
        ORDER BY b.binding_name
        "#,
    )
    .bind(source_id)
    .fetch_all(pool)
    .await
    .with_context(|| format!("failed to list bindings for {source_id}"))?;
    rows.into_iter()
        .map(|row| {
            Ok(AccountBinding {
                source_id: source_id.to_string(),
                binding_name: row.try_get("binding_name")?,
                position_strategy_name: row.try_get("position_strategy_name")?,
                order_strategy_name: row.try_get("order_strategy_name")?,
                shares: row.try_get("shares")?,
                position_equity_usdt: row.try_get("equity_usdt")?,
                allocation_ratio: 0.0,
                updated_at_us: row.try_get("updated_at_us")?,
            })
        })
        .collect()
}

fn decode_position_row(row: sqlx::postgres::PgRow) -> Result<PositionStrategy> {
    Ok(PositionStrategy {
        strategy_name: row.try_get("strategy_name")?,
        equity_usdt: row.try_get("equity_usdt")?,
        targets: serde_json::from_value(row.try_get("targets")?)?,
        updated_at_us: row.try_get("updated_at_us")?,
    })
}

fn decode_order_row(row: sqlx::postgres::PgRow) -> Result<OrderStrategy> {
    Ok(OrderStrategy {
        strategy_name: row.try_get("strategy_name")?,
        order_parameters: OrderParameters {
            single_order_usdt: row.try_get("single_order_usdt")?,
            orders_per_batch: u32::try_from(row.try_get::<i32, _>("orders_per_batch")?)?,
            maker_price_anchor: row.try_get("maker_price_anchor")?,
            tick_spacing: u32::try_from(row.try_get::<i32, _>("tick_spacing")?)?,
            batch_interval_ms: u32::try_from(row.try_get::<i32, _>("batch_interval_ms")?)?,
            maker_timeout_ms: u32::try_from(row.try_get::<i32, _>("maker_timeout_ms")?)?,
            max_maker_requotes: u32::try_from(row.try_get::<i32, _>("max_maker_requotes")?)?,
            target_tolerance_usdt: row.try_get("target_tolerance_usdt")?,
        },
        updated_at_us: row.try_get("updated_at_us")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_share_reference_equity_not_account_capacity() {
        let studio = AccountStudio::from_parts(
            "binance_exec_trade01".into(),
            2.0,
            1,
            vec![
                AccountBinding {
                    source_id: "binance_exec_trade01".into(),
                    binding_name: "combo_a".into(),
                    position_strategy_name: "pos_a".into(),
                    order_strategy_name: "ord_a".into(),
                    shares: 1.0,
                    position_equity_usdt: 10_000.0,
                    allocation_ratio: 0.0,
                    updated_at_us: 1,
                },
                AccountBinding {
                    source_id: "binance_exec_trade01".into(),
                    binding_name: "combo_b".into(),
                    position_strategy_name: "pos_b".into(),
                    order_strategy_name: "ord_b".into(),
                    shares: 1.0,
                    position_equity_usdt: 30_000.0,
                    allocation_ratio: 0.0,
                    updated_at_us: 1,
                },
            ],
        )
        .unwrap();
        assert_eq!(studio.leverage, 2.0);
        assert_eq!(studio.bound_equity_usdt, 40_000.0);
        assert!((studio.bindings[0].allocation_ratio - 0.25).abs() < 1e-12);
        assert!((studio.bindings[1].allocation_ratio - 0.75).abs() < 1e-12);
    }

    #[test]
    fn binding_shares_scale_allocation_and_targets() {
        let studio = AccountStudio::from_parts(
            "binance_exec_trade01".into(),
            1.0,
            1,
            vec![
                AccountBinding {
                    source_id: "binance_exec_trade01".into(),
                    binding_name: "cta_a".into(),
                    position_strategy_name: "cta_a".into(),
                    order_strategy_name: "default_order".into(),
                    shares: 1.0,
                    position_equity_usdt: 10_000.0,
                    allocation_ratio: 0.0,
                    updated_at_us: 1,
                },
                AccountBinding {
                    source_id: "binance_exec_trade01".into(),
                    binding_name: "cta_b".into(),
                    position_strategy_name: "cta_b".into(),
                    order_strategy_name: "default_order".into(),
                    shares: 3.0,
                    position_equity_usdt: 10_000.0,
                    allocation_ratio: 0.0,
                    updated_at_us: 1,
                },
            ],
        )
        .unwrap();
        assert_eq!(studio.bound_equity_usdt, 40_000.0);
        assert!((studio.bindings[0].allocation_ratio - 0.25).abs() < 1e-12);
        assert!((studio.bindings[1].allocation_ratio - 0.75).abs() < 1e-12);
        let scaled = scale_targets(
            &BTreeMap::from([(
                "BTCUSDT".into(),
                TargetPosition {
                    qty: -0.006,
                    signal: -1,
                },
            )]),
            3.0,
        );
        assert!((scaled["BTCUSDT"].qty + 0.018).abs() < 1e-12);
        assert_eq!(scaled["BTCUSDT"].signal, -1);
        let next = apply_allocation_ratios(
            &studio.bindings,
            &BTreeMap::from([("cta_a".into(), 0.5), ("cta_b".into(), 0.5)]),
        )
        .unwrap();
        assert!((next[0].1 - 2.0).abs() < 1e-9);
        assert!((next[1].1 - 2.0).abs() < 1e-9);
        let rejected = apply_allocation_ratios(
            &studio.bindings,
            &BTreeMap::from([("cta_a".into(), 0.4), ("cta_b".into(), 0.4)]),
        );
        assert!(rejected.is_err());
    }

    #[test]
    fn leverage_must_be_positive_and_finite() {
        assert!(validate_equity(2.0, "leverage").is_ok());
        assert!(validate_equity(0.0, "leverage").is_err());
        assert!(validate_equity(-1.0, "leverage").is_err());
        assert!(validate_equity(f64::NAN, "leverage").is_err());
    }
}
