//! Investor-specific rate observations supplied by market-replay. Never query
//! the broker from Manager or use a newer observation to price older fills.
use super::reference::{self, Fee};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub api_url: String,
    /// Stable source_id -> accountKey maintained by the reference service.
    pub accounts: BTreeMap<String, String>,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        let url = Url::parse(&self.api_url).context("invalid reference service URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && matches!(url.path(), "" | "/"),
            "reference api_url must be an HTTP origin without credentials, query or path"
        );
        ensure!(
            !self.accounts.is_empty()
                && self.accounts.values().all(|a| !a.is_empty()
                    && a.len() <= 128
                    && a.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))),
            "reference accounts require explicit nonempty account aliases"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct AccountRates {
    pub account_key: String,
    pub venue: String,
    pub symbol: String,
    pub observed_at: String,
    pub observed_at_us: i64,
    pub trading_day: String,
    pub contract_multiplier: f64,
    pub price_tick: f64,
    pub active: bool,
    pub fees: BTreeMap<String, Fee>,
    pub long_margin_rate: f64,
    pub short_margin_rate: f64,
    pub long_margin_per_lot: f64,
    pub short_margin_per_lot: f64,
    pub record_hash: String,
    pub receipt_sha256: String,
}
impl AccountRates {
    pub fn estimated_fee(&self, offset: &str, lots: i64, price: f64, at_us: i64) -> Option<f64> {
        if at_us < self.observed_at_us || lots <= 0 || !price.is_finite() || price <= 0.0 {
            return None;
        }
        if offset == "close" {
            let ordinary = self.fees.get("close")?;
            let today = self.fees.get("close_today")?;
            // A generic Close report does not declare its today/yesterday
            // allocation. Distinct rates cannot be resolved from FIFO choice.
            if ordinary.per_lot != today.per_lot || ordinary.notional_rate != today.notional_rate {
                return None;
            }
        }
        let amount = self
            .fees
            .get(offset)?
            .amount(lots, price, self.contract_multiplier);
        amount.is_finite().then_some(amount)
    }
}

pub struct Feed {
    settings: Settings,
    client: Client,
    permits: Semaphore,
}
impl Feed {
    pub fn new(settings: Settings) -> Result<Self> {
        settings.validate()?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            settings,
            client,
            permits: Semaphore::new(4),
        })
    }
    pub async fn rates(
        &self,
        source: &str,
        venue: &str,
        symbol: &str,
        as_of_us: i64,
    ) -> Result<Option<AccountRates>> {
        super::validate_market(&format!("{venue}:{symbol}"))?;
        let account = self
            .settings
            .accounts
            .get(source)
            .context("source has no explicit reference account mapping")?;
        let as_of = chrono::DateTime::from_timestamp_micros(as_of_us)
            .context("invalid rate observation query timestamp")?
            .to_rfc3339();
        let _permit = self.permits.acquire().await?;
        let url = Url::parse(&self.settings.api_url)?.join("api/v1/account-rates")?;
        let mut response = self
            .client
            .get(url)
            .query(&[
                ("account", account.as_str()),
                ("venue", venue),
                ("instrument", symbol),
                ("asOf", &as_of),
            ])
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 512 * 1024,
                "reference account-rate response exceeds budget"
            );
            bytes.extend_from_slice(&chunk);
        }
        parse(
            &serde_json::from_slice(&bytes)?,
            account,
            venue,
            symbol,
            as_of_us,
        )
    }

    /// One causal opening observation plus all changes inside the requested
    /// window; avoids one HTTP request for every fill.
    pub async fn history(
        &self,
        source: &str,
        venue: &str,
        symbol: &str,
        start_us: i64,
        end_us: i64,
    ) -> Result<Vec<AccountRates>> {
        ensure!(
            start_us <= end_us && end_us - start_us <= 366 * 86400 * 1_000_000i64,
            "invalid account rate history window"
        );
        let first = self.rates(source, venue, symbol, start_us).await?;
        let mut observations = BTreeMap::new();
        if let Some(rate) = first {
            observations.insert(rate.observed_at_us, rate);
        }
        if start_us == end_us {
            return Ok(observations.into_values().collect());
        }
        let account = self
            .settings
            .accounts
            .get(source)
            .context("missing source account alias")?;
        let start = chrono::DateTime::from_timestamp_micros(start_us)
            .context("invalid fee history start")?
            .to_rfc3339();
        let end = chrono::DateTime::from_timestamp_micros(end_us)
            .context("invalid fee history end")?
            .to_rfc3339();
        let _permit = self.permits.acquire().await?;
        let url = Url::parse(&self.settings.api_url)?.join("api/v1/account-rates")?;
        let mut response = self
            .client
            .get(url)
            .query(&[
                ("account", account.as_str()),
                ("venue", venue),
                ("instrument", symbol),
                ("start", start.as_str()),
                ("end", end.as_str()),
            ])
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 16 * 1024 * 1024,
                "account rate history response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            text(&value, "sourceType")? == "CTP_ACCOUNT_QUERY"
                && text(&value, "accountKey")? == account
                && text(&value, "mode")? == "observations"
                && value["asOf"].is_null(),
            "wrong account fee history identity"
        );
        let rows = value["rates"]
            .as_array()
            .context("missing account fee history")?;
        ensure!(
            rows.len() <= 10000 && value["count"].as_u64() == Some(rows.len() as u64),
            "account fee history count exceeds budget"
        );
        for row in rows {
            let rate = parse_record(row, account, venue, symbol, end_us)?;
            ensure!(
                rate.observed_at_us >= start_us,
                "fee history predates requested window"
            );
            if let Some(old) = observations.get(&rate.observed_at_us) {
                ensure!(
                    serde_json::to_value(old)? == serde_json::to_value(&rate)?,
                    "conflicting account fee observations"
                );
            } else {
                observations.insert(rate.observed_at_us, rate);
            }
        }
        Ok(observations.into_values().collect())
    }
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("reference account rates missing {key}"))
}
fn decimal(value: &Value) -> Result<f64> {
    let number = match value {
        Value::String(s) => s.parse().context("invalid reference decimal")?,
        _ => value.as_f64().context("missing reference decimal")?,
    };
    ensure!(number.is_finite(), "nonfinite reference decimal");
    Ok(number)
}
fn hash(value: &Value, key: &str) -> Result<String> {
    let h = text(value, key)?;
    ensure!(
        h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid reference receipt/hash"
    );
    Ok(h.to_owned())
}
fn parse(
    value: &Value,
    account: &str,
    venue: &str,
    symbol: &str,
    as_of_us: i64,
) -> Result<Option<AccountRates>> {
    ensure!(
        text(value, "sourceType")? == "CTP_ACCOUNT_QUERY"
            && text(value, "accountKey")? == account
            && text(value, "mode")? == "as_of"
            && reference::timestamp(text(value, "asOf")?)? == as_of_us,
        "reference service returned another account, source or query boundary"
    );
    let rows = value["rates"]
        .as_array()
        .context("missing reference rates array")?;
    ensure!(
        rows.len() <= 1 && value["count"].as_u64() == Some(rows.len() as u64),
        "unexpected reference rate result count"
    );
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    parse_record(row, account, venue, symbol, as_of_us).map(Some)
}
fn parse_record(
    row: &Value,
    account: &str,
    venue: &str,
    symbol: &str,
    as_of_us: i64,
) -> Result<AccountRates> {
    ensure!(
        text(row, "accountKey")? == account
            && text(row, "venue")? == venue
            && text(row, "instrumentId")? == symbol
            && text(row, "sourceType")? == "CTP_ACCOUNT_QUERY",
        "reference rate identity mismatch"
    );
    let observed_at = text(row, "observedAt")?;
    let observed_at_us = reference::timestamp(observed_at)?;
    ensure!(
        observed_at_us <= as_of_us,
        "future account-rate observation cannot price historical fills"
    );
    let trading_day = text(row, "tradingDay")?;
    reference::valid_day(trading_day)?;
    let multiplier = decimal(&row["contractMultiplier"])?;
    let tick = decimal(&row["priceTick"])?;
    ensure!(
        multiplier > 0.0 && tick > 0.0,
        "invalid account-rate multiplier or tick"
    );
    let mut fees = BTreeMap::new();
    for (key, native) in [
        ("open", "open"),
        ("close", "close"),
        ("close_today", "closeToday"),
        ("close_yesterday", "close"),
    ] {
        let raw = &row["fees"][native];
        fees.insert(
            key.to_owned(),
            Fee {
                per_lot: decimal(&raw["perLot"])?,
                notional_rate: decimal(&raw["rate"])?,
            },
        );
    }
    let margin = &row["margin"];
    let result = AccountRates {
        account_key: account.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        observed_at: observed_at.into(),
        observed_at_us,
        trading_day: trading_day.into(),
        contract_multiplier: multiplier,
        price_tick: tick,
        active: row["active"]
            .as_bool()
            .context("missing rate contract status")?,
        fees,
        long_margin_rate: decimal(&margin["longRate"])?,
        short_margin_rate: decimal(&margin["shortRate"])?,
        long_margin_per_lot: decimal(&margin["longPerLot"])?,
        short_margin_per_lot: decimal(&margin["shortPerLot"])?,
        record_hash: hash(row, "recordHash")?,
        receipt_sha256: hash(row, "receiptSha256")?,
    };
    Ok(result)
}

#[cfg(test)]
mod tests;
