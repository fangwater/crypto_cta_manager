use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub database: crate::config::DatabaseConfig,
    pub secondary_root: PathBuf,
    #[serde(default = "default_stale_secs")]
    pub stale_secs: u64,
    #[serde(default = "default_rows")]
    pub recent_rows: usize,
    #[serde(default)]
    pub source_viewers: BTreeMap<String, Vec<String>>,
    pub sources: Vec<Source>,
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
                !source.account.trim().is_empty() && source.adapter == "yida",
                "first release requires a named Yida source"
            );
            let path = resolved(&source.rocksdb_path)?;
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
        Ok(())
    }
}
