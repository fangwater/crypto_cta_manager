//! CN-only, SDK-free replication of native evidence into Manager-owned RocksDB.
//! The source stays authoritative; transport cursors never denote trading facts.
mod collector;
mod export;
mod settings;
mod store;

pub use collector::{Replica, pull};
pub use export::{ExportService, adopt_source_checkpoint};
pub use settings::{ExportSettings, PullSettings, Remote, validate_config};
pub use store::{ReplicaBusy, ReplicaStatus, replica_read_lock, replica_status};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGE_RECORDS: usize = 10_000;
const OWNER: &[u8] = b"meta/owner";
const STATE: &[u8] = b"meta/state";
const STAGING: &str = "cn_sync_staging";
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The two replicated domestic logs. Both are append-only: overwriting or
/// deleting an existing key is a conflict.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
pub enum NativeCf {
    #[serde(rename = "cn_uniform_orders")]
    Orders,
    #[serde(rename = "cn_trade_updates")]
    Trades,
}
pub(super) const CF_COUNT: usize = 2;
impl NativeCf {
    const ALL: [Self; CF_COUNT] = [Self::Orders, Self::Trades];
    fn name(self) -> &'static str {
        match self {
            Self::Orders => super::records::ORDERS,
            Self::Trades => super::records::TRADES,
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Orders => 0,
            Self::Trades => 1,
        }
    }
}

/// Manager-owned replica families that are not Exec records.
pub(super) fn internal(cf: &str) -> bool {
    cf == STAGING
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub sequence: u64,
    pub column_family: NativeCf,
    pub key: String,
    pub value: String,
    pub value_sha256: String,
    pub previous_sha256: String,
    pub record_sha256: String,
}
impl Change {
    fn new(sequence: u64, cf: NativeCf, key: &[u8], value: &[u8], previous: &str) -> Result<Self> {
        ensure!(
            key.len() + value.len() <= MAX_RECORD_BYTES,
            "native record exceeds replication limit"
        );
        let mut record = Self {
            sequence,
            column_family: cf,
            key: STANDARD.encode(key),
            value: STANDARD.encode(value),
            value_sha256: hex::encode(Sha256::digest(value)),
            previous_sha256: previous.into(),
            record_sha256: String::new(),
        };
        record.record_sha256 = record.chain_hash(key)?;
        Ok(record)
    }
    fn chain_hash(&self, key: &[u8]) -> Result<String> {
        let previous = hex::decode(&self.previous_sha256)?;
        let value = hex::decode(&self.value_sha256)?;
        ensure!(
            previous.len() == 32 && value.len() == 32,
            "invalid replication chain hash"
        );
        let mut digest = Sha256::new();
        digest.update(b"cn_futures");
        digest.update(previous);
        digest.update(self.sequence.to_be_bytes());
        digest.update([self.column_family.index() as u8]);
        digest.update((key.len() as u64).to_be_bytes());
        digest.update(key);
        digest.update(value);
        Ok(hex::encode(digest.finalize()))
    }
    fn decode(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        ensure!(
            self.key.len() + self.value.len() <= MAX_RECORD_BYTES * 2,
            "oversized encoded replication record"
        );
        let key = STANDARD
            .decode(&self.key)
            .context("invalid replication key")?;
        let value = STANDARD
            .decode(&self.value)
            .context("invalid replication value")?;
        ensure!(
            key.len() + value.len() <= MAX_RECORD_BYTES
                && hex::encode(Sha256::digest(&value)) == self.value_sha256
                && self.chain_hash(&key)? == self.record_sha256,
            "replication record checksum mismatch"
        );
        Ok((key, value))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub mode: crate::runtime_mode::RuntimeMode,
    pub store_id: Option<String>,
    pub after: u64,
    pub after_sha256: String,
    pub until: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub mode: crate::runtime_mode::RuntimeMode,
    pub source_id: String,
    pub store_id: String,
    pub after: u64,
    pub after_sha256: String,
    pub next: u64,
    pub until: u64,
    pub next_sha256: String,
    pub until_sha256: String,
    pub captured_at_us: i64,
    pub changes: Vec<Change>,
}

/// Manager's read-only query of a replica source's broker funds through the
/// same authenticated exporter. `None` when the exporter serves no funds.
pub async fn account_funds(
    client: &reqwest::Client,
    source: &crate::native_futures::config::Source,
) -> Result<Option<crate::native_futures::funds::Observation>> {
    let remote = source.sync.as_ref().context("native remote source")?;
    funds_with_token(client, source, &settings::token(&remote.token_env)?).await
}
async fn funds_with_token(
    client: &reqwest::Client,
    source: &crate::native_futures::config::Source,
    token: &str,
) -> Result<Option<crate::native_futures::funds::Observation>> {
    let remote = source.sync.as_ref().context("native remote source")?;
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?;
    authorization.set_sensitive(true);
    let response = client
        .get(format!(
            "{}/api/native/sync/{}/account-funds",
            remote.url.trim_end_matches('/'),
            source.id
        ))
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let observation: crate::native_futures::funds::Observation =
        response.error_for_status()?.json().await?;
    observation.validate(&source.id, &source.adapter)?;
    Ok(Some(observation))
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests;
