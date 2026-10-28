//! Reuse the same validated fills, snapshot choice and strategy ownership as NAV.
use super::*;
use crate::funding_rates::{
    FundingEstimateRecord, FundingEstimateRecords, FundingRate, supported_symbol,
};

impl NavTimelineCache {
    /// Fingerprint funding quantity inputs at daily cutoffs in one ordered pass.
    /// Late fills and audited ownership corrections invalidate affected daily fees.
    pub fn funding_daily_fingerprints(
        &self,
        source_id: &str,
        cutoffs: &[i64],
    ) -> Result<BTreeMap<i64, String>> {
        use sha2::{Digest, Sha256};
        let source = self
            .sources
            .get(&(source_id.to_owned(), true))
            .context("funding source unavailable")?;
        let mut hash = Sha256::new();
        hash.update(serde_json::to_vec(&source.prepared.snapshot_ts_us)?);
        for (key, state) in &source.prepared.initial_states {
            hash.update(serde_json::to_vec(&(
                key,
                state.fifo.long_quantity(1.0),
                state.fifo.short_quantity(1.0),
            ))?);
        }
        for (key, state) in &source.prepared.initial_strategy_states {
            hash.update(serde_json::to_vec(&(
                key,
                state.fifo.long_quantity(1.0),
                state.fifo.short_quantity(1.0),
            ))?);
        }
        let mut index = 0;
        let fills = &source.prepared.fill_events;
        let mut result = BTreeMap::new();
        for &end in cutoffs {
            while index < fills.len() && fills[index].fill_ts_us <= end {
                let fill = &fills[index];
                let e = &fill.event;
                // Millions of fills share the dashboard CPU: hash unambiguous
                // length-prefixed bytes without allocating JSON per fill.
                for text in [&e.record_key, &e.symbol, &e.from_key_text] {
                    hash.update((text.len() as u64).to_be_bytes());
                    hash.update(text.as_bytes());
                }
                hash.update(fill.fill_ts_us.to_be_bytes());
                hash.update(e.venue_code.to_be_bytes());
                hash.update(e.side_code.to_be_bytes());
                hash.update(e.order_type_code.to_be_bytes());
                hash.update(e.amount_update.to_bits().to_be_bytes());
                index += 1;
            }
            let mut daily = hash.clone();
            for allocation in source
                .prepared
                .allocations
                .iter()
                .take_while(|a| a.snapshot_ts_us <= end)
            {
                daily.update(serde_json::to_vec(allocation)?);
            }
            result.insert(end, hex::encode(daily.finalize()));
        }
        Ok(result)
    }

    /// Funding needs signed quantities, not a second priced NAV replay. Scope it
    /// to exactly the same immutable anchor used by the cumulative dashboard.
    pub fn cumulative_funding_scope(&self, source_id: &str, end: i64) -> Option<NavTimelineReport> {
        let source = self.sources.get(&(source_id.to_owned(), true))?;
        let start = self.start_ts_us(source_id, end, true)?;
        let selected_symbols = source
            .report
            .symbols
            .iter()
            .map(|s| s.symbol.clone())
            .collect::<Vec<_>>();
        Some(NavTimelineReport {
            valuation: "quantity_fifo_window_delta",
            earliest_start_ts_us: start,
            start_ts_us: start,
            end_ts_us: end,
            selected_source_ids: vec![source_id.to_owned()],
            available_symbols: selected_symbols.clone(),
            selected_symbols,
            available_strategies: Vec::new(),
            summary: source.report.totals,
            symbols: Vec::new(),
            points: Vec::new(),
            symbol_points: Vec::new(),
            strategy_points: Vec::new(),
            sampled: false,
        })
    }

    /// Read-only settlement diagnostics, using the exact validated NAV fill stream.
    pub fn funding_boundary_fills(
        &self,
        source_id: &str,
        allocation: bool,
        symbol: &str,
        ts_us: i64,
    ) -> Vec<serde_json::Value> {
        let Some(source) = self.sources.get(&(source_id.to_owned(), allocation)) else {
            return Vec::new();
        };
        let fills = &source.prepared.fill_events;
        let start = fill_partition_point(fills, |f| f.fill_ts_us < ts_us - 300_000_000);
        let end = fill_partition_point(fills, |f| f.fill_ts_us <= ts_us + 300_000_000);
        (start..end)
            .filter_map(|i| {
                let fill = &fills[i];
                let e = &fill.event;
                (e.venue_code == 1 && e.symbol == symbol).then(|| serde_json::json!({
                    "fill_ts_us": fill.fill_ts_us,
                    "receive_ts_us": e.event_ts_us,
                    "signed_qty": if fill.side == Side::Buy { e.amount_update } else { -e.amount_update },
                    "price": e.price,
                    "record_key": e.record_key,
                    "client_order_id": e.client_order_id,
                    "strategy": strategy_from_from_key(&e.from_key_text),
                    "internal_cross": e.order_type_code == INTERNAL_CROSS_ORDER_TYPE_CODE,
                }))
            })
            .collect()
    }

    pub fn funding_symbols(&self, report: &NavTimelineReport, allocation: bool) -> Vec<String> {
        let mut symbols = BTreeSet::new();
        for source_id in &report.selected_source_ids {
            let Some(source) = self.sources.get(&(source_id.clone(), allocation)) else {
                continue;
            };
            for ((symbol, venue), _) in &source.prepared.initial_states {
                if *venue == 1
                    && supported_symbol(symbol)
                    && report.selected_symbols.contains(symbol)
                {
                    symbols.insert(symbol.clone());
                }
            }
            // Offset strategy inventories can exist even when account net quantity is zero.
            for ((_, symbol, venue), _) in &source.prepared.initial_strategy_states {
                if *venue == 1
                    && supported_symbol(symbol)
                    && report.selected_symbols.contains(symbol)
                {
                    symbols.insert(symbol.clone());
                }
            }
            for fill in &source.prepared.fill_events {
                if fill.fill_ts_us > report.end_ts_us {
                    break;
                }
                if fill.event.venue_code == 1
                    && supported_symbol(&fill.event.symbol)
                    && report.selected_symbols.contains(&fill.event.symbol)
                {
                    symbols.insert(fill.event.symbol.clone());
                }
            }
        }
        symbols.into_iter().collect()
    }

    pub fn estimate_funding_records(
        &self,
        report: &NavTimelineReport,
        allocation: bool,
        rates: &BTreeMap<String, Arc<Vec<FundingRate>>>,
    ) -> Result<FundingEstimateRecords> {
        let mut settlements: Vec<_> = rates
            .values()
            .flat_map(|rows| rows.iter())
            .map(|r| (r.estimate_ts_us(), r))
            .filter(|(ts, r)| {
                *ts >= report.start_ts_us
                    && *ts <= report.end_ts_us
                    && report.selected_symbols.contains(&r.symbol)
            })
            .collect();
        settlements.sort_by(|a, b| {
            (a.0, &a.1.symbol, &a.1.rate_type).cmp(&(b.0, &b.1.symbol, &b.1.rate_type))
        });
        let mut measured = FundingEstimateRecords::default();
        for source_id in &report.selected_source_ids {
            let source = self
                .sources
                .get(&(source_id.clone(), allocation))
                .context("funding NAV source unavailable")?;
            let config = self
                .config
                .sources
                .iter()
                .find(|s| &s.id == source_id)
                .context("funding source configuration unavailable")?;
            if config.venue != "binance-futures" {
                continue;
            }
            // A snapshot is an after-events anchor. Skip only settlements whose
            // pre-settlement holdings cannot be reconstructed, then initialize
            // at the first measurable settlement so opening lots are preserved.
            let measurable: Vec<_> = settlements
                .iter()
                .copied()
                .filter(|(ts, _)| {
                    source
                        .prepared
                        .snapshot_ts_us
                        .is_none_or(|anchor| anchor < *ts)
                })
                .collect();
            measured.skipped_settlements += settlements.len() - measurable.len();
            for (_, rate) in settlements.iter().filter(|(ts, _)| {
                source
                    .prepared
                    .snapshot_ts_us
                    .is_some_and(|anchor| anchor >= *ts)
            }) {
                *measured
                    .skipped_by_symbol
                    .entry(rate.symbol.clone())
                    .or_default() += 1;
            }
            let Some(first) = measurable.first() else {
                continue;
            };
            let checkpoint = source
                .checkpoints
                .iter()
                .rev()
                .find(|cp| cp.ts_us <= first.0);
            let (mut state, mut index) = match checkpoint {
                Some(cp) => (cp.state.clone(), cp.next_fill),
                None => (initial_timeline_state(config, &source.prepared)?, 0),
            };
            state.activate_snapshot_at(first.0);
            // Quantities alone are sufficient for funding; seed them from NAV's
            // immutable checkpoint, then apply the remaining validated fills once.
            let mut quantities: BTreeMap<String, f64> = state
                .states
                .iter()
                .filter(|((_, venue), _)| *venue == 1)
                .map(|((symbol, _), s)| {
                    (
                        symbol.clone(),
                        s.fifo.long_quantity(1.0) - s.fifo.short_quantity(1.0),
                    )
                })
                .collect();
            let mut strategies: BTreeMap<(String, String), f64> = state
                .strategy_states
                .iter()
                .filter(|((_, _, venue), _)| *venue == 1)
                .map(|((strategy, symbol, _), s)| {
                    (
                        (strategy.clone(), symbol.clone()),
                        s.fifo.long_quantity(1.0) - s.fifo.short_quantity(1.0),
                    )
                })
                .collect();
            let fills = &source.prepared.fill_events;
            let mut allocation_index = state.next_allocation;
            let allocations = &source.prepared.allocations;
            for &(settlement_ts_us, rate) in &measurable {
                // The operator-selected estimate uses the scheduled hour.
                // At/after-boundary fills never move the estimated cutoff.
                while index < fills.len() && fills[index].fill_ts_us < settlement_ts_us {
                    let fill = &fills[index];
                    apply_funding_allocations(
                        allocations,
                        &mut allocation_index,
                        fill.fill_ts_us,
                        &mut strategies,
                    );
                    if fill.event.venue_code == 1 {
                        let signed = match fill.side {
                            Side::Buy => fill.event.amount_update,
                            Side::Sell => -fill.event.amount_update,
                        };
                        *quantities.entry(fill.event.symbol.clone()).or_default() += signed;
                        let strategy = fill.strategy();
                        if !(state.strategy_allocation_active
                            && is_system_position_close(&strategy))
                        {
                            *strategies
                                .entry((strategy, fill.event.symbol.clone()))
                                .or_default() += signed;
                        }
                    }
                    index += 1;
                }
                apply_funding_allocations(
                    allocations,
                    &mut allocation_index,
                    settlement_ts_us,
                    &mut strategies,
                );
                let quantity = quantities.get(&rate.symbol).copied().unwrap_or(0.0);
                let factor = -rate.mark_price * rate.rate;
                let mut owners = BTreeMap::new();
                let mut allocated = 0.0;
                for ((strategy, symbol), qty) in &strategies {
                    if symbol == &rate.symbol {
                        allocated += qty;
                        *owners.entry(strategy.clone()).or_default() += qty * factor;
                    }
                }
                if state.strategy_allocation_active {
                    *owners.entry(UNALLOCATED_STRATEGY.to_string()).or_default() +=
                        (quantity - allocated) * factor;
                }
                let near_start =
                    fill_partition_point(fills, |f| f.fill_ts_us < settlement_ts_us - 15_000_000);
                let near_end =
                    fill_partition_point(fills, |f| f.fill_ts_us <= settlement_ts_us + 15_000_000);
                let boundary_fill_count = (near_start..near_end)
                    .filter(|i| {
                        let event = &fills[*i].event;
                        event.venue_code == 1
                            && event.symbol == rate.symbol
                            && event.order_type_code != INTERNAL_CROSS_ORDER_TYPE_CODE
                    })
                    .count();
                let income = quantity * factor;
                if !income.is_finite() || !owners.values().all(|v: &f64| v.is_finite()) {
                    measured.skipped_settlements += 1;
                    *measured
                        .skipped_by_symbol
                        .entry(rate.symbol.clone())
                        .or_default() += 1;
                    continue;
                }
                measured.records.push(FundingEstimateRecord {
                    source_id: source_id.clone(),
                    symbol: rate.symbol.clone(),
                    asset: rate.symbol[rate.symbol.len() - 4..].to_string(),
                    ts_us: settlement_ts_us,
                    rate_type: rate.rate_type.clone(),
                    rate: rate.rate,
                    mark_price: rate.mark_price,
                    quantity,
                    income_quote: income,
                    strategies: owners,
                    boundary_fill_count,
                });
            }
        }
        measured.records.sort_by(|a, b| {
            (a.ts_us, &a.source_id, &a.symbol, &a.rate_type).cmp(&(
                b.ts_us,
                &b.source_id,
                &b.symbol,
                &b.rate_type,
            ))
        });
        Ok(measured)
    }
}

fn apply_funding_allocations(
    snapshots: &[allocation::AllocationSnapshot],
    index: &mut usize,
    before_us: i64,
    strategies: &mut BTreeMap<(String, String), f64>,
) {
    while let Some(snapshot) = snapshots.get(*index) {
        if snapshot.snapshot_ts_us >= before_us {
            break;
        }
        for p in &snapshot.positions {
            if p.venue_code == 1 && p.strategy != UNALLOCATED_STRATEGY {
                strategies.insert((p.strategy.clone(), p.symbol.clone()), p.after_qty);
            }
        }
        *index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::StrategySnapshotPosition;

    const OFFSET: i64 = 3_400_000_000;

    #[test]
    fn residual_correction_is_derived_once_and_applies_after_equal_time_fills() {
        let positions = [("alpha", 2.0, 0.0), (UNALLOCATED_STRATEGY, -2.0, 0.0)]
            .into_iter()
            .map(
                |(strategy, before_qty, after_qty)| allocation::AllocationPosition {
                    strategy: strategy.into(),
                    symbol: "BTCUSDT".into(),
                    venue_code: 1,
                    before_qty,
                    after_qty,
                    reference_price: 100.0,
                },
            )
            .collect();
        let corrections = vec![allocation::AllocationSnapshot {
            source_id: "alignment".into(),
            snapshot_ts_us: 100,
            positions,
            restatements: Vec::new(),
        }];
        let mut owners = BTreeMap::from([(("alpha".into(), "BTCUSDT".into()), 2.0)]);
        let mut index = 0;
        apply_funding_allocations(&corrections, &mut index, 100, &mut owners);
        assert_eq!(owners[&("alpha".into(), "BTCUSDT".into())], 2.0);
        apply_funding_allocations(&corrections, &mut index, 101, &mut owners);
        assert_eq!(owners[&("alpha".into(), "BTCUSDT".into())], 0.0);
        assert!(!owners.keys().any(|(s, _)| s == UNALLOCATED_STRATEGY));
        owners.insert(("alpha".into(), "BTCUSDT".into()), 1.0);
        apply_funding_allocations(&corrections, &mut index, 200, &mut owners);
        assert_eq!(owners[&("alpha".into(), "BTCUSDT".into())], 1.0);
    }

    fn fixture() -> (NavTimelineCache, NavTimelineReport) {
        let mut config: AppConfig =
            toml::from_str(include_str!("../../config/cta-manager.example.toml")).unwrap();
        config.sources.truncate(1);
        let source = &mut config.sources[0];
        source.id = "funding-fixture".into();
        source.enabled = true;
        source.maker_fee_rate = Some(0.0);
        source.taker_fee_rate = Some(0.0);
        let id = source.id.clone();
        let position = |strategy: &str, symbol: &str, venue, quantity| StrategySnapshotPosition {
            strategy_name: strategy.into(),
            symbol: symbol.into(),
            venue_code: venue,
            quantity,
            reference_price: 100.0,
        };
        let strategy = StrategyPositionSnapshot {
            source_id: id.clone(),
            snapshot_ts_us: OFFSET + 100_000_000,
            positions: vec![
                position("alpha", "BTCUSDT", 1, 3.0),
                position("beta", "BTCUSDT", 1, -1.0),
                position("alpha", "BTCUSDC", 1, -4.0),
                position("alpha", "BTC", 0, 100.0),
            ],
        };
        let snapshot = strategy.account_snapshot().unwrap();
        let event = |ts, side, qty, owner: &str, kind| {
            let ts = OFFSET + ts;
            Arc::new(NavEvent {
                record_key: format!("{ts}-{owner}"),
                event_ts_us: ts,
                create_ts_us: ts,
                update_ts_us: ts,
                signal_ts_us: ts,
                symbol: "BTCUSDT".into(),
                client_order_id: ts,
                venue_code: 1,
                venue: "BinanceFutures".into(),
                order_type_code: kind,
                side_code: side,
                price: 100.0,
                amount_update: qty,
                from_key_text: format!("batch_exec:{owner}"),
                fill_liquidity: None,
            })
        };
        let history = NavSourceHistory {
            events: vec![
                event(150_000_000, 1, 1.0, "alpha", 1),
                event(170_000_000, 1, 1.0, "alpha", INTERNAL_CROSS_ORDER_TYPE_CODE),
                event(170_000_000, 2, 1.0, "beta", INTERNAL_CROSS_ORDER_TYPE_CODE),
                event(180_000_000, 2, 1.0, "SYSTEM_POSITION_CLOSE", 1),
                event(200_000_000, 1, 10.0, "alpha", 1),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let cache = NavTimelineCache::build(
            config,
            Arc::new(BTreeMap::from([(id.clone(), snapshot)])),
            Arc::new(BTreeMap::from([(id.clone(), strategy)])),
            Arc::new(BTreeMap::from([(id.clone(), history)])),
        )
        .unwrap();
        let report = cache
            .rebuild(
                NavTimelineRequest {
                    start_ts_us: Some(OFFSET + 190_000_000),
                    end_ts_us: OFFSET + 210_000_000,
                    selected_source_ids: vec![id],
                    selected_symbols: Vec::new(),
                    max_points: 1000,
                    tick_interval_us: None,
                },
                true,
            )
            .unwrap();
        (cache, report.as_ref().clone())
    }

    #[test]
    fn estimates_signed_strategy_inventory_residual_and_usdc_without_spot_contamination() {
        let (cache, report) = fixture();
        let symbols = cache.funding_symbols(&report, true);
        assert_eq!(symbols, vec!["BTCUSDC", "BTCUSDT"]);
        let rate = |symbol: &str, rate, mark| FundingRate {
            symbol: symbol.into(),
            ts_us: OFFSET + 200_000_000,
            rate,
            mark_price: mark,
            rate_type: "Regular".into(),
        };
        let rates = BTreeMap::from([
            (
                "BTCUSDT".into(),
                Arc::new(vec![rate("BTCUSDT", 0.001, 100.0)]),
            ),
            (
                "BTCUSDC".into(),
                Arc::new(vec![rate("BTCUSDC", -0.002, 200.0)]),
            ),
        ]);
        let records = cache
            .estimate_funding_records(&report, true, &rates)
            .unwrap();
        let usdt = records
            .records
            .iter()
            .find(|r| r.symbol == "BTCUSDT")
            .unwrap();
        assert_eq!(usdt.quantity, 2.0); // Equal-timestamp buy is excluded.
        assert!((usdt.income_quote + 0.2).abs() < 1e-12);
        assert!((usdt.strategies["alpha"] + 0.5).abs() < 1e-12);
        assert!((usdt.strategies["beta"] - 0.2).abs() < 1e-12);
        assert!((usdt.strategies["__unallocated__"] - 0.1).abs() < 1e-12);
        assert_eq!(usdt.boundary_fill_count, 1);
        let usdc = records
            .records
            .iter()
            .find(|r| r.symbol == "BTCUSDC")
            .unwrap();
        assert_eq!(usdc.asset, "USDC");
        assert!((usdc.income_quote + 1.6).abs() < 1e-12);
        for record in &records.records {
            assert!((record.strategies.values().sum::<f64>() - record.income_quote).abs() < 1e-12);
        }
        let coverage = crate::funding_rates::RateCoverage {
            rates,
            ..Default::default()
        };
        let estimate = crate::funding_rates::estimate_timeline(&report, &coverage, &records);
        assert!((estimate.income_quote.unwrap() + 1.8).abs() < 1e-12);
        assert!(
            (estimate
                .strategies
                .iter()
                .map(|s| s.income_quote)
                .sum::<f64>()
                + 1.8)
                .abs()
                < 1e-12
        );
        let mut filtered = report.clone();
        filtered.selected_symbols = vec!["BTCUSDC".into()];
        let selected = cache
            .estimate_funding_records(&filtered, true, &coverage.rates)
            .unwrap();
        assert_eq!(selected.records.len(), 1);
    }

    #[test]
    fn shanghai_daily_fees_add_to_full_window_and_late_fills_invalidate_only_later_days() {
        use crate::funding_daily::{DAY_US, day_start};
        let (cache, mut report) = fixture();
        let first_day = day_start(report.start_ts_us);
        report.end_ts_us = first_day + 3 * DAY_US - 1;
        let cutoffs = (1..=3)
            .map(|i| first_day + i * DAY_US - 1)
            .collect::<Vec<_>>();
        let rates = BTreeMap::from([(
            "BTCUSDT".into(),
            Arc::new(
                [
                    OFFSET + 200_000_000,
                    first_day + DAY_US,
                    first_day + 2 * DAY_US,
                ]
                .into_iter()
                .map(|ts_us| FundingRate {
                    symbol: "BTCUSDT".into(),
                    ts_us,
                    rate: 0.001,
                    mark_price: 100.0,
                    rate_type: "Special".into(),
                })
                .collect(),
            ),
        )]);
        let full = cache
            .estimate_funding_records(&report, true, &rates)
            .unwrap();
        let mut daily = Vec::new();
        for (i, end) in cutoffs.iter().enumerate() {
            let mut day = report.clone();
            day.start_ts_us = report.start_ts_us.max(first_day + i as i64 * DAY_US);
            day.end_ts_us = *end;
            daily.extend(
                cache
                    .estimate_funding_records(&day, true, &rates)
                    .unwrap()
                    .records,
            );
        }
        assert_eq!(full.records.len(), 3);
        assert_eq!(daily.len(), full.records.len());
        let amount =
            |rows: &[FundingEstimateRecord]| rows.iter().map(|r| r.income_quote).sum::<f64>();
        assert!((amount(&daily) - amount(&full.records)).abs() < 1e-12);
        assert!((amount(&daily) + 2.6).abs() < 1e-12);

        let id = &report.selected_source_ids[0];
        let original = cache.funding_daily_fingerprints(id, &cutoffs).unwrap();
        let mut histories = cache.histories.as_ref().clone();
        let history = histories.get_mut(id).unwrap();
        let mut late = history.events[0].as_ref().clone();
        late.record_key = "late-second-day".into();
        late.event_ts_us = first_day + DAY_US + 1;
        late.update_ts_us = late.event_ts_us;
        late.create_ts_us = late.event_ts_us;
        late.client_order_id = late.event_ts_us;
        history.events.push_back(Arc::new(late));
        let amended = NavTimelineCache::build(
            cache.config.clone(),
            cache.snapshots.clone(),
            cache.strategy_snapshots.clone(),
            Arc::new(histories),
        )
        .unwrap();
        let changed = amended.funding_daily_fingerprints(id, &cutoffs).unwrap();
        assert_eq!(original[&cutoffs[0]], changed[&cutoffs[0]]);
        assert_ne!(original[&cutoffs[1]], changed[&cutoffs[1]]);
        assert_ne!(original[&cutoffs[2]], changed[&cutoffs[2]]);
    }

    #[test]
    fn partial_funding_keeps_known_income_and_never_replaces_absent_coverage_with_zero() {
        let (cache, report) = fixture();
        let coverage = crate::funding_rates::RateCoverage {
            rates: BTreeMap::from([(
                "BTCUSDT".into(),
                Arc::new(vec![FundingRate {
                    symbol: "BTCUSDT".into(),
                    ts_us: OFFSET + 200_000_000,
                    rate: 0.001,
                    mark_price: 100.0,
                    rate_type: "Regular".into(),
                }]),
            )]),
            unavailable_reason: Some("仅部分时段及币种有公开费率".into()),
            ..Default::default()
        };
        let records = cache
            .estimate_funding_records(&report, true, &coverage.rates)
            .unwrap();
        let estimate = crate::funding_rates::estimate_timeline(&report, &coverage, &records);
        assert!((estimate.income_quote.unwrap() + 0.2).abs() < 1e-12);
        assert_eq!(estimate.settlement_count, 1);
        assert!(estimate.unavailable_reason.is_some());
        assert_eq!(estimate.points.first().unwrap().funding_income_quote, 0.0);
        assert_eq!(
            estimate.points.last().unwrap().funding_income_quote,
            estimate.income_quote.unwrap()
        );
        assert!(
            (estimate
                .strategies
                .iter()
                .map(|s| s.income_quote)
                .sum::<f64>()
                + 0.2)
                .abs()
                < 1e-12
        );
        let absent = crate::funding_rates::RateCoverage {
            rates: BTreeMap::new(),
            ..coverage
        };
        let estimate =
            crate::funding_rates::estimate_timeline(&report, &absent, &Default::default());
        assert_eq!(estimate.income_quote, None);
        assert!(estimate.points.is_empty());
        assert!(estimate.strategies.is_empty());
    }

    #[test]
    fn unmeasurable_settlements_are_excluded_without_losing_known_income() {
        let (cache, mut report) = fixture();
        let anchor = OFFSET + 100_000_000;
        report.start_ts_us = anchor - 1;
        let rate = |ts_us, rate| FundingRate {
            symbol: "BTCUSDT".into(),
            ts_us,
            rate,
            mark_price: 100.0,
            rate_type: "Special".into(),
        };
        let coverage = crate::funding_rates::RateCoverage {
            rates: BTreeMap::from([(
                "BTCUSDT".into(),
                Arc::new(vec![
                    rate(anchor - 1, 0.001),
                    rate(anchor, 0.001),
                    rate(OFFSET + 200_000_000, 0.001),
                    rate(OFFSET + 201_000_000, f64::MAX),
                ]),
            )]),
            ..Default::default()
        };
        for allocation in [false, true] {
            let measured = cache
                .estimate_funding_records(&report, allocation, &coverage.rates)
                .unwrap();
            assert_eq!(measured.skipped_settlements, 3);
            assert_eq!(measured.records.len(), 1);
            assert_eq!(measured.records[0].quantity, 2.0);
            let estimate = crate::funding_rates::estimate_timeline(&report, &coverage, &measured);
            assert_eq!(estimate.settlement_count, 1);
            assert!((estimate.income_quote.unwrap() + 0.2).abs() < 1e-12);
            assert!(estimate.unavailable_reason.unwrap().contains("3 次结算"));
            assert_eq!(
                estimate.points.last().unwrap().funding_income_quote,
                estimate.income_quote.unwrap()
            );
        }
        report.end_ts_us = anchor;
        let measured = cache
            .estimate_funding_records(&report, true, &coverage.rates)
            .unwrap();
        assert_eq!(measured.skipped_settlements, 2);
        let estimate = crate::funding_rates::estimate_timeline(&report, &coverage, &measured);
        assert_eq!(estimate.income_quote, None);
        assert_eq!(estimate.total_nav_after_fee_quote, None);
        assert!(estimate.points.is_empty());
    }

    #[test]
    fn regular_funding_uses_the_hour_even_when_published_milliseconds_later() {
        let (cache, mut report) = fixture();
        let hour = OFFSET + 200_000_000;
        report.end_ts_us = hour;
        let rate = FundingRate {
            symbol: "BTCUSDT".into(),
            ts_us: hour + 6_000,
            rate: 0.001,
            mark_price: 100.0,
            rate_type: "Regular".into(),
        };
        let rates = BTreeMap::from([("BTCUSDT".into(), Arc::new(vec![rate.clone()]))]);
        let records = cache
            .estimate_funding_records(&report, true, &rates)
            .unwrap();
        assert_eq!(records.records.len(), 1); // Included in a window ending exactly at the hour.
        assert_eq!(records.records[0].ts_us, hour);
        assert_eq!(records.records[0].quantity, 2.0); // At-hour buy of 10 is excluded.
        assert_eq!(records.records[0].boundary_fill_count, 1);
        let special = FundingRate {
            rate_type: "Special".into(),
            ..rate
        };
        assert_eq!(special.estimate_ts_us(), hour + 6_000);
    }

    #[test]
    fn account_snapshot_keeps_initial_owner_and_sources_do_not_mix() {
        let (cache, mut report) = fixture();
        let rates = BTreeMap::from([(
            "BTCUSDT".into(),
            Arc::new(vec![FundingRate {
                symbol: "BTCUSDT".into(),
                ts_us: OFFSET + 200_000_000,
                rate: 0.001,
                mark_price: 100.0,
                rate_type: "Regular".into(),
            }]),
        )]);
        let records = cache
            .estimate_funding_records(&report, false, &rates)
            .unwrap();
        assert!((records.records[0].strategies["__initial_position__"] + 0.1).abs() < 1e-12);
        assert!(
            !records.records[0]
                .strategies
                .contains_key("SYSTEM_POSITION_CLOSE")
        );
        assert!((records.records[0].income_quote + 0.2).abs() < 1e-12);
        report.selected_source_ids = vec!["another-account".into()];
        assert!(
            cache
                .estimate_funding_records(&report, false, &rates)
                .is_err()
        );
    }
}
