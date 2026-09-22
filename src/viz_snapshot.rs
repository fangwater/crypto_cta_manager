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

#[derive(Debug, Clone, PartialEq)]
pub struct ExecStateSnapshot {
    pub source_id: String,
    pub snapshot_ts_ms: i64,
    pub position_ready: bool,
    pub rows: Vec<ExecStateRowSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecStateRowSnapshot {
    pub strategy_name: String,
    pub symbol: String,
    pub source_updated_at_ms: i64,
    pub current_qty: Option<f64>,
    pub current_usdt: Option<f64>,
    pub target_qty: Option<f64>,
    pub pending_qty: Option<f64>,
    pub live_order_qty: Option<f64>,
    pub remaining_batches: u32,
    pub estimated_completion_ts_ms: i64,
    pub execution_complete: bool,
    pub completion_reason: String,
    pub account_position_qty: Option<f64>,
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

    pub async fn load_exec_state(
        &self,
        source_id: &str,
        base_url: &str,
    ) -> Result<ExecStateSnapshot> {
        let snapshot = self.fetch_snapshot(source_id, base_url).await?;
        Ok(extract_exec_state_from_decoded(source_id, &snapshot))
    }

    async fn fetch_snapshot(&self, source_id: &str, base_url: &str) -> Result<VizSnapshot> {
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
        serde_json::from_str(&body)
            .with_context(|| format!("failed to decode Exec Viz snapshot for {source_id}"))
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
    #[serde(default)]
    source_updated_at_ms: i64,
    target_qty: Option<f64>,
    pending_qty: Option<f64>,
    live_order_qty: Option<f64>,
    #[serde(default)]
    remaining_batches: u32,
    #[serde(default)]
    estimated_completion_ts_ms: i64,
    #[serde(default)]
    execution_complete: bool,
    #[serde(default)]
    completion_reason: String,
}

fn extract_exec_state_from_decoded(source_id: &str, snapshot: &VizSnapshot) -> ExecStateSnapshot {
    let Some(entry) = snapshot
        .entries
        .iter()
        .find(|entry| entry.msg_type.as_deref() == Some(EXEC_PRE_TRADE_STATE))
    else {
        return ExecStateSnapshot {
            source_id: source_id.to_string(),
            snapshot_ts_ms: snapshot.ts_ms,
            position_ready: false,
            rows: Vec::new(),
        };
    };
    let Some(state) = &entry.entry else {
        return ExecStateSnapshot {
            source_id: source_id.to_string(),
            snapshot_ts_ms: entry.ts_ms.unwrap_or(snapshot.ts_ms),
            position_ready: false,
            rows: Vec::new(),
        };
    };
    ExecStateSnapshot {
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
            .map(|row| ExecStateRowSnapshot {
                strategy_name: row.strategy_name.clone(),
                symbol: normalize_symbol(&row.symbol),
                source_updated_at_ms: row.source_updated_at_ms,
                current_qty: row.current_qty.filter(|value| value.is_finite()),
                current_usdt: row.current_usdt.filter(|value| value.is_finite()),
                target_qty: row.target_qty.filter(|value| value.is_finite()),
                pending_qty: row.pending_qty.filter(|value| value.is_finite()),
                live_order_qty: row.live_order_qty.filter(|value| value.is_finite()),
                remaining_batches: row.remaining_batches,
                estimated_completion_ts_ms: row.estimated_completion_ts_ms,
                execution_complete: row.execution_complete,
                completion_reason: row.completion_reason.clone(),
                account_position_qty: row.account_position_qty.filter(|value| value.is_finite()),
            })
            .filter(|row| !row.strategy_name.trim().is_empty() && !row.symbol.is_empty())
            .collect(),
    }
}

pub fn extract_exec_state(source_id: &str, snapshot_json: &serde_json::Value) -> ExecStateSnapshot {
    match serde_json::from_value::<VizSnapshot>(snapshot_json.clone()) {
        Ok(snapshot) => extract_exec_state_from_decoded(source_id, &snapshot),
        Err(error) => {
            warn!(
                source_id,
                error = %error,
                "Exec Viz snapshot JSON did not match expected execution state shape"
            );
            ExecStateSnapshot {
                source_id: source_id.to_string(),
                snapshot_ts_ms: 0,
                position_ready: false,
                rows: Vec::new(),
            }
        }
    }
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

    #[test]
    fn extracts_execution_fields_for_health_monitoring() {
        let snapshot = serde_json::json!({
            "ts_ms": 1000,
            "entries": [{
                "type": "exec_pre_trade_state",
                "entry": {
                    "ts_ms": 1000,
                    "position_ready": true,
                    "rows": [{
                        "strategy_name": "cta_a",
                        "symbol": "BTC-USDT",
                        "source_updated_at_ms": 990,
                        "current_qty": 0.5,
                        "current_usdt": 32500.0,
                        "target_qty": 0.8,
                        "pending_qty": 0.2,
                        "live_order_qty": 0.1,
                        "remaining_batches": 3,
                        "estimated_completion_ts_ms": 2000,
                        "execution_complete": false,
                        "completion_reason": "",
                        "account_position_qty": 0.3
                    }]
                }
            }]
        });
        let state = extract_exec_state("trade01", &snapshot);
        assert!(state.position_ready);
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].symbol, "BTCUSDT");
        assert_eq!(state.rows[0].current_usdt, Some(32_500.0));
        assert_eq!(state.rows[0].target_qty, Some(0.8));
        assert_eq!(state.rows[0].pending_qty, Some(0.2));
        assert_eq!(state.rows[0].live_order_qty, Some(0.1));
        assert_eq!(state.rows[0].remaining_batches, 3);
        assert_eq!(state.rows[0].estimated_completion_ts_ms, 2000);
    }
}
