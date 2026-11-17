//! SDK-free native NAV HTTP/PG regression; requires an isolated PostgreSQL socket.
use anyhow::{Context, Result, ensure};
use cn_market_proto::{
    cn_futures_market_server::{CnFuturesMarket, CnFuturesMarketServer},
    *,
};
use crypto_cta_manager::{
    auth,
    native_futures::{
        config::{Config, Source},
        web,
    },
    postgres,
};
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::collections::BTreeMap;
use tonic::{Request, Response, Status};
const BASE: i64 = 1794877200000000; // 2026-11-17 09:00 Shanghai
#[derive(Clone)]
struct Market;
#[tonic::async_trait]
impl CnFuturesMarket for Market {
    async fn get_minute_bars(
        &self,
        r: Request<GetMinuteBarsRequest>,
    ) -> Result<Response<GetMinuteBarsResponse>, Status> {
        let r = r.into_inner();
        let bars = (-1..4)
            .filter_map(|i| {
                let open = BASE / 1000 + i * 60_000;
                let inside = match &r.range {
                    Some(get_minute_bars_request::Range::Time(t)) => {
                        open >= t.start_ms && open <= t.end_ms
                    }
                    _ => true,
                };
                let price = if i < 1 { 100.0 } else { 110.0 };
                inside.then_some(MinuteBar {
                    open_time_ms: open,
                    trading_day: 20261117,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume_lots: 1.0,
                    turnover_cny: price * 10.0,
                    vwap: price,
                    ..Default::default()
                })
            })
            .collect();
        Ok(Response::new(GetMinuteBarsResponse {
            contract: Some(Contract {
                exchange: Exchange::Shfe as i32,
                product: "CU".into(),
                native_code: "cu2612".into(),
                canonical_code: "cu2612".into(),
                volume_multiple: 10.0,
                price_tick: 1.0,
                price_tick_source: "instrument".into(),
            }),
            bars,
            data_state: Some(DataState {
                source: "baseline_data_1min".into(),
                latest_trading_day: 20261117,
                session_snapshot_as_of: "2026-11-14".into(),
                ..Default::default()
            }),
        }))
    }
    async fn get_trading_sessions(
        &self,
        _: Request<GetTradingSessionsRequest>,
    ) -> Result<Response<GetTradingSessionsResponse>, Status> {
        Ok(Response::new(GetTradingSessionsResponse {
            snapshot_as_of: "2026-11-14".into(),
            products: vec![ProductSessions {
                product: "CU".into(),
                windows: vec![
                    SessionWindow {
                        begin_sec: 9 * 3600,
                        end_sec: 11 * 3600 + 30 * 60,
                    },
                    SessionWindow {
                        begin_sec: 13 * 3600 + 30 * 60,
                        end_sec: 15 * 3600,
                    },
                ],
            }],
        }))
    }
    async fn get_data_state(
        &self,
        _: Request<GetDataStateRequest>,
    ) -> Result<Response<DataState>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn get_size_thresholds(
        &self,
        _: Request<GetSizeThresholdsRequest>,
    ) -> Result<Response<GetSizeThresholdsResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    type StreamIntradayFramesStream =
        tokio_stream::wrappers::ReceiverStream<Result<IntradayFrameBatch, Status>>;
    async fn stream_intraday_frames(
        &self,
        _: Request<GetIntradayFramesRequest>,
    ) -> Result<Response<Self::StreamIntradayFramesStream>, Status> {
        Err(Status::unimplemented("unused"))
    }
}
async fn rates(
    axum::extract::Query(q): axum::extract::Query<BTreeMap<String, String>>,
) -> axum::Json<Value> {
    let row = json!({"sourceType":"CTP_ACCOUNT_QUERY","accountKey":"test-fee-account","venue":"SHFE","instrumentId":"cu2612","observedAt":"2026-11-17T00:00:00Z","tradingDay":"20261117","contractMultiplier":"10","priceTick":"1","active":true,
 "fees":{"open":{"rate":"0","perLot":"2"},"close":{"rate":"0","perLot":"3"},"closeToday":{"rate":"0","perLot":"3"}},"margin":{"longRate":"0.1","shortRate":"0.1","longPerLot":"0","shortPerLot":"0"},"recordHash":"a".repeat(64),"receiptSha256":"b".repeat(64)});
    if let Some(at) = q.get("asOf") {
        axum::Json(
            json!({"sourceType":"CTP_ACCOUNT_QUERY","accountKey":"test-fee-account","mode":"as_of","asOf":at,"count":1,"rates":[row]}),
        )
    } else {
        axum::Json(
            json!({"sourceType":"CTP_ACCOUNT_QUERY","accountKey":"test-fee-account","mode":"observations","asOf":null,"count":0,"rates":[]}),
        )
    }
}
fn event(db: &rocksdb::DB, seq: i64, at: i64, value: Value) {
    db.put_cf(
        db.cf_handle("cn_trading_events").unwrap(),
        seq.to_be_bytes(),
        serde_json::to_vec(
            &json!({"sequence":seq,"received_us":at,"projection_error":null,"value":value}),
        )
        .unwrap(),
    )
    .unwrap();
}
fn inventory() -> Value {
    json!([
     {"venue":"SHFE","symbol":"cu2612","side":"buy","position":{"today":1,"yesterday":0,"frozen_today":0,"frozen_yesterday":0}},
     {"venue":"SHFE","symbol":"cu2612","side":"sell","position":{"today":1,"yesterday":0,"frozen_today":0,"frozen_yesterday":0}}
    ])
}
fn state(db: &rocksdb::DB, seq: i64, at: i64) {
    event(
        db,
        seq,
        at,
        json!({"type":"state","state":{"ready":true,"trading_day":"20261117","positions":inventory(),"available_local_budget":100000,"reason":""}}),
    );
}
fn trade(
    db: &rocksdb::DB,
    seq: i64,
    side: &str,
    offset: &str,
    price: f64,
    at: i64,
    live: bool,
    commission: Option<f64>,
) {
    let id = json!({"venue":"SHFE","symbol":"cu2612","trading_day":"20261117","trade_id":format!("trade-{seq}"),"side":side});
    event(
        db,
        seq - 1,
        at,
        json!({"type":"ctp_event","event":{"received_us":at,"kind":if live {10}else{14},"direction":if side=="buy" {48}else{49},"exchange":"SHFE","symbol":"cu2612","trading_day":"20261117","trade_id":format!("trade-{seq}"),"system_id":format!("sys-{seq}"),"volume":1,"price":price}}),
    );
    event(
        db,
        seq,
        at,
        json!({"type":"normalized_trade","identity":id,"origin":if live {"live_report"}else{"recovery_query"}}),
    );
    let fill = json!({"fill":{"identity":id,"system_id":format!("sys-{seq}"),"order_ref":format!("{seq}"),"offset":offset,"lots":1,"price":price},"commission":commission,"commission_currency":commission.map(|_|"CNY"),"native_time":null,"received_us":at,"evidence_seq":seq});
    db.put_cf(
        db.cf_handle("cn_trade_facts").unwrap(),
        format!("trade-{seq}"),
        serde_json::to_vec(&fill).unwrap(),
    )
    .unwrap();
}
#[tokio::test]
#[ignore = "requires an isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL cluster"]
async fn native_nav_http_preserves_gross_anchor_permissions_fee_history_and_recovery_cut()
-> Result<()> {
    let socket = std::env::var("CTA_SCHEMA_TEST_SOCKET").context("set isolated test socket")?;
    ensure!(
        socket.starts_with("/tmp/cta-schema-test-")
            && std::path::Path::new(&socket)
                .join(".s.PGSQL.15433")
                .exists(),
        "only isolated test PostgreSQL accepted"
    );
    let options = PgConnectOptions::new()
        .host(&socket)
        .port(15433)
        .username(&std::env::var("USER")?)
        .database("postgres");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let name = format!(
        "cn_nav_test_{}_{}",
        std::process::id(),
        crypto_cta_manager::native_futures::now_us()
    );
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(options.database(&name))
        .await?;
    postgres::initialize(&pool).await?;
    let tmp = tempfile::tempdir()?;
    let primary = tmp.path().join("exec");
    let mut o = rocksdb::Options::default();
    o.create_if_missing(true);
    o.create_missing_column_families(true);
    let db = rocksdb::DB::open_cf(
        &o,
        &primary,
        ["cn_trading_events", "cn_uniform_orders", "cn_trade_facts"],
    )?;
    event(
        &db,
        8,
        BASE,
        json!({"type":"reconciled","state":{"ready":false,"trading_day":"20261117","positions":inventory(),"available_local_budget":100000,"reason":"settlement not confirmed; read-only queries remain available"}}),
    );
    event(
        &db,
        10,
        BASE + 10,
        json!({"type":"state","state":{"ready":false,"trading_day":"20261117","positions":inventory(),"available_local_budget":100000,"reason":"settlement not confirmed; read-only queries remain available"}}),
    );
    let grpc = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let grpc_url = format!("http://{}", grpc.local_addr()?);
    let grpc_server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(CnFuturesMarketServer::new(Market))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(grpc))
            .await
            .unwrap()
    });
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let ref_url = format!("http://{}", http.local_addr()?);
    let ref_server = tokio::spawn(async move {
        axum::serve(
            http,
            axum::Router::new().route("/api/v1/account-rates", axum::routing::get(rates)),
        )
        .await
        .unwrap()
    });
    let config = Config {
        mode: crypto_cta_manager::runtime_mode::RuntimeMode::CnFutures,
        database: crypto_cta_manager::config::DatabaseConfig {
            url_env: "CN_TEST_ONLY".into(),
            max_connections: 6,
        },
        secondary_root: tmp.path().join("secondary"),
        reference_root: None,
        market_data: Some(crypto_cta_manager::native_futures::market_data::Settings {
            grpc_url,
            cache_root: tmp.path().join("market-cache"),
        }),
        reference_service: Some(
            crypto_cta_manager::native_futures::reference_data::Settings {
                api_url: ref_url,
                accounts: BTreeMap::from([("sim".into(), "test-fee-account".into())]),
            },
        ),
        sync: None,
        sync_export: None,
        stale_secs: 30,
        recent_rows: 20,
        source_viewers: BTreeMap::new(),
        sources: vec![Source {
            id: "sim".into(),
            account: "synthetic".into(),
            adapter: "ctp".into(),
            rocksdb_path: primary,
            enabled: true,
            sync: None,
        }],
    };
    web::register_sources(&pool, &config).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let nav_source = config.sources[0].clone();
    let app = web::Service::new(config, pool.clone())?.router();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let bootstrap = client
        .post(format!("{base}/api/auth/register"))
        .json(&json!({"username":"admin","password":"synthetic-native-nav-test"}))
        .send()
        .await?
        .error_for_status()?;
    let cookie = bootstrap.headers()["set-cookie"]
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let user = auth::create_user(
        &pool,
        auth::CreateUserRequest {
            username: "nav-reader".into(),
            password: "synthetic-native-nav-reader".into(),
            role: None,
        },
    )
    .await?;
    auth::set_sources(
        &pool,
        user.user_id,
        &[auth::SourceGrantInput {
            source_id: "sim".into(),
            access_level: "view".into(),
        }],
        &std::collections::BTreeSet::from(["sim".into()]),
    )
    .await?;
    let login = client
        .post(format!("{base}/api/auth/login"))
        .json(&json!({"username":"nav-reader","password":"synthetic-native-nav-reader"}))
        .send()
        .await?
        .error_for_status()?;
    let reader_cookie = login.headers()["set-cookie"]
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let snapshots = format!("{base}/api/native/sources/sim/nav-snapshots");
    let body =
        json!({"expected_state_us":BASE,"note":"synthetic complete hedged read-only anchor"});
    assert_eq!(
        client
            .post(&snapshots)
            .header("Cookie", &reader_cookie)
            .json(&body)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let response = client
        .post(&snapshots)
        .header("Cookie", &cookie)
        .json(&body)
        .send()
        .await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "{}",
        response.text().await?
    );
    trade(
        &db,
        12,
        "buy",
        "open",
        100.0,
        BASE + 60_000_000,
        true,
        Some(1.0),
    );
    trade(
        &db,
        14,
        "sell",
        "close_today",
        110.0,
        BASE + 120_000_000,
        true,
        None,
    );
    state(&db, 15, BASE + 180_000_000);
    let url = format!(
        "{base}/api/native/sources/sim/nav?start_ms={}&end_ms={}",
        BASE / 1000,
        (BASE + 180_000_000) / 1000
    );
    let response = client
        .get(&url)
        .header("Cookie", &reader_cookie)
        .send()
        .await?
        .error_for_status()?;
    let nav: Value = response.json().await?;
    assert_eq!(nav["ready"], true, "{nav}");
    assert_eq!(nav["fees"]["actual_count"], 1);
    assert_eq!(nav["fees"]["estimated_count"], 1);
    assert_eq!(nav["summary"]["nav_before_fees_cny"], 100.0);
    assert_eq!(nav["summary"]["nav_after_fees_cny"], 96.0);
    assert_eq!(nav["points"][0]["nav_after_fees_cny"], 0.0);
    let stored = client
        .get(&snapshots)
        .header("Cookie", &cookie)
        .send()
        .await?
        .json::<Vec<Value>>()
        .await?;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["snapshot_us"], BASE);
    assert_eq!(stored[0]["positions"].as_array().unwrap().len(), 2);
    let mut conflicting: crypto_cta_manager::native_futures::nav::Anchor =
        serde_json::from_value(stored[0].clone())?;
    use crypto_cta_manager::native_futures::nav::source::validate_anchor;
    validate_anchor(&nav_source, &db, &conflicting)?;
    conflicting.snapshot_us += 1;
    assert!(validate_anchor(&nav_source, &db, &conflicting).is_err());
    conflicting.snapshot_us -= 1;
    conflicting.trading_day = "20261116".into();
    assert!(validate_anchor(&nav_source, &db, &conflicting).is_err());
    conflicting.trading_day = "20261117".into();
    conflicting.note = "different immutable financial anchor".into();
    assert!(
        crypto_cta_manager::native_futures::nav::store::insert(&pool, &conflicting, "admin")
            .await
            .is_err()
    );
    event(&db, 16, BASE + 190_000_000, json!({"type":"runtime_start"}));
    let pending: Value = client
        .get(format!(
            "{base}/api/native/sources/sim/nav?start_ms={}&end_ms={}",
            BASE / 1000,
            (BASE + 190_000_000) / 1000
        ))
        .header("Cookie", &cookie)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        pending["ready"], false,
        "runtime start must not advance financial coverage: {pending}"
    );
    assert!(pending["summary"].is_null());
    trade(
        &db,
        18,
        "buy",
        "open",
        100.0,
        BASE + 200_000_000,
        false,
        None,
    );
    state(&db, 20, BASE + 240_000_000);
    let nav: Value = client
        .get(format!(
            "{base}/api/native/sources/sim/nav?start_ms={}&end_ms={}",
            BASE / 1000,
            (BASE + 240_000_000) / 1000
        ))
        .header("Cookie", &reader_cookie)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(nav["ready"], false);
    assert!(nav["points"].as_array().unwrap().is_empty());
    db.put(b"primary-still-writable", b"yes")?;
    server.abort();
    grpc_server.abort();
    ref_server.abort();
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}
