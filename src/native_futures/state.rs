use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::Value;

/// Observation only. It never advances execution state or releases a reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderPhase {
    Prepared,
    Pending,
    Working,
    PartiallyFilled,
    CancelPending,
    Settling,
    Filled,
    Cancelled,
    Rejected,
    Uncertain,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservedOrder {
    pub phase: OrderPhase,
    pub exchange_terminal: bool,
    pub facts_complete: bool,
    pub record: Value,
}

pub fn observe(record: Value) -> Result<ObservedOrder> {
    let order = &record["order"];
    let number = |key: &str| {
        order[key]
            .as_i64()
            .with_context(|| format!("missing order {key}"))
    };
    let lots = order["request"]["lots"]
        .as_i64()
        .context("missing order lots")?;
    let filled = number("filled")?;
    let reported = number("reported_filled")?;
    let cancelled = number("cancelled")?;
    ensure!(
        lots > 0
            && filled >= 0
            && reported >= 0
            && cancelled >= 0
            && reported <= lots
            && filled + cancelled <= lots,
        "inconsistent native order quantities"
    );
    let status = order["status"]
        .as_str()
        .context("missing native order status")?;
    ensure!(
        matches!(
            status,
            "pending" | "working" | "filled" | "cancelled" | "rejected"
        ),
        "unknown native order status"
    );
    let send = record["send_state"]
        .as_str()
        .context("missing native send state")?;
    ensure!(
        matches!(
            send,
            "prepared" | "submitted_locally" | "rejected_locally" | "uncertain" | "observed"
        ),
        "unknown native send state"
    );
    let exchange_terminal =
        matches!(status, "filled" | "cancelled" | "rejected") && send == "observed";
    let facts_complete = filled == reported;
    let phase = if send == "uncertain" {
        OrderPhase::Uncertain
    } else if matches!(status, "filled" | "cancelled" | "rejected") && !facts_complete {
        OrderPhase::Settling
    } else if status == "filled" {
        ensure!(
            filled == lots,
            "filled terminal without complete factual fills"
        );
        OrderPhase::Filled
    } else if status == "cancelled" {
        OrderPhase::Cancelled
    } else if status == "rejected" || send == "rejected_locally" {
        OrderPhase::Rejected
    } else if order["cancel_pending"]
        .as_bool()
        .context("missing cancel_pending")?
    {
        OrderPhase::CancelPending
    } else if filled > 0 {
        OrderPhase::PartiallyFilled
    } else if send == "prepared" {
        OrderPhase::Prepared
    } else if status == "working" {
        OrderPhase::Working
    } else {
        OrderPhase::Pending
    };
    Ok(ObservedOrder {
        phase,
        exchange_terminal,
        facts_complete,
        record,
    })
}
