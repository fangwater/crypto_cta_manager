use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    #[serde(default)]
    pub ingestion: IngestionConfig,
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
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// Stable, globally unique deployment identity, such as binance_exec_trade01.
    pub id: String,
    pub account: String,
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
            bail!("at least one source must be enabled");
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
            sources,
        }
    }

    fn source(id: &str, path: &str) -> SourceConfig {
        SourceConfig {
            id: id.to_string(),
            account: id.to_string(),
            venue: "binance-futures".to_string(),
            rocksdb_path: PathBuf::from(path),
            enabled: true,
            start_ts_us: None,
            poll_interval_secs: None,
            estimated_fee_rate: Some(0.0004),
            gateway_prefix: Some(format!("/{id}")),
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
    fn example_config_stays_valid() {
        let config: AppConfig =
            toml::from_str(include_str!("../config/cta-manager.example.toml")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.sources[0].id, "binance_exec_trade01");
    }
}
