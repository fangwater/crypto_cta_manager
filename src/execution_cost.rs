use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{AppConfig, SourceConfig};
use crate::nav;
use crate::position_archive::{
    ArchivedPublishedAccount, ArchivedSourcePositions, PositionArchive, PositionUpdateMsg,
};
use crate::twap::{TwapBar, TwapStore};

pub const DEFAULT_WINDOW_SECS: u64 = 300;
pub const MAX_WINDOW_SECS: u64 = 86_400;
pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const MAX_PAGE_SIZE: usize = 100;
pub const MINUTE_TWAP_SECS: u64 = 60;
const MINUTE_US: i64 = 60_000_000;
const MAX_COST_POINTS: usize = 2_000;

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionCostReport {
    pub generated_at_us: i64,
    pub window_secs: u64,
    pub twap_secs: u64,
    pub price_basis: &'static str,
    pub fee_basis: &'static str,
    pub actual_fee_basis: &'static str,
    pub start_received_at_us: i64,
    pub end_received_at_us: Option<i64>,
    pub source_ids: Vec<String>,
    pub strategy_name: Option<String>,
    pub update_count: usize,
    pub page: usize,
    pub page_size: usize,
    pub page_count: usize,
    pub returned_update_count: usize,
    pub skipped_legacy_update_count: usize,
    pub totals: CostTotals,
    pub points: Vec<ExecutionCostPoint>,
    pub updates: Vec<PositionUpdateCost>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct CostTotals {
    pub intended_qty: f64,
    pub filled_qty: f64,
    pub arrival_notional_usdt: f64,
    pub twap_notional_usdt: f64,
    pub actual_notional_usdt: f64,
    pub twap_cost_before_fee_usdt: f64,
    pub actual_cost_before_fee_usdt: f64,
    pub estimated_trading_fee_usdt: f64,
    pub actual_cost_after_fee_usdt: f64,
}

impl CostTotals {
    fn add(&mut self, other: CostTotals) {
        self.intended_qty += other.intended_qty;
        self.filled_qty += other.filled_qty;
        self.arrival_notional_usdt += other.arrival_notional_usdt;
        self.twap_notional_usdt += other.twap_notional_usdt;
        self.actual_notional_usdt += other.actual_notional_usdt;
        self.twap_cost_before_fee_usdt += other.twap_cost_before_fee_usdt;
        self.actual_cost_before_fee_usdt += other.actual_cost_before_fee_usdt;
        self.estimated_trading_fee_usdt += other.estimated_trading_fee_usdt;
        self.actual_cost_after_fee_usdt =
            self.actual_cost_before_fee_usdt + self.estimated_trading_fee_usdt;
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ExecutionCostPoint {
    pub ts_us: i64,
    pub twap_cost_before_fee_usdt: f64,
    pub actual_cost_before_fee_usdt: f64,
    pub estimated_trading_fee_usdt: f64,
    pub actual_cost_after_fee_usdt: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionUpdateCost {
    pub received_at_us: i64,
    pub seq: u32,
    pub schema_version: u32,
    pub strategy_name: String,
    pub window_start_us: i64,
    pub window_end_us: i64,
    pub skipped_legacy: bool,
    pub totals: CostTotals,
    pub accounts: Vec<AccountUpdateCost>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountUpdateCost {
    pub source_id: String,
    pub binding_name: String,
    pub shares: f64,
    pub snapshot_ts_ms: Option<i64>,
    pub position_ready: Option<bool>,
    pub totals: CostTotals,
    pub symbols: Vec<SymbolUpdateCost>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolUpdateCost {
    pub symbol: String,
    pub template_qty: f64,
    pub published_qty: f64,
    pub snapshot_qty: f64,
    pub intended_qty: f64,
    pub filled_qty: f64,
    pub fill_count: u64,
    pub minute_bar_count: u32,
    pub missing_minute_bar_count: u32,
    pub arrival_mid: Option<f64>,
    pub twap_mid: Option<f64>,
    pub actual_vwap: Option<f64>,
    pub arrival_notional_usdt: Option<f64>,
    pub twap_notional_usdt: Option<f64>,
    pub actual_notional_usdt: Option<f64>,
    pub twap_cost_before_fee_usdt: Option<f64>,
    pub actual_cost_before_fee_usdt: Option<f64>,
    pub estimated_trading_fee_usdt: f64,
    pub actual_cost_after_fee_usdt: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct SignedFill {
    qty: f64,
    price: f64,
    estimated_fee_usdt: f64,
}

type TwapMarketKey = (String, String);
type TwapRangesByMarket = BTreeMap<TwapMarketKey, (i64, i64)>;
type TwapBarsByMarket = BTreeMap<TwapMarketKey, Vec<TwapBar>>;

pub fn report_execution_cost(
    config: &AppConfig,
    archive: &PositionArchive,
    twap: &TwapStore,
    start_received_at_us: i64,
    end_received_at_us: Option<i64>,
    window_secs: u64,
    generated_at_us: i64,
    source_ids: &[String],
    strategy_name: Option<&str>,
    page: usize,
    page_size: usize,
    histories: &nav::NavSourceHistories,
) -> Result<ExecutionCostReport> {
    if start_received_at_us < 0 {
        bail!("start timestamp must not be negative");
    }
    if let Some(end) = end_received_at_us {
        if end < start_received_at_us {
            bail!("end timestamp must not be before start timestamp");
        }
    }
    if window_secs == 0 {
        bail!("windowSecs must be greater than zero");
    }
    if window_secs > MAX_WINDOW_SECS {
        bail!("windowSecs must not exceed {MAX_WINDOW_SECS}");
    }
    if page == 0 {
        bail!("page must be greater than zero");
    }
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        bail!("pageSize must be between 1 and {MAX_PAGE_SIZE}");
    }
    let total_started = Instant::now();
    let window_us = i64::try_from(window_secs)
        .ok()
        .and_then(|value| value.checked_mul(1_000_000))
        .context("windowSecs is too large")?;
    let selected_sources: BTreeSet<&str> = source_ids.iter().map(String::as_str).collect();
    if selected_sources.len() != source_ids.len() {
        bail!("sourceIds must not contain duplicates");
    }
    for source_id in &selected_sources {
        if !config.sources.iter().any(|source| source.id == *source_id) {
            bail!("sourceIds contains an unknown source: {source_id}");
        }
    }
    let strategy_name = strategy_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let messages = archive.scan_from(start_received_at_us.max(1))?;
    let archive_ms = total_started.elapsed().as_millis();
    let matching: Vec<PositionUpdateMsg> = messages
        .into_iter()
        .filter(|msg| {
            strategy_name
                .as_deref()
                .is_none_or(|name| msg.strategy.strategy_name == name)
        })
        .collect();
    let next_same_strategy = next_same_strategy_starts(&matching);
    let selected: Vec<(usize, &PositionUpdateMsg)> = matching
        .iter()
        .enumerate()
        .filter(|(_, msg)| {
            msg.received_at_us <= generated_at_us
                && end_received_at_us.is_none_or(|end| msg.received_at_us <= end)
                && message_matches_sources(msg, &selected_sources)
        })
        .collect();
    let needed = needed_fill_ranges(
        &matching,
        window_us,
        generated_at_us,
        &next_same_strategy,
        &selected_sources,
        end_received_at_us,
    );
    let fills_started = Instant::now();
    let fills = load_signed_fills_from_histories(config, &needed, histories)?;
    let fills_ms = fills_started.elapsed().as_millis();
    let twap_ranges = needed_twap_ranges(
        config,
        &selected,
        window_us,
        generated_at_us,
        &next_same_strategy,
        &selected_sources,
    );
    let twap_started = Instant::now();
    let twap_bars = load_twap_bars(twap, &twap_ranges)?;
    let twap_ms = twap_started.elapsed().as_millis();

    let update_count = selected.len();
    let page_count = update_count.div_ceil(page_size);
    let page_offset = page.saturating_sub(1).saturating_mul(page_size);
    let detail_end = update_count.saturating_sub(page_offset);
    let detail_start = detail_end.saturating_sub(page_size);
    let mut updates = Vec::with_capacity(detail_end.saturating_sub(detail_start));
    let mut totals = CostTotals::default();
    let mut points = Vec::with_capacity(update_count.min(MAX_COST_POINTS));
    let mut skipped_legacy_update_count = 0usize;
    let compute_started = Instant::now();
    for (position, (index, msg)) in selected.into_iter().enumerate() {
        let window_end_us = execution_window_end(
            msg,
            next_same_strategy.get(index).copied().flatten(),
            window_us,
            generated_at_us,
        );
        let include_details = position >= detail_start && position < detail_end;
        let update = cost_for_update(
            config,
            &twap_bars,
            msg,
            window_end_us,
            &fills,
            &selected_sources,
            include_details,
        )?;
        if update.skipped_legacy {
            skipped_legacy_update_count += 1;
        }
        totals.add(update.totals);
        points.push(ExecutionCostPoint {
            ts_us: msg.received_at_us,
            twap_cost_before_fee_usdt: totals.twap_cost_before_fee_usdt,
            actual_cost_before_fee_usdt: totals.actual_cost_before_fee_usdt,
            estimated_trading_fee_usdt: totals.estimated_trading_fee_usdt,
            actual_cost_after_fee_usdt: totals.actual_cost_after_fee_usdt,
        });
        if include_details {
            updates.push(update);
        }
    }
    let compute_ms = compute_started.elapsed().as_millis();
    tracing::info!(
        update_count,
        returned_update_count = updates.len(),
        archive_ms,
        fills_ms,
        twap_ms,
        compute_ms,
        total_ms = total_started.elapsed().as_millis(),
        "generated execution-cost report"
    );

    Ok(ExecutionCostReport {
        generated_at_us,
        window_secs,
        twap_secs: MINUTE_TWAP_SECS,
        price_basis: "1m_mid_twap",
        fee_basis: "before_fee",
        actual_fee_basis: "maker_taker_estimated",
        start_received_at_us,
        end_received_at_us,
        source_ids: source_ids.to_vec(),
        strategy_name,
        update_count,
        page,
        page_size,
        page_count,
        returned_update_count: updates.len(),
        skipped_legacy_update_count,
        totals,
        points: downsample_cost_points(points, MAX_COST_POINTS),
        updates,
    })
}

fn cost_for_update(
    config: &AppConfig,
    twap_bars: &TwapBarsByMarket,
    msg: &PositionUpdateMsg,
    window_end_us: i64,
    fills: &BTreeMap<(String, String, String), Vec<(i64, SignedFill)>>,
    selected_sources: &BTreeSet<&str>,
    include_details: bool,
) -> Result<PositionUpdateCost> {
    let skipped_legacy = msg.published_accounts.is_empty();
    let mut accounts = Vec::with_capacity(msg.published_accounts.len());
    let mut totals = CostTotals::default();
    for account in &msg.published_accounts {
        if !selected_sources.is_empty() && !selected_sources.contains(account.source_id.as_str()) {
            continue;
        }
        let snapshot = msg
            .factual_positions
            .iter()
            .find(|item| item.source_id == account.source_id);
        let source = config
            .sources
            .iter()
            .find(|source| source.id == account.source_id);
        let venue = source
            .map(|source| source.venue.as_str())
            .unwrap_or("binance-futures");
        let account_cost = cost_for_account(
            twap_bars,
            msg,
            account,
            snapshot,
            venue,
            window_end_us,
            fills,
            include_details,
        )?;
        totals.add(account_cost.totals);
        if include_details {
            accounts.push(account_cost);
        }
    }
    Ok(PositionUpdateCost {
        received_at_us: msg.received_at_us,
        seq: msg.seq,
        schema_version: msg.schema_version,
        strategy_name: msg.strategy.strategy_name.clone(),
        window_start_us: msg.received_at_us,
        window_end_us,
        skipped_legacy,
        totals,
        accounts,
    })
}

fn cost_for_account(
    twap_bars: &TwapBarsByMarket,
    msg: &PositionUpdateMsg,
    account: &ArchivedPublishedAccount,
    snapshot: Option<&ArchivedSourcePositions>,
    venue: &str,
    window_end_us: i64,
    fills: &BTreeMap<(String, String, String), Vec<(i64, SignedFill)>>,
    include_details: bool,
) -> Result<AccountUpdateCost> {
    let snapshot_qty = snapshot_qty_by_symbol(snapshot);
    let mut symbols = BTreeSet::new();
    symbols.extend(msg.strategy.targets.keys().cloned());
    symbols.extend(snapshot_qty.keys().cloned());
    let mut rows = Vec::new();
    let mut totals = CostTotals::default();
    for symbol in symbols {
        let template = msg
            .strategy
            .targets
            .get(&symbol)
            .map(|target| target.qty)
            .unwrap_or(0.0);
        let published = template * account.effective_shares();
        let snap = snapshot_qty.get(&symbol).copied().unwrap_or(0.0);
        let intended = published - snap;
        let bars = bars_for_window(twap_bars, &symbol, venue, msg.received_at_us, window_end_us);
        let (minute_buckets, missing_minute_bar_count) =
            minute_twap_buckets(&bars, msg.received_at_us, window_end_us);
        let arrival_mid = minute_buckets.first().map(|bucket| bucket.mid);
        let twap_mid = duration_weighted_mid(&minute_buckets);
        let key = (
            account.source_id.clone(),
            msg.strategy.strategy_name.clone(),
            symbol.clone(),
        );
        let signed_fills: Vec<SignedFill> = fills
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|(ts, _)| *ts >= msg.received_at_us && *ts < window_end_us)
            .map(|(_, fill)| *fill)
            .collect();
        let filled_qty: f64 = signed_fills.iter().map(|fill| fill.qty).sum();
        let actual_notional: f64 = signed_fills.iter().map(|fill| fill.qty * fill.price).sum();
        let estimated_fee: f64 = signed_fills
            .iter()
            .map(|fill| fill.estimated_fee_usdt)
            .sum();
        let actual_vwap = if filled_qty.abs() > 0.0 {
            Some(actual_notional / filled_qty)
        } else {
            None
        };
        let arrival_notional = arrival_mid.map(|price| intended * price);
        let twap_notional = twap_mid.map(|price| intended * price);
        let twap_cost = match (twap_notional, arrival_notional) {
            (Some(exec), Some(arrive)) => Some(exec - arrive),
            _ => None,
        };
        let actual_cost = match (actual_vwap, arrival_mid) {
            (Some(vwap), Some(arrive)) => Some(filled_qty * (vwap - arrive)),
            _ => None,
        };
        let actual_cost_after_fee = actual_cost.map(|cost| cost + estimated_fee);
        let row = SymbolUpdateCost {
            symbol,
            template_qty: template,
            published_qty: published,
            snapshot_qty: snap,
            intended_qty: intended,
            filled_qty,
            fill_count: signed_fills.len() as u64,
            minute_bar_count: minute_buckets.len() as u32,
            missing_minute_bar_count,
            arrival_mid,
            twap_mid,
            actual_vwap,
            arrival_notional_usdt: arrival_notional,
            twap_notional_usdt: twap_notional,
            actual_notional_usdt: if filled_qty.abs() > 0.0 {
                Some(actual_notional)
            } else {
                None
            },
            twap_cost_before_fee_usdt: twap_cost,
            actual_cost_before_fee_usdt: actual_cost,
            estimated_trading_fee_usdt: estimated_fee,
            actual_cost_after_fee_usdt: actual_cost_after_fee,
        };
        totals.intended_qty += row.intended_qty;
        totals.filled_qty += row.filled_qty;
        totals.arrival_notional_usdt += row.arrival_notional_usdt.unwrap_or(0.0);
        totals.twap_notional_usdt += row.twap_notional_usdt.unwrap_or(0.0);
        totals.actual_notional_usdt += row.actual_notional_usdt.unwrap_or(0.0);
        totals.twap_cost_before_fee_usdt += row.twap_cost_before_fee_usdt.unwrap_or(0.0);
        totals.actual_cost_before_fee_usdt += row.actual_cost_before_fee_usdt.unwrap_or(0.0);
        totals.estimated_trading_fee_usdt += row.estimated_trading_fee_usdt;
        totals.actual_cost_after_fee_usdt =
            totals.actual_cost_before_fee_usdt + totals.estimated_trading_fee_usdt;
        if include_details {
            rows.push(row);
        }
    }
    Ok(AccountUpdateCost {
        source_id: account.source_id.clone(),
        binding_name: account.binding_name.clone(),
        shares: account.effective_shares(),
        snapshot_ts_ms: snapshot.map(|item| item.snapshot_ts_ms),
        position_ready: snapshot.map(|item| item.position_ready),
        totals,
        symbols: rows,
    })
}

fn snapshot_qty_by_symbol(snapshot: Option<&ArchivedSourcePositions>) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Some(snapshot) = snapshot else {
        return out;
    };
    for position in &snapshot.positions {
        out.insert(position.symbol.clone(), position.qty);
    }
    out
}

fn message_matches_sources(msg: &PositionUpdateMsg, selected_sources: &BTreeSet<&str>) -> bool {
    selected_sources.is_empty()
        || msg.published_accounts.is_empty()
        || msg
            .published_accounts
            .iter()
            .any(|account| selected_sources.contains(account.source_id.as_str()))
}

fn next_same_strategy_starts(messages: &[PositionUpdateMsg]) -> Vec<Option<i64>> {
    let mut next = vec![None; messages.len()];
    let mut upcoming: BTreeMap<String, i64> = BTreeMap::new();
    for (index, msg) in messages.iter().enumerate().rev() {
        next[index] = upcoming.get(&msg.strategy.strategy_name).copied();
        upcoming.insert(msg.strategy.strategy_name.clone(), msg.received_at_us);
    }
    next
}

fn execution_window_end(
    msg: &PositionUpdateMsg,
    next_same_strategy: Option<i64>,
    window_us: i64,
    generated_at_us: i64,
) -> i64 {
    let mut end = msg.received_at_us.saturating_add(window_us);
    if let Some(next) = next_same_strategy {
        end = end.min(next);
    }
    end.min(generated_at_us.max(msg.received_at_us))
}

fn needed_fill_ranges(
    messages: &[PositionUpdateMsg],
    window_us: i64,
    generated_at_us: i64,
    next_same_strategy: &[Option<i64>],
    selected_sources: &BTreeSet<&str>,
    end_received_at_us: Option<i64>,
) -> BTreeMap<String, (i64, i64)> {
    let mut ranges = BTreeMap::<String, (i64, i64)>::new();
    for (index, msg) in messages.iter().enumerate() {
        if msg.published_accounts.is_empty() {
            continue;
        }
        if msg.received_at_us > generated_at_us {
            continue;
        }
        if end_received_at_us.is_some_and(|end| msg.received_at_us > end) {
            continue;
        }
        let end = execution_window_end(
            msg,
            next_same_strategy.get(index).copied().flatten(),
            window_us,
            generated_at_us,
        );
        for account in &msg.published_accounts {
            if !selected_sources.is_empty()
                && !selected_sources.contains(account.source_id.as_str())
            {
                continue;
            }
            let entry = ranges
                .entry(account.source_id.clone())
                .or_insert((msg.received_at_us, end));
            entry.0 = entry.0.min(msg.received_at_us);
            entry.1 = entry.1.max(end);
        }
    }
    ranges
}

fn needed_twap_ranges(
    config: &AppConfig,
    selected: &[(usize, &PositionUpdateMsg)],
    window_us: i64,
    generated_at_us: i64,
    next_same_strategy: &[Option<i64>],
    selected_sources: &BTreeSet<&str>,
) -> TwapRangesByMarket {
    let mut ranges = TwapRangesByMarket::new();
    for (index, msg) in selected {
        if msg.published_accounts.is_empty() {
            continue;
        }
        let end = execution_window_end(
            msg,
            next_same_strategy.get(*index).copied().flatten(),
            window_us,
            generated_at_us,
        );
        for account in &msg.published_accounts {
            if !selected_sources.is_empty()
                && !selected_sources.contains(account.source_id.as_str())
            {
                continue;
            }
            let venue = config
                .sources
                .iter()
                .find(|source| source.id == account.source_id)
                .map(|source| source.venue.as_str())
                .unwrap_or("binance-futures");
            let snapshot = msg
                .factual_positions
                .iter()
                .find(|item| item.source_id == account.source_id);
            let mut symbols = msg
                .strategy
                .targets
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>();
            if let Some(snapshot) = snapshot {
                symbols.extend(
                    snapshot
                        .positions
                        .iter()
                        .map(|position| position.symbol.clone()),
                );
            }
            for symbol in symbols {
                let entry = ranges
                    .entry((symbol, venue.to_string()))
                    .or_insert((msg.received_at_us, end));
                entry.0 = entry.0.min(msg.received_at_us);
                entry.1 = entry.1.max(end);
            }
        }
    }
    ranges
}

fn load_twap_bars(twap: &TwapStore, ranges: &TwapRangesByMarket) -> Result<TwapBarsByMarket> {
    let mut out = TwapBarsByMarket::new();
    for ((symbol, venue), (start, end)) in ranges {
        let bars = twap.scan_bars(symbol, venue, *start, end.saturating_add(1))?;
        out.insert((symbol.clone(), venue.clone()), bars);
    }
    Ok(out)
}

fn bars_for_window<'a>(
    bars_by_market: &'a TwapBarsByMarket,
    symbol: &str,
    venue: &str,
    start_us: i64,
    end_us: i64,
) -> &'a [TwapBar] {
    let Some(bars) = bars_by_market.get(&(symbol.to_string(), venue.to_string())) else {
        return &[];
    };
    let start = bars.partition_point(|bar| bar.end_ts_us <= start_us);
    let end = bars.partition_point(|bar| bar.end_ts_us <= end_us);
    bars.get(start..end).unwrap_or_default()
}

fn load_signed_fills_from_histories(
    config: &AppConfig,
    ranges: &BTreeMap<String, (i64, i64)>,
    histories: &nav::NavSourceHistories,
) -> Result<BTreeMap<(String, String, String), Vec<(i64, SignedFill)>>> {
    let mut out = BTreeMap::<(String, String, String), Vec<(i64, SignedFill)>>::new();
    for (source_id, (start, end)) in ranges {
        let Some(source) = config
            .sources
            .iter()
            .find(|source| source.id == *source_id && source.enabled)
        else {
            continue;
        };
        let history = histories
            .get(source_id)
            .with_context(|| format!("NAV history is missing execution-cost source {source_id}"))?;
        append_source_fills_from_history(source, history, *start, *end, &mut out)?;
    }
    Ok(out)
}

fn append_source_fills_from_history(
    source: &SourceConfig,
    history: &nav::NavSourceHistory,
    start_ts_us: i64,
    end_ts_us: i64,
    out: &mut BTreeMap<(String, String, String), Vec<(i64, SignedFill)>>,
) -> Result<()> {
    for event in history
        .events()
        .iter()
        .filter(|event| event.event_ts_us >= start_ts_us && event.event_ts_us < end_ts_us)
    {
        if event.amount_update <= 0.0 || !event.price.is_finite() || event.price <= 0.0 {
            continue;
        }
        let signed_qty = match event.side_code {
            1 => event.amount_update,
            2 => -event.amount_update,
            _ => continue,
        };
        let strategy = nav::strategy_from_from_key(&event.from_key_text);
        let estimated_fee_usdt = history.estimated_fee_quote(source, event)?;
        out.entry((source.id.clone(), strategy, event.symbol.clone()))
            .or_default()
            .push((
                event.update_ts_us,
                SignedFill {
                    qty: signed_qty,
                    price: event.price,
                    estimated_fee_usdt,
                },
            ));
    }
    Ok(())
}

fn downsample_cost_points(
    points: Vec<ExecutionCostPoint>,
    max_points: usize,
) -> Vec<ExecutionCostPoint> {
    if points.len() <= max_points || max_points < 2 {
        return points;
    }
    let last = points.len() - 1;
    (0..max_points)
        .map(|index| points[index * last / (max_points - 1)])
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct MinuteTwapBucket {
    mid: f64,
    duration_us: i64,
}

fn minute_twap_buckets(
    bars: &[TwapBar],
    start_us: i64,
    end_us: i64,
) -> (Vec<MinuteTwapBucket>, u32) {
    if end_us <= start_us {
        return (Vec::new(), 0);
    }
    let mut buckets = Vec::new();
    let mut missing = 0u32;
    let mut bucket_start = start_us;
    while bucket_start < end_us {
        let bucket_end = bucket_start.saturating_add(MINUTE_US).min(end_us);
        let duration_us = bucket_end - bucket_start;
        let mut sum = 0.0;
        let mut count = 0u32;
        for bar in bars {
            if bar.end_ts_us > bucket_start && bar.end_ts_us <= bucket_end {
                sum += bar.twap;
                count += 1;
            }
        }
        if count > 0 && duration_us > 0 {
            buckets.push(MinuteTwapBucket {
                mid: sum / f64::from(count),
                duration_us,
            });
        } else {
            missing += 1;
        }
        if bucket_end <= bucket_start {
            break;
        }
        bucket_start = bucket_end;
    }
    (buckets, missing)
}

fn duration_weighted_mid(buckets: &[MinuteTwapBucket]) -> Option<f64> {
    let mut weighted = 0.0;
    let mut duration = 0.0;
    for bucket in buckets {
        let seconds = bucket.duration_us as f64;
        weighted += bucket.mid * seconds;
        duration += seconds;
    }
    if duration > 0.0 {
        Some(weighted / duration)
    } else {
        None
    }
}

pub fn intended_qty(template_qty: f64, shares: f64, snapshot_qty: f64) -> f64 {
    template_qty * shares - snapshot_qty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager_db::ManagerDb;
    use crate::order_config::TargetPosition;
    use crate::position_archive::{self, PositionArchive};
    use crate::strategy_catalog;
    use crate::strategy_catalog::PositionStrategy;
    use crate::twap;
    use crate::viz_snapshot::{FactualPosition, SourceFactualPositions};
    use tempfile::TempDir;

    fn strategy(qty: f64) -> PositionStrategy {
        PositionStrategy {
            strategy_name: "cta_a".into(),
            targets: BTreeMap::from([("BTCUSDT".into(), TargetPosition { qty, signal: 0 })]),
            updated_at_us: 1,
        }
    }

    #[test]
    fn intended_qty_uses_direct_archived_shares() {
        assert!((intended_qty(0.1, 2.0, 0.15) - 0.05).abs() < 1e-12);
    }

    fn five_second_bars_for_minutes(
        start: i64,
        minutes: i64,
        mid_for_minute: impl Fn(i64) -> f64,
    ) -> Vec<TwapBar> {
        let mut bars = Vec::new();
        for minute in 0..minutes {
            for sample in 1..=12 {
                let end_ts_us = start + minute * MINUTE_US + sample * 5_000_000;
                bars.push(TwapBar {
                    end_ts_us,
                    twap: mid_for_minute(minute),
                    sample_count: 1,
                    first_ts_us: end_ts_us - 5_000_000,
                });
            }
        }
        bars
    }

    fn prefetched_btc_bars(twap: &TwapStore, start: i64, end: i64) -> TwapBarsByMarket {
        BTreeMap::from([(
            ("BTCUSDT".to_string(), "binance-futures".to_string()),
            twap.scan_bars("BTCUSDT", "binance-futures", start, end)
                .unwrap(),
        )])
    }

    #[test]
    fn five_minute_window_uses_five_equal_one_minute_mids() {
        let start = 1_000_000;
        let end = start + 5 * MINUTE_US;
        let bars = five_second_bars_for_minutes(start, 5, |minute| 100.0 + minute as f64);
        let (buckets, missing) = minute_twap_buckets(&bars, start, end);
        assert_eq!(missing, 0);
        assert_eq!(buckets.len(), 5);
        for (minute, bucket) in buckets.iter().enumerate() {
            assert_eq!(bucket.duration_us, MINUTE_US);
            assert!((bucket.mid - (100.0 + minute as f64)).abs() < 1e-12);
        }
        let twap = duration_weighted_mid(&buckets).unwrap();
        assert!((twap - 102.0).abs() < 1e-12);
    }

    #[test]
    fn twap_cost_is_intended_qty_times_twap_minus_arrival() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let archive = PositionArchive::open(db.clone()).unwrap();
        let twap = TwapStore::from_db(db, 1).unwrap();
        let cf = twap::column_family_name("BTCUSDT", "binance-futures").unwrap();
        for bar in five_second_bars_for_minutes(1_000_000, 5, |minute| 100.0 + minute as f64) {
            twap.append_bar(&cf, bar).unwrap();
        }
        let msg = archive
            .append(
                1_000_000,
                &strategy(1.0),
                vec![SourceFactualPositions {
                    source_id: "binance_exec_trade01".into(),
                    snapshot_ts_ms: 1,
                    position_ready: true,
                    positions: BTreeMap::from([(
                        "BTCUSDT".into(),
                        FactualPosition {
                            symbol: "BTCUSDT".into(),
                            qty: 0.0,
                            usdt: None,
                        },
                    )]),
                }],
                vec![position_archive::published_account(
                    "binance_exec_trade01",
                    "cta_a",
                    1.0,
                )],
            )
            .unwrap();
        let config = AppConfig {
            database: crate::config::DatabaseConfig {
                url_env: "CRYPTO_CTA_LOCAL_DATABASE_URL".into(),
                max_connections: 1,
            },
            ingestion: crate::config::IngestionConfig::default(),
            order_config: crate::config::OrderConfigSettings::default(),
            redis: crate::config::RedisSettings::default(),
            twap: crate::config::TwapConfig {
                enabled: false,
                rocksdb_path: dir.path().to_path_buf(),
                venue: "binance-futures".into(),
                interval_ms: 5_000,
                retain_days: 1,
                catalog_reload_secs: 30,
                compact_interval_secs: 3600,
            },
            sources: vec![],
        };
        let fills = BTreeMap::new();
        let twap_bars = prefetched_btc_bars(&twap, 1_000_000, i64::MAX);
        let update = cost_for_update(
            &config,
            &twap_bars,
            &msg,
            1_000_000 + 5 * MINUTE_US,
            &fills,
            &BTreeSet::new(),
            true,
        )
        .unwrap();
        let row = &update.accounts[0].symbols[0];
        assert!((row.intended_qty - 1.0).abs() < 1e-12);
        assert_eq!(row.minute_bar_count, 5);
        assert!((row.arrival_mid.unwrap() - 100.0).abs() < 1e-12);
        assert!((row.twap_mid.unwrap() - 102.0).abs() < 1e-12);
        assert!((row.twap_cost_before_fee_usdt.unwrap() - 2.0).abs() < 1e-9);
        assert!(row.actual_cost_before_fee_usdt.is_none());
    }

    #[test]
    fn actual_cost_uses_signed_fill_vwap_against_arrival() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let archive = PositionArchive::open(db.clone()).unwrap();
        let twap = TwapStore::from_db(db, 1).unwrap();
        let cf = twap::column_family_name("BTCUSDT", "binance-futures").unwrap();
        for bar in five_second_bars_for_minutes(1_000_000, 5, |_| 100.0) {
            twap.append_bar(&cf, bar).unwrap();
        }
        let msg = archive
            .append(
                1_000_000,
                &strategy(1.0),
                vec![SourceFactualPositions {
                    source_id: "binance_exec_trade01".into(),
                    snapshot_ts_ms: 1,
                    position_ready: true,
                    positions: BTreeMap::from([(
                        "BTCUSDT".into(),
                        FactualPosition {
                            symbol: "BTCUSDT".into(),
                            qty: 0.0,
                            usdt: None,
                        },
                    )]),
                }],
                vec![position_archive::published_account(
                    "binance_exec_trade01",
                    "cta_a",
                    1.0,
                )],
            )
            .unwrap();
        let config = AppConfig {
            database: crate::config::DatabaseConfig {
                url_env: "CRYPTO_CTA_LOCAL_DATABASE_URL".into(),
                max_connections: 1,
            },
            ingestion: crate::config::IngestionConfig::default(),
            order_config: crate::config::OrderConfigSettings::default(),
            redis: crate::config::RedisSettings::default(),
            twap: crate::config::TwapConfig {
                enabled: false,
                rocksdb_path: dir.path().to_path_buf(),
                venue: "binance-futures".into(),
                interval_ms: 5_000,
                retain_days: 1,
                catalog_reload_secs: 30,
                compact_interval_secs: 3600,
            },
            sources: vec![],
        };
        let mut fills = BTreeMap::new();
        fills.insert(
            (
                "binance_exec_trade01".into(),
                "cta_a".into(),
                "BTCUSDT".into(),
            ),
            vec![
                (
                    2_000_000,
                    SignedFill {
                        qty: 0.4,
                        price: 101.0,
                        estimated_fee_usdt: 0.01,
                    },
                ),
                (
                    3_000_000,
                    SignedFill {
                        qty: 0.6,
                        price: 104.0,
                        estimated_fee_usdt: 0.02,
                    },
                ),
            ],
        );
        let twap_bars = prefetched_btc_bars(&twap, 1_000_000, i64::MAX);
        let update = cost_for_update(
            &config,
            &twap_bars,
            &msg,
            1_000_000 + 5 * MINUTE_US,
            &fills,
            &BTreeSet::new(),
            true,
        )
        .unwrap();
        let row = &update.accounts[0].symbols[0];
        assert!((row.filled_qty - 1.0).abs() < 1e-12);
        assert!((row.actual_vwap.unwrap() - 102.8).abs() < 1e-12);
        let arrival = row.arrival_mid.unwrap();
        assert!((row.actual_cost_before_fee_usdt.unwrap() - (102.8 - arrival)).abs() < 1e-9);
        assert!((row.estimated_trading_fee_usdt - 0.03).abs() < 1e-12);
        assert!((row.actual_cost_after_fee_usdt.unwrap() - (102.8 - arrival + 0.03)).abs() < 1e-9);
        assert!((row.twap_cost_before_fee_usdt.unwrap()).abs() < 1e-12);
    }

    #[test]
    fn scale_targets_is_used_for_published_qty() {
        let scaled = strategy_catalog::scale_targets(
            &BTreeMap::from([(
                "BTCUSDT".into(),
                TargetPosition {
                    qty: 0.2,
                    signal: 1,
                },
            )]),
            2.0,
        );
        assert!((scaled["BTCUSDT"].qty - 0.4).abs() < 1e-12);
    }

    #[test]
    fn actual_cost_uses_fill_vwap_versus_arrival_mid() {
        let arrival = 100.0_f64;
        let filled_qty = 2.0_f64;
        let vwap = 101.5_f64;
        let actual = filled_qty * (vwap - arrival);
        assert!((actual - 3.0).abs() < 1e-12);
        let intended = 2.0_f64;
        let twap_mid = 100.5_f64;
        let twap_cost = intended * (twap_mid - arrival);
        assert!((twap_cost - 1.0).abs() < 1e-12);
    }

    #[test]
    fn aggregate_after_fee_cost_includes_fees_without_arrival_mid() {
        let mut totals = CostTotals {
            actual_cost_before_fee_usdt: 1.0,
            estimated_trading_fee_usdt: 0.1,
            actual_cost_after_fee_usdt: 1.1,
            ..CostTotals::default()
        };
        totals.add(CostTotals {
            estimated_trading_fee_usdt: 0.2,
            ..CostTotals::default()
        });

        assert!((totals.actual_cost_after_fee_usdt - 1.3).abs() < 1e-12);
    }

    #[test]
    fn execution_window_stops_at_next_same_strategy() {
        let msg = PositionUpdateMsg {
            msg_type: "position_update".into(),
            schema_version: 3,
            received_at_us: 1_000_000,
            seq: 0,
            strategy: strategy(1.0),
            factual_positions: Vec::new(),
            published_accounts: Vec::new(),
        };
        let end = execution_window_end(&msg, Some(10_000_000), 300_000_000, 1_000_000_000);
        assert_eq!(end, 10_000_000);
    }

    #[test]
    fn report_paginates_latest_updates_without_changing_range_totals() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let archive = PositionArchive::open(db.clone()).unwrap();
        let twap = TwapStore::from_db(db, 1).unwrap();
        for received_at_us in [1_000_000, 2_000_000, 3_000_000] {
            archive
                .append(
                    received_at_us,
                    &strategy(1.0),
                    Vec::new(),
                    vec![position_archive::published_account(
                        "binance_exec_trade01",
                        "cta_a",
                        1.0,
                    )],
                )
                .unwrap();
        }
        let config = AppConfig {
            database: crate::config::DatabaseConfig {
                url_env: "CRYPTO_CTA_LOCAL_DATABASE_URL".into(),
                max_connections: 1,
            },
            ingestion: crate::config::IngestionConfig::default(),
            order_config: crate::config::OrderConfigSettings::default(),
            redis: crate::config::RedisSettings::default(),
            twap: crate::config::TwapConfig::default(),
            sources: Vec::new(),
        };
        let histories = nav::NavSourceHistories::new();
        let page_one = report_execution_cost(
            &config,
            &archive,
            &twap,
            1,
            None,
            300,
            10_000_000,
            &[],
            None,
            1,
            2,
            &histories,
        )
        .unwrap();
        let page_two = report_execution_cost(
            &config,
            &archive,
            &twap,
            1,
            None,
            300,
            10_000_000,
            &[],
            None,
            2,
            2,
            &histories,
        )
        .unwrap();

        assert_eq!(page_one.update_count, 3);
        assert_eq!(page_one.page_count, 2);
        assert_eq!(page_one.returned_update_count, 2);
        assert_eq!(
            page_one
                .updates
                .iter()
                .map(|update| update.received_at_us)
                .collect::<Vec<_>>(),
            vec![2_000_000, 3_000_000]
        );
        assert_eq!(page_two.returned_update_count, 1);
        assert_eq!(page_two.updates[0].received_at_us, 1_000_000);
        assert_eq!(page_one.totals.intended_qty, page_two.totals.intended_qty);
        assert_eq!(page_one.totals.intended_qty, 3.0);
        assert_eq!(page_one.points.len(), 3);
        assert_eq!(page_one.points.last().unwrap().ts_us, 3_000_000);
        assert_eq!(
            page_one.points.last().unwrap().actual_cost_after_fee_usdt,
            page_one.totals.actual_cost_after_fee_usdt
        );
    }

    #[test]
    fn cost_point_downsampling_preserves_range_endpoints() {
        let points = (0..3_000)
            .map(|index| ExecutionCostPoint {
                ts_us: index,
                twap_cost_before_fee_usdt: index as f64,
                actual_cost_before_fee_usdt: index as f64,
                estimated_trading_fee_usdt: index as f64,
                actual_cost_after_fee_usdt: index as f64,
            })
            .collect();
        let sampled = downsample_cost_points(points, 2_000);

        assert_eq!(sampled.len(), 2_000);
        assert_eq!(sampled.first().unwrap().ts_us, 0);
        assert_eq!(sampled.last().unwrap().ts_us, 2_999);
    }
}
