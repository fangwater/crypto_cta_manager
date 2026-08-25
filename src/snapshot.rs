use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PositionSnapshot {
    pub source_id: String,
    pub snapshot_ts_us: i64,
    pub positions: Vec<SnapshotPosition>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SnapshotPosition {
    pub symbol: String,
    pub venue_code: i16,
    /// Signed base quantity: positive is long, negative is short.
    pub quantity: f64,
    /// Snapshot valuation basis. None uses the first later RocksDB fill price.
    pub reference_price: Option<f64>,
}

/// An immutable per-strategy allocation anchor. Unlike an account position
/// snapshot, every entry has an explicit strategy owner and valuation basis.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StrategyPositionSnapshot {
    pub source_id: String,
    pub snapshot_ts_us: i64,
    pub positions: Vec<StrategySnapshotPosition>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StrategySnapshotPosition {
    pub strategy_name: String,
    pub symbol: String,
    pub venue_code: i16,
    /// Signed base quantity: positive is long, negative is short.
    pub quantity: f64,
    /// Shared mark at the allocation anchor. It is required so every strategy
    /// begins the post-anchor PnL calculation at zero.
    pub reference_price: f64,
}

impl PositionSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.source_id.trim().is_empty() {
            bail!("position snapshot source_id must not be empty");
        }
        if self.snapshot_ts_us <= 0 {
            bail!("position snapshot timestamp must be positive");
        }
        let mut keys = HashSet::new();
        for position in &self.positions {
            validate_position(
                &position.symbol,
                position.venue_code,
                position.quantity,
                position.reference_price,
                "snapshot",
            )?;
            if !keys.insert((position.symbol.as_str(), position.venue_code)) {
                bail!(
                    "duplicate snapshot position for {} venue {}",
                    position.symbol,
                    position.venue_code
                );
            }
        }
        Ok(())
    }
}

impl StrategyPositionSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.source_id.trim().is_empty() {
            bail!("strategy position snapshot source_id must not be empty");
        }
        if self.snapshot_ts_us <= 0 {
            bail!("strategy position snapshot timestamp must be positive");
        }
        if self.positions.is_empty() {
            bail!("strategy position snapshot must contain at least one position");
        }
        let mut keys = HashSet::new();
        let mut marks = HashMap::<(&str, i16), f64>::new();
        for position in &self.positions {
            validate_strategy_name(&position.strategy_name)?;
            validate_position(
                &position.symbol,
                position.venue_code,
                position.quantity,
                Some(position.reference_price),
                "strategy position snapshot",
            )?;
            if !keys.insert((
                position.strategy_name.as_str(),
                position.symbol.as_str(),
                position.venue_code,
            )) {
                bail!(
                    "duplicate strategy position snapshot entry for {} {} venue {}",
                    position.strategy_name,
                    position.symbol,
                    position.venue_code
                );
            }
            let key = (position.symbol.as_str(), position.venue_code);
            if let Some(mark) = marks.insert(key, position.reference_price)
                && !same_mark(mark, position.reference_price)
            {
                bail!(
                    "strategy position snapshot entries for {} venue {} must share one reference price",
                    position.symbol,
                    position.venue_code
                );
            }
        }
        Ok(())
    }

    /// Derive the matching account-level anchor. Strategy entries may offset
    /// each other, so zero-net symbol/venue pairs intentionally disappear.
    pub fn account_snapshot(&self) -> Result<PositionSnapshot> {
        self.validate()?;
        let mut positions = BTreeMap::<(String, i16), (f64, f64)>::new();
        for position in &self.positions {
            let entry = positions
                .entry((position.symbol.clone(), position.venue_code))
                .or_insert((0.0, position.reference_price));
            entry.0 += position.quantity;
        }
        let snapshot = PositionSnapshot {
            source_id: self.source_id.clone(),
            snapshot_ts_us: self.snapshot_ts_us,
            positions: positions
                .into_iter()
                .filter_map(|((symbol, venue_code), (quantity, reference_price))| {
                    (quantity.abs() > 1e-12).then_some(SnapshotPosition {
                        symbol,
                        venue_code,
                        quantity,
                        reference_price: Some(reference_price),
                    })
                })
                .collect(),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

fn validate_strategy_name(value: &str) -> Result<()> {
    if value == "__unallocated__" {
        return Ok(());
    }
    crate::order_config::validate_strategy_name(value).map_err(anyhow::Error::msg)
}

fn validate_position(
    symbol: &str,
    venue_code: i16,
    quantity: f64,
    reference_price: Option<f64>,
    kind: &str,
) -> Result<()> {
    if symbol.trim().is_empty() || symbol != symbol.trim() {
        bail!("{kind} position symbol must be nonempty without surrounding whitespace: {symbol:?}");
    }
    if !(0..=u8::MAX as i16).contains(&venue_code) {
        bail!("{kind} position {symbol} has invalid venue_code {venue_code}");
    }
    if !quantity.is_finite() || quantity == 0.0 {
        bail!("{kind} position {symbol} quantity must be finite and nonzero");
    }
    if reference_price.is_some_and(|price| !price.is_finite() || price <= 0.0) {
        bail!("{kind} position {symbol} reference_price must be finite and positive");
    }
    Ok(())
}

fn same_mark(left: f64, right: f64) -> bool {
    (left - right).abs() <= left.abs().max(right.abs()).max(1.0) * 1e-10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_position_identity_and_values() {
        let mut snapshot = PositionSnapshot {
            source_id: "trade01".to_string(),
            snapshot_ts_us: 1,
            positions: vec![SnapshotPosition {
                symbol: "BTCUSDT".to_string(),
                venue_code: 1,
                quantity: -0.01,
                reference_price: None,
            }],
        };
        snapshot.validate().unwrap();
        snapshot.positions.push(snapshot.positions[0].clone());
        assert!(
            snapshot
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn strategy_snapshot_derives_an_account_anchor_without_losing_net_quantity() {
        let snapshot = StrategyPositionSnapshot {
            source_id: "trade01".to_string(),
            snapshot_ts_us: 2,
            positions: vec![
                StrategySnapshotPosition {
                    strategy_name: "cta_a".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    venue_code: 1,
                    quantity: 0.4,
                    reference_price: 100.0,
                },
                StrategySnapshotPosition {
                    strategy_name: "cta_b".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    venue_code: 1,
                    quantity: -0.1,
                    reference_price: 100.0,
                },
            ],
        };

        let account = snapshot.account_snapshot().unwrap();
        assert_eq!(account.snapshot_ts_us, 2);
        assert_eq!(account.positions.len(), 1);
        assert!((account.positions[0].quantity - 0.3).abs() < 1e-12);
        assert_eq!(account.positions[0].reference_price, Some(100.0));
    }

    #[test]
    fn strategy_snapshot_rejects_mixed_marks_for_one_symbol_venue() {
        let snapshot = StrategyPositionSnapshot {
            source_id: "trade01".to_string(),
            snapshot_ts_us: 2,
            positions: vec![
                StrategySnapshotPosition {
                    strategy_name: "cta_a".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    venue_code: 1,
                    quantity: 0.4,
                    reference_price: 100.0,
                },
                StrategySnapshotPosition {
                    strategy_name: "cta_b".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    venue_code: 1,
                    quantity: -0.1,
                    reference_price: 101.0,
                },
            ],
        };

        assert!(snapshot.validate().is_err());
    }
}
