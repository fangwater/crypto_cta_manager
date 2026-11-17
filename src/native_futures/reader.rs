use super::config::Source;
use anyhow::{Context, Result, ensure};
use rocksdb::{DB, Direction, IteratorMode, Options};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fs::File, ops::Deref, path::Path};

mod cache;
mod changes;
mod projection;
pub use cache::Cache;

const EVENTS: &str = "cn_trading_events";
const ORDERS: &str = "cn_uniform_orders";
const FILLS: &str = "cn_trade_facts";

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    #[serde(skip)]
    max_fill_evidence_sequence: i64,
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
    pub positions_may_lag: bool,
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
        positions_may_lag: false,
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
    let mut identities = BTreeSet::new();
    for position in &result.positions {
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
    result.read_at_us = super::now_us();
    result.positions_may_lag = result.max_fill_evidence_sequence > state_sequence;
    projection.apply(&mut result, stale_secs);
    Ok(result)
}

/// A complete trading day's evidence, independent of the recent-row UI limit.
/// Fill/order correlation is performed later using the complete native key.
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
                ensure!(
                    integer(&record["fill"], "lots")? > 0
                        && record["fill"]["price"]
                            .as_f64()
                            .is_some_and(|p| p.is_finite() && p > 0.0),
                    "invalid factual fill quantity or price"
                );
                ensure!(
                    identity["trade_id"].is_string() && record["fill"]["system_id"].is_string(),
                    "native fill identifiers must remain strings"
                );
                result.fills.push(record);
            } else {
                super::state::observe(record.clone())?;
                result.orders.push(record);
            }
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
