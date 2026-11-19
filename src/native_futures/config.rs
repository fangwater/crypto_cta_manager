use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mode: crate::runtime_mode::RuntimeMode,
    #[serde(default = "default_database")]
    pub database: crate::config::DatabaseConfig,
    pub secondary_root: PathBuf,
    /// Operator-maintained daily reference packs; never an execution store.
    #[serde(default)]
    pub reference_root: Option<PathBuf>,
    #[serde(default)]
    pub market_data: Option<super::market_data::Settings>,
    #[serde(default)]
    pub reference_service: Option<super::reference_data::Settings>,
    #[serde(default)]
    pub sync: Option<super::sync::PullSettings>,
    #[serde(default)]
    pub sync_export: Option<super::sync::ExportSettings>,
    #[serde(default = "default_stale_secs")]
    pub stale_secs: u64,
    #[serde(default = "default_rows")]
    pub recent_rows: usize,
    #[serde(default)]
    pub source_viewers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub execution_monitor_urls: BTreeMap<String, String>,
    /// Dedicated per-source read credentials for the Exec-side plan consumer.
    #[serde(default)]
    pub ctaker_plan_readers: BTreeMap<String, String>,
    pub sources: Vec<Source>,
}
fn default_database() -> crate::config::DatabaseConfig {
    crate::config::DatabaseConfig {
        url_env: "CN_FUTURES_CTA_DATABASE_URL".into(),
        max_connections: 8,
    }
}
fn default_stale_secs() -> u64 {
    30
}
fn default_rows() -> usize {
    200
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub account: String,
    pub adapter: String,
    pub rocksdb_path: PathBuf,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Presence explicitly identifies a Manager-owned local replica.
    #[serde(default)]
    pub sync: Option<super::sync::Remote>,
}
fn default_enabled() -> bool {
    true
}

/// Resolve existing ancestors too, so a not-yet-created secondary cannot hide
/// an overlap behind a symlink or `..` component.
pub(super) fn resolved(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "storage paths must be absolute and contain no .."
    );
    if path.exists() {
        return path.canonicalize().context("resolve storage path");
    }
    Ok(
        resolved(path.parent().context("storage path has no parent")?)?
            .join(path.file_name().context("storage path has no filename")?),
    )
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = toml::from_str(
            &std::fs::read_to_string(path).context("read domestic Manager configuration")?,
        )
        .context("parse domestic Manager configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn source_ids(&self) -> BTreeSet<String> {
        self.sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.clone())
            .collect()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.mode == crate::runtime_mode::RuntimeMode::CnFutures,
            "native Manager and sync require mode = cn_futures"
        );
        ensure!(
            !self.sources.is_empty(),
            "at least one domestic source is required"
        );
        ensure!(
            (1..=1000).contains(&self.recent_rows),
            "recent_rows must be 1..1000"
        );
        ensure!(
            (1..=86400).contains(&self.stale_secs),
            "stale_secs must be 1..86400"
        );
        ensure!(
            !self.database.url_env.trim().is_empty() && self.database.max_connections > 0,
            "database environment name and pool size are required"
        );
        let secondary = resolved(&self.secondary_root)?;
        let reference = self.reference_root.as_deref().map(resolved).transpose()?;
        let market_cache = self
            .market_data
            .as_ref()
            .map(|m| {
                m.validate()?;
                resolved(&m.cache_root)
            })
            .transpose()?;
        if let Some(cache) = &market_cache {
            ensure!(
                !cache.starts_with(&secondary) && !secondary.starts_with(cache),
                "market cache and secondary_root must be disjoint"
            );
            if let Some(root) = &reference {
                ensure!(
                    !cache.starts_with(root) && !root.starts_with(cache),
                    "market cache and reference_root must be disjoint"
                );
            }
        }
        if let Some(root) = &reference {
            ensure!(
                !root.starts_with(&secondary) && !secondary.starts_with(root),
                "reference_root and secondary_root must be disjoint"
            );
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for source in &self.sources {
            ensure!(
                !source.id.is_empty()
                    && source
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    && ids.insert(source.id.clone()),
                "invalid or duplicate source id"
            );
            ensure!(
                !source.account.trim().is_empty()
                    && matches!(source.adapter.as_str(), "yida" | "ctp"),
                "domestic sources require an account name and adapter = yida or ctp"
            );
            let path = resolved(&source.rocksdb_path)?;
            if let Some(cache) = &market_cache {
                ensure!(
                    !cache.starts_with(&path) && !path.starts_with(cache),
                    "market cache and execution stores must be disjoint"
                );
            }
            if let Some(root) = &reference {
                ensure!(
                    !root.starts_with(&path) && !path.starts_with(root),
                    "reference_root and execution stores must be disjoint"
                );
            }
            ensure!(
                !path.starts_with(&secondary) && !secondary.starts_with(&path),
                "secondary_root and execution stores must be disjoint"
            );
            ensure!(
                !source.enabled || paths.insert(path),
                "enabled sources must not share an execution store"
            );
        }
        ensure!(
            self.source_viewers
                .iter()
                .all(|(id, users)| ids.contains(id)
                    && !users.is_empty()
                    && users.iter().all(|u| !u.trim().is_empty())),
            "source_viewers requires configured sources and nonempty usernames"
        );
        for (id, address) in &self.execution_monitor_urls {
            let url = reqwest::Url::parse(address).context("invalid execution monitor URL")?;
            ensure!(
                ids.contains(id)
                    && matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none(),
                "execution_monitor_urls requires configured sources and HTTP(S) links without credentials"
            );
        }
        let mut plan_envs = BTreeSet::new();
        ensure!(
            self.ctaker_plan_readers.iter().all(|(id, env)| {
                !env.is_empty()
                    && env.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    && env != &self.database.url_env
                    && plan_envs.insert(env)
                    && self.sources.iter().any(|s| &s.id == id && s.enabled)
                    && self
                        .sources
                        .iter()
                        .all(|s| s.sync.as_ref().is_none_or(|s| &s.token_env != env))
            }),
            "ctaker plan readers require enabled native sources and distinct dedicated token variables"
        );
        if let Some(reference) = &self.reference_service {
            reference.validate()?;
            ensure!(
                reference.accounts.keys().all(|id| self
                    .sources
                    .iter()
                    .any(|s| &s.id == id && s.adapter == "ctp")),
                "reference account mappings require configured CTP source_ids"
            );
        }
        super::sync::validate_config(self)?;
        Ok(())
    }
}
