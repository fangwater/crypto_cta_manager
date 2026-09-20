use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use hmac::{Hmac, Mac};
use iceoryx2::prelude::*;
use iceoryx2::service::ipc;
use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use sha2::Sha256;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::config::{AppConfig, DingTalkConfig, MonitorConfig, SourceConfig};
use crate::model::{
    ORDER_UPDATES_CF, TRADE_UPDATES_CF, UniformOrderEvent, decode_order_update,
    decode_trade_update, decode_uniform_order,
};
use crate::redis_runtime::RedisRuntime;
use crate::rocks_source::{RawRocksRecord, read_latest_column_families};
use crate::twap::parse_ask_bid_spread;
use crate::viz_snapshot::{ExecStateSnapshot, VizSnapshotClient};

const RECENT_COLUMN_FAMILIES: [&str; 3] = ["uniform_orders", ORDER_UPDATES_CF, TRADE_UPDATES_CF];
const MARKET_PAYLOAD_BYTES: usize = 128;
const MARKET_HISTORY_SIZE: usize = 100;
const MARKET_MAX_SUBSCRIBERS: usize = 64;
const MARKET_SUBSCRIBER_BUFFER: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorIssue {
    pub key: String,
    pub source_id: String,
    pub category: String,
    pub message: String,
    /// Delay the first notification while a transient condition settles.
    pub initial_delay_secs: u64,
}

impl MonitorIssue {
    fn new(category: &str, scope: &str, source_id: &str, message: impl Into<String>) -> Self {
        Self {
            key: format!("{category}:{scope}:{source_id}"),
            source_id: source_id.to_string(),
            category: category.to_string(),
            message: message.into(),
            initial_delay_secs: 0,
        }
    }

    fn with_initial_delay(mut self, seconds: u64) -> Self {
        self.initial_delay_secs = seconds;
        self
    }
}

/// Issue scoped to one configured source; the account alias is prepended to
/// the message so pushed alerts identify which account produced them.
fn issue(
    category: &str,
    scope: &str,
    source: &SourceConfig,
    message: impl Into<String>,
) -> MonitorIssue {
    let label = source
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let account = source.account.trim();
            (!account.is_empty()).then_some(account)
        })
        .unwrap_or(source.id.as_str());
    MonitorIssue::new(
        category,
        scope,
        &source.id,
        format!("[{label}] {}", message.into()),
    )
}

#[derive(Debug, Default)]
struct MarketFeedState {
    /// Latest local receive time per (venue, symbol).
    latest: BTreeMap<(String, String), i64>,
    last_any_by_venue: BTreeMap<String, i64>,
}

#[derive(Clone, Default)]
pub struct MarketFeed {
    state: Arc<RwLock<MarketFeedState>>,
}

impl MarketFeed {
    pub fn spawn<I, S>(venues: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let feed = Self::default();
        let mut seen = HashSet::new();
        for venue in venues.into_iter().map(Into::into) {
            let venue = venue.trim().to_string();
            if venue.is_empty() || !seen.insert(venue.clone()) {
                continue;
            }
            let state = Arc::clone(&feed.state);
            thread::Builder::new()
                .name(format!(
                    "cta-monitor-market-{}",
                    sanitize_thread_name(&venue)
                ))
                .spawn(move || market_subscription_loop(venue, state))
                .expect("failed to spawn CTA monitor market subscriber");
        }
        feed
    }

    fn last_any(&self, venue: &str) -> Option<i64> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.last_any_by_venue.get(venue).copied())
    }

    /// Latest local receive time across the watched symbols on one venue.
    fn latest_received(&self, venue: &str, symbols: &[String]) -> Option<i64> {
        self.state.read().ok().and_then(|state| {
            symbols
                .iter()
                .filter_map(|symbol| {
                    state
                        .latest
                        .get(&(venue.to_string(), symbol.clone()))
                        .copied()
                })
                .max()
        })
    }
}

fn market_subscription_loop(venue: String, state: Arc<RwLock<MarketFeedState>>) {
    loop {
        if let Err(error) = run_market_subscription(&venue, &state) {
            warn!(venue = %venue, error = %error, "CTA monitor market subscriber stopped; retrying");
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn run_market_subscription(venue: &str, state: &Arc<RwLock<MarketFeedState>>) -> Result<()> {
    let node_name = format!("cta_monitor_market_{}", sanitize_thread_name(venue));
    let node = NodeBuilder::new()
        .name(&NodeName::new(&node_name)?)
        .create::<ipc::Service>()
        .with_context(|| format!("failed to create market monitor node {node_name}"))?;
    let service_name = format!("spread_pbs/{venue}/ask_bid_spread");
    let service = loop {
        match node
            .service_builder(&ServiceName::new(&service_name)?)
            .publish_subscribe::<[u8; MARKET_PAYLOAD_BYTES]>()
            .max_publishers(1)
            .max_subscribers(MARKET_MAX_SUBSCRIBERS)
            .history_size(MARKET_HISTORY_SIZE)
            .subscriber_max_buffer_size(MARKET_SUBSCRIBER_BUFFER)
            .open()
        {
            Ok(service) => break service,
            Err(error) => {
                warn!(
                    venue,
                    service_name,
                    error = ?error,
                    "waiting for market BBO IPC service"
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let subscriber = service
        .subscriber_builder()
        .buffer_size(MARKET_SUBSCRIBER_BUFFER)
        .create()
        .with_context(|| format!("failed to subscribe to {service_name}"))?;
    info!(venue, service_name, "CTA monitor market BBO subscribed");

    loop {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                let Some(quote) = parse_ask_bid_spread(sample.payload()) else {
                    continue;
                };
                let received_ts_us = unix_time_us();
                if let Ok(mut guard) = state.write() {
                    guard
                        .latest
                        .insert((venue.to_string(), quote.symbol), received_ts_us);
                    guard
                        .last_any_by_venue
                        .insert(venue.to_string(), received_ts_us);
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => bail!("market BBO receive failed: {error}"),
        }
    }
}

pub async fn run(config: AppConfig, once: bool, dry_run: bool) -> Result<()> {
    if !config.monitor.enabled && !dry_run {
        info!("CTA monitor is disabled in configuration");
        return Ok(());
    }
    let enabled_sources = config
        .sources
        .iter()
        .filter(|source| source.enabled)
        .cloned()
        .collect::<Vec<_>>();
    let venues = enabled_sources
        .iter()
        .map(|source| source.venue.clone())
        .collect::<Vec<_>>();
    let market = MarketFeed::spawn(venues);
    let viz = VizSnapshotClient::new(config.order_config.request_timeout_secs)?;
    let redis = RedisRuntime::connect(config.redis.clone())?;
    redis.spawn_keepalive();
    // Give the BBO subscription one stale window to deliver before judging
    // freshness; the first poll would otherwise always report a market outage.
    let market_warmup = Duration::from_secs(config.monitor.market_stale_secs);
    tokio::select! {
        _ = tokio::time::sleep(market_warmup) => {}
        result = tokio::signal::ctrl_c() => {
            result.context("failed to wait for shutdown signal")?;
            return Ok(());
        }
    }
    let senders = (!dry_run)
        .then(|| DingTalkSenders::from_config(&config.monitor.dingtalk, &config.monitor.host_tag))
        .transpose()?;
    let mut tracker = AlertTracker::default();
    let mut heartbeat = [
        (
            NoticeChannel::Market,
            HeartbeatSchedule::new(config.monitor.market_heartbeat_hours, shanghai_now_secs()),
        ),
        (
            NoticeChannel::Order,
            HeartbeatSchedule::new(config.monitor.order_heartbeat_hours, shanghai_now_secs()),
        ),
    ];
    let poll_interval = Duration::from_secs(config.monitor.poll_interval_secs);

    loop {
        let issues = check_once(&config, &enabled_sources, &market, &viz, &redis).await;
        if dry_run {
            print_dry_run(&issues);
        } else {
            let pending =
                tracker.pending(&issues, Instant::now(), config.monitor.repeat_alert_secs);
            if let Some(senders) = &senders {
                for channel in [NoticeChannel::Market, NoticeChannel::Order] {
                    let channel_pending = pending
                        .iter()
                        .filter(|notice| notice.channel() == channel)
                        .cloned()
                        .collect::<Vec<_>>();
                    if channel_pending.is_empty() {
                        continue;
                    }
                    let sender = match channel {
                        NoticeChannel::Market => &senders.market,
                        NoticeChannel::Order => &senders.order,
                    };
                    match sender
                        .send_with_retry(
                            &channel_pending,
                            config.monitor.dingtalk.retry_attempts,
                            config.monitor.dingtalk.retry_backoff_ms,
                        )
                        .await
                    {
                        Ok(()) => tracker.mark_sent(&channel_pending, Instant::now()),
                        Err(error) => warn!(
                            channel = channel.as_str(),
                            error = %error,
                            "CTA monitor DingTalk send failed after retries"
                        ),
                    }
                }
                let shanghai_secs = shanghai_now_secs();
                let quiet = in_quiet_window(
                    shanghai_hour_of_day(shanghai_secs),
                    config.monitor.heartbeat_quiet_start_hour,
                    config.monitor.heartbeat_quiet_end_hour,
                );
                for (channel, schedule) in &mut heartbeat {
                    if !schedule.due(shanghai_secs, quiet) {
                        continue;
                    }
                    let sender = match channel {
                        NoticeChannel::Market => &senders.market,
                        NoticeChannel::Order => &senders.order,
                    };
                    let message = heartbeat_message(*channel, tracker.active_count(*channel));
                    match sender
                        .send_heartbeat(
                            &message,
                            config.monitor.dingtalk.retry_attempts,
                            config.monitor.dingtalk.retry_backoff_ms,
                        )
                        .await
                    {
                        Ok(()) => info!(channel = channel.as_str(), "CTA monitor heartbeat sent"),
                        Err(error) => warn!(
                            channel = channel.as_str(),
                            error = %error,
                            "CTA monitor DingTalk heartbeat failed after retries"
                        ),
                    }
                }
            }
        }

        if once {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            result = tokio::signal::ctrl_c() => {
                result.context("failed to wait for shutdown signal")?;
                info!("CTA monitor shutdown requested");
                return Ok(());
            }
        }
    }
}

async fn check_once(
    config: &AppConfig,
    sources: &[SourceConfig],
    market: &MarketFeed,
    viz: &VizSnapshotClient,
    redis: &RedisRuntime,
) -> Vec<MonitorIssue> {
    let now_us = unix_time_us();
    let mut issues = check_market(config, market, now_us);
    let mut tasks = JoinSet::new();
    for source in sources {
        let source = source.clone();
        let monitor = config.monitor.clone();
        let viz = viz.clone();
        let redis = redis.clone();
        tasks.spawn(async move { check_source(source, monitor, viz, redis, now_us).await });
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(source_issues) => issues.extend(source_issues),
            Err(error) => issues.push(MonitorIssue::new(
                "monitor",
                "worker",
                "manager",
                format!("source monitor task failed: {error}"),
            )),
        }
    }
    issues.sort_by(|left, right| left.key.cmp(&right.key));
    issues
}

fn check_market(config: &AppConfig, market: &MarketFeed, now_us: i64) -> Vec<MonitorIssue> {
    let venues = config
        .sources
        .iter()
        .filter(|source| source.enabled)
        .map(|source| source.venue.trim())
        .filter(|venue| !venue.is_empty())
        .collect::<HashSet<_>>();
    let symbols = config
        .monitor
        .market_symbols
        .iter()
        .map(|symbol| normalize_symbol(symbol))
        .filter(|symbol| !symbol.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let stale_us = seconds_to_us(config.monitor.market_stale_secs);
    let mut issues = Vec::new();
    for venue in venues {
        if symbols.is_empty() {
            let age = market
                .last_any(venue)
                .map(|received| now_us.saturating_sub(received));
            if age.is_none_or(|age| age > stale_us) {
                issues.push(MonitorIssue::new(
                    "market",
                    &format!("{venue}:any"),
                    "global",
                    format!(
                        "行情 BBO 在 {venue} 已超过 {} 秒没有新消息",
                        config.monitor.market_stale_secs
                    ),
                ));
            }
            continue;
        }
        // Single liveness check across the watched symbols: any fresh BBO from
        // the watched set means the feed is healthy, regardless of which
        // symbol delivered it.
        let latest = market.latest_received(venue, &symbols);
        if latest.is_none_or(|received| now_us.saturating_sub(received) > stale_us) {
            issues.push(MonitorIssue::new(
                "market",
                &format!("{venue}:bbo"),
                "global",
                format!(
                    "行情故障: {venue} 已超过 {} 秒没有新 BBO",
                    config.monitor.market_stale_secs
                ),
            ));
        }
    }
    issues
}

async fn check_source(
    source: SourceConfig,
    monitor: MonitorConfig,
    viz: VizSnapshotClient,
    redis: RedisRuntime,
    now_us: i64,
) -> Vec<MonitorIssue> {
    let mut issues = Vec::new();
    let source_id = source.id.clone();
    let path = source.rocksdb_path.clone();
    let limit = monitor.recent_order_records;
    let records = match tokio::task::spawn_blocking(move || {
        read_latest_column_families(&path, &RECENT_COLUMN_FAMILIES, limit)
    })
    .await
    {
        Ok(Ok(records)) => records,
        Ok(Err(error)) => {
            issues.push(issue(
                "orders",
                "rocksdb",
                &source,
                format!("订单 RocksDB 只读检查失败: {error:#}"),
            ));
            BTreeMap::new()
        }
        Err(error) => {
            issues.push(issue(
                "orders",
                "rocksdb-worker",
                &source,
                format!("订单 RocksDB 检查任务失败: {error}"),
            ));
            BTreeMap::new()
        }
    };
    if !records.contains_key("uniform_orders") {
        issues.push(issue(
            "orders",
            "uniform-orders-cf",
            &source,
            "订单 RocksDB 缺少 uniform_orders column family",
        ));
    } else {
        issues.extend(check_orders(&source, &monitor, &records, now_us));
    }

    match source.exec_viz_origin() {
        None => issues.push(issue(
            "position",
            "endpoint",
            &source,
            "未配置 Exec Viz snapshot 地址，无法确认仓位执行状态",
        )),
        Some(origin) => {
            // Configured positions come from the Redis Exec ledgers; a
            // failed read must skip the account comparison, not compare
            // against an empty map.
            let configured = match redis.load_exec_targets(&source).await {
                Ok(targets) => Some(targets),
                Err(error) => {
                    issues.push(issue(
                        "position",
                        "redis-targets",
                        &source,
                        format!("Redis 配置仓位读取失败，无法核对账户仓位: {error:#}"),
                    ));
                    None
                }
            };
            match viz.load_exec_state(&source_id, origin).await {
                Ok(snapshot) => issues.extend(check_position(
                    &source,
                    &monitor,
                    &snapshot,
                    configured.as_ref(),
                    now_us,
                )),
                Err(error) => issues.push(issue(
                    "position",
                    "snapshot",
                    &source,
                    format!("Exec Viz snapshot 读取失败: {error:#}"),
                )),
            }
        }
    }
    issues
}

fn check_orders(
    source: &SourceConfig,
    monitor: &MonitorConfig,
    records: &BTreeMap<String, Vec<RawRocksRecord>>,
    now_us: i64,
) -> Vec<MonitorIssue> {
    let mut issues = Vec::new();
    let mut latest_orders = HashMap::<i64, UniformOrderEvent>::new();
    let mut order_activity = HashMap::<i64, i64>::new();
    let mut decode_failures = 0usize;
    for record in records.get("uniform_orders").into_iter().flatten() {
        match decode_uniform_order(&record.key, &record.value) {
            Ok(event) => {
                let activity = [event.event_ts_us, event.recv_ts_us, event.update_ts_us]
                    .into_iter()
                    .max()
                    .unwrap_or(event.event_ts_us);
                order_activity
                    .entry(event.client_order_id)
                    .and_modify(|value| *value = (*value).max(activity))
                    .or_insert(activity);
                latest_orders
                    .entry(event.client_order_id)
                    .and_modify(|previous| {
                        if event.event_ts_us > previous.event_ts_us {
                            *previous = event.clone();
                        }
                    })
                    .or_insert(event);
            }
            Err(_) => decode_failures += 1,
        }
    }
    for record in records.get(ORDER_UPDATES_CF).into_iter().flatten() {
        match decode_order_update(&record.key, &record.value) {
            Ok(event) => {
                order_activity
                    .entry(event.client_order_id)
                    .and_modify(|value| *value = (*value).max(event.event_ts_us))
                    .or_insert(event.event_ts_us);
            }
            Err(_) => decode_failures += 1,
        }
    }
    for record in records.get(TRADE_UPDATES_CF).into_iter().flatten() {
        match decode_trade_update(&record.key, &record.value) {
            Ok(event) => {
                order_activity
                    .entry(event.client_order_id)
                    .and_modify(|value| *value = (*value).max(event.event_ts_us))
                    .or_insert(event.event_ts_us);
            }
            Err(_) => decode_failures += 1,
        }
    }
    if decode_failures > 0 {
        issues.push(issue(
            "orders",
            "decode",
            source,
            format!("订单事件最近窗口有 {decode_failures} 条无法解码记录"),
        ));
    }

    for event in latest_orders.values() {
        if is_terminal_order_status(&event.status) {
            continue;
        }
        if event.status.starts_with("UNKNOWN(") {
            issues.push(issue(
                "orders",
                &format!("unknown-status:{}", event.client_order_id),
                source,
                format!(
                    "订单 {} {} 状态无法识别: {}",
                    event.client_order_id, event.symbol, event.status
                ),
            ));
        }
        let activity = order_activity
            .get(&event.client_order_id)
            .copied()
            .unwrap_or(event.event_ts_us);
        let age_us = now_us.saturating_sub(activity);
        if age_us > seconds_to_us(monitor.order_stale_secs) {
            issues.push(issue(
                "orders",
                &format!("stale:{}", event.client_order_id),
                source,
                format!(
                    "订单 {} {} 处于 {} 状态，最近活动已停止约 {} 秒",
                    event.client_order_id,
                    event.symbol,
                    event.status,
                    age_us / 1_000_000
                ),
            ));
        }
    }

    issues
}

fn check_position(
    source: &SourceConfig,
    monitor: &MonitorConfig,
    snapshot: &ExecStateSnapshot,
    configured: Option<&BTreeMap<String, f64>>,
    now_us: i64,
) -> Vec<MonitorIssue> {
    let mut issues = Vec::new();
    if snapshot.source_id != source.id {
        issues.push(issue(
            "position",
            "source-id",
            source,
            format!("Exec Viz snapshot source_id 不匹配: {}", snapshot.source_id),
        ));
    }
    let now_ms = now_us / 1_000;
    let age_ms = now_ms.saturating_sub(snapshot.snapshot_ts_ms);
    if snapshot.snapshot_ts_ms <= 0 || age_ms > monitor.position_stale_secs as i64 * 1_000 {
        issues.push(issue(
            "position",
            "stale",
            source,
            format!(
                "Exec pre-trade 仓位快照已超过 {} 秒没有更新",
                monitor.position_stale_secs
            ),
        ));
    }
    if !snapshot.position_ready {
        issues.push(issue(
            "position",
            "not-ready",
            source,
            "Exec pre-trade position_ready=false，仓位尚未准备好",
        ));
    }

    let mut account_qty_by_symbol = HashMap::<&str, f64>::new();
    let mut inflight_sum = HashMap::<&str, f64>::new();
    let mut symbol_last_update = HashMap::<&str, i64>::new();
    let mut usdt_value_by_symbol = HashMap::<&str, f64>::new();
    let mut valued_qty_by_symbol = HashMap::<&str, f64>::new();
    for row in &snapshot.rows {
        if let Some(account_qty) = row.account_position_qty {
            if let Some(previous) = account_qty_by_symbol.get(row.symbol.as_str())
                && (*previous - account_qty).abs() > monitor.position_tolerance
            {
                issues.push(issue(
                    "position",
                    &format!("account-qty:{}", row.symbol),
                    source,
                    format!("{} 的快照行包含不一致的 account_position_qty", row.symbol),
                ));
            } else {
                account_qty_by_symbol.insert(row.symbol.as_str(), account_qty);
            }
        }
        *inflight_sum.entry(row.symbol.as_str()).or_insert(0.0) +=
            row.pending_qty.unwrap_or(0.0) + row.live_order_qty.unwrap_or(0.0);
        // Rows with both qty and USDT value price the symbol's position gaps.
        if let (Some(qty), Some(usdt)) = (row.current_qty, row.current_usdt) {
            *usdt_value_by_symbol
                .entry(row.symbol.as_str())
                .or_insert(0.0) += usdt.abs();
            *valued_qty_by_symbol
                .entry(row.symbol.as_str())
                .or_insert(0.0) += qty.abs();
        }
        symbol_last_update
            .entry(row.symbol.as_str())
            .and_modify(|ts| *ts = (*ts).max(row.source_updated_at_ms))
            .or_insert(row.source_updated_at_ms);
    }

    // Configured positions (Redis BatchExec targets) vs the factual account
    // position (Exec Viz): alert only when the gap has no live order quantity
    // working on it and the symbol has been quiet past the execution grace
    // window. None means the Redis read failed and the check is skipped.
    let grace_ms = monitor.execution_grace_secs as i64 * 1_000;
    if let Some(configured) = configured {
        let mut symbols = BTreeSet::new();
        symbols.extend(account_qty_by_symbol.keys().copied());
        symbols.extend(
            configured
                .iter()
                .filter(|(_, qty)| qty.abs() > monitor.position_tolerance)
                .map(|(symbol, _)| symbol.as_str()),
        );
        for symbol in symbols {
            let configured_qty = configured.get(symbol).copied().unwrap_or(0.0);
            let Some(account_qty) = account_qty_by_symbol.get(symbol).copied() else {
                issues.push(
                    issue(
                        "position",
                        &format!("account-missing:{symbol}"),
                        source,
                        format!(
                            "{symbol} Redis 已配置仓位 {configured_qty:.12}，但 Exec Viz 快照暂未提供账户仓位"
                        ),
                    )
                    .with_initial_delay(monitor.execution_grace_secs),
                );
                continue;
            };
            let scale = configured_qty.abs().max(account_qty.abs()).max(1.0);
            let tolerance = monitor.position_tolerance * scale;
            let gap = configured_qty - account_qty;
            if gap.abs() <= tolerance {
                continue;
            }
            // Dust-valued gaps are residuals, not mismatches.
            let implied_price = valued_qty_by_symbol
                .get(symbol)
                .filter(|qty| **qty > f64::EPSILON)
                .map(|qty| usdt_value_by_symbol.get(symbol).copied().unwrap_or(0.0) / *qty);
            if implied_price
                .is_some_and(|price| gap.abs() * price <= monitor.position_residual_usdt)
            {
                continue;
            }
            let inflight = inflight_sum.get(symbol).copied().unwrap_or(0.0);
            if inflight.abs() > tolerance {
                continue;
            }
            let settling = symbol_last_update
                .get(symbol)
                .is_some_and(|ts| *ts > 0 && now_ms.saturating_sub(*ts) <= grace_ms);
            if settling {
                continue;
            }
            issues.push(
                issue(
                    "position",
                    &format!("account:{symbol}"),
                    source,
                    format!(
                        "{symbol} 配置仓位 {configured_qty:.12} 与账户仓位 {account_qty:.12} 不一致且无挂单执行"
                    ),
                )
                .with_initial_delay(monitor.execution_grace_secs),
            );
        }
    }
    issues
}

fn is_terminal_order_status(status: &str) -> bool {
    matches!(
        status,
        "FILLED" | "CANCELED" | "EXPIRED" | "EXPIRED_IN_MATCH"
    )
}

fn normalize_symbol(raw: &str) -> String {
    raw.chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_uppercase)
        .collect()
}

fn sanitize_thread_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn seconds_to_us(seconds: u64) -> i64 {
    seconds.saturating_mul(1_000_000).min(i64::MAX as u64) as i64
}

fn unix_time_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
struct PendingNotice {
    key: String,
    message: String,
    recovery: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeChannel {
    Market,
    Order,
}

impl NoticeChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Market => "market",
            Self::Order => "order",
        }
    }
}

fn channel_for_key(key: &str) -> NoticeChannel {
    if key.starts_with("market:") {
        NoticeChannel::Market
    } else {
        NoticeChannel::Order
    }
}

impl PendingNotice {
    fn channel(&self) -> NoticeChannel {
        channel_for_key(&self.key)
    }
}

/// Minimum gap between repeated alerts for the same unresolved issue.
const MIN_REPEAT_ALERT_SECS: u64 = 30;

#[derive(Debug, Default)]
struct AlertState {
    active: bool,
    visible: bool,
    first_seen: Option<Instant>,
    last_sent: Option<Instant>,
    recovery_pending: bool,
    /// Message of the last alert sent, reused for the recovery notice.
    message: String,
}

#[derive(Debug, Default)]
struct AlertTracker {
    states: HashMap<String, AlertState>,
}

impl AlertTracker {
    fn pending(
        &mut self,
        issues: &[MonitorIssue],
        now: Instant,
        repeat_alert_secs: u64,
    ) -> Vec<PendingNotice> {
        let current_keys = issues
            .iter()
            .map(|issue| issue.key.as_str())
            .collect::<HashSet<_>>();
        let repeat = Duration::from_secs(repeat_alert_secs.max(MIN_REPEAT_ALERT_SECS));
        let mut pending = Vec::new();
        for issue in issues {
            let state = self.states.entry(issue.key.clone()).or_default();
            if !state.active {
                state.first_seen = Some(now);
            }
            state.active = true;
            state.recovery_pending = false;
            state.visible = state.first_seen.is_none_or(|first_seen| {
                now.duration_since(first_seen) >= Duration::from_secs(issue.initial_delay_secs)
            });
            if !state.visible {
                continue;
            }
            let due = state
                .last_sent
                .is_none_or(|last_sent| now.duration_since(last_sent) >= repeat);
            if due {
                pending.push(PendingNotice {
                    key: issue.key.clone(),
                    message: issue.message.clone(),
                    recovery: false,
                });
            }
        }
        for (key, state) in &mut self.states {
            if !state.active || current_keys.contains(key.as_str()) {
                continue;
            }
            if state.last_sent.is_none() {
                *state = AlertState::default();
                continue;
            }
            if !state.recovery_pending {
                let detail = if state.message.is_empty() {
                    key.clone()
                } else {
                    state.message.clone()
                };
                pending.push(PendingNotice {
                    key: key.clone(),
                    message: format!("问题已恢复: {detail}"),
                    recovery: true,
                });
            }
        }
        pending
    }

    fn mark_sent(&mut self, notices: &[PendingNotice], now: Instant) {
        for notice in notices {
            let state = self.states.entry(notice.key.clone()).or_default();
            if notice.recovery {
                state.active = false;
                state.visible = false;
                state.first_seen = None;
                state.last_sent = None;
                state.recovery_pending = false;
                state.message.clear();
            } else {
                state.active = true;
                state.last_sent = Some(now);
                state.recovery_pending = false;
                state.message = notice.message.clone();
            }
        }
    }

    /// Issues currently unresolved on one DingTalk channel.
    fn active_count(&self, channel: NoticeChannel) -> usize {
        self.states
            .iter()
            .filter(|(key, state)| state.active && state.visible && channel_for_key(key) == channel)
            .count()
    }
}

/// Fixed UTC+8 offset for Shanghai wall-clock time; China has no DST.
const SHANGHAI_OFFSET_SECS: i64 = 8 * 3_600;

/// Current Unix time shifted into the Shanghai zone.
fn shanghai_now_secs() -> i64 {
    shanghai_secs(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or_default(),
    )
}

fn shanghai_secs(unix_secs: i64) -> i64 {
    unix_secs + SHANGHAI_OFFSET_SECS
}

fn shanghai_hour_of_day(shanghai_secs: i64) -> u32 {
    (shanghai_secs.rem_euclid(86_400) / 3_600) as u32
}

/// Shanghai-hour quiet window [start, end); a wrapped range covers an
/// overnight window and equal bounds disable it.
fn in_quiet_window(hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        return false;
    }
    if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

fn heartbeat_slot(shanghai_secs: i64, interval_hours: u64) -> u64 {
    if interval_hours == 0 {
        return 0;
    }
    (shanghai_secs.div_euclid(3_600).max(0) as u64) / interval_hours
}

/// One channel's heartbeat cadence aligned to Shanghai wall-clock slots.
struct HeartbeatSchedule {
    interval_hours: u64,
    /// Slot index already handled. Initialized to the current slot so a
    /// (re)start never back-fills a beat mid-slot.
    last_slot: u64,
}

impl HeartbeatSchedule {
    fn new(interval_hours: u64, shanghai_secs: i64) -> Self {
        Self {
            interval_hours,
            last_slot: heartbeat_slot(shanghai_secs, interval_hours),
        }
    }

    /// True once when a new heartbeat slot opens. The slot is consumed even in
    /// the quiet window or after a failed send so heartbeats stay aligned and
    /// never retry-storm; the next beat lands on the next boundary.
    fn due(&mut self, shanghai_secs: i64, quiet: bool) -> bool {
        if self.interval_hours == 0 {
            return false;
        }
        let slot = heartbeat_slot(shanghai_secs, self.interval_hours);
        if slot == self.last_slot {
            return false;
        }
        self.last_slot = slot;
        !quiet
    }
}

fn heartbeat_message(channel: NoticeChannel, active_issues: usize) -> String {
    let label = match channel {
        NoticeChannel::Market => "行情监控",
        NoticeChannel::Order => "交易监控",
    };
    if active_issues == 0 {
        format!("{label}运行正常，无未恢复告警")
    } else {
        format!("{label}运行中，{active_issues} 条告警未恢复")
    }
}

struct DingTalkSenders {
    market: DingTalkSender,
    order: DingTalkSender,
}

impl DingTalkSenders {
    fn from_config(config: &DingTalkConfig, host_tag: &str) -> Result<Self> {
        Ok(Self {
            market: DingTalkSender::from_env(
                &config.market_webhook_url_env,
                config.market_secret_env.as_deref(),
                config.request_timeout_secs,
                config.at_mobiles.clone(),
                config.is_at_all,
                host_tag,
            )?,
            order: DingTalkSender::from_env(
                &config.order_webhook_url_env,
                config.order_secret_env.as_deref(),
                config.request_timeout_secs,
                config.at_mobiles.clone(),
                config.is_at_all,
                host_tag,
            )?,
        })
    }
}

struct DingTalkSender {
    client: Client,
    webhook_url: Url,
    secret: Option<String>,
    at_mobiles: Vec<String>,
    is_at_all: bool,
    host_tag: String,
}

impl DingTalkSender {
    fn from_env(
        webhook_env: &str,
        secret_env: Option<&str>,
        request_timeout_secs: u64,
        at_mobiles: Vec<String>,
        is_at_all: bool,
        host_tag: &str,
    ) -> Result<Self> {
        let webhook_env = webhook_env.trim();
        let webhook = env::var(webhook_env).with_context(|| {
            format!("DingTalk webhook environment variable {webhook_env} is not set")
        })?;
        let mut webhook_url = Url::parse(&webhook).context("DingTalk webhook URL is invalid")?;
        if webhook_url.scheme() != "https" {
            bail!("DingTalk webhook URL must use https");
        }
        let secret = secret_env
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(env::var)
            .transpose()
            .context("failed to read DingTalk signing secret")?;
        let client = Client::builder()
            .timeout(Duration::from_secs(request_timeout_secs.max(1)))
            .build()
            .context("failed to build DingTalk HTTP client")?;
        // Validate before the first send while retaining the original URL only in memory.
        webhook_url.set_fragment(None);
        Ok(Self {
            client,
            webhook_url,
            secret,
            at_mobiles,
            is_at_all,
            host_tag: host_tag.trim().to_string(),
        })
    }

    async fn send_with_retry(
        &self,
        notices: &[PendingNotice],
        retry_attempts: u32,
        retry_backoff_ms: u64,
    ) -> Result<()> {
        self.post_with_retry(
            self.notice_content(notices),
            retry_attempts,
            retry_backoff_ms,
        )
        .await
    }

    async fn send_heartbeat(
        &self,
        message: &str,
        retry_attempts: u32,
        retry_backoff_ms: u64,
    ) -> Result<()> {
        self.post_with_retry(
            self.heartbeat_content(message),
            retry_attempts,
            retry_backoff_ms,
        )
        .await
    }

    fn heartbeat_content(&self, message: &str) -> String {
        let mut content = self.header();
        content.push_str("[心跳] ");
        content.push_str(message);
        content.push('\n');
        content
    }

    fn header(&self) -> String {
        if self.host_tag.is_empty() {
            String::from("[crypto_cta_manager] CTA 运行监控\n")
        } else {
            format!("[{}][crypto_cta_manager] CTA 运行监控\n", self.host_tag)
        }
    }

    fn notice_content(&self, notices: &[PendingNotice]) -> String {
        let mut content = self.header();
        for notice in notices.iter().take(30) {
            let prefix = if notice.recovery {
                "[恢复]"
            } else {
                "[告警]"
            };
            content.push_str(prefix);
            content.push(' ');
            content.push_str(&notice.message);
            content.push('\n');
        }
        if notices.len() > 30 {
            content.push_str(&format!("另有 {} 条告警未展开\n", notices.len() - 30));
        }
        if content.len() > 4_000 {
            content.truncate(3_980);
            content.push_str("...\n");
        }
        content
    }

    async fn post_with_retry(
        &self,
        content: String,
        retry_attempts: u32,
        retry_backoff_ms: u64,
    ) -> Result<()> {
        let attempts = retry_attempts.max(1);
        let mut last_error = None;
        for attempt in 0..attempts {
            match self.post(&content).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last_error = Some(format!("{error:#}"));
                    if attempt + 1 < attempts {
                        let shift = attempt.min(5);
                        let backoff = retry_backoff_ms.saturating_mul(1_u64 << shift).min(30_000);
                        tokio::time::sleep(Duration::from_millis(backoff)).await;
                    }
                }
            }
        }
        bail!(
            "DingTalk webhook failed after {attempts} attempts: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )
    }

    async fn post(&self, content: &str) -> Result<()> {
        let payload = DingTalkPayload {
            msg_type: "text",
            text: DingTalkText {
                content: content.to_string(),
            },
            at: DingTalkAt {
                at_mobiles: &self.at_mobiles,
                is_at_all: self.is_at_all,
            },
        };
        let url = signed_url(&self.webhook_url, self.secret.as_deref())?;
        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .context("DingTalk webhook request failed")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read DingTalk webhook response")?;
        if status != StatusCode::OK {
            bail!("DingTalk webhook returned {status}: {body}");
        }
        let result = serde_json::from_str::<DingTalkResponse>(&body)
            .context("DingTalk webhook returned invalid JSON")?;
        if result.errcode != 0 {
            bail!("DingTalk webhook rejected message: {}", result.errmsg);
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct DingTalkPayload<'a> {
    #[serde(rename = "msgtype")]
    msg_type: &'static str,
    text: DingTalkText,
    at: DingTalkAt<'a>,
}

#[derive(Debug, Serialize)]
struct DingTalkText {
    content: String,
}

#[derive(Debug, Serialize)]
struct DingTalkAt<'a> {
    #[serde(rename = "atMobiles")]
    at_mobiles: &'a [String],
    #[serde(rename = "isAtAll")]
    is_at_all: bool,
}

#[derive(Debug, serde::Deserialize)]
struct DingTalkResponse {
    errcode: i64,
    #[serde(default)]
    errmsg: String,
}

fn signed_url(base: &Url, secret: Option<&str>) -> Result<Url> {
    let Some(secret) = secret else {
        return Ok(base.clone());
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis();
    let timestamp_text = timestamp.to_string();
    let string_to_sign = format!("{timestamp}\n{secret}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .context("failed to initialize DingTalk HMAC")?;
    mac.update(string_to_sign.as_bytes());
    let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    let mut url = base.clone();
    url.query_pairs_mut()
        .append_pair("timestamp", &timestamp_text)
        .append_pair("sign", &sign);
    Ok(url)
}

fn print_dry_run(issues: &[MonitorIssue]) {
    if issues.is_empty() {
        println!("CTA monitor: OK");
        return;
    }
    println!("CTA monitor: {} issue(s)", issues.len());
    for issue in issues {
        println!("{} [{}] {}", issue.key, issue.category, issue.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viz_snapshot::ExecStateRowSnapshot;

    #[test]
    fn signed_url_preserves_webhook_and_adds_signature_parameters() {
        let url = Url::parse("https://oapi.dingtalk.com/robot/send?access_token=redacted").unwrap();
        let signed = signed_url(&url, Some("secret")).unwrap();
        assert_eq!(signed.host_str(), Some("oapi.dingtalk.com"));
        assert!(signed.query().unwrap().contains("access_token=redacted"));
        assert!(signed.query().unwrap().contains("timestamp="));
        assert!(signed.query().unwrap().contains("sign="));
    }

    #[test]
    fn tracker_notifies_on_transition_repeats_and_recovery() {
        let issue = MonitorIssue::new("position", "stale", "trade01", "stale");
        let mut tracker = AlertTracker::default();
        let first = Instant::now();
        let notices = tracker.pending(std::slice::from_ref(&issue), first, 60);
        assert_eq!(notices.len(), 1);
        tracker.mark_sent(&notices, first);
        assert!(
            tracker
                .pending(std::slice::from_ref(&issue), first, 60)
                .is_empty()
        );
        let later = first + Duration::from_secs(61);
        assert_eq!(
            tracker
                .pending(std::slice::from_ref(&issue), later, 60)
                .len(),
            1
        );
        let recovery = tracker.pending(&[], later, 60);
        assert_eq!(recovery.len(), 1);
        assert!(recovery[0].recovery);
        tracker.mark_sent(&recovery, later);
        assert!(tracker.pending(&[], later, 60).is_empty());
    }

    #[test]
    fn market_symbols_are_normalized() {
        assert_eq!(normalize_symbol("btc-usdt"), "BTCUSDT");
        assert_eq!(normalize_symbol("eth_usdt"), "ETHUSDT");
    }

    #[test]
    fn signed_url_without_secret_is_unchanged() {
        let url = Url::parse("https://example.test/hook?access_token=x").unwrap();
        assert_eq!(signed_url(&url, None).unwrap(), url);
    }

    fn market_test_config(symbols: &[&str]) -> AppConfig {
        AppConfig {
            database: crate::config::DatabaseConfig {
                url_env: "TEST_DATABASE_URL".to_string(),
                max_connections: 1,
            },
            ingestion: Default::default(),
            order_config: Default::default(),
            redis: Default::default(),
            twap: Default::default(),
            monitor: MonitorConfig {
                market_stale_secs: 5,
                market_symbols: symbols.iter().map(|s| s.to_string()).collect(),
                ..MonitorConfig::default()
            },
            sources: vec![test_source("binance-futures")],
        }
    }

    fn test_source(venue: &str) -> SourceConfig {
        SourceConfig {
            id: "src".to_string(),
            account: "src".to_string(),
            alias: None,
            venue: venue.to_string(),
            rocksdb_path: std::path::PathBuf::from("/tmp/nonexistent"),
            enabled: true,
            start_ts_us: None,
            poll_interval_secs: None,
            estimated_fee_rate: None,
            maker_fee_rate: None,
            taker_fee_rate: None,
            gateway_prefix: None,
            exec_config_url: None,
            exec_viz_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            legacy_share_unit_usdt: None,
            env_path: None,
        }
    }

    fn insert_quote(feed: &MarketFeed, venue: &str, symbol: &str, received_ts_us: i64) {
        feed.state
            .write()
            .unwrap()
            .latest
            .insert((venue.to_string(), symbol.to_string()), received_ts_us);
    }

    #[test]
    fn market_check_alerts_only_when_no_watched_symbol_is_fresh() {
        let venue = "binance-futures";
        let config = market_test_config(&["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"]);
        let feed = MarketFeed::default();
        let now = unix_time_us();

        // No BBO received at all -> market failure issue.
        let issues = check_market(&config, &feed, now);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "market:binance-futures:bbo:global");

        // Fresh BBO on a symbol outside the watched set still alerts.
        insert_quote(&feed, venue, "DOGEUSDT", now);
        assert_eq!(check_market(&config, &feed, now).len(), 1);

        // One fresh watched symbol is enough to clear the failure.
        insert_quote(&feed, venue, "ETHUSDT", now);
        assert!(check_market(&config, &feed, now).is_empty());

        // The only watched quote goes stale -> alert again.
        insert_quote(&feed, venue, "ETHUSDT", now - 6_000_000);
        assert_eq!(check_market(&config, &feed, now).len(), 1);
    }

    #[test]
    fn repeat_alert_interval_has_thirty_second_floor() {
        let issue = MonitorIssue::new("market", "v:bbo", "global", "m");
        let mut tracker = AlertTracker::default();
        let start = Instant::now();
        let notices = tracker.pending(std::slice::from_ref(&issue), start, 1);
        tracker.mark_sent(&notices, start);
        assert!(
            tracker
                .pending(
                    std::slice::from_ref(&issue),
                    start + Duration::from_secs(5),
                    1
                )
                .is_empty()
        );
        assert_eq!(
            tracker
                .pending(
                    std::slice::from_ref(&issue),
                    start + Duration::from_secs(31),
                    1
                )
                .len(),
            1
        );
    }

    #[test]
    fn recovery_notice_reuses_alert_message() {
        let issue = MonitorIssue::new("market", "v:bbo", "global", "行情故障: test");
        let mut tracker = AlertTracker::default();
        let start = Instant::now();
        let notices = tracker.pending(std::slice::from_ref(&issue), start, 30);
        tracker.mark_sent(&notices, start);
        let recovery = tracker.pending(&[], start + Duration::from_secs(1), 30);
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].message, "问题已恢复: 行情故障: test");
    }

    #[test]
    fn delayed_issue_disappears_without_alert_or_recovery() {
        let issue = MonitorIssue::new("position", "account-missing:GUSDT", "trade06", "missing")
            .with_initial_delay(30);
        let mut tracker = AlertTracker::default();
        let start = Instant::now();

        assert!(
            tracker
                .pending(std::slice::from_ref(&issue), start, 30)
                .is_empty()
        );
        assert_eq!(tracker.active_count(NoticeChannel::Order), 0);
        assert!(
            tracker
                .pending(&[], start + Duration::from_secs(10), 30)
                .is_empty()
        );
        assert_eq!(tracker.active_count(NoticeChannel::Order), 0);
    }

    #[test]
    fn delayed_issue_alerts_after_grace_and_then_recovers() {
        let issue = MonitorIssue::new("position", "account-missing:GUSDT", "trade06", "missing")
            .with_initial_delay(30);
        let mut tracker = AlertTracker::default();
        let start = Instant::now();

        assert!(
            tracker
                .pending(std::slice::from_ref(&issue), start, 30)
                .is_empty()
        );
        assert!(
            tracker
                .pending(
                    std::slice::from_ref(&issue),
                    start + Duration::from_secs(29),
                    30,
                )
                .is_empty()
        );
        let notices = tracker.pending(
            std::slice::from_ref(&issue),
            start + Duration::from_secs(30),
            30,
        );
        assert_eq!(notices.len(), 1);
        tracker.mark_sent(&notices, start + Duration::from_secs(30));
        assert_eq!(tracker.active_count(NoticeChannel::Order), 1);

        let recovery = tracker.pending(&[], start + Duration::from_secs(31), 30);
        assert_eq!(recovery.len(), 1);
        assert!(recovery[0].recovery);
    }

    fn position_row(
        strategy: &str,
        symbol: &str,
        current: f64,
        target: f64,
        pending: f64,
        live: f64,
        account: Option<f64>,
        updated_ms: i64,
    ) -> ExecStateRowSnapshot {
        ExecStateRowSnapshot {
            strategy_name: strategy.to_string(),
            symbol: symbol.to_string(),
            source_updated_at_ms: updated_ms,
            current_qty: Some(current),
            current_usdt: None,
            target_qty: Some(target),
            pending_qty: Some(pending),
            live_order_qty: Some(live),
            estimated_completion_ts_ms: updated_ms + 3_600_000,
            execution_complete: false,
            completion_reason: String::new(),
            account_position_qty: account,
        }
    }

    #[test]
    fn position_check_alerts_only_when_configured_gap_is_stuck() {
        let source = test_source("binance-futures");
        let monitor = MonitorConfig::default();
        let now_us = unix_time_us();
        let now_ms = now_us / 1_000;
        let old_ms = now_ms - (monitor.execution_grace_secs as i64 + 10) * 1_000;
        // Configured position comes from the Redis Exec targets.
        let configured = BTreeMap::from([("BTCUSDT".to_string(), 0.3)]);
        let snapshot = |account: f64| ExecStateSnapshot {
            source_id: source.id.clone(),
            snapshot_ts_ms: now_ms,
            position_ready: true,
            rows: vec![
                position_row(
                    "cta_a",
                    "BTCUSDT",
                    0.5,
                    0.5,
                    0.0,
                    0.0,
                    Some(account),
                    old_ms,
                ),
                position_row(
                    "cta_b",
                    "BTCUSDT",
                    -0.2,
                    -0.2,
                    0.0,
                    0.0,
                    Some(account),
                    old_ms,
                ),
            ],
        };

        // Configured 0.3 == account 0.3 -> consistent.
        let issues = check_position(&source, &monitor, &snapshot(0.3), Some(&configured), now_us);
        assert!(issues.iter().all(|issue| !issue.key.contains("account:")));

        // Configured 0.3 vs account 0.4, quiet and no live qty -> mismatch.
        // The alert waits out the execution grace window before notifying so a
        // just-published Redis target propagating to the snapshot stays silent.
        let issues = check_position(&source, &monitor, &snapshot(0.4), Some(&configured), now_us);
        let gap_issue = issues
            .iter()
            .find(|issue| issue.key.contains("account:BTCUSDT"))
            .expect("stuck gap should be reported");
        assert_eq!(gap_issue.initial_delay_secs, monitor.execution_grace_secs);

        // Live order qty covering the gap means it is still executing.
        let mut executing = snapshot(0.2);
        executing.rows[0].current_qty = Some(0.4);
        executing.rows[0].live_order_qty = Some(0.1);
        let issues = check_position(&source, &monitor, &executing, Some(&configured), now_us);
        assert!(issues.iter().all(|issue| !issue.key.contains("account:")));

        // A strategy row updated inside the grace window is still settling.
        let mut settling = snapshot(0.4);
        settling.rows[0].source_updated_at_ms = now_ms;
        let issues = check_position(&source, &monitor, &settling, Some(&configured), now_us);
        assert!(issues.iter().all(|issue| !issue.key.contains("account:")));

        // A failed Redis read skips the comparison instead of alerting on an
        // empty configured map.
        let issues = check_position(&source, &monitor, &snapshot(0.4), None, now_us);
        assert!(issues.iter().all(|issue| !issue.key.contains("account:")));

        // A configured symbol absent from the Viz snapshot is unknown rather
        // than a factual zero, and receives an initial grace period.
        let missing = BTreeMap::from([("SOLUSDT".to_string(), 5.0)]);
        let issues = check_position(&source, &monitor, &snapshot(0.3), Some(&missing), now_us);
        let issue = issues
            .iter()
            .find(|issue| issue.key.contains("account-missing:SOLUSDT"))
            .expect("missing account row should be reported separately");
        assert_eq!(issue.initial_delay_secs, monitor.execution_grace_secs);
        assert!(!issue.message.contains("账户仓位 0.000000000000"));

        // An explicit zero from Viz remains factual and alerts as a mismatch.
        let zero_issues =
            check_position(&source, &monitor, &snapshot(0.0), Some(&configured), now_us);
        assert!(
            zero_issues
                .iter()
                .any(|issue| issue.key.contains("account:BTCUSDT"))
        );
        assert!(
            zero_issues
                .iter()
                .all(|issue| !issue.key.contains("account-missing:BTCUSDT"))
        );
    }

    #[test]
    fn position_check_suppresses_dust_residuals() {
        let source = test_source("binance-futures");
        let monitor = MonitorConfig::default();
        let now_us = unix_time_us();
        let now_ms = now_us / 1_000;
        let old_ms = now_ms - (monitor.execution_grace_secs as i64 + 10) * 1_000;
        let snapshot = |rows: Vec<ExecStateRowSnapshot>| ExecStateSnapshot {
            source_id: source.id.clone(),
            snapshot_ts_ms: now_ms,
            position_ready: true,
            rows,
        };

        // Configured-vs-account gap valued below the residual threshold -> quiet.
        let configured = BTreeMap::from([("DOGEUSDT".to_string(), 150.0)]);
        let mut mismatch = snapshot(vec![position_row(
            "cta_a",
            "DOGEUSDT",
            100.0,
            100.0,
            0.0,
            0.0,
            Some(100.0),
            old_ms,
        )]);
        mismatch.rows[0].current_usdt = Some(10.0); // gap 50 qty x 0.1 = 5 USDT
        let issues = check_position(&source, &monitor, &mismatch, Some(&configured), now_us);
        assert!(issues.iter().all(|issue| !issue.key.contains("account:")));

        // Same gap priced far above the residual threshold -> alert.
        mismatch.rows[0].current_usdt = Some(100_000.0); // gap 50 qty x 1000 = 50k USDT
        let issues = check_position(&source, &monitor, &mismatch, Some(&configured), now_us);
        assert!(
            issues
                .iter()
                .any(|issue| issue.key.contains("account:DOGEUSDT"))
        );

        // No USDT value to price the gap -> conservative, still alerts.
        mismatch.rows[0].current_usdt = None;
        let issues = check_position(&source, &monitor, &mismatch, Some(&configured), now_us);
        assert!(
            issues
                .iter()
                .any(|issue| issue.key.contains("account:DOGEUSDT"))
        );
    }

    #[test]
    fn market_notices_and_order_notices_use_separate_channels() {
        let market = PendingNotice {
            key: "market:binance-futures:any:global".to_string(),
            message: "market".to_string(),
            recovery: false,
        };
        let order = PendingNotice {
            key: "orders:stale:trade01".to_string(),
            message: "order".to_string(),
            recovery: false,
        };
        assert_eq!(market.channel(), NoticeChannel::Market);
        assert_eq!(order.channel(), NoticeChannel::Order);
    }

    #[test]
    fn dingtalk_payload_is_text_with_at_fields() {
        let payload = DingTalkPayload {
            msg_type: "text",
            text: DingTalkText {
                content: "alert".to_string(),
            },
            at: DingTalkAt {
                at_mobiles: &["13800000000".to_string()],
                is_at_all: false,
            },
        };
        let value = serde_json::to_value(payload).unwrap();
        assert_eq!(value["msgtype"], serde_json::json!("text"));
        assert_eq!(value["at"]["isAtAll"], serde_json::json!(false));
    }

    #[test]
    fn shanghai_time_uses_fixed_utc8() {
        // The Unix epoch was 08:00 in Shanghai; 16:00 UTC is midnight there.
        assert_eq!(shanghai_hour_of_day(shanghai_secs(0)), 8);
        assert_eq!(shanghai_hour_of_day(shanghai_secs(16 * 3_600)), 0);
        assert_eq!(shanghai_hour_of_day(shanghai_secs(86_400 + 3_600)), 9);
    }

    #[test]
    fn quiet_window_covers_hours_and_wraps_overnight() {
        assert!(in_quiet_window(0, 0, 6));
        assert!(in_quiet_window(5, 0, 6));
        assert!(!in_quiet_window(6, 0, 6));
        assert!(!in_quiet_window(23, 0, 6));
        // A wrapped window such as 22:00-06:00 spans midnight.
        assert!(in_quiet_window(23, 22, 6));
        assert!(in_quiet_window(3, 22, 6));
        assert!(!in_quiet_window(12, 22, 6));
        // Equal bounds disable the window entirely.
        assert!((0..24).all(|hour| !in_quiet_window(hour, 6, 6)));
    }

    #[test]
    fn heartbeat_fires_once_per_slot_and_consumes_quiet_slots() {
        let day = 86_400 * 20_000;
        // 3h cadence, started mid-slot: no beat until the next boundary.
        let mut schedule = HeartbeatSchedule::new(3, day + 9 * 3_600 + 30 * 60);
        assert!(!schedule.due(day + 10 * 3_600, false));
        assert!(!schedule.due(day + 11 * 3_600 + 59 * 60, false));
        // The 12:00 boundary fires exactly once.
        assert!(schedule.due(day + 12 * 3_600, false));
        assert!(!schedule.due(day + 12 * 3_600 + 1, false));

        // A boundary inside the quiet window is consumed silently; the first
        // boundary after dawn still fires on schedule.
        let mut schedule = HeartbeatSchedule::new(3, day + 21 * 3_600);
        assert!(!schedule.due(day + 24 * 3_600, true)); // 00:00, quiet
        assert!(!schedule.due(day + 27 * 3_600, true)); // 03:00, quiet
        assert!(schedule.due(day + 30 * 3_600, false)); // 06:00, fires

        // Zero interval disables the heartbeat.
        let mut off = HeartbeatSchedule::new(0, day);
        assert!(!off.due(day + 5 * 86_400, false));
    }

    #[test]
    fn heartbeat_message_reports_channel_status() {
        assert_eq!(
            heartbeat_message(NoticeChannel::Market, 0),
            "行情监控运行正常，无未恢复告警"
        );
        assert_eq!(
            heartbeat_message(NoticeChannel::Order, 2),
            "交易监控运行中，2 条告警未恢复"
        );
    }

    #[test]
    fn source_issues_carry_the_account_alias() {
        let mut source = test_source("binance-futures");
        source.id = "binance_exec_trade03".to_string();
        source.account = "trade03".to_string();
        // The alias is the operator-facing label.
        source.alias = Some("p1prcp1".to_string());
        let entry = issue("position", "account:BTCUSDT", &source, "m");
        assert_eq!(entry.key, "position:account:BTCUSDT:binance_exec_trade03");
        assert_eq!(entry.message, "[p1prcp1] m");
        // A missing or blank alias falls back to account, then to source id.
        source.alias = Some(" ".to_string());
        let entry = issue("position", "scope", &source, "m");
        assert_eq!(entry.message, "[trade03] m");
        source.alias = None;
        source.account = " ".to_string();
        let entry = issue("position", "scope", &source, "m");
        assert_eq!(entry.message, "[binance_exec_trade03] m");
    }

    #[test]
    fn heartbeat_content_uses_heartbeat_prefix_and_host_tag() {
        let sender = DingTalkSender {
            client: Client::new(),
            webhook_url: Url::parse("https://example.test/hook").unwrap(),
            secret: None,
            at_mobiles: Vec::new(),
            is_at_all: false,
            host_tag: "el01".to_string(),
        };
        let content = sender.heartbeat_content("行情监控运行正常，无未恢复告警");
        assert_eq!(
            content,
            "[el01][crypto_cta_manager] CTA 运行监控\n[心跳] 行情监控运行正常，无未恢复告警\n"
        );
    }
}
