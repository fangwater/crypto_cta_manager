use super::*;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde_json::json;

fn pack() -> Pack {
    serde_json::from_str(include_str!(
        "../../../config/cn-futures-reference.example.json"
    ))
    .unwrap()
}
fn rows(pack: &Pack) -> Vec<Value> {
    pack.bars
        .iter()
        .map(|b| {
            let ms = reference::timestamp(&b.open_time).unwrap() / 1000;
            json!([
                ms,
                b.open.to_string(),
                b.high.to_string(),
                b.low.to_string(),
                b.close.to_string(),
                b.volume_lots.to_string(),
                ms + 59_999,
                b.turnover.to_string(),
                1,
                "0",
                "0",
                "0"
            ])
        })
        .collect()
}
#[derive(Clone, Default)]
struct Mock {
    rows: Arc<Mutex<Vec<Value>>>,
    queries: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
}
async fn handler(
    State(state): State<Mock>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Vec<Value>> {
    state.queries.lock().unwrap().push(query.clone());
    let start = query["startTime"].parse::<i64>().unwrap();
    let end = query["endTime"].parse::<i64>().unwrap();
    Json(
        state
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r[0].as_i64().unwrap() >= start && r[0].as_i64().unwrap() <= end)
            .cloned()
            .collect(),
    )
}
async fn server(state: Mock) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fapi/v1/klines", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/fapi/v1/klines", get(handler))
        .with_state(state);
    (
        url,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
fn settings(url: String, root: &Path) -> Settings {
    Settings {
        api_url: url,
        cache_root: root.to_owned(),
        symbols: BTreeMap::from([("SHFE:cu2612".into(), "SHFE:cu2612".into())]),
    }
}

#[tokio::test]
async fn binance_http_contract_lot_units_cache_reuse_and_concurrent_misses() {
    let mut p = pack();
    let state = Mock {
        rows: Arc::new(Mutex::new(rows(&p))),
        ..Default::default()
    };
    let (url, server) = server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let settings = settings(url, temp.path());
    let feed = Feed::new(settings.clone()).unwrap();
    p.bars.clear();
    let mut other = p.clone();
    other.source_id = "another_source".into();
    let now = reference::timestamp("2026-11-14T00:00:00Z").unwrap();
    let (a, b) = tokio::join!(feed.augment(&mut p, now), feed.augment(&mut other, now));
    a.unwrap();
    b.unwrap();
    assert_eq!(p.bars.len(), 2);
    assert_eq!(other.bars.len(), 2);
    assert_eq!(state.queries.lock().unwrap().len(), 1);
    let q = state.queries.lock().unwrap()[0].clone();
    assert_eq!(q["symbol"], "SHFE:cu2612");
    assert_eq!(q["interval"], "1m");
    assert_eq!(q["limit"], "1000");
    assert_eq!(
        q["startTime"],
        (reference::timestamp(&pack().bars[0].open_time).unwrap() / 1000).to_string()
    );
    assert_eq!(
        q["endTime"],
        ((reference::timestamp(&pack().bars[1].open_time).unwrap() + MINUTE_US) / 1000 - 1)
            .to_string()
    );
    let report = cost::build(
        &p.source_id,
        &p.trading_day,
        Some(&p),
        super::super::reader::CostFacts {
            replication: None,
            orders: vec![],
            fills: vec![],
        },
        now,
    )
    .unwrap();
    assert_eq!(report.signals[0].theoretical_price, Some(105.0));
    drop(feed);
    p.bars.clear();
    Feed::new(settings)
        .unwrap()
        .augment(&mut p, now)
        .await
        .unwrap();
    assert_eq!(
        state.queries.lock().unwrap().len(),
        1,
        "restart must reuse the durable minute cache"
    );
    server.abort();
}

#[tokio::test]
async fn omitted_minutes_stay_missing_and_retry_without_redownloading_cached_candles() {
    let mut p = pack();
    let all = rows(&p);
    let state = Mock {
        rows: Arc::new(Mutex::new(vec![all[0].clone()])),
        ..Default::default()
    };
    let (url, server) = server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let feed = Feed::new(settings(url, temp.path())).unwrap();
    let now = reference::timestamp("2026-11-14T00:00:00Z").unwrap();
    p.bars.clear();
    feed.augment(&mut p, now).await.unwrap();
    assert_eq!(p.bars.len(), 1);
    let report = cost::build(
        &p.source_id,
        &p.trading_day,
        Some(&p),
        super::super::reader::CostFacts {
            replication: None,
            orders: vec![],
            fills: vec![],
        },
        now,
    )
    .unwrap();
    assert!(report.signals[0].theoretical_price.is_none());
    *state.rows.lock().unwrap() = all;
    p.bars.clear();
    feed.augment(&mut p, now).await.unwrap();
    assert_eq!(p.bars.len(), 2);
    let q = state.queries.lock().unwrap();
    assert_eq!(q.len(), 2);
    assert_eq!(
        q[1]["startTime"],
        (reference::timestamp(&pack().bars[1].open_time).unwrap() / 1000).to_string()
    );
    server.abort();
}

#[tokio::test]
async fn invalid_http_units_are_rejected_before_entering_the_cache() {
    let mut p = pack();
    let mut invalid = rows(&p);
    invalid[0][7] = json!("2000");
    let state = Mock {
        rows: Arc::new(Mutex::new(invalid)),
        ..Default::default()
    };
    let (url, server) = server(state).await;
    let temp = tempfile::tempdir().unwrap();
    let feed = Feed::new(settings(url, temp.path())).unwrap();
    p.bars.clear();
    assert!(
        feed.augment(
            &mut p,
            reference::timestamp("2026-11-14T00:00:00Z").unwrap()
        )
        .await
        .is_err()
    );
    assert!(p.bars.is_empty());
    assert!(!temp.path().join("SHFE/cu2612/20261113.json").exists());
    server.abort();
}

#[test]
fn strict_minute_timestamps_explicit_symbols_and_http_configuration() {
    let p = pack();
    let mut raw = rows(&p)[0].clone();
    raw[6] = json!(1);
    assert!(parse_bar(&raw, "SHFE", "cu2612").is_err());
    let temp = tempfile::tempdir().unwrap();
    let mut s = settings("http://127.0.0.1:18311/fapi/v1/klines".into(), temp.path());
    assert!(s.validate().is_ok());
    s.api_url = "http://user:secret@127.0.0.1/klines".into();
    assert!(s.validate().is_err());
    s.api_url = "http://127.0.0.1/klines".into();
    s.symbols.insert("INE:sc2612".into(), "SHFE:cu2612".into());
    assert!(s.validate().is_err());
}
