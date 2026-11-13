//! Manager-owned daily FIFO restart points. Exec stores are always read-only.
use super::*;
use crate::manager_db::ManagerDb;
use rocksdb::{Direction, IteratorMode, WriteBatch};
use sha2::{Digest, Sha256};
use std::sync::{Mutex, OnceLock};

pub const CF: &str = "nav_fifo_daily";
pub const MEMORY_WINDOW_US: i64 = 7 * crate::funding_daily::DAY_US;
const DAY_US: i64 = crate::funding_daily::DAY_US;
use crate::funding_daily::day_start;

// Historical replays and cold source builds share one memory budget. The guard
// stays in the blocking worker, including when its HTTP caller disconnects.
static REPLAY: OnceLock<Mutex<()>> = OnceLock::new();
pub(crate) struct ReplayGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for ReplayGuard {
    fn drop(&mut self) {
        // Long replays free many small allocations across glibc arenas. Return
        // those already-free pages before another replay takes the memory budget.
        // Live recent history and open FIFO lots remain allocated and untouched.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            unsafe extern "C" {
                fn malloc_trim(pad: usize) -> std::ffi::c_int;
            }
            malloc_trim(0);
        }
    }
}

pub(crate) fn replay_lock() -> ReplayGuard {
    ReplayGuard {
        _lock: REPLAY
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    }
}

#[derive(Clone)]
pub struct DailyFifoStore(ManagerDb);
impl std::fmt::Debug for DailyFifoStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DailyFifoStore")
            .field(&self.0.path())
            .finish()
    }
}
#[derive(Serialize, Deserialize)]
struct DailyEntry {
    source_id: String,
    boundary_us: i64,
    input_fingerprint: String,
    uniform_scan_from: Option<Vec<u8>>,
    account: TimelineSourceState,
    strategy: TimelineSourceState,
}

impl DailyFifoStore {
    pub fn open(db: ManagerDb) -> Result<Self> {
        db.ensure_column_family(CF)?;
        Ok(Self(db))
    }
    fn prefix(source: &str) -> Result<Vec<u8>> {
        anyhow::ensure!(
            !source.is_empty() && !source.contains('\0'),
            "invalid FIFO source id"
        );
        let mut key = source.as_bytes().to_vec();
        key.push(0);
        Ok(key)
    }
    fn key(source: &str, boundary: i64) -> Result<Vec<u8>> {
        anyhow::ensure!(
            boundary > 0 && day_start(boundary) == boundary,
            "invalid FIFO day boundary"
        );
        let mut key = Self::prefix(source)?;
        key.extend_from_slice(&boundary.to_be_bytes());
        Ok(key)
    }
    fn fingerprint(cache: &NavTimelineCache, id: &str) -> Result<String> {
        let source = cache
            .config
            .sources
            .iter()
            .find(|s| s.id == id)
            .context("missing FIFO source")?;
        let mut hash = Sha256::new();
        hash.update(serde_json::to_vec(&cache.inputs[&(id.to_owned(), false)])?);
        hash.update(serde_json::to_vec(&cache.inputs[&(id.to_owned(), true)])?);
        hash.update(source.rocksdb_path.to_string_lossy().as_bytes());
        hash.update(cache.histories[id].checkpoint_epoch.to_be_bytes());
        hash.update(
            cache.histories[id]
                .source_identity
                .as_deref()
                .unwrap_or_default(),
        );
        Ok(hex::encode(hash.finalize()))
    }
    fn load(&self, cache: &NavTimelineCache, id: &str, before: i64) -> Result<Option<DailyEntry>> {
        let day = day_start(before);
        if day <= 0 {
            return Ok(None);
        }
        let prefix = Self::prefix(id)?;
        let key = Self::key(id, day)?;
        let cf = self.0.db().cf_handle(CF).context("daily FIFO CF missing")?;
        let fingerprint = Self::fingerprint(cache, id)?;
        for item in self
            .0
            .db()
            .iterator_cf(&cf, IteratorMode::From(&key, Direction::Reverse))
        {
            let (key, bytes) = item?;
            if !key.starts_with(&prefix) {
                break;
            }
            let entry: DailyEntry =
                serde_json::from_slice(&bytes).context("invalid daily FIFO state")?;
            anyhow::ensure!(
                entry.source_id == id && Self::key(id, entry.boundary_us)? == key.as_ref(),
                "daily FIFO identity mismatch"
            );
            if entry.input_fingerprint == fingerprint {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }
    fn invalidate(&self, id: &str, from: i64) -> Result<()> {
        let prefix = Self::prefix(id)?;
        let cf = self.0.db().cf_handle(CF).context("daily FIFO CF missing")?;
        let mut batch = WriteBatch::default();
        for item in self
            .0
            .db()
            .iterator_cf(&cf, IteratorMode::From(&prefix, Direction::Forward))
        {
            let (key, _) = item?;
            if !key.starts_with(&prefix) {
                break;
            }
            let ts = key
                .get(prefix.len()..)
                .and_then(|v| <[u8; 8]>::try_from(v).ok())
                .map(i64::from_be_bytes)
                .context("invalid FIFO key")?;
            // State is strictly before the boundary: a fill exactly at `from`
            // changes the next day, not that day's opening state.
            if ts > from {
                batch.delete_cf(&cf, key);
            }
        }
        self.0.db().write(batch)?;
        Ok(())
    }
    fn persist(&self, cache: &NavTimelineCache, id: &str, through: i64) -> Result<()> {
        let account = &cache.sources[&(id.to_owned(), false)];
        let strategy = &cache.sources[&(id.to_owned(), true)];
        let first = [account, strategy]
            .into_iter()
            .filter_map(|s| s.prepared.snapshot_ts_us.or(s.report.first_fill_ts_us))
            .min();
        let Some(first) = first else {
            return Ok(());
        };
        let fingerprint = Self::fingerprint(cache, id)?;
        let mut day = self
            .load(cache, id, through)?
            .map_or(day_start(first) + DAY_US, |e| e.boundary_us + DAY_US);
        let cf = self.0.db().cf_handle(CF).context("daily FIFO CF missing")?;
        while day <= through {
            // No approximate or partial state is ever published.
            let entry = DailyEntry {
                source_id: id.to_owned(),
                boundary_us: day,
                input_fingerprint: fingerprint.clone(),
                uniform_scan_from: cache.histories[id]
                    .events
                    .iter()
                    .filter(|e| fifo_ts_us(e) >= day)
                    .map(|e| e.record_key.as_bytes().to_vec())
                    .min()
                    .or_else(|| {
                        cache.histories[id]
                            .read_limits
                            .get(crate::model::UNIFORM_ORDERS_CF)
                            .cloned()
                            .flatten()
                    }),
                account: account.state_before(day)?,
                strategy: strategy.state_before(day)?,
            };
            let mut options = rocksdb::WriteOptions::default();
            options.set_sync(true);
            self.0.db().put_cf_opt(
                &cf,
                Self::key(id, day)?,
                serde_json::to_vec(&entry)?,
                &options,
            )?;
            day += DAY_US;
        }
        Ok(())
    }
}

impl CachedTimelineSource {
    fn state_before(&self, ts: i64) -> Result<TimelineSourceState> {
        anyhow::ensure!(
            self.retained_from_us.is_none_or(|bound| ts >= bound),
            "FIFO state predates retained history"
        );
        let cp = self.checkpoints.iter().rev().find(|cp| cp.ts_us <= ts);
        let mut state = cp
            .map(|cp| cp.state.clone())
            .unwrap_or_else(|| TimelineSourceState {
                allocations: self.prepared.allocations.clone(),
                next_allocation: 0,
                source_id: self.end_state.source_id.clone(),
                fee_rates: self.end_state.fee_rates.clone(),
                snapshot_ts_us: self.prepared.snapshot_ts_us,
                pending_initial_states: self
                    .prepared
                    .snapshot_ts_us
                    .map(|_| self.prepared.initial_states.clone()),
                pending_initial_strategy_states: self
                    .prepared
                    .snapshot_ts_us
                    .map(|_| self.prepared.initial_strategy_states.clone()),
                states: im::OrdMap::new(),
                strategy_states: im::OrdMap::new(),
                latest_marks: im::OrdMap::new(),
                strategy_allocation_active: self.prepared.strategy_allocation_active,
            });
        for fill in self
            .prepared
            .fill_events
            .iter()
            .skip(cp.map_or(0, |c| c.next_fill))
            .take_while(|f| f.fill_ts_us < ts)
        {
            state.apply_fill(fill)?;
        }
        state.activate_snapshot_at(ts);
        state.advance_allocations(ts, false)?;
        Ok(state)
    }
    fn retain_since(&mut self, boundary: i64) -> Result<()> {
        let state = self.state_before(boundary)?;
        let skip = fill_partition_point(&self.prepared.fill_events, |f| f.fill_ts_us < boundary);
        self.prepared.fill_events = self.prepared.fill_events.skip(skip);
        self.checkpoints = std::iter::once(Arc::new(FifoCheckpoint {
            ts_us: boundary,
            next_fill: 0,
            state,
        }))
        .chain(
            self.checkpoints
                .iter()
                .filter(|cp| cp.ts_us > boundary)
                .map(|cp| {
                    Arc::new(FifoCheckpoint {
                        ts_us: cp.ts_us,
                        next_fill: cp.next_fill - skip,
                        state: cp.state.clone(),
                    })
                }),
        )
        .collect();
        self.retained_from_us = Some(boundary);
        Ok(())
    }
}

// JSON's object keys cannot represent symbol/venue tuples. Store pairs without
// changing the in-memory persistent maps used for cheap FIFO checkpoint clones.
pub(super) mod ordered_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<K: Serialize + Ord + Clone, V: Serialize + Clone, S: Serializer>(
        value: &im::OrdMap<K, V>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.iter().collect::<Vec<_>>().serialize(s)
    }
    pub fn deserialize<
        'de,
        K: Deserialize<'de> + Ord + Clone,
        V: Deserialize<'de> + Clone,
        D: Deserializer<'de>,
    >(
        d: D,
    ) -> Result<im::OrdMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}
pub(super) mod optional_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;
    pub fn serialize<K: Serialize + Ord, V: Serialize, S: Serializer>(
        value: &Option<BTreeMap<K, V>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .map(|v| v.iter().collect::<Vec<_>>())
            .serialize(s)
    }
    pub fn deserialize<
        'de,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    >(
        d: D,
    ) -> Result<Option<BTreeMap<K, V>>, D::Error> {
        Ok(Option::<Vec<(K, V)>>::deserialize(d)?.map(|v| v.into_iter().collect()))
    }
}

pub(super) fn load_history_window(
    source: &SourceConfig,
    from: Option<i64>,
    limits: Option<&BTreeMap<String, Option<Vec<u8>>>>,
) -> Result<NavSourceHistory> {
    load_history_window_from(source, from, limits, None, None)
}

pub(super) fn load_fill_history(
    source: &SourceConfig,
    symbols: &[String],
) -> Result<NavSourceHistory> {
    load_history_window_from(source, None, None, None, Some(symbols))
}

fn load_history_window_from(
    source: &SourceConfig,
    from: Option<i64>,
    limits: Option<&BTreeMap<String, Option<Vec<u8>>>>,
    uniform_start: Option<Vec<u8>>,
    fill_symbols: Option<&[String]>,
) -> Result<NavSourceHistory> {
    if !source.rocksdb_path.join("CURRENT").is_file() {
        return Ok(NavSourceHistory::default());
    }
    // RapidX recovery joins lifecycle and exchange execution evidence. Keep the
    // full join temporary; trimming inputs before recovery can double-count fills.
    let rapidx = crate::market_rules::source_values(source)
        .ok()
        .is_some_and(|v| {
            matches!(
                crate::market_rules::execution_backend(&source.venue, &v),
                Ok("ltp")
            )
        });
    let requests = NAV_HISTORY_COLUMN_FAMILIES
        .iter()
        .map(|cf| {
            (
                cf.to_string(),
                if !rapidx && *cf == crate::model::UNIFORM_ORDERS_CF {
                    uniform_start.clone()
                } else {
                    None
                },
            )
        })
        .collect::<Vec<_>>();
    let mut events = im::Vector::new();
    let mut roles = LiquidityByOrder::new();
    let mut wanted = BTreeSet::new();
    let heads = rocks_source::visit_column_families_from(
        &source.rocksdb_path,
        &requests,
        |cf, key, value| {
            if let Some(limits) = limits {
                if limits
                    .get(cf)
                    .and_then(Option::as_ref)
                    .is_none_or(|last| key > last.as_slice())
                {
                    return Ok(());
                }
            }
            if cf == crate::model::UNIFORM_ORDERS_CF {
                let event: Arc<NavEvent> = decode_uniform_order(key, value)?.into();
                // Recovery requires RapidX lifecycle rows. Native diagnostic
                // replays can discard non-fills before allocating their history.
                if rapidx
                    || ((fill_symbols.is_none_or(|symbols| {
                        event.amount_update != 0.0
                            && (symbols.is_empty()
                                || symbols.contains(&crate::exec_routing::target_symbol(
                                    &event.symbol,
                                    event.venue_code,
                                )))
                    })) && from.is_none_or(|bound| fifo_ts_us(&event) >= bound))
                {
                    wanted.insert((
                        event.symbol.clone(),
                        event.venue_code,
                        event.client_order_id,
                    ));
                    events.push_back(event);
                }
            } else {
                let event = decode_trade_update(key, value)?;
                let k = (
                    crate::exec_routing::target_symbol(&event.symbol, event.venue_code),
                    event.venue_code,
                    event.client_order_id,
                );
                if wanted.contains(&k) {
                    merge_liquidity_event(&mut roles, &mut BTreeSet::new(), event);
                }
            }
            Ok(())
        },
    )?;
    anyhow::ensure!(
        heads.contains_key(crate::model::UNIFORM_ORDERS_CF),
        "missing uniform_orders"
    );
    let events =
        fill_recovery::resolve(source, &events, &mut Default::default())?.unwrap_or(events);
    let events = events
        .into_iter()
        .filter(|e| {
            fill_symbols.is_none_or(|symbols| {
                e.amount_update != 0.0
                    && (symbols.is_empty()
                        || symbols
                            .contains(&crate::exec_routing::target_symbol(&e.symbol, e.venue_code)))
            })
        })
        .filter(|e| from.is_none_or(|bound| fifo_ts_us(e) >= bound))
        .collect();
    Ok(NavSourceHistory {
        events,
        liquidity_by_order: roles,
        read_limits: limits.cloned().unwrap_or(heads),
        source_identity: std::fs::read(source.rocksdb_path.join("IDENTITY")).ok(),
        ..Default::default()
    })
}

impl NavSourceHistory {
    pub(crate) fn symbol_index_events(&self) -> impl Iterator<Item = &NavEvent> {
        self.symbol_index_events
            .iter()
            .chain(self.events.iter())
            .map(AsRef::as_ref)
    }
    fn retain_since(&mut self, boundary: i64) {
        self.funding_starts = self.funding_symbols();
        let mut representatives: BTreeMap<(String, i16), [Option<Arc<NavEvent>>; 3]> =
            BTreeMap::new();
        for e in self.symbol_index_events.iter().chain(self.events.iter()) {
            let slots = representatives
                .entry((e.symbol.clone(), e.venue_code))
                .or_default();
            if slots[0]
                .as_ref()
                .is_none_or(|old| e.event_ts_us < old.event_ts_us)
            {
                slots[0] = Some(e.clone());
            }
            if e.amount_update > 0.0 {
                if slots[1]
                    .as_ref()
                    .is_none_or(|old| fifo_ts_us(e) < fifo_ts_us(old))
                {
                    slots[1] = Some(e.clone());
                }
                if slots[2]
                    .as_ref()
                    .is_none_or(|old| fifo_ts_us(e) > fifo_ts_us(old))
                {
                    slots[2] = Some(e.clone());
                }
            }
        }
        self.symbol_index_events = representatives.into_values().flatten().flatten().collect();
        self.events = self
            .events
            .iter()
            .filter(|e| fifo_ts_us(e) >= boundary || e.event_ts_us >= boundary)
            .cloned()
            .collect();
        let keys = self
            .events
            .iter()
            .map(|e| (e.symbol.clone(), e.venue_code, e.client_order_id))
            .collect::<BTreeSet<_>>();
        self.liquidity_by_order = self
            .liquidity_by_order
            .iter()
            .filter(|(k, _)| keys.contains(*k))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        self.change = None;
        self.retained_from_us = Some(boundary);
    }
}

impl NavTimelineCache {
    /// Build one account at a time, then discard its old event arrays before
    /// reading the next. Only immutable FIFO lots and the recent tail stay in RAM.
    pub fn refresh_daily(
        config: AppConfig,
        snapshots: Arc<SourcePositionSnapshots>,
        strategies: Arc<SourceStrategyPositionSnapshots>,
        allocations: &BTreeMap<String, Arc<Vec<allocation::AllocationSnapshot>>>,
        store: &mut NavHistoryStore,
        previous: Option<&Self>,
        daily: Arc<DailyFifoStore>,
        now: i64,
    ) -> Result<Self> {
        let _budget = replay_lock();
        let boundary = day_start(now - MEMORY_WINDOW_US);
        let mut sources = BTreeMap::new();
        let mut inputs = BTreeMap::new();
        let mut histories = BTreeMap::new();
        let mut funding_fingerprints = BTreeMap::new();
        for source in config.sources.iter().filter(|s| s.enabled) {
            let mut scoped = config.clone();
            scoped.sources.retain(|s| s.id == source.id);
            // Refresh only this source without removing other incremental readers.
            let state = store.sources.entry(source.id.clone()).or_default();
            if !source.rocksdb_path.join("CURRENT").is_file() {
                *state = IncrementalSourceHistory::default();
            } else {
                state.refresh_source(source)?;
            }
            let mut history = state.history.clone();
            history.allocations = allocations.get(&source.id).cloned().unwrap_or_default();
            let old = previous.and_then(|p| p.sources.get(&(source.id.clone(), true)));
            let changed_from = history.change.as_ref().and_then(|c| {
                c.events
                    .iter()
                    .map(|e| fifo_ts_us(e))
                    .chain(c.earliest_liquidity_fill_ts_us)
                    .min()
            });
            let needs_full = old.is_none()
                || !source.rocksdb_path.join("CURRENT").is_file()
                || history.change.is_none()
                    && previous.is_none_or(|p| {
                        p.histories
                            .get(&source.id)
                            .is_none_or(|h| h.revision != history.revision)
                    })
                || changed_from.is_some_and(|ts| {
                    ts < boundary
                        || old.is_some_and(|s| {
                            s.report
                                .last_fill_ts_us
                                .into_iter()
                                .chain(s.prepared.allocations.last().map(|a| a.snapshot_ts_us))
                                .any(|end| ts <= end)
                        })
                })
                || previous.is_some_and(|p| {
                    [false, true].iter().any(|mode| {
                        p.inputs.get(&(source.id.clone(), *mode)).is_none_or(|i| {
                            i.fees != source.nav_fee_rates().unwrap_or_else(|_| i.fees.clone())
                                || i.snapshot != snapshots.get(&source.id).cloned()
                                || i.strategy
                                    != mode
                                        .then(|| strategies.get(&source.id).cloned())
                                        .flatten()
                                        .filter(|s| {
                                            snapshots.get(&source.id).is_none_or(|a| {
                                                s.snapshot_ts_us >= a.snapshot_ts_us
                                            })
                                        })
                                || i.allocations != history.allocations
                        })
                    })
                });
            let needs_full = needs_full
                || crate::market_rules::source_values(source)
                    .ok()
                    .is_some_and(|v| {
                        matches!(
                            crate::market_rules::execution_backend(&source.venue, &v),
                            Ok("ltp")
                        )
                    });
            if needs_full {
                daily.invalidate(&source.id, i64::MIN)?;
                if history.retained_from_us.is_some() {
                    *state = IncrementalSourceHistory::default();
                    state.refresh_source(source)?;
                    history = state.history.clone();
                    history.allocations = allocations.get(&source.id).cloned().unwrap_or_default();
                }
            } else if let Some(from) = changed_from {
                daily.invalidate(&source.id, from)?;
            }
            let mut built = Self::refresh(
                scoped,
                snapshots.clone(),
                strategies.clone(),
                Arc::new(BTreeMap::from([(source.id.clone(), history)])),
                if needs_full { None } else { previous },
            )?;
            // Keep original lifetime first-fill metadata when incremental tails
            // no longer contain the first fill.
            for mode in [false, true] {
                if let Some(old) = previous.and_then(|p| p.sources.get(&(source.id.clone(), mode)))
                {
                    if !needs_full {
                        let new = Arc::make_mut(
                            built.sources.get_mut(&(source.id.clone(), mode)).unwrap(),
                        );
                        new.report.first_fill_ts_us =
                            old.report.first_fill_ts_us.or(new.report.first_fill_ts_us);
                        new.report.last_fill_ts_us =
                            new.report.last_fill_ts_us.or(old.report.last_fill_ts_us);
                    }
                }
            }
            // Daily funding fingerprints are small immutable metadata. Reuse
            // completed days; do not reload every old fill on each dashboard poll.
            let first = built.start_ts_us(&source.id, now, true);
            let cutoffs = first
                .map(|first| {
                    (day_start(first)..day_start(now))
                        .step_by(DAY_US as usize)
                        .map(|day| day + DAY_US - 1)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let previous_hashes = previous
                .and_then(|p| p.funding_fingerprints.get(&source.id))
                .filter(|_| !needs_full);
            let hashes = if let Some(old) =
                previous_hashes.filter(|old| cutoffs.iter().all(|t| old.contains_key(t)))
            {
                old.clone()
            } else {
                let hashes = if built.histories[&source.id].retained_from_us.is_some() {
                    built
                        .restore_window(std::slice::from_ref(&source.id), None)?
                        .funding_daily_fingerprints(&source.id, &cutoffs)?
                } else {
                    built.funding_daily_fingerprints(&source.id, &cutoffs)?
                };
                Arc::new(hashes)
            };
            funding_fingerprints.insert(source.id.clone(), hashes);
            daily.persist(&built, &source.id, now)?;
            for mode in [false, true] {
                Arc::make_mut(built.sources.get_mut(&(source.id.clone(), mode)).unwrap())
                    .retain_since(boundary)?;
            }
            state.history.retain_since(boundary);
            state.legacy_fill_times = state
                .legacy_fill_times
                .iter()
                .filter(|(_, ts)| **ts >= boundary)
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            // Recovery needs the original lifecycle; refresh it temporarily for
            // that source rather than keeping its entire raw history resident.
            state.raw_events = None;
            let mut compact = state.history.clone();
            compact.allocations = allocations.get(&source.id).cloned().unwrap_or_default();
            info!(source_id=%source.id, retained_from_us=boundary, cached_events=compact.events.len(),
                cached_account_fills=built.sources[&(source.id.clone(),false)].prepared.fill_events.len(),
                cached_strategy_fills=built.sources[&(source.id.clone(),true)].prepared.fill_events.len(),
                "retained recent FIFO history; daily boundaries persisted in Manager");
            histories.insert(source.id.clone(), compact);
            sources.extend(built.sources);
            inputs.extend(built.inputs);
        }
        store
            .sources
            .retain(|id, _| config.sources.iter().any(|s| s.enabled && &s.id == id));
        Ok(Self {
            config,
            snapshots,
            strategy_snapshots: strategies,
            histories: Arc::new(histories),
            sources,
            inputs,
            results: Arc::new(crate::analysis::QueryCache::new(4)),
            mark_plans: Arc::new(crate::analysis::QueryCache::new(4)),
            daily: Some(daily),
            funding_fingerprints,
        })
    }
    pub fn histories(&self) -> Arc<NavSourceHistories> {
        self.histories.clone()
    }

    pub(super) fn needs_history(&self, ids: &[String], start: Option<i64>) -> bool {
        self.daily.is_some()
            && self.sources.iter().any(|((id, _), s)| {
                (ids.is_empty() || ids.contains(id))
                    && s.retained_from_us
                        .is_some_and(|bound| start.is_none_or(|ts| ts < bound))
            })
    }
    /// Caller owns the replay budget until the returned temporary cache is dropped.
    pub(super) fn restore_window(&self, ids: &[String], start: Option<i64>) -> Result<Self> {
        let mut config = self.config.clone();
        config
            .sources
            .retain(|s| s.enabled && (ids.is_empty() || ids.contains(&s.id)));
        let mut sources = BTreeMap::new();
        let mut histories = BTreeMap::new();
        for source in &config.sources {
            let entry = match (&self.daily, start) {
                (Some(store), Some(start)) => store.load(self, &source.id, start)?,
                _ => None,
            };
            let from = entry.as_ref().map(|e| e.boundary_us);
            let current = &self.histories[&source.id];
            anyhow::ensure!(
                current.source_identity == std::fs::read(source.rocksdb_path.join("IDENTITY")).ok(),
                "Exec database changed; wait for dashboard refresh"
            );
            let mut history = load_history_window_from(
                source,
                from,
                Some(&current.read_limits),
                entry.as_ref().and_then(|e| e.uniform_scan_from.clone()),
                None,
            )?;
            history.allocations = current.allocations.clone();
            history.events = history
                .events
                .into_iter()
                .filter(|e| e.amount_update > 0.0)
                .collect();
            for mode in [false, true] {
                let old = &self.sources[&(source.id.clone(), mode)];
                let mut prepared = old.prepared.clone();
                prepared.fill_events.clear();
                for event in &history.events {
                    if prepared
                        .snapshot_ts_us
                        .is_some_and(|ts| fifo_ts_us(event) <= ts)
                    {
                        continue;
                    }
                    if let Some(side) = validated_fill_side(source, event)? {
                        let mut fill = PreparedFill {
                            event: event.clone(),
                            side,
                            fill_ts_us: fifo_ts_us(event),
                            liquidity: liquidity_role_for_event(event, &history.liquidity_by_order),
                            initial_position_close: false,
                        };
                        fill.attribute_initial_position(
                            prepared.strategy_allocation_active,
                            &prepared.initial_states,
                        );
                        prepared.fill_events.push_back(fill);
                    }
                }
                prepared.fill_events.sort_by(fill_order);
                let checkpoints = entry
                    .as_ref()
                    .map(|entry| {
                        vec![Arc::new(FifoCheckpoint {
                            ts_us: entry.boundary_us,
                            next_fill: 0,
                            state: if mode {
                                entry.strategy.clone()
                            } else {
                                entry.account.clone()
                            },
                        })]
                    })
                    .unwrap_or_default();
                // The query performs the replay itself. Do not eagerly replay
                // and retain another complete set of historical hourly states.
                let built = CachedTimelineSource {
                    prepared,
                    checkpoints,
                    symbols: old.symbols.clone(),
                    strategies: old.strategies.clone(),
                    report: old.report.clone(),
                    end_state: old.end_state.clone(),
                    next_checkpoint: None,
                    replayed_fills: 0,
                    retained_from_us: from,
                };
                sources.insert((source.id.clone(), mode), Arc::new(built));
            }
            histories.insert(source.id.clone(), history);
        }
        Ok(Self {
            config,
            snapshots: self.snapshots.clone(),
            strategy_snapshots: self.strategy_snapshots.clone(),
            histories: Arc::new(histories),
            sources,
            inputs: self.inputs.clone(),
            results: Arc::new(crate::analysis::QueryCache::new(0)),
            mark_plans: Arc::new(crate::analysis::QueryCache::new(0)),
            daily: None,
            funding_fingerprints: BTreeMap::new(),
        })
    }
}

impl NavSourceHistory {
    pub(crate) fn complete_for_window(&self, source: &SourceConfig, start: i64) -> Result<Self> {
        if self.retained_from_us.is_some_and(|bound| start < bound) {
            let _budget = replay_lock();
            anyhow::ensure!(
                self.source_identity == std::fs::read(source.rocksdb_path.join("IDENTITY")).ok(),
                "Exec database changed; wait for dashboard refresh"
            );
            // Acquisition-cost queries are keyed by receive timestamp, so a
            // FIFO timestamp filter would incorrectly drop delayed fills.
            let mut full = load_history_window(source, None, Some(&self.read_limits))?;
            full.allocations = self.allocations.clone();
            full.events = full
                .events
                .into_iter()
                .filter(|e| e.event_ts_us >= start)
                .collect();
            Ok(full)
        } else {
            Ok(self.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daily_keys_are_source_scoped_ordered_and_shanghai_aligned() {
        let day = day_start(1_790_000_000_000_000);
        let first = DailyFifoStore::key("trade01", day).unwrap();
        let second = DailyFifoStore::key("trade01", day + DAY_US).unwrap();
        assert!(first < second);
        assert!(first.starts_with(&DailyFifoStore::prefix("trade01").unwrap()));
        assert!(!first.starts_with(&DailyFifoStore::prefix("trade0").unwrap()));
        assert!(DailyFifoStore::key("trade01", day + 1).is_err());
        assert!(DailyFifoStore::key("trade\0bad", day).is_err());
        assert!(DailyFifoStore::key("", day).is_err());
    }
}
