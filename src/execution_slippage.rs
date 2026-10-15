//! Same-window NAV deviation against the minute-VWAP theoretical portfolio.
use crate::nav::NavTotals;
use crate::theoretical_nav::TheoreticalNavPoint;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ExecutionSlippage {
    pub start_ts_us: i64,
    pub end_ts_us: Option<i64>,
    pub volume_quote: f64,
    pub before_fee_gap_quote: Option<f64>,
    pub after_fee_gap_quote: Option<f64>,
    pub before_fee_bps: Option<f64>,
    pub after_fee_bps: Option<f64>,
    pub unavailable_reason: Option<String>,
}

impl ExecutionSlippage {
    pub fn unavailable(start_ts_us: i64, reason: impl Into<String>) -> Self {
        Self {
            start_ts_us,
            unavailable_reason: Some(reason.into()),
            ..Default::default()
        }
    }
    pub fn calculate(
        start_ts_us: i64,
        actual: NavTotals,
        theoretical: TheoreticalNavPoint,
    ) -> Self {
        let before = theoretical.nav_change_before_fee_quote - actual.nav_change_before_fee_quote;
        let after = theoretical.nav_change_after_fee_quote - actual.nav_change_after_fee_quote;
        let valid = actual.volume_quote.is_finite() && actual.volume_quote > 0.0;
        Self {
            start_ts_us,
            end_ts_us: Some(theoretical.ts_us),
            volume_quote: actual.volume_quote,
            before_fee_gap_quote: Some(before),
            after_fee_gap_quote: Some(after),
            before_fee_bps: valid
                .then(|| before / actual.volume_quote * 10_000.0)
                .filter(|v| v.is_finite()),
            after_fee_bps: valid
                .then(|| after / actual.volume_quote * 10_000.0)
                .filter(|v| v.is_finite()),
            unavailable_reason: (!valid).then(|| "比较区间没有实际成交额".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fees_and_rebates_use_the_same_turnover_and_loss_sign() {
        let actual = NavTotals {
            volume_quote: 100_000.0,
            nav_change_before_fee_quote: 10.0,
            nav_change_after_fee_quote: -30.0,
            ..Default::default()
        };
        let theory = TheoreticalNavPoint {
            ts_us: 2,
            nav_change_before_fee_quote: 30.0,
            nav_change_after_fee_quote: 10.0,
            ..Default::default()
        };
        let result = ExecutionSlippage::calculate(1, actual, theory);
        assert_eq!(result.before_fee_bps, Some(2.0));
        assert_eq!(result.after_fee_bps, Some(4.0));
        let rebated = NavTotals {
            nav_change_after_fee_quote: 50.0,
            ..actual
        };
        assert_eq!(
            ExecutionSlippage::calculate(1, rebated, theory).after_fee_bps,
            Some(-4.0)
        );
        assert_eq!(
            ExecutionSlippage::calculate(1, NavTotals::default(), theory).before_fee_bps,
            None
        );
    }
}
