//! Disposable disk ordering and bounded fill batches for every Exec source.
//! The Exec DB is opened read-only; only Manager-owned scratch and daily states
//! are written. Zero-fill lifecycle rows never enter the ordering or replay.
use super::*;
use rocksdb::{BlockBasedOptions, Cache, DB, Options, WriteOptions};

const BATCH_EVENTS: usize = 4096;
const BATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub(in crate::nav) struct Stats {
    pub fills: usize,
    pub zero_fills: usize,
    pub max_buffered_events: usize,
    pub max_buffered_bytes: usize,
}

struct OrderedFills {
    db: DB,
    // Drop the DB before removing the private scratch directory.
    _directory: tempfile::TempDir,
    batch: WriteBatch,
    batch_bytes: usize,
}

fn signed_key(value: i64) -> [u8; 8] {
    ((value as u64) ^ (1 << 63)).to_be_bytes()
}

fn role_key(tag: u8, symbol: &str, venue: i16, client: i64) -> Result<Vec<u8>> {
    let mut key = vec![tag];
    key.extend(serde_json::to_vec(&(symbol, venue, client))?);
    Ok(key)
}

impl OrderedFills {
    fn open(daily: &DailyFifoStore, source: &SourceConfig) -> Result<Self> {
        let root = daily
            .0
            .path()
            .parent()
            .context("Manager DB has no parent")?
            .join("run/nav-replay");
        anyhow::ensure!(
            !root.starts_with(&source.rocksdb_path),
            "replay scratch cannot be inside Exec RocksDB"
        );
        std::fs::create_dir_all(&root)?;
        // Killed processes cannot run TempDir::drop. Delete only recognized
        // scratch whose owner process no longer exists, never another reader.
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_dir()
                && let Some(pid) = name
                    .strip_prefix("fills-")
                    .and_then(|tail| tail.split('-').next())
                    .and_then(|pid| pid.parse::<u32>().ok())
                && !std::path::Path::new("/proc").join(pid.to_string()).exists()
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        let directory = tempfile::Builder::new()
            .prefix(&format!("fills-{}-", std::process::id()))
            .tempdir_in(root)?;
        let mut options = Options::default();
        options.create_if_missing(true);
        options.set_max_open_files(64);
        options.set_write_buffer_size(8 * 1024 * 1024);
        options.set_max_write_buffer_number(2);
        options.set_compression_type(rocksdb::DBCompressionType::Lz4);
        let mut table = BlockBasedOptions::default();
        table.set_block_cache(&Cache::new_lru_cache(8 * 1024 * 1024));
        options.set_block_based_table_factory(&table);
        let db = DB::open(&options, directory.path())?;
        Ok(Self {
            db,
            _directory: directory,
            batch: WriteBatch::default(),
            batch_bytes: 0,
        })
    }

    fn writes() -> WriteOptions {
        let mut options = WriteOptions::default();
        options.disable_wal(true); // Scratch is disposable and never a durable source.
        options
    }

    fn flush(&mut self) -> Result<()> {
        if !self.batch.is_empty() {
            self.db
                .write_opt(std::mem::take(&mut self.batch), &Self::writes())?;
            self.batch_bytes = 0;
        }
        Ok(())
    }

    fn insert(&mut self, event: &NavEvent) -> Result<()> {
        let mut key = vec![0];
        key.extend(signed_key(fifo_ts_us(event)));
        key.extend(signed_key(event.event_ts_us));
        key.extend(event.record_key.as_bytes());
        let value = serde_json::to_vec(event)?;
        anyhow::ensure!(
            value.len() <= BATCH_BYTES,
            "one NAV fill exceeds the replay buffer"
        );
        if self.batch_bytes + value.len() + key.len() > BATCH_BYTES {
            self.flush()?;
        }
        self.batch_bytes += key.len() + value.len();
        self.batch.put(key, value);
        if event.fill_liquidity.is_none() && event.client_order_id != 0 {
            self.batch.put(
                role_key(2, &event.symbol, event.venue_code, event.client_order_id)?,
                [],
            );
        }
        if self.batch.len() >= BATCH_EVENTS {
            self.flush()?;
        }
        Ok(())
    }

    fn observe_role(&self, event: crate::model::TradeUpdateEvent) -> Result<()> {
        if event.cumulative_filled_quantity <= 0.0 || event.client_order_id == 0 {
            return Ok(());
        }
        let symbol = crate::exec_routing::target_symbol(&event.symbol, event.venue_code);
        if self
            .db
            .get(role_key(
                2,
                &symbol,
                event.venue_code,
                event.client_order_id,
            )?)?
            .is_none()
        {
            return Ok(());
        }
        let key = role_key(1, &symbol, event.venue_code, event.client_order_id)?;
        let observed = if event.is_maker { 1_u8 } else { 2 };
        let role = match self.db.get(&key)? {
            Some(old) if old.as_slice() != [observed] => 0,
            _ => observed,
        };
        self.db.put_opt(key, [role], &Self::writes())?;
        Ok(())
    }

    fn join_role(&self, event: &mut NavEvent) -> Result<()> {
        if event.fill_liquidity.is_none() {
            event.report_liquidity = self
                .db
                .get(role_key(
                    1,
                    &event.symbol,
                    event.venue_code,
                    event.client_order_id,
                )?)?
                .map(|v| match v.as_slice() {
                    [1] => Ok(crate::model::FillLiquidity::Maker),
                    [2] => Ok(crate::model::FillLiquidity::Taker),
                    [0] => Ok(crate::model::FillLiquidity::Unknown),
                    _ => anyhow::bail!("invalid temporary fill role"),
                })
                .transpose()?;
        }
        Ok(())
    }
}

pub(in crate::nav) fn note_key(
    state: &mut IncrementalSourceHistory,
    cf: &str,
    key: &[u8],
) -> Result<()> {
    let ts = parse_record_key_ts(key)?;
    let checkpoint = state.checkpoints.entry(cf.to_owned()).or_default();
    checkpoint.last_record_ts_us = Some(ts);
    checkpoint.seen_keys.insert(key.to_vec());
    if checkpoint.seen_keys.len() > 4096 {
        let bound = rocks_source::format_time_key(ts - NAV_HISTORY_SCAN_OVERLAP_US).into_bytes();
        checkpoint.seen_keys = checkpoint.seen_keys.split_off(&bound);
    }
    Ok(())
}

pub(in crate::nav) fn finish_keys(state: &mut IncrementalSourceHistory) {
    for checkpoint in state.checkpoints.values_mut() {
        if let Some(ts) = checkpoint.last_record_ts_us {
            let bound =
                rocks_source::format_time_key(ts - NAV_HISTORY_SCAN_OVERLAP_US).into_bytes();
            checkpoint.seen_keys = checkpoint.seen_keys.split_off(&bound);
        }
    }
}

fn empty_source(
    source: &SourceConfig,
    snapshots: &SourcePositionSnapshots,
    strategies: &SourceStrategyPositionSnapshots,
    allocations: &Arc<Vec<allocation::AllocationSnapshot>>,
    seeds: impl Iterator<Item = Arc<NavEvent>>,
) -> Result<(CachedTimelineSource, TimelineSourceInputs)> {
    let strategy = strategies.get(&source.id).filter(|s| {
        snapshots
            .get(&source.id)
            .is_none_or(|a| s.snapshot_ts_us >= a.snapshot_ts_us)
    });
    let mut prepared = prepare_source_events(
        source,
        seeds,
        snapshots.get(&source.id),
        strategy,
        &Default::default(),
        allocations,
    )?;
    prepared.fill_events.clear();
    prepared.order_event_count = 0;
    prepared.ignored_at_or_before_snapshot_event_count = 0;
    prepared.ignored_non_fill_event_count = 0;
    // Construct report metadata without executing future ownership corrections.
    let mut empty = prepared.clone();
    empty.allocations = Arc::default();
    let initial_count = effective_position_snapshot(snapshots.get(&source.id), strategy)?
        .map_or(0, |s| s.positions.len());
    let mut cached = CachedTimelineSource::build(source, empty, initial_count)?;
    cached.end_state = initial_timeline_state(source, &prepared)?;
    cached.prepared = prepared;
    let inputs = TimelineSourceInputs {
        allocations: allocations.clone(),
        snapshot: snapshots.get(&source.id).cloned(),
        strategy: strategy.cloned(),
        fees: source.nav_fee_rates()?,
        account: source.display_name().to_owned(),
        venue: source.venue.clone(),
    };
    Ok((cached, inputs))
}

fn append(
    source: &SourceConfig,
    old: &CachedTimelineSource,
    events: &[Arc<NavEvent>],
    through: Option<i64>,
) -> Result<CachedTimelineSource> {
    let mut prepared = old.prepared.clone();
    let first = prepared.fill_events.len();
    for event in events {
        prepared.order_event_count += 1;
        if prepared
            .snapshot_ts_us
            .is_some_and(|ts| fifo_ts_us(event) <= ts)
        {
            prepared.ignored_at_or_before_snapshot_event_count += 1;
            continue;
        }
        if let Some(side) = validated_fill_side(source, event)? {
            let mut fill = PreparedFill {
                event: event.clone(),
                side,
                fill_ts_us: fifo_ts_us(event),
                liquidity: liquidity_role_for_event(event, &Default::default()),
                initial_position_close: false,
            };
            fill.attribute_initial_position(
                prepared.strategy_allocation_active,
                &prepared.initial_states,
            );
            prepared.fill_events.push_back(fill);
        }
    }
    let checkpoint = old.next_checkpoint.or_else(|| {
        prepared
            .fill_events
            .front()
            .map(|f| (f.fill_ts_us.div_euclid(CHECKPOINT_US) + 1) * CHECKPOINT_US)
    });
    let mut cached = CachedTimelineSource::replay(
        source,
        prepared,
        old.report.initial_position_count,
        first,
        old.end_state.clone(),
        old.checkpoints.clone(),
        old.symbols.clone(),
        old.strategies.clone(),
        checkpoint,
        through,
    )?;
    cached.report.first_fill_ts_us = old
        .report
        .first_fill_ts_us
        .or(cached.report.first_fill_ts_us);
    cached.report.last_fill_ts_us = cached.report.last_fill_ts_us.or(old.report.last_fill_ts_us);
    Ok(cached)
}

fn checkpoint(
    cache: &mut NavTimelineCache,
    source: &SourceConfig,
    daily: &DailyFifoStore,
    cursors: &BTreeMap<i64, Vec<u8>>,
    through: i64,
    retain: i64,
) -> Result<()> {
    let id = &source.id;
    let first = cache.start_ts_us(id, through, true);
    let cutoffs = first
        .map(|first| {
            (day_start(first)..day_start(through))
                .step_by(DAY_US as usize)
                .map(|day| day + DAY_US - 1)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let prefixes = funding_estimate::DailyPrefixes::refresh(
        &cache.sources[&(id.clone(), true)].prepared,
        &cutoffs,
        cache.funding_prefixes.get(id).map(AsRef::as_ref),
        None,
    )?;
    cache
        .funding_fingerprints
        .insert(id.clone(), Arc::new(prefixes.fingerprints()));
    cache
        .funding_prefixes
        .insert(id.clone(), Arc::new(prefixes));
    daily.persist_with_cursors(cache, id, through, Some(cursors))?;
    for mode in [false, true] {
        Arc::make_mut(cache.sources.get_mut(&(id.clone(), mode)).unwrap()).retain_since(retain)?;
    }
    Ok(())
}

fn stage(
    event: Arc<NavEvent>,
    key: &[u8],
    ordered: &mut OrderedFills,
    stats: &mut Stats,
    cursors: &mut BTreeMap<i64, Vec<u8>>,
    seeds: &mut BTreeMap<(bool, String, i16), Arc<NavEvent>>,
    anchors: &[Option<i64>; 2],
) -> Result<()> {
    if event.amount_update == 0.0 {
        stats.zero_fills += 1;
        return Ok(());
    }
    // Negative/NaN quantities are retained for the normal validation error.
    ordered.insert(&event)?;
    stats.fills += 1;
    cursors
        .entry(day_start(fifo_ts_us(&event)))
        .and_modify(|old| {
            if key < old.as_slice() {
                *old = key.to_vec();
            }
        })
        .or_insert_with(|| key.to_vec());
    if event.amount_update > 0.0 {
        for mode in [false, true] {
            let anchor = anchors[usize::from(mode)];
            if anchor.is_some_and(|ts| fifo_ts_us(&event) <= ts) {
                continue;
            }
            let entry = seeds
                .entry((mode, event.symbol.clone(), event.venue_code))
                .or_insert_with(|| event.clone());
            if (fifo_ts_us(&event), event.event_ts_us, &event.record_key)
                < (fifo_ts_us(entry), entry.event_ts_us, &entry.record_key)
            {
                *entry = event.clone();
            }
        }
    }
    Ok(())
}

/// All raw reads and disk ordering finish before any replay starts. At most one
/// bounded fill batch, current FIFO state and the final recent tail stay live.
#[cfg(test)]
pub(in crate::nav) fn build(
    config: AppConfig,
    source: &SourceConfig,
    snapshots: Arc<SourcePositionSnapshots>,
    strategies: Arc<SourceStrategyPositionSnapshots>,
    allocations: Arc<Vec<allocation::AllocationSnapshot>>,
    daily: Arc<DailyFifoStore>,
    now: i64,
) -> Result<(NavTimelineCache, IncrementalSourceHistory, Stats)> {
    build_with_state(
        config,
        source,
        snapshots,
        strategies,
        allocations,
        daily,
        now,
        IncrementalSourceHistory::default(),
    )
}

pub(in crate::nav) fn build_with_state(
    mut config: AppConfig,
    source: &SourceConfig,
    snapshots: Arc<SourcePositionSnapshots>,
    strategies: Arc<SourceStrategyPositionSnapshots>,
    allocations: Arc<Vec<allocation::AllocationSnapshot>>,
    daily: Arc<DailyFifoStore>,
    now: i64,
    mut state: IncrementalSourceHistory,
) -> Result<(NavTimelineCache, IncrementalSourceHistory, Stats)> {
    let boundary = day_start(now - MEMORY_WINDOW_US);
    let mut ordered = OrderedFills::open(&daily, source)?;
    let identity = std::fs::read(source.rocksdb_path.join("IDENTITY")).ok();
    let mut metadata = HistoryMetadata::default();
    let mut seeds = BTreeMap::<(bool, String, i16), Arc<NavEvent>>::new();
    let mut cursors = BTreeMap::<i64, Vec<u8>>::new();
    let mut stats = Stats::default();
    let mut last_cf = String::new();
    let anchors = [
        effective_position_snapshot(snapshots.get(&source.id), None)?.map(|s| s.snapshot_ts_us),
        effective_position_snapshot(snapshots.get(&source.id), strategies.get(&source.id))?
            .map(|s| s.snapshot_ts_us),
    ];
    let requests = NAV_HISTORY_COLUMN_FAMILIES
        .iter()
        .map(|cf| (cf.to_string(), None))
        .collect::<Vec<_>>();
    let heads = if rapidx_source(source) {
        if state.history.source_identity != identity {
            state = IncrementalSourceHistory::default();
        }
        if state.history.recovery_view.is_none() {
            let root = daily
                .0
                .path()
                .parent()
                .context("Manager DB has no parent")?
                .join("run/nav-recovery");
            state.recovery = Some(fill_recovery::store::State::open(source, Some(&root))?);
            state.refresh_recovered_source(source, false)?;
        }
        metadata = HistoryMetadata::from_history(&state.history);
        stats.zero_fills = state.recovery.as_ref().map_or(0, |r| r.zero_fill_records());
        let view = state
            .history
            .recovery_view
            .clone()
            .context("missing canonical fill view")?;
        view.visit(|event| {
            let receive = rocks_source::format_time_key(event.event_ts_us);
            stage(
                event,
                receive.as_bytes(),
                &mut ordered,
                &mut stats,
                &mut cursors,
                &mut seeds,
                &anchors,
            )
        })?;
        state.full_rebuild_required = false;
        state.history.read_limits.clone()
    } else {
        state = IncrementalSourceHistory::default();
        rocks_source::visit_column_families_from(
            &source.rocksdb_path,
            &requests,
            |cf, key, bytes| {
                if last_cf != cf {
                    ordered.flush()?;
                    last_cf = cf.to_owned();
                }
                note_key(&mut state, cf, key)?;
                if cf != crate::model::UNIFORM_ORDERS_CF {
                    return ordered.observe_role(decode_trade_update(key, bytes)?);
                }
                let event: Arc<NavEvent> = decode_uniform_order(key, bytes)?.into();
                metadata.observe(&event);
                stage(
                    event,
                    key,
                    &mut ordered,
                    &mut stats,
                    &mut cursors,
                    &mut seeds,
                    &anchors,
                )
            },
        )?
    };
    ordered.flush()?;
    anyhow::ensure!(
        heads.contains_key(crate::model::UNIFORM_ORDERS_CF),
        "missing uniform_orders"
    );
    anyhow::ensure!(
        identity == std::fs::read(source.rocksdb_path.join("IDENTITY")).ok(),
        "Exec database changed during replay scan"
    );
    finish_keys(&mut state);
    // Suffix minima include a future-timestamped fill with an earlier receive
    // key; a day boundary must never turn into a receive-time filter.
    let mut minimum: Option<Vec<u8>> = None;
    for key in cursors.values_mut().rev() {
        if minimum
            .as_ref()
            .is_none_or(|old| key.as_slice() < old.as_slice())
        {
            minimum = Some(key.clone());
        }
        *key = minimum.clone().unwrap();
    }
    state.db_identity = identity.clone();
    state.history.source_identity = identity;
    state.history.read_limits = heads;
    state.history.checkpoint_epoch = HISTORY_REVISION.fetch_add(1, AtomicOrdering::Relaxed);
    state.history.revision = HISTORY_REVISION.fetch_add(1, AtomicOrdering::Relaxed);
    state.history.allocations = allocations.clone();
    state.history.events.clear();
    state.history.change = None;
    state.history.retained_from_us = None;
    state.history.liquidity_by_order.clear();
    state.legacy_fill_times.clear();
    metadata.attach(&mut state.history);
    let mut sources = BTreeMap::<(String, bool), Arc<CachedTimelineSource>>::new();
    let mut inputs = BTreeMap::<(String, bool), TimelineSourceInputs>::new();
    let explicit_strategy = strategies.get(&source.id).is_some_and(|s| {
        snapshots
            .get(&source.id)
            .is_none_or(|a| s.snapshot_ts_us >= a.snapshot_ts_us)
    });
    for mode in [false, true] {
        if mode && !explicit_strategy {
            sources.insert(
                (source.id.clone(), true),
                sources[&(source.id.clone(), false)].clone(),
            );
            inputs.insert(
                (source.id.clone(), true),
                inputs[&(source.id.clone(), false)].clone(),
            );
            continue;
        }
        let mut scoped_strategies = strategies.as_ref().clone();
        if !mode {
            scoped_strategies.remove(&source.id);
        }
        let (cached, input) = empty_source(
            source,
            &snapshots,
            &scoped_strategies,
            &allocations,
            seeds
                .iter()
                .filter(|((allocation, _, _), _)| *allocation == mode)
                .map(|(_, e)| e.clone()),
        )?;
        sources.insert((source.id.clone(), mode), Arc::new(cached));
        inputs.insert((source.id.clone(), mode), input);
    }
    drop(seeds);
    config.sources.retain(|s| s.id == source.id);
    let mut cache = NavTimelineCache {
        config,
        snapshots,
        strategy_snapshots: strategies,
        histories: Arc::new(BTreeMap::from([(source.id.clone(), state.history.clone())])),
        sources,
        inputs,
        results: Arc::new(crate::analysis::QueryCache::new(0)),
        mark_plans: Arc::new(crate::analysis::QueryCache::new(0)),
        daily: Some(daily.clone()),
        funding_fingerprints: BTreeMap::new(),
        funding_prefixes: BTreeMap::new(),
    };
    daily.invalidate(&source.id, i64::MIN)?;
    let mut batch = Vec::new();
    let mut bytes = 0;
    let flush = |batch: &mut Vec<Arc<NavEvent>>,
                 cache: &mut NavTimelineCache,
                 state: &mut IncrementalSourceHistory,
                 stats: &mut Stats,
                 bytes: usize|
     -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        stats.max_buffered_events = stats.max_buffered_events.max(batch.len());
        stats.max_buffered_bytes = stats.max_buffered_bytes.max(bytes);
        let through = fifo_ts_us(batch.last().unwrap());
        for mode in [false, true] {
            if mode && !explicit_strategy {
                cache.sources.insert(
                    (source.id.clone(), true),
                    cache.sources[&(source.id.clone(), false)].clone(),
                );
            } else {
                let current = &cache.sources[&(source.id.clone(), mode)];
                let next = append(source, current, batch, Some(through))?;
                cache
                    .sources
                    .insert((source.id.clone(), mode), Arc::new(next));
            }
        }
        for event in batch.drain(..) {
            if fifo_ts_us(&event) >= boundary || event.event_ts_us >= boundary {
                if event.fill_liquidity.is_none() {
                    state
                        .legacy_fill_times
                        .entry((
                            event.symbol.clone(),
                            event.venue_code,
                            event.client_order_id,
                        ))
                        .and_modify(|old| *old = (*old).min(fifo_ts_us(&event)))
                        .or_insert(fifo_ts_us(&event));
                }
                state.history.events.push_back(event);
            }
        }
        Arc::make_mut(&mut cache.histories).insert(source.id.clone(), state.history.clone());
        checkpoint(
            cache,
            source,
            &daily,
            &cursors,
            through.min(now),
            boundary.min(day_start(through)),
        )?;
        Ok(())
    };
    for row in ordered.db.iterator(IteratorMode::Start) {
        let (key, value) = row?;
        if key.first() != Some(&0) {
            break;
        }
        if batch.len() >= BATCH_EVENTS || bytes + value.len() > BATCH_BYTES {
            flush(&mut batch, &mut cache, &mut state, &mut stats, bytes)?;
            bytes = 0;
        }
        let mut event: NavEvent = serde_json::from_slice(&value)?;
        ordered.join_role(&mut event)?;
        bytes += value.len();
        batch.push(Arc::new(event));
    }
    flush(&mut batch, &mut cache, &mut state, &mut stats, bytes)?;
    for mode in [false, true] {
        if mode && !explicit_strategy {
            cache.sources.insert(
                (source.id.clone(), true),
                cache.sources[&(source.id.clone(), false)].clone(),
            );
        } else {
            let next = append(
                source,
                &cache.sources[&(source.id.clone(), mode)],
                &[],
                None,
            )?;
            cache
                .sources
                .insert((source.id.clone(), mode), Arc::new(next));
        }
    }
    checkpoint(&mut cache, source, &daily, &cursors, now, boundary)?;
    state
        .history
        .events
        .sort_by(|a, b| a.record_key.cmp(&b.record_key));
    state.history.retained_from_us = Some(boundary);
    Arc::make_mut(&mut cache.histories).insert(source.id.clone(), state.history.clone());
    info!(source_id=%source.id, fills=stats.fills, skipped_zero_fills=stats.zero_fills,
        max_buffered_fills=stats.max_buffered_events, max_buffered_bytes=stats.max_buffered_bytes,
        retained_fills=state.history.events.len(), "completed bounded NAV fill rebuild");
    Ok((cache, state, stats))
}
