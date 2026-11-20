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
        nav::{self, Offset, Side},
        netting,
        records::{self, fixture},
        web,
    },
    postgres,
};
use prost::Message;
use serde_json::{Value, json};
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
/// One factual fill of its own one-lot order, and Exec's attribution of it to
/// `strategy` one microsecond later.
fn trade(
    db: &rocksdb::DB,
    seq: i64,
    side: Side,
    offset: Offset,
    price: f64,
    at: i64,
    trading_day: &str,
    strategy: &str,
    commission: Option<f64>,
) {
    let order = fixture::Order::new(
        "SHFE:cu2612",
        trading_day,
        "ctp:1:2",
        &seq.to_string(),
        side,
        offset,
        1,
        price,
    );
    let fill = fixture::Fill {
        trade_id: format!("trade-{seq}"),
        lots: 1,
        price,
        commission,
        order_id: seq,
    };
    fixture::put_trade(db, at, &order.trade(&fill, 1, Some(fixture::FILLED), at));
    fixture::put_order(
        db,
        at + 1,
        &order.share(&fill, strategy, 1, commission, fixture::FILLED, at + 1),
    );
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
    let market = "SHFE:cu2612";
    let request = netting::Initialize {
        strategy_groups: BTreeMap::from([(
            "fixture".into(),
            BTreeSet::from(["alpha".into(), "beta".into()]),
        )]),
        contracts: BTreeMap::from([(market.into(), netting::Contract { multiplier: 10.0 })]),
        positions: BTreeMap::new(),
        note: "synthetic flat net strategy anchor before Exec attribution".into(),
    };
    let mut portfolio = netting::Portfolio::initialize(
        &source.id,
        &request,
        &records::strategy_positions(db, 0)?,
        netting::Anchor {
            source_id: source.id.clone(),
            snapshot_us: BASE,
            orders_key: 0,
            store_id: records::generation(source, db)?,
            note: request.note.clone(),
        },
    )?;
    netting::store::initialize(pool, &portfolio, &request, user).await?;
    let mut tx = pool.begin().await?;
    for (second, alpha_target) in [(30, 2), (90, 1)] {
        let decision_us = BASE + second * 1_000_000;
        let target = netting::TargetBatch {
            strategy_group: "fixture".into(),
            batch_id: format!("synthetic-batch-{second}"),
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
        };
        let (next, receipt) = portfolio.batch(&target, decision_us)?;
        portfolio = next;
        sqlx::query(
            "INSERT INTO cta_native_netting_events(source_id,event_id,revision,kind,fingerprint,request,receipt,actor_user_id,received_at_us)
             VALUES($1,$2,$3,'target_batch',$4,$5,$6,$7,$8)",
        ).bind(&source.id).bind(&target.batch_id).bind(portfolio.revision)
            .bind(netting::fingerprint(&target)?).bind(DbJson(&target)).bind(DbJson(&receipt))
            .bind(user).bind(decision_us).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE cta_native_netting_portfolios SET state=$2 WHERE source_id=$1")
        .bind(&source.id)
        .bind(DbJson(&portfolio))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // Exec's internal cross at the mid: alpha takes beta's lot. Its two factual
    // fills were already attributed to alpha in their uniform-order shares.
    for (leg, (side, name)) in [(Side::Buy, "alpha"), (Side::Sell, "beta")]
        .into_iter()
        .enumerate()
    {
        fixture::put_order(
            db,
            BASE + 30_000_000 + leg as i64,
            &fixture::cross_leg(
                market,
                "20261117",
                BASE + 30_000_000,
                0,
                side,
                name,
                1,
                100.0,
                BASE + 30_000_000,
            ),
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL cluster"]
async fn native_nav_http_preserves_gross_anchor_permissions_fee_history_and_trading_day_cut()
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
    let db = fixture::open(&primary);
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
        exec_plan_readers: BTreeMap::new(),
        execution_monitor_urls: BTreeMap::new(),
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
    // The operator states the (flat) inventory now; the trade log is cut at its tip.
    let body =
        json!({"trading_day":"20261117","positions":[],"note":"synthetic flat current anchor"});
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
    let captured: nav::Anchor = response.json().await?;
    assert_eq!(captured.trades_key, 0);
    assert!(captured.positions.is_empty());
    let malformed = json!({"trading_day":"20261117","positions":[{"venue":"SHFE","symbol":"cu2612","side":"buy","today":-1,"yesterday":0}],"note":"negative"});
    assert_eq!(
        client
            .post(&snapshots)
            .header("Cookie", &cookie)
            .json(&malformed)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    // A historical gross hedge, priced at that minute, for the replayed window.
    let gross = |side| nav::InitialPosition {
        venue: "SHFE".into(),
        symbol: "cu2612".into(),
        side,
        today: 1,
        yesterday: 0,
        reference_price: 100.0,
        contract_multiplier: 10.0,
    };
    nav::store::insert(
        &pool,
        &nav::Anchor {
            source_id: "sim".into(),
            snapshot_us: BASE,
            trading_day: "20261117".into(),
            trades_key: 0,
            store_id: records::generation(&nav_source, &db)?,
            positions: vec![gross(Side::Buy), gross(Side::Sell)],
            note: "synthetic complete hedged anchor".into(),
        },
        "admin",
    )
    .await?;
    trade(
        &db,
        12,
        Side::Buy,
        Offset::Open,
        100.0,
        BASE + 60_000_000,
        "20261117",
        "alpha",
        Some(1.0),
    );
    trade(
        &db,
        14,
        Side::Sell,
        Offset::CloseToday,
        110.0,
        BASE + 120_000_000,
        "20261117",
        "alpha",
        None,
    );
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
    // A local store covers its records through the read itself, so the
    // requested end (already in the past) is served unchanged.
    assert_eq!(
        report.fields["end_ts_us"].kind,
        Some(prost_types::value::Kind::NumberValue(
            (end + 20_000_000) as f64
        ))
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
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1]["snapshot_us"], BASE);
    assert_eq!(stored[1]["positions"].as_array().unwrap().len(), 2);
    let mut conflicting: nav::Anchor = serde_json::from_value(stored[1].clone())?;
    use crypto_cta_manager::native_futures::nav::source::validate_anchor;
    validate_anchor(&nav_source, &db, &conflicting)?;
    conflicting.store_id = "native:replacement-store".into();
    assert!(validate_anchor(&nav_source, &db, &conflicting).is_err());
    conflicting.store_id = records::generation(&nav_source, &db)?;
    conflicting.trades_key = i64::MAX;
    assert!(validate_anchor(&nav_source, &db, &conflicting).is_err());
    conflicting.trades_key = 0;
    conflicting.note = "different immutable financial anchor".into();
    assert!(
        crypto_cta_manager::native_futures::nav::store::insert(&pool, &conflicting, "admin")
            .await
            .is_err()
    );
    // A fill of an earlier trading day recorded after the anchor may already be
    // in its inventory; NAV stays unavailable rather than double counting it.
    // Its share names a strategy outside the roster, so strategy NAV stops too.
    trade(
        &db,
        18,
        Side::Buy,
        Offset::Open,
        100.0,
        BASE + 200_000_000,
        "20261116",
        "gamma",
        None,
    );
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
