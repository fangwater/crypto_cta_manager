use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::Value;
use sha2::Sha256;

use super::MarketRule;

const DEFAULT_REST_URL: &str = "https://api.liquiditytech.com";
const SYM_INFO_PATH: &str = "/api/v1/trading/sym/info";

pub(super) async fn fetch(
    client: &Client,
    venue: &str,
    values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, MarketRule>> {
    let api_key = required_value(values, "LTP_API_KEY")?;
    let secret = required_value(values, "LTP_API_SECRET")?;
    let base_url = values
        .get("LTP_REST_URL")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_REST_URL)
        .trim_end_matches('/');
    let url = format!("{base_url}{SYM_INFO_PATH}");
    let nonce = unix_seconds()?;
    let signature = signature(secret, nonce)?;
    let ts = unix_microseconds()?;

    let response = client
        .get(&url)
        .header("X-MBX-APIKEY", api_key)
        .header("nonce", nonce.to_string())
        .header("signature", signature)
        .header("ts", ts.to_string())
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("GET RapidX sym info")?;
    let status = response.status();
    if !status.is_success() {
        bail!("RapidX sym info returned HTTP {status}");
    }
    let response = response
        .json::<SymInfoResponse>()
        .await
        .context("decode RapidX sym info JSON")?;
    ensure_success_code(&response.code)?;
    parse_data(&response.data, venue)
}

#[cfg(test)]
fn parse(value: &Value, venue: &str) -> Result<BTreeMap<String, MarketRule>> {
    ensure_success_code(value.get("code").context("RapidX sym info omitted code")?)?;
    parse_data(
        value.get("data").context("RapidX sym info omitted data")?,
        venue,
    )
}

fn parse_data(value: &Value, venue: &str) -> Result<BTreeMap<String, MarketRule>> {
    let target = target_venue(venue)?;
    let rows = value
        .as_object()
        .context("RapidX sym info data is not a symbol map")?;
    let mut rules = BTreeMap::new();
    let mut target_rows = 0usize;

    for (map_symbol, row) in rows {
        let map_parsed = parse_symbol(map_symbol);
        if map_parsed.is_none() && claims_target_perp(map_symbol, &target) {
            bail!("malformed RapidX target symbol map key: {map_symbol}");
        }
        let row_sym = row.get("sym").and_then(Value::as_str);
        if !is_target(map_parsed.as_ref(), &target) {
            if row_sym.is_some_and(|sym| claims_target_perp(sym, &target)) {
                bail!("RapidX target row sym does not match unrelated map key: {map_symbol}");
            }
            continue;
        }
        let parsed = map_parsed.expect("target symbol was checked above");
        target_rows += 1;
        let sym = required_string(row, "sym")?;
        if sym != map_symbol {
            bail!("RapidX symbol map key does not match row sym: {map_symbol} != {sym}");
        }
        let original_symbol = required_string(row, "originalSymbol")?;
        let expected_original = target.native_symbol(&parsed);
        if original_symbol != expected_original {
            bail!(
                "RapidX originalSymbol is incoherent for {sym}: expected {expected_original}, got {original_symbol}"
            );
        }
        let state = required_string(row, "state")?;
        let status = target.status(state, sym)?;
        let contract_size = positive_decimal(row, "contractSize", sym)?;
        if target.exchange == "BINANCE"
            && decimal_value(&contract_size, "contractSize", sym)? != 1.0
        {
            bail!("RapidX Binance PERP contractSize must be 1 for {sym}, got {contract_size}");
        }
        let min_notional = optional_nonnegative_decimal(row, "minNotional", sym)?;
        let symbol = format!("{}{}", parsed.base, parsed.quote).to_uppercase();
        let rule = MarketRule {
            status,
            base_asset: parsed.base.to_string(),
            quote_asset: parsed.quote.to_string(),
            price_tick: positive_decimal(row, "tickSize", sym)?,
            qty_step: positive_decimal(row, "lotSize", sym)?,
            min_qty: positive_decimal(row, "minSize", sym)?,
            min_notional,
            contract_multiplier: Some(contract_size),
        };
        if rules.insert(symbol.clone(), rule).is_some() {
            bail!("duplicate RapidX target symbol: {symbol}");
        }
    }

    if target_rows == 0 || rules.is_empty() {
        bail!("RapidX sym info contained no target {venue} PERP USDT records");
    }
    Ok(rules)
}

fn is_target(symbol: Option<&ParsedSymbol<'_>>, target: &TargetVenue) -> bool {
    symbol.is_some_and(|symbol| {
        symbol.exchange == target.exchange && symbol.kind == "PERP" && symbol.quote == "USDT"
    })
}

fn claims_target_perp(sym: &str, target: &TargetVenue) -> bool {
    sym.strip_prefix(target.exchange)
        .is_some_and(|suffix| suffix.starts_with("_PERP"))
}

struct ParsedSymbol<'a> {
    exchange: &'a str,
    kind: &'a str,
    base: &'a str,
    quote: &'a str,
}

fn parse_symbol(sym: &str) -> Option<ParsedSymbol<'_>> {
    let mut parts = sym.split('_');
    let exchange = parts.next()?;
    let kind = parts.next()?;
    let base = parts.next()?;
    let quote = parts.next()?;
    if parts.next().is_some()
        || !matches!(exchange, "BINANCE" | "OKX")
        || base.is_empty()
        || quote.is_empty()
    {
        return None;
    }
    Some(ParsedSymbol {
        exchange,
        kind,
        base,
        quote,
    })
}

struct TargetVenue {
    exchange: &'static str,
}

impl TargetVenue {
    fn native_symbol(&self, symbol: &ParsedSymbol<'_>) -> String {
        match self.exchange {
            "BINANCE" => format!("{}{}", symbol.base, symbol.quote),
            "OKX" => format!("{}-{}-SWAP", symbol.base, symbol.quote),
            _ => unreachable!("TargetVenue only contains supported exchanges"),
        }
    }

    fn status(&self, state: &str, sym: &str) -> Result<String> {
        match (self.exchange, state) {
            ("BINANCE", "live") => Ok("TRADING".to_string()),
            ("BINANCE", "suspend") => Ok("SUSPEND".to_string()),
            ("OKX", "live") | ("OKX", "suspend") => Ok(state.to_string()),
            _ => bail!("unknown RapidX state for {sym}: {state}"),
        }
    }
}

fn target_venue(venue: &str) -> Result<TargetVenue> {
    match venue {
        "binance-futures" => Ok(TargetVenue {
            exchange: "BINANCE",
        }),
        "okex-futures" => Ok(TargetVenue { exchange: "OKX" }),
        _ => bail!("unsupported RapidX market-rules venue: {venue}"),
    }
}

fn required_value<'a>(values: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    values
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("RapidX {name} is required"))
}

fn required_string<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("RapidX required field is missing: {field}"))
}

fn positive_decimal(row: &Value, field: &str, sym: &str) -> Result<String> {
    let raw = decimal_string(row, field, sym)?;
    validate_positive_decimal(&raw, field, sym)?;
    Ok(raw)
}

fn optional_nonnegative_decimal(row: &Value, field: &str, sym: &str) -> Result<Option<String>> {
    let raw = decimal_string(row, field, sym)?;
    let parsed = raw
        .parse::<f64>()
        .with_context(|| format!("RapidX invalid decimal {sym}.{field}={raw}"))?;
    if !parsed.is_finite() || parsed < 0.0 {
        bail!("RapidX {sym}.{field} must be non-negative: {raw}");
    }
    Ok((parsed > 0.0).then_some(raw))
}

fn decimal_string(row: &Value, field: &str, sym: &str) -> Result<String> {
    let value = row
        .get(field)
        .with_context(|| format!("RapidX required field is missing: {sym}.{field}"))?;
    let raw = value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_number().map(ToString::to_string))
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("RapidX decimal field is invalid: {sym}.{field}"))?;
    Ok(raw)
}

fn validate_positive_decimal(raw: &str, field: &str, sym: &str) -> Result<()> {
    let parsed = decimal_value(raw, field, sym)?;
    if !parsed.is_finite() || parsed <= 0.0 {
        bail!("RapidX {sym}.{field} must be positive: {raw}");
    }
    Ok(())
}

fn decimal_value(raw: &str, field: &str, sym: &str) -> Result<f64> {
    raw.parse::<f64>()
        .with_context(|| format!("RapidX invalid decimal {sym}.{field}={raw}"))
}

struct SymInfoResponse {
    code: Value,
    data: Value,
}

impl<'de> Deserialize<'de> for SymInfoResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawResponse {
            code: Value,
            #[serde(deserialize_with = "deserialize_unique_symbol_map")]
            data: Value,
        }
        let raw = RawResponse::deserialize(deserializer)?;
        Ok(Self {
            code: raw.code,
            data: raw.data,
        })
    }
}

fn deserialize_unique_symbol_map<'de, D>(deserializer: D) -> std::result::Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    struct SymbolMapVisitor;

    impl<'de> Visitor<'de> for SymbolMapVisitor {
        type Value = Value;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a RapidX symbol object with unique keys")
        }

        fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut rows = serde_json::Map::new();
            while let Some((symbol, row)) = map.next_entry::<String, Value>()? {
                if rows.insert(symbol.clone(), row).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate RapidX symbol map key: {symbol}"
                    )));
                }
            }
            Ok(Value::Object(rows))
        }
    }

    deserializer.deserialize_map(SymbolMapVisitor)
}

fn ensure_success_code(code: &Value) -> Result<()> {
    if code.as_str() == Some("200000") || code.as_u64() == Some(200000) {
        return Ok(());
    }
    bail!("RapidX sym info business code was not 200000")
}

fn signature(secret: &str, nonce: u64) -> Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .context("initialize RapidX HMAC-SHA256")?;
    mac.update(format!("&{nonce}").as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn unix_seconds() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")
        .map(|elapsed| elapsed.as_secs())
}

fn unix_microseconds() -> Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")
        .map(|elapsed| elapsed.as_micros())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    fn response(data: Value) -> Value {
        serde_json::json!({"code": "200000", "data": data})
    }

    fn binance_row(state: &str) -> Value {
        serde_json::json!({
            "sym": "BINANCE_PERP_BTC_USDT", "originalSymbol": "BTCUSDT", "state": state,
            "lotSize": "0.001", "tickSize": "0.10", "minSize": "0.001",
            "minNotional": "0", "contractSize": "1"
        })
    }

    #[test]
    fn parses_binance_and_preserves_decimal_strings() {
        let rows = BTreeMap::from([
            ("BINANCE_PERP_BTC_USDT".to_string(), binance_row("live")),
            (
                "BINANCE_SPOT_BTC_USDT".to_string(),
                serde_json::json!({"sym": "BINANCE_SPOT_BTC_USDT"}),
            ),
        ]);
        let rules = parse(
            &response(serde_json::to_value(rows).unwrap()),
            "binance-futures",
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        let rule = &rules["BTCUSDT"];
        assert_eq!(rule.status, "TRADING");
        assert_eq!(rule.price_tick, "0.10");
        assert_eq!(rule.min_notional, None);
        assert_eq!(rule.contract_multiplier.as_deref(), Some("1"));
    }

    #[test]
    fn parses_okx_contract_quantity_rules() {
        let row = serde_json::json!({
            "sym": "OKX_PERP_ETH_USDT", "originalSymbol": "ETH-USDT-SWAP", "state": "suspend",
            "lotSize": "1", "tickSize": "0.01", "minSize": "1",
            "minNotional": "5.00", "contractSize": "0.01"
        });
        let rules = parse(
            &response(serde_json::json!({"OKX_PERP_ETH_USDT": row})),
            "okex-futures",
        )
        .unwrap();
        let rule = &rules["ETHUSDT"];
        assert_eq!(rule.status, "suspend");
        assert_eq!(rule.contract_multiplier.as_deref(), Some("0.01"));
    }

    #[test]
    fn rejects_target_malformed_records_and_unknown_states() {
        let mut bad_contract = binance_row("live");
        bad_contract["contractSize"] = Value::String("2".to_string());
        assert!(
            parse(
                &response(serde_json::json!({"BINANCE_PERP_BTC_USDT": bad_contract})),
                "binance-futures"
            )
            .is_err()
        );

        let bad_state = binance_row("paused");
        assert!(
            parse(
                &response(serde_json::json!({"BINANCE_PERP_BTC_USDT": bad_state})),
                "binance-futures"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_bad_business_code_target_key_and_target_row_mismatch() {
        assert!(
            parse(
                &serde_json::json!({"code": "400000", "data": {}}),
                "binance-futures"
            )
            .is_err()
        );

        assert!(
            parse(
                &response(serde_json::json!({"BINANCE_PERP_BTC_USDT_EXTRA": {}})),
                "binance-futures"
            )
            .is_err()
        );

        assert!(
            parse(
                &response(serde_json::json!({
                    "BINANCE_SPOT_BTC_USDT": {"sym": "BINANCE_PERP_BTC_USDT"}
                })),
                "binance-futures"
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_numeric_binance_contract_size_one() {
        let mut row = binance_row("live");
        row["contractSize"] = serde_json::json!(1.0);
        let rules = parse(
            &response(serde_json::json!({"BINANCE_PERP_BTC_USDT": row})),
            "binance-futures",
        )
        .unwrap();
        assert_eq!(rules["BTCUSDT"].contract_multiplier.as_deref(), Some("1.0"));
    }

    #[test]
    fn rejects_duplicate_raw_symbol_keys() {
        let payload = r#"{
            "code":"200000",
            "data": {
                "BINANCE_PERP_BTC_USDT": {},
                "BINANCE_PERP_BTC_USDT": {}
            }
        }"#;
        assert!(serde_json::from_str::<SymInfoResponse>(payload).is_err());
    }

    #[tokio::test]
    async fn fetch_signs_empty_query_and_sets_required_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
            }
            let request = String::from_utf8(request).unwrap();
            let mut lines = request.split("\r\n");
            assert_eq!(lines.next(), Some("GET /api/v1/trading/sym/info HTTP/1.1"));
            let headers = lines
                .filter_map(|line| line.split_once(": "))
                .map(|(key, value)| (key.to_ascii_lowercase(), value.to_string()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                headers.get("x-mbx-apikey").map(String::as_str),
                Some("test-key")
            );
            assert_eq!(
                headers.get("content-type").map(String::as_str),
                Some("application/json")
            );
            let nonce = headers.get("nonce").unwrap().parse::<u64>().unwrap();
            let expected_signature = signature("test-secret", nonce).unwrap();
            assert_eq!(
                headers.get("signature").map(String::as_str),
                Some(expected_signature.as_str())
            );
            assert!(headers.get("ts").unwrap().parse::<u128>().unwrap() > 0);

            let body = response(serde_json::json!({"BINANCE_PERP_BTC_USDT": binance_row("live")}))
                .to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let values = BTreeMap::from([
            ("LTP_API_KEY".to_string(), "test-key".to_string()),
            ("LTP_API_SECRET".to_string(), "test-secret".to_string()),
            ("LTP_REST_URL".to_string(), format!("http://{address}")),
        ]);
        let rules = fetch(&Client::new(), "binance-futures", &values)
            .await
            .unwrap();
        assert!(rules.contains_key("BTCUSDT"));
        server.join().unwrap();
    }
}
