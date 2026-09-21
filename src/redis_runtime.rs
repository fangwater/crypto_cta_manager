use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::{RedisSettings, SourceConfig};
use crate::market_rules::MarketRulesSnapshot;
use crate::order_config::{
    ChaseParameterOverrides, ExecutionAlgorithm, ExecutionFamily, OrderParameterOverrides,
    OrderParameters, OrderStrategyView, PovParameters, TargetPosition, validate_exec_symbol,
    validate_strategy_name,
};

const POSITION_CLOSE_STRATEGY_NAME: &str = "SYSTEM_POSITION_CLOSE";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredExecAlgorithmSwitch {
    from_family: String,
    to_family: String,
    state: String,
    requested_at_us: i64,
    updated_at_us: i64,
    #[serde(default)]
    positions: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
struct StoredBatchExecConfig<'a> {
    algorithm: ExecutionAlgorithm,
    pov: &'a PovParameters,
    single_order_usdt: f64,
    orders_per_batch: u32,
    max_batch: u32,
    maker_price_anchor: &'a str,
    tick_spacing: u32,
    batch_interval_ms: u32,
    maker_timeout_ms: u32,
    max_maker_requotes: u32,
    target_tolerance_usdt: f64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    symbol_overrides: &'a BTreeMap<String, OrderParameterOverrides>,
    targets: &'a BTreeMap<String, TargetPosition>,
    updated_at_us: i64,
}

#[derive(Debug, Clone, Serialize)]
struct StoredChaseExecConfig<'a> {
    single_order_usdt: f64,
    max_open_usdt: f64,
    maker_recenter_trigger_bps: f64,
    maker_amend_cooldown_ms: u32,
    maker_timeout_sec: u32,
    target_tolerance_usdt: f64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    symbol_overrides: &'a BTreeMap<String, ChaseParameterOverrides>,
    targets: &'a BTreeMap<String, TargetPosition>,
    updated_at_us: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExecStrategyTargetState {
    pub(crate) family: String,
    pub(crate) updated_at_us: i64,
    pub(crate) targets: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ExecTargetSnapshot {
    pub(crate) aggregate_targets: BTreeMap<String, f64>,
    pub(crate) strategies: BTreeMap<String, ExecStrategyTargetState>,
}

#[derive(Clone)]
pub struct RedisRuntime {
    inner: Arc<Mutex<RedisRuntimeInner>>,
}

struct RedisRuntimeInner {
    settings: RedisSettings,
    client: redis::Client,
    connection: Option<ConnectionManager>,
    last_reconnect_error_at: Option<Instant>,
}

impl RedisRuntime {
    pub fn connect(settings: RedisSettings) -> Result<Self> {
        let client = redis::Client::open(settings.url.clone())
            .with_context(|| format!("invalid redis.url {}", settings.url))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(RedisRuntimeInner {
                settings,
                client,
                connection: None,
                last_reconnect_error_at: None,
            })),
        })
    }

    pub fn spawn_keepalive(&self) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            loop {
                let sleep_ms = {
                    let mut guard = inner.lock().await;
                    let retry_ms = guard.settings.reconnect_interval_ms.max(100);
                    match keep_connection_alive(&mut guard).await {
                        Ok(true) => 5_000,
                        Ok(false) | Err(_) => retry_ms,
                    }
                };
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            }
        });
    }

    pub async fn publish_strategy(
        &self,
        source: &SourceConfig,
        strategy_name: &str,
        order_parameters: &OrderParameters,
        symbol_order_parameters: &BTreeMap<String, OrderParameters>,
        targets: &BTreeMap<String, TargetPosition>,
    ) -> Result<OrderStrategyView> {
        validate_strategy_name(strategy_name).map_err(anyhow::Error::msg)?;
        order_parameters.validate().map_err(anyhow::Error::msg)?;
        for (symbol, selected) in symbol_order_parameters {
            validate_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
            selected.validate().map_err(anyhow::Error::msg)?;
            if selected.algorithm.family() != order_parameters.algorithm.family() {
                bail!(
                    "symbol {symbol} selects {} but the binding uses {}; symbol overrides must stay in one execution family",
                    selected.algorithm.as_str(),
                    order_parameters.algorithm.as_str()
                );
            }
        }
        if source.venue != "binance-futures" && source.venue != "okex-futures" {
            bail!(
                "source {} venue must be binance-futures or okex-futures",
                source.id
            );
        }
        if matches!(
            strategy_name,
            "strategy_names" | "removed_strategy_names" | POSITION_CLOSE_STRATEGY_NAME
        ) {
            bail!("strategy_name is reserved");
        }

        let family = order_parameters.algorithm.family();
        let prefix = format!(
            "{}:{}:{}:",
            source.id,
            source.venue,
            family.redis_namespace()
        );
        let config_key = format!("{prefix}{strategy_name}");
        let index_key = format!("{prefix}strategy_names");
        let removed_key = format!("{prefix}removed_strategy_names");
        let timeout = Duration::from_secs(self.request_timeout_secs().await);

        let stored = {
            let mut inner = self.inner.lock().await;
            let connection = inner.connection().await?;
            tokio::time::timeout(timeout, async {
                let removed = decode_strategy_names(
                    connection.get::<_, Option<String>>(&removed_key).await?,
                    "removed strategy index",
                )?;
                if removed.iter().any(|name| name == strategy_name) {
                    bail!("strategy removal already requested: {strategy_name}");
                }

                let mut names = decode_strategy_names(
                    connection.get::<_, Option<String>>(&index_key).await?,
                    "strategy index",
                )?;
                let current =
                    load_stored_config(connection.get::<_, Option<String>>(&config_key).await?)?;
                let current_version = current
                    .as_ref()
                    .and_then(|payload| payload.get("updated_at_us"))
                    .and_then(serde_json::Value::as_i64);
                let updated_at_us = next_updated_at_us(current_version);
                let opposite = family.opposite();
                let opposite_prefix = format!(
                    "{}:{}:{}:",
                    source.id,
                    source.venue,
                    opposite.redis_namespace()
                );
                let opposite_names = decode_strategy_names(
                    connection
                        .get::<_, Option<String>>(format!("{opposite_prefix}strategy_names"))
                        .await?,
                    "opposite execution-family strategy index",
                )?;
                let switching = opposite_names.iter().any(|name| name == strategy_name);
                let switch_index_key =
                    format!("{}:{}:exec_switch:strategy_names", source.id, source.venue);
                let switch_key = format!(
                    "{}:{}:exec_switch:{}",
                    source.id, source.venue, strategy_name
                );
                let mut switch_names = decode_strategy_names(
                    connection
                        .get::<_, Option<String>>(&switch_index_key)
                        .await?,
                    "Exec algorithm switch index",
                )?;
                let existing_switch = connection.get::<_, Option<String>>(&switch_key).await?;
                let reusable_completed_switch = if let Some(raw) = existing_switch.as_deref() {
                    let existing: StoredExecAlgorithmSwitch = serde_json::from_str(&raw)
                        .context("Exec algorithm switch is invalid JSON")?;
                    let expected_from = opposite.redis_namespace();
                    let expected_to = family.redis_namespace();
                    if existing.state != "completed"
                        && (existing.from_family != expected_from
                            || existing.to_family != expected_to)
                    {
                        bail!(
                            "algorithm switch already in progress for {strategy_name}: {} -> {} ({})",
                            existing.from_family,
                            existing.to_family,
                            existing.state
                        );
                    }
                    existing.state == "completed"
                } else {
                    false
                };
                if switching && names.iter().any(|name| name == strategy_name) {
                    bail!(
                        "strategy {strategy_name} is active in both execution families; resolve the duplicate ownership before switching"
                    );
                }
                ensure_no_opposite_family_claims(
                    connection,
                    source,
                    family,
                    targets,
                    switching.then_some(strategy_name),
                )
                .await?;
                let symbol_overrides = symbol_order_parameters
                    .iter()
                    .filter_map(|(symbol, selected)| match family {
                        ExecutionFamily::BatchExec => {
                            let value =
                                OrderParameterOverrides::from_templates(order_parameters, selected);
                            (!value.is_empty()).then_some((symbol.clone(), value))
                        }
                        ExecutionFamily::ChaseExec => None,
                    })
                    .collect::<BTreeMap<_, _>>();
                let chase_symbol_overrides = symbol_order_parameters
                    .iter()
                    .filter_map(|(symbol, selected)| match family {
                        ExecutionFamily::BatchExec => None,
                        ExecutionFamily::ChaseExec => {
                            let value = ChaseParameterOverrides::from_templates(
                                &order_parameters.chase,
                                &selected.chase,
                            );
                            (!value.is_empty()).then_some((symbol.clone(), value))
                        }
                    })
                    .collect::<BTreeMap<_, _>>();
                let encoded = match family {
                    ExecutionFamily::BatchExec => serde_json::to_string(&StoredBatchExecConfig {
                        algorithm: order_parameters.algorithm,
                        pov: &order_parameters.pov,
                        single_order_usdt: order_parameters.single_order_usdt,
                        orders_per_batch: order_parameters.orders_per_batch,
                        max_batch: order_parameters.max_batch,
                        maker_price_anchor: &order_parameters.maker_price_anchor,
                        tick_spacing: order_parameters.tick_spacing,
                        batch_interval_ms: order_parameters.batch_interval_ms,
                        maker_timeout_ms: order_parameters.maker_timeout_ms,
                        max_maker_requotes: order_parameters.max_maker_requotes,
                        target_tolerance_usdt: order_parameters.target_tolerance_usdt,
                        symbol_overrides: &symbol_overrides,
                        targets,
                        updated_at_us,
                    }),
                    ExecutionFamily::ChaseExec => serde_json::to_string(&StoredChaseExecConfig {
                        single_order_usdt: order_parameters.chase.single_order_usdt,
                        max_open_usdt: order_parameters.chase.max_open_usdt,
                        maker_recenter_trigger_bps: order_parameters
                            .chase
                            .maker_recenter_trigger_bps,
                        maker_amend_cooldown_ms: order_parameters.chase.maker_amend_cooldown_ms,
                        maker_timeout_sec: order_parameters.chase.maker_timeout_sec,
                        target_tolerance_usdt: order_parameters.chase.target_tolerance_usdt,
                        symbol_overrides: &chase_symbol_overrides,
                        targets,
                        updated_at_us,
                    }),
                }
                .context("failed to encode Exec Redis payload")?;
                let expected: serde_json::Value = serde_json::from_str(&encoded)
                    .context("failed to decode encoded Exec Redis payload")?;

                let mut pipe = redis::pipe();
                pipe.atomic();
                pipe.set(&config_key, &encoded);
                if switching && (existing_switch.is_none() || reusable_completed_switch) {
                    if !switch_names.iter().any(|name| name == strategy_name) {
                        switch_names.push(strategy_name.to_string());
                        switch_names.sort();
                    }
                    let request = StoredExecAlgorithmSwitch {
                        from_family: opposite.redis_namespace().to_string(),
                        to_family: family.redis_namespace().to_string(),
                        state: "requested".to_string(),
                        requested_at_us: updated_at_us,
                        updated_at_us,
                        positions: BTreeMap::new(),
                    };
                    pipe.set(
                        &switch_index_key,
                        serde_json::to_string(&switch_names)
                            .context("failed to encode Exec algorithm switch index")?,
                    )
                    .set(
                        &switch_key,
                        serde_json::to_string(&request)
                            .context("failed to encode Exec algorithm switch request")?,
                    );
                } else if !switching && !names.iter().any(|name| name == strategy_name) {
                    names.push(strategy_name.to_string());
                    names.sort();
                    names.dedup();
                    pipe.set(
                        &index_key,
                        serde_json::to_string(&names)
                            .context("failed to encode Exec strategy index")?,
                    );
                }
                let _: () = pipe
                    .query_async(connection)
                    .await
                    .context("failed to commit Exec Redis write")?;

                let stored =
                    load_stored_config(connection.get::<_, Option<String>>(&config_key).await?)?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Redis write was not readable after save: {strategy_name}"
                            )
                        })?;
                if stored != expected {
                    bail!("Redis write confirmation mismatched payload: {strategy_name}");
                }
                let confirmed_names = decode_strategy_names(
                    connection.get::<_, Option<String>>(&index_key).await?,
                    "strategy index",
                )?;
                if !switching && !confirmed_names.iter().any(|name| name == strategy_name) {
                    bail!("Redis write confirmation missing strategy index: {strategy_name}");
                }
                if switching {
                    let confirmed_switch: Option<String> = connection.get(&switch_key).await?;
                    if confirmed_switch.is_none() {
                        bail!("Redis write confirmation missing algorithm switch: {strategy_name}");
                    }
                }
                Ok(stored)
            })
            .await
        };

        match stored {
            Ok(Ok(stored)) => {
                let updated_at_us = stored
                    .get("updated_at_us")
                    .and_then(serde_json::Value::as_i64)
                    .filter(|value| *value > 0)
                    .context("Redis write confirmation omitted updated_at_us")?;
                Ok(OrderStrategyView {
                    source_id: source.id.clone(),
                    strategy_name: strategy_name.to_string(),
                    order_parameters: order_parameters.clone(),
                    symbol_overrides: stored
                        .get("symbol_overrides")
                        .and_then(serde_json::Value::as_object)
                        .map(|values| {
                            values
                                .iter()
                                .map(|(symbol, value)| (symbol.clone(), value.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    updated_at_us: Some(updated_at_us),
                    target_count: targets.len(),
                    nonzero_target_count: targets
                        .values()
                        .filter(|target| target.qty.abs() > 0.0)
                        .count(),
                })
            }
            Ok(Err(error)) => {
                if is_redis_transport_error(&error) {
                    self.mark_broken().await;
                }
                Err(error)
            }
            Err(_) => {
                self.mark_broken().await;
                bail!(
                    "Redis request timed out after {}s",
                    self.request_timeout_secs().await
                )
            }
        }
    }

    pub async fn request_strategy_removal(
        &self,
        source: &SourceConfig,
        strategy_name: &str,
        family: ExecutionFamily,
    ) -> Result<()> {
        validate_strategy_name(strategy_name).map_err(anyhow::Error::msg)?;
        let prefix = format!(
            "{}:{}:{}:",
            source.id,
            source.venue,
            family.redis_namespace()
        );
        let index_key = format!("{prefix}strategy_names");
        let removed_key = format!("{prefix}removed_strategy_names");
        let timeout = Duration::from_secs(self.request_timeout_secs().await);
        let removed = {
            let mut inner = self.inner.lock().await;
            let connection = inner.connection().await?;
            tokio::time::timeout(timeout, async {
                let mut names = decode_strategy_names(
                    connection.get::<_, Option<String>>(&index_key).await?,
                    "strategy index",
                )?;
                let mut removed = decode_strategy_names(
                    connection.get::<_, Option<String>>(&removed_key).await?,
                    "removed strategy index",
                )?;
                names.retain(|name| name != strategy_name);
                if !removed.iter().any(|name| name == strategy_name) {
                    removed.push(strategy_name.to_string());
                    removed.sort();
                }
                let mut pipe = redis::pipe();
                pipe.atomic()
                    .set(
                        &index_key,
                        serde_json::to_string(&names)
                            .context("failed to encode Exec strategy index")?,
                    )
                    .set(
                        &removed_key,
                        serde_json::to_string(&removed)
                            .context("failed to encode removed Exec strategy index")?,
                    );
                let _: () = pipe
                    .query_async(connection)
                    .await
                    .context("failed to request Exec strategy removal")?;
                Ok(())
            })
            .await
        };
        match removed {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                if is_redis_transport_error(&error) {
                    self.mark_broken().await;
                }
                Err(error)
            }
            Err(_) => {
                self.mark_broken().await;
                bail!(
                    "Redis request timed out after {}s",
                    self.request_timeout_secs().await
                )
            }
        }
    }

    /// Load both the account aggregate and the per-strategy versions that Exec
    /// must acknowledge in its Viz state.
    pub(crate) async fn load_exec_target_snapshot(
        &self,
        source: &SourceConfig,
    ) -> Result<ExecTargetSnapshot> {
        let timeout = Duration::from_secs(self.request_timeout_secs().await);
        let loaded = {
            let mut inner = self.inner.lock().await;
            let connection = inner.connection().await?;
            tokio::time::timeout(timeout, async {
                let mut snapshot = ExecTargetSnapshot::default();
                for family in [ExecutionFamily::BatchExec, ExecutionFamily::ChaseExec] {
                    let prefix = format!(
                        "{}:{}:{}:",
                        source.id,
                        source.venue,
                        family.redis_namespace()
                    );
                    let index_key = format!("{prefix}strategy_names");
                    let names = decode_strategy_names(
                        connection.get::<_, Option<String>>(&index_key).await?,
                        "strategy index",
                    )?;
                    for name in &names {
                        let config_key = format!("{prefix}{name}");
                        let raw = connection
                            .get::<_, Option<String>>(&config_key)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "{} strategy config missing in Redis: {name}",
                                    family.redis_namespace()
                                )
                            })?;
                        let stored: serde_json::Value =
                            serde_json::from_str(&raw).with_context(|| {
                                format!(
                                    "{} Redis config is not valid JSON: {name}",
                                    family.redis_namespace()
                                )
                            })?;
                        let strategy = decode_exec_strategy_target_state(
                            family.redis_namespace(),
                            name,
                            &stored,
                        )?;
                        for (symbol, qty) in &strategy.targets {
                            *snapshot
                                .aggregate_targets
                                .entry(symbol.clone())
                                .or_insert(0.0) += qty;
                        }
                        if snapshot.strategies.insert(name.clone(), strategy).is_some() {
                            bail!(
                                "Exec strategy is indexed by multiple execution families: {name}"
                            );
                        }
                    }
                }
                Ok(snapshot)
            })
            .await
        };
        match loaded {
            Ok(Ok(snapshot)) => Ok(snapshot),
            Ok(Err(error)) => {
                if is_redis_transport_error(&error) {
                    self.mark_broken().await;
                }
                Err(error)
            }
            Err(_) => {
                self.mark_broken().await;
                bail!(
                    "Redis request timed out after {}s",
                    self.request_timeout_secs().await
                )
            }
        }
    }

    pub async fn publish_market_rules(
        &self,
        source: &SourceConfig,
        snapshot: &MarketRulesSnapshot,
    ) -> Result<()> {
        snapshot.validate()?;
        if source.venue != snapshot.venue {
            bail!(
                "market-rules venue mismatch for source {}: source={} snapshot={}",
                source.id,
                source.venue,
                snapshot.venue
            );
        }

        let key = format!("{}:{}:market_rules", source.id, source.venue);
        let encoded = serde_json::to_string(snapshot).context("encode market-rules snapshot")?;
        let timeout = Duration::from_secs(self.request_timeout_secs().await);
        let stored = {
            let mut inner = self.inner.lock().await;
            let connection = inner.connection().await?;
            tokio::time::timeout(timeout, async {
                connection
                    .set::<_, _, ()>(&key, &encoded)
                    .await
                    .with_context(|| format!("write Redis market-rules key {key}"))?;
                connection
                    .get::<_, Option<String>>(&key)
                    .await
                    .with_context(|| format!("confirm Redis market-rules key {key}"))
            })
            .await
        };

        match stored {
            Ok(Ok(Some(stored))) if stored == encoded => Ok(()),
            Ok(Ok(Some(_))) => bail!("Redis market-rules confirmation mismatched: {key}"),
            Ok(Ok(None)) => bail!("Redis market-rules key missing after write: {key}"),
            Ok(Err(error)) => {
                if is_redis_transport_error(&error) {
                    self.mark_broken().await;
                }
                Err(error)
            }
            Err(_) => {
                self.mark_broken().await;
                bail!(
                    "Redis market-rules request timed out after {}s",
                    self.request_timeout_secs().await
                )
            }
        }
    }

    async fn request_timeout_secs(&self) -> u64 {
        self.inner.lock().await.settings.request_timeout_secs
    }

    async fn mark_broken(&self) {
        let mut inner = self.inner.lock().await;
        inner.connection = None;
    }
}

fn decode_exec_strategy_target_state(
    family: &str,
    strategy_name: &str,
    stored: &serde_json::Value,
) -> Result<ExecStrategyTargetState> {
    let updated_at_us = stored
        .get("updated_at_us")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value > 0)
        .with_context(|| {
            format!("{family} Redis config has invalid updated_at_us: {strategy_name}")
        })?;
    let raw_targets = stored
        .get("targets")
        .and_then(serde_json::Value::as_object)
        .with_context(|| format!("{family} Redis config has invalid targets: {strategy_name}"))?;
    let mut targets = BTreeMap::new();
    for (symbol, target) in raw_targets {
        validate_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
        let qty = target
            .as_f64()
            .or_else(|| target.get("qty").and_then(serde_json::Value::as_f64))
            .filter(|value| value.is_finite())
            .with_context(|| {
                format!(
                    "{family} Redis config has invalid target quantity: {strategy_name}/{symbol}"
                )
            })?;
        targets.insert(symbol.clone(), qty);
    }
    Ok(ExecStrategyTargetState {
        family: family.to_string(),
        updated_at_us,
        targets,
    })
}

async fn ensure_no_opposite_family_claims(
    connection: &mut ConnectionManager,
    source: &SourceConfig,
    family: ExecutionFamily,
    targets: &BTreeMap<String, TargetPosition>,
    allowed_strategy_name: Option<&str>,
) -> Result<()> {
    let mut requested = targets
        .iter()
        .filter(|(_, target)| allowed_strategy_name.is_some() || target.qty != 0.0)
        .map(|(symbol, _)| symbol.clone())
        .collect::<BTreeSet<_>>();

    let opposite = family.opposite();
    let prefix = format!(
        "{}:{}:{}:",
        source.id,
        source.venue,
        opposite.redis_namespace()
    );
    let ledger_key = format!(
        "{}:{}:{}_state:position_allocations",
        source.id,
        source.venue,
        opposite.redis_namespace()
    );
    let ledger = connection
        .get::<_, Option<String>>(&ledger_key)
        .await?
        .map(|raw| {
            serde_json::from_str::<serde_json::Value>(&raw).with_context(|| {
                format!("opposite execution-family ledger is invalid JSON: {ledger_key}")
            })
        })
        .transpose()?;
    if let (Some(strategy_name), Some(ledger)) = (allowed_strategy_name, ledger.as_ref()) {
        let strategy_positions = ledger
            .get("positions")
            .and_then(serde_json::Value::as_object)
            .and_then(|positions| positions.get(strategy_name))
            .and_then(serde_json::Value::as_object);
        if let Some(strategy_positions) = strategy_positions {
            requested.extend(strategy_positions.keys().cloned());
        }
    }
    if requested.is_empty() {
        return Ok(());
    }

    let names = decode_strategy_names(
        connection
            .get::<_, Option<String>>(format!("{prefix}strategy_names"))
            .await?,
        "opposite execution-family strategy index",
    )?;
    for name in names {
        if allowed_strategy_name == Some(name.as_str()) {
            continue;
        }
        let key = format!("{prefix}{name}");
        let raw = connection
            .get::<_, Option<String>>(&key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("indexed Exec strategy config missing: {key}"))?;
        let stored: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("opposite execution-family config is invalid JSON: {key}"))?;
        let claimed = config_target_symbols(&stored).with_context(|| {
            format!("invalid targets in opposite execution-family config: {key}")
        })?;
        if let Some(symbol) = requested
            .iter()
            .find(|symbol| claimed.contains(symbol.as_str()))
        {
            bail!(
                "symbol {symbol} is already claimed by {} strategy {name}; batch/POV and chase cannot share one account-symbol position ledger",
                opposite.redis_namespace()
            );
        }
    }

    if let Some(ledger) = ledger.as_ref() {
        let claimed = ledger_symbols_except(ledger, allowed_strategy_name)
            .with_context(|| format!("invalid opposite execution-family ledger: {ledger_key}"))?;
        if let Some(symbol) = requested
            .iter()
            .find(|symbol| claimed.contains(symbol.as_str()))
        {
            bail!(
                "symbol {symbol} is still present in the {} position ledger; wait for its removal to settle before publishing a {} target",
                opposite.redis_namespace(),
                family.redis_namespace()
            );
        }
    }
    Ok(())
}

fn config_target_symbols(value: &serde_json::Value) -> Result<BTreeSet<&str>> {
    let targets = value
        .get("targets")
        .and_then(serde_json::Value::as_object)
        .context("targets must be an object")?;
    Ok(targets.keys().map(String::as_str).collect())
}

#[cfg(test)]
fn ledger_symbols(value: &serde_json::Value) -> Result<BTreeSet<&str>> {
    ledger_symbols_except(value, None)
}

fn ledger_symbols_except<'a>(
    value: &'a serde_json::Value,
    excluded_strategy_name: Option<&str>,
) -> Result<BTreeSet<&'a str>> {
    let positions = value
        .get("positions")
        .and_then(serde_json::Value::as_object)
        .context("positions must be an object")?;
    let mut symbols = BTreeSet::new();
    for (strategy_name, strategy_positions) in positions {
        if excluded_strategy_name == Some(strategy_name.as_str()) {
            continue;
        }
        let strategy_positions = strategy_positions
            .as_object()
            .context("strategy positions must be an object")?;
        for (symbol, quantity) in strategy_positions {
            let quantity = quantity
                .as_f64()
                .filter(|quantity| quantity.is_finite())
                .context("strategy position quantity must be finite")?;
            if quantity.abs() > 1e-10 {
                symbols.insert(symbol.as_str());
            }
        }
    }
    Ok(symbols)
}

async fn keep_connection_alive(inner: &mut RedisRuntimeInner) -> Result<bool> {
    let url = inner.settings.url.clone();
    let connection = match inner.connection().await {
        Ok(connection) => connection,
        Err(error) => return Err(error),
    };
    match redis::cmd("PING").query_async::<String>(connection).await {
        Ok(_) => Ok(true),
        Err(error) => {
            warn!(url = %url, error = %error, "Manager Redis keepalive failed; reconnecting");
            inner.connection = None;
            Err(error).context("Manager Redis keepalive failed")
        }
    }
}

fn is_redis_transport_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<redis::RedisError>().is_some())
}

impl RedisRuntimeInner {
    async fn connection(&mut self) -> Result<&mut ConnectionManager> {
        if self.connection.is_none() {
            match ConnectionManager::new(self.client.clone()).await {
                Ok(connection) => {
                    info!(url = %self.settings.url, "Manager Redis long connection ready");
                    self.connection = Some(connection);
                    self.last_reconnect_error_at = None;
                }
                Err(error) => {
                    let now = Instant::now();
                    let should_log = self.last_reconnect_error_at.is_none_or(|previous| {
                        now.duration_since(previous)
                            >= Duration::from_millis(self.settings.reconnect_interval_ms)
                    });
                    if should_log {
                        warn!(
                            url = %self.settings.url,
                            error = %error,
                            "Manager Redis reconnect failed; will retry on next publish"
                        );
                        self.last_reconnect_error_at = Some(now);
                    }
                    return Err(error).context("failed to open Manager Redis long connection");
                }
            }
        }
        self.connection
            .as_mut()
            .context("Manager Redis connection missing after reconnect")
    }
}

fn next_updated_at_us(current: Option<i64>) -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(1);
    match current.filter(|value| *value > 0) {
        Some(current) => now.max(current.saturating_add(1)),
        None => now.max(1),
    }
}

fn decode_strategy_names(raw: Option<String>, label: &str) -> Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let names: Vec<String> =
        serde_json::from_str(&raw).with_context(|| format!("{label} is not valid JSON"))?;
    let mut seen = BTreeMap::new();
    for name in names {
        validate_strategy_name(&name).map_err(anyhow::Error::msg)?;
        if seen.insert(name.clone(), ()).is_some() {
            bail!("{label} contains duplicate names");
        }
    }
    Ok(seen.into_keys().collect())
}

fn load_stored_config(raw: Option<String>) -> Result<Option<serde_json::Value>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    serde_json::from_str(&raw).context("Redis value is not valid JSON")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn valid_parameters() -> OrderParameters {
        OrderParameters {
            single_order_usdt: 100.0,
            orders_per_batch: 3,
            max_batch: 20,
            maker_price_anchor: "own_best".to_string(),
            tick_spacing: 1,
            batch_interval_ms: 500,
            maker_timeout_ms: 1_000,
            max_maker_requotes: 2,
            target_tolerance_usdt: 10.0,
            ..OrderParameters::default()
        }
    }

    #[test]
    fn next_version_never_goes_backwards() {
        assert!(next_updated_at_us(None) > 0);
        assert_eq!(next_updated_at_us(Some(i64::MAX)), i64::MAX);
        let current = next_updated_at_us(None);
        assert!(next_updated_at_us(Some(current)) > current);
    }

    #[test]
    fn decode_strategy_names_sorts_and_rejects_duplicates() {
        let names =
            decode_strategy_names(Some(r#"["CTA_B","CTA_A"]"#.to_string()), "strategy index")
                .unwrap();
        assert_eq!(names, vec!["CTA_A".to_string(), "CTA_B".to_string()]);
        assert!(
            decode_strategy_names(Some(r#"["CTA_A","CTA_A"]"#.to_string()), "strategy index")
                .is_err()
        );
    }

    #[test]
    fn redis_payload_omits_empty_overrides_and_preserves_symbol_overrides() {
        let parameters = valid_parameters();
        let targets = BTreeMap::new();
        let empty = BTreeMap::new();
        let without_overrides = serde_json::to_value(StoredBatchExecConfig {
            algorithm: parameters.algorithm,
            pov: &parameters.pov,
            single_order_usdt: parameters.single_order_usdt,
            orders_per_batch: parameters.orders_per_batch,
            max_batch: parameters.max_batch,
            maker_price_anchor: &parameters.maker_price_anchor,
            tick_spacing: parameters.tick_spacing,
            batch_interval_ms: parameters.batch_interval_ms,
            maker_timeout_ms: parameters.maker_timeout_ms,
            max_maker_requotes: parameters.max_maker_requotes,
            target_tolerance_usdt: parameters.target_tolerance_usdt,
            symbol_overrides: &empty,
            targets: &targets,
            updated_at_us: 1,
        })
        .unwrap();
        assert!(without_overrides.get("symbol_overrides").is_none());

        let overrides = BTreeMap::from([(
            "BTCUSDT".to_string(),
            OrderParameterOverrides {
                single_order_usdt: Some(250.0),
                ..Default::default()
            },
        )]);
        let with_overrides = serde_json::to_value(StoredBatchExecConfig {
            algorithm: parameters.algorithm,
            pov: &parameters.pov,
            single_order_usdt: parameters.single_order_usdt,
            orders_per_batch: parameters.orders_per_batch,
            max_batch: parameters.max_batch,
            maker_price_anchor: &parameters.maker_price_anchor,
            tick_spacing: parameters.tick_spacing,
            batch_interval_ms: parameters.batch_interval_ms,
            maker_timeout_ms: parameters.maker_timeout_ms,
            max_maker_requotes: parameters.max_maker_requotes,
            target_tolerance_usdt: parameters.target_tolerance_usdt,
            symbol_overrides: &overrides,
            targets: &targets,
            updated_at_us: 1,
        })
        .unwrap();
        assert_eq!(
            with_overrides["symbol_overrides"]["BTCUSDT"]["single_order_usdt"],
            250.0
        );
    }

    #[test]
    fn chase_payload_matches_exec_contract() {
        let mut parameters = valid_parameters();
        parameters.algorithm = ExecutionAlgorithm::Chase;
        parameters.chase.max_open_usdt = 500.0;
        let targets = BTreeMap::from([(
            "BTCUSDT".to_string(),
            TargetPosition {
                qty: 0.1,
                signal: 0,
            },
        )]);
        let overrides = BTreeMap::from([(
            "BTCUSDT".to_string(),
            ChaseParameterOverrides {
                maker_recenter_trigger_bps: Some(0.0),
                ..Default::default()
            },
        )]);
        let payload = serde_json::to_value(StoredChaseExecConfig {
            single_order_usdt: parameters.chase.single_order_usdt,
            max_open_usdt: parameters.chase.max_open_usdt,
            maker_recenter_trigger_bps: parameters.chase.maker_recenter_trigger_bps,
            maker_amend_cooldown_ms: parameters.chase.maker_amend_cooldown_ms,
            maker_timeout_sec: parameters.chase.maker_timeout_sec,
            target_tolerance_usdt: parameters.chase.target_tolerance_usdt,
            symbol_overrides: &overrides,
            targets: &targets,
            updated_at_us: 1,
        })
        .unwrap();
        assert_eq!(payload["max_open_usdt"], 500.0);
        assert_eq!(
            payload["symbol_overrides"]["BTCUSDT"]["maker_recenter_trigger_bps"],
            0.0
        );
        assert!(payload.get("algorithm").is_none());
        assert!(payload.get("maker_price_anchor").is_none());
    }

    #[test]
    fn monitor_target_state_decodes_structured_and_legacy_targets() {
        let stored = serde_json::json!({
            "updated_at_us": 123_456,
            "targets": {
                "BTCUSDT": {"qty": 0.25, "signal": 1},
                "ETHUSDT": -2.0
            }
        });
        let decoded = decode_exec_strategy_target_state("batch_exec", "cta_a", &stored).unwrap();
        assert_eq!(decoded.family, "batch_exec");
        assert_eq!(decoded.updated_at_us, 123_456);
        assert_eq!(decoded.targets["BTCUSDT"], 0.25);
        assert_eq!(decoded.targets["ETHUSDT"], -2.0);
    }

    #[test]
    fn monitor_target_state_requires_a_publish_version() {
        let stored = serde_json::json!({
            "targets": {"BTCUSDT": {"qty": 0.25, "signal": 0}}
        });
        let error = decode_exec_strategy_target_state("batch_exec", "cta_a", &stored)
            .expect_err("missing updated_at_us must make application verification fail");
        assert!(error.to_string().contains("updated_at_us"));
    }

    #[test]
    fn extracts_cross_family_symbol_claims_from_config_and_ledger() {
        let config = serde_json::json!({
            "targets": {
                "BTCUSDT": {"qty": 0.0, "signal": 0},
                "ETHUSDT": {"qty": 1.0, "signal": 0}
            }
        });
        assert_eq!(
            config_target_symbols(&config).unwrap(),
            BTreeSet::from(["BTCUSDT", "ETHUSDT"])
        );

        let ledger = serde_json::json!({
            "positions": {
                "alpha": {"BTCUSDT": 0.0},
                "beta": {"SOLUSDT": 2.0}
            }
        });
        assert_eq!(
            ledger_symbols(&ledger).unwrap(),
            BTreeSet::from(["SOLUSDT"])
        );
    }
}
