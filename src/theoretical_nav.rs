//! On-demand theoretical execution from immutable targets and minute candles.
//! No PostgreSQL materialization cursor or realtime BBO dependency.
use crate::config::AppConfig;
use crate::kline::{KlineStore, MINUTE_US, first_complete_open, now_us};
use crate::position_archive::{PositionArchive, PositionUpdateMsg};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use sqlx::postgres::PgPool;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const EXECUTION_WINDOW_SECS: u64 = 300;
const ZERO_EPSILON: f64 = 1e-12;
pub const SAMPLE_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TheoreticalNavPoint {
    pub ts_us: i64,
    pub nav_change_before_fee_quote: f64,
    pub nav_change_after_fee_quote: f64,
    pub estimated_trading_fee_quote: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TheoreticalNavTimeline {
    pub valuation: &'static str,
    pub execution_window_secs: u64,
    pub price_basis: &'static str,
    pub fee_basis: &'static str,
    pub available_from_us: Option<i64>,
    pub latest_point_ts_us: Option<i64>,
    pub points: Vec<TheoreticalNavPoint>,
    pub sampled: bool,
    pub unavailable_reason: Option<String>,
    pub missing_price_count: usize,
    pub legacy_fee_delta_count: usize,
}
impl Default for TheoreticalNavTimeline {
    fn default() -> Self {
        Self {
            valuation: "quantity_fifo_window_delta",
            execution_window_secs: EXECUTION_WINDOW_SECS,
            price_basis: "five_equal_qty_complete_1m_quote_over_base_vwap+closed_1m_close_mark",
            fee_basis: "archived_theoretical_rate_or_current_rate_for_legacy_targets",
            available_from_us: None,
            latest_point_ts_us: None,
            points: Vec::new(),
            sampled: false,
            unavailable_reason: None,
            missing_price_count: 0,
            legacy_fee_delta_count: 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualDelta {
    pub source_id: String,
    pub binding_name: String,
    pub strategy_name: String,
    pub symbol: String,
    pub venue: String,
    pub received_at_us: i64,
    pub seq: u32,
    pub delta_qty: f64,
    pub fee_rate: f64,
    pub legacy_fee: bool,
    pub target_signal: i32,
}
impl VirtualDelta {
    pub fn open_ts_us(&self, index: usize) -> i64 {
        first_complete_open(self.received_at_us) + index as i64 * MINUTE_US
    }
    pub fn execution_ts_us(&self) -> i64 {
        self.open_ts_us(4) + MINUTE_US
    }
    pub fn prices(&self, store: &KlineStore) -> Result<Option<[f64; SAMPLE_COUNT]>> {
        if self.venue != "binance-futures" {
            return Ok(None);
        }
        let mut prices = [0.0; SAMPLE_COUNT];
        for (index, price) in prices.iter_mut().enumerate() {
            let Some(candle) = store.get(&self.symbol, self.open_ts_us(index))? else {
                return Ok(None);
            };
            let Some(vwap) = candle.vwap() else {
                return Ok(None);
            };
            *price = vwap;
        }
        Ok(Some(prices))
    }
}

#[derive(Default)]
struct TargetCacheState {
    cursor: Option<(i64, u32)>,
    latest: BTreeMap<(String, String), LatestTargets>,
    deltas: Vec<VirtualDelta>,
}

#[derive(Serialize)]
pub struct TargetHistoryStatus {
    pub ready: bool,
    pub loading: bool,
    pub processed_messages: u64,
    pub last_error: Option<String>,
}

/// Rebuild once in the background, then fold only newly archived publications.
/// This is a disposable metadata cache; RocksDB remains the durable source.
pub struct TheoreticalTargetCache {
    archive: Arc<PositionArchive>,
    config: Arc<AppConfig>,
    state: Mutex<TargetCacheState>,
    ready: AtomicBool,
    loading: AtomicBool,
    processed_messages: AtomicU64,
    error: Mutex<Option<String>>,
    changed: tokio::sync::Notify,
}
impl TheoreticalTargetCache {
    pub fn new(config: Arc<AppConfig>, archive: Arc<PositionArchive>) -> Arc<Self> {
        Arc::new(Self {
            archive,
            config,
            state: Mutex::new(TargetCacheState::default()),
            ready: AtomicBool::new(false),
            loading: AtomicBool::new(false),
            processed_messages: AtomicU64::new(0),
            error: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
        })
    }
    pub fn status(&self) -> TargetHistoryStatus {
        TargetHistoryStatus {
            ready: self.ready.load(Ordering::Acquire),
            loading: self.loading.load(Ordering::Acquire),
            processed_messages: self.processed_messages.load(Ordering::Relaxed),
            last_error: self.error.lock().unwrap().clone(),
        }
    }
    pub fn refresh(self: &Arc<Self>) {
        if self.loading.load(Ordering::Acquire) {
            return;
        }
        if self.ready.load(Ordering::Acquire)
            && self.state.lock().unwrap().cursor.unwrap_or((0, 0)) >= self.archive.latest_cursor()
        {
            return;
        }
        if self
            .loading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = cache.refresh_blocking();
            *cache.error.lock().unwrap() = result.err().map(|error| error.to_string());
            cache.loading.store(false, Ordering::Release);
            cache.changed.notify_waiters();
        });
    }
    fn refresh_blocking(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let after = state.cursor;
        self.archive
            .visit_target_updates_after(after, now_us(), |message| {
                ingest_target(&self.config, &mut state, &message)?;
                state.cursor = Some((message.received_at_us, message.seq));
                self.processed_messages.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })?;
        self.ready.store(true, Ordering::Release);
        Ok(())
    }
    async fn query(
        self: &Arc<Self>,
        fee_rates: BTreeMap<String, f64>,
        end: i64,
        source_ids: &[String],
        strategy: Option<&str>,
    ) -> Result<Option<Vec<VirtualDelta>>> {
        self.refresh();
        let wait = async {
            loop {
                let changed = self.changed.notified();
                // Ready data must include the completed incremental refresh too.
                if !self.loading.load(Ordering::Acquire) {
                    if let Some(error) = self.error.lock().unwrap().clone() {
                        bail!("target history cache: {error}");
                    }
                    if self.ready.load(Ordering::Acquire) {
                        return Ok(());
                    }
                }
                changed.await;
            }
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), wait).await {
            Ok(result) => result?,
            Err(_) => return Ok(None),
        }
        let cache = self.clone();
        let source_ids = source_ids.to_vec();
        let strategy = strategy.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            let state = cache.state.lock().unwrap();
            let mut deltas = Vec::new();
            for delta in state
                .deltas
                .iter()
                .take_while(|delta| delta.received_at_us <= end)
            {
                if (!source_ids.is_empty() && !source_ids.contains(&delta.source_id))
                    || strategy
                        .as_deref()
                        .is_some_and(|name| name != delta.strategy_name)
                {
                    continue;
                }
                let mut delta = delta.clone();
                if delta.legacy_fee {
                    delta.fee_rate =
                        fee_rates.get(&delta.source_id).copied().with_context(|| {
                            format!("missing theoretical fee for {}", delta.source_id)
                        })?;
                }
                ensure!(delta.fee_rate.is_finite(), "invalid theoretical fee");
                deltas.push(delta);
            }
            Ok(Some(deltas))
        })
        .await
        .context("theoretical target reader failed")?
    }
}

fn ingest_target(
    config: &AppConfig,
    state: &mut TargetCacheState,
    message: &PositionUpdateMsg,
) -> Result<()> {
    for account in &message.published_accounts {
        let Some(source) = config
            .sources
            .iter()
            .find(|s| s.enabled && s.id == account.source_id)
        else {
            continue;
        };
        let key = (account.source_id.clone(), account.binding_name.clone());
        let next = LatestTargets {
            position_strategy_name: message.strategy.strategy_name.clone(),
            venue: source.venue.clone(),
            targets: normalized_scaled_targets(message, account.effective_shares())?,
            received_at_us: message.received_at_us,
            update_seq: message.seq,
        };
        if target_positions_changed(state.latest.get(&key), &next) {
            let fee_rate = account.theoretical_fee_rate.unwrap_or(0.0);
            ensure!(fee_rate.is_finite(), "invalid archived theoretical fee");
            for (symbol, quantity) in target_deltas(state.latest.get(&key), &next) {
                let target_signal = message
                    .strategy
                    .targets
                    .get(&symbol)
                    .map(|target| target.signal)
                    .unwrap_or(0);
                state.deltas.push(VirtualDelta {
                    source_id: source.id.clone(),
                    binding_name: account.binding_name.clone(),
                    strategy_name: next.position_strategy_name.clone(),
                    symbol,
                    venue: next.venue.clone(),
                    received_at_us: next.received_at_us,
                    seq: next.update_seq,
                    delta_qty: quantity,
                    fee_rate,
                    legacy_fee: account.theoretical_fee_rate.is_none(),
                    target_signal,
                });
            }
        }
        state.latest.insert(key, next);
    }
    Ok(())
}

/// Detached backfills survive HTTP cancellation. Return coverage after at most
/// 10s of waiting; a later query reuses the cache rather than restarting pulls.
pub async fn warm_ranges(store: &KlineStore, ranges: BTreeMap<String, (i64, i64)>) -> Vec<String> {
    let store = store.clone();
    let mut task = tokio::spawn(async move {
        let mut jobs = tokio::task::JoinSet::new();
        for (symbol, (start, end)) in ranges {
            let store = store.clone();
            jobs.spawn(async move { store.ensure_range(&symbol, start, end).await });
        }
        let mut errors = Vec::new();
        while let Some(result) = jobs.join_next().await {
            if let Err(error) = result.unwrap_or_else(|e| Err(e.into())) {
                errors.push(error.to_string());
            }
        }
        errors
    });
    match tokio::time::timeout(std::time::Duration::from_secs(10), &mut task).await {
        Ok(Ok(errors)) => errors,
        Ok(Err(error)) => vec![error.to_string()],
        Err(_) => vec!["分钟 K 线正在后台补齐，请稍后重新查询".into()],
    }
}
fn add_range(ranges: &mut BTreeMap<String, (i64, i64)>, symbol: &str, start: i64, end: i64) {
    ranges
        .entry(symbol.into())
        .and_modify(|range| {
            range.0 = range.0.min(start);
            range.1 = range.1.max(end);
        })
        .or_insert((start, end));
}
pub async fn prepare_acquisition(
    pool: &PgPool,
    targets: &Arc<TheoreticalTargetCache>,
    store: &KlineStore,
    start: i64,
    end: i64,
    source_ids: &[String],
    strategy: Option<&str>,
) -> Result<(Vec<VirtualDelta>, Vec<String>)> {
    store.validate_range(start, end, now_us())?;
    ensure!(store.enabled(), "分钟 K 线理论分析未启用");
    let fees = crate::postgres::load_theoretical_twap_fee_rates(pool).await?;
    // Older target metadata establishes deltas only; no older market data is read.
    let Some(mut deltas) = targets.query(fees, end, source_ids, strategy).await? else {
        return Ok((
            Vec::new(),
            vec!["目标历史正在后台读取，请稍后重新查询".into()],
        ));
    };
    deltas.retain(|d| d.received_at_us >= (start - 300 * 1_000_000).max(store.cutoff_us(now_us())));
    let mut ranges = BTreeMap::new();
    for delta in &deltas {
        if delta.venue == "binance-futures" {
            add_range(
                &mut ranges,
                &delta.symbol,
                delta.open_ts_us(0),
                delta.execution_ts_us(),
            );
        }
    }
    let errors = warm_ranges(store, ranges).await;
    Ok((deltas, errors))
}

pub async fn load_timeline(
    pool: &PgPool,
    targets: &Arc<TheoreticalTargetCache>,
    store: &KlineStore,
    start: i64,
    end: i64,
    source_ids: &[String],
    max_points: usize,
) -> Result<TheoreticalNavTimeline> {
    let mut output = TheoreticalNavTimeline {
        available_from_us: Some(first_complete_open(store.cutoff_us(now_us())) + MINUTE_US),
        ..Default::default()
    };
    if !store.enabled() {
        output.unavailable_reason = Some("分钟 K 线理论分析未启用".into());
        return Ok(output);
    }
    if let Err(error) = store.validate_range(start, end, now_us()) {
        output.unavailable_reason = Some(error.to_string());
        return Ok(output);
    }
    let baseline_open = start.div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
    if baseline_open < store.cutoff_us(now_us()) {
        output.unavailable_reason =
            Some("区间起点没有保留范围内的完整分钟基准价，请将起点后移一分钟".into());
        return Ok(output);
    }
    let fees = crate::postgres::load_theoretical_twap_fee_rates(pool).await?;
    let Some(deltas) = targets.query(fees, end, source_ids, None).await? else {
        output.unavailable_reason = Some("目标历史正在后台读取，请稍后重新查询".into());
        return Ok(output);
    };
    // Exact inventory immediately before start follows from the frozen schedules,
    // without needing old execution prices. Carry lots use the start's mark.
    let mut carry = BTreeMap::<(String, String, String), f64>::new();
    let mut fills = Vec::<(i64, usize, usize)>::new();
    let mut ranges = BTreeMap::new();
    for (index, delta) in deltas.iter().enumerate() {
        for slice in 0..SAMPLE_COUNT {
            let ts = delta.open_ts_us(slice) + MINUTE_US;
            if ts < start {
                *carry
                    .entry((
                        delta.source_id.clone(),
                        delta.symbol.clone(),
                        delta.venue.clone(),
                    ))
                    .or_default() += delta.delta_qty / 5.0;
            } else if ts <= end {
                fills.push((ts, index, slice));
                if delta.venue == "binance-futures" {
                    add_range(&mut ranges, &delta.symbol, baseline_open, end);
                }
            }
        }
    }
    carry.retain(|_, qty| clean_zero(*qty) != 0.0);
    for ((_, symbol, venue), _) in &carry {
        if venue == "binance-futures" {
            add_range(&mut ranges, symbol, baseline_open, end);
        }
    }
    let errors = warm_ranges(store, ranges).await;
    fills.sort_by_key(|(ts, delta, slice)| {
        (
            *ts,
            deltas[*delta].received_at_us,
            deltas[*delta].seq,
            *delta,
            *slice,
        )
    });
    rebuild_timeline(
        &deltas,
        carry,
        fills,
        store,
        start,
        end,
        max_points,
        &mut output,
    )?;
    if !errors.is_empty() && output.points.is_empty() {
        output.unavailable_reason = Some(errors.join("; "));
    }
    Ok(output)
}

fn rebuild_timeline(
    deltas: &[VirtualDelta],
    carry: BTreeMap<(String, String, String), f64>,
    fills: Vec<(i64, usize, usize)>,
    store: &KlineStore,
    start: i64,
    end: i64,
    max_points: usize,
    output: &mut TheoreticalNavTimeline,
) -> Result<()> {
    let mut states = BTreeMap::<(String, String, String), (SymbolState, VecDeque<FifoLot>)>::new();
    let mut markets = BTreeMap::new();
    let mut needed = carry
        .keys()
        .map(|(_, symbol, _)| symbol.clone())
        .collect::<BTreeSet<_>>();
    needed.extend(
        fills
            .iter()
            .map(|(_, index, _)| deltas[*index].symbol.clone()),
    );
    for symbol in needed {
        markets.insert(
            symbol.clone(),
            store.scan(
                &symbol,
                start.div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US,
                end,
            )?,
        );
    }
    let mark = |symbol: &str, ts: i64| -> Option<f64> {
        let bars = markets.get(symbol)?;
        let end = bars.partition_point(|c| c.end_ts_us() <= ts);
        let candle = bars.get(end.checked_sub(1)?)?;
        (ts - candle.end_ts_us() < MINUTE_US).then_some(candle.close)
    };
    for (key, quantity) in carry {
        let price = if key.2 == "binance-futures" {
            mark(&key.1, start)
        } else {
            None
        };
        let Some(price) = price else {
            output.missing_price_count += 1;
            continue;
        };
        states.insert(
            key,
            (
                SymbolState {
                    net_quantity: quantity,
                    next_lot_seq: 2,
                    ..Default::default()
                },
                VecDeque::from([FifoLot {
                    seq: 1,
                    quantity,
                    entry_price: price,
                }]),
            ),
        );
    }
    let mut ticks = BTreeSet::from([start, end]);
    let mut tick = start.div_euclid(900 * 1_000_000) * 900 * 1_000_000 + 900 * 1_000_000;
    while tick < end {
        ticks.insert(tick);
        tick += 900 * 1_000_000;
    }
    ticks.extend(fills.iter().map(|(ts, _, _)| *ts));
    let mut cursor = 0;
    let mut legacy = BTreeSet::new();
    for ts in ticks {
        while cursor < fills.len() && fills[cursor].0 <= ts {
            let (_, index, slice) = fills[cursor];
            let delta = &deltas[index];
            cursor += 1;
            let candle = if delta.venue == "binance-futures" {
                store.get(&delta.symbol, delta.open_ts_us(slice))?
            } else {
                None
            };
            let Some(price) = candle.and_then(|c| c.vwap()) else {
                output.missing_price_count += 1;
                continue;
            };
            if delta.legacy_fee {
                legacy.insert(index);
            }
            let key = (
                delta.source_id.clone(),
                delta.symbol.clone(),
                delta.venue.clone(),
            );
            let (state, lots) = states.entry(key).or_insert_with(|| {
                (
                    SymbolState {
                        next_lot_seq: 1,
                        ..Default::default()
                    },
                    VecDeque::new(),
                )
            });
            let applied = evaluate_fill(
                *state,
                std::mem::take(lots),
                delta.delta_qty / 5.0,
                price,
                delta.fee_rate,
            )?;
            *state = SymbolState {
                net_quantity: applied.net_quantity,
                realized_pnl_before_fee_quote: applied.realized_pnl_before_fee_quote,
                estimated_trading_fee_quote: applied.cumulative_fee_quote,
                next_lot_seq: applied.next_lot_seq,
            };
            *lots = applied.lots;
        }
        let mut point = TheoreticalNavPoint {
            ts_us: ts,
            ..Default::default()
        };
        for ((_, symbol, _), (state, lots)) in &states {
            let floating = if lots.is_empty() {
                0.0
            } else if let Some(price) = mark(symbol, ts) {
                floating_pnl_at_mark(lots, price)?
            } else {
                output.missing_price_count += 1;
                0.0
            };
            point.nav_change_before_fee_quote += state.realized_pnl_before_fee_quote + floating;
            point.estimated_trading_fee_quote += state.estimated_trading_fee_quote;
        }
        point.nav_change_after_fee_quote =
            point.nav_change_before_fee_quote - point.estimated_trading_fee_quote;
        push_or_replace_point(&mut output.points, point);
    }
    output.legacy_fee_delta_count = legacy.len();
    if output.missing_price_count > 0 {
        output.points.clear();
        output.unavailable_reason = Some(format!(
            "分钟行情尚未补齐或币对不受支持（{} 处缺失），理论净值暂不可用",
            output.missing_price_count
        ));
    } else {
        output.latest_point_ts_us = output.points.last().map(|point| point.ts_us);
        output.sampled = output.points.len() > max_points;
        output.points = downsample_points(std::mem::take(&mut output.points), max_points);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FifoLot {
    seq: i64,
    quantity: f64,
    entry_price: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SymbolState {
    net_quantity: f64,
    realized_pnl_before_fee_quote: f64,
    estimated_trading_fee_quote: f64,
    next_lot_seq: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct AppliedFill {
    lots: VecDeque<FifoLot>,
    net_quantity: f64,
    realized_pnl_before_fee_quote: f64,
    fee_quote: f64,
    cumulative_fee_quote: f64,
    floating_pnl_quote: f64,
    nav_before_fee_quote: f64,
    nav_after_fee_quote: f64,
    next_lot_seq: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct LatestTargets {
    position_strategy_name: String,
    venue: String,
    targets: BTreeMap<String, f64>,
    received_at_us: i64,
    update_seq: u32,
}

fn normalized_scaled_targets(
    message: &PositionUpdateMsg,
    shares: f64,
) -> Result<BTreeMap<String, f64>> {
    let mut targets = BTreeMap::new();
    for (symbol, target) in &message.strategy.targets {
        let quantity = clean_zero(target.qty * shares);
        if !quantity.is_finite() {
            bail!("theoretical target scaling overflowed");
        }
        if quantity != 0.0 {
            targets.insert(symbol.clone(), quantity);
        }
    }
    Ok(targets)
}

fn target_positions_changed(previous: Option<&LatestTargets>, next: &LatestTargets) -> bool {
    previous.is_none_or(|previous| {
        previous.venue != next.venue || !target_maps_equal(&previous.targets, &next.targets)
    })
}

fn target_maps_equal(left: &BTreeMap<String, f64>, right: &BTreeMap<String, f64>) -> bool {
    left.len() == right.len()
        && left.iter().all(|(symbol, left_quantity)| {
            right.get(symbol).is_some_and(|right_quantity| {
                quantities_equal(*left_quantity, *right_quantity, left_quantity.abs())
            })
        })
}

fn target_deltas(previous: Option<&LatestTargets>, next: &LatestTargets) -> BTreeMap<String, f64> {
    let empty = BTreeMap::new();
    let previous = previous.map(|value| &value.targets).unwrap_or(&empty);
    previous
        .keys()
        .chain(next.targets.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|symbol| {
            let before = previous.get(&symbol).copied().unwrap_or(0.0);
            let after = next.targets.get(&symbol).copied().unwrap_or(0.0);
            (!quantities_equal(before, after, before.abs()))
                .then_some((symbol, clean_zero(after - before)))
        })
        .collect()
}

fn apply_fifo_fill(
    mut lots: VecDeque<FifoLot>,
    fill_quantity: f64,
    fill_price: f64,
    mut next_lot_seq: i64,
) -> Result<(VecDeque<FifoLot>, f64, i64)> {
    if !fill_quantity.is_finite() || !fill_price.is_finite() || fill_price <= 0.0 {
        bail!("invalid theoretical FIFO fill");
    }
    let mut remaining = fill_quantity;
    let mut realized = 0.0;
    while remaining.abs() > ZERO_EPSILON {
        let Some(front) = lots.front_mut() else {
            break;
        };
        if front.quantity.signum() == remaining.signum() {
            break;
        }
        let matched = front.quantity.abs().min(remaining.abs());
        realized += if front.quantity > 0.0 {
            matched * (fill_price - front.entry_price)
        } else {
            matched * (front.entry_price - fill_price)
        };
        let direction = remaining.signum();
        front.quantity = clean_zero(front.quantity + direction * matched);
        remaining = clean_zero(remaining - direction * matched);
        if front.quantity == 0.0 {
            lots.pop_front();
        }
    }
    if remaining.abs() > ZERO_EPSILON {
        lots.push_back(FifoLot {
            seq: next_lot_seq,
            quantity: remaining,
            entry_price: fill_price,
        });
        next_lot_seq = next_lot_seq
            .checked_add(1)
            .context("theoretical FIFO lot sequence overflowed")?;
    }
    if !realized.is_finite() {
        bail!("theoretical FIFO realized PnL overflowed");
    }
    Ok((lots, clean_zero(realized), next_lot_seq))
}

fn evaluate_fill(
    state: SymbolState,
    lots: VecDeque<FifoLot>,
    fill_quantity: f64,
    fill_price: f64,
    fee_rate: f64,
) -> Result<AppliedFill> {
    if !fee_rate.is_finite() {
        bail!("invalid theoretical fee rate");
    }
    let (lots, realized_increment, next_lot_seq) =
        apply_fifo_fill(lots, fill_quantity, fill_price, state.next_lot_seq)?;
    let net_quantity = clean_zero(lots.iter().map(|lot| lot.quantity).sum());
    let expected_net = clean_zero(state.net_quantity + fill_quantity);
    let comparison_scale = state
        .net_quantity
        .abs()
        .max(fill_quantity.abs())
        .max(net_quantity.abs());
    if !quantities_equal(net_quantity, expected_net, comparison_scale) {
        bail!("theoretical FIFO quantity diverged: fifo={net_quantity} expected={expected_net}");
    }
    let realized = clean_zero(state.realized_pnl_before_fee_quote + realized_increment);
    let fee_quote = fill_quantity.abs() * fill_price * fee_rate;
    let cumulative_fee = clean_zero(state.estimated_trading_fee_quote + fee_quote);
    let floating = floating_pnl_at_mark(&lots, fill_price)?;
    let nav_before = clean_zero(realized + floating);
    let nav_after = clean_zero(nav_before - cumulative_fee);
    if ![
        net_quantity,
        realized,
        fee_quote,
        cumulative_fee,
        floating,
        nav_before,
        nav_after,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        bail!("theoretical NAV overflowed");
    }
    Ok(AppliedFill {
        lots,
        net_quantity,
        realized_pnl_before_fee_quote: realized,
        fee_quote,
        cumulative_fee_quote: cumulative_fee,
        floating_pnl_quote: floating,
        nav_before_fee_quote: nav_before,
        nav_after_fee_quote: nav_after,
        next_lot_seq,
    })
}

fn floating_pnl_at_mark(lots: &VecDeque<FifoLot>, mark_price: f64) -> Result<f64> {
    if !mark_price.is_finite() || mark_price <= 0.0 {
        bail!("invalid theoretical mark price");
    }
    let floating = clean_zero(
        lots.iter()
            .map(|lot| lot.quantity * (mark_price - lot.entry_price))
            .sum(),
    );
    if !floating.is_finite() {
        bail!("theoretical floating PnL overflowed");
    }
    Ok(floating)
}

fn push_or_replace_point(points: &mut Vec<TheoreticalNavPoint>, point: TheoreticalNavPoint) {
    if let Some(last) = points.last_mut()
        && last.ts_us == point.ts_us
    {
        *last = point;
    } else {
        points.push(point);
    }
}

fn downsample_points(
    points: Vec<TheoreticalNavPoint>,
    max_points: usize,
) -> Vec<TheoreticalNavPoint> {
    if points.len() <= max_points || max_points < 6 {
        return points;
    }
    let interior = &points[1..points.len() - 1];
    let bucket_count = ((max_points - 2) / 4).max(1);
    let bucket_size = interior.len().div_ceil(bucket_count);
    let mut sampled = Vec::with_capacity(max_points);
    sampled.push(points[0]);
    for bucket in interior.chunks(bucket_size) {
        let mut extrema = Vec::with_capacity(4);
        for selector in [
            |point: &TheoreticalNavPoint| point.nav_change_before_fee_quote,
            |point: &TheoreticalNavPoint| point.nav_change_after_fee_quote,
        ] {
            if let Some(row) = bucket.iter().min_by(|left, right| {
                selector(left)
                    .partial_cmp(&selector(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                extrema.push(*row);
            }
            if let Some(row) = bucket.iter().max_by(|left, right| {
                selector(left)
                    .partial_cmp(&selector(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                extrema.push(*row);
            }
        }
        extrema.sort_by_key(|row| row.ts_us);
        extrema.dedup_by_key(|row| row.ts_us);
        sampled.extend(extrema);
    }
    sampled.push(*points.last().expect("non-empty theoretical NAV points"));
    sampled
}

fn clean_zero(value: f64) -> f64 {
    if value.abs() <= ZERO_EPSILON {
        0.0
    } else {
        value
    }
}

fn quantities_equal(left: f64, right: f64, scale_hint: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(scale_hint.abs()).max(1.0);
    (left - right).abs() <= ZERO_EPSILON * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn target_cache_coalesces_reads_keeps_fee_and_source_identity_and_resumes_same_timestamp()
    {
        let dir = tempfile::TempDir::new().unwrap();
        let archive = Arc::new(
            PositionArchive::open(crate::manager_db::ManagerDb::open(dir.path()).unwrap()).unwrap(),
        );
        let mut config: AppConfig =
            toml::from_str(include_str!("../config/cta-manager.example.toml")).unwrap();
        config.sources[1].enabled = true;
        let first_source = config.sources[0].id.clone();
        let second_source = config.sources[1].id.clone();
        let config = Arc::new(config);
        let cache = TheoreticalTargetCache::new(config.clone(), archive.clone());
        let mut strategy = crate::strategy_catalog::PositionStrategy {
            strategy_name: "alpha".into(),
            targets: BTreeMap::from([(
                "BTCUSDT".into(),
                crate::order_config::TargetPosition {
                    qty: 1.0,
                    signal: -1,
                },
            )]),
            symbol_order_strategy_overrides: BTreeMap::new(),
            updated_at_us: 1,
        };
        let a = crate::position_archive::published_account(&first_source, "alpha", 2.0);
        let mut b = crate::position_archive::published_account(&second_source, "alpha", 3.0);
        b.theoretical_fee_rate = Some(0.0003);
        archive
            .append(100, &strategy, Vec::new(), vec![a.clone(), b.clone()])
            .unwrap();
        // A repeated target still archives a message but cannot add an execution.
        archive
            .append(100, &strategy, Vec::new(), vec![a.clone(), b.clone()])
            .unwrap();
        strategy.targets.get_mut("BTCUSDT").unwrap().qty = 3.0;
        archive
            .append(101, &strategy, Vec::new(), vec![a, b])
            .unwrap();
        let fees = BTreeMap::from([(first_source.clone(), 0.0004)]);
        let scope_a = vec![first_source.clone()];
        let scope_b = vec![second_source.clone()];
        let (a, b) = tokio::join!(
            cache.query(fees.clone(), 101, &scope_a, Some("alpha")),
            cache.query(BTreeMap::new(), 101, &scope_b, None)
        );
        let a = a.unwrap().unwrap();
        let b = b.unwrap().unwrap();
        assert_eq!(
            a.iter().map(|delta| delta.delta_qty).collect::<Vec<_>>(),
            [2.0, 4.0]
        );
        assert!(a.iter().all(|delta| delta.legacy_fee
            && delta.fee_rate == 0.0004
            && delta.target_signal == -1));
        assert_eq!(
            b.iter().map(|delta| delta.delta_qty).collect::<Vec<_>>(),
            [3.0, 6.0]
        );
        assert!(
            b.iter()
                .all(|delta| !delta.legacy_fee && delta.fee_rate == 0.0003)
        );
        assert_eq!(cache.status().processed_messages, 3);
        let mut stopped = crate::position_archive::published_account(&first_source, "alpha", 0.0);
        stopped.theoretical_fee_rate = Some(-0.0001);
        archive
            .append(101, &strategy, Vec::new(), vec![stopped])
            .unwrap();
        let changed_fees = BTreeMap::from([(first_source.clone(), 0.0008)]);
        let resumed = cache
            .query(changed_fees.clone(), 101, &scope_a, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resumed.len(), 3);
        assert_eq!(resumed[0].fee_rate, 0.0008);
        assert_eq!(resumed[2].delta_qty, -6.0);
        assert_eq!(resumed[2].seq, 1);
        assert_eq!(resumed[2].fee_rate, -0.0001);
        assert_eq!(cache.status().processed_messages, 4);
        assert_eq!(
            cache
                .query(fees, 100, &scope_a, None)
                .await
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(cache.status().processed_messages, 4);
        let restarted = TheoreticalTargetCache::new(config, archive);
        assert_eq!(
            restarted
                .query(changed_fees, 101, &scope_a, None)
                .await
                .unwrap()
                .unwrap(),
            resumed
        );
        assert!(
            cache
                .query(BTreeMap::new(), 101, &scope_a, None)
                .await
                .is_err()
        );
    }

    fn cached_market() -> (tempfile::TempDir, KlineStore, i64, Vec<crate::kline::Kline>) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = KlineStore::from_db(
            crate::manager_db::ManagerDb::open(dir.path()).unwrap(),
            Default::default(),
        )
        .unwrap();
        let start = now_us().div_euclid(MINUTE_US) * MINUTE_US - crate::kline::DAY_US;
        let volumes = [1.0, 100.0, 1.0, 1000.0, 1.0];
        let bars = (0..6)
            .map(|index| {
                let volume = if index == 0 { 1.0 } else { volumes[index - 1] };
                let price = if index == 0 {
                    100.0
                } else {
                    99.0 + index as f64
                };
                crate::kline::Kline {
                    open_ts_us: start + (index as i64 - 1) * MINUTE_US,
                    open: 100.0,
                    high: 110.0,
                    low: 90.0,
                    close: if index == 0 { 100.0 } else { 105.0 },
                    base_volume: volume,
                    quote_volume: volume * price,
                    trades: 10,
                }
            })
            .collect::<Vec<_>>();
        store
            .save_page("BTCUSDT", start - MINUTE_US, start + 5 * MINUTE_US, &bars)
            .unwrap();
        (dir, store, start, bars)
    }
    fn delta(start: i64) -> VirtualDelta {
        VirtualDelta {
            source_id: "test".into(),
            binding_name: "a".into(),
            strategy_name: "a".into(),
            symbol: "BTCUSDT".into(),
            venue: "binance-futures".into(),
            received_at_us: start,
            seq: 0,
            delta_qty: 5.0,
            fee_rate: 0.0002,
            legacy_fee: false,
            target_signal: 0,
        }
    }
    #[test]
    fn minutes_get_equal_quantity_despite_different_volume_and_later_targets_do_not_truncate() {
        let (_dir, store, start, bars) = cached_market();
        let first = delta(start);
        let samples = first.prices(&store).unwrap().unwrap();
        assert_eq!(samples, [100.0, 101.0, 102.0, 103.0, 104.0]);
        assert_eq!(samples.iter().sum::<f64>() / 5.0, 102.0);
        let weighted = bars[1..].iter().map(|c| c.quote_volume).sum::<f64>()
            / bars[1..].iter().map(|c| c.base_volume).sum::<f64>();
        assert!((weighted - 102.0).abs() > 0.5);
        let mut second = first.clone();
        second.received_at_us += MINUTE_US;
        second.delta_qty = -2.0;
        assert_eq!(first.execution_ts_us(), start + 5 * MINUTE_US);
        assert_eq!(second.execution_ts_us(), start + 6 * MINUTE_US);
    }
    #[test]
    fn missing_minute_suppresses_nav_and_cache_repair_reconstructs_without_ghost_holdings() {
        let (_dir, store, start, bars) = cached_market();
        let delta = delta(start);
        let fills = (0..5)
            .map(|slice| (start + (slice as i64 + 1) * MINUTE_US, 0, slice))
            .collect::<Vec<_>>();
        let mut output = TheoreticalNavTimeline::default();
        rebuild_timeline(
            &[delta.clone()],
            BTreeMap::new(),
            fills.clone(),
            &store,
            start,
            start + 5 * MINUTE_US,
            100,
            &mut output,
        )
        .unwrap();
        let final_point = output.points.last().unwrap();
        assert_eq!(output.points[0].nav_change_before_fee_quote, 0.0);
        assert!((final_point.nav_change_before_fee_quote - 15.0).abs() < 1e-10);
        assert!((final_point.estimated_trading_fee_quote - 0.102).abs() < 1e-10);
        let mut missing = bars.clone();
        missing[3].base_volume = 0.0;
        missing[3].quote_volume = 0.0;
        store
            .save_page(
                "BTCUSDT",
                start - MINUTE_US,
                start + 5 * MINUTE_US,
                &missing,
            )
            .unwrap();
        assert!(delta.prices(&store).unwrap().is_none());
        let mut unavailable = TheoreticalNavTimeline::default();
        rebuild_timeline(
            &[delta.clone()],
            BTreeMap::new(),
            fills.clone(),
            &store,
            start,
            start + 5 * MINUTE_US,
            100,
            &mut unavailable,
        )
        .unwrap();
        assert!(unavailable.points.is_empty());
        assert!(unavailable.unavailable_reason.is_some());
        store
            .save_page("BTCUSDT", start - MINUTE_US, start + 5 * MINUTE_US, &bars)
            .unwrap();
        let mut repaired = TheoreticalNavTimeline::default();
        rebuild_timeline(
            &[delta],
            BTreeMap::new(),
            fills,
            &store,
            start,
            start + 5 * MINUTE_US,
            100,
            &mut repaired,
        )
        .unwrap();
        assert_eq!(output.points, repaired.points);
    }
    #[test]
    fn carried_inventory_uses_zero_fee_mark_anchor() {
        let (_dir, store, start, _) = cached_market();
        let carry = BTreeMap::from([(
            ("test".into(), "BTCUSDT".into(), "binance-futures".into()),
            2.0,
        )]);
        let delta = delta(start);
        let fills = (0..5)
            .map(|slice| (start + (slice as i64 + 1) * MINUTE_US, 0, slice))
            .collect();
        let mut output = TheoreticalNavTimeline::default();
        rebuild_timeline(
            &[delta],
            carry,
            fills,
            &store,
            start,
            start + 5 * MINUTE_US,
            100,
            &mut output,
        )
        .unwrap();
        let final_point = output.points.last().unwrap();
        assert!((final_point.nav_change_before_fee_quote - 25.0).abs() < 1e-10);
        assert!((final_point.estimated_trading_fee_quote - 0.102).abs() < 1e-10);
    }
    fn latest_targets(targets: BTreeMap<String, f64>) -> LatestTargets {
        LatestTargets {
            position_strategy_name: "cta_a".into(),
            venue: "binance-futures".into(),
            targets,
            received_at_us: 1,
            update_seq: 0,
        }
    }
    #[test]
    fn repeated_target_metadata_does_not_create_an_execution() {
        let previous = latest_targets(BTreeMap::from([("BTCUSDT".into(), 1.0)]));
        let mut repeated = previous.clone();
        repeated.position_strategy_name = "renamed".into();
        repeated.received_at_us = 2;
        repeated.update_seq = 4;
        assert!(!target_positions_changed(Some(&previous), &repeated));

        repeated.targets.insert("BTCUSDT".into(), 2.0);
        assert!(target_positions_changed(Some(&previous), &repeated));
    }

    #[test]
    fn persisted_float_tail_does_not_create_an_execution() {
        let previous = latest_targets(BTreeMap::from([
            ("SOPHUSDT".into(), -58_485.413_990_491_346),
            ("ORCAUSDT".into(), 404.123_456_789),
        ]));
        let mut repeated = previous.clone();
        repeated.received_at_us = 2;
        repeated
            .targets
            .insert("SOPHUSDT".into(), -58_485.413_990_491_34);
        assert!(!target_positions_changed(Some(&previous), &repeated));

        repeated.targets.insert("ORCAUSDT".into(), 404.124);
        assert!(target_positions_changed(Some(&previous), &repeated));
    }

    #[test]
    fn target_delta_is_frozen_between_distinct_vectors() {
        let previous = latest_targets(BTreeMap::from([
            ("BTCUSDT".into(), 2.0),
            ("ETHUSDT".into(), -3.0),
        ]));
        let next = latest_targets(BTreeMap::from([
            ("BTCUSDT".into(), 5.0),
            ("SOLUSDT".into(), 7.0),
        ]));
        assert_eq!(
            target_deltas(Some(&previous), &next),
            BTreeMap::from([
                ("BTCUSDT".into(), 3.0),
                ("ETHUSDT".into(), 3.0),
                ("SOLUSDT".into(), 7.0),
            ])
        );
    }

    #[test]
    fn fifo_quantity_check_scales_with_large_positions() {
        assert!(quantities_equal(100_000.0, 100_000.0 + 1e-8, 100_000.0));
        assert!(!quantities_equal(100_000.0, 100_000.0 + 1e-4, 100_000.0));
        assert!(quantities_equal(0.0, 1.82e-12, 50_000.0));
        assert!(!quantities_equal(1.0, 1.0 + 1e-8, 1.0));
    }

    #[test]
    fn open_position_keeps_accruing_pnl_between_signals() {
        let lots = VecDeque::from([
            FifoLot {
                seq: 1,
                quantity: 2.0,
                entry_price: 100.0,
            },
            FifoLot {
                seq: 2,
                quantity: 0.5,
                entry_price: 110.0,
            },
        ]);
        assert_eq!(floating_pnl_at_mark(&lots, 120.0).unwrap(), 45.0);
        assert_eq!(floating_pnl_at_mark(&lots, 90.0).unwrap(), -30.0);
    }

    #[test]
    fn fifo_realizes_long_and_keeps_the_remainder() {
        let lots = VecDeque::from([FifoLot {
            seq: 1,
            quantity: 2.0,
            entry_price: 100.0,
        }]);
        let (lots, realized, next) = apply_fifo_fill(lots, -1.5, 110.0, 2).unwrap();
        assert_eq!(realized, 15.0);
        assert_eq!(next, 2);
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].quantity, 0.5);
    }

    #[test]
    fn fifo_crosses_from_short_to_long() {
        let lots = VecDeque::from([FifoLot {
            seq: 1,
            quantity: -1.0,
            entry_price: 100.0,
        }]);
        let (lots, realized, next) = apply_fifo_fill(lots, 1.5, 90.0, 2).unwrap();
        assert_eq!(realized, 10.0);
        assert_eq!(next, 3);
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].quantity, 0.5);
        assert_eq!(lots[0].entry_price, 90.0);
    }

    #[test]
    fn theoretical_nav_has_before_and_after_fee_values() {
        let applied = evaluate_fill(
            SymbolState {
                next_lot_seq: 1,
                ..SymbolState::default()
            },
            VecDeque::new(),
            2.0,
            100.0,
            0.001,
        )
        .unwrap();
        assert_eq!(applied.nav_before_fee_quote, 0.0);
        assert!((applied.fee_quote - 0.2).abs() < 1e-12);
        assert!((applied.nav_after_fee_quote + 0.2).abs() < 1e-12);

        let rebate = evaluate_fill(
            SymbolState {
                next_lot_seq: 1,
                ..SymbolState::default()
            },
            VecDeque::new(),
            2.0,
            100.0,
            -0.001,
        )
        .unwrap();
        assert!((rebate.nav_after_fee_quote - 0.2).abs() < 1e-12);
    }

    #[test]
    fn repeated_timestamp_replaces_the_sparse_portfolio_point() {
        let mut points = vec![TheoreticalNavPoint {
            ts_us: 10,
            nav_change_before_fee_quote: 1.0,
            ..TheoreticalNavPoint::default()
        }];
        push_or_replace_point(
            &mut points,
            TheoreticalNavPoint {
                ts_us: 10,
                nav_change_before_fee_quote: 2.0,
                ..TheoreticalNavPoint::default()
            },
        );
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].nav_change_before_fee_quote, 2.0);
    }
}
