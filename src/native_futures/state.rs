use super::records::{Order, OrderStatus};
use anyhow::{Result, ensure};
use serde::Serialize;

/// Observation only. It never advances execution state or releases a reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderPhase {
    Working,
    PartiallyFilled,
    /// Terminal at the exchange while its factual fills are still arriving.
    Settling,
    Filled,
    Cancelled,
    Rejected,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservedOrder {
    pub phase: OrderPhase,
    pub exchange_terminal: bool,
    pub facts_complete: bool,
    /// Lots joined from the order's factual trade updates.
    pub filled_lots: i64,
    pub order: Order,
}

/// Current order state: its latest lifecycle record joined with the factual
/// fills of the same `client_order_id`.
pub fn observe(order: Order, filled_lots: i64) -> Result<ObservedOrder> {
    ensure!(
        (0..=order.lots).contains(&filled_lots),
        "factual fills exceed native order lots"
    );
    let exchange_terminal = order.status.terminal();
    let facts_complete = match order.status {
        OrderStatus::Filled => filled_lots == order.lots,
        // Cancellation and rejection carry no fill count; later fills of a
        // terminal order cannot exist once it is terminal at the exchange.
        OrderStatus::Canceled | OrderStatus::Expired => true,
        OrderStatus::PartiallyFilled => filled_lots > 0,
        OrderStatus::New => true,
    };
    let phase = match order.status {
        _ if exchange_terminal && !facts_complete => OrderPhase::Settling,
        OrderStatus::Filled => OrderPhase::Filled,
        OrderStatus::Canceled => OrderPhase::Cancelled,
        OrderStatus::Expired => OrderPhase::Rejected,
        OrderStatus::PartiallyFilled => OrderPhase::PartiallyFilled,
        OrderStatus::New if filled_lots > 0 => OrderPhase::PartiallyFilled,
        OrderStatus::New => OrderPhase::Working,
    };
    Ok(ObservedOrder {
        phase,
        exchange_terminal,
        facts_complete,
        filled_lots,
        order,
    })
}
