use super::*;
use crate::kline::{DAY_US, Kline};
use sha2::{Digest, Sha256};

// Frozen scan-based replay oracle: quantities, FIFO realization and fees are
// checked independently of the new constant-time inventory valuation.
fn reference_curve(
    deltas: &[VirtualDelta],
    carry: CarriedInventory,
    fills: &[(i64, usize, usize)],
    store: &KlineStore,
    start: i64,
    end: i64,
) -> Vec<TheoreticalNavPoint> {
    let schedules = deltas
        .iter()
        .map(|delta| delta.prices(store).unwrap().unwrap())
        .collect::<Vec<_>>();
    let markets = deltas
        .iter()
        .map(|delta| delta.symbol.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|symbol| {
            (
                symbol.clone(),
                store.scan(&symbol, start - MINUTE_US, end).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mark = |symbol: &str, ts: i64| {
        let bars = &markets[symbol];
        bars[bars.partition_point(|c| c.end_ts_us() <= ts) - 1].close
    };
    let mut states = BTreeMap::new();
    for (key, quantity) in carry {
        let price = mark(&key.1, start);
        states.insert(
            key,
            (
                SymbolState {
                    net_quantity: quantity,
                    next_lot_seq: 2,
                    ..Default::default()
                },
                VecDeque::from([FifoLot {
                    seq: 1,
                    quantity,
                    entry_price: price,
                }]),
            ),
        );
    }
    let mut ticks = BTreeSet::from([start, end]);
    let mut tick = start.div_euclid(900_000_000) * 900_000_000 + 900_000_000;
    while tick < end {
        ticks.insert(tick);
        tick += 900_000_000;
    }
    ticks.extend(fills.iter().map(|fill| fill.0));
    let mut cursor = 0;
    let mut points = Vec::new();
    for ts in ticks {
        while cursor < fills.len() && fills[cursor].0 <= ts {
            let (_, index, slice) = fills[cursor];
            cursor += 1;
            let delta = &deltas[index];
            let (state, lots) = states
                .entry((
                    delta.source_id.clone(),
                    delta.symbol.clone(),
                    delta.venue.clone(),
                ))
                .or_insert_with(|| {
                    (
                        SymbolState {
                            next_lot_seq: 1,
                            ..Default::default()
                        },
                        VecDeque::new(),
                    )
                });
            let priced = &schedules[index];
            let applied = evaluate_fill(
                *state,
                std::mem::take(lots),
                priced.quantity(delta.delta_qty, slice),
                priced.prices[slice].unwrap(),
                delta.fee_rate,
            )
            .unwrap();
            *state = SymbolState {
                net_quantity: applied.net_quantity,
                realized_pnl_before_fee_quote: applied.realized_pnl_before_fee_quote,
                estimated_trading_fee_quote: applied.cumulative_fee_quote,
                next_lot_seq: applied.next_lot_seq,
            };
            *lots = applied.lots;
        }
        let mut point = TheoreticalNavPoint {
            ts_us: ts,
            ..Default::default()
        };
        for ((_, symbol, _), (state, lots)) in &states {
            let floating = if lots.is_empty() {
                0.0
            } else {
                floating_pnl_at_mark(lots, mark(symbol, ts)).unwrap()
            };
            point.nav_change_before_fee_quote += state.realized_pnl_before_fee_quote + floating;
            point.estimated_trading_fee_quote += state.estimated_trading_fee_quote;
        }
        point.nav_change_after_fee_quote =
            point.nav_change_before_fee_quote - point.estimated_trading_fee_quote;
        if ts == start {
            point = TheoreticalNavPoint {
                ts_us: ts,
                ..Default::default()
            };
        }
        points.push(point);
    }
    points
}

#[test]
#[ignore = "release-mode seven-day FIFO benchmark"]
fn seven_day_curve_benchmark() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = KlineStore::from_db(
        crate::manager_db::ManagerDb::open(dir.path()).unwrap(),
        Default::default(),
    )
    .unwrap();
    let start = now_us().div_euclid(DAY_US) * DAY_US - 8 * DAY_US;
    let end = start + 7 * DAY_US;
    let mut deltas = Vec::new();
    let mut carry = CarriedInventory::new();
    for market in 0..95 {
        let symbol = format!("S{market}USDT");
        let bars = (-1_i64..=10_085)
            .map(|minute| {
                let price = 100.0 + market as f64 + (minute % 113) as f64 * 0.01;
                let volume = 1.0 + (minute + 1).rem_euclid(17) as f64;
                Kline {
                    open_ts_us: start + minute * MINUTE_US,
                    open: price - 0.1,
                    high: price + 1.0,
                    low: price - 1.0,
                    close: price,
                    base_volume: volume,
                    quote_volume: price * volume,
                    trades: 20,
                }
            })
            .collect::<Vec<_>>();
        store
            .save_page(&symbol, start - MINUTE_US, end + 6 * MINUTE_US, &bars)
            .unwrap();
        carry.insert(
            ("bench".into(), symbol.clone(), "binance-futures".into()),
            1.0,
        );
        let mut previous = 1.0;
        for minute in (0..10_080).step_by(12) {
            let next = ((minute / 12 + market * 3) % 19) as f64 - 9.0;
            let quantity = next - previous;
            previous = next;
            deltas.push(VirtualDelta {
                execution_model: ExecutionModel::Twap5m,
                source_id: "bench".into(),
                binding_name: "alpha".into(),
                strategy_name: "alpha".into(),
                symbol: symbol.clone(),
                venue: "binance-futures".into(),
                received_at_us: start + minute as i64 * MINUTE_US + market as i64,
                seq: market as u32,
                delta_qty: quantity,
                fee_rate: if market % 3 == 0 { -0.00001 } else { 0.0002 },
                archived_taker_fee_rate: Some(0.0003),
                legacy_fee: false,
                target_signal: 0,
            });
        }
    }
    deltas.sort_by_key(|delta| (delta.received_at_us, delta.seq));
    for model in [
        ExecutionModel::Twap5m,
        ExecutionModel::Vwap3m,
        ExecutionModel::InstantTaker,
    ] {
        for delta in &mut deltas {
            delta.execution_model = model;
        }
        let mut fills = deltas
            .iter()
            .enumerate()
            .flat_map(|(index, delta)| {
                (0..model.sample_count()).map(move |slice| (delta.slice_ts_us(slice), index, slice))
            })
            .filter(|(ts, _, _)| *ts <= end)
            .collect::<Vec<_>>();
        fills.sort_by_key(|(ts, index, slice)| {
            (
                *ts,
                deltas[*index].received_at_us,
                deltas[*index].seq,
                *index,
                *slice,
            )
        });
        let mut result = TheoreticalNavTimeline::default();
        let reference = reference_curve(&deltas, carry.clone(), &fills, &store, start, end);
        let at = std::time::Instant::now();
        rebuild_timeline(
            &deltas,
            carry.clone(),
            fills,
            &store,
            start,
            end,
            usize::MAX,
            &mut result,
        )
        .unwrap();
        let elapsed = at.elapsed();
        assert!(
            result.unavailable_reason.is_none(),
            "{:?}",
            result.unavailable_reason
        );
        let mut digest = Sha256::new();
        for point in &downsample_points(reference.clone(), 3500) {
            digest.update((point.ts_us - start).to_be_bytes());
            for value in [
                point.nav_change_before_fee_quote,
                point.nav_change_after_fee_quote,
                point.estimated_trading_fee_quote,
            ] {
                digest.update(value.to_bits().to_be_bytes());
            }
        }
        let fingerprint = format!("{:x}", digest.finalize());
        let expected = match model {
            ExecutionModel::Twap5m => {
                "687ce8f7ecd9b0fffc3403b1fe99390b2a24d8555dbc174b3f9f872bf6f815fa"
            }
            ExecutionModel::Vwap3m => {
                "76ee6738e42e57654c2c987fe9fa3a90d08979b17372052f9b029451ae060491"
            }
            ExecutionModel::InstantTaker => {
                "fd926561d40cfc8d448648ff4fd88b1b7b2fda02013ce4f460d47d72dd7aa197"
            }
            _ => unreachable!(),
        };
        assert_eq!(
            fingerprint, expected,
            "scan-based reference changed for {model:?}"
        );
        assert_eq!(result.points.len(), reference.len());
        let mut max_error = 0.0_f64;
        for (actual, expected) in result.points.iter().zip(&reference) {
            assert_eq!(actual.ts_us, expected.ts_us);
            assert_eq!(
                actual.estimated_trading_fee_quote.to_bits(),
                expected.estimated_trading_fee_quote.to_bits()
            );
            for (actual, expected) in [
                (
                    actual.nav_change_before_fee_quote,
                    expected.nav_change_before_fee_quote,
                ),
                (
                    actual.nav_change_after_fee_quote,
                    expected.nav_change_after_fee_quote,
                ),
            ] {
                max_error = max_error.max((actual - expected).abs());
                assert!((actual - expected).abs() <= 1e-8 + expected.abs() * 2e-13);
            }
        }
        eprintln!(
            "model={model:?} deltas={} points={} elapsed_ms={} max_nav_error={max_error:e} reference_sha256={}",
            deltas.len(),
            result.points.len(),
            elapsed.as_millis(),
            fingerprint
        );
    }
}
