use super::config::Source;
use anyhow::{Context, Result, ensure};
use rocksdb::{DB, Direction, IteratorMode, Options};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fs::File, ops::Deref, path::Path};

const EVENTS: &str = "cn_trading_events";
const ORDERS: &str = "cn_uniform_orders";
const FILLS: &str = "cn_trade_facts";

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub source_id: String,
    pub account: String,
    pub read_at_us: i64,
    pub last_event_us: Option<i64>,
    pub state_observed_us: Option<i64>,
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
        source_id: source.id.clone(),
        account: source.account.clone(),
        read_at_us: super::now_us(),
        last_event_us: None,
        state_observed_us: None,
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
    let mut state_sequence = 0;
    let mut runtime = None;
    let mut latest_start = None;
    let mut transport_sequence = None;
    let mut guard: Option<(i64, &str, String)> = None;
    for item in view.iterator_cf(
        db.cf_handle(EVENTS).context("events handle")?,
        IteratorMode::End,
    ) {
        let (_, bytes) = item.context("scan domestic evidence")?;
        let event = json(&bytes, EVENTS)?;
        let sequence = integer(&event, "sequence")?;
        let received = integer(&event, "received_us")?;
        result.last_event_us.get_or_insert(received);
        if !event["projection_error"].is_null() {
            result.projection_errors += 1;
        }
        let value = &event["value"];
        let kind = value["type"].as_str().context("missing evidence type")?;
        if kind == "runtime_start" && latest_start.is_none() {
            latest_start = Some(sequence);
        }
        if result.transport_ready.is_none() {
            let ready = if kind == "transport_status" {
                value["ready"].as_bool()
            } else {
                value["transport_ready"].as_bool()
            };
            if let Some(ready) = ready {
                result.transport_ready = Some(ready);
                result.transport_observed_us = Some(received);
                transport_sequence = Some(sequence);
            }
        }
        if guard.is_none() {
            let outcome = match kind {
                "trading_blocked" => Some((
                    "blocked",
                    value["error"]
                        .as_str()
                        .unwrap_or("execution blocked")
                        .to_owned(),
                )),
                "reconciling" | "recovery_checkpoint" => Some(("recovering", String::new())),
                "restart_required" => Some((
                    "blocked",
                    value["reason"]
                        .as_str()
                        .unwrap_or("execution restart required")
                        .to_owned(),
                )),
                // Current raw Yida evidence contract: 1=connected, 2=disconnected,
                // 14=invalidated, 16=fatal. Never interpret native times as UTC.
                "yida_event"
                    if source.adapter == "yida" && value["event"]["replay"].as_i64() == Some(0) =>
                {
                    match value["event"]["kind"].as_i64() {
                        Some(1) => Some(("recovering", String::new())),
                        Some(2) => Some(("disconnected", "native connection disconnected".into())),
                        Some(14 | 16) => {
                            Some(("blocked", "native adapter invalidated or failed".into()))
                        }
                        _ => None,
                    }
                }
                // CTP has no replay field. Only connection events are readiness
                // guards: 14=query trade and 16=response error (possibly retried).
                // Execution faults are reported by explicit state/block evidence.
                "ctp_event" if source.adapter == "ctp" => match value["event"]["kind"].as_i64() {
                    Some(1) => Some(("recovering", String::new())),
                    Some(2) => Some(("disconnected", "CTP connection disconnected".into())),
                    _ => None,
                },
                _ => None,
            };
            if let Some((phase, reason)) = outcome {
                guard = Some((sequence, phase, reason));
            }
        }
        if runtime.is_none() && matches!(kind, "runtime_start" | "runtime_stopped") {
            runtime = Some((kind.to_owned(), sequence));
        }
        if result.state_observed_us.is_none() && matches!(kind, "state" | "reconciled") {
            let state = &value["state"];
            ensure!(state.is_object(), "missing execution state");
            result.snapshot_ready = state["ready"].as_bool().context("missing ready state")?;
            result.reported_can_trade = value["can_trade"].as_bool();
            result.reason = state["reason"].as_str().unwrap_or_default().into();
            result.trading_day = state["trading_day"].as_str().map(str::to_owned);
            result.available_local_budget = state["available_local_budget"].as_f64();
            result.positions = state["positions"]
                .as_array()
                .context("missing positions snapshot")?
                .clone();
            result.state_observed_us = Some(received);
            state_sequence = sequence;
        }
    }
    for (cf, is_fill) in [(ORDERS, false), (FILLS, true)] {
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
                result.positions_may_lag |= integer(&record, "evidence_seq")? > state_sequence;
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
    result.snapshot_current_runtime =
        result.state_observed_us.is_some() && latest_start.is_none_or(|seq| state_sequence >= seq);
    if !result.snapshot_current_runtime {
        result.reported_can_trade = None;
    }
    if transport_sequence.is_some_and(|seq| latest_start.is_some_and(|start| seq < start)) {
        result.transport_ready = None;
        result.transport_observed_us = None;
    }
    result.status = if runtime
        .as_ref()
        .is_some_and(|(k, _)| k == "runtime_stopped")
    {
        "stopped"
    } else if result.last_event_us.is_none() {
        "waiting"
    } else if let Some((_, phase, reason)) =
        guard.as_ref().filter(|(seq, _, _)| *seq > state_sequence)
    {
        result.reason = reason.clone();
        *phase
    } else if !result.snapshot_current_runtime {
        "recovering"
    } else if runtime
        .as_ref()
        .is_some_and(|(_, seq)| *seq > state_sequence)
    {
        "recovering"
    } else if result.projection_errors > 0 || !result.snapshot_ready {
        "blocked"
    } else if result.transport_ready == Some(false) {
        "transport_unavailable"
    } else if result
        .last_event_us
        .is_none_or(|ts| result.read_at_us - ts > stale_secs as i64 * 1_000_000)
    {
        "stale"
    } else {
        "observing"
    }
    .into();
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
    let mut result = CostFacts {
        fills: vec![],
        orders: vec![],
        replication: if source.sync.is_some() {
            Some(super::sync::replica_status(&db, &source.id)?)
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
