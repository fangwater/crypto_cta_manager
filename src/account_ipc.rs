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
const ACCOUNT_RISK_TYPE: u32 = 4007;
const BINANCE_STD_UM_SCOPE: u32 = 3;
const BINANCE_UNIFIED_SCOPE: u32 = 1;
const OKEX_UNIFIED_SCOPE: u32 = 10;
const EVENT_HEADER_LEN: usize = 12;
const ACCOUNT_RISK_ACTUAL_EQUITY_OFFSET: usize = 4 + 8 + 8;
const ACCOUNT_RISK_MARGIN_RATIO_OFFSET: usize = 4 + 8 + 8 * 4;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveEquitySnapshot {
    pub source: String,
    pub equity_usdt: f64,
    pub wallet_balance_usdt: Option<f64>,
    pub unrealized_pnl_usdt: Option<f64>,
    pub available_balance_usdt: Option<f64>,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct WalletAssetSnapshot {
    asset: String,
    wallet_balance_usdt: f64,
    unrealized_pnl_usdt: f64,
    available_balance_usdt: f64,
    ts_ms: i64,
}

#[derive(Default)]
struct WalletAssetAccumulator {
    ts_ms: Option<i64>,
    assets: HashMap<String, WalletAssetSnapshot>,
}

impl WalletAssetAccumulator {
    fn update(&mut self, row: WalletAssetSnapshot) -> Option<LiveEquitySnapshot> {
        match self.ts_ms {
            Some(ts_ms) if row.ts_ms < ts_ms => return None,
            Some(ts_ms) if row.ts_ms > ts_ms => self.assets.clear(),
            _ => {}
        }
        self.ts_ms = Some(row.ts_ms);
        self.assets.insert(row.asset.clone(), row);

        // USDT is emitted for every standard UM wallet snapshot; BFUSD is optional.
        // Wait for it so an earlier BFUSD row cannot publish a partial group.
        if !self.assets.contains_key("USDT") {
            return None;
        }

        let mut wallet_balance_usdt = 0.0;
        let mut unrealized_pnl_usdt = 0.0;
        let mut available_balance_usdt = 0.0;
        for row in self.assets.values() {
            wallet_balance_usdt += row.wallet_balance_usdt;
            unrealized_pnl_usdt += row.unrealized_pnl_usdt;
            available_balance_usdt += row.available_balance_usdt;
        }
        Some(LiveEquitySnapshot {
            source: "binance_std_um_stable_wallets".to_string(),
            equity_usdt: wallet_balance_usdt + unrealized_pnl_usdt,
            wallet_balance_usdt: Some(wallet_balance_usdt),
            unrealized_pnl_usdt: Some(unrealized_pnl_usdt),
            available_balance_usdt: Some(available_balance_usdt),
            ts_ms: self.ts_ms?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveAccountReading {
    pub equity: Option<LiveEquitySnapshot>,
    pub unified_account: bool,
    pub uni_mmr: Option<f64>,
    pub uni_mmr_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct SourceLiveState {
    equity: Option<LiveEquitySnapshot>,
    account_risk_equity: bool,
    unified_account: bool,
    uni_mmr: Option<f64>,
    uni_mmr_ts_ms: Option<i64>,
}

impl SourceLiveState {
    fn apply_wallet_equity(&mut self, snapshot: LiveEquitySnapshot) {
        // Wallet rows cover only stable assets. Once account totals arrive,
        // subsequent wallet rows must not erase BTC/ETH collateral valuation.
        if self.account_risk_equity
            || self
                .equity
                .as_ref()
                .is_some_and(|previous| snapshot.ts_ms < previous.ts_ms)
        {
            return;
        }
        self.equity = Some(snapshot);
    }

    fn apply_account_equity(&mut self, snapshot: LiveEquitySnapshot) {
        if self.account_risk_equity
            && self
                .equity
                .as_ref()
                .is_some_and(|previous| snapshot.ts_ms < previous.ts_ms)
        {
            return;
        }
        self.account_risk_equity = true;
        self.equity = Some(snapshot);
    }

    fn reading(&self) -> Option<LiveAccountReading> {
        if self.equity.is_none() && !self.unified_account {
            return None;
        }
        Some(LiveAccountReading {
            equity: self.equity.clone(),
            unified_account: self.unified_account,
            uni_mmr: self.uni_mmr,
            uni_mmr_ts_ms: self.uni_mmr_ts_ms,
        })
    }

    fn apply_okx_unified_risk(&mut self, ts_ms: i64, uni_mmr: f64) {
        if self.uni_mmr_ts_ms.is_some_and(|previous| ts_ms < previous) {
            return;
        }
        self.unified_account = true;
        self.uni_mmr = Some(uni_mmr);
        self.uni_mmr_ts_ms = Some(ts_ms);
    }
}

#[derive(Clone, Default)]
pub struct LiveEquityHub {
    snapshots: Arc<RwLock<HashMap<String, SourceLiveState>>>,
}

impl LiveEquityHub {
    pub fn spawn(sources: &[SourceConfig]) -> Self {
        let hub = Self::default();
        for source in sources {
            if !source.enabled || !source.monitor_enabled {
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

    pub fn get(&self, source_id: &str) -> Option<LiveAccountReading> {
        self.snapshots
            .read()
            .ok()
            .and_then(|guard| guard.get(source_id).and_then(SourceLiveState::reading))
    }
}

fn subscribe_loop(
    source_id: String,
    service_name: String,
    snapshots: Arc<RwLock<HashMap<String, SourceLiveState>>>,
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
    snapshots: &Arc<RwLock<HashMap<String, SourceLiveState>>>,
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
    let mut wallet_assets = WalletAssetAccumulator::default();
    let rapidx_scope = rapidx_account_scope(service_name);

    loop {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                let payload = sample.payload();
                if let Some(snapshot) =
                    parse_wallet_asset(payload).and_then(|row| wallet_assets.update(row))
                {
                    let equity_usdt = snapshot.equity_usdt;
                    let first = snapshots.read().ok().is_none_or(|guard| {
                        guard
                            .get(source_id)
                            .and_then(|state| state.equity.as_ref())
                            .is_none()
                    });
                    if let Ok(mut guard) = snapshots.write() {
                        guard
                            .entry(source_id.to_string())
                            .or_default()
                            .apply_wallet_equity(snapshot);
                    }
                    if first {
                        info!(
                            source_id,
                            equity_usdt, "account monitor live equity received"
                        );
                    }
                }
                if let Some(snapshot) = rapidx_scope
                    .and_then(|scope| parse_account_equity(payload, scope, "rapidx_account_risk"))
                    .or_else(|| parse_binance_std_um_equity(payload))
                {
                    let equity_usdt = snapshot.equity_usdt;
                    if let Ok(mut guard) = snapshots.write() {
                        let state = guard.entry(source_id.to_string()).or_default();
                        let first = !state.account_risk_equity;
                        state.apply_account_equity(snapshot);
                        if first {
                            info!(
                                source_id,
                                equity_usdt, "account monitor total equity received"
                            );
                        }
                    }
                }
                if let Some((ts_ms, uni_mmr)) = parse_okx_unified_risk(payload) {
                    let first = snapshots.read().ok().is_none_or(|guard| {
                        guard
                            .get(source_id)
                            .and_then(|state| state.uni_mmr)
                            .is_none()
                    });
                    if let Ok(mut guard) = snapshots.write() {
                        guard
                            .entry(source_id.to_string())
                            .or_default()
                            .apply_okx_unified_risk(ts_ms, uni_mmr);
                    }
                    if first {
                        info!(source_id, uni_mmr, "account monitor OKX UniMMR received");
                    }
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => bail!("account IPC receive failed: {error}"),
        }
    }
}

fn parse_wallet_asset(payload: &[u8]) -> Option<WalletAssetSnapshot> {
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

fn parse_okx_unified_risk(payload: &[u8]) -> Option<(i64, f64)> {
    let body = account_risk_body(payload, OKEX_UNIFIED_SCOPE)?;
    if body.len() < ACCOUNT_RISK_MARGIN_RATIO_OFFSET + 8 {
        return None;
    }
    let ts_ms = i64::from_le_bytes(body[4..12].try_into().ok()?);
    let uni_mmr = f64::from_le_bytes(
        body[ACCOUNT_RISK_MARGIN_RATIO_OFFSET..ACCOUNT_RISK_MARGIN_RATIO_OFFSET + 8]
            .try_into()
            .ok()?,
    );
    if ts_ms < 0 || !uni_mmr.is_finite() || uni_mmr < 0.0 {
        return None;
    }
    Some((ts_ms, uni_mmr))
}

fn parse_binance_std_um_equity(payload: &[u8]) -> Option<LiveEquitySnapshot> {
    parse_account_equity(payload, BINANCE_STD_UM_SCOPE, "binance_std_um_account_risk")
}

fn rapidx_account_scope(service_name: &str) -> Option<u32> {
    let (_, service) = service_name.rsplit_once("/account_pubs/")?;
    if service.starts_with("rapidx_binance_") {
        Some(BINANCE_UNIFIED_SCOPE)
    } else if service.starts_with("rapidx_okex_") {
        Some(OKEX_UNIFIED_SCOPE)
    } else {
        None
    }
}

fn parse_account_equity(payload: &[u8], scope: u32, source: &str) -> Option<LiveEquitySnapshot> {
    let body = account_risk_body(payload, scope)?;
    if body.len() < ACCOUNT_RISK_ACTUAL_EQUITY_OFFSET + 8 {
        return None;
    }
    let ts_ms = i64::from_le_bytes(body[4..12].try_into().ok()?);
    let equity_usdt = f64::from_le_bytes(
        body[ACCOUNT_RISK_ACTUAL_EQUITY_OFFSET..ACCOUNT_RISK_ACTUAL_EQUITY_OFFSET + 8]
            .try_into()
            .ok()?,
    );
    if ts_ms < 0 || !equity_usdt.is_finite() {
        return None;
    }
    Some(LiveEquitySnapshot {
        source: source.to_string(),
        equity_usdt,
        // This event carries exchange-valued totalMarginBalance, including
        // Multi-Assets collateral. It has no aggregate wallet/PnL breakdown.
        wallet_balance_usdt: None,
        unrealized_pnl_usdt: None,
        available_balance_usdt: None,
        ts_ms,
    })
}

fn account_risk_body(payload: &[u8], expected_scope: u32) -> Option<&[u8]> {
    if payload.len() < EVENT_HEADER_LEN {
        return None;
    }
    let event_type = u32::from_le_bytes(payload[0..4].try_into().ok()?);
    let scope = u32::from_le_bytes(payload[4..8].try_into().ok()?);
    if event_type != ACCOUNT_RISK_TYPE || scope != expected_scope {
        return None;
    }
    let body_len = u32::from_le_bytes(payload[8..12].try_into().ok()?) as usize;
    let end = EVENT_HEADER_LEN.checked_add(body_len)?;
    if payload.len() < end || body_len < 4 {
        return None;
    }
    let body = &payload[EVENT_HEADER_LEN..end];
    let inner_type = u32::from_le_bytes(body[0..4].try_into().ok()?);
    if inner_type != ACCOUNT_RISK_TYPE {
        return None;
    }
    Some(body)
}

fn parse_wallet_snapshot(data: &[u8]) -> Option<WalletAssetSnapshot> {
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
    if !matches!(asset.as_str(), "USDT" | "BFUSD") {
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
    if !wallet_balance_usdt.is_finite()
        || !unrealized_pnl_usdt.is_finite()
        || !available_balance_usdt.is_finite()
    {
        return None;
    }
    Some(WalletAssetSnapshot {
        asset,
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
        let row = parse_wallet_asset(&payload).expect("wallet snapshot");
        let snapshot = WalletAssetAccumulator::default()
            .update(row)
            .expect("live equity");
        assert_eq!(snapshot.source, "binance_std_um_stable_wallets");
        assert!((snapshot.equity_usdt - 11_750.5).abs() < 1e-9);
        assert_eq!(snapshot.wallet_balance_usdt, Some(11_500.0));
        assert_eq!(snapshot.unrealized_pnl_usdt, Some(250.5));
        assert_eq!(snapshot.ts_ms, 1_700_000_000_000);
    }

    #[test]
    fn aggregates_usdt_and_bfusd_wallet_rows() {
        let usdt = encode_wallet_event(10, 10, "USDT", true, 100.0, 90.0, 5.0, 80.0, 70.0);
        let bfusd = encode_wallet_event(10, 10, "BFUSD", true, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut accumulator = WalletAssetAccumulator::default();
        assert!(
            accumulator
                .update(parse_wallet_asset(&bfusd).expect("BFUSD row"))
                .is_none()
        );
        let snapshot = accumulator
            .update(parse_wallet_asset(&usdt).expect("USDT row"))
            .expect("combined equity");
        assert!((snapshot.equity_usdt - 95.0).abs() < 1e-9);
        assert_eq!(snapshot.wallet_balance_usdt, Some(90.0));
        assert_eq!(snapshot.available_balance_usdt, Some(80.0));
    }

    #[test]
    fn a_new_poll_replaces_previous_asset_rows() {
        let mut accumulator = WalletAssetAccumulator::default();
        let usdt = encode_wallet_event(10, 10, "USDT", true, 100.0, 100.0, 0.0, 100.0, 100.0);
        let bfusd = encode_wallet_event(10, 10, "BFUSD", true, 20.0, 20.0, 0.0, 20.0, 20.0);
        assert_eq!(
            accumulator
                .update(parse_wallet_asset(&usdt).expect("USDT row"))
                .expect("USDT-only equity")
                .equity_usdt,
            100.0
        );
        assert_eq!(
            accumulator
                .update(parse_wallet_asset(&bfusd).expect("BFUSD row"))
                .expect("combined equity")
                .equity_usdt,
            120.0
        );
        let next_bfusd = encode_wallet_event(11, 11, "BFUSD", true, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            accumulator
                .update(parse_wallet_asset(&next_bfusd).expect("next BFUSD row"))
                .is_none()
        );
        let next = encode_wallet_event(11, 11, "USDT", true, 50.0, 50.0, 0.0, 50.0, 50.0);
        let snapshot = accumulator
            .update(parse_wallet_asset(&next).expect("next wallet row"))
            .expect("next equity");
        assert!((snapshot.equity_usdt - 50.0).abs() < 1e-9);
        assert_eq!(snapshot.ts_ms, 11);
        assert!(
            accumulator
                .update(parse_wallet_asset(&bfusd).expect("stale BFUSD row"))
                .is_none()
        );
    }

    #[test]
    fn ignores_non_stable_wallet_rows() {
        let payload = encode_wallet_event(1, 1, "BTC", true, 1.0, 1.0, 0.0, 1.0, 1.0);
        assert!(parse_wallet_asset(&payload).is_none());
    }

    fn encode_account_risk(scope: u32, timestamp_ms: i64, margin_ratio: f64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&ACCOUNT_RISK_TYPE.to_le_bytes());
        body.extend_from_slice(&timestamp_ms.to_le_bytes());
        for value in [0.0, 0.0, 0.0, 0.0, margin_ratio, 0.0, 0.0, f64::NAN] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(&ACCOUNT_RISK_TYPE.to_le_bytes());
        wrapped.extend_from_slice(&scope.to_le_bytes());
        wrapped.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wrapped.extend_from_slice(&body);
        wrapped
    }

    fn encode_um_equity(timestamp_ms: i64, equity: f64) -> Vec<u8> {
        let mut payload = encode_account_risk(BINANCE_STD_UM_SCOPE, timestamp_ms, 0.0);
        let offset = EVENT_HEADER_LEN + ACCOUNT_RISK_ACTUAL_EQUITY_OFFSET;
        payload[offset..offset + 8].copy_from_slice(&equity.to_le_bytes());
        payload
    }

    #[test]
    fn rapidx_total_equity_is_scoped_to_its_account_stream_and_keeps_newest_value() {
        let service = "trade05/account_pubs/rapidx_binance_test_pm";
        let scope = rapidx_account_scope(service).unwrap();
        assert_eq!(scope, BINANCE_UNIFIED_SCOPE);
        assert_eq!(
            rapidx_account_scope("trade02/account_pubs/binance_pm"),
            None
        );
        assert_eq!(
            rapidx_account_scope("trade05/account_pubs/rapidx_okex_test_pm"),
            Some(OKEX_UNIFIED_SCOPE)
        );
        let risk = |timestamp, equity| {
            let mut payload = encode_account_risk(scope, timestamp, 123.0);
            let offset = EVENT_HEADER_LEN + ACCOUNT_RISK_ACTUAL_EQUITY_OFFSET;
            payload[offset..offset + 8].copy_from_slice(&f64::to_le_bytes(equity));
            payload
        };
        let mut state = SourceLiveState::default();
        for (timestamp, equity) in [(20, 9994.0), (30, 9995.0), (10, 9000.0)] {
            state.apply_account_equity(
                parse_account_equity(&risk(timestamp, equity), scope, "rapidx_account_risk")
                    .unwrap(),
            );
        }
        let reading = state.reading().unwrap();
        let snapshot = reading.equity.unwrap();
        assert_eq!(snapshot.equity_usdt, 9995.0);
        assert_eq!(snapshot.ts_ms, 30);
        assert_eq!(snapshot.source, "rapidx_account_risk");
        assert!(snapshot.wallet_balance_usdt.is_none());
        assert!(parse_account_equity(&risk(40, f64::NAN), scope, "rapidx_account_risk").is_none());
        assert!(
            parse_account_equity(&encode_um_equity(40, 9995.0), scope, "rapidx_account_risk")
                .is_none()
        );
        let payload = risk(40, 9995.0);
        for len in 0..payload.len() {
            assert!(parse_account_equity(&payload[..len], scope, "rapidx_account_risk").is_none());
        }
    }

    #[test]
    fn account_total_keeps_multi_asset_collateral_after_newer_wallet_rows() {
        let wallet = |timestamp_ms| {
            let payload = encode_wallet_event(
                timestamp_ms,
                timestamp_ms,
                "USDT",
                true,
                288_000.0,
                288_000.0,
                -2_000.0,
                250_000.0,
                250_000.0,
            );
            WalletAssetAccumulator::default()
                .update(parse_wallet_asset(&payload).unwrap())
                .unwrap()
        };
        let risk = |timestamp_ms, equity| {
            parse_binance_std_um_equity(&encode_um_equity(timestamp_ms, equity)).unwrap()
        };
        let mut state = SourceLiveState::default();
        state.apply_wallet_equity(wallet(20));
        assert_eq!(
            state.reading().unwrap().equity.unwrap().equity_usdt,
            286_000.0
        );

        // Account totals take precedence even if the first wallet row is newer.
        state.apply_account_equity(risk(19, 830_000.0));
        state.apply_wallet_equity(wallet(19));
        state.apply_wallet_equity(wallet(25));
        state.apply_account_equity(risk(30, 831_000.0));
        state.apply_account_equity(risk(29, 829_000.0));
        let reading = state.reading().unwrap();
        assert!(!reading.unified_account);
        assert_eq!(reading.uni_mmr, None);
        let snapshot = reading.equity.unwrap();
        assert_eq!(snapshot.equity_usdt, 831_000.0);
        assert_eq!(snapshot.ts_ms, 30);
        assert_eq!(snapshot.source, "binance_std_um_account_risk");
        let json = serde_json::to_value(snapshot).unwrap();
        for field in [
            "wallet_balance_usdt",
            "unrealized_pnl_usdt",
            "available_balance_usdt",
        ] {
            assert!(json[field].is_null());
        }
    }

    #[test]
    fn rejects_invalid_account_equity_without_affecting_other_scopes() {
        assert!(parse_binance_std_um_equity(&encode_account_risk(4, 10, 1.0)).is_none());
        assert!(parse_binance_std_um_equity(&encode_account_risk(10, 10, 1.0)).is_none());
        assert!(parse_okx_unified_risk(&encode_um_equity(10, 830_000.0)).is_none());
        for equity in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(parse_binance_std_um_equity(&encode_um_equity(10, equity)).is_none());
        }
        assert!(parse_binance_std_um_equity(&encode_um_equity(-1, 100.0)).is_none());
        assert_eq!(
            parse_binance_std_um_equity(&encode_um_equity(10, -100.0))
                .unwrap()
                .equity_usdt,
            -100.0
        );
        let payload = encode_um_equity(10, 830_000.0);
        for len in 0..payload.len() {
            assert!(parse_binance_std_um_equity(&payload[..len]).is_none());
        }
        let mut wrong_outer_type = payload.clone();
        wrong_outer_type[0..4].copy_from_slice(&WALLET_SNAPSHOT_TYPE.to_le_bytes());
        assert!(parse_binance_std_um_equity(&wrong_outer_type).is_none());
        let mut wrong_inner_type = payload.clone();
        wrong_inner_type[12..16].copy_from_slice(&WALLET_SNAPSHOT_TYPE.to_le_bytes());
        assert!(parse_binance_std_um_equity(&wrong_inner_type).is_none());
        for body_len in [0u32, 3, 27, u32::MAX] {
            let mut invalid_length = payload.clone();
            invalid_length[8..12].copy_from_slice(&body_len.to_le_bytes());
            assert!(parse_binance_std_um_equity(&invalid_length).is_none());
        }
    }

    #[test]
    fn decodes_okx_unified_unimmr_and_keeps_the_newer_sample() {
        let payload = encode_account_risk(OKEX_UNIFIED_SCOPE, 1_700_000_000_000, 66.7489666667);
        let (ts_ms, uni_mmr) = parse_okx_unified_risk(&payload).expect("okx risk");
        assert_eq!(ts_ms, 1_700_000_000_000);
        assert!((uni_mmr - 66.7489666667).abs() < 1e-12);
        assert!(parse_okx_unified_risk(&encode_account_risk(1, 1, 2.0)).is_none());

        let mut state = SourceLiveState::default();
        state.apply_okx_unified_risk(20, 4.5);
        state.apply_okx_unified_risk(10, 1.1);
        let reading = state.reading().expect("reading");
        assert!(reading.unified_account);
        assert_eq!(reading.uni_mmr, Some(4.5));
        assert!(reading.equity.is_none());
    }
}
