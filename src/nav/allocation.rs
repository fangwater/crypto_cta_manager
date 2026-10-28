//! Audited, timestamped ownership corrections. These are not exchange fills and
//! never change account FIFO, execution counts, volume, fees or historical rows.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AllocationSnapshot {
    pub source_id: String,
    pub snapshot_ts_us: i64,
    pub positions: Vec<AllocationPosition>,
    /// Explicitly audited new preconditions for a later immutable correction.
    /// The original database row and its ownership targets remain untouched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restatements: Vec<AllocationRestatement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AllocationRestatement {
    pub snapshot_ts_us: i64,
    pub original_positions_sha256: String,
    pub before: Vec<AllocationQuantity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AllocationQuantity {
    pub strategy: String,
    pub symbol: String,
    pub venue_code: i16,
    pub quantity: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnershipObservation {
    pub snapshot_ts_us: i64,
    pub positions: Vec<AllocationQuantity>,
}

pub fn positions_fingerprint(snapshot: &AllocationSnapshot) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &snapshot.positions,
    )?)))
}

/// Resolve append-only historical amendments before replay. Only the audited
/// before quantities can change; later targets, marks and raw records cannot.
pub fn resolve(snapshots: &[AllocationSnapshot]) -> Result<Vec<AllocationSnapshot>> {
    let mut result = snapshots.to_vec();
    let mut indexes = BTreeMap::new();
    for (index, snapshot) in snapshots.iter().enumerate() {
        snapshot.validate()?;
        anyhow::ensure!(
            indexes
                .insert((&snapshot.source_id, snapshot.snapshot_ts_us), index)
                .is_none(),
            "duplicate allocation timestamp"
        );
    }
    let mut amended = BTreeSet::new();
    for snapshot in snapshots {
        for revision in &snapshot.restatements {
            let key = (&snapshot.source_id, revision.snapshot_ts_us);
            let index = *indexes
                .get(&key)
                .context("restated allocation is missing")?;
            anyhow::ensure!(
                snapshot.uses_residual() == snapshots[index].uses_residual(),
                "restatement cannot cross account/strategy allocation scopes"
            );
            anyhow::ensure!(
                amended.insert(key),
                "allocation preconditions restated twice"
            );
            anyhow::ensure!(
                positions_fingerprint(&snapshots[index])? == revision.original_positions_sha256,
                "restated allocation original fingerprint changed"
            );
            let before: BTreeMap<_, _> = revision
                .before
                .iter()
                .map(|p| ((&p.strategy, &p.symbol, p.venue_code), p.quantity))
                .collect();
            anyhow::ensure!(
                before.len() == revision.before.len()
                    && before.len() == result[index].positions.len(),
                "restatement must name every original position exactly once"
            );
            for p in &mut result[index].positions {
                p.before_qty = *before
                    .get(&(&p.strategy, &p.symbol, p.venue_code))
                    .context("restatement position mismatch")?;
            }
            result[index].validate()?;
        }
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AllocationPosition {
    pub strategy: String,
    pub symbol: String,
    pub venue_code: i16,
    pub before_qty: f64,
    pub after_qty: f64,
    pub reference_price: f64,
}

impl AllocationSnapshot {
    /// An explicit residual leg identifies ownership relative to an immutable
    /// strategy anchor. The residual remains derived from the account ledger.
    pub(super) fn uses_residual(&self) -> bool {
        self.positions
            .iter()
            .any(|p| p.strategy == UNALLOCATED_STRATEGY)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.source_id.trim().is_empty() && self.snapshot_ts_us > 0,
            "allocation source and timestamp are required"
        );
        anyhow::ensure!(!self.positions.is_empty(), "allocation snapshot is empty");
        for revision in &self.restatements {
            anyhow::ensure!(
                revision.snapshot_ts_us > self.snapshot_ts_us,
                "only a later allocation can be restated"
            );
            anyhow::ensure!(
                revision.original_positions_sha256.len() == 64
                    && revision
                        .original_positions_sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit()),
                "invalid restatement fingerprint"
            );
            anyhow::ensure!(
                !revision.before.is_empty()
                    && revision.before.iter().all(|p| p.quantity.is_finite()),
                "invalid restatement quantities"
            );
        }
        let mut keys = BTreeSet::new();
        let mut markets = BTreeMap::<(&str, i16), (f64, f64)>::new();
        for p in &self.positions {
            anyhow::ensure!(
                matches!(p.venue_code, 0 | 1),
                "allocation supports Binance linear/cash only"
            );
            anyhow::ensure!(
                !p.symbol.is_empty() && p.symbol.trim() == p.symbol,
                "invalid allocation symbol"
            );
            anyhow::ensure!(
                strategy_from_from_key(&format!("batch_exec:{}", p.strategy)) == p.strategy,
                "invalid allocation strategy"
            );
            anyhow::ensure!(
                p.before_qty.is_finite()
                    && p.after_qty.is_finite()
                    && p.reference_price.is_finite()
                    && p.reference_price > 0.0,
                "invalid allocation quantity or price"
            );
            anyhow::ensure!(
                keys.insert((&p.strategy, &p.symbol, p.venue_code)),
                "duplicate allocation position"
            );
            let market = markets
                .entry((&p.symbol, p.venue_code))
                .or_insert((0.0, p.reference_price));
            anyhow::ensure!(
                (market.1 - p.reference_price).abs() <= market.1 * 1e-12,
                "allocation market must share one reference price"
            );
            market.0 += p.after_qty - p.before_qty;
        }
        for ((symbol, venue), (delta, price)) in markets {
            anyhow::ensure!(
                (delta * price).abs() < 1e-6,
                "allocation changes account quantity for {symbol} venue {venue}: {delta}"
            );
        }
        Ok(())
    }
}

pub async fn load(pool: &PgPool) -> Result<BTreeMap<String, Arc<Vec<AllocationSnapshot>>>> {
    let rows = sqlx::query("SELECT source_id,snapshot_ts_us,positions,restatements FROM cta_strategy_allocation_snapshots ORDER BY source_id,snapshot_ts_us")
        .fetch_all(pool).await.context("load strategy allocation snapshots; apply explicit allocation schema maintenance first")?;
    let mut result = BTreeMap::<String, Vec<AllocationSnapshot>>::new();
    for row in rows {
        let snapshot = AllocationSnapshot {
            source_id: row.try_get("source_id")?,
            snapshot_ts_us: row.try_get("snapshot_ts_us")?,
            positions: serde_json::from_value(row.try_get("positions")?)?,
            restatements: serde_json::from_value(row.try_get("restatements")?)?,
        };
        snapshot.validate()?;
        result
            .entry(snapshot.source_id.clone())
            .or_default()
            .push(snapshot);
    }
    Ok(result.into_iter().map(|(s, a)| (s, Arc::new(a))).collect())
}

pub fn attach(
    histories: &mut NavSourceHistories,
    snapshots: &BTreeMap<String, Arc<Vec<AllocationSnapshot>>>,
) {
    for (source, history) in histories {
        history.allocations = snapshots.get(source).cloned().unwrap_or_default();
    }
}

pub async fn insert(pool: &PgPool, snapshot: &AllocationSnapshot, note: &str) -> Result<()> {
    snapshot.validate()?;
    anyhow::ensure!(!note.trim().is_empty(), "allocation audit note is required");
    sqlx::query("INSERT INTO cta_strategy_allocation_snapshots (source_id,snapshot_ts_us,positions,restatements,note) VALUES ($1,$2,$3,$4,$5)")
        .bind(&snapshot.source_id).bind(snapshot.snapshot_ts_us)
        .bind(serde_json::to_value(&snapshot.positions)?).bind(serde_json::to_value(&snapshot.restatements)?).bind(note)
        .execute(pool).await.context("insert immutable strategy allocation snapshot")?;
    Ok(())
}

impl TimelineSourceState {
    pub(super) fn advance_allocations(&mut self, ts_us: i64, inclusive: bool) -> Result<()> {
        while let Some(snapshot) = self.allocations.get(self.next_allocation) {
            if snapshot.snapshot_ts_us > ts_us || (!inclusive && snapshot.snapshot_ts_us == ts_us) {
                break;
            }
            let snapshot = snapshot.clone();
            self.apply_allocation(&snapshot)?;
            self.next_allocation += 1;
        }
        Ok(())
    }

    fn apply_allocation(&mut self, snapshot: &AllocationSnapshot) -> Result<()> {
        // A correction can be the first event after the opening anchor.
        self.activate_snapshot_at(snapshot.snapshot_ts_us);
        // Verify the complete old projection before mutating any bucket.
        for p in &snapshot.positions {
            let before = self.allocation_quantity(&p.strategy, &p.symbol, p.venue_code);
            anyhow::ensure!(
                ((before - p.before_qty) * p.reference_price).abs() < 1e-6,
                "source {} allocation {} no longer matches {} {} historical quantity; review late fills",
                self.source_id,
                snapshot.snapshot_ts_us,
                p.strategy,
                p.symbol
            );
        }
        for p in &snapshot.positions {
            if p.strategy == UNALLOCATED_STRATEGY {
                // Named legs change the residual by conservation. Creating
                // a second FIFO for it would count the transfer twice.
                continue;
            }
            let key = (p.strategy.clone(), p.symbol.clone(), p.venue_code);
            let state = self.strategy_states.entry(key).or_insert_with(|| {
                VenueState::from_initial_position(
                    p.venue_code,
                    venue_name(p.venue_code as u8),
                    0.0,
                    p.reference_price,
                    InitialReferencePriceSource::Configured,
                )
            });
            let before = state.fifo.long_quantity(1.0) - state.fifo.short_quantity(1.0);
            let delta = p.after_qty - before;
            if delta != 0.0 {
                state.fifo.apply_fill(
                    if delta > 0.0 { Side::Buy } else { Side::Sell },
                    p.reference_price,
                    delta.abs(),
                );
            }
            if p.after_qty == 0.0 {
                // The observed target is exactly flat. Remove arithmetic
                // dust after realizing the transferred quantity at its mark.
                state.fifo.longs.clear();
                state.fifo.shorts.clear();
            }
        }
        Ok(())
    }

    fn allocation_quantity(&self, strategy: &str, symbol: &str, venue: i16) -> f64 {
        let quantity =
            |state: &VenueState| state.fifo.long_quantity(1.0) - state.fifo.short_quantity(1.0);
        if strategy == UNALLOCATED_STRATEGY {
            return self
                .states
                .get(&(symbol.to_owned(), venue))
                .map_or(0.0, quantity)
                - self
                    .strategy_states
                    .iter()
                    .filter(|((_, s, v), _)| s == symbol && *v == venue)
                    .map(|(_, state)| quantity(state))
                    .sum::<f64>();
        }
        self.strategy_states
            .get(&(strategy.to_owned(), symbol.to_owned(), venue))
            .map_or(0.0, quantity)
    }
}

#[derive(Debug, Serialize)]
pub struct AllocationAudit {
    pub source_id: String,
    pub snapshot_ts_us: i64,
    pub account: Vec<(String, i16, f64)>,
    pub strategies: Vec<(String, String, i16, f64)>,
}

// Replay an immutable correction at its original place between observations.
// Later before-values may be restated; targets, marks and record bytes stay fixed.
// A partial later correction that cannot balance these targets is refused.
fn apply_historical_original(
    state: &mut TimelineSourceState,
    prepared: &PreparedSourceEvents,
    fill_index: &mut usize,
    candidates: &mut [AllocationSnapshot],
    original: &AllocationSnapshot,
) -> Result<()> {
    while *fill_index < prepared.fill_events.len()
        && prepared.fill_events[*fill_index].fill_ts_us <= original.snapshot_ts_us
    {
        state.apply_fill(&prepared.fill_events[*fill_index])?;
        *fill_index += 1;
    }
    let mut effective = original.clone();
    if let Some(candidate) = candidates.last_mut() {
        for p in &mut effective.positions {
            p.before_qty = state.allocation_quantity(&p.strategy, &p.symbol, p.venue_code);
        }
        effective.validate()?;
        if effective.positions != original.positions {
            candidate.restatements.push(AllocationRestatement {
                snapshot_ts_us: original.snapshot_ts_us,
                original_positions_sha256: positions_fingerprint(original)?,
                before: effective
                    .positions
                    .iter()
                    .map(|p| AllocationQuantity {
                        strategy: p.strategy.clone(),
                        symbol: p.symbol.clone(),
                        venue_code: p.venue_code,
                        quantity: p.before_qty,
                    })
                    .collect(),
            });
        }
    }
    state.apply_allocation(&effective)
}

impl NavTimelineCache {
    /// Prepare historical transfers from independently audited ownership
    /// observations. Account FIFO and factual fills remain the original stream.
    /// Later immutable corrections retain their targets and receive explicit,
    /// fingerprint-bound preconditions in the new append-only batch.
    pub fn prepare_historical_allocations(
        &self,
        source_id: &str,
        observations: &[OwnershipObservation],
        mut mark: impl FnMut(&str, i16, i64) -> Result<f64>,
    ) -> Result<Vec<AllocationSnapshot>> {
        anyhow::ensure!(
            !observations.is_empty(),
            "historical observations are empty"
        );
        anyhow::ensure!(
            observations
                .windows(2)
                .all(|w| w[0].snapshot_ts_us < w[1].snapshot_ts_us),
            "historical observations must increase"
        );
        let anchored = self.strategy_snapshots.contains_key(source_id);
        let source = self
            .sources
            .get(&(source_id.to_owned(), anchored))
            .context("unknown historical source")?;
        let config = self
            .config
            .sources
            .iter()
            .find(|s| s.id == source_id)
            .context("unknown source")?;
        let prepared = &source.prepared;
        let counterparty = if anchored {
            UNALLOCATED_STRATEGY
        } else {
            "system_position_close"
        };
        anyhow::ensure!(
            prepared
                .allocations
                .iter()
                .all(|a| a.restatements.is_empty()),
            "existing historical amendments require separate review"
        );
        anyhow::ensure!(
            prepared
                .snapshot_ts_us
                .is_none_or(|ts| ts < observations[0].snapshot_ts_us),
            "historical observation precedes opening anchor"
        );
        for observation in observations {
            anyhow::ensure!(
                prepared
                    .allocations
                    .binary_search_by_key(&observation.snapshot_ts_us, |a| a.snapshot_ts_us)
                    .is_err(),
                "observation collides with an immutable correction"
            );
        }
        let mut state = initial_timeline_state(config, prepared)?;
        state.allocations = Arc::default();
        state.next_allocation = 0;
        let mut index = 0;
        let mut candidates = Vec::new();
        let mut original_index = 0;
        for observation in observations {
            while original_index < prepared.allocations.len()
                && prepared.allocations[original_index].snapshot_ts_us < observation.snapshot_ts_us
            {
                apply_historical_original(
                    &mut state,
                    prepared,
                    &mut index,
                    &mut candidates,
                    &prepared.allocations[original_index],
                )?;
                original_index += 1;
            }
            while index < prepared.fill_events.len()
                && prepared.fill_events[index].fill_ts_us <= observation.snapshot_ts_us
            {
                state.apply_fill(&prepared.fill_events[index])?;
                index += 1;
            }
            state.activate_snapshot_at(observation.snapshot_ts_us);
            let mut keys = BTreeSet::new();
            let mut markets = BTreeMap::<(String, i16), (f64, f64)>::new();
            let mut positions = Vec::new();
            for p in &observation.positions {
                anyhow::ensure!(
                    p.venue_code == 1
                        && p.quantity.is_finite()
                        && (!p.strategy.starts_with("__") || p.strategy == UNATTRIBUTED_STRATEGY)
                        && !is_system_position_close(&p.strategy),
                    "historical observation must identify a concrete linear owner"
                );
                anyhow::ensure!(
                    keys.insert((&p.strategy, &p.symbol, p.venue_code)),
                    "duplicate observed ownership"
                );
                let before = state.allocation_quantity(&p.strategy, &p.symbol, p.venue_code);
                if (p.quantity - before).abs() < 1e-12 {
                    continue;
                }
                let market_key = (p.symbol.clone(), p.venue_code);
                let price = if let Some((_, price)) = markets.get(&market_key) {
                    *price
                } else {
                    mark(&p.symbol, p.venue_code, observation.snapshot_ts_us)?
                };
                let market = markets.entry(market_key).or_insert((0.0, price));
                market.0 += p.quantity - before;
                positions.push(AllocationPosition {
                    strategy: p.strategy.clone(),
                    symbol: p.symbol.clone(),
                    venue_code: p.venue_code,
                    before_qty: before,
                    after_qty: p.quantity,
                    reference_price: price,
                });
            }
            if positions.is_empty() {
                continue;
            }
            for ((symbol, venue_code), (delta, price)) in markets {
                let before = state.allocation_quantity(counterparty, &symbol, venue_code);
                positions.push(AllocationPosition {
                    strategy: counterparty.into(),
                    symbol,
                    venue_code,
                    before_qty: before,
                    after_qty: before - delta,
                    reference_price: price,
                });
            }
            let snapshot = AllocationSnapshot {
                source_id: source_id.into(),
                snapshot_ts_us: observation.snapshot_ts_us,
                positions,
                restatements: Vec::new(),
            };
            snapshot.validate()?;
            state.apply_allocation(&snapshot)?;
            candidates.push(snapshot);
        }
        anyhow::ensure!(
            !candidates.is_empty(),
            "historical observations change no ownership"
        );
        for original in prepared.allocations.iter().skip(original_index) {
            apply_historical_original(&mut state, prepared, &mut index, &mut candidates, original)?;
        }
        Ok(candidates)
    }

    /// Uses the exact recovered/deduplicated NAV projection, including RapidX.
    pub fn allocation_audit(&self, source_id: &str, ts_us: i64) -> Result<AllocationAudit> {
        let strategy_allocation = self.strategy_snapshots.contains_key(source_id);
        let source = self
            .sources
            .get(&(source_id.to_owned(), strategy_allocation))
            .context("unknown allocation source")?;
        let config = self
            .config
            .sources
            .iter()
            .find(|s| s.id == source_id)
            .context("unknown source configuration")?;
        let mut state = initial_timeline_state(config, &source.prepared)?;
        for fill in &source.prepared.fill_events {
            if fill.fill_ts_us > ts_us {
                break;
            }
            state.apply_fill(fill)?;
        }
        state.activate_snapshot_at(ts_us);
        state.advance_allocations(ts_us, true)?;
        let mut strategies: Vec<_> = state
            .strategy_states
            .iter()
            .map(|((st, s, v), state)| {
                (
                    st.clone(),
                    s.clone(),
                    *v,
                    state.fifo.long_quantity(1.0) - state.fifo.short_quantity(1.0),
                )
            })
            .collect();
        if state.strategy_allocation_active {
            let markets: BTreeSet<_> = state
                .states
                .keys()
                .cloned()
                .chain(
                    state
                        .strategy_states
                        .keys()
                        .map(|(_, s, v)| (s.clone(), *v)),
                )
                .collect();
            strategies.extend(markets.into_iter().map(|(s, v)| {
                let q = state.allocation_quantity(UNALLOCATED_STRATEGY, &s, v);
                (UNALLOCATED_STRATEGY.to_owned(), s, v, q)
            }));
            strategies.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
        }
        Ok(AllocationAudit {
            source_id: source_id.to_owned(),
            snapshot_ts_us: ts_us,
            account: state
                .states
                .iter()
                .map(|((s, v), state)| {
                    (
                        s.clone(),
                        *v,
                        state.fifo.long_quantity(1.0) - state.fifo.short_quantity(1.0),
                    )
                })
                .collect(),
            strategies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HOUR: i64 = 3_600_000_000;

    fn fixture() -> (AppConfig, NavSourceHistories, AllocationSnapshot) {
        let mut config: AppConfig =
            toml::from_str(include_str!("../../config/cta-manager.example.toml")).unwrap();
        config.sources.truncate(1);
        config.sources[0].id = "alignment".into();
        config.sources[0].enabled = true;
        config.sources[0].venue = "binance-futures".into();
        config.sources[0].estimated_fee_rate = Some(0.001);
        config.sources[0].maker_fee_rate = Some(0.001);
        config.sources[0].taker_fee_rate = Some(0.001);
        let event = |ts: i64, strategy: &str, side, price, qty| {
            Arc::new(NavEvent {
                record_key: ts.to_string(),
                event_ts_us: ts,
                create_ts_us: ts,
                update_ts_us: ts,
                signal_ts_us: ts,
                symbol: "BTCUSDT".into(),
                client_order_id: ts,
                venue_code: 1,
                venue: venue_name(1),
                order_type_code: 1,
                side_code: side,
                price,
                amount_update: qty,
                from_key_text: format!("batch_exec:{strategy}"),
                fill_liquidity: None,
            })
        };
        let history = NavSourceHistory {
            events: vec![
                event(10, "alpha", 1, 100.0, 2.0),
                event(20, "system_position_close", 2, 110.0, 2.0),
                event(2 * HOUR, "beta", 1, 150.0, 1.0),
                event(3 * HOUR, "beta", 2, 160.0, 1.0),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let snapshot = AllocationSnapshot {
            source_id: "alignment".into(),
            snapshot_ts_us: HOUR,
            restatements: Vec::new(),
            positions: vec![
                AllocationPosition {
                    strategy: "alpha".into(),
                    symbol: "BTCUSDT".into(),
                    venue_code: 1,
                    before_qty: 2.0,
                    after_qty: 0.0,
                    reference_price: 120.0,
                },
                AllocationPosition {
                    strategy: "system_position_close".into(),
                    symbol: "BTCUSDT".into(),
                    venue_code: 1,
                    before_qty: -2.0,
                    after_qty: 0.0,
                    reference_price: 120.0,
                },
            ],
        };
        (
            config,
            BTreeMap::from([("alignment".into(), history)]),
            snapshot,
        )
    }

    fn build(config: &AppConfig, histories: NavSourceHistories) -> NavTimelineCache {
        NavTimelineCache::build(
            config.clone(),
            Arc::default(),
            Arc::default(),
            Arc::new(histories),
        )
        .unwrap()
    }

    #[test]
    fn later_correction_keeps_earlier_symbols_and_owners_visible_from_checkpoint() {
        let (config, mut histories, mut correction) = fixture();
        let history = histories.get_mut("alignment").unwrap();
        for index in [2, 3] {
            Arc::make_mut(&mut history.events[index]).symbol = "ETHUSDT".into();
        }
        correction.snapshot_ts_us = 4 * HOUR;
        let original = build(&config, histories.clone());
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![correction]))]),
        );
        let corrected = build(&config, histories);
        // BTC and its owners have no new fills after the 2h checkpoint. Their
        // first appearance must remain at the old fills, before the 4h repair.
        let start = 2 * HOUR;
        let end = 3 * HOUR - 1;
        assert_eq!(
            *query(&original, start, end),
            *query(&corrected, start, end)
        );
    }
    fn query(cache: &NavTimelineCache, start: i64, end: i64) -> Arc<NavTimelineReport> {
        cache
            .rebuild(
                NavTimelineRequest {
                    start_ts_us: Some(start),
                    end_ts_us: end,
                    selected_source_ids: vec!["alignment".into()],
                    selected_symbols: vec![],
                    max_points: 1000,
                    tick_interval_us: None,
                },
                false,
            )
            .unwrap()
    }

    #[test]
    fn correction_preserves_account_history_fees_and_freezes_closed_ownership() {
        let (config, mut histories, snapshot) = fixture();
        let original = build(&config, histories.clone());
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![snapshot]))]),
        );
        let corrected = build(&config, histories);
        assert_eq!(
            *query(&original, 10, HOUR - 1),
            *query(&corrected, 10, HOUR - 1)
        );
        for (start, end) in [(10, 4 * HOUR), (HOUR + 1, 4 * HOUR), (HOUR, HOUR)] {
            let before = query(&original, start, end);
            let after = query(&corrected, start, end);
            assert_eq!(before.summary, after.summary);
            assert_eq!(before.symbols, after.symbols);
            assert_eq!(before.points, after.points);
            let sum: f64 = after
                .strategy_points
                .iter()
                .map(|s| s.summary.nav_change_after_fee_quote)
                .sum();
            assert!((sum - after.summary.nav_change_after_fee_quote).abs() < 1e-10);
        }
        let after = query(&corrected, HOUR + 1, 4 * HOUR);
        assert!(
            !after
                .available_strategies
                .contains(&"system_position_close".into())
        );
        let history = query(&corrected, 10, 4 * HOUR);
        let system = history
            .strategy_points
            .iter()
            .find(|s| s.strategy == "system_position_close")
            .unwrap();
        assert_eq!(system.gross_position_value_quote, 0.0);
        assert_eq!(system.summary.fill_count, 1);
        let export = corrected
            .rebuild_strategy_pnl(StrategyPnlRequest {
                source_id: "alignment".into(),
                strategy_name: "system_position_close".into(),
                start_ts_us: 10,
                end_ts_us: 4 * HOUR,
            })
            .unwrap();
        assert_eq!(export.rows.last().unwrap().totals, system.summary);
        let audit = corrected.allocation_audit("alignment", HOUR).unwrap();
        assert!(audit.strategies.iter().all(|(_, _, _, q)| *q == 0.0));
    }

    #[test]
    fn rejects_cross_market_balance_and_stale_projection() {
        let (config, mut histories, mut snapshot) = fixture();
        snapshot.positions[1].venue_code = 0;
        assert!(snapshot.validate().is_err());
        snapshot.positions[1].venue_code = 1;
        snapshot.positions[0].before_qty = 3.0;
        snapshot.positions[1].before_qty = -3.0;
        snapshot.validate().unwrap();
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![snapshot]))]),
        );
        assert!(
            NavTimelineCache::build(config, Arc::default(), Arc::default(), Arc::new(histories))
                .is_err()
        );
    }

    #[test]
    fn funding_uses_old_owner_at_boundary_and_corrected_owner_afterwards() {
        let (config, mut histories, snapshot) = fixture();
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![snapshot]))]),
        );
        let cache = build(&config, histories);
        let report = query(&cache, 10, 4 * HOUR);
        let rates = BTreeMap::from([(
            "BTCUSDT".into(),
            Arc::new(vec![
                crate::funding_rates::FundingRate {
                    symbol: "BTCUSDT".into(),
                    ts_us: HOUR,
                    rate: 0.001,
                    mark_price: 120.0,
                    rate_type: "regular".into(),
                },
                crate::funding_rates::FundingRate {
                    symbol: "BTCUSDT".into(),
                    ts_us: 2 * HOUR,
                    rate: 0.001,
                    mark_price: 150.0,
                    rate_type: "regular".into(),
                },
            ]),
        )]);
        let funding = cache
            .estimate_funding_records(&report, false, &rates)
            .unwrap()
            .records;
        assert_eq!(funding[0].quantity, 0.0);
        assert!((funding[0].strategies["system_position_close"] - 0.24).abs() < 1e-10);
        assert_eq!(funding[1].quantity, 0.0);
        assert_eq!(funding[1].strategies["system_position_close"], 0.0);
    }

    #[test]
    fn minute_marks_and_replacement_cache_keep_account_and_history_unchanged() {
        let (config, mut histories, snapshot) = fixture();
        let original = build(&config, histories.clone());
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![snapshot]))]),
        );
        let corrected = NavTimelineCache::refresh(
            config,
            Arc::default(),
            Arc::default(),
            Arc::new(histories),
            Some(&original),
        )
        .unwrap();
        let marks = MinuteMarks::Loaded {
            revision: 1,
            prices: BTreeMap::from([(
                ("BTCUSDT".into(), 1),
                (-1..=240)
                    .map(|minute| {
                        (
                            minute * 60_000_000,
                            if minute < 59 {
                                110.0
                            } else {
                                120.0 + (minute - 59) as f64
                            },
                        )
                    })
                    .collect(),
            )]),
        };
        let request = |start, end| NavTimelineRequest {
            start_ts_us: Some(start),
            end_ts_us: end,
            selected_source_ids: vec!["alignment".into()],
            selected_symbols: vec![],
            max_points: 1000,
            tick_interval_us: None,
        };
        let old = original
            .rebuild_with_prices(request(10, HOUR - 1), false, Some(&marks))
            .unwrap();
        let new = corrected
            .rebuild_with_prices(request(10, HOUR - 1), false, Some(&marks))
            .unwrap();
        assert_eq!(old, new);
        let old = original
            .rebuild_with_prices(request(10, 4 * HOUR), false, Some(&marks))
            .unwrap();
        let new = corrected
            .rebuild_with_prices(request(10, 4 * HOUR), false, Some(&marks))
            .unwrap();
        assert_eq!(old.summary, new.summary);
        assert_eq!(old.points, new.points);
        for point in &new.points {
            let sum: f64 = new
                .strategy_points
                .iter()
                .filter_map(|s| s.points.iter().find(|p| p.ts_us == point.ts_us))
                .map(|p| p.totals.nav_change_after_fee_quote)
                .sum();
            assert!((sum - point.totals.nav_change_after_fee_quote).abs() < 1e-9);
        }
        let new = corrected
            .rebuild_with_prices(request(HOUR + 1, 4 * HOUR), false, Some(&marks))
            .unwrap();
        assert!(
            !new.available_strategies
                .contains(&"system_position_close".into())
        );
    }

    #[test]
    fn boundary_fill_precedes_the_observed_allocation_and_sources_stay_isolated() {
        let (mut config, mut histories, mut snapshot) = fixture();
        snapshot.snapshot_ts_us = 20;
        snapshot
            .positions
            .iter_mut()
            .for_each(|p| p.reference_price = 110.0);
        let mut other = config.sources[0].clone();
        other.id = "other".into();
        config.sources.push(other);
        histories.insert("other".into(), histories["alignment"].clone());
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![snapshot]))]),
        );
        let cache = build(&config, histories);
        let corrected = cache.allocation_audit("alignment", 20).unwrap();
        let other = cache.allocation_audit("other", 20).unwrap();
        assert!(corrected.strategies.iter().all(|(_, _, _, q)| *q == 0.0));
        assert!(
            other
                .strategies
                .iter()
                .any(|(s, _, _, q)| s == "system_position_close" && *q == -2.0)
        );
        let report = query(&cache, 20, 20);
        assert_eq!(report.summary.fill_count, 1);
        assert_eq!(report.summary.volume_quote, 220.0);
    }

    #[test]
    fn historical_repair_without_opening_anchor_transfers_real_system_inventory() {
        let (config, mut histories, _) = fixture();
        let original = build(&config, histories.clone());
        let observation = OwnershipObservation {
            snapshot_ts_us: HOUR,
            positions: vec![AllocationQuantity {
                strategy: "alpha".into(),
                symbol: "BTCUSDT".into(),
                venue_code: 1,
                quantity: 0.0,
            }],
        };
        let batch = original
            .prepare_historical_allocations("alignment", &[observation], |_, _, _| Ok(120.0))
            .unwrap();
        assert_eq!(batch.len(), 1);
        assert!(
            batch[0]
                .positions
                .iter()
                .any(|p| p.strategy == "system_position_close"
                    && p.before_qty == -2.0
                    && p.after_qty == 0.0)
        );
        assert!(!batch[0].uses_residual());
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(batch))]),
        );
        let corrected = build(&config, histories);
        assert_eq!(
            *query(&original, 10, HOUR - 1),
            *query(&corrected, 10, HOUR - 1)
        );
        let before = query(&original, 10, 4 * HOUR);
        let after = query(&corrected, 10, 4 * HOUR);
        assert_eq!(before.summary, after.summary);
        assert_eq!(before.symbols, after.symbols);
        assert_eq!(before.points, after.points);
        let audit = corrected.allocation_audit("alignment", HOUR).unwrap();
        assert!(audit.strategies.iter().all(|(_, _, _, q)| *q == 0.0));
        assert!(
            (after
                .strategy_points
                .iter()
                .map(|s| s.summary.nav_change_after_fee_quote)
                .sum::<f64>()
                - after.summary.nav_change_after_fee_quote)
                .abs()
                < 1e-8
        );
    }

    #[test]
    fn historical_observations_interleave_immutable_corrections_without_changing_targets() {
        let (config, mut histories, mut later) = fixture();
        let mut earlier = later.clone();
        earlier.snapshot_ts_us = 30;
        earlier.positions[0].after_qty = 1.0;
        earlier.positions[1].after_qty = -1.0;
        later.positions[0].before_qty = 1.0;
        later.positions[1].before_qty = -1.0;
        let originals = vec![earlier.clone(), later.clone()];
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(originals.clone()))]),
        );
        let original = build(&config, histories.clone());
        let observation = |ts, strategy: &str, quantity| OwnershipObservation {
            snapshot_ts_us: ts,
            positions: vec![AllocationQuantity {
                strategy: strategy.into(),
                symbol: "BTCUSDT".into(),
                venue_code: 1,
                quantity,
            }],
        };
        let batch = original
            .prepare_historical_allocations(
                "alignment",
                &[
                    observation(40, "alpha", 0.5),
                    observation(HOUR + 1, "alpha", 0.25),
                ],
                |_, _, _| Ok(115.0),
            )
            .unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].restatements.len(), 1);
        assert!(batch[1].restatements.is_empty());
        assert_eq!(batch[0].restatements[0].snapshot_ts_us, HOUR);
        let mut raw = originals.clone();
        raw.extend(batch);
        raw.sort_by_key(|r| r.snapshot_ts_us);
        let resolved = resolve(&raw).unwrap();
        assert_eq!(raw[0], earlier);
        assert_eq!(raw[2], later);
        for (effective, saved) in resolved[2].positions.iter().zip(&later.positions) {
            assert_eq!(effective.after_qty, saved.after_qty);
            assert_eq!(effective.reference_price, saved.reference_price);
        }
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(raw))]),
        );
        let corrected = build(&config, histories);
        assert_eq!(*query(&original, 10, 39), *query(&corrected, 10, 39));
        let qty = |ts| {
            corrected
                .allocation_audit("alignment", ts)
                .unwrap()
                .strategies
                .into_iter()
                .find(|r| r.0 == "alpha")
                .unwrap()
                .3
        };
        assert_eq!(qty(40), 0.5);
        assert_eq!(qty(HOUR), 0.0);
        assert_eq!(qty(HOUR + 1), 0.25);
        let before = query(&original, 10, 4 * HOUR);
        let after = query(&corrected, 10, 4 * HOUR);
        assert_eq!(before.summary, after.summary);
        assert_eq!(before.symbols, after.symbols);
        assert_eq!(before.points, after.points);
        assert!(
            original
                .prepare_historical_allocations(
                    "alignment",
                    &[observation(HOUR, "alpha", 0.5)],
                    |_, _, _| Ok(115.0),
                )
                .is_err()
        );
        // beta is absent from the immutable later correction: its fixed targets
        // cannot absorb this earlier transfer, so no candidate may be applied.
        assert!(
            original
                .prepare_historical_allocations(
                    "alignment",
                    &[observation(40, "beta", 1.0)],
                    |_, _, _| Ok(115.0),
                )
                .is_err()
        );
    }

    fn strategy_anchor() -> SourceStrategyPositionSnapshots {
        BTreeMap::from([(
            "alignment".into(),
            StrategyPositionSnapshot {
                source_id: "alignment".into(),
                snapshot_ts_us: 10,
                positions: vec![crate::snapshot::StrategySnapshotPosition {
                    strategy_name: "alpha".into(),
                    symbol: "BTCUSDT".into(),
                    venue_code: 1,
                    quantity: 2.0,
                    reference_price: 100.0,
                }],
            },
        )])
    }

    #[test]
    fn historical_reconstruction_restates_preconditions_without_editing_later_record() {
        let (config, mut histories, mut correction) = fixture();
        correction.positions[1].strategy = UNALLOCATED_STRATEGY.into();
        let original_record = correction.clone();
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![correction.clone()]))]),
        );
        let make = |histories| {
            NavTimelineCache::build(
                config.clone(),
                Arc::default(),
                Arc::new(strategy_anchor()),
                Arc::new(histories),
            )
        };
        let original = make(histories.clone()).unwrap();
        let batch = original
            .prepare_historical_allocations(
                "alignment",
                &[OwnershipObservation {
                    snapshot_ts_us: 30,
                    positions: vec![AllocationQuantity {
                        strategy: "alpha".into(),
                        symbol: "BTCUSDT".into(),
                        venue_code: 1,
                        quantity: 1.0,
                    }],
                }],
                |_, _, _| Ok(115.0),
            )
            .unwrap();
        assert_eq!(correction, original_record);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].restatements.len(), 1);
        let raw = vec![batch[0].clone(), correction.clone()];
        let resolved = resolve(&raw).unwrap();
        assert_eq!(resolved[1].positions[0].before_qty, 1.0);
        assert_eq!(
            resolved[1].positions[0].after_qty,
            correction.positions[0].after_qty
        );
        assert_eq!(
            resolved[1].positions[0].reference_price,
            correction.positions[0].reference_price
        );
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(raw.clone()))]),
        );
        let corrected = make(histories.clone()).unwrap();
        let request = |start, end| NavTimelineRequest {
            start_ts_us: Some(start),
            end_ts_us: end,
            selected_source_ids: vec!["alignment".into()],
            selected_symbols: vec![],
            max_points: 1000,
            tick_interval_us: None,
        };
        assert_eq!(
            original.rebuild(request(10, 29), true).unwrap(),
            corrected.rebuild(request(10, 29), true).unwrap()
        );
        for mode in [false, true] {
            for (start, end) in [(10, 4 * HOUR), (30, HOUR - 1), (HOUR + 1, 4 * HOUR)] {
                let before = original.rebuild(request(start, end), mode).unwrap();
                let after = corrected.rebuild(request(start, end), mode).unwrap();
                assert_eq!(before.summary, after.summary);
                assert_eq!(before.symbols, after.symbols);
                assert_eq!(before.points, after.points);
                let sum: f64 = after
                    .strategy_points
                    .iter()
                    .map(|s| s.summary.nav_change_after_fee_quote)
                    .sum();
                assert!((sum - after.summary.nav_change_after_fee_quote).abs() < 1e-9);
            }
        }
        let priced_before = original
            .rebuild_from_minute_reader(request(10, 4 * HOUR), true, |_, _, _| Ok(Some(125.0)))
            .unwrap();
        let priced_after = corrected
            .rebuild_from_minute_reader(request(10, 4 * HOUR), true, |_, _, _| Ok(Some(125.0)))
            .unwrap();
        assert_eq!(priced_before.summary, priced_after.summary);
        assert_eq!(priced_before.points, priced_after.points);
        assert_eq!(priced_after.valuation, minute_marks::VALUATION);
        assert!(
            corrected
                .rebuild_from_minute_reader(request(10, 4 * HOUR), true, |_, _, _| Ok(None))
                .is_err()
        );
        let at = corrected.allocation_audit("alignment", 30).unwrap();
        assert_eq!(
            at.strategies.iter().find(|r| r.0 == "alpha").unwrap().3,
            1.0
        );
        assert_eq!(
            corrected
                .allocation_audit("alignment", HOUR)
                .unwrap()
                .strategies,
            original
                .allocation_audit("alignment", HOUR)
                .unwrap()
                .strategies
        );
        // The normalized records also drive funding ownership, including the
        // exclusive cutoff at the later correction's timestamp.
        let rates = BTreeMap::from([(
            "BTCUSDT".into(),
            Arc::new(vec![crate::funding_rates::FundingRate {
                symbol: "BTCUSDT".into(),
                ts_us: HOUR,
                rate: 0.001,
                mark_price: 120.0,
                rate_type: "regular".into(),
            }]),
        )]);
        let report = corrected.rebuild(request(10, 2 * HOUR), true).unwrap();
        let funding = corrected
            .estimate_funding_records(&report, true, &rates)
            .unwrap()
            .records;
        assert!((funding[0].strategies["alpha"] + 0.12).abs() < 1e-12);
        assert!((funding[0].strategies[UNALLOCATED_STRATEGY] - 0.12).abs() < 1e-12);
        assert!(funding[0].quantity.abs() < 1e-12);
        // Neither missing historical transfers nor changed raw later records
        // can silently bypass the original audit protection.
        assert!(resolve(&[batch[0].clone()]).is_err());
        let mut changed = raw.clone();
        changed[1].positions[0].after_qty = 0.5;
        changed[1].positions[1].after_qty = -0.5;
        assert!(resolve(&changed).is_err());
        let mut late = (**histories["alignment"].events.front().unwrap()).clone();
        late.record_key = "late".into();
        late.update_ts_us = 15;
        late.amount_update = 0.5;
        histories
            .get_mut("alignment")
            .unwrap()
            .events
            .push_back(Arc::new(late));
        assert!(make(histories).is_err());
    }

    #[test]
    fn restatement_rejects_partial_duplicate_cross_source_and_unbalanced_preconditions() {
        let (_, _, mut target) = fixture();
        let mut earlier = target.clone();
        earlier.snapshot_ts_us = 30;
        earlier.restatements = vec![AllocationRestatement {
            snapshot_ts_us: target.snapshot_ts_us,
            original_positions_sha256: positions_fingerprint(&target).unwrap(),
            before: target
                .positions
                .iter()
                .map(|p| AllocationQuantity {
                    strategy: p.strategy.clone(),
                    symbol: p.symbol.clone(),
                    venue_code: p.venue_code,
                    quantity: p.before_qty,
                })
                .collect(),
        }];
        assert!(resolve(&[earlier.clone(), target.clone()]).is_ok());
        let mut bad = earlier.clone();
        bad.restatements[0].before.pop();
        assert!(resolve(&[bad, target.clone()]).is_err());
        let mut bad = earlier.clone();
        bad.restatements[0].before[0].quantity += 1.0;
        assert!(resolve(&[bad, target.clone()]).is_err());
        let mut duplicate = earlier.clone();
        duplicate.snapshot_ts_us = 40;
        assert!(resolve(&[earlier.clone(), duplicate, target.clone()]).is_err());
        let mut scoped = earlier.clone();
        scoped.positions[1].strategy = UNALLOCATED_STRATEGY.into();
        assert!(resolve(&[scoped, target.clone()]).is_err());
        target.source_id = "other".into();
        assert!(resolve(&[earlier, target]).is_err());
    }

    #[test]
    fn observed_settlement_can_flatten_unidentified_inventory_without_relabeling_fill() {
        let (config, mut histories, _) = fixture();
        Arc::make_mut(&mut histories.get_mut("alignment").unwrap().events[1]).from_key_text =
            "exchange_forced_close:settlement:order=1:trade=2".into();
        let make = |histories| {
            NavTimelineCache::build(
                config.clone(),
                Arc::default(),
                Arc::new(strategy_anchor()),
                Arc::new(histories),
            )
            .unwrap()
        };
        let original = make(histories.clone());
        let batch = original
            .prepare_historical_allocations(
                "alignment",
                &[OwnershipObservation {
                    snapshot_ts_us: 30,
                    positions: ["alpha", UNATTRIBUTED_STRATEGY]
                        .into_iter()
                        .map(|strategy| AllocationQuantity {
                            strategy: strategy.into(),
                            symbol: "BTCUSDT".into(),
                            venue_code: 1,
                            quantity: 0.0,
                        })
                        .collect(),
                }],
                |_, _, _| Ok(120.0),
            )
            .unwrap();
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(batch))]),
        );
        let corrected = make(histories.clone());
        let audit = corrected.allocation_audit("alignment", 30).unwrap();
        assert!(audit.account.iter().all(|r| r.2 == 0.0));
        assert!(audit.strategies.iter().all(|r| r.3 == 0.0));
        let request = NavTimelineRequest {
            start_ts_us: Some(10),
            end_ts_us: HOUR,
            selected_source_ids: vec!["alignment".into()],
            selected_symbols: vec![],
            max_points: 1000,
            tick_interval_us: None,
        };
        let before = original.rebuild(request.clone(), true).unwrap();
        let after = corrected.rebuild(request, true).unwrap();
        assert_eq!(before.summary, after.summary);
        assert_eq!(before.points, after.points);
        assert_eq!(
            after
                .strategy_points
                .iter()
                .find(|s| s.strategy == UNATTRIBUTED_STRATEGY)
                .unwrap()
                .summary
                .fill_count,
            1
        );
        assert_eq!(
            after
                .strategy_points
                .iter()
                .find(|s| s.strategy == "alpha")
                .unwrap()
                .summary
                .fill_count,
            0
        );
        assert!(
            histories["alignment"].events[1]
                .from_key_text
                .starts_with("exchange_forced_close:")
        );
    }

    #[test]
    fn anchored_residual_transfer_preserves_account_and_earlier_strategy_pnl() {
        let (config, mut histories, mut correction) = fixture();
        correction.positions[1].strategy = UNALLOCATED_STRATEGY.into();
        let anchors = Arc::new(strategy_anchor());
        let make = |histories| {
            NavTimelineCache::build(
                config.clone(),
                Arc::default(),
                anchors.clone(),
                Arc::new(histories),
            )
            .unwrap()
        };
        let original = make(histories.clone());
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![correction]))]),
        );
        let corrected = make(histories.clone());
        let query = |cache: &NavTimelineCache, start, end, mode| {
            cache
                .rebuild(
                    NavTimelineRequest {
                        start_ts_us: Some(start),
                        end_ts_us: end,
                        selected_source_ids: vec!["alignment".into()],
                        selected_symbols: vec![],
                        max_points: 1000,
                        tick_interval_us: None,
                    },
                    mode,
                )
                .unwrap()
        };
        assert_eq!(
            *query(&original, 10, HOUR - 1, true),
            *query(&corrected, 10, HOUR - 1, true)
        );
        // The correction cannot invalidate or change account-only queries.
        assert_eq!(
            *query(&original, 10, 4 * HOUR, false),
            *query(&corrected, 10, 4 * HOUR, false)
        );
        for (start, end) in [(10, 4 * HOUR), (HOUR, HOUR), (HOUR + 1, 4 * HOUR)] {
            let before = query(&original, start, end, true);
            let after = query(&corrected, start, end, true);
            assert_eq!(before.summary, after.summary);
            assert_eq!(before.symbols, after.symbols);
            assert_eq!(before.points, after.points);
            let sum: f64 = after
                .strategy_points
                .iter()
                .map(|s| s.summary.nav_change_after_fee_quote)
                .sum();
            assert!((sum - after.summary.nav_change_after_fee_quote).abs() < 1e-10);
        }
        let audit = corrected.allocation_audit("alignment", HOUR).unwrap();
        assert!(audit.strategies.iter().all(|(_, _, _, q)| q.abs() < 1e-12));
        let report = query(&corrected, HOUR + 1, 4 * HOUR, true);
        assert!(
            !report
                .available_strategies
                .contains(&UNALLOCATED_STRATEGY.into())
        );
        // A late historical fill must still invalidate the audited before leg.
        let history = histories.get_mut("alignment").unwrap();
        let mut late = (**history.events.front().unwrap()).clone();
        late.record_key = "late".into();
        late.update_ts_us = 15;
        late.amount_update = 0.5;
        history.events.push_back(Arc::new(late));
        assert!(
            NavTimelineCache::build(config, Arc::default(), anchors, Arc::new(histories)).is_err()
        );
    }

    #[test]
    fn residual_transfer_before_first_post_anchor_fill_activates_opening_lots() {
        let (config, mut histories, mut correction) = fixture();
        let history = histories.get_mut("alignment").unwrap();
        history.events = history
            .events
            .iter()
            .filter(|e| e.update_ts_us >= 2 * HOUR)
            .cloned()
            .collect();
        correction.positions[0].after_qty = 1.0;
        correction.positions[1].strategy = UNALLOCATED_STRATEGY.into();
        correction.positions[1].before_qty = 0.0;
        correction.positions[1].after_qty = 1.0;
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![correction]))]),
        );
        let cache = NavTimelineCache::build(
            config,
            Arc::default(),
            Arc::new(strategy_anchor()),
            Arc::new(histories),
        )
        .unwrap();
        let audit = cache.allocation_audit("alignment", HOUR).unwrap();
        assert_eq!(audit.account, vec![("BTCUSDT".into(), 1, 2.0)]);
        assert_eq!(
            audit
                .strategies
                .iter()
                .find(|(s, _, _, _)| s == "alpha")
                .unwrap()
                .3,
            1.0
        );
        assert_eq!(
            audit
                .strategies
                .iter()
                .find(|(s, _, _, _)| s == UNALLOCATED_STRATEGY)
                .unwrap()
                .3,
            1.0
        );
    }

    #[test]
    fn anchored_correction_requires_an_audited_residual_per_market() {
        let (config, mut histories, correction) = fixture();
        attach(
            &mut histories,
            &BTreeMap::from([("alignment".into(), Arc::new(vec![correction]))]),
        );
        assert!(
            NavTimelineCache::build(
                config,
                Arc::default(),
                Arc::new(strategy_anchor()),
                Arc::new(histories),
            )
            .is_err()
        );
    }
}
