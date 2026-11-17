use super::*;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn fixture() -> Value {
    json!({"sourceType":"CTP_ACCOUNT_QUERY","accountKey":"test-investor","mode":"as_of","asOf":"2026-11-17T04:00:00Z","count":1,"rates":[{
        "sourceType":"CTP_ACCOUNT_QUERY","accountKey":"test-investor","venue":"SHFE","instrumentId":"sn2612","observedAt":"2026-11-17T03:58:00Z","tradingDay":"20261117","contractMultiplier":"1","priceTick":"10","active":true,
        "fees":{"open":{"rate":"0.00000006","perLot":"3.03"},"close":{"rate":"0.00000006","perLot":"3.03"},"closeToday":{"rate":"0","perLot":"0"}},
        "margin":{"longRate":"0.2","shortRate":"0.2","longPerLot":"0","shortPerLot":"0"},
        "recordHash":"a".repeat(64),"receiptSha256":"b".repeat(64)
    }]})
}
fn at() -> i64 {
    reference::timestamp("2026-11-17T04:00:00Z").unwrap()
}

#[test]
fn fees_preserve_offsets_and_zero_without_liquidity_or_future_backfill() {
    let rates = parse(&fixture(), "test-investor", "SHFE", "sn2612", at())
        .unwrap()
        .unwrap();
    let fee = rates.estimated_fee("open", 2, 260000.0, at()).unwrap();
    assert!((fee - 6.0912).abs() < 1e-10);
    assert_eq!(
        rates.estimated_fee("close_today", 2, 260000.0, at()),
        Some(0.0)
    );
    assert_eq!(
        rates.estimated_fee("close_yesterday", 2, 260000.0, at()),
        Some(fee)
    );
    assert_eq!(rates.estimated_fee("maker", 2, 260000.0, at()), None);
    assert_eq!(
        rates.estimated_fee("open", 2, 260000.0, rates.observed_at_us - 1),
        None
    );
    let mut empty = fixture();
    empty["rates"] = json!([]);
    empty["count"] = json!(0);
    assert!(
        parse(&empty, "test-investor", "SHFE", "sn2612", at())
            .unwrap()
            .is_none()
    );
}
#[test]
fn future_observations_other_accounts_and_missing_rate_fields_fail_closed() {
    let mut bad = fixture();
    bad["rates"][0]["observedAt"] = json!("2026-11-18T00:00:00Z");
    assert!(parse(&bad, "test-investor", "SHFE", "sn2612", at()).is_err());
    assert!(parse(&fixture(), "another-investor", "SHFE", "sn2612", at()).is_err());
    assert!(parse(&fixture(), "test-investor", "SHFE", "sn2701", at()).is_err());
    let mut bad = fixture();
    bad["rates"][0]["fees"]["closeToday"]["perLot"] = Value::Null;
    assert!(parse(&bad, "test-investor", "SHFE", "sn2612", at()).is_err());
    let mut bad = fixture();
    bad["rates"][0]["recordHash"] = json!("unverified");
    assert!(parse(&bad, "test-investor", "SHFE", "sn2612", at()).is_err());
}

#[derive(Clone, Default)]
struct Mock {
    queries: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
}
async fn handler(
    State(state): State<Mock>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    state.queries.lock().unwrap().push(query.clone());
    let mut result = fixture();
    result["asOf"] = json!(query["asOf"]);
    if reference::timestamp(&query["asOf"]).unwrap()
        < reference::timestamp("2026-11-17T03:58:00Z").unwrap()
    {
        result["rates"] = json!([]);
        result["count"] = json!(0);
    }
    Json(result)
}
#[tokio::test]
async fn http_queries_explicit_source_alias_and_exact_historical_boundary() {
    let state = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/api/v1/account-rates", get(handler))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let feed = Feed::new(Settings {
        api_url: url,
        accounts: BTreeMap::from([("source-01".into(), "test-investor".into())]),
    })
    .unwrap();
    assert!(
        feed.rates("source-01", "SHFE", "sn2612", at())
            .await
            .unwrap()
            .is_some()
    );
    let before = reference::timestamp("2026-11-17T03:00:00.123456Z").unwrap();
    assert!(
        feed.rates("source-01", "SHFE", "sn2612", before)
            .await
            .unwrap()
            .is_none()
    );
    assert!(feed.rates("unknown", "SHFE", "sn2612", at()).await.is_err());
    let queries = state.queries.lock().unwrap();
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[0]["account"], "test-investor");
    assert_eq!(queries[0]["instrument"], "sn2612");
    assert_eq!(reference::timestamp(&queries[1]["asOf"]).unwrap(), before);
    server.abort();
}
