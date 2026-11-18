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
        evidence, netting, web,
    },
    postgres,
};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    AssertSqlSafe,
    postgres::{PgConnectOptions, PgPoolOptions},
    types::Json as DbJson,
};
use std::collections::{BTreeMap, BTreeSet};
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
mod wire {
    include!(concat!(env!("OUT_DIR"), "/cta.nav.rs"));
}

async fn browser_timeline(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    cookie: &str,
    view: &str,
    start: i64,
    end: i64,
    symbols: Option<&str>,
) -> Result<reqwest::Response> {
    let mut request = client
        .get(format!("{base}/api/{path}"))
        .header("Cookie", cookie)
        .header("Accept", "application/x-protobuf")
        .query(&[
            ("sourceIds", "sim".into()),
            ("startMs", (start / 1000).to_string()),
            ("endMs", (end / 1000).to_string()),
            ("maxPoints", "3500".into()),
            ("includeTheoretical", "false".into()),
            ("view", view.into()),
        ]);
    if let Some(symbols) = symbols {
        request = request.query(&[("symbols", symbols)]);
    }
    Ok(request.send().await?)
}

async fn decode_timeline(response: reqwest::Response) -> Result<wire::Timeline> {
    if response.status() != reqwest::StatusCode::OK {
        anyhow::bail!(
            "unexpected timeline status {}: {}",
            response.status(),
            response.text().await?
        );
    }
    assert_eq!(response.headers()["content-type"], "application/x-protobuf");
    let timeline = wire::Timeline::decode(response.bytes().await?)?;
    assert!(timeline.metadata.is_some());
    for curve in &timeline.curves {
        let axis = &timeline.axes[curve.axis as usize];
        let expected = (1..=7).collect::<BTreeSet<_>>();
        let metrics = curve
            .columns
            .iter()
            .map(|column| column.metric)
            .collect::<BTreeSet<_>>();
        assert_eq!(metrics, expected);
        assert!(curve.columns.iter().all(|column| {
            (column.values.len() == 1 || column.values.len() == axis.deltas_us.len() + 1)
                && column.values.iter().all(|value| value.is_finite())
        }));
    }
    Ok(timeline)
}

fn terminal(curve: &wire::Curve, metric: wire::Metric) -> f64 {
    *curve
        .columns
        .iter()
        .find(|column| column.metric == metric as i32)
        .unwrap()
        .values
        .last()
        .unwrap()
}

async fn persist_attribution(pool: &sqlx::PgPool, db: &rocksdb::DB, source: &Source) -> Result<()> {
    let user: i64 = sqlx::query_scalar("SELECT user_id FROM cta_users WHERE username='admin'")
        .fetch_one(pool)
        .await?;
    sqlx::query(
        "INSERT INTO cta_position_strategies(strategy_name,updated_at_us,created_by_user_id)
         VALUES('alpha',1,$1),('beta',1,$1)",
    )
    .bind(user)
    .execute(pool)
    .await?;
    let raw = evidence::journal_entry(db, 8)?;
    let market = "SHFE:cu2612";
    let request = netting::Initialize {
        strategy_groups: BTreeMap::from([(
            "fixture".into(),
            BTreeSet::from(["alpha".into(), "beta".into()]),
        )]),
        contracts: BTreeMap::from([(market.into(), netting::Contract { multiplier: 10.0 })]),
        positions: BTreeMap::new(),
        snapshot_evidence_sequence: 8,
        note: "synthetic verified gross hedge and flat net strategy anchor".into(),
    };
    let mut portfolio = netting::Portfolio::initialize(
        &source.id,
        &request,
        &BTreeMap::new(),
        evidence::Anchor {
            source_id: source.id.clone(),
            snapshot_us: BASE,
            trading_day: "20261117".into(),
            evidence_sequence: 8,
            evidence_sha256: hex::encode(Sha256::digest(raw)),
            store_id: evidence::generation(source, db)?,
            note: request.note.clone(),
        },
    )?;
    netting::store::initialize(pool, &portfolio, &request, user).await?;
    let mut tx = pool.begin().await?;
    for (second, alpha_target, seq, qty) in [(30, 2, 12, 1), (90, 1, 14, -1)] {
        let decision_us = BASE + second * 1_000_000;
        let target = netting::TargetBatch {
            strategy_group: "fixture".into(),
            batch_id: format!("synthetic-batch-{seq}"),
            expected_revision: portfolio.revision,
            decision_us,
            trading_day: Some("20261117".into()),
            signal_times: BTreeMap::new(),
            strategies: BTreeMap::from([
                (
                    "alpha".into(),
                    BTreeMap::from([(market.into(), alpha_target)]),
                ),
                ("beta".into(), BTreeMap::from([(market.into(), -1)])),
            ]),
            quotes: BTreeMap::from([(
                market.into(),
                netting::Quote {
                    bid: 99.0,
                    ask: 101.0,
                    observed_us: decision_us,
                    source: "synthetic".into(),
                },
            )]),
        };
        let (next, receipt) = portfolio.batch(&target, decision_us)?;
        portfolio = next;
        sqlx::query(
            "INSERT INTO cta_native_netting_events(source_id,event_id,revision,kind,fingerprint,request,receipt,actor_user_id,received_at_us)
             VALUES($1,$2,$3,'target_batch',$4,$5,$6,$7,$8)",
        ).bind(&source.id).bind(&target.batch_id).bind(portfolio.revision)
            .bind(netting::fingerprint(&target)?).bind(DbJson(&target)).bind(DbJson(&receipt))
            .bind(user).bind(decision_us).execute(&mut *tx).await?;
        let key = format!("trade-{seq}");
        let raw = db
            .get_cf(db.cf_handle("cn_trade_facts").unwrap(), &key)?
            .unwrap();
        let fill = netting::FactualFill::from_evidence(&serde_json::from_slice(&raw)?)?;
        let allocation = BTreeMap::from([("alpha".into(), qty)]);
        portfolio = portfolio.fill(&fill, &allocation)?;
        let receipt = json!({"source_id":source.id,"revision":portfolio.revision,
            "evidence_key":hex::encode(key),"fill":fill,"allocations":allocation});
        sqlx::query(
            "INSERT INTO cta_native_netting_events(source_id,event_id,revision,kind,fingerprint,request,receipt,actor_user_id,received_at_us)
             VALUES($1,$2,$3,'exchange_fill_allocation',$4,$5,$5,$6,$7)",
        ).bind(&source.id).bind(format!("fixture-fill-{seq}")).bind(portfolio.revision)
            .bind(netting::fingerprint(&receipt)?).bind(DbJson(&receipt))
            .bind(user).bind(fill.received_us).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE cta_native_netting_portfolios SET state=$2 WHERE source_id=$1")
        .bind(&source.id)
        .bind(DbJson(&portfolio))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
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
        ctaker_plan_readers: BTreeMap::new(),
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
    let (start, end) = (BASE, BASE + 180_000_000);
    let missing = browser_timeline(
        &client,
        &base,
        "account-timeline",
        &reader_cookie,
        "portfolio",
        start,
        end,
        None,
    )
    .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        missing.json::<Value>().await?["error"]
            .as_str()
            .unwrap()
            .contains("快照")
    );
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
    let symbol = decode_timeline(
        browser_timeline(
            &client,
            &base,
            "account-timeline",
            &reader_cookie,
            "symbols",
            start,
            end + 20_000_000,
            Some("SHFE:cu2612"),
        )
        .await?,
    )
    .await?;
    assert_eq!(symbol.curves.len(), 2);
    assert_eq!(symbol.curves[1].kind, wire::CurveKind::Symbol as i32);
    assert_eq!(symbol.curves[1].key, "SHFE:cu2612");
    assert_eq!(terminal(&symbol.curves[0], wire::Metric::NavAfterFee), 96.0);
    assert_eq!(
        terminal(&symbol.curves[1], wire::Metric::GrossPosition),
        2200.0
    );
    assert_eq!(terminal(&symbol.curves[1], wire::Metric::NetPosition), 0.0);
    let metadata = symbol.metadata.unwrap();
    let Some(prost_types::value::Kind::StructValue(report)) = &metadata.fields["report"].kind
    else {
        anyhow::bail!("expected report");
    };
    assert_eq!(
        report.fields["end_ts_us"].kind,
        Some(prost_types::value::Kind::NumberValue(end as f64))
    );
    let absent_symbol = browser_timeline(
        &client,
        &base,
        "account-timeline",
        &reader_cookie,
        "symbols",
        start,
        end,
        Some("DCE:m2701"),
    )
    .await?;
    assert_eq!(
        absent_symbol.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );

    persist_attribution(&pool, &db, &nav_source).await?;
    let forbidden = browser_timeline(
        &client,
        &base,
        "timeline",
        &reader_cookie,
        "strategies",
        start,
        end,
        None,
    )
    .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    let forbidden_portfolio = browser_timeline(
        &client,
        &base,
        "timeline",
        &reader_cookie,
        "portfolio",
        start,
        end,
        None,
    )
    .await?;
    assert_eq!(forbidden_portfolio.status(), reqwest::StatusCode::FORBIDDEN);
    sqlx::query(
        "INSERT INTO cta_position_strategy_grants(strategy_name,user_id,access_level)
         VALUES('alpha',$1,'view'),('beta',$1,'view')",
    )
    .bind(user.user_id)
    .execute(&pool)
    .await?;
    let strategy = decode_timeline(
        browser_timeline(
            &client,
            &base,
            "timeline",
            &reader_cookie,
            "strategies",
            start,
            end,
            None,
        )
        .await?,
    )
    .await?;
    assert_eq!(
        strategy
            .curves
            .iter()
            .map(|curve| curve.key.as_str())
            .collect::<Vec<_>>(),
        ["", "alpha", "beta"]
    );
    assert_eq!(
        terminal(&strategy.curves[0], wire::Metric::NavAfterFee),
        96.0
    );
    assert_eq!(
        terminal(&strategy.curves[1], wire::Metric::NavAfterFee),
        196.0
    );
    assert_eq!(
        terminal(&strategy.curves[2], wire::Metric::NavAfterFee),
        -100.0
    );

    let strategy_symbol = decode_timeline(
        browser_timeline(
            &client,
            &base,
            "timeline",
            &reader_cookie,
            "symbols",
            start,
            end,
            Some("SHFE:cu2612"),
        )
        .await?,
    )
    .await?;
    assert_eq!(strategy_symbol.curves.len(), 2);
    assert_eq!(
        strategy_symbol.curves[1].kind,
        wire::CurveKind::Symbol as i32
    );
    assert_eq!(
        terminal(&strategy_symbol.curves[1], wire::Metric::NavAfterFee),
        96.0
    );

    let rebased = decode_timeline(
        browser_timeline(
            &client,
            &base,
            "timeline",
            &reader_cookie,
            "strategies",
            BASE + 60_001_000,
            end,
            None,
        )
        .await?,
    )
    .await?;
    assert_eq!(terminal(&rebased.curves[0], wire::Metric::TradingFee), 3.0);
    assert_eq!(
        terminal(&rebased.curves[0], wire::Metric::NavAfterFee),
        97.0
    );
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
    let uncovered = browser_timeline(
        &client,
        &base,
        "timeline",
        &reader_cookie,
        "strategies",
        start,
        BASE + 240_000_000,
        None,
    )
    .await?;
    assert_eq!(
        uncovered.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
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
