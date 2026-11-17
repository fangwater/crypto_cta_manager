use super::*;
use crate::native_futures::config::{Config, resolved};
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
    pub url: String,
    pub token_env: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullSettings {
    #[serde(default)]
    pub enabled: bool,
    pub replica_root: PathBuf,
    #[serde(default = "poll")]
    pub poll_secs: u64,
    #[serde(default = "timeout")]
    pub timeout_secs: u64,
    #[serde(default = "concurrent_sources")]
    pub concurrent_sources: usize,
}
fn concurrent_sources() -> usize {
    4
}
fn poll() -> u64 {
    10
}
fn timeout() -> u64 {
    30
}
fn bind() -> SocketAddr {
    "127.0.0.1:18212".parse().unwrap()
}
fn records() -> usize {
    500
}
fn bytes() -> usize {
    4 * 1024 * 1024
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSettings {
    #[serde(default = "bind")]
    pub bind: SocketAddr,
    pub token_env: String,
    pub journal_root: PathBuf,
    pub source_ids: Vec<String>,
    #[serde(default = "records")]
    pub batch_records: usize,
    #[serde(default = "bytes")]
    pub batch_bytes: usize,
}

fn env_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}
fn disjoint(a: &Path, b: &Path) -> bool {
    !a.starts_with(b) && !b.starts_with(a)
}
pub fn validate_config(config: &Config) -> Result<()> {
    let mut roots = vec![resolved(&config.secondary_root)?];
    if let Some(r) = &config.reference_root {
        roots.push(resolved(r)?);
    }
    if let Some(m) = &config.market_data {
        roots.push(resolved(&m.cache_root)?);
    }
    if let Some(sync) = &config.sync {
        ensure!(
            (1..=86400).contains(&sync.poll_secs)
                && (1..=3600).contains(&sync.timeout_secs)
                && (1..=32).contains(&sync.concurrent_sources),
            "invalid native synchronization timing"
        );
        let root = resolved(&sync.replica_root)?;
        ensure!(
            roots.iter().all(|r| disjoint(r, &root)),
            "replica_root overlaps another Manager store"
        );
        for source in &config.sources {
            let path = resolved(&source.rocksdb_path)?;
            if let Some(remote) = &source.sync {
                ensure!(
                    path == resolved(&root.join(&source.id))?,
                    "replica path must equal replica_root/source_id"
                );
                let url = reqwest::Url::parse(&remote.url).context("invalid native sync URL")?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && env_name(&remote.token_env),
                    "native sync requires HTTP(S), an environment token name and no URL credentials/query/fragment"
                );
            } else {
                ensure!(
                    disjoint(&root, &path),
                    "replica_root overlaps a local execution store"
                );
            }
        }
        roots.push(root);
    } else {
        ensure!(
            config.sources.iter().all(|s| s.sync.is_none()),
            "native remote sources require [sync] and an explicit replica_root"
        );
    }
    if let Some(export) = &config.sync_export {
        let root = resolved(&export.journal_root)?;
        ensure!(
            env_name(&export.token_env) && !export.source_ids.is_empty(),
            "export requires a token environment name and explicit source_ids"
        );
        ensure!(
            (1..=1000).contains(&export.batch_records)
                && (1024..=8 * 1024 * 1024).contains(&export.batch_bytes),
            "invalid native export batch limits"
        );
        ensure!(
            roots.iter().all(|r| disjoint(r, &root))
                && config
                    .sources
                    .iter()
                    .all(|s| resolved(&s.rocksdb_path).is_ok_and(|p| disjoint(&p, &root))),
            "export journal overlaps an execution or Manager store"
        );
        let mut ids = BTreeSet::new();
        for id in &export.source_ids {
            ensure!(
                ids.insert(id)
                    && config
                        .sources
                        .iter()
                        .any(|s| s.id == *id && s.enabled && s.sync.is_none()),
                "export source must be unique, enabled and read directly from a local execution store"
            );
        }
    }
    Ok(())
}

pub(super) fn token(name: &str) -> Result<String> {
    let value = std::env::var(name).context("native sync token environment variable is not set")?;
    ensure!(
        value.len() >= 32 && !value.bytes().any(|b| b.is_ascii_whitespace()),
        "native sync token must contain at least 32 non-whitespace bytes"
    );
    Ok(value)
}
