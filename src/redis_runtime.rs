use std::collections::BTreeMap;
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
const EXEC_ORDER_RATE_LIMIT_PER_MIN_FIELD: &str = "exec_order_rate_limit_per_min";
const EXEC_ORDER_RATE_LIMIT_10S_FIELD: &str = "exec_order_rate_limit_10s";

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
    batch_floor_usdt: f64,
    max_batch: u32,
    max_open_batches: u32,
    maker_recenter_trigger_bps: f64,
    maker_amend_cooldown_ms: u32,
    maker_timeout_sec: u32,
    target_tolerance_usdt: f64,
    strategy_order_rate_limit_per_min: u32,
    strategy_order_rate_limit_10s: u32,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecOrderRateLimits {
    pub source_id: String,
    pub exec_order_rate_limit_per_min: i32,
    pub exec_order_rate_limit_10s: i32,
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

        let runtime_targets =
            filter_runtime_target_signals(order_parameters, symbol_order_parameters, targets);
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
                        targets: &runtime_targets,
                        updated_at_us,
                    }),
                    ExecutionFamily::ChaseExec => serde_json::to_string(&StoredChaseExecConfig {
                        batch_floor_usdt: order_parameters.chase.batch_floor_usdt,
                        max_batch: order_parameters.chase.max_batch,
                        max_open_batches: order_parameters.chase.max_open_batches,
                        maker_recenter_trigger_bps: order_parameters
                            .chase
                            .maker_recenter_trigger_bps,
                        maker_amend_cooldown_ms: order_parameters.chase.maker_amend_cooldown_ms,
                        maker_timeout_sec: order_parameters.chase.maker_timeout_sec,
                        target_tolerance_usdt: order_parameters.chase.target_tolerance_usdt,
                        strategy_order_rate_limit_per_min: order_parameters
                            .chase
                            .strategy_order_rate_limit_per_min,
                        strategy_order_rate_limit_10s: order_parameters
                            .chase
                            .strategy_order_rate_limit_10s,
                        symbol_overrides: &chase_symbol_overrides,
                        targets: &runtime_targets,
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

    pub async fn load_exec_order_rate_limits(
        &self,
        source: &SourceConfig,
    ) -> Result<ExecOrderRateLimits> {
        self.exec_order_rate_limits(source, None).await
    }

    pub async fn save_exec_order_rate_limits(
        &self,
        source: &SourceConfig,
        limit_per_min: i32,
        limit_10s: i32,
    ) -> Result<ExecOrderRateLimits> {
        if limit_per_min < 0 || limit_10s < 0 {
            bail!("Exec order rate limits must be non-negative");
        }
        self.exec_order_rate_limits(source, Some((limit_per_min, limit_10s)))
            .await
    }

    async fn exec_order_rate_limits(
        &self,
        source: &SourceConfig,
        update: Option<(i32, i32)>,
    ) -> Result<ExecOrderRateLimits> {
        let key = exec_risk_params_key(source);
        let timeout = Duration::from_secs(self.request_timeout_secs().await);
        let loaded = {
            let mut inner = self.inner.lock().await;
            let connection = inner.connection().await?;
            tokio::time::timeout(timeout, async {
                let exists = connection
                    .exists::<_, bool>(&key)
                    .await
                    .with_context(|| format!("check Redis Exec risk params key {key}"))?;
                if !exists {
                    bail!("Redis Exec risk params hash does not exist: {key}");
                }
                if let Some((limit_per_min, limit_10s)) = update {
                    let _: i64 = redis::cmd("HSET")
                        .arg(&key)
                        .arg(EXEC_ORDER_RATE_LIMIT_PER_MIN_FIELD)
                        .arg(limit_per_min)
                        .arg(EXEC_ORDER_RATE_LIMIT_10S_FIELD)
                        .arg(limit_10s)
                        .query_async(connection)
                        .await
                        .with_context(|| format!("write Redis Exec order rate limits key {key}"))?;
                }
                let values: Vec<Option<String>> = redis::cmd("HMGET")
                    .arg(&key)
                    .arg(EXEC_ORDER_RATE_LIMIT_PER_MIN_FIELD)
                    .arg(EXEC_ORDER_RATE_LIMIT_10S_FIELD)
                    .query_async(connection)
                    .await
                    .with_context(|| format!("read Redis Exec order rate limits key {key}"))?;
                if values.len() != 2 {
                    bail!("Redis Exec order rate limit response has invalid field count");
                }
                Ok(ExecOrderRateLimits {
                    source_id: source.id.clone(),
                    exec_order_rate_limit_per_min: parse_exec_order_rate_limit(
                        values[0].as_deref(),
                        EXEC_ORDER_RATE_LIMIT_PER_MIN_FIELD,
                    )?,
                    exec_order_rate_limit_10s: parse_exec_order_rate_limit(
                        values[1].as_deref(),
                        EXEC_ORDER_RATE_LIMIT_10S_FIELD,
                    )?,
                })
            })
            .await
        };
        match loaded {
            Ok(Ok(limits)) => Ok(limits),
            Ok(Err(error)) => {
                if is_redis_transport_error(&error) {
                    self.mark_broken().await;
                }
                Err(error)
            }
            Err(_) => {
                self.mark_broken().await;
                bail!(
                    "Redis Exec order rate limit request timed out after {}s",
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

fn exec_risk_params_key(source: &SourceConfig) -> String {
    format!("{}:{}:pre_trade_risk_params", source.id, source.venue)
}

fn parse_exec_order_rate_limit(raw: Option<&str>, field: &str) -> Result<i32> {
    let Some(raw) = raw else {
        return Ok(0);
    };
    let value = raw
        .parse::<i64>()
        .with_context(|| format!("Redis {field} is not an integer"))?;
    i32::try_from(value)
        .ok()
        .filter(|value| *value >= 0)
        .with_context(|| format!("Redis {field} must be in 0..={}", i32::MAX))
}

fn filter_runtime_target_signals(
    order_parameters: &OrderParameters,
    symbol_order_parameters: &BTreeMap<String, OrderParameters>,
    targets: &BTreeMap<String, TargetPosition>,
) -> BTreeMap<String, TargetPosition> {
    targets
        .iter()
        .map(|(symbol, target)| {
            let signal_execution_enabled = symbol_order_parameters
                .get(symbol)
                .unwrap_or(order_parameters)
                .signal_execution_enabled;
            let target = if signal_execution_enabled {
                *target
            } else {
                TargetPosition {
                    qty: target.qty,
                    signal: 0,
                }
            };
            (symbol.clone(), target)
        })
        .collect()
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
    fn runtime_target_signal_filter_uses_effective_symbol_order_strategy() {
        let mut defaults = valid_parameters();
        defaults.signal_execution_enabled = false;
        let mut passthrough = defaults.clone();
        passthrough.signal_execution_enabled = true;
        let symbol_order_parameters = BTreeMap::from([
            ("ETHUSDT".to_string(), passthrough),
            ("SOLUSDT".to_string(), defaults.clone()),
        ]);
        let targets = BTreeMap::from([
            (
                "BTCUSDT".to_string(),
                TargetPosition {
                    qty: 0.1,
                    signal: 1,
                },
            ),
            (
                "ETHUSDT".to_string(),
                TargetPosition {
                    qty: -2.0,
                    signal: -1,
                },
            ),
            (
                "SOLUSDT".to_string(),
                TargetPosition {
                    qty: 3.0,
                    signal: 2,
                },
            ),
        ]);

        let filtered = filter_runtime_target_signals(&defaults, &symbol_order_parameters, &targets);

        assert_eq!(filtered["BTCUSDT"].qty, 0.1);
        assert_eq!(filtered["BTCUSDT"].signal, 0);
        assert_eq!(filtered["ETHUSDT"].signal, -1);
        assert_eq!(filtered["SOLUSDT"].signal, 0);
        assert_eq!(targets["BTCUSDT"].signal, 1);
    }

    #[test]
    fn exec_order_rate_limit_contract_uses_account_risk_hash() {
        let source = SourceConfig {
            id: "binance_exec_trade01".to_string(),
            account: "trade01".to_string(),
            alias: None,
            venue: "binance-futures".to_string(),
            rocksdb_path: "/tmp/orders".into(),
            enabled: true,
            monitor_enabled: true,
            start_ts_us: None,
            poll_interval_secs: None,
            estimated_fee_rate: Some(0.0),
            maker_fee_rate: Some(0.0),
            taker_fee_rate: Some(0.0),
            gateway_prefix: None,
            exec_config_url: None,
            exec_viz_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            legacy_share_unit_usdt: None,
            env_path: None,
        };
        assert_eq!(
            exec_risk_params_key(&source),
            "binance_exec_trade01:binance-futures:pre_trade_risk_params"
        );
        assert_eq!(parse_exec_order_rate_limit(None, "limit").unwrap(), 0);
        assert_eq!(
            parse_exec_order_rate_limit(Some("400"), "limit").unwrap(),
            400
        );
        assert!(parse_exec_order_rate_limit(Some("-1"), "limit").is_err());
        assert!(parse_exec_order_rate_limit(Some("bad"), "limit").is_err());
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
        assert!(without_overrides.get("signal_execution_enabled").is_none());

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
        parameters.chase.max_batch = 8;
        parameters.chase.max_open_batches = 3;
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
            batch_floor_usdt: parameters.chase.batch_floor_usdt,
            max_batch: parameters.chase.max_batch,
            max_open_batches: parameters.chase.max_open_batches,
            maker_recenter_trigger_bps: parameters.chase.maker_recenter_trigger_bps,
            maker_amend_cooldown_ms: parameters.chase.maker_amend_cooldown_ms,
            maker_timeout_sec: parameters.chase.maker_timeout_sec,
            target_tolerance_usdt: parameters.chase.target_tolerance_usdt,
            strategy_order_rate_limit_per_min: parameters.chase.strategy_order_rate_limit_per_min,
            strategy_order_rate_limit_10s: parameters.chase.strategy_order_rate_limit_10s,
            symbol_overrides: &overrides,
            targets: &targets,
            updated_at_us: 1,
        })
        .unwrap();
        assert_eq!(payload["max_batch"], 8);
        assert_eq!(payload["max_open_batches"], 3);
        assert_eq!(payload["strategy_order_rate_limit_per_min"], 0);
        assert_eq!(payload["strategy_order_rate_limit_10s"], 0);
        assert_eq!(
            payload["symbol_overrides"]["BTCUSDT"]["maker_recenter_trigger_bps"],
            0.0
        );
        assert!(payload.get("algorithm").is_none());
        assert!(payload.get("maker_price_anchor").is_none());
        assert!(payload.get("signal_execution_enabled").is_none());
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
}
