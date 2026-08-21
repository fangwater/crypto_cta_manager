use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use rocksdb::{Direction, IteratorMode};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::manager_db::{self, ManagerDb, POSITION_UPDATES_CF};
use crate::strategy_catalog::PositionStrategy;
use crate::viz_snapshot::SourceFactualPositions;

const MSG_TYPE: &str = "position_update";
const SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedFactualPosition {
    pub symbol: String,
    pub qty: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usdt: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedSourcePositions {
    pub source_id: String,
    pub snapshot_ts_ms: i64,
    pub position_ready: bool,
    pub positions: Vec<ArchivedFactualPosition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedPublishedAccount {
    pub source_id: String,
    pub binding_name: String,
    pub shares: f64,
    #[serde(default, rename = "leverage", skip_serializing_if = "Option::is_none")]
    legacy_leverage: Option<f64>,
}

impl ArchivedPublishedAccount {
    pub fn effective_shares(&self) -> f64 {
        self.shares * self.legacy_leverage.unwrap_or(1.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionUpdateMsg {
    pub msg_type: String,
    pub schema_version: u32,
    pub received_at_us: i64,
    pub seq: u32,
    pub strategy: PositionStrategy,
    #[serde(default)]
    pub factual_positions: Vec<ArchivedSourcePositions>,
    #[serde(default)]
    pub published_accounts: Vec<ArchivedPublishedAccount>,
}

impl PositionUpdateMsg {
    pub fn from_strategy(
        received_at_us: i64,
        seq: u32,
        strategy: PositionStrategy,
        factual_positions: Vec<SourceFactualPositions>,
        published_accounts: Vec<ArchivedPublishedAccount>,
    ) -> Self {
        Self {
            msg_type: MSG_TYPE.to_string(),
            schema_version: SCHEMA_VERSION,
            received_at_us,
            seq,
            strategy,
            factual_positions: factual_positions.into_iter().map(archive_source).collect(),
            published_accounts,
        }
    }
}

pub fn published_account(
    source_id: impl Into<String>,
    binding_name: impl Into<String>,
    shares: f64,
) -> ArchivedPublishedAccount {
    ArchivedPublishedAccount {
        source_id: source_id.into(),
        binding_name: binding_name.into(),
        shares,
        legacy_leverage: None,
    }
}

fn archive_source(source: SourceFactualPositions) -> ArchivedSourcePositions {
    ArchivedSourcePositions {
        source_id: source.source_id,
        snapshot_ts_ms: source.snapshot_ts_ms,
        position_ready: source.position_ready,
        positions: source
            .positions
            .into_values()
            .map(|position| ArchivedFactualPosition {
                symbol: position.symbol,
                qty: position.qty,
                usdt: position.usdt,
            })
            .collect(),
    }
}

pub struct PositionArchive {
    db: ManagerDb,
    last_key: Mutex<(i64, u32)>,
}

impl PositionArchive {
    pub fn open(db: ManagerDb) -> Result<Self> {
        db.ensure_column_family(POSITION_UPDATES_CF)?;
        let last_key = latest_key(&db)?.unwrap_or((0, 0));
        info!(
            path = %db.path().display(),
            cf = POSITION_UPDATES_CF,
            last_received_at_us = last_key.0,
            last_seq = last_key.1,
            "CTA Manager position update archive opened"
        );
        Ok(Self {
            db,
            last_key: Mutex::new(last_key),
        })
    }

    pub fn append(
        &self,
        received_at_us: i64,
        strategy: &PositionStrategy,
        factual_positions: Vec<SourceFactualPositions>,
        published_accounts: Vec<ArchivedPublishedAccount>,
    ) -> Result<PositionUpdateMsg> {
        if received_at_us <= 0 {
            bail!("position update received_at_us must be positive");
        }
        strategy
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        let seq = {
            let mut last = self
                .last_key
                .lock()
                .expect("position archive sequence lock poisoned");
            let seq = if received_at_us == last.0 {
                last.1.saturating_add(1)
            } else {
                0
            };
            *last = (received_at_us, seq);
            seq
        };
        let msg = PositionUpdateMsg::from_strategy(
            received_at_us,
            seq,
            strategy.clone(),
            factual_positions,
            published_accounts,
        );
        let key = manager_db::encode_seq_key(received_at_us, seq)?;
        let value = serde_json::to_vec(&msg).context("failed to encode position update message")?;
        let handle = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("position_updates column family disappeared")?;
        self.db.db().put_cf(&handle, key, value).with_context(|| {
            format!(
                "failed to append position update {} seq {seq}",
                strategy.strategy_name
            )
        })?;
        Ok(msg)
    }

    pub fn latest(&self) -> Result<Option<PositionUpdateMsg>> {
        let handle = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("position_updates column family disappeared")?;
        let mut iter = self.db.db().iterator_cf(&handle, IteratorMode::End);
        match iter.next() {
            Some(Ok((_, value))) => decode_msg(&value).map(Some),
            Some(Err(error)) => Err(error).context("failed to read latest position update"),
            None => Ok(None),
        }
    }

    pub fn scan_from(&self, start_received_at_us: i64) -> Result<Vec<PositionUpdateMsg>> {
        if start_received_at_us < 0 {
            bail!("position update scan start must not be negative");
        }
        let handle = self
            .db
            .db()
            .cf_handle(POSITION_UPDATES_CF)
            .context("position_updates column family disappeared")?;
        let start = manager_db::encode_seq_key(start_received_at_us.max(1), 0)?;
        let mut out = Vec::new();
        let iter = self
            .db
            .db()
            .iterator_cf(&handle, IteratorMode::From(&start, Direction::Forward));
        for item in iter {
            let (_, value) = item.context("failed to iterate position updates")?;
            out.push(decode_msg(&value)?);
        }
        Ok(out)
    }
}

fn decode_msg(bytes: &[u8]) -> Result<PositionUpdateMsg> {
    serde_json::from_slice(bytes).context("failed to decode position update message")
}

fn latest_key(db: &ManagerDb) -> Result<Option<(i64, u32)>> {
    let Some(handle) = db.db().cf_handle(POSITION_UPDATES_CF) else {
        return Ok(None);
    };
    let mut iter = db.db().iterator_cf(&handle, IteratorMode::End);
    match iter.next() {
        Some(Ok((key, _))) => {
            let Some(decoded) = manager_db::decode_seq_key(&key) else {
                warn!("ignoring unreadable latest position update key");
                return Ok(None);
            };
            Ok(Some(decoded))
        }
        Some(Err(error)) => Err(error).context("failed to inspect latest position update key"),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::*;
    use crate::order_config::TargetPosition;

    fn strategy(name: &str, qty: f64, signal: i32, updated_at_us: i64) -> PositionStrategy {
        PositionStrategy {
            strategy_name: name.to_string(),
            targets: BTreeMap::from([("BTCUSDT".to_string(), TargetPosition { qty, signal })]),
            updated_at_us,
        }
    }

    #[test]
    fn appends_one_message_per_accepted_position_update() {
        let dir = TempDir::new().unwrap();
        let archive = PositionArchive::open(ManagerDb::open(dir.path()).unwrap()).unwrap();
        let first = archive
            .append(
                1_700_000_000_000_001,
                &strategy("cta_a", 0.1, 1, 11),
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        let second = archive
            .append(
                1_700_000_000_000_001,
                &strategy("cta_a", 0.2, -1, 12),
                vec![SourceFactualPositions {
                    source_id: "binance_exec_trade01".into(),
                    snapshot_ts_ms: 1_700_000_000_000,
                    position_ready: true,
                    positions: BTreeMap::from([(
                        "BTCUSDT".into(),
                        crate::viz_snapshot::FactualPosition {
                            symbol: "BTCUSDT".into(),
                            qty: 0.15,
                            usdt: Some(1_500.0),
                        },
                    )]),
                }],
                vec![published_account("binance_exec_trade01", "cta_a", 2.0)],
            )
            .unwrap();

        assert_eq!(first.msg_type, "position_update");
        assert_eq!(first.schema_version, 4);
        assert_eq!(first.seq, 0);
        assert!(first.factual_positions.is_empty());
        assert!(first.published_accounts.is_empty());
        assert_eq!(second.factual_positions.len(), 1);
        assert_eq!(second.published_accounts.len(), 1);
        assert_eq!(
            second.published_accounts[0].source_id,
            "binance_exec_trade01"
        );
        assert!((second.published_accounts[0].shares - 2.0).abs() < 1e-12);
        assert!((second.published_accounts[0].effective_shares() - 2.0).abs() < 1e-12);
        assert_eq!(
            second.factual_positions[0].source_id,
            "binance_exec_trade01"
        );
        assert!((second.factual_positions[0].positions[0].qty - 0.15).abs() < 1e-12);
        assert_eq!(second.seq, 1);
        assert!((second.strategy.targets["BTCUSDT"].qty - 0.2).abs() < 1e-12);
        assert_eq!(second.strategy.targets["BTCUSDT"].signal, -1);
        let encoded = serde_json::to_value(&second).unwrap();
        assert!(encoded["strategy"].get("equity_usdt").is_none());
        assert!(encoded["published_accounts"][0].get("leverage").is_none());

        let latest = archive.latest().unwrap().expect("latest");
        assert_eq!(latest, second);
        let scanned = archive.scan_from(1_700_000_000_000_001).unwrap();
        assert_eq!(scanned, vec![first, second]);
    }

    #[test]
    fn continues_sequence_after_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let archive = PositionArchive::open(ManagerDb::open(dir.path()).unwrap()).unwrap();
            archive
                .append(
                    1_700_000_000_000_001,
                    &strategy("cta_a", 0.1, 0, 11),
                    Vec::new(),
                    Vec::new(),
                )
                .unwrap();
        }
        let archive = PositionArchive::open(ManagerDb::open(dir.path()).unwrap()).unwrap();
        let same_tick = archive
            .append(
                1_700_000_000_000_001,
                &strategy("cta_a", 0.2, 1, 12),
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        let later = archive
            .append(
                1_700_000_000_000_002,
                &strategy("cta_a", 0.3, 2, 13),
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(same_tick.seq, 1);
        assert_eq!(later.seq, 0);
    }

    #[test]
    fn reads_legacy_messages_without_published_accounts() {
        let raw = serde_json::json!({
            "msg_type": "position_update",
            "schema_version": 2,
            "received_at_us": 1,
            "seq": 0,
            "strategy": {
                "strategy_name": "cta_a",
                "equity_usdt": 10000.0,
                "targets": {"BTCUSDT": {"qty": 0.1, "signal": 0}},
                "updated_at_us": 1
            },
            "factual_positions": []
        });
        let msg: PositionUpdateMsg = serde_json::from_value(raw).unwrap();
        assert!(msg.published_accounts.is_empty());
        assert_eq!(msg.schema_version, 2);
    }

    #[test]
    fn folds_legacy_leverage_into_effective_historical_shares() {
        let raw = serde_json::json!({
            "source_id": "binance_exec_trade01",
            "binding_name": "cta_a",
            "shares": 2.0,
            "leverage": 3.0
        });
        let account: ArchivedPublishedAccount = serde_json::from_value(raw).unwrap();
        assert!((account.effective_shares() - 6.0).abs() < 1e-12);
    }
}
