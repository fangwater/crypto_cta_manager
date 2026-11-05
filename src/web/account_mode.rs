//! Account modes belong to the startup configuration, not to each target.
//! Sources and their credentials/mode settings require a Manager restart to
//! reload. Never refresh this cache from a publish or a periodic worker.
use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::http::StatusCode;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::config::SourceConfig;
use crate::exchange_leverage::{has_required_trading_account_mode, required_trading_account_mode};
use crate::order_config::TargetPosition;

use super::{PublishFailure, targets_require_trading_account_mode};

#[derive(Debug)]
enum CheckedMode {
    Verified,
    Rejected,
    Failed(String),
}

#[derive(Default)]
pub(super) struct AccountModes {
    sources: BTreeMap<String, CheckedMode>,
}

impl AccountModes {
    /// Finish every enabled publisher's check before opening the API. An
    /// account failure is retained locally and does not disable other sources.
    pub(super) async fn initialize(sources: &[SourceConfig]) -> Result<Self> {
        let mut pending = sources
            .iter()
            .filter(|source| source.enabled && source.exec_config_url.is_some());
        let mut jobs = JoinSet::new();
        let mut modes = Self::default();
        loop {
            while jobs.len() < 4 {
                let Some(source) = pending.next().cloned() else {
                    break;
                };
                jobs.spawn(async move {
                    let started = Instant::now();
                    let checked = match has_required_trading_account_mode(&source).await {
                        Ok(true) => CheckedMode::Verified,
                        Ok(false) => CheckedMode::Rejected,
                        Err(error) => CheckedMode::Failed(format!("{error:#}")),
                    };
                    let duration_ms = started.elapsed().as_millis() as u64;
                    match &checked {
                        CheckedMode::Verified => info!(source_id = %source.id, duration_ms,
                            "startup account mode verified"),
                        CheckedMode::Rejected => warn!(source_id = %source.id, duration_ms,
                            required_mode = required_trading_account_mode(&source.venue),
                            "startup account mode rejected; non-zero publishes blocked until restart"),
                        CheckedMode::Failed(error) => warn!(source_id = %source.id, duration_ms, %error,
                            "startup account mode check failed; non-zero publishes blocked until restart"),
                    }
                    (source.id, checked)
                });
            }
            let Some(result) = jobs.join_next().await else {
                break;
            };
            let (id, checked) = result.context("account mode startup task failed")?;
            modes.sources.insert(id, checked);
        }
        Ok(modes)
    }

    /// Deliberately synchronous: target publication must not query the exchange
    /// or reread credentials. Clearing targets remains possible after a failed
    /// startup check. An absent result is never treated as verified.
    pub(super) fn check(
        &self,
        source: &SourceConfig,
        targets: &BTreeMap<String, TargetPosition>,
    ) -> std::result::Result<(), PublishFailure> {
        if !targets_require_trading_account_mode(targets) {
            return Ok(());
        }
        let required = required_trading_account_mode(&source.venue);
        let (status, message) = match self.sources.get(&source.id) {
            Some(CheckedMode::Verified) => return Ok(()),
            Some(CheckedMode::Rejected) => (
                StatusCode::CONFLICT,
                format!(
                    "source {} must use {required}; startup check rejected the account mode",
                    source.id
                ),
            ),
            Some(CheckedMode::Failed(error)) => (
                StatusCode::BAD_GATEWAY,
                format!(
                    "failed to verify {required} for source {} at startup: {error}",
                    source.id
                ),
            ),
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "source {} has no startup account mode verification",
                    source.id
                ),
            ),
        };
        Err(PublishFailure {
            status,
            message: format!(
                "{message}; correct the account configuration and restart Manager before publishing non-zero targets"
            ),
        })
    }
}

#[cfg(test)]
mod tests;
