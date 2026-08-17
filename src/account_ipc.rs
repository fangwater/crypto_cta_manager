use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iceoryx2::prelude::*;
use iceoryx2::service::ipc;
use serde::Serialize;
use tracing::{info, warn};

use crate::config::SourceConfig;

pub const ACCOUNT_IPC_PAYLOAD: usize = 16_384;
pub const ACCOUNT_IPC_HISTORY_SIZE: usize = 4_096;
pub const ACCOUNT_IPC_MAX_SUBSCRIBERS: usize = 4;
pub const ACCOUNT_IPC_SUBSCRIBER_BUFFER: usize = 4_096;
const WALLET_SNAPSHOT_TYPE: u32 = 4008;
const EVENT_HEADER_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveEquitySnapshot {
    pub source: String,
    pub equity_usdt: f64,
    pub wallet_balance_usdt: f64,
    pub unrealized_pnl_usdt: f64,
    pub available_balance_usdt: f64,
    pub ts_ms: i64,
}

#[derive(Clone, Default)]
pub struct LiveEquityHub {
    snapshots: Arc<RwLock<HashMap<String, LiveEquitySnapshot>>>,
}

impl LiveEquityHub {
    pub fn spawn(sources: &[SourceConfig]) -> Self {
        let hub = Self::default();
        for source in sources {
            if !source.enabled {
                continue;
            }
            let Some(service_name) = source.account_ipc_service_name() else {
                continue;
            };
            let source_id = source.id.clone();
            let snapshots = Arc::clone(&hub.snapshots);
            thread::Builder::new()
                .name(format!("cta-equity-{source_id}"))
                .spawn(move || subscribe_loop(source_id, service_name, snapshots))
                .expect("failed to spawn account IPC subscriber");
        }
        hub
    }

    pub fn get(&self, source_id: &str) -> Option<LiveEquitySnapshot> {
        self.snapshots
            .read()
            .ok()
            .and_then(|guard| guard.get(source_id).cloned())
    }
}

fn subscribe_loop(
    source_id: String,
    service_name: String,
    snapshots: Arc<RwLock<HashMap<String, LiveEquitySnapshot>>>,
) {
    loop {
        if let Err(error) = run_subscriber(&source_id, &service_name, &snapshots) {
            warn!(
                source_id,
                service_name,
                error = %error,
                "account IPC subscriber stopped, retrying"
            );
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn run_subscriber(
    source_id: &str,
    service_name: &str,
    snapshots: &Arc<RwLock<HashMap<String, LiveEquitySnapshot>>>,
) -> Result<()> {
    let node_name = format!(
        "cta_web_am_{}",
        source_id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect::<String>()
    );
    let node = NodeBuilder::new()
        .name(&NodeName::new(&node_name)?)
        .create::<ipc::Service>()
        .with_context(|| format!("failed to create iceoryx node {node_name}"))?;
    info!(source_id, service_name, "opening account monitor IPC");

    let service = loop {
        match node
            .service_builder(&ServiceName::new(service_name)?)
            .publish_subscribe::<[u8; ACCOUNT_IPC_PAYLOAD]>()
            .max_publishers(1)
            .max_subscribers(ACCOUNT_IPC_MAX_SUBSCRIBERS)
            .history_size(ACCOUNT_IPC_HISTORY_SIZE)
            .subscriber_max_buffer_size(ACCOUNT_IPC_SUBSCRIBER_BUFFER)
            .open()
        {
            Ok(service) => break service,
            Err(error) => {
                warn!(
                    source_id,
                    service_name,
                    error = ?error,
                    "waiting for account_monitor IPC service"
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    };

    let subscriber = service
        .subscriber_builder()
        .buffer_size(ACCOUNT_IPC_SUBSCRIBER_BUFFER)
        .create()
        .with_context(|| format!("failed to subscribe to {service_name}"))?;
    info!(source_id, service_name, "account monitor IPC subscribed");

    loop {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                if let Some(snapshot) = parse_live_equity(sample.payload()) {
                    let equity_usdt = snapshot.equity_usdt;
                    let first = snapshots
                        .read()
                        .ok()
                        .is_none_or(|guard| !guard.contains_key(source_id));
                    if let Ok(mut guard) = snapshots.write() {
                        guard.insert(source_id.to_string(), snapshot);
                    }
                    if first {
                        info!(
                            source_id,
                            equity_usdt, "account monitor live equity received"
                        );
                    }
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => bail!("account IPC receive failed: {error}"),
        }
    }
}

pub fn parse_live_equity(payload: &[u8]) -> Option<LiveEquitySnapshot> {
    if payload.len() < EVENT_HEADER_LEN {
        return None;
    }
    let event_type = u32::from_le_bytes(payload[0..4].try_into().ok()?);
    if event_type != WALLET_SNAPSHOT_TYPE {
        return None;
    }
    let body_len = u32::from_le_bytes(payload[8..12].try_into().ok()?) as usize;
    let end = EVENT_HEADER_LEN.checked_add(body_len)?;
    if payload.len() < end {
        return None;
    }
    parse_wallet_snapshot(&payload[EVENT_HEADER_LEN..end])
}

fn parse_wallet_snapshot(data: &[u8]) -> Option<LiveEquitySnapshot> {
    const MIN_SIZE: usize = 4 + 8 + 8 + 4 + 1 + 3 + 8 * 5;
    if data.len() < MIN_SIZE {
        return None;
    }
    let msg_type = u32::from_le_bytes(data[0..4].try_into().ok()?);
    if msg_type != WALLET_SNAPSHOT_TYPE {
        return None;
    }
    let timestamp = i64::from_le_bytes(data[4..12].try_into().ok()?);
    let asset_len = u32::from_le_bytes(data[20..24].try_into().ok()?) as usize;
    let asset_start = 28usize;
    let numbers_start = asset_start.checked_add(asset_len)?;
    if data.len() < numbers_start + 40 {
        return None;
    }
    let asset = std::str::from_utf8(&data[asset_start..numbers_start])
        .ok()?
        .to_ascii_uppercase();
    if asset != "USDT" {
        return None;
    }
    let wallet_balance_usdt = f64::from_le_bytes(
        data[numbers_start + 8..numbers_start + 16]
            .try_into()
            .ok()?,
    );
    let unrealized_pnl_usdt = f64::from_le_bytes(
        data[numbers_start + 16..numbers_start + 24]
            .try_into()
            .ok()?,
    );
    let available_balance_usdt = f64::from_le_bytes(
        data[numbers_start + 24..numbers_start + 32]
            .try_into()
            .ok()?,
    );
    if !wallet_balance_usdt.is_finite() || !unrealized_pnl_usdt.is_finite() {
        return None;
    }
    Some(LiveEquitySnapshot {
        source: "binance_std_um_wallet".to_string(),
        equity_usdt: wallet_balance_usdt + unrealized_pnl_usdt,
        wallet_balance_usdt,
        unrealized_pnl_usdt,
        available_balance_usdt,
        ts_ms: timestamp,
    })
}

pub fn encode_wallet_event(
    timestamp_ms: i64,
    update_time_ms: i64,
    asset: &str,
    margin_available: bool,
    balance: f64,
    cross_wallet_balance: f64,
    cross_un_pnl: f64,
    available_balance: f64,
    max_withdraw_amount: f64,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&WALLET_SNAPSHOT_TYPE.to_le_bytes());
    body.extend_from_slice(&timestamp_ms.to_le_bytes());
    body.extend_from_slice(&update_time_ms.to_le_bytes());
    body.extend_from_slice(&(asset.len() as u32).to_le_bytes());
    body.push(u8::from(margin_available));
    body.extend_from_slice(&[0u8; 3]);
    body.extend_from_slice(asset.as_bytes());
    body.extend_from_slice(&balance.to_le_bytes());
    body.extend_from_slice(&cross_wallet_balance.to_le_bytes());
    body.extend_from_slice(&cross_un_pnl.to_le_bytes());
    body.extend_from_slice(&available_balance.to_le_bytes());
    body.extend_from_slice(&max_withdraw_amount.to_le_bytes());

    let mut wrapped = Vec::new();
    wrapped.extend_from_slice(&WALLET_SNAPSHOT_TYPE.to_le_bytes());
    wrapped.extend_from_slice(&3u32.to_le_bytes());
    wrapped.extend_from_slice(&(body.len() as u32).to_le_bytes());
    wrapped.extend_from_slice(&body);
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_standard_um_wallet_equity() {
        let payload = encode_wallet_event(
            1_700_000_000_000,
            1_700_000_000_100,
            "USDT",
            true,
            12_000.0,
            11_500.0,
            250.5,
            9_000.0,
            8_000.0,
        );
        let snapshot = parse_live_equity(&payload).expect("wallet snapshot");
        assert_eq!(snapshot.source, "binance_std_um_wallet");
        assert!((snapshot.equity_usdt - 11_750.5).abs() < 1e-9);
        assert!((snapshot.wallet_balance_usdt - 11_500.0).abs() < 1e-9);
        assert!((snapshot.unrealized_pnl_usdt - 250.5).abs() < 1e-9);
        assert_eq!(snapshot.ts_ms, 1_700_000_000_000);
    }

    #[test]
    fn ignores_non_usdt_wallet_rows() {
        let payload = encode_wallet_event(1, 1, "BTC", true, 1.0, 1.0, 0.0, 1.0, 1.0);
        assert!(parse_live_equity(&payload).is_none());
    }
}
