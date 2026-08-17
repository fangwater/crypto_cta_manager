use serde::Serialize;

use crate::account_ipc::LiveEquitySnapshot;
use crate::strategy_catalog::{AccountStudio, DEFAULT_POSITION_EQUITY_USDT};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountCapacityView {
    pub live: Option<LiveEquityView>,
    pub leverage: f64,
    pub share_unit_usdt: f64,
    pub buying_power_usdt: Option<f64>,
    pub configurable_shares: Option<f64>,
    pub bound_shares: f64,
    pub remaining_shares: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveEquityView {
    pub status: &'static str,
    pub source: String,
    pub equity_usdt: f64,
    pub wallet_balance_usdt: f64,
    pub unrealized_pnl_usdt: f64,
    pub available_balance_usdt: f64,
    pub ts_ms: i64,
    pub age_ms: i64,
}

const STALE_AFTER_MS: i64 = 45_000;

pub fn live_equity_view(snapshot: &LiveEquitySnapshot, now_ms: i64) -> LiveEquityView {
    let age_ms = now_ms.saturating_sub(snapshot.ts_ms).max(0);
    LiveEquityView {
        status: if age_ms > STALE_AFTER_MS {
            "stale"
        } else {
            "ok"
        },
        source: snapshot.source.clone(),
        equity_usdt: snapshot.equity_usdt,
        wallet_balance_usdt: snapshot.wallet_balance_usdt,
        unrealized_pnl_usdt: snapshot.unrealized_pnl_usdt,
        available_balance_usdt: snapshot.available_balance_usdt,
        ts_ms: snapshot.ts_ms,
        age_ms,
    }
}

pub fn buying_power_usdt(equity_usdt: f64, leverage: f64) -> Option<f64> {
    if equity_usdt.is_finite() && equity_usdt > 0.0 && leverage.is_finite() && leverage > 0.0 {
        Some(equity_usdt * leverage)
    } else {
        None
    }
}

pub fn shares(notional_usdt: f64, share_unit_usdt: f64) -> Option<f64> {
    if share_unit_usdt.is_finite() && share_unit_usdt > 0.0 && notional_usdt.is_finite() {
        Some(notional_usdt / share_unit_usdt)
    } else {
        None
    }
}

pub fn capacity_view(
    studio: &AccountStudio,
    live: Option<&LiveEquitySnapshot>,
    share_unit_usdt: f64,
    now_ms: i64,
) -> AccountCapacityView {
    let unit = if share_unit_usdt.is_finite() && share_unit_usdt > 0.0 {
        share_unit_usdt
    } else {
        DEFAULT_POSITION_EQUITY_USDT
    };
    let live_view = live.map(|snapshot| live_equity_view(snapshot, now_ms));
    let buying_power_usdt = live_view
        .as_ref()
        .and_then(|view| buying_power_usdt(view.equity_usdt, studio.leverage));
    let configurable_shares = buying_power_usdt.and_then(|value| shares(value, unit));
    let bound_shares = shares(studio.bound_equity_usdt, unit).unwrap_or(0.0);
    let remaining_shares = configurable_shares.map(|value| value - bound_shares);
    AccountCapacityView {
        live: live_view,
        leverage: studio.leverage,
        share_unit_usdt: unit,
        buying_power_usdt,
        configurable_shares,
        bound_shares,
        remaining_shares,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy_catalog::AccountBinding;

    #[test]
    fn two_times_leverage_doubles_available_shares() {
        let studio = AccountStudio::from_parts(
            "binance_exec_trade01".into(),
            2.0,
            1,
            vec![AccountBinding {
                source_id: "binance_exec_trade01".into(),
                binding_name: "cta_a".into(),
                position_strategy_name: "cta_a".into(),
                order_strategy_name: "default_order".into(),
                shares: 1.0,
                position_equity_usdt: 10_000.0,
                allocation_ratio: 0.0,
                updated_at_us: 1,
            }],
        )
        .unwrap();
        let live = LiveEquitySnapshot {
            source: "binance_std_um_wallet".into(),
            equity_usdt: 25_000.0,
            wallet_balance_usdt: 24_000.0,
            unrealized_pnl_usdt: 1_000.0,
            available_balance_usdt: 20_000.0,
            ts_ms: 1_000,
        };
        let view = capacity_view(&studio, Some(&live), 10_000.0, 1_000);
        assert_eq!(view.live.as_ref().unwrap().status, "ok");
        assert!((view.buying_power_usdt.unwrap() - 50_000.0).abs() < 1e-9);
        assert!((view.configurable_shares.unwrap() - 5.0).abs() < 1e-9);
        assert!((view.bound_shares - 1.0).abs() < 1e-9);
        assert!((view.remaining_shares.unwrap() - 4.0).abs() < 1e-9);
    }
}
