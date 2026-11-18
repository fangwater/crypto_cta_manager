use super::{config::Source, evidence::NativeOrderIdentity};
use anyhow::{Context, Result, ensure};
use rocksdb::{DB, Direction, IteratorMode, Options};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    ops::Deref,
    path::Path,
    sync::Arc,
};

mod cache;
mod changes;
mod funds;
mod projection;
pub use cache::Cache;
pub use funds::{AccountFunds, AccountFundsSource};

const EVENTS: &str = "cn_trading_events";
const ORDERS: &str = "cn_uniform_orders";
const FILLS: &str = "cn_trade_facts";

#[derive(Debug, Clone, Serialize)]
pub struct PositionObservation {
    pub observed_us: i64,
    pub evidence_sequence: i64,
    pub current_runtime: bool,
    pub later_fills: bool,
    #[serde(skip)]
    pub(super) rows: Arc<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    #[serde(skip)]
    max_fill_evidence_sequence: i64,
    #[serde(skip)]
    pub(super) nav_evidence_revision: u64,
    pub source_id: String,
    pub account: String,
    pub read_at_us: i64,
    pub last_event_us: Option<i64>,
    pub state_observed_us: Option<i64>,
    pub state_evidence_sequence: Option<i64>,
    pub nav_anchor_observed_us: Option<i64>,
    pub nav_anchor_evidence_sequence: Option<i64>,
    pub status: String,
    pub snapshot_ready: bool,
    pub snapshot_current_runtime: bool,
    pub reported_can_trade: Option<bool>,
    pub transport_ready: Option<bool>,
    pub transport_observed_us: Option<i64>,
    pub reason: String,
    pub trading_day: Option<String>,
    /// A conservative local execution budget, never account equity.
    pub available_local_budget: Option<f64>,
    pub funds: Option<AccountFunds>,
    pub funds_current_runtime: bool,
    pub funds_stale: bool,
    pub positions_may_lag: bool,
    pub position_observation: Option<PositionObservation>,
    pub positions: Vec<Value>,
    pub orders: Vec<super::state::ObservedOrder>,
    pub fills: Vec<Value>,
    pub total_orders: usize,
    pub total_fills: usize,
    pub projection_errors: usize,
    pub replication: Option<super::sync::ReplicaStatus>,
    pub replication_stale: bool,
}

fn json(bytes: &[u8], cf: &str) -> Result<Value> {
    serde_json::from_slice(bytes).with_context(|| format!("decode domestic {cf} record"))
}
fn integer(value: &Value, key: &str) -> Result<i64> {
    value[key]
        .as_i64()
        .with_context(|| format!("missing or invalid {key}"))
}

/// A secondary can follow a live primary. Only the explicitly separate
/// secondary directory is writable; no SDK or exchange credentials are used.
pub fn read(source: &Source, secondary: &Path, limit: usize, stale_secs: u64) -> Result<Snapshot> {
    ensure!((1..=1000).contains(&limit), "invalid row limit");
    let db = open_secondary(source, secondary)?;
    read_snapshot(source, &db, limit, stale_secs)
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

pub(super) fn open_secondary(source: &Source, secondary: &Path) -> Result<ReadView> {
    let mut options = Options::default();
    options.set_max_open_files(-1);
    let cfs = DB::list_cf(&options, &source.rocksdb_path)
        .context("list domestic execution column families")?;
    for cf in [EVENTS, ORDERS, FILLS] {
        ensure!(
            cfs.iter().any(|c| c == cf),
            "execution store is missing {cf}"
        );
    }
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
    let db = DB::open_cf_as_secondary(&options, &primary, &secondary, &cfs)
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

fn read_snapshot(source: &Source, db: &DB, limit: usize, stale_secs: u64) -> Result<Snapshot> {
    let projection = projection::read(db, &source.adapter, None)?;
    read_snapshot_with_projection(
        source,
        db,
        limit,
        stale_secs,
        &projection,
        None,
        false,
        false,
    )
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

fn read_snapshot_with_projection(
    source: &Source,
    db: &DB,
    limit: usize,
    stale_secs: u64,
    projection: &projection::Projection,
    previous_rows: Option<&Snapshot>,
    reuse_orders: bool,
    reuse_fills: bool,
) -> Result<Snapshot> {
    // RocksDB secondaries do not support snapshots. This handle is caught up
    // exactly once and never advanced while its column families are read.
    let view = db;
    let replication = if source.sync.is_some() {
        Some(super::sync::replica_status(db, &source.id)?)
    } else {
        None
    };
    let replication_stale = replication.as_ref().is_some_and(|s| {
        s.last_synced_at_us
            .is_none_or(|t| super::now_us().saturating_sub(t) > stale_secs as i64 * 1_000_000)
    });
    let mut result = Snapshot {
        max_fill_evidence_sequence: 0,
        nav_evidence_revision: 0,
        source_id: source.id.clone(),
        account: source.account.clone(),
        read_at_us: super::now_us(),
        last_event_us: None,
        state_observed_us: None,
        state_evidence_sequence: None,
        nav_anchor_observed_us: None,
        nav_anchor_evidence_sequence: None,
        status: "waiting".into(),
        snapshot_ready: false,
        snapshot_current_runtime: false,
        reported_can_trade: None,
        transport_ready: None,
        transport_observed_us: None,
        reason: String::new(),
        trading_day: None,
        available_local_budget: None,
        funds: None,
        funds_current_runtime: false,
        funds_stale: false,
        positions_may_lag: false,
        position_observation: None,
        positions: vec![],
        orders: vec![],
        fills: vec![],
        total_orders: 0,
        total_fills: 0,
        projection_errors: 0,
        replication,
        replication_stale,
    };
    let state_sequence = projection.state_sequence();
    projection.apply(&mut result, stale_secs);
    for (cf, is_fill) in [(ORDERS, false), (FILLS, true)] {
        if let Some(previous) = previous_rows {
            if is_fill && reuse_fills {
                result.total_fills = previous.total_fills;
                result.fills = previous.fills.clone();
                result.max_fill_evidence_sequence = previous.max_fill_evidence_sequence;
                continue;
            }
            if !is_fill && reuse_orders {
                result.total_orders = previous.total_orders;
                result.orders = previous.orders.clone();
                continue;
            }
        }
        let mut recent = Vec::new();
        for item in view.iterator_cf(
            db.cf_handle(cf).context("record handle")?,
            IteratorMode::From(&[], Direction::Forward),
        ) {
            let (_, bytes) = item.with_context(|| format!("scan {cf}"))?;
            let record = json(&bytes, cf)?;
            let ts = integer(&record, if is_fill { "received_us" } else { "updated_us" })?;
            if is_fill {
                result.total_fills += 1;
                ensure!(
                    record["fill"]["identity"]["trade_id"].is_string()
                        && record["fill"]["system_id"].is_string(),
                    "native fill identifiers must remain strings"
                );
                ensure!(
                    integer(&record["fill"], "lots")? > 0
                        && record["fill"]["price"]
                            .as_f64()
                            .is_some_and(|p| p.is_finite() && p > 0.0),
                    "invalid factual fill quantity or price"
                );
                result.max_fill_evidence_sequence = result
                    .max_fill_evidence_sequence
                    .max(integer(&record, "evidence_seq")?);
            } else {
                result.total_orders += 1;
                ensure!(
                    record["identity"]["native"]["session"].is_string()
                        && record["identity"]["native"]["order_ref"].is_string(),
                    "missing native order identity"
                );
            }
            recent.push((ts, record));
            // Bound memory even when a source has years of history.
            if recent.len() > limit * 2 {
                recent.sort_by_key(|r| std::cmp::Reverse(r.0));
                recent.truncate(limit);
            }
        }
        recent.sort_by_key(|r| std::cmp::Reverse(r.0));
        recent.truncate(limit);
        if is_fill {
            result.fills = recent.into_iter().map(|r| r.1).collect();
        } else {
            result.orders = recent
                .into_iter()
                .map(|r| super::state::observe(r.1))
                .collect::<Result<_>>()?;
        }
    }
    validate_position_rows(&result.positions)?;
    result.read_at_us = super::now_us();
    result.positions_may_lag = result.max_fill_evidence_sequence > state_sequence;
    projection.apply(&mut result, stale_secs);
    Ok(result)
}

fn validate_position_rows(rows: &[Value]) -> Result<()> {
    let mut identities = BTreeSet::new();
    for position in rows {
        let p = &position["position"];
        for field in ["today", "yesterday", "frozen_today", "frozen_yesterday"] {
            ensure!(integer(p, field)? >= 0, "negative native position");
        }
        ensure!(
            position["venue"]
                .as_str()
                .is_some_and(|venue| super::VENUES.contains(&venue))
                && matches!(position["side"].as_str(), Some("buy" | "sell")),
            "unsupported native position scope"
        );
        ensure!(
            identities.insert((
                position["venue"].to_string(),
                position["symbol"].to_string(),
                position["side"].to_string()
            )),
            "duplicate position identity"
        );
    }
    Ok(())
}

/// A complete trading day's evidence, independent of the recent-row UI limit.
/// Fill/order correlation is performed later using the complete native key.
#[derive(Default)]
pub struct CostFacts {
    pub fills: Vec<Value>,
    pub orders: Vec<Value>,
    pub replication: Option<super::sync::ReplicaStatus>,
}

pub fn read_cost_facts(source: &Source, secondary: &Path, day: &str) -> Result<CostFacts> {
    super::reference::valid_day(day)?;
    let db = open_secondary(source, secondary)?;
    read_cost_facts_from_db(source, &db, day)
}

pub(super) fn read_cost_facts_from_db(source: &Source, db: &DB, day: &str) -> Result<CostFacts> {
    super::reference::valid_day(day)?;
    let mut result = CostFacts {
        fills: vec![],
        orders: vec![],
        replication: if source.sync.is_some() {
            Some(super::sync::replica_status(db, &source.id)?)
        } else {
            None
        },
    };
    for (cf, is_fill) in [(ORDERS, false), (FILLS, true)] {
        for item in db.iterator_cf(
            db.cf_handle(cf).context("record handle")?,
            IteratorMode::Start,
        ) {
            let (_, bytes) = item?;
            let record = json(&bytes, cf)?;
            let identity = if is_fill {
                &record["fill"]["identity"]
            } else {
                &record["identity"]
            };
            let trading_day = identity["trading_day"]
                .as_str()
                .context("missing native trading day")?;
            if trading_day != day {
                continue;
            }
            if is_fill {
                validate_cost_fill(&record)?;
                result.fills.push(record);
            } else {
                super::state::observe(record.clone())?;
                result.orders.push(record);
            }
        }
    }
    Ok(result)
}

fn validate_cost_fill(record: &Value) -> Result<()> {
    let fill = &record["fill"];
    ensure!(
        integer(fill, "lots")? > 0
            && fill["price"]
                .as_f64()
                .is_some_and(|p| p.is_finite() && p > 0.0),
        "invalid factual fill quantity or price"
    );
    ensure!(
        fill["identity"]["trade_id"].is_string() && fill["system_id"].is_string(),
        "native fill identifiers must remain strings"
    );
    Ok(())
}

pub(super) struct CostWindow {
    pub days: BTreeMap<String, CostFacts>,
    pub replication: Option<super::sync::ReplicaStatus>,
}

#[derive(Deserialize)]
struct FillReceipt {
    received_us: i64,
}

pub(super) fn read_cost_window(
    source: &Source,
    db: &DB,
    start: i64,
    end: i64,
) -> Result<CostWindow> {
    const MAX_SCAN: usize = 2_000_000;
    const MAX_FILLS: usize = 250_000;
    const ORDER_LOOKUP_CHUNK_SIZE: usize = 4_096;
    let mut result = CostWindow {
        days: BTreeMap::new(),
        replication: source
            .sync
            .as_ref()
            .map(|_| super::sync::replica_status(db, &source.id))
            .transpose()?,
    };
    let mut linked_orders = BTreeSet::new();
    let mut retained = 0;
    for (index, item) in db
        .iterator_cf(
            db.cf_handle(FILLS).context("native fills CF")?,
            IteratorMode::Start,
        )
        .enumerate()
    {
        ensure!(index < MAX_SCAN, "native cost fill scan budget exceeded");
        let (_, bytes) = item?;
        let receipt: FillReceipt =
            serde_json::from_slice(&bytes).context("decode native fill receipt time")?;
        if !(start..=end).contains(&receipt.received_us) {
            continue;
        }
        let record = json(&bytes, FILLS)?;
        validate_cost_fill(&record)?;
        retained += 1;
        ensure!(
            retained <= MAX_FILLS,
            "native cost retained fill budget exceeded"
        );
        let fill = &record["fill"];
        let day = fill["identity"]["trading_day"]
            .as_str()
            .context("missing native fill trading day")?;
        super::reference::valid_day(day)?;
        if let Some(order) = NativeOrderIdentity::for_fill(fill)? {
            linked_orders.insert(order);
        }
        result
            .days
            .entry(day.to_owned())
            .or_default()
            .fills
            .push(record);
    }
    let orders = db.cf_handle(ORDERS).context("native orders CF")?;
    let linked_orders: Vec<_> = linked_orders.into_iter().collect();
    for chunk in linked_orders.chunks(ORDER_LOOKUP_CHUNK_SIZE) {
        let keys = chunk
            .iter()
            .map(serde_json::to_vec)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let found = db.multi_get_cf(keys.iter().map(|key| (&orders, key)));
        for (expected, bytes) in chunk.iter().zip(found) {
            let Some(bytes) = bytes? else {
                continue;
            };
            let record = json(&bytes, ORDERS)?;
            let identity: NativeOrderIdentity = serde_json::from_value(record["identity"].clone())?;
            ensure!(
                &identity == expected,
                "native order value differs from its key"
            );
            super::state::observe(record.clone())?;
            result
                .days
                .get_mut(expected.trading_day())
                .context("missing cost trading day")?
                .orders
                .push(record);
        }
    }
    Ok(result)
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
        &[EVENTS.into(), ORDERS.into(), FILLS.into()],
        |db| {
            for name in [EVENTS, ORDERS, FILLS] {
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
