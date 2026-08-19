use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::{RedisSettings, SourceConfig};
use crate::order_config::{
    OrderParameters, OrderStrategyView, TargetPosition, validate_strategy_name,
};

const POSITION_CLOSE_STRATEGY_NAME: &str = "SYSTEM_POSITION_CLOSE";

#[derive(Debug, Clone, Serialize)]
struct StoredExecConfig<'a> {
    single_order_usdt: f64,
    orders_per_batch: u32,
    maker_price_anchor: &'a str,
    tick_spacing: u32,
    batch_interval_ms: u32,
    maker_timeout_ms: u32,
    max_maker_requotes: u32,
    target_tolerance_usdt: f64,
    targets: &'a BTreeMap<String, TargetPosition>,
    updated_at_us: i64,
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
        targets: &BTreeMap<String, TargetPosition>,
    ) -> Result<OrderStrategyView> {
        validate_strategy_name(strategy_name).map_err(anyhow::Error::msg)?;
        order_parameters.validate().map_err(anyhow::Error::msg)?;
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

        let prefix = format!("{}:{}:batch_exec:", source.id, source.venue);
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
                let payload = StoredExecConfig {
                    single_order_usdt: order_parameters.single_order_usdt,
                    orders_per_batch: order_parameters.orders_per_batch,
                    maker_price_anchor: &order_parameters.maker_price_anchor,
                    tick_spacing: order_parameters.tick_spacing,
                    batch_interval_ms: order_parameters.batch_interval_ms,
                    maker_timeout_ms: order_parameters.maker_timeout_ms,
                    max_maker_requotes: order_parameters.max_maker_requotes,
                    target_tolerance_usdt: order_parameters.target_tolerance_usdt,
                    targets,
                    updated_at_us,
                };
                let encoded = serde_json::to_string(&payload)
                    .context("failed to encode BatchExec Redis payload")?;
                let expected: serde_json::Value = serde_json::from_str(&encoded)
                    .context("failed to decode encoded BatchExec Redis payload")?;

                let mut pipe = redis::pipe();
                pipe.atomic();
                pipe.set(&config_key, &encoded);
                if !names.iter().any(|name| name == strategy_name) {
                    names.push(strategy_name.to_string());
                    names.sort();
                    names.dedup();
                    pipe.set(
                        &index_key,
                        serde_json::to_string(&names)
                            .context("failed to encode BatchExec strategy index")?,
                    );
                }
                let _: () = pipe
                    .query_async(connection)
                    .await
                    .context("failed to commit BatchExec Redis write")?;

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
                if !confirmed_names.iter().any(|name| name == strategy_name) {
                    bail!("Redis write confirmation missing strategy index: {strategy_name}");
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

    async fn request_timeout_secs(&self) -> u64 {
        self.inner.lock().await.settings.request_timeout_secs
    }

    async fn mark_broken(&self) {
        let mut inner = self.inner.lock().await;
        inner.connection = None;
    }
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
    use super::*;

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
}
