//! Broker account funds, relayed read-only from the cloud Exec Viz through the
//! authenticated sync exporter. Funds are factual broker values: Manager never
//! estimates equity or margin. Daily rows are keyed by the native trading day,
//! so a night session belongs to the following trading day, and the previous
//! day's equity is the broker's settled `pre_balance`.
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row, types::Json};

/// CTP queries funds every five seconds; older answers are shown as historical.
pub const FRESH_US: i64 = 60_000_000;
/// Trading days covered by the drawdown window.
pub const DRAWDOWN_DAYS: usize = 15;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BrokerFunds {
    pub observed_us: i64,
    pub trading_day: String,
    pub currency: String,
    /// Settled equity of the previous trading day.
    pub pre_balance: f64,
    pub deposit: f64,
    pub withdraw: f64,
    pub balance: f64,
    pub available: f64,
    pub withdraw_quota: f64,
    pub curr_margin: f64,
    pub frozen_margin: f64,
    pub frozen_cash: f64,
    pub frozen_commission: f64,
    pub position_profit: f64,
    pub close_profit: f64,
    pub commission: f64,
}

impl BrokerFunds {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.observed_us > 0, "broker funds query time is missing");
        super::reference::valid_day(&self.trading_day)?;
        ensure!(self.currency == "CNY", "unsupported broker funds currency");
        ensure!(
            [
                self.pre_balance,
                self.deposit,
                self.withdraw,
                self.balance,
                self.available,
                self.withdraw_quota,
                self.curr_margin,
                self.frozen_margin,
                self.frozen_cash,
                self.frozen_commission,
                self.position_profit,
                self.close_profit,
                self.commission,
            ]
            .iter()
            .all(|value| value.is_finite()),
            "nonfinite broker funds"
        );
        Ok(())
    }
    /// Trading-day profit; deposits and withdrawals are not profit.
    pub fn profit(&self) -> f64 {
        self.balance - self.pre_balance - self.deposit + self.withdraw
    }
    pub fn profit_percent(&self) -> Option<f64> {
        (self.pre_balance > 0.0).then(|| self.profit() / self.pre_balance * 100.0)
    }
    pub fn margin_percent(&self) -> Option<f64> {
        (self.balance > 0.0).then(|| self.curr_margin / self.balance * 100.0)
    }
    /// Gross notional over positive equity; unknown inputs stay unknown.
    pub fn leverage(&self, notional: Option<f64>) -> Option<f64> {
        notional
            .filter(|n| n.is_finite())
            .filter(|_| self.balance > 0.0)
            .map(|n| n / self.balance)
    }
}

/// The Viz relay for one Manager source. Unknown additional fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Observation {
    pub manager_source_id: String,
    pub adapter: String,
    pub error: Option<String>,
    pub observed_us: Option<i64>,
    pub runtime_status: Option<String>,
    pub connected: Option<bool>,
    pub transport_ready: Option<bool>,
    pub account: Option<BrokerFunds>,
}

impl Observation {
    pub fn validate(&self, source_id: &str, adapter: &str) -> Result<()> {
        ensure!(
            self.manager_source_id == source_id && self.adapter == adapter,
            "broker funds belong to another source or adapter"
        );
        if let Some(account) = &self.account {
            account.validate()?;
            ensure!(
                self.observed_us
                    .is_some_and(|captured| account.observed_us <= captured),
                "broker funds query is newer than its capture"
            );
        }
        Ok(())
    }
    /// `None` when the funds are a current answer of a running, connected counter.
    pub fn historical_reason(&self, now_us: i64) -> Option<&'static str> {
        let account = self.account.as_ref()?;
        if self.error.is_some() || self.runtime_status.as_deref() != Some("running") {
            Some("柜台进程未运行，显示最近一次资金查询")
        } else if self.connected != Some(true) || self.transport_ready != Some(true) {
            Some("柜台连接中断，显示最近一次资金查询")
        } else if account.observed_us > now_us || now_us - account.observed_us > FRESH_US {
            Some("资金查询超过 60 秒未更新")
        } else {
            None
        }
    }
}

/// The latest broker observation of one trading day, with the gross position
/// notionals Manager replayed at about the same time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DailyEquity {
    pub source_id: String,
    pub funds: BrokerFunds,
    pub long_notional: Option<f64>,
    pub short_notional: Option<f64>,
    pub positions_at_us: Option<i64>,
}

pub fn day(trading_day: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(trading_day, "%Y%m%d").ok()
}

fn next_weekday(day: NaiveDate) -> NaiveDate {
    let mut next = day + Duration::days(1);
    while matches!(next.weekday(), Weekday::Sat | Weekday::Sun) {
        next += Duration::days(1);
    }
    next
}

/// Maximum drawdown (percent, positive) of the cash-flow-adjusted equity index
/// over the last fifteen trading days ending with the final row. Daily returns
/// are profit over the broker's settled previous equity. There is no trading
/// calendar here, so any missing weekday, including an exchange holiday, leaves
/// the drawdown unknown rather than chaining across an unobserved day.
pub fn drawdown(rows: &[DailyEquity]) -> Option<f64> {
    let window = rows.get(rows.len().checked_sub(DRAWDOWN_DAYS)?..)?;
    let mut index = 1.0_f64;
    let mut peak = index;
    let mut worst = 0.0_f64;
    for (i, row) in window.iter().enumerate() {
        if i > 0 {
            let previous = day(&window[i - 1].funds.trading_day)?;
            if day(&row.funds.trading_day)? != next_weekday(previous) {
                return None;
            }
        }
        // Validated rows are finite; a non-positive base has no return.
        if row.funds.pre_balance <= 0.0 {
            return None;
        }
        index *= 1.0 + row.funds.profit() / row.funds.pre_balance;
        if !index.is_finite() {
            return None;
        }
        peak = peak.max(index);
        worst = worst.max((peak - index) / peak);
    }
    Some(worst * 100.0)
}

/// Explicit schema maintenance creates the table; startup never runs DDL.
pub mod store {
    use super::*;

    const MISSING: &str =
        "native equity table unavailable; apply scripts/apply_native_equity_schema.sql";

    /// Keeps the latest observation per trading day. Rows without a position
    /// replay retain the notionals of the earlier observation.
    pub async fn upsert(pool: &PgPool, row: &DailyEquity) -> Result<()> {
        row.funds.validate()?;
        sqlx::query(
            "INSERT INTO cn_futures_account_equity_daily
               (source_id,trading_day,observed_us,funds,long_notional,short_notional,positions_at_us)
             VALUES($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (source_id,trading_day) DO UPDATE SET
               observed_us=excluded.observed_us, funds=excluded.funds,
               long_notional=CASE WHEN excluded.positions_at_us IS NULL
                 THEN cn_futures_account_equity_daily.long_notional ELSE excluded.long_notional END,
               short_notional=CASE WHEN excluded.positions_at_us IS NULL
                 THEN cn_futures_account_equity_daily.short_notional ELSE excluded.short_notional END,
               positions_at_us=COALESCE(excluded.positions_at_us,
                 cn_futures_account_equity_daily.positions_at_us),
               updated_at=now()
             WHERE excluded.observed_us >= cn_futures_account_equity_daily.observed_us",
        )
        .bind(&row.source_id)
        .bind(&row.funds.trading_day)
        .bind(row.funds.observed_us)
        .bind(Json(&row.funds))
        .bind(row.long_notional)
        .bind(row.short_notional)
        .bind(row.positions_at_us)
        .execute(pool)
        .await
        .context(MISSING)?;
        Ok(())
    }

    /// Ascending by trading day.
    pub async fn list(pool: &PgPool, source_id: &str) -> Result<Vec<DailyEquity>> {
        let rows = sqlx::query(
            "SELECT funds,long_notional,short_notional,positions_at_us
             FROM cn_futures_account_equity_daily WHERE source_id=$1
             ORDER BY trading_day DESC LIMIT 400",
        )
        .bind(source_id)
        .fetch_all(pool)
        .await
        .context(MISSING)?;
        let mut out = rows
            .into_iter()
            .map(|row| {
                let Json(funds): Json<BrokerFunds> = row.try_get("funds")?;
                funds.validate()?;
                Ok(DailyEquity {
                    source_id: source_id.into(),
                    funds,
                    long_notional: row.try_get("long_notional")?,
                    short_notional: row.try_get("short_notional")?,
                    positions_at_us: row.try_get("positions_at_us")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        out.reverse();
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
