//! Native UTA USDT perpetual rules. Publish only after every page validates.
use std::collections::BTreeSet;

use super::*;

pub(super) async fn fetch_bybit(
    client: &Client,
    base: &str,
) -> Result<BTreeMap<String, MarketRule>> {
    let mut symbols = BTreeMap::new();
    let mut cursor = String::new();
    let mut seen = BTreeSet::new();
    loop {
        let mut url = reqwest::Url::parse(&format!("{base}/v5/market/instruments-info"))?;
        url.query_pairs_mut()
            .append_pair("category", "linear")
            .append_pair("limit", "1000");
        if !cursor.is_empty() {
            url.query_pairs_mut().append_pair("cursor", &cursor);
        }
        let value = get_json(client, url.as_str()).await?;
        let (page, next) = parse_bybit_page(&value)?;
        merge_page(&mut symbols, page)?;
        if next.is_empty() {
            break;
        }
        anyhow::ensure!(
            seen.insert(next.clone()) && seen.len() <= 100,
            "Bybit instruments cursor repeated or exceeded page limit"
        );
        cursor = next;
    }
    anyhow::ensure!(
        !symbols.is_empty(),
        "Bybit USDT perpetual snapshot is empty"
    );
    Ok(symbols)
}

fn merge_page(
    symbols: &mut BTreeMap<String, MarketRule>,
    page: BTreeMap<String, MarketRule>,
) -> Result<()> {
    for (symbol, rule) in page {
        anyhow::ensure!(
            symbols.insert(symbol.clone(), rule).is_none(),
            "duplicate instrument: {symbol}"
        );
    }
    Ok(())
}

fn parse_bybit_page(value: &Value) -> Result<(BTreeMap<String, MarketRule>, String)> {
    anyhow::ensure!(
        value["retCode"].as_i64() == Some(0),
        "Bybit instruments request failed"
    );
    let result = &value["result"];
    anyhow::ensure!(
        result["category"].as_str() == Some("linear"),
        "Bybit instruments category mismatch"
    );
    let rows = result["list"]
        .as_array()
        .context("Bybit instruments omitted list")?;
    let cursor = result["nextPageCursor"]
        .as_str()
        .context("Bybit instruments omitted cursor")?;
    let mut symbols = BTreeMap::new();
    for row in rows {
        if required_string(row, "contractType")? != "LinearPerpetual"
            || required_string(row, "quoteCoin")? != "USDT"
        {
            continue;
        }
        anyhow::ensure!(
            required_string(row, "settleCoin")? == "USDT",
            "Bybit settlement mismatch"
        );
        let symbol = required_string(row, "symbol")?.to_owned();
        anyhow::ensure!(symbol.ends_with("USDT"), "Bybit symbol/quote mismatch");
        let lot = &row["lotSizeFilter"];
        let rule = MarketRule {
            status: required_string(row, "status")?.into(),
            base_asset: required_string(row, "baseCoin")?.into(),
            quote_asset: "USDT".into(),
            price_tick: required_string(&row["priceFilter"], "tickSize")?.into(),
            qty_step: required_string(lot, "qtyStep")?.into(),
            min_qty: required_string(lot, "minOrderQty")?.into(),
            min_notional: notional(lot, "minNotionalValue")?,
            contract_multiplier: None,
        };
        anyhow::ensure!(
            symbols.insert(symbol.clone(), rule).is_none(),
            "duplicate Bybit instrument: {symbol}"
        );
    }
    Ok((symbols, cursor.into()))
}

pub(super) async fn fetch_bitget(
    client: &Client,
    base: &str,
) -> Result<BTreeMap<String, MarketRule>> {
    let value = get_json(
        client,
        &format!("{base}/api/v3/market/instruments?category=USDT-FUTURES"),
    )
    .await?;
    parse_bitget(&value)
}

fn parse_bitget(value: &Value) -> Result<BTreeMap<String, MarketRule>> {
    anyhow::ensure!(
        value["code"].as_str() == Some("00000"),
        "Bitget instruments request failed"
    );
    let rows = value["data"]
        .as_array()
        .context("Bitget instruments omitted data")?;
    let mut symbols = BTreeMap::new();
    for row in rows {
        anyhow::ensure!(
            required_string(row, "category")? == "USDT-FUTURES",
            "Bitget instruments category mismatch"
        );
        if required_string(row, "type")? != "perpetual" {
            continue;
        }
        let symbol = required_string(row, "symbol")?.to_owned();
        anyhow::ensure!(
            required_string(row, "quoteCoin")? == "USDT" && symbol.ends_with("USDT"),
            "Bitget symbol/quote mismatch"
        );
        let rule = MarketRule {
            status: required_string(row, "status")?.into(),
            base_asset: required_string(row, "baseCoin")?.into(),
            quote_asset: "USDT".into(),
            // V3 multipliers are already decimal increments, not V2 precision factors.
            price_tick: required_string(row, "priceMultiplier")?.into(),
            qty_step: required_string(row, "quantityMultiplier")?.into(),
            min_qty: required_string(row, "minOrderQty")?.into(),
            min_notional: notional(row, "minOrderAmount")?,
            contract_multiplier: None,
        };
        anyhow::ensure!(
            symbols.insert(symbol.clone(), rule).is_none(),
            "duplicate Bitget instrument: {symbol}"
        );
    }
    anyhow::ensure!(
        !symbols.is_empty(),
        "Bitget USDT perpetual snapshot is empty"
    );
    Ok(symbols)
}

fn notional(row: &Value, field: &str) -> Result<Option<String>> {
    let raw = required_string(row, field)?;
    let value: f64 = raw.parse().context("invalid minimum notional")?;
    anyhow::ensure!(
        value.is_finite() && value >= 0.0,
        "invalid minimum notional"
    );
    Ok((value > 0.0).then(|| raw.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bybit_page() -> Value {
        json!({"retCode":0,"result":{"category":"linear","nextPageCursor":"page2","list":[{
            "symbol":"BTCUSDT","contractType":"LinearPerpetual","status":"Trading","baseCoin":"BTC","quoteCoin":"USDT","settleCoin":"USDT",
            "priceFilter":{"tickSize":"0.1"},"lotSizeFilter":{"qtyStep":"0.001","minOrderQty":"0.001","minNotionalValue":"5"}
        }]}})
    }

    fn bitget_page() -> Value {
        json!({"code":"00000","data":[{"symbol":"BTCUSDT","category":"USDT-FUTURES","type":"perpetual","status":"online","baseCoin":"BTC","quoteCoin":"USDT","pricePrecision":"2","priceMultiplier":"0.05","quantityMultiplier":"0.001","minOrderQty":"0.002","minOrderAmount":"5"}]})
    }

    #[tokio::test]
    async fn bybit_fetch_requires_all_pages_and_rejects_a_loop() {
        use axum::{
            Json, Router,
            extract::{Query, State},
            routing::get,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        async fn page(
            State(looping): State<Arc<AtomicBool>>,
            Query(query): Query<BTreeMap<String, String>>,
        ) -> Json<Value> {
            assert_eq!(query["category"], "linear");
            let mut value = bybit_page();
            if query.contains_key("cursor") {
                assert_eq!(query["cursor"], "page2");
                value["result"]["list"][0]["symbol"] = json!("ETHUSDT");
                value["result"]["list"][0]["baseCoin"] = json!("ETH");
                value["result"]["nextPageCursor"] = json!(if looping.load(Ordering::SeqCst) {
                    "page2"
                } else {
                    ""
                });
            }
            Json(value)
        }
        let looping = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .route("/v5/market/instruments-info", get(page))
            .with_state(looping.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        assert_eq!(fetch_bybit(&client, &base).await.unwrap().len(), 2);
        looping.store(true, Ordering::SeqCst);
        assert!(fetch_bybit(&client, &base).await.is_err());
        server.abort();
    }

    #[test]
    fn bybit_pages_preserve_status_and_reject_partial_or_duplicate_snapshots() {
        let mut value = bybit_page();
        let (mut symbols, cursor) = parse_bybit_page(&value).unwrap();
        assert_eq!(cursor, "page2");
        assert_eq!(symbols["BTCUSDT"].qty_step, "0.001");
        assert!(merge_page(&mut symbols, parse_bybit_page(&value).unwrap().0).is_err());
        value["result"]["list"][0]["status"] = json!("Settling");
        assert_eq!(
            parse_bybit_page(&value).unwrap().0["BTCUSDT"].status,
            "Settling"
        );
        value["result"]["nextPageCursor"] = Value::Null;
        assert!(parse_bybit_page(&value).is_err());
        value = bybit_page();
        value["result"]["list"][0]["lotSizeFilter"]["qtyStep"] = Value::Null;
        assert!(parse_bybit_page(&value).is_err());
    }

    #[test]
    fn bitget_uses_decimal_multipliers_and_retains_inactive_status() {
        let mut value = bitget_page();
        let rules = parse_bitget(&value).unwrap();
        assert_eq!(rules["BTCUSDT"].price_tick, "0.05");
        assert_eq!(rules["BTCUSDT"].min_qty, "0.002");
        value["data"][0]["status"] = json!("limit_open");
        assert_eq!(
            parse_bitget(&value).unwrap()["BTCUSDT"].status,
            "limit_open"
        );
        value["data"][0]["category"] = json!("COIN-FUTURES");
        assert!(parse_bitget(&value).is_err());
        value = bitget_page();
        let duplicate = value["data"][0].clone();
        value["data"].as_array_mut().unwrap().push(duplicate);
        assert!(parse_bitget(&value).is_err());
    }
}
