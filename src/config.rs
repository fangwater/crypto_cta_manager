use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeRates {
    pub maker: f64,
    pub taker: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
    #[serde(default)]
    pub order_config: OrderConfigSettings,
    #[serde(default)]
    pub redis: RedisSettings,
    #[serde(default)]
    pub kline: KlineConfig,
    #[serde(default)]
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub treasury: TreasuryConfig,
    pub sources: Vec<SourceConfig>,
}

/// Explicit egress selection for BFUSD and BNB asset management.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TreasuryConfig {
    pub local_ip: Option<IpAddr>,
    /// Explicit operator opt-in to the source account's existing local_ips.
    pub use_account_ip_rotation: bool,
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
pub struct DashboardConfig {
    pub refresh_secs: u64,
    /// CPU computation only; independent of Binance request concurrency/budget.
    pub compute_threads: usize,
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
pub struct KlineConfig {
    pub enabled: bool,
    pub rocksdb_path: PathBuf,
    pub retain_days: u32,
    pub refresh_secs: u64,
    pub compact_interval_secs: u64,
    pub default_symbols: Vec<String>,
    /// The assigned local address selecting the dedicated market-data route.
    pub local_ip: Option<IpAddr>,
    /// Expected public address after NAT. Verified before accessing Binance.
    pub public_ip: Option<IpAddr>,
    /// All live trade engine TOMLs on this host; only their IP fields are read.
    pub trade_engine_configs: Vec<PathBuf>,
    /// Additional public trading addresses when local_ips are behind NAT.
    pub forbidden_public_ips: Vec<IpAddr>,
    pub request_timeout_secs: u64,
    pub concurrency: usize,
    pub weight_per_minute: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonitorConfig {
    /// Enables the standalone cta_monitor process checks.
    pub enabled: bool,
    pub poll_interval_secs: u64,
    /// Repeat an unresolved alert after this interval.
    pub repeat_alert_secs: u64,
    pub market_stale_secs: u64,
    pub order_stale_secs: u64,
    pub position_stale_secs: u64,
    /// Continuous position_ready=false duration before alerting.
    pub position_not_ready_secs: u64,
    /// Grace after the Exec estimated completion time before alerting.
    pub execution_grace_secs: u64,
    /// Maximum recent records read from each Exec RocksDB column family per poll.
    pub recent_order_records: usize,
    pub position_tolerance: f64,
    /// Unfinished or mismatched position value below this USDT amount is
    /// treated as ignorable dust instead of an alert.
    pub position_residual_usdt: f64,
    /// Empty means monitor any symbol seen on the configured venue feed.
    pub market_symbols: Vec<String>,
    /// Host label prepended to every DingTalk alert, such as "[el01]".
    pub host_tag: String,
    /// Hours between market-channel heartbeat pushes aligned to Shanghai
    /// wall-clock boundaries. Zero disables the heartbeat.
    pub market_heartbeat_hours: u64,
    /// Hours between order-channel heartbeat pushes aligned to Shanghai
    /// wall-clock boundaries. Zero disables the heartbeat.
    pub order_heartbeat_hours: u64,
    /// Shanghai-time quiet window suppressing heartbeat pushes,
    /// [heartbeat_quiet_start_hour, heartbeat_quiet_end_hour). A start later
    /// than the end wraps past midnight; equal bounds disable the window.
    /// Fault alerts are never suppressed.
    pub heartbeat_quiet_start_hour: u32,
    pub heartbeat_quiet_end_hour: u32,
    pub dingtalk: DingTalkConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DingTalkConfig {
    /// Environment variable containing the market-data robot webhook URL.
    pub market_webhook_url_env: String,
    /// Environment variable containing the order/position robot webhook URL.
    pub order_webhook_url_env: String,
    /// Optional signing secret environment variables for each robot.
    pub market_secret_env: Option<String>,
    pub order_secret_env: Option<String>,
    pub request_timeout_secs: u64,
    /// Number of attempts per webhook batch, including the first request.
    pub retry_attempts: u32,
    /// Initial delay between failed webhook attempts.
    pub retry_backoff_ms: u64,
    pub at_mobiles: Vec<String>,
    pub is_at_all: bool,
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
    /// Skip Exec health checks and live account IPC until this account's runtime starts.
    #[serde(default = "default_true")]
    pub monitor_enabled: bool,
    /// Effective fee rate used to estimate fees from fill notional, for example 0.0004.
    pub estimated_fee_rate: Option<f64>,
    /// Maker fee fraction. Negative values represent rebates.
    pub maker_fee_rate: Option<f64>,
    /// Taker fee fraction.
    pub taker_fee_rate: Option<f64>,
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
    /// Accepted during rollout for old host TOML files. Direct shares never use it.
    #[serde(default, rename = "share_unit_usdt")]
    pub legacy_share_unit_usdt: Option<f64>,
    /// Optional Exec env.sh used only to read exchange API credentials.
    #[serde(default)]
    pub env_path: Option<PathBuf>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            refresh_secs: 60,
            compute_threads: crate::analysis::default_threads(),
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

impl Default for KlineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rocksdb_path: PathBuf::from("/home/el01/crypto_cta_manager/db"),
            retain_days: 30,
            refresh_secs: 300,
            compact_interval_secs: 3_600,
            default_symbols: ["BNBUSDT", "XRPUSDT", "ETHUSDT", "BTCUSDT", "SOLUSDT"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            local_ip: None,
            public_ip: None,
            trade_engine_configs: Vec::new(),
            forbidden_public_ips: Vec::new(),
            request_timeout_secs: 15,
            concurrency: 8,
            weight_per_minute: 600,
        }
    }
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: 10,
            repeat_alert_secs: 30,
            market_stale_secs: 5,
            order_stale_secs: 120,
            position_stale_secs: 30,
            position_not_ready_secs: 60,
            execution_grace_secs: 30,
            recent_order_records: 2_000,
            position_tolerance: 1e-8,
            position_residual_usdt: 50.0,
            market_symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
                "XRPUSDT".to_string(),
            ],
            host_tag: String::new(),
            market_heartbeat_hours: 3,
            order_heartbeat_hours: 4,
            heartbeat_quiet_start_hour: 0,
            heartbeat_quiet_end_hour: 6,
            dingtalk: DingTalkConfig::default(),
        }
    }
}

impl KlineConfig {
    pub fn validate(&self) -> Result<()> {
        if !(1..=30).contains(&self.retain_days) {
            bail!("kline.retain_days must be between 1 and 30");
        }
        if self.refresh_secs == 0
            || self.compact_interval_secs == 0
            || self.request_timeout_secs == 0
            || !(1..=64).contains(&self.concurrency)
            || !(2..=1200).contains(&self.weight_per_minute)
        {
            bail!("invalid kline refresh, timeout, concurrency or weight budget");
        }
        for symbol in &self.default_symbols {
            if symbol.len() >= 256 || crate::order_config::validate_exec_symbol(symbol).is_err() {
                bail!("kline.default_symbols must contain valid exchange symbols under 256 bytes");
            }
        }
        if self.enabled {
            let local = self
                .local_ip
                .context("kline.local_ip is required when enabled")?;
            let public = self
                .public_ip
                .context("kline.public_ip is required when enabled")?;
            if local.is_unspecified()
                || local.is_loopback()
                || local.is_multicast()
                || public.is_unspecified()
                || public.is_loopback()
                || public.is_multicast()
                || matches!(public, IpAddr::V4(address) if address.is_private())
                || matches!(public, IpAddr::V6(address) if address.is_unique_local() || address.is_unicast_link_local())
            {
                bail!("kline requires explicit dedicated local/public addresses");
            }
            if self.trade_engine_configs.is_empty() {
                bail!("kline.trade_engine_configs must list all live trade engine TOMLs");
            }
            if self
                .trade_engine_configs
                .iter()
                .any(|path| !path.is_absolute())
            {
                bail!("kline.trade_engine_configs paths must be absolute");
            }
            if self.forbidden_public_ips.contains(&public) {
                bail!("kline.public_ip must not use a trading public IP");
            }
        }
        Ok(())
    }
}

impl Default for DingTalkConfig {
    fn default() -> Self {
        Self {
            market_webhook_url_env: "CTA_DINGTALK_MARKET_WEBHOOK_URL".to_string(),
            order_webhook_url_env: "CTA_DINGTALK_ORDER_WEBHOOK_URL".to_string(),
            market_secret_env: None,
            order_secret_env: None,
            request_timeout_secs: 5,
            retry_attempts: 5,
            retry_backoff_ms: 1_000,
            at_mobiles: Vec::new(),
            is_at_all: false,
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
        if self.treasury.use_account_ip_rotation && self.treasury.local_ip.is_some() {
            bail!("treasury.local_ip and use_account_ip_rotation are mutually exclusive");
        }
        if self.database.url_env.trim().is_empty() {
            bail!("database.url_env must not be empty");
        }
        if self.database.max_connections == 0 {
            bail!("database.max_connections must be greater than zero");
        }
        if self.dashboard.refresh_secs == 0 {
            bail!("dashboard.refresh_secs must be greater than zero");
        }
        if !(1..=256).contains(&self.dashboard.compute_threads) {
            bail!("dashboard.compute_threads must be between 1 and 256");
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
        if self.monitor.poll_interval_secs == 0 {
            bail!("monitor.poll_interval_secs must be greater than zero");
        }
        if self.monitor.repeat_alert_secs == 0 {
            bail!("monitor.repeat_alert_secs must be greater than zero");
        }
        if self.monitor.market_stale_secs == 0 {
            bail!("monitor.market_stale_secs must be greater than zero");
        }
        if self.monitor.order_stale_secs == 0 {
            bail!("monitor.order_stale_secs must be greater than zero");
        }
        if self.monitor.position_stale_secs == 0 {
            bail!("monitor.position_stale_secs must be greater than zero");
        }
        if self.monitor.position_not_ready_secs == 0 {
            bail!("monitor.position_not_ready_secs must be greater than zero");
        }
        if self.monitor.recent_order_records == 0 {
            bail!("monitor.recent_order_records must be greater than zero");
        }
        if !self.monitor.position_tolerance.is_finite() || self.monitor.position_tolerance < 0.0 {
            bail!("monitor.position_tolerance must be finite and non-negative");
        }
        if !self.monitor.position_residual_usdt.is_finite()
            || self.monitor.position_residual_usdt < 0.0
        {
            bail!("monitor.position_residual_usdt must be finite and non-negative");
        }
        if self.monitor.market_heartbeat_hours > 24 {
            bail!("monitor.market_heartbeat_hours must be between 0 and 24");
        }
        if self.monitor.order_heartbeat_hours > 24 {
            bail!("monitor.order_heartbeat_hours must be between 0 and 24");
        }
        if self.monitor.heartbeat_quiet_start_hour >= 24
            || self.monitor.heartbeat_quiet_end_hour >= 24
        {
            bail!("monitor heartbeat quiet hours must be between 0 and 23");
        }
        if self.monitor.dingtalk.request_timeout_secs == 0 {
            bail!("monitor.dingtalk.request_timeout_secs must be greater than zero");
        }
        if self.monitor.dingtalk.retry_attempts == 0 {
            bail!("monitor.dingtalk.retry_attempts must be greater than zero");
        }
        if self.monitor.dingtalk.retry_backoff_ms == 0 {
            bail!("monitor.dingtalk.retry_backoff_ms must be greater than zero");
        }
        if self.monitor.enabled {
            if self
                .monitor
                .dingtalk
                .market_webhook_url_env
                .trim()
                .is_empty()
            {
                bail!("monitor.dingtalk.market_webhook_url_env must not be empty");
            }
            if self
                .monitor
                .dingtalk
                .order_webhook_url_env
                .trim()
                .is_empty()
            {
                bail!("monitor.dingtalk.order_webhook_url_env must not be empty");
            }
            if self
                .monitor
                .dingtalk
                .market_secret_env
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                bail!("monitor.dingtalk.market_secret_env must not be empty when set");
            }
            if self
                .monitor
                .dingtalk
                .order_secret_env
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                bail!("monitor.dingtalk.order_secret_env must not be empty when set");
            }
        }
        validate_loopback_redis_url(&self.redis.url)?;
        if !self.kline.rocksdb_path.is_absolute() {
            bail!(
                "kline.rocksdb_path must be absolute: {}",
                self.kline.rocksdb_path.display()
            );
        }
        for source in &self.sources {
            if source.enabled && source.rocksdb_path == self.kline.rocksdb_path {
                bail!(
                    "kline.rocksdb_path must not reuse an Exec persist_manager path: {}",
                    self.kline.rocksdb_path.display()
                );
            }
        }
        self.kline.validate()?;
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
            if source
                .estimated_fee_rate
                .is_some_and(|value| !value.is_finite())
            {
                bail!("source {} estimated_fee_rate must be finite", source.id);
            }
            if source
                .maker_fee_rate
                .is_some_and(|value| !value.is_finite())
            {
                bail!("source {} maker_fee_rate must be finite", source.id);
            }
            if source
                .taker_fee_rate
                .is_some_and(|value| !value.is_finite())
            {
                bail!("source {} taker_fee_rate must be finite", source.id);
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
            if let Some(env_path) = &source.env_path {
                if !env_path.is_absolute() {
                    bail!(
                        "source {} env_path must be absolute: {}",
                        source.id,
                        env_path.display()
                    );
                }
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

    /// Overlay operator-managed fee rates (typically loaded from PostgreSQL).
    /// Missing map entries keep the current value (usually toml bootstrap).
    pub fn with_fee_rates(mut self, rates: &std::collections::BTreeMap<String, FeeRates>) -> Self {
        for source in &mut self.sources {
            if let Some(rates) = rates.get(&source.id) {
                source.maker_fee_rate = Some(rates.maker);
                source.taker_fee_rate = Some(rates.taker);
            }
        }
        self
    }
}

impl SourceConfig {
    pub fn nav_fee_rates(&self) -> Result<FeeRates> {
        let legacy = self.estimated_fee_rate;
        let rates = FeeRates {
            maker: self
                .maker_fee_rate
                .or(legacy)
                .with_context(|| format!("source {} requires maker_fee_rate", self.id))?,
            taker: self
                .taker_fee_rate
                .or(legacy)
                .with_context(|| format!("source {} requires taker_fee_rate", self.id))?,
        };
        validate_fee_rates(rates)?;
        Ok(rates)
    }

    pub fn env_path(&self) -> PathBuf {
        self.env_path.clone().unwrap_or_else(|| {
            self.rocksdb_path
                .parent()
                .and_then(Path::parent)
                .map(|root| root.join("env.sh"))
                .unwrap_or_else(|| PathBuf::from("env.sh"))
        })
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

pub fn validate_fee_rates(rates: FeeRates) -> Result<()> {
    if !rates.maker.is_finite() {
        bail!("maker_fee_rate must be finite");
    }
    if !rates.taker.is_finite() {
        bail!("taker_fee_rate must be finite");
    }
    Ok(())
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

    #[test]
    fn monitor_not_ready_threshold_defaults_and_validates() {
        let monitor: MonitorConfig =
            toml::from_str("position_stale_secs = 30").expect("legacy monitor config");
        assert_eq!(monitor.position_not_ready_secs, 60);

        let mut config = config_with_sources(vec![source("trade01", "/tmp/trade01")]);
        config.monitor.position_not_ready_secs = 0;
        assert!(
            config
                .validate()
                .expect_err("zero threshold must be rejected")
                .to_string()
                .contains("monitor.position_not_ready_secs")
        );
    }

    fn config_with_sources(sources: Vec<SourceConfig>) -> AppConfig {
        AppConfig {
            database: DatabaseConfig {
                url_env: default_database_url_env(),
                max_connections: 4,
            },
            dashboard: DashboardConfig::default(),
            order_config: OrderConfigSettings::default(),
            redis: RedisSettings::default(),
            kline: KlineConfig::default(),
            monitor: MonitorConfig::default(),
            treasury: crate::config::TreasuryConfig::default(),
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
            monitor_enabled: true,
            estimated_fee_rate: Some(0.0004),
            maker_fee_rate: None,
            taker_fee_rate: None,
            gateway_prefix: Some(format!("/{id}")),
            exec_config_url: None,
            exec_viz_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            legacy_share_unit_usdt: None,
            env_path: None,
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
    fn dashboard_refresh_defaults_and_validates_host_configs() {
        for raw in [
            include_str!("../config/cta-manager.example.toml"),
            include_str!("../deploy/crypto_cta_manager/cta-manager.toml"),
            include_str!("../deploy/jp_meta/cta-manager.toml"),
        ] {
            let mut settings: toml::Value = toml::from_str(raw).unwrap();
            settings.as_table_mut().unwrap().remove("dashboard");
            let config: AppConfig = settings.clone().try_into().unwrap();
            config.validate().unwrap();
            assert_eq!(config.dashboard.refresh_secs, 60);

            let mut dashboard = toml::Table::new();
            dashboard.insert("refresh_secs".to_string(), 90.into());
            settings
                .as_table_mut()
                .unwrap()
                .insert("dashboard".to_string(), toml::Value::Table(dashboard));
            let config: AppConfig = settings.clone().try_into().unwrap();
            config.validate().unwrap();
            assert_eq!(config.dashboard.refresh_secs, 90);

            settings["dashboard"]["refresh_secs"] = 0.into();
            let config: AppConfig = settings.try_into().unwrap();
            assert!(
                config
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("dashboard.refresh_secs")
            );
        }
    }

    #[test]
    fn monitor_enabled_defaults_on_and_can_be_disabled_for_provisioned_source() {
        let raw = include_str!("../deploy/crypto_cta_manager/cta-manager.toml");
        let config: AppConfig = toml::from_str(raw).unwrap();
        config.validate().unwrap();
        assert!(
            config
                .sources
                .iter()
                .find(|source| source.id == "binance_exec_trade01")
                .unwrap()
                .monitor_enabled
        );
        assert!(
            config
                .sources
                .iter()
                .find(|source| source.id == "binance_exec_trade09")
                .unwrap()
                .monitor_enabled
        );
        for number in 10..=11 {
            let id = format!("binance_exec_trade{number:02}");
            let source = config
                .sources
                .iter()
                .find(|source| source.id == id)
                .unwrap();
            assert!(source.enabled);
            assert!(!source.monitor_enabled);
        }
        assert!(
            config
                .sources
                .iter()
                .find(|source| source.id == "binance_exec_trade12")
                .unwrap()
                .monitor_enabled
        );
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
        assert!(without_rate.nav_fee_rates().is_err());
    }

    #[test]
    fn fee_rates_overlay_replaces_matching_sources_only() {
        let config = config_with_sources(vec![
            source("binance_exec_trade01", "/srv/trade01/persist_manager"),
            source("binance_exec_trade02", "/srv/trade02/persist_manager"),
        ])
        .with_fee_rates(&std::collections::BTreeMap::from([(
            "binance_exec_trade02".to_string(),
            FeeRates {
                maker: -0.0001,
                taker: 0.0008,
            },
        )]));
        assert_eq!(config.sources[0].maker_fee_rate, None);
        assert_eq!(config.sources[1].maker_fee_rate, Some(-0.0001));
        assert_eq!(config.sources[1].taker_fee_rate, Some(0.0008));
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
    fn enabled_monitor_requires_a_webhook_environment_name() {
        let mut config = config_with_sources(vec![source(
            "binance_exec_trade01",
            "/srv/trade01/persist_manager",
        )]);
        config.monitor.enabled = true;
        config.monitor.dingtalk.market_webhook_url_env.clear();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("market_webhook_url_env")
        );
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
        assert!(!config.kline.enabled);
        assert_eq!(config.kline.refresh_secs, 300);
        assert_eq!(config.kline.retain_days, 30);
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
    fn accepts_but_does_not_use_legacy_share_unit_config() {
        let raw = include_str!("../config/cta-manager.example.toml").replacen(
            "account_ipc_service = \"account_pubs/binance_pm\"",
            "account_ipc_service = \"account_pubs/binance_pm\"\nshare_unit_usdt = 10000",
            1,
        );
        let config: AppConfig = toml::from_str(&raw).unwrap();
        config.validate().unwrap();
        assert_eq!(config.sources[0].legacy_share_unit_usdt, Some(10_000.0));
    }

    #[test]
    fn jp_meta_config_enables_four_trade_accounts() {
        let config: AppConfig =
            toml::from_str(include_str!("../deploy/jp_meta/cta-manager.toml")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.sources.len(), 4);
        assert_eq!(config.sources[0].id, "binance_exec_trade01");
        assert_eq!(config.sources[0].display_name(), "prc_cta_01");
        assert_eq!(config.sources[1].id, "binance_exec_trade02");
        assert_eq!(config.sources[1].display_name(), "prc_cta_02");
        assert_eq!(config.sources[3].display_name(), "prc");
        assert!(config.sources.iter().all(|source| source.enabled));
        assert_eq!(
            config.kline.rocksdb_path.as_os_str(),
            "/home/ubuntu/crypto_cta_manager/db"
        );
    }

    #[test]
    fn kline_path_must_not_reuse_exec_persist_manager() {
        let mut config = config_with_sources(vec![source(
            "binance_exec_trade01",
            "/srv/trade01/persist_manager",
        )]);
        config.kline.enabled = false;
        config.kline.rocksdb_path = PathBuf::from("/srv/trade01/persist_manager");
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("must not reuse")
        );
    }
}
