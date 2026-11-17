//! Liquidity evidence owned and durably ACKed by Exec persist_manager.
use super::{Execution, FillLiquidity, StoredExecution};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(in crate::nav) struct RoleEvidence {
    pub(super) execution: Execution,
    pub(super) role: FillLiquidity,
}

impl RoleEvidence {
    #[cfg(test)]
    pub(in crate::nav) fn transaction_id(&self) -> &str {
        &self.execution.transaction_id
    }
    pub(super) fn validate(&self, e: &Execution) -> Result<FillLiquidity> {
        let j = &self.execution;
        ensure!(
            j.transaction_id == e.transaction_id
                && j.order_id == e.order_id
                && j.client_order_id == e.client_order_id
                && j.symbol == e.symbol
                && j.side == e.side
                && j.timestamp_ms == e.timestamp_ms
                && number(&j.quantity)? == number(&e.quantity)?
                && number(&j.price)? == number(&e.price)?,
            "RapidX durable role conflicts with persisted execution {}",
            e.transaction_id
        );
        Ok(self.role)
    }
}

fn number(value: &str) -> Result<f64> {
    let value: f64 = value.parse().context("invalid RapidX execution number")?;
    ensure!(
        value.is_finite() && value > 0.0,
        "invalid RapidX execution number"
    );
    Ok(value)
}

#[derive(serde::Deserialize)]
struct StoredRole {
    record: StoredExecution,
    role: String,
}

pub(in crate::nav) fn decode(
    key: &[u8],
    bytes: &[u8],
    portfolio: &str,
    exchange: &str,
) -> Result<Option<RoleEvidence>> {
    let stored: StoredRole = serde_json::from_slice(bytes).context("decode durable RapidX role")?;
    if stored.record.portfolio != portfolio || stored.record.exchange != exchange {
        return Ok(None);
    }
    let e = stored.record.execution;
    let mut hash = Sha256::new();
    hash.update(b"mkt_signal/rapidx_execution/stable_key");
    for part in [portfolio, exchange, &e.transaction_id] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.update([u8::from(e.rest)]);
    let expected: [u8; 32] = hash.finalize().into();
    ensure!(key == expected, "RapidX role storage identity mismatch");
    let role = match stored.role.as_str() {
        "MAKER" => FillLiquidity::Maker,
        "TAKER" => FillLiquidity::Taker,
        _ => anyhow::bail!("invalid durable RapidX execution role"),
    };
    ensure!(
        !e.transaction_id.is_empty() && !e.order_id.is_empty() && e.timestamp_ms > 0,
        "invalid role identity"
    );
    number(&e.quantity)?;
    number(&e.price)?;
    Ok(Some(RoleEvidence { execution: e, role }))
}
