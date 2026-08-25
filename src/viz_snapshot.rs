use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use tracing::warn;

const EXEC_PRE_TRADE_STATE: &str = "exec_pre_trade_state";

#[derive(Debug, Clone, PartialEq)]
pub struct FactualPosition {
    pub symbol: String,
    pub qty: f64,
    pub usdt: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFactualPositions {
    pub source_id: String,
    pub snapshot_ts_ms: i64,
    pub position_ready: bool,
    pub positions: BTreeMap<String, FactualPosition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrategyAllocationRow {
    pub strategy_name: String,
    pub symbol: String,
    pub current_qty: f64,
    pub current_usdt: Option<f64>,
    pub account_position_qty: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceStrategyAllocation {
    pub source_id: String,
    pub snapshot_ts_ms: i64,
    pub position_ready: bool,
    pub rows: Vec<StrategyAllocationRow>,
}

#[derive(Clone)]
pub struct VizSnapshotClient {
    http: Client,
}

impl VizSnapshotClient {
    pub fn new(timeout_secs: u64) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
            .no_proxy()
            .build()
            .context("failed to build Exec Viz snapshot client")?;
        Ok(Self { http })
    }

    pub async fn load_strategy_positions(
        &self,
        source_id: &str,
        base_url: &str,
        strategy_name: &str,
    ) -> Result<SourceFactualPositions> {
        let url = snapshot_url(base_url)?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to request Exec Viz snapshot for {source_id}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("failed to read Exec Viz snapshot for {source_id}"))?;
        if status != StatusCode::OK {
            bail!("Exec Viz snapshot for {source_id} returned {status}: {body}");
        }
        let snapshot: VizSnapshot = serde_json::from_str(&body)
            .with_context(|| format!("failed to decode Exec Viz snapshot for {source_id}"))?;
        Ok(extract_strategy_positions_from_decoded(
            source_id,
            strategy_name,
            &snapshot,
        ))
    }

    pub async fn load_strategy_allocation(
        &self,
        source_id: &str,
        base_url: &str,
    ) -> Result<SourceStrategyAllocation> {
        let url = snapshot_url(base_url)?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to request Exec Viz snapshot for {source_id}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("failed to read Exec Viz snapshot for {source_id}"))?;
        if status != StatusCode::OK {
            bail!("Exec Viz snapshot for {source_id} returned {status}: {body}");
        }
        let snapshot: VizSnapshot = serde_json::from_str(&body)
            .with_context(|| format!("failed to decode Exec Viz snapshot for {source_id}"))?;
        Ok(extract_strategy_allocation_from_decoded(
            source_id, &snapshot,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct VizSnapshot {
    #[serde(default)]
    ts_ms: i64,
    #[serde(default)]
    entries: Vec<VizSnapshotEntry>,
}

#[derive(Debug, Deserialize)]
struct VizSnapshotEntry {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    #[serde(default)]
    ts_ms: Option<i64>,
    entry: Option<ExecStateEntry>,
}

#[derive(Debug, Deserialize)]
struct ExecStateEntry {
    #[serde(default)]
    ts_ms: i64,
    #[serde(default)]
    position_ready: bool,
    #[serde(default)]
    rows: Vec<ExecStateRow>,
}

#[derive(Debug, Deserialize)]
struct ExecStateRow {
    #[serde(default)]
    strategy_name: String,
    #[serde(default)]
    symbol: String,
    current_qty: Option<f64>,
    current_usdt: Option<f64>,
    account_position_qty: Option<f64>,
}

pub fn extract_strategy_allocation(
    source_id: &str,
    snapshot_json: &serde_json::Value,
) -> SourceStrategyAllocation {
    match serde_json::from_value::<VizSnapshot>(snapshot_json.clone()) {
        Ok(snapshot) => extract_strategy_allocation_from_decoded(source_id, &snapshot),
        Err(error) => {
            warn!(
                source_id,
                error = %error,
                "Exec Viz snapshot JSON did not match expected allocation shape"
            );
            SourceStrategyAllocation {
                source_id: source_id.to_string(),
                snapshot_ts_ms: 0,
                position_ready: false,
                rows: Vec::new(),
            }
        }
    }
}

fn snapshot_url(base_url: &str) -> Result<Url> {
    let parsed =
        Url::parse(base_url).with_context(|| format!("invalid Exec Viz origin: {base_url}"))?;
    parsed
        .join("snapshot")
        .with_context(|| format!("failed to build Exec Viz snapshot URL from {base_url}"))
}

pub fn extract_strategy_positions(
    source_id: &str,
    strategy_name: &str,
    snapshot_json: &serde_json::Value,
) -> SourceFactualPositions {
    match serde_json::from_value::<VizSnapshot>(snapshot_json.clone()) {
        Ok(snapshot) => {
            extract_strategy_positions_from_decoded(source_id, strategy_name, &snapshot)
        }
        Err(error) => {
            warn!(
                source_id,
                strategy_name,
                error = %error,
                "Exec Viz snapshot JSON did not match expected shape"
            );
            SourceFactualPositions {
                source_id: source_id.to_string(),
                snapshot_ts_ms: 0,
                position_ready: false,
                positions: BTreeMap::new(),
            }
        }
    }
}

fn extract_strategy_positions_from_decoded(
    source_id: &str,
    strategy_name: &str,
    snapshot: &VizSnapshot,
) -> SourceFactualPositions {
    let Some(entry) = snapshot
        .entries
        .iter()
        .find(|entry| entry.msg_type.as_deref() == Some(EXEC_PRE_TRADE_STATE))
    else {
        return SourceFactualPositions {
            source_id: source_id.to_string(),
            snapshot_ts_ms: snapshot.ts_ms,
            position_ready: false,
            positions: BTreeMap::new(),
        };
    };
    let Some(state) = &entry.entry else {
        return SourceFactualPositions {
            source_id: source_id.to_string(),
            snapshot_ts_ms: entry.ts_ms.unwrap_or(snapshot.ts_ms),
            position_ready: false,
            positions: BTreeMap::new(),
        };
    };

    let mut positions = BTreeMap::new();
    for row in &state.rows {
        if row.strategy_name != strategy_name {
            continue;
        }
        let symbol = normalize_symbol(&row.symbol);
        if symbol.is_empty() {
            continue;
        }
        let Some(qty) = row.current_qty.filter(|value| value.is_finite()) else {
            continue;
        };
        positions.insert(
            symbol.clone(),
            FactualPosition {
                symbol,
                qty,
                usdt: row.current_usdt.filter(|value| value.is_finite()),
            },
        );
    }
    SourceFactualPositions {
        source_id: source_id.to_string(),
        snapshot_ts_ms: if state.ts_ms > 0 {
            state.ts_ms
        } else {
            entry.ts_ms.unwrap_or(snapshot.ts_ms)
        },
        position_ready: state.position_ready,
        positions,
    }
}

fn extract_strategy_allocation_from_decoded(
    source_id: &str,
    snapshot: &VizSnapshot,
) -> SourceStrategyAllocation {
    let Some(entry) = snapshot
        .entries
        .iter()
        .find(|entry| entry.msg_type.as_deref() == Some(EXEC_PRE_TRADE_STATE))
    else {
        return SourceStrategyAllocation {
            source_id: source_id.to_string(),
            snapshot_ts_ms: snapshot.ts_ms,
            position_ready: false,
            rows: Vec::new(),
        };
    };
    let Some(state) = &entry.entry else {
        return SourceStrategyAllocation {
            source_id: source_id.to_string(),
            snapshot_ts_ms: entry.ts_ms.unwrap_or(snapshot.ts_ms),
            position_ready: false,
            rows: Vec::new(),
        };
    };
    SourceStrategyAllocation {
        source_id: source_id.to_string(),
        snapshot_ts_ms: if state.ts_ms > 0 {
            state.ts_ms
        } else {
            entry.ts_ms.unwrap_or(snapshot.ts_ms)
        },
        position_ready: state.position_ready,
        rows: state
            .rows
            .iter()
            .filter_map(|row| {
                let current_qty = row.current_qty.filter(|value| value.is_finite())?;
                let symbol = normalize_symbol(&row.symbol);
                (!symbol.is_empty() && !row.strategy_name.trim().is_empty()).then_some(
                    StrategyAllocationRow {
                        strategy_name: row.strategy_name.clone(),
                        symbol,
                        current_qty,
                        current_usdt: row.current_usdt.filter(|value| value.is_finite()),
                        account_position_qty: row
                            .account_position_qty
                            .filter(|value| value.is_finite()),
                    },
                )
            })
            .collect(),
    }
}

fn normalize_symbol(raw: &str) -> String {
    raw.chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_the_requested_strategy_current_qty() {
        let snapshot = serde_json::json!({
            "ts_ms": 1785916800000i64,
            "entries": [
                {
                    "type": "exec_pre_trade_state",
                    "ts_ms": 1785916800000i64,
                    "entry": {
                        "ts_ms": 1785916800000i64,
                        "position_ready": true,
                        "rows": [
                            {
                                "strategy_name": "cta_a",
                                "symbol": "btcusdt",
                                "current_qty": 0.5,
                                "current_usdt": 32500.0,
                                "account_position_qty": 0.3
                            },
                            {
                                "strategy_name": "cta_b",
                                "symbol": "BTCUSDT",
                                "current_qty": -0.2,
                                "current_usdt": -13000.0
                            },
                            {
                                "strategy_name": "cta_a",
                                "symbol": "ETHUSDT",
                                "current_qty": 8.0,
                                "current_usdt": 28000.0
                            }
                        ]
                    }
                }
            ]
        });
        let extracted = extract_strategy_positions("binance_exec_trade01", "cta_a", &snapshot);
        assert!(extracted.position_ready);
        assert_eq!(extracted.snapshot_ts_ms, 1_785_916_800_000);
        assert_eq!(extracted.positions.len(), 2);
        assert!((extracted.positions["BTCUSDT"].qty - 0.5).abs() < 1e-12);
        assert_eq!(extracted.positions["BTCUSDT"].usdt, Some(32_500.0));
        assert!((extracted.positions["ETHUSDT"].qty - 8.0).abs() < 1e-12);
        assert!(!extracted.positions.contains_key("cta_b"));
    }

    #[test]
    fn missing_exec_state_returns_empty_positions() {
        let snapshot = serde_json::json!({
            "ts_ms": 1,
            "entries": [{"type": "exec_pre_trade_risk", "entry": {"ts_ms": 1}}]
        });
        let extracted = extract_strategy_positions("binance_exec_trade01", "cta_a", &snapshot);
        assert!(!extracted.position_ready);
        assert!(extracted.positions.is_empty());
    }

    #[test]
    fn extracts_complete_strategy_allocation_rows() {
        let snapshot = serde_json::json!({
            "ts_ms": 1,
            "entries": [{
                "type": "exec_pre_trade_state",
                "entry": {
                    "ts_ms": 2,
                    "position_ready": true,
                    "rows": [{
                        "strategy_name": "cta_a",
                        "symbol": "BTC-USDT",
                        "current_qty": 0.5,
                        "current_usdt": 50000.0,
                        "account_position_qty": 0.3
                    }, {
                        "strategy_name": "SYSTEM_POSITION_CLOSE",
                        "symbol": "BTCUSDT",
                        "current_qty": -0.2,
                        "current_usdt": -20000.0,
                        "account_position_qty": 0.3
                    }]
                }
            }]
        });

        let allocation = extract_strategy_allocation("trade01", &snapshot);
        assert!(allocation.position_ready);
        assert_eq!(allocation.snapshot_ts_ms, 2);
        assert_eq!(allocation.rows.len(), 2);
        assert_eq!(allocation.rows[0].symbol, "BTCUSDT");
        assert_eq!(allocation.rows[0].account_position_qty, Some(0.3));
        assert_eq!(allocation.rows[1].strategy_name, "SYSTEM_POSITION_CLOSE");
    }
}
