//! Read-only views of the domestic Exec logs (`cn_uniform_orders`,
//! `cn_trade_updates`). Like crypto, Manager reads only Exec records: no runtime,
//! counter snapshot, funds or transport state.
use super::{
    config::Source,
    records::{self, ORDERS, Order, TRADES, Trade, Uniform},
    state::ObservedOrder,
};
use anyhow::{Context, Result, ensure};
use rocksdb::{DB, IteratorMode, Options};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    ops::Deref,
    path::Path,
};

mod cache;
pub use cache::Cache;

/// Lifecycle records scanned backwards for the recent-order view.
const RECENT_ORDER_SCAN: usize = 200_000;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub source_id: String,
    pub account: String,
    pub read_at_us: i64,
    /// Key time of the newest record in either log.
    pub last_record_us: Option<i64>,
    /// Records are complete through this time: the read itself for a local
    /// store, the committed export cut for a replica.
    pub through_us: Option<i64>,
    pub orders: Vec<ObservedOrder>,
    pub fills: Vec<Trade>,
    /// Order lifecycle records (order events), not distinct orders.
    pub total_orders: usize,
    pub total_fills: usize,
    pub replication: Option<super::sync::ReplicaStatus>,
    pub replication_stale: bool,
}

/// A secondary can follow a live primary. Only the explicitly separate
/// secondary directory is writable; no SDK or exchange credentials are used.
pub fn read(source: &Source, secondary: &Path, limit: usize, stale_secs: u64) -> Result<Snapshot> {
    let db = open_secondary(source, secondary)?;
    Cache::default().read(source, &db, limit, stale_secs)
}

pub(super) struct ReadView {
    db: DB,
    _replica_lock: Option<File>,
}
impl Deref for ReadView {
    type Target = DB;
    fn deref(&self) -> &DB {
        &self.db
    }
}

/// The store must hold exactly the two domestic logs (a Manager replica also
/// keeps its staging family); anything else is a different store contract.
fn check_families(source: &Source, families: &[String]) -> Result<()> {
    let found: BTreeSet<_> = families
        .iter()
        .map(String::as_str)
        .filter(|name| {
            *name != "default" && (source.sync.is_none() || !super::sync::internal(name))
        })
        .collect();
    ensure!(
        found == BTreeSet::from(records::COLUMN_FAMILIES),
        "execution store must contain exactly {ORDERS} and {TRADES}; found {found:?}"
    );
    Ok(())
}

pub(super) fn open_secondary(source: &Source, secondary: &Path) -> Result<ReadView> {
    let mut options = Options::default();
    options.set_max_open_files(-1);
    let cfs = DB::list_cf(&options, &source.rocksdb_path)
        .context("list domestic execution column families")?;
    check_families(source, &cfs)?;
    let primary = source
        .rocksdb_path
        .canonicalize()
        .context("resolve execution store")?;
    let replica_lock = if source.sync.is_some() {
        Some(super::sync::replica_read_lock(&primary)?)
    } else {
        None
    };
    let secondary = super::config::resolved(secondary)?;
    ensure!(
        !secondary.starts_with(&primary) && !primary.starts_with(&secondary),
        "secondary must be outside execution store"
    );
    std::fs::create_dir_all(&secondary).context("create Manager secondary directory")?;
    let db = DB::open_cf_as_secondary(&options, &primary, &secondary, records::COLUMN_FAMILIES)
        .context("open execution RocksDB secondary")?;
    db.try_catch_up_with_primary()
        .context("catch up execution RocksDB secondary")?;
    if source.sync.is_some() {
        let state = super::sync::replica_status(&db, &source.id)?;
        if !state.initialized || state.applying {
            return Err(super::sync::ReplicaBusy.into());
        }
    }
    Ok(ReadView {
        db,
        _replica_lock: replica_lock,
    })
}

fn replication_stale(
    status: Option<&super::sync::ReplicaStatus>,
    now: i64,
    stale_secs: u64,
) -> bool {
    status.is_some_and(|s| {
        s.last_synced_at_us
            .is_none_or(|t| now.saturating_sub(t) > stale_secs as i64 * 1_000_000)
    })
}

pub(super) fn replica_caught_up(replica: &super::sync::ReplicaStatus) -> bool {
    replica.initialized
        && !replica.applying
        && replica.received_cursor == replica.committed_cursor
        && replica
            .target_cursor
            .is_none_or(|target| target <= replica.committed_cursor)
}

/// Latest lifecycle record per `client_order_id` (latest record wins).
#[derive(Default)]
pub(super) struct OrderFold(BTreeMap<i64, Order>);
impl OrderFold {
    fn add(&mut self, order: Order) {
        self.0.insert(order.client_order_id, order);
    }
    pub(super) fn into_orders(self) -> impl Iterator<Item = Order> {
        self.0.into_values()
    }
}

/// Factual lots per `client_order_id`.
fn filled(trades: &[Trade]) -> Result<BTreeMap<i64, i64>> {
    let mut lots = BTreeMap::<i64, i64>::new();
    for trade in trades {
        let sum = lots.entry(trade.client_order_id).or_default();
        *sum = sum
            .checked_add(trade.lots)
            .context("filled lots overflow")?;
    }
    Ok(lots)
}

/// A complete trading day's records, independent of the recent-row UI limit.
/// Fills join orders through their complete native identity later.
#[derive(Default)]
pub struct CostFacts {
    pub fills: Vec<Trade>,
    pub orders: Vec<ObservedOrder>,
    pub replication: Option<super::sync::ReplicaStatus>,
}

fn replication(source: &Source, db: &DB) -> Result<Option<super::sync::ReplicaStatus>> {
    source
        .sync
        .as_ref()
        .map(|_| super::sync::replica_status(db, &source.id))
        .transpose()
}

fn observed(orders: OrderFold, trades: &[Trade]) -> Result<Vec<ObservedOrder>> {
    let filled = filled(trades)?;
    orders
        .into_orders()
        .map(|order| {
            let lots = filled.get(&order.client_order_id).copied().unwrap_or(0);
            super::state::observe(order, lots)
        })
        .collect()
}

pub fn read_cost_facts(source: &Source, secondary: &Path, day: &str) -> Result<CostFacts> {
    let db = open_secondary(source, secondary)?;
    read_cost_facts_from_db(source, &db, day)
}

pub(super) fn read_cost_facts_from_db(source: &Source, db: &DB, day: &str) -> Result<CostFacts> {
    super::reference::valid_day(day)?;
    let mut fills = vec![];
    records::scan(db, TRADES, 0, |key, value| {
        let trade = records::decode_trade(key, value)?;
        if trade.trading_day == day {
            fills.push(trade);
        }
        Ok(true)
    })?;
    let mut orders = OrderFold::default();
    records::scan(db, ORDERS, 0, |key, value| {
        if let Uniform::Order(order) = records::decode_uniform(key, value)?
            && order.trading_day == day
        {
            orders.add(order);
        }
        Ok(true)
    })?;
    Ok(CostFacts {
        orders: observed(orders, &fills)?,
        fills,
        replication: replication(source, db)?,
    })
}

pub(super) struct CostWindow {
    pub days: BTreeMap<String, CostFacts>,
    pub replication: Option<super::sync::ReplicaStatus>,
}

/// Domestic orders are day orders: a trading day spans at most a Friday night
/// session through Monday's close, so an order's lifecycle starts within this
/// span before its fills.
const ORDER_LOOKBACK_US: i64 = 3 * 86_400 * 1_000_000;

pub(super) fn read_cost_window(
    source: &Source,
    db: &DB,
    start: i64,
    end: i64,
) -> Result<CostWindow> {
    const MAX_SCAN: usize = 2_000_000;
    const MAX_FILLS: usize = 250_000;
    ensure!(start <= end, "invalid native cost window");
    let mut days: BTreeMap<String, Vec<Trade>> = BTreeMap::new();
    let mut linked = BTreeSet::new();
    let (mut scanned, mut retained) = (0, 0);
    let stop = end.saturating_add(records::KEY_SLACK_US);
    records::scan(db, TRADES, start.saturating_sub(1), |key, value| {
        scanned += 1;
        ensure!(scanned <= MAX_SCAN, "native cost fill scan budget exceeded");
        if records::key_us(key)? > stop {
            return Ok(false);
        }
        let trade = records::decode_trade(key, value)?;
        if (start..=end).contains(&trade.received_us) {
            retained += 1;
            ensure!(
                retained <= MAX_FILLS,
                "native cost retained fill budget exceeded"
            );
            linked.insert(trade.client_order_id);
            days.entry(trade.trading_day.clone())
                .or_default()
                .push(trade);
        }
        Ok(true)
    })?;
    let mut orders = OrderFold::default();
    let mut scanned = 0;
    if !linked.is_empty() {
        records::scan(
            db,
            ORDERS,
            start.saturating_sub(ORDER_LOOKBACK_US + 1),
            |key, value| {
                scanned += 1;
                ensure!(
                    scanned <= MAX_SCAN,
                    "native cost order scan budget exceeded"
                );
                if records::key_us(key)? > stop {
                    return Ok(false);
                }
                if let Uniform::Order(order) = records::decode_uniform(key, value)?
                    && linked.contains(&order.client_order_id)
                {
                    orders.add(order);
                }
                Ok(true)
            },
        )?;
    }
    let mut by_day: BTreeMap<String, OrderFold> = BTreeMap::new();
    for order in orders.into_orders() {
        by_day
            .entry(order.trading_day.clone())
            .or_default()
            .add(order);
    }
    let days = days
        .into_iter()
        .map(|(day, fills)| {
            let orders = observed(by_day.remove(&day).unwrap_or_default(), &fills)?;
            Ok((
                day,
                CostFacts {
                    fills,
                    orders,
                    replication: None,
                },
            ))
        })
        .collect::<Result<_>>()?;
    Ok(CostWindow {
        days,
        replication: replication(source, db)?,
    })
}

/// Keep the replica apply barrier only for this query, independently of the
/// reusable secondary's lifetime. Stage reception may continue concurrently.
pub(super) fn with_db<T>(
    pool: &crate::rocks_source::ReaderPool,
    source: &Source,
    scan: impl FnOnce(&DB) -> Result<T>,
) -> Result<T> {
    let _guard = if source.sync.is_some() {
        Some(super::sync::replica_read_lock(&source.rocksdb_path)?)
    } else {
        None
    };
    pool.read(
        &source.rocksdb_path,
        &[ORDERS.into(), TRADES.into()],
        |db| {
            for name in records::COLUMN_FAMILIES {
                ensure!(
                    db.cf_handle(name).is_some(),
                    "execution store is missing {name}"
                );
            }
            if source.sync.is_some() {
                let state = super::sync::replica_status(db, &source.id)?;
                if !state.initialized || state.applying {
                    return Err(super::sync::ReplicaBusy.into());
                }
            }
            scan(db)
        },
    )
}

/// Newest `limit` distinct orders by their latest lifecycle record, scanning backwards.
fn recent_orders(db: &DB, limit: usize) -> Result<Vec<Order>> {
    let mut seen = BTreeSet::new();
    let mut recent = vec![];
    for (scanned, item) in db
        .iterator_cf(
            db.cf_handle(ORDERS).context("native orders CF")?,
            IteratorMode::End,
        )
        .enumerate()
    {
        if recent.len() == limit || scanned == RECENT_ORDER_SCAN {
            break;
        }
        let (key, value) = item?;
        if let Uniform::Order(order) = records::decode_uniform(&key, &value)?
            && seen.insert(order.client_order_id)
        {
            recent.push(order);
        }
    }
    Ok(recent)
}

fn recent_trades(db: &DB, limit: usize) -> Result<Vec<Trade>> {
    db.iterator_cf(
        db.cf_handle(TRADES).context("native trades CF")?,
        IteratorMode::End,
    )
    .take(limit)
    .map(|item| {
        let (key, value) = item?;
        records::decode_trade(&key, &value)
    })
    .collect()
}
