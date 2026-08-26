use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgPool;

use crate::config::{FeeRates, validate_fee_rates};
use crate::order_config::{
    OrderParameters, TargetPosition, validate_exec_symbol, validate_strategy_name,
};

pub const DEFAULT_CONTRACT_LEVERAGE: i32 = 5;
pub const MIN_CONTRACT_LEVERAGE: i32 = 1;
pub const MAX_CONTRACT_LEVERAGE: i32 = 125;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionStrategy {
    pub strategy_name: String,
    pub targets: BTreeMap<String, TargetPosition>,
    #[serde(default)]
    pub symbol_order_strategy_overrides: BTreeMap<String, String>,
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
    pub updated_at_us: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStudio {
    pub source_id: String,
    /// NAV estimated trading fee rate as a fraction (e.g. 0.0004 = 4 bps).
    pub estimated_fee_rate: f64,
    pub maker_fee_rate: f64,
    pub taker_fee_rate: f64,
    pub bindings: Vec<AccountBinding>,
}

#[derive(Debug, Deserialize)]
pub struct SaveEstimatedFeeRateRequest {
    pub estimated_fee_rate: f64,
}

#[derive(Debug, Deserialize)]
pub struct SaveFeeRatesRequest {
    pub maker_fee_rate: f64,
    pub taker_fee_rate: f64,
}

#[derive(Debug, Deserialize)]
pub struct SavePositionStrategyRequest {
    pub strategy_name: String,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetPosition>,
    #[serde(default)]
    pub symbol_order_strategy_overrides: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveOrderStrategyRequest {
    pub strategy_name: String,
    pub order_parameters: OrderParameters,
}

#[derive(Debug, Deserialize)]
pub struct SaveSymbolContractLeverageRequest {
    pub symbol: String,
    pub contract_leverage: i32,
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

fn default_shares() -> f64 {
    1.0
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

pub fn validate_targets(targets: &BTreeMap<String, TargetPosition>) -> Result<(), String> {
    for (symbol, target) in targets {
        validate_exec_symbol(symbol)?;
        if !target.qty.is_finite() {
            return Err(format!("targets.{symbol}.qty must be finite"));
        }
        crate::order_config::validate_target_signal(target.signal)
            .map_err(|error| format!("targets.{symbol}.{error}"))?;
    }
    Ok(())
}

pub fn validate_symbol_order_strategy_overrides(
    targets: &BTreeMap<String, TargetPosition>,
    overrides: &BTreeMap<String, String>,
) -> Result<(), String> {
    for (symbol, order_strategy_name) in overrides {
        validate_exec_symbol(symbol)?;
        if !targets.contains_key(symbol) {
            return Err(format!(
                "symbol_order_strategy_overrides.{symbol} has no matching target"
            ));
        }
        validate_strategy_name(order_strategy_name)
            .map_err(|error| format!("symbol_order_strategy_overrides.{symbol}.{error}"))?;
    }
    Ok(())
}

pub fn validate_positive_multiplier(value: f64, field: &str) -> Result<(), String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{field} must be finite and greater than zero"));
    }
    Ok(())
}

pub fn validate_contract_leverage(value: i32) -> Result<(), String> {
    if !(MIN_CONTRACT_LEVERAGE..=MAX_CONTRACT_LEVERAGE).contains(&value) {
        return Err(format!(
            "contract_leverage must be an integer from {MIN_CONTRACT_LEVERAGE} to {MAX_CONTRACT_LEVERAGE}"
        ));
    }
    Ok(())
}

pub fn validate_estimated_fee_rate(value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err("estimated_fee_rate must be finite".to_string());
    }
    Ok(())
}

pub fn validate_account_fee_rates(maker: f64, taker: f64) -> Result<(), String> {
    validate_fee_rates(FeeRates { maker, taker }).map_err(|error| error.to_string())
}

pub fn validate_contract_symbol(symbol: &str) -> Result<(), String> {
    validate_exec_symbol(symbol)
}

impl PositionStrategy {
    pub fn validate(&self) -> Result<(), String> {
        validate_strategy_name(&self.strategy_name)?;
        validate_targets(&self.targets)?;
        validate_symbol_order_strategy_overrides(
            &self.targets,
            &self.symbol_order_strategy_overrides,
        )?;
        Ok(())
    }
}

impl AccountStudio {
    pub fn from_parts(
        source_id: String,
        fee_rates: FeeRates,
        bindings: Vec<AccountBinding>,
    ) -> Self {
        Self {
            source_id,
            estimated_fee_rate: fee_rates.taker,
            maker_fee_rate: fee_rates.maker,
            taker_fee_rate: fee_rates.taker,
            bindings,
        }
    }
}

pub async fn list_binding_source_ids_for_position(
    pool: &PgPool,
    strategy_name: &str,
) -> Result<Vec<String>> {
    let bindings = list_bindings_for_position(pool, strategy_name).await?;
    let mut source_ids = bindings
        .into_iter()
        .map(|binding| binding.source_id)
        .collect::<Vec<_>>();
    source_ids.sort();
    source_ids.dedup();
    Ok(source_ids)
}

#[derive(Debug, Clone, PartialEq)]
pub struct BindingPublishSnapshot {
    pub source_id: String,
    pub binding_name: String,
    pub shares: f64,
}

pub async fn list_publish_snapshots_for_position(
    pool: &PgPool,
    strategy_name: &str,
) -> Result<Vec<BindingPublishSnapshot>> {
    let rows = sqlx::query(
        r#"
        SELECT
            b.source_id,
            b.binding_name,
            b.shares
        FROM cta_account_strategy_bindings b
        WHERE b.position_strategy_name = $1
        ORDER BY b.source_id, b.binding_name
        "#,
    )
    .bind(strategy_name)
    .fetch_all(pool)
    .await
    .with_context(|| {
        format!("failed to list publish snapshots for position strategy {strategy_name}")
    })?;
    rows.into_iter()
        .map(|row| {
            Ok(BindingPublishSnapshot {
                source_id: row.try_get("source_id")?,
                binding_name: row.try_get("binding_name")?,
                shares: row.try_get("shares")?,
            })
        })
        .collect()
}

pub async fn list_bindings_for_position(
    pool: &PgPool,
    strategy_name: &str,
) -> Result<Vec<AccountBinding>> {
    let rows = sqlx::query(
        r#"
        SELECT
            b.source_id,
            b.binding_name,
            b.position_strategy_name,
            b.order_strategy_name,
            b.shares,
            b.updated_at_us
        FROM cta_account_strategy_bindings b
        WHERE b.position_strategy_name = $1
        ORDER BY b.source_id, b.binding_name
        "#,
    )
    .bind(strategy_name)
    .fetch_all(pool)
    .await
    .with_context(|| format!("failed to list bindings for position strategy {strategy_name}"))?;
    rows.into_iter()
        .map(|row| {
            Ok(AccountBinding {
                source_id: row.try_get("source_id")?,
                binding_name: row.try_get("binding_name")?,
                position_strategy_name: row.try_get("position_strategy_name")?,
                order_strategy_name: row.try_get("order_strategy_name")?,
                shares: row.try_get("shares")?,
                updated_at_us: row.try_get("updated_at_us")?,
            })
        })
        .collect()
}

pub async fn list_position_strategies(pool: &PgPool) -> Result<Vec<PositionStrategy>> {
    let rows = sqlx::query(
        r#"
        SELECT strategy_name, targets, symbol_order_strategy_overrides, updated_at_us
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
    validate_targets(&request.targets).map_err(|error| anyhow::anyhow!(error))?;
    validate_symbol_order_strategy_overrides(
        &request.targets,
        &request.symbol_order_strategy_overrides,
    )
    .map_err(|error| anyhow::anyhow!(error))?;
    let known_order_strategies = list_order_strategies(pool)
        .await?
        .into_iter()
        .map(|strategy| strategy.strategy_name)
        .collect::<BTreeSet<_>>();
    for (symbol, order_strategy_name) in &request.symbol_order_strategy_overrides {
        if !known_order_strategies.contains(order_strategy_name) {
            bail!(
                "symbol_order_strategy_overrides.{symbol} references unknown order strategy: {order_strategy_name}"
            );
        }
    }
    let targets = serde_json::to_value(&request.targets)?;
    let symbol_order_strategy_overrides =
        serde_json::to_value(&request.symbol_order_strategy_overrides)?;
    sqlx::query(
        r#"
        INSERT INTO cta_position_strategies (
            strategy_name, targets, symbol_order_strategy_overrides, updated_at_us
        )
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (strategy_name) DO UPDATE SET
            targets = EXCLUDED.targets,
            symbol_order_strategy_overrides = EXCLUDED.symbol_order_strategy_overrides,
            updated_at_us = EXCLUDED.updated_at_us
        "#,
    )
    .bind(&request.strategy_name)
    .bind(targets)
    .bind(symbol_order_strategy_overrides)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save position strategy {}", request.strategy_name))?;
    Ok(PositionStrategy {
        strategy_name: request.strategy_name.clone(),
        targets: request.targets.clone(),
        symbol_order_strategy_overrides: request.symbol_order_strategy_overrides.clone(),
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
        SELECT strategy_name, single_order_usdt, orders_per_batch, max_batch, maker_price_anchor,
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
            strategy_name, single_order_usdt, orders_per_batch, max_batch, maker_price_anchor,
            tick_spacing, batch_interval_ms, maker_timeout_ms, max_maker_requotes,
            target_tolerance_usdt, updated_at_us
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ON CONFLICT (strategy_name) DO UPDATE SET
            single_order_usdt = EXCLUDED.single_order_usdt,
            orders_per_batch = EXCLUDED.orders_per_batch,
            max_batch = EXCLUDED.max_batch,
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
    .bind(i32::try_from(request.order_parameters.max_batch)?)
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
    if let Some(position) = list_position_strategies(pool)
        .await?
        .into_iter()
        .find(|position| {
            position
                .symbol_order_strategy_overrides
                .values()
                .any(|override_name| override_name == strategy_name)
        })
    {
        bail!(
            "order strategy {strategy_name} is referenced by position strategy {}",
            position.strategy_name
        );
    }
    let result = sqlx::query("DELETE FROM cta_order_strategies WHERE strategy_name = $1")
        .bind(strategy_name)
        .execute(pool)
        .await
        .with_context(|| format!("failed to delete order strategy {strategy_name}"))?;
    Ok(result.rows_affected() > 0)
}

pub async fn load_account_studio(pool: &PgPool, source_id: &str) -> Result<AccountStudio> {
    let bindings = list_bindings(pool, source_id).await?;
    let fee_rates = crate::postgres::load_fee_rate(pool, source_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("source {source_id} is not registered in cta_order_sources")
        })?;
    Ok(AccountStudio::from_parts(
        source_id.to_string(),
        fee_rates,
        bindings,
    ))
}

pub async fn save_symbol_contract_leverage(
    pool: &PgPool,
    source_id: &str,
    request: &SaveSymbolContractLeverageRequest,
    updated_at_us: i64,
) -> Result<()> {
    validate_contract_symbol(&request.symbol).map_err(|error| anyhow::anyhow!(error))?;
    validate_contract_leverage(request.contract_leverage)
        .map_err(|error| anyhow::anyhow!(error))?;
    sqlx::query(
        r#"
        INSERT INTO cta_account_symbol_leverages (
            source_id, symbol, contract_leverage, updated_at_us
        )
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (source_id, symbol) DO UPDATE SET
            contract_leverage = EXCLUDED.contract_leverage,
            updated_at_us = EXCLUDED.updated_at_us
        "#,
    )
    .bind(source_id)
    .bind(&request.symbol)
    .bind(request.contract_leverage)
    .bind(updated_at_us)
    .execute(pool)
    .await
    .with_context(|| {
        format!(
            "failed to save contract leverage for {} on {source_id}",
            request.symbol
        )
    })?;
    Ok(())
}

pub async fn load_symbol_contract_leverage(
    pool: &PgPool,
    source_id: &str,
    symbol: &str,
) -> Result<Option<i32>> {
    let row = sqlx::query(
        r#"
        SELECT contract_leverage
        FROM cta_account_symbol_leverages
        WHERE source_id = $1 AND symbol = $2
        "#,
    )
    .bind(source_id)
    .bind(symbol)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load contract leverage for {symbol} on {source_id}"))?;
    row.map(|row| row.try_get("contract_leverage"))
        .transpose()
        .context("failed to decode contract leverage")
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
    validate_positive_multiplier(request.shares, "shares")
        .map_err(|error| anyhow::anyhow!(error))?;
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
    load_account_studio(pool, source_id).await
}

pub async fn save_binding_shares(
    pool: &PgPool,
    source_id: &str,
    binding_name: &str,
    request: &SaveBindingSharesRequest,
    updated_at_us: i64,
) -> Result<AccountStudio> {
    validate_strategy_name(binding_name).map_err(|error| anyhow::anyhow!(error))?;
    validate_positive_multiplier(request.shares, "shares")
        .map_err(|error| anyhow::anyhow!(error))?;
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
    load_account_studio(pool, source_id).await
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
) -> Result<
    Option<(
        PositionStrategy,
        OrderStrategy,
        BTreeMap<String, OrderParameters>,
        f64,
    )>,
> {
    let row = sqlx::query(
        r#"
        SELECT
            p.strategy_name AS position_name,
            p.targets,
            p.symbol_order_strategy_overrides,
            p.updated_at_us AS position_updated_at_us,
            o.strategy_name AS order_name,
            o.single_order_usdt,
            o.orders_per_batch,
            o.max_batch,
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
    let position = PositionStrategy {
        strategy_name: row.try_get("position_name")?,
        targets: serde_json::from_value(row.try_get("targets")?)?,
        symbol_order_strategy_overrides: serde_json::from_value(
            row.try_get("symbol_order_strategy_overrides")?,
        )?,
        updated_at_us: row.try_get("position_updated_at_us")?,
    };
    let symbol_order_parameters =
        load_symbol_order_parameters(pool, &position.symbol_order_strategy_overrides).await?;
    Ok(Some((
        position,
        OrderStrategy {
            strategy_name: row.try_get("order_name")?,
            order_parameters: OrderParameters {
                single_order_usdt: row.try_get("single_order_usdt")?,
                orders_per_batch: u32::try_from(row.try_get::<i32, _>("orders_per_batch")?)?,
                max_batch: u32::try_from(row.try_get::<i32, _>("max_batch")?)?,
                maker_price_anchor: row.try_get("maker_price_anchor")?,
                tick_spacing: u32::try_from(row.try_get::<i32, _>("tick_spacing")?)?,
                batch_interval_ms: u32::try_from(row.try_get::<i32, _>("batch_interval_ms")?)?,
                maker_timeout_ms: u32::try_from(row.try_get::<i32, _>("maker_timeout_ms")?)?,
                max_maker_requotes: u32::try_from(row.try_get::<i32, _>("max_maker_requotes")?)?,
                target_tolerance_usdt: row.try_get("target_tolerance_usdt")?,
            },
            updated_at_us: row.try_get("order_updated_at_us")?,
        },
        symbol_order_parameters,
        row.try_get("shares")?,
    )))
}

async fn load_symbol_order_parameters(
    pool: &PgPool,
    overrides: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, OrderParameters>> {
    if overrides.is_empty() {
        return Ok(BTreeMap::new());
    }
    let named = list_order_strategies(pool)
        .await?
        .into_iter()
        .map(|strategy| (strategy.strategy_name, strategy.order_parameters))
        .collect::<BTreeMap<_, _>>();
    let mut resolved = BTreeMap::new();
    for (symbol, order_strategy_name) in overrides {
        let parameters = named.get(order_strategy_name).ok_or_else(|| {
            anyhow::anyhow!(
                "symbol_order_strategy_overrides.{symbol} references missing order strategy: {order_strategy_name}"
            )
        })?;
        resolved.insert(symbol.clone(), parameters.clone());
    }
    Ok(resolved)
}

async fn list_bindings(pool: &PgPool, source_id: &str) -> Result<Vec<AccountBinding>> {
    let rows = sqlx::query(
        r#"
        SELECT
            b.binding_name,
            b.position_strategy_name,
            b.order_strategy_name,
            b.shares,
            b.updated_at_us
        FROM cta_account_strategy_bindings b
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
                updated_at_us: row.try_get("updated_at_us")?,
            })
        })
        .collect()
}

fn decode_position_row(row: sqlx::postgres::PgRow) -> Result<PositionStrategy> {
    Ok(PositionStrategy {
        strategy_name: row.try_get("strategy_name")?,
        targets: serde_json::from_value(row.try_get("targets")?)?,
        symbol_order_strategy_overrides: serde_json::from_value(
            row.try_get("symbol_order_strategy_overrides")?,
        )?,
        updated_at_us: row.try_get("updated_at_us")?,
    })
}

fn decode_order_row(row: sqlx::postgres::PgRow) -> Result<OrderStrategy> {
    Ok(OrderStrategy {
        strategy_name: row.try_get("strategy_name")?,
        order_parameters: OrderParameters {
            single_order_usdt: row.try_get("single_order_usdt")?,
            orders_per_batch: u32::try_from(row.try_get::<i32, _>("orders_per_batch")?)?,
            max_batch: u32::try_from(row.try_get::<i32, _>("max_batch")?)?,
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
    fn binding_shares_are_the_only_target_multiplier() {
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
    }

    #[test]
    fn symbol_order_strategy_overrides_require_a_target_and_valid_template_name() {
        let targets = BTreeMap::from([(
            "BTCUSDT".to_string(),
            TargetPosition {
                qty: 0.1,
                signal: 0,
            },
        )]);
        assert!(
            validate_symbol_order_strategy_overrides(
                &targets,
                &BTreeMap::from([("BTCUSDT".to_string(), "fast_order".to_string())]),
            )
            .is_ok()
        );
        assert!(
            validate_symbol_order_strategy_overrides(
                &targets,
                &BTreeMap::from([("ETHUSDT".to_string(), "fast_order".to_string())]),
            )
            .is_err()
        );
        assert!(
            validate_symbol_order_strategy_overrides(
                &targets,
                &BTreeMap::from([("BTCUSDT".to_string(), "bad/template".to_string())]),
            )
            .is_err()
        );
    }

    #[test]
    fn official_unicode_symbols_are_valid_targets_and_overrides() {
        let targets = BTreeMap::from([(
            "龙虾USDT".to_string(),
            TargetPosition {
                qty: 0.1,
                signal: 0,
            },
        )]);
        assert!(validate_targets(&targets).is_ok());
        assert!(
            validate_symbol_order_strategy_overrides(
                &targets,
                &BTreeMap::from([("龙虾USDT".to_string(), "fast_order".to_string())]),
            )
            .is_ok()
        );
    }

    #[test]
    fn shares_must_be_positive_and_finite() {
        assert!(validate_positive_multiplier(2.0, "shares").is_ok());
        assert!(validate_positive_multiplier(0.0, "shares").is_err());
        assert!(validate_positive_multiplier(-1.0, "shares").is_err());
        assert!(validate_positive_multiplier(f64::NAN, "shares").is_err());
    }

    #[test]
    fn fee_rates_accept_any_finite_fraction() {
        assert!(validate_estimated_fee_rate(0.0).is_ok());
        assert!(validate_estimated_fee_rate(0.0004).is_ok());
        assert!(validate_estimated_fee_rate(-4.0).is_ok());
        assert!(validate_estimated_fee_rate(4.0).is_ok());
        assert!(validate_estimated_fee_rate(f64::NAN).is_err());
        assert!(validate_estimated_fee_rate(f64::INFINITY).is_err());
        assert!(validate_account_fee_rates(-0.00005, 0.000146).is_ok());
        assert!(validate_account_fee_rates(-100.0, -200.0).is_ok());
    }

    #[test]
    fn contract_leverage_must_be_a_supported_integer() {
        assert!(validate_contract_leverage(5).is_ok());
        assert!(validate_contract_leverage(1).is_ok());
        assert!(validate_contract_leverage(125).is_ok());
        assert!(validate_contract_leverage(0).is_err());
        assert!(validate_contract_leverage(126).is_err());
        assert!(validate_contract_leverage(-1).is_err());
        assert!(validate_contract_symbol("BTCUSDT").is_ok());
        assert!(validate_contract_symbol("龙虾USDT").is_ok());
        assert!(validate_contract_symbol("btc").is_err());
        assert!(validate_contract_symbol("龙虾 USDT").is_err());
    }
}
