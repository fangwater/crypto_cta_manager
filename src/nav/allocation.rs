//! Audited, timestamped ownership corrections. These are not exchange fills and
//! never change account FIFO, execution counts, volume, fees or historical rows.
use super::*;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AllocationSnapshot {
    pub source_id: String,
    pub snapshot_ts_us: i64,
    pub positions: Vec<AllocationPosition>,
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
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.source_id.trim().is_empty() && self.snapshot_ts_us > 0,
            "allocation source and timestamp are required"
        );
        anyhow::ensure!(!self.positions.is_empty(), "allocation snapshot is empty");
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
    let rows = sqlx::query("SELECT source_id,snapshot_ts_us,positions FROM cta_strategy_allocation_snapshots ORDER BY source_id,snapshot_ts_us")
        .fetch_all(pool).await.context("load strategy allocation snapshots; apply explicit allocation schema maintenance first")?;
    let mut result = BTreeMap::<String, Vec<AllocationSnapshot>>::new();
    for row in rows {
        let snapshot = AllocationSnapshot {
            source_id: row.try_get("source_id")?,
            snapshot_ts_us: row.try_get("snapshot_ts_us")?,
            positions: serde_json::from_value(row.try_get("positions")?)?,
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
    sqlx::query("INSERT INTO cta_strategy_allocation_snapshots (source_id,snapshot_ts_us,positions,note) VALUES ($1,$2,$3,$4)")
        .bind(&snapshot.source_id).bind(snapshot.snapshot_ts_us)
        .bind(serde_json::to_value(&snapshot.positions)?).bind(note)
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
            // Verify the complete old projection before mutating any bucket.
            for p in &snapshot.positions {
                let key = (p.strategy.clone(), p.symbol.clone(), p.venue_code);
                let before = self.strategy_states.get(&key).map_or(0.0, |s| {
                    s.fifo.long_quantity(1.0) - s.fifo.short_quantity(1.0)
                });
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
            self.next_allocation += 1;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct AllocationAudit {
    pub source_id: String,
    pub snapshot_ts_us: i64,
    pub account: Vec<(String, i16, f64)>,
    pub strategies: Vec<(String, String, i16, f64)>,
}

impl NavTimelineCache {
    /// Uses the exact recovered/deduplicated NAV projection, including RapidX.
    pub fn allocation_audit(&self, source_id: &str, ts_us: i64) -> Result<AllocationAudit> {
        anyhow::ensure!(
            !self.strategy_snapshots.contains_key(source_id),
            "allocation repair currently requires a source without a strategy initial snapshot"
        );
        let source = self
            .sources
            .get(&(source_id.to_owned(), false))
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
            strategies: state
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
                .collect(),
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
    fn query(cache: &NavTimelineCache, start: i64, end: i64) -> Arc<NavTimelineReport> {
        cache
            .rebuild(
                NavTimelineRequest {
                    start_ts_us: Some(start),
                    end_ts_us: end,
                    selected_source_ids: vec!["alignment".into()],
                    selected_symbols: vec![],
                    max_points: 1000,
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
}
