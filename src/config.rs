use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    #[serde(default)]
    pub ingestion: IngestionConfig,
    #[serde(default)]
    pub order_config: OrderConfigSettings,
    #[serde(default)]
    pub redis: RedisSettings,
    #[serde(default)]
    pub twap: TwapConfig,
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    #[serde(default = "default_database_url_env")]
    pub url_env: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestionConfig {
    pub poll_interval_secs: u64,
    pub safety_lag_secs: u64,
    pub overlap_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrderConfigSettings {
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedisSettings {
    /// Loopback Redis used as the Exec runtime store. Defaults to 127.0.0.1:6379/0.
    pub url: String,
    pub request_timeout_secs: u64,
    pub reconnect_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TwapConfig {
    pub enabled: bool,
    pub rocksdb_path: PathBuf,
    pub venue: String,
    pub interval_ms: u32,
    pub retain_days: u32,
    pub catalog_reload_secs: u64,
    pub compact_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// Stable, globally unique deployment identity, such as binance_exec_trade01.
    pub id: String,
    pub account: String,
    /// Optional Manager display name. source_id remains the identity.
    #[serde(default)]
    pub alias: Option<String>,
    pub venue: String,
    pub rocksdb_path: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// First RocksDB key to ingest when this source has no checkpoint. Defaults to all history.
    pub start_ts_us: Option<i64>,
    pub poll_interval_secs: Option<u64>,
    /// Effective fee rate used to estimate fees from fill notional, for example 0.0004.
    pub estimated_fee_rate: Option<f64>,
    /// Same-origin gateway path for this account's Exec Viz, for example /exec_trade01.
    pub gateway_prefix: Option<String>,
    /// Loopback-only Exec Config service used by the Manager backend.
    pub exec_config_url: Option<String>,
    /// Loopback-only Exec Viz origin used to read `/snapshot` factual positions.
    pub exec_viz_url: Option<String>,
    /// Iceoryx namespace used by this Exec's account_monitor. Defaults to source id.
    #[serde(default)]
    pub ipc_namespace: Option<String>,
    /// Iceoryx service path after the namespace, for example account_pubs/binance_pm.
    #[serde(default)]
    pub account_ipc_service: Option<String>,
    /// One CTA share equals this many USDT of reference equity.
    #[serde(default)]
    pub share_unit_usdt: Option<f64>,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 60,
            safety_lag_secs: 5,
            overlap_secs: 300,
        }
    }
}

impl Default for OrderConfigSettings {
    fn default() -> Self {
        Self {
            request_timeout_secs: 5,
        }
    }
}

impl Default for RedisSettings {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379/0".to_string(),
            request_timeout_secs: 2,
            reconnect_interval_ms: 500,
        }
    }
}

impl Default for TwapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rocksdb_path: PathBuf::from("/home/el01/crypto_cta_manager/db"),
            venue: "binance-futures".to_string(),
            interval_ms: 5_000,
            retain_days: 30,
            catalog_reload_secs: 30,
            compact_interval_secs: 3_600,
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.database.url_env.trim().is_empty() {
            bail!("database.url_env must not be empty");
        }
        if self.database.max_connections == 0 {
            bail!("database.max_connections must be greater than zero");
        }
        if self.ingestion.poll_interval_secs == 0 {
            bail!("ingestion.poll_interval_secs must be greater than zero");
        }
        if self.order_config.request_timeout_secs == 0 {
            bail!("order_config.request_timeout_secs must be greater than zero");
        }
        if self.redis.request_timeout_secs == 0 {
            bail!("redis.request_timeout_secs must be greater than zero");
        }
        if self.redis.reconnect_interval_ms == 0 {
            bail!("redis.reconnect_interval_ms must be greater than zero");
        }
        validate_loopback_redis_url(&self.redis.url)?;
        if !self.twap.rocksdb_path.is_absolute() {
            bail!(
                "twap.rocksdb_path must be absolute: {}",
                self.twap.rocksdb_path.display()
            );
        }
        for source in &self.sources {
            if source.enabled && source.rocksdb_path == self.twap.rocksdb_path {
                bail!(
                    "twap.rocksdb_path must not reuse an Exec persist_manager path: {}",
                    self.twap.rocksdb_path.display()
                );
            }
        }
        if self.twap.enabled {
            if self.twap.venue.trim().is_empty() {
                bail!("twap.venue must not be empty");
            }
            if self.twap.interval_ms == 0 {
                bail!("twap.interval_ms must be greater than zero");
            }
            if self.twap.retain_days == 0 {
                bail!("twap.retain_days must be greater than zero");
            }
            if self.twap.catalog_reload_secs == 0 {
                bail!("twap.catalog_reload_secs must be greater than zero");
            }
            if self.twap.compact_interval_secs == 0 {
                bail!("twap.compact_interval_secs must be greater than zero");
            }
        }
        if self.sources.is_empty() {
            bail!("at least one [[sources]] entry is required");
        }

        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        let mut gateway_prefixes = HashSet::new();
        let mut enabled = 0usize;
        for source in &self.sources {
            validate_source_id(&source.id)?;
            if source.account.trim().is_empty() {
                bail!("source {} has an empty account", source.id);
            }
            if source
                .alias
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                bail!("source {} alias must not be empty when set", source.id);
            }
            if source.venue.trim().is_empty() {
                bail!("source {} has an empty venue", source.id);
            }
            if !source.rocksdb_path.is_absolute() {
                bail!(
                    "source {} rocksdb_path must be absolute: {}",
                    source.id,
                    source.rocksdb_path.display()
                );
            }
            if source.start_ts_us.is_some_and(|value| value < 0) {
                bail!("source {} start_ts_us must not be negative", source.id);
            }
            if source.poll_interval_secs == Some(0) {
                bail!(
                    "source {} poll_interval_secs must be greater than zero",
                    source.id
                );
            }
            if source
                .estimated_fee_rate
                .is_some_and(|value| !value.is_finite() || value < 0.0)
            {
                bail!(
                    "source {} estimated_fee_rate must be finite and nonnegative",
                    source.id
                );
            }
            if let Some(gateway_prefix) = &source.gateway_prefix {
                validate_gateway_prefix(&source.id, gateway_prefix)?;
                if !gateway_prefixes.insert(gateway_prefix.clone()) {
                    bail!("duplicate source gateway_prefix: {gateway_prefix}");
                }
            }
            if let Some(exec_config_url) = &source.exec_config_url {
                validate_loopback_http_origin(&source.id, "exec_config_url", exec_config_url)?;
            }
            if let Some(exec_viz_url) = &source.exec_viz_url {
                validate_loopback_http_origin(&source.id, "exec_viz_url", exec_viz_url)?;
            }
            if source
                .share_unit_usdt
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
            {
                bail!(
                    "source {} share_unit_usdt must be finite and greater than zero",
                    source.id
                );
            }
            if !ids.insert(source.id.clone()) {
                bail!("duplicate source id: {}", source.id);
            }
            if source.enabled && !paths.insert(source.rocksdb_path.clone()) {
                bail!(
                    "enabled sources must not share rocksdb_path: {}",
                    source.rocksdb_path.display()
                );
            }
            enabled += usize::from(source.enabled);
        }
        if enabled == 0 {
            // A host may reserve sources for later Exec accounts and still run
            // Manager catalog/publish against an empty NAV set.
        }
        Ok(())
    }

    pub fn database_url(&self) -> Result<String> {
        let env_name = self.database.url_env.trim();
        std::env::var(env_name)
            .with_context(|| format!("database URL environment variable {env_name} is not set"))
    }
}

impl SourceConfig {
    pub fn poll_interval_secs(&self, defaults: &IngestionConfig) -> u64 {
        self.poll_interval_secs
            .unwrap_or(defaults.poll_interval_secs)
    }

    pub fn nav_fee_rate(&self) -> Result<f64> {
        self.estimated_fee_rate.with_context(|| {
            format!(
                "source {} requires estimated_fee_rate for NAV reconstruction",
                self.id
            )
        })
    }

    pub fn share_unit_usdt(&self) -> f64 {
        self.share_unit_usdt
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(10_000.0)
    }

    pub fn display_name(&self) -> &str {
        self.alias
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.account.as_str())
    }

    pub fn account_ipc_service_name(&self) -> Option<String> {
        let namespace = self
            .ipc_namespace
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.id.as_str());
        if namespace.is_empty() {
            return None;
        }
        let service = self
            .account_ipc_service
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("account_pubs/binance_pm");
        Some(format!("{namespace}/{service}"))
    }

    pub fn reload_notify_service_name(&self) -> Option<String> {
        let namespace = self
            .ipc_namespace
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.id.as_str());
        if namespace.is_empty() {
            return None;
        }
        Some(format!("{namespace}/batch_exec_pubs/reload_notify"))
    }

    pub fn exec_viz_origin(&self) -> Option<&str> {
        self.exec_viz_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

fn validate_source_id(value: &str) -> Result<()> {
    let valid_len = !value.is_empty() && value.len() <= 128;
    let valid_chars = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !valid_len || !valid_chars {
        bail!("source id must contain 1-128 ASCII letters, digits, '_' or '-': {value:?}");
    }
    Ok(())
}

fn validate_gateway_prefix(source_id: &str, value: &str) -> Result<()> {
    let suffix = value.strip_prefix('/').unwrap_or_default();
    let valid_len = !suffix.is_empty() && value.len() <= 128;
    let valid_chars = suffix
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !valid_len || !valid_chars {
        bail!(
            "source {source_id} gateway_prefix must be one absolute path segment containing only ASCII letters, digits, '_' or '-': {value:?}"
        );
    }
    Ok(())
}

fn is_supported_redis_db_path(path: &str) -> bool {
    matches!(
        path,
        "" | "/"
            | "/0"
            | "/1"
            | "/2"
            | "/3"
            | "/4"
            | "/5"
            | "/6"
            | "/7"
            | "/8"
            | "/9"
            | "/10"
            | "/11"
            | "/12"
            | "/13"
            | "/14"
            | "/15"
    )
}

fn validate_loopback_redis_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("redis.url is invalid")?;
    if url.scheme() != "redis"
        || url.query().is_some()
        || url.fragment().is_some()
        || !is_supported_redis_db_path(url.path())
    {
        bail!("redis.url must be a loopback redis:// origin with an optional database index 0-15");
    }
    let host = url.host_str().context("redis.url has no host")?;
    let normalized_host = host.trim_matches(['[', ']']);
    let loopback = normalized_host.eq_ignore_ascii_case("localhost")
        || normalized_host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        bail!("redis.url host must be loopback");
    }
    Ok(())
}

fn validate_loopback_http_origin(source_id: &str, field: &str, value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)
        .with_context(|| format!("source {source_id} has invalid {field}"))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        bail!(
            "source {source_id} {field} must be a loopback HTTP origin without credentials, path, query, or fragment"
        );
    }
    let host = url
        .host_str()
        .with_context(|| format!("source {source_id} {field} has no host"))?;
    let normalized_host = host.trim_matches(['[', ']']);
    let loopback = normalized_host.eq_ignore_ascii_case("localhost")
        || normalized_host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        bail!("source {source_id} {field} host must be loopback");
    }
    Ok(())
}

fn default_database_url_env() -> String {
    "CRYPTO_CTA_LOCAL_DATABASE_URL".to_string()
}

const fn default_max_connections() -> u32 {
    8
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_sources(sources: Vec<SourceConfig>) -> AppConfig {
        AppConfig {
            database: DatabaseConfig {
                url_env: default_database_url_env(),
                max_connections: 4,
            },
            ingestion: IngestionConfig::default(),
            order_config: OrderConfigSettings::default(),
            redis: RedisSettings::default(),
            twap: TwapConfig::default(),
            sources,
        }
    }

    fn source(id: &str, path: &str) -> SourceConfig {
        SourceConfig {
            id: id.to_string(),
            account: id.to_string(),
            alias: None,
            venue: "binance-futures".to_string(),
            rocksdb_path: PathBuf::from(path),
            enabled: true,
            start_ts_us: None,
            poll_interval_secs: None,
            estimated_fee_rate: Some(0.0004),
            gateway_prefix: Some(format!("/{id}")),
            exec_config_url: None,
            exec_viz_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            share_unit_usdt: None,
        }
    }

    #[test]
    fn accepts_multiple_independent_sources() {
        let config = config_with_sources(vec![
            source("binance_exec_trade01", "/srv/trade01/persist_manager"),
            source("binance_exec_trade02", "/srv/trade02/persist_manager"),
        ]);
        config.validate().unwrap();
    }

    #[test]
    fn rejects_duplicate_source_ids() {
        let config = config_with_sources(vec![
            source("binance_exec_trade01", "/srv/trade01/persist_manager"),
            source("binance_exec_trade01", "/srv/trade02/persist_manager"),
        ]);
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn rejects_duplicate_enabled_paths() {
        let config = config_with_sources(vec![
            source("binance_exec_trade01", "/srv/shared/persist_manager"),
            source("binance_exec_trade02", "/srv/shared/persist_manager"),
        ]);
        assert!(config.validate().unwrap_err().to_string().contains("share"));
    }

    #[test]
    fn rejects_invalid_estimated_fee_rate() {
        let mut invalid = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        invalid.estimated_fee_rate = Some(f64::NAN);
        assert!(
            config_with_sources(vec![invalid])
                .validate()
                .unwrap_err()
                .to_string()
                .contains("estimated_fee_rate")
        );
    }

    #[test]
    fn rejects_invalid_or_duplicate_gateway_prefixes() {
        let mut invalid = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        invalid.gateway_prefix = Some("/exec/trade01".to_string());
        assert!(
            config_with_sources(vec![invalid])
                .validate()
                .unwrap_err()
                .to_string()
                .contains("gateway_prefix")
        );

        let mut first = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        let mut second = source("binance_exec_trade02", "/srv/trade02/persist_manager");
        first.gateway_prefix = Some("/exec_trade".to_string());
        second.gateway_prefix = Some("/exec_trade".to_string());
        assert!(
            config_with_sources(vec![first, second])
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate source gateway_prefix")
        );
    }

    #[test]
    fn nav_fee_rate_is_required_only_when_requested() {
        let mut without_rate = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        without_rate.estimated_fee_rate = None;
        config_with_sources(vec![without_rate.clone()])
            .validate()
            .unwrap();
        assert!(without_rate.nav_fee_rate().is_err());
    }

    #[test]
    fn exec_loopback_origins_must_be_http_loopback() {
        let mut valid = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        valid.exec_config_url = Some("http://127.0.0.1:18161/".to_string());
        valid.exec_viz_url = Some("http://127.0.0.1:10041/".to_string());
        config_with_sources(vec![valid]).validate().unwrap();

        for url in [
            "https://127.0.0.1:18161/",
            "http://172.16.30.42:18161/",
            "http://127.0.0.1:18161/api/",
        ] {
            let mut invalid = source("binance_exec_trade01", "/srv/trade01/persist_manager");
            invalid.exec_config_url = Some(url.to_string());
            assert!(config_with_sources(vec![invalid]).validate().is_err());
            let mut invalid_viz = source("binance_exec_trade01", "/srv/trade01/persist_manager");
            invalid_viz.exec_viz_url = Some(url.replace("18161", "10041"));
            assert!(config_with_sources(vec![invalid_viz]).validate().is_err());
        }
    }

    #[test]
    fn redis_url_must_be_loopback() {
        let mut config = config_with_sources(vec![source(
            "binance_exec_trade01",
            "/srv/trade01/persist_manager",
        )]);
        config.validate().unwrap();
        config.redis.url = "redis://172.16.30.42:6379/0".to_string();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("loopback")
        );
        config.redis.url = "http://127.0.0.1:6379/0".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn display_name_prefers_alias() {
        let mut source = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        source.account = "trade01".into();
        assert_eq!(source.display_name(), "trade01");
        source.alias = Some("bahll202210".into());
        assert_eq!(source.display_name(), "bahll202210");
    }

    #[test]
    fn example_config_stays_valid() {
        let config: AppConfig =
            toml::from_str(include_str!("../config/cta-manager.example.toml")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.sources[0].id, "binance_exec_trade01");
        assert_eq!(config.sources[0].display_name(), "trade01");
        assert!(config.twap.enabled);
        assert_eq!(config.twap.interval_ms, 5_000);
        assert_eq!(config.twap.retain_days, 30);
        assert_eq!(config.redis.url, "redis://127.0.0.1:6379/0");
        assert_eq!(
            config.sources[0].reload_notify_service_name().as_deref(),
            Some("binance_exec_trade01/batch_exec_pubs/reload_notify")
        );
        let mut without_namespace = source("binance_exec_trade01", "/srv/trade01/persist_manager");
        without_namespace.ipc_namespace = None;
        assert_eq!(
            without_namespace.reload_notify_service_name().as_deref(),
            Some("binance_exec_trade01/batch_exec_pubs/reload_notify")
        );
    }

    #[test]
    fn jp_meta_config_enables_trade01_and_reserves_later_accounts() {
        let config: AppConfig =
            toml::from_str(include_str!("../deploy/jp_meta/cta-manager.toml")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.sources.len(), 4);
        assert_eq!(config.sources[0].id, "binance_exec_trade01");
        assert_eq!(config.sources[0].display_name(), "trade01");
        assert!(config.sources[0].enabled);
        assert!(config.sources[1..].iter().all(|source| !source.enabled));
        assert_eq!(
            config.twap.rocksdb_path.as_os_str(),
            "/home/ubuntu/crypto_cta_manager/db"
        );
    }

    #[test]
    fn twap_path_must_not_reuse_exec_persist_manager() {
        let mut config = config_with_sources(vec![source(
            "binance_exec_trade01",
            "/srv/trade01/persist_manager",
        )]);
        config.twap.enabled = false;
        config.twap.rocksdb_path = PathBuf::from("/srv/trade01/persist_manager");
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("must not reuse")
        );
    }
}
