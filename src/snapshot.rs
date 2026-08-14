use std::collections::HashSet;

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
            if position.symbol.trim().is_empty() || position.symbol != position.symbol.trim() {
                bail!(
                    "snapshot position symbol must be nonempty without surrounding whitespace: {:?}",
                    position.symbol
                );
            }
            if !(0..=u8::MAX as i16).contains(&position.venue_code) {
                bail!(
                    "snapshot position {} has invalid venue_code {}",
                    position.symbol,
                    position.venue_code
                );
            }
            if !position.quantity.is_finite() || position.quantity == 0.0 {
                bail!(
                    "snapshot position {} quantity must be finite and nonzero",
                    position.symbol
                );
            }
            if position
                .reference_price
                .is_some_and(|price| !price.is_finite() || price <= 0.0)
            {
                bail!(
                    "snapshot position {} reference_price must be finite and positive",
                    position.symbol
                );
            }
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
}
