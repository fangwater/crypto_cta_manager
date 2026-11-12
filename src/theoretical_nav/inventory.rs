//! Constant-time inventory valuation; individual lots still close in FIFO order.
use super::*;

/// Neumaier accumulation also supports removals. An empty FIFO resets both sums
/// so a closed book cannot carry numerical residue into its next position.
#[derive(Default)]
struct Sum {
    value: f64,
    correction: f64,
}

impl Sum {
    fn add(&mut self, value: f64) {
        let next = self.value + value;
        self.correction += if self.value.abs() >= value.abs() {
            (self.value - next) + value
        } else {
            (value - next) + self.value
        };
        self.value = next;
    }

    fn total(&self) -> f64 {
        self.value + self.correction
    }
}

#[derive(Default)]
pub(super) struct Inventory {
    quantity: Sum,
    // Store costs relative to the opening price. Subtracting two large quote
    // notionals would otherwise lose precision when floating PnL is small.
    cost_offset: Sum,
    anchor: Option<f64>,
}

impl Inventory {
    pub fn new(lots: &VecDeque<FifoLot>) -> Self {
        let mut result = Self::default();
        for lot in lots {
            result.replace(0.0, lot.quantity, lot.entry_price);
        }
        result
    }

    pub fn replace(&mut self, previous: f64, next: f64, price: f64) {
        let anchor = *self.anchor.get_or_insert(price);
        // Add the old and new contributions separately: the stored rounded lot
        // quantity, including clean_zero, is the source of truth for inventory.
        self.quantity.add(-previous);
        self.quantity.add(next);
        let offset = price - anchor;
        self.cost_offset.add(-previous * offset);
        self.cost_offset.add(next * offset);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn quantity(&self) -> f64 {
        clean_zero(self.quantity.total())
    }

    pub fn floating(&self, mark: f64) -> Result<f64> {
        if !mark.is_finite() || mark <= 0.0 {
            bail!("invalid theoretical mark price");
        }
        let Some(anchor) = self.anchor else {
            return Ok(0.0);
        };
        let floating = clean_zero(
            self.quantity
                .total()
                .mul_add(mark - anchor, -self.cost_offset.total()),
        );
        ensure!(floating.is_finite(), "theoretical floating PnL overflowed");
        Ok(floating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare(fills: usize, shape: usize) {
        let mut reference = SymbolState {
            next_lot_seq: 1,
            ..Default::default()
        };
        let mut state = reference;
        let mut reference_lots = VecDeque::new();
        let mut lots = VecDeque::new();
        let mut inventory = Inventory::new(&lots);
        for i in 0..fills {
            let price = 100_000.0 + (i % 101) as f64 * 0.01;
            let size = (1 + i % 19) as f64 * 0.00013;
            let quantity = match shape {
                0 => {
                    if i < fills / 2 {
                        size
                    } else {
                        -size
                    }
                }
                1 => {
                    if i < fills / 2 {
                        -size
                    } else {
                        size
                    }
                }
                _ => {
                    if i % 31 < 17 {
                        size
                    } else {
                        -size * 1.19
                    }
                }
            };
            let fee = if i % 3 == 0 { -0.00002 } else { 0.0003 };
            let applied = evaluate_fill(reference, reference_lots, quantity, price, fee).unwrap();
            reference = SymbolState {
                net_quantity: applied.net_quantity,
                realized_pnl_before_fee_quote: applied.realized_pnl_before_fee_quote,
                estimated_trading_fee_quote: applied.cumulative_fee_quote,
                next_lot_seq: applied.next_lot_seq,
            };
            reference_lots = applied.lots;
            state = apply_fill(state, &mut lots, &mut inventory, quantity, price, fee).unwrap();
            assert_eq!(lots, reference_lots, "FIFO lots differ at {shape}/{i}");
            assert_eq!(state.next_lot_seq, reference.next_lot_seq);
            assert_eq!(
                state.realized_pnl_before_fee_quote.to_bits(),
                reference.realized_pnl_before_fee_quote.to_bits()
            );
            assert_eq!(
                state.estimated_trading_fee_quote.to_bits(),
                reference.estimated_trading_fee_quote.to_bits()
            );
            assert!((state.net_quantity - reference.net_quantity).abs() < 1e-10);
            for mark in [99_999.9, price, 100_003.17] {
                let expected = floating_pnl_at_mark(&reference_lots, mark).unwrap();
                let actual = inventory.floating(mark).unwrap();
                assert!(
                    (actual - expected).abs() <= 1e-8 + expected.abs() * 2e-13,
                    "floating differs at {shape}/{i}: {actual} != {expected}"
                );
            }
        }
        // Fully close every factual lot, then reopen in the opposite direction.
        while let Some(front) = lots.front().copied() {
            state = apply_fill(
                state,
                &mut lots,
                &mut inventory,
                -front.quantity,
                100_001.0,
                0.0,
            )
            .unwrap();
        }
        assert_eq!(inventory.quantity(), 0.0);
        assert_eq!(inventory.floating(1e9).unwrap(), 0.0);
        apply_fill(state, &mut lots, &mut inventory, -3.0, 17.0, -0.001).unwrap();
        assert_eq!(inventory.floating(19.0).unwrap(), -6.0);
    }

    #[test]
    fn incremental_inventory_matches_fifo_scans_with_long_lived_lots_and_reversals() {
        for shape in 0..3 {
            compare(4_000, shape);
        }
    }

    #[test]
    fn inventory_retains_quantity_and_overflow_validation() {
        let mut lots = VecDeque::new();
        let mut inventory = Inventory::default();
        assert!(
            apply_fill(
                SymbolState::default(),
                &mut lots,
                &mut inventory,
                f64::MAX,
                100.0,
                0.001
            )
            .is_err()
        );
        assert!(inventory.floating(f64::NAN).is_err());
        let mut lots = VecDeque::new();
        let mut inventory = Inventory::default();
        assert!(
            apply_fill(
                SymbolState {
                    net_quantity: 9.0,
                    ..Default::default()
                },
                &mut lots,
                &mut inventory,
                1.0,
                100.0,
                0.0
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "release-mode single-thread FIFO inventory benchmark"]
    fn single_thread_inventory_benchmark() {
        let mut lots = VecDeque::new();
        let mut state = SymbolState {
            next_lot_seq: 1,
            ..Default::default()
        };
        let mut inventory = Inventory::default();
        let started = std::time::Instant::now();
        for i in 0..40_000 {
            state = apply_fill(
                state,
                &mut lots,
                &mut inventory,
                0.0013,
                100_000.0 + (i % 101) as f64 * 0.01,
                0.0002,
            )
            .unwrap();
            std::hint::black_box(inventory.floating(100_002.0).unwrap());
        }
        let incremental = started.elapsed();
        let mut reference = SymbolState {
            next_lot_seq: 1,
            ..Default::default()
        };
        let mut reference_lots = VecDeque::new();
        let started = std::time::Instant::now();
        for i in 0..40_000 {
            let applied = evaluate_fill(
                reference,
                reference_lots,
                0.0013,
                100_000.0 + (i % 101) as f64 * 0.01,
                0.0002,
            )
            .unwrap();
            reference = SymbolState {
                net_quantity: applied.net_quantity,
                realized_pnl_before_fee_quote: applied.realized_pnl_before_fee_quote,
                estimated_trading_fee_quote: applied.cumulative_fee_quote,
                next_lot_seq: applied.next_lot_seq,
            };
            reference_lots = applied.lots;
            std::hint::black_box(floating_pnl_at_mark(&reference_lots, 100_002.0).unwrap());
        }
        assert_eq!(lots, reference_lots);
        assert_eq!(
            state.realized_pnl_before_fee_quote,
            reference.realized_pnl_before_fee_quote
        );
        assert_eq!(
            state.estimated_trading_fee_quote,
            reference.estimated_trading_fee_quote
        );
        eprintln!(
            "single_thread_lots={} scan_ms={} incremental_ms={}",
            lots.len(),
            started.elapsed().as_millis(),
            incremental.as_millis()
        );
    }
}
