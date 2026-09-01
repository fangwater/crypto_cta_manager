use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::config::SourceConfig;
use crate::redis_runtime::RedisRuntime;

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketRule {
    pub status: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub price_tick: String,
    pub qty_step: String,
    pub min_qty: String,
    pub min_notional: Option<String>,
    pub contract_multiplier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketRulesSnapshot {
    pub venue: String,
    pub fetched_at_us: i64,
    pub symbols: BTreeMap<String, MarketRule>,
}

impl MarketRulesSnapshot {
    pub fn validate(&self) -> Result<()> {
        if !matches!(
            self.venue.as_str(),
            "binance-futures" | "binance-coin-futures" | "okex-futures"
        ) {
            bail!("unsupported market-rules venue: {}", self.venue);
        }
        if self.fetched_at_us <= 0 {
            bail!("market-rules fetched_at_us must be positive");
        }
        if self.symbols.is_empty() {
            bail!("market-rules snapshot is empty for venue {}", self.venue);
        }
        for (symbol, rule) in &self.symbols {
            if symbol.is_empty() || symbol.to_uppercase() != *symbol {
                bail!("market-rules symbol is not normalized: {symbol}");
            }
            if rule.status.trim().is_empty()
                || rule.base_asset.trim().is_empty()
                || rule.quote_asset.trim().is_empty()
            {
                bail!("market-rules identity fields are empty: {symbol}");
            }
            validate_positive_decimal(symbol, "price_tick", &rule.price_tick)?;
            validate_positive_decimal(symbol, "qty_step", &rule.qty_step)?;
            validate_positive_decimal(symbol, "min_qty", &rule.min_qty)?;
            if let Some(value) = rule.min_notional.as_deref() {
                validate_positive_decimal(symbol, "min_notional", value)?;
            }
            if let Some(value) = rule.contract_multiplier.as_deref() {
                validate_positive_decimal(symbol, "contract_multiplier", value)?;
            }
        }
        Ok(())
    }
}

fn validate_positive_decimal(symbol: &str, field: &str, value: &str) -> Result<()> {
    let parsed = value
        .parse::<f64>()
        .with_context(|| format!("market-rules {field} is not a decimal: {symbol}={value}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        bail!("market-rules {field} must be positive: {symbol}={value}");
    }
    Ok(())
}

pub fn spawn(sources: Vec<SourceConfig>, redis: RedisRuntime) {
    tokio::spawn(async move {
        let client = match Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            Err(error) => {
                warn!(error = %error, "failed to build market-rules HTTP client");
                return;
            }
        };
        let mut timer = tokio::time::interval(REFRESH_INTERVAL);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            timer.tick().await;
            refresh_all(&client, &redis, &sources).await;
        }
    });
}

async fn refresh_all(client: &Client, redis: &RedisRuntime, sources: &[SourceConfig]) {
    let venues = sources
        .iter()
        .filter(|source| source.enabled)
        .map(|source| source.venue.clone())
        .collect::<BTreeSet<_>>();

    for venue in venues {
        let snapshot = match fetch_snapshot(client, &venue).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!(venue, error = %error, "market-rules refresh failed; retaining last good snapshot");
                continue;
            }
        };
        let symbol_count = snapshot.symbols.len();
        for source in sources
            .iter()
            .filter(|source| source.enabled && source.venue == venue)
        {
            match redis.publish_market_rules(source, &snapshot).await {
                Ok(()) => info!(
                    source_id = source.id,
                    venue,
                    fetched_at_us = snapshot.fetched_at_us,
                    symbols = symbol_count,
                    "market-rules snapshot published"
                ),
                Err(error) => warn!(
                    source_id = source.id,
                    venue,
                    error = %error,
                    "market-rules Redis publish failed; retaining last good snapshot"
                ),
            }
        }
    }
}

async fn fetch_snapshot(client: &Client, venue: &str) -> Result<MarketRulesSnapshot> {
    let symbols = match venue {
        "binance-futures" => fetch_binance_futures(client).await?,
        "binance-coin-futures" => fetch_binance_coin_futures(client).await?,
        "okex-futures" => fetch_okx_futures(client).await?,
        _ => bail!("unsupported market-rules venue: {venue}"),
    };
    let snapshot = MarketRulesSnapshot {
        venue: venue.to_string(),
        fetched_at_us: now_us(),
        symbols,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

async fn fetch_binance_futures(client: &Client) -> Result<BTreeMap<String, MarketRule>> {
    let value = get_json(client, "https://fapi.binance.com/fapi/v1/exchangeInfo").await?;
    parse_binance(&value, "status", false)
}

#[cfg(test)]
fn parse_binance_futures(value: &Value) -> Result<BTreeMap<String, MarketRule>> {
    parse_binance(value, "status", false)
}

async fn fetch_binance_coin_futures(client: &Client) -> Result<BTreeMap<String, MarketRule>> {
    let value = get_json(client, "https://dapi.binance.com/dapi/v1/exchangeInfo").await?;
    parse_binance(&value, "contractStatus", true)
}

fn parse_binance(
    value: &Value,
    status_field: &str,
    has_contract_size: bool,
) -> Result<BTreeMap<String, MarketRule>> {
    let rows = value
        .get("symbols")
        .and_then(Value::as_array)
        .context("Binance exchangeInfo omitted symbols")?;
    let mut symbols = BTreeMap::new();
    for row in rows {
        let symbol = required_string(row, "symbol")?.to_uppercase();
        let filters = row
            .get("filters")
            .and_then(Value::as_array)
            .with_context(|| format!("Binance exchangeInfo omitted filters: {symbol}"))?;
        let mut price_tick = None;
        let mut qty_step = None;
        let mut min_qty = None;
        let mut min_notional = None;
        for filter in filters {
            match filter.get("filterType").and_then(Value::as_str) {
                Some("PRICE_FILTER") => price_tick = json_decimal(filter, "tickSize"),
                Some("LOT_SIZE") => {
                    qty_step = json_decimal(filter, "stepSize");
                    min_qty = json_decimal(filter, "minQty");
                }
                Some("MIN_NOTIONAL") | Some("NOTIONAL") => {
                    min_notional = json_decimal(filter, "notional")
                        .or_else(|| json_decimal(filter, "minNotional"));
                }
                _ => {}
            }
        }
        let rule = MarketRule {
            status: required_string(row, status_field)?.to_string(),
            base_asset: required_string(row, "baseAsset")?.to_uppercase(),
            quote_asset: required_string(row, "quoteAsset")?.to_uppercase(),
            price_tick: price_tick
                .with_context(|| format!("Binance missing price tick: {symbol}"))?,
            qty_step: qty_step.with_context(|| format!("Binance missing qty step: {symbol}"))?,
            min_qty: min_qty.with_context(|| format!("Binance missing min qty: {symbol}"))?,
            min_notional,
            contract_multiplier: if has_contract_size {
                json_decimal(row, "contractSize")
                    .with_context(|| format!("Binance missing contract size: {symbol}"))?
                    .into()
            } else {
                Some("1".to_string())
            },
        };
        if symbols.insert(symbol.clone(), rule).is_some() {
            bail!("duplicate Binance symbol: {symbol}");
        }
    }
    Ok(symbols)
}

async fn fetch_okx_futures(client: &Client) -> Result<BTreeMap<String, MarketRule>> {
    let value = get_json(
        client,
        "https://www.okx.com/api/v5/public/instruments?instType=SWAP",
    )
    .await?;
    parse_okx_futures(&value)
}

fn parse_okx_futures(value: &Value) -> Result<BTreeMap<String, MarketRule>> {
    if value.get("code").and_then(Value::as_str) != Some("0") {
        bail!(
            "OKX instruments request failed: code={:?} msg={:?}",
            value.get("code"),
            value.get("msg")
        );
    }
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .context("OKX instruments omitted data")?;
    let mut symbols = BTreeMap::new();
    for row in rows {
        if row.get("ctType").and_then(Value::as_str) != Some("linear")
            || row.get("settleCcy").and_then(Value::as_str) != Some("USDT")
        {
            continue;
        }
        let inst_id = required_string(row, "instId")?;
        let symbol = inst_id.replace("-SWAP", "").replace('-', "").to_uppercase();
        let ct_val = required_positive_f64(row, "ctVal", &symbol)?;
        let ct_mult = optional_positive_f64(row, "ctMult", &symbol)?.unwrap_or(1.0);
        let rule = MarketRule {
            status: required_string(row, "state")?.to_string(),
            base_asset: inst_id.split('-').next().unwrap_or_default().to_uppercase(),
            quote_asset: "USDT".to_string(),
            price_tick: required_string(row, "tickSz")?.to_string(),
            qty_step: required_string(row, "lotSz")?.to_string(),
            min_qty: required_string(row, "minSz")?.to_string(),
            min_notional: None,
            contract_multiplier: Some((ct_val * ct_mult).to_string()),
        };
        if symbols.insert(symbol.clone(), rule).is_some() {
            bail!("duplicate OKX symbol: {symbol}");
        }
    }
    Ok(symbols)
}

async fn get_json(client: &Client, url: &str) -> Result<Value> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("GET {url} failed with HTTP {status}");
    }
    response
        .json::<Value>()
        .await
        .with_context(|| format!("decode JSON from {url}"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("required field is missing: {field}"))
}

fn json_decimal(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| Some(value.to_string()))
        })
        .filter(|value| !value.is_empty())
}

fn required_positive_f64(value: &Value, field: &str, symbol: &str) -> Result<f64> {
    optional_positive_f64(value, field, symbol)?
        .with_context(|| format!("required decimal is missing: {symbol}.{field}"))
}

fn optional_positive_f64(value: &Value, field: &str, symbol: &str) -> Result<Option<f64>> {
    let Some(raw) = value.get(field).and_then(Value::as_str) else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let parsed = raw
        .parse::<f64>()
        .with_context(|| format!("invalid decimal: {symbol}.{field}={raw}"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        bail!("decimal must be positive: {symbol}.{field}={raw}");
    }
    Ok(Some(parsed))
}

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binance_unicode_symbol_filters() {
        let value = serde_json::json!({
            "symbols": [{
                "symbol": "牛来USDT",
                "status": "TRADING",
                "baseAsset": "牛来",
                "quoteAsset": "USDT",
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.0001"},
                    {"filterType": "LOT_SIZE", "minQty": "1", "stepSize": "1"},
                    {"filterType": "MIN_NOTIONAL", "notional": "5"}
                ]
            }]
        });
        let symbols = parse_binance_futures(&value).unwrap();
        let rule = symbols.get("牛来USDT").unwrap();
        assert_eq!(rule.price_tick, "0.0001");
        assert_eq!(rule.qty_step, "1");
        assert_eq!(rule.min_notional.as_deref(), Some("5"));
    }

    #[test]
    fn parses_okx_linear_usdt_swap() {
        let value = serde_json::json!({
            "code": "0",
            "msg": "",
            "data": [{
                "instId": "BTC-USDT-SWAP",
                "ctType": "linear",
                "settleCcy": "USDT",
                "state": "live",
                "ctVal": "0.01",
                "ctMult": "1",
                "lotSz": "0.01",
                "minSz": "0.01",
                "tickSz": "0.1"
            }]
        });
        let symbols = parse_okx_futures(&value).unwrap();
        let rule = symbols.get("BTCUSDT").unwrap();
        assert_eq!(rule.contract_multiplier.as_deref(), Some("0.01"));
    }

    #[test]
    fn parses_binance_coin_futures_contract_size() {
        let value = serde_json::json!({
            "symbols": [{
                "symbol": "BTCUSD_PERP",
                "contractStatus": "TRADING",
                "baseAsset": "BTC",
                "quoteAsset": "USD",
                "contractSize": 100,
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.1"},
                    {"filterType": "LOT_SIZE", "minQty": "1", "stepSize": "1"}
                ]
            }]
        });
        let symbols = parse_binance(&value, "contractStatus", true).unwrap();
        assert_eq!(
            symbols["BTCUSD_PERP"].contract_multiplier.as_deref(),
            Some("100")
        );
    }

    #[test]
    fn rejects_non_positive_rule_values() {
        let snapshot = MarketRulesSnapshot {
            venue: "binance-futures".to_string(),
            fetched_at_us: 1,
            symbols: BTreeMap::from([(
                "BTCUSDT".to_string(),
                MarketRule {
                    status: "TRADING".to_string(),
                    base_asset: "BTC".to_string(),
                    quote_asset: "USDT".to_string(),
                    price_tick: "0".to_string(),
                    qty_step: "0.001".to_string(),
                    min_qty: "0.001".to_string(),
                    min_notional: Some("5".to_string()),
                    contract_multiplier: Some("1".to_string()),
                },
            )]),
        };
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn wire_contract_has_one_unversioned_shape() {
        let snapshot = MarketRulesSnapshot {
            venue: "binance-futures".to_string(),
            fetched_at_us: 1,
            symbols: BTreeMap::from([(
                "BTCUSDT".to_string(),
                MarketRule {
                    status: "TRADING".to_string(),
                    base_asset: "BTC".to_string(),
                    quote_asset: "USDT".to_string(),
                    price_tick: "0.1".to_string(),
                    qty_step: "0.001".to_string(),
                    min_qty: "0.001".to_string(),
                    min_notional: Some("5".to_string()),
                    contract_multiplier: Some("1".to_string()),
                },
            )]),
        };
        let value = serde_json::to_value(snapshot).unwrap();
        let fields = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            fields,
            BTreeSet::from([
                "fetched_at_us".to_string(),
                "symbols".to_string(),
                "venue".to_string(),
            ])
        );
    }
}
