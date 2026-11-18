//! Only an isolated test PostgreSQL socket is accepted. No trading SDK is linked.
use anyhow::{Context, Result, ensure};
use crypto_cta_manager::{
    auth,
    native_futures::{
        config::{Config, Source},
        web,
    },
    postgres,
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe,
    postgres::{PgConnectOptions, PgPoolOptions},
};

#[tokio::test]
#[ignore = "requires an isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL cluster"]
async fn native_observation_preserves_permission_boundaries_and_never_submits_orders() -> Result<()>
{
    let socket = std::env::var("CTA_SCHEMA_TEST_SOCKET").context("set isolated test socket")?;
    ensure!(
        socket.starts_with("/tmp/cta-schema-test-")
            && std::path::Path::new(&socket)
                .join(".s.PGSQL.15433")
                .exists(),
        "only isolated test sockets on 15433 are accepted"
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
        "cn_manager_test_{}_{}",
        std::process::id(),
        crypto_cta_manager::native_futures::now_us()
    );
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.database(&name))
        .await?;
    postgres::initialize(&pool).await?;
    let tmp = tempfile::tempdir()?;
    let primary = tmp.path().join("exec");
    let mut opts = rocksdb::Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    let db = rocksdb::DB::open_cf_descriptors(
        &opts,
        &primary,
        ["cn_trading_events", "cn_uniform_orders", "cn_trade_facts"]
            .into_iter()
            .map(|cf| rocksdb::ColumnFamilyDescriptor::new(cf, rocksdb::Options::default())),
    )?;
    let fixture: Value = serde_json::from_str(include_str!("fixtures/yida_native_records.json"))?;
    for (cf, key) in [
        ("cn_trading_events", "state"),
        ("cn_uniform_orders", "order"),
        ("cn_trade_facts", "fill"),
    ] {
        db.put_cf(
            db.cf_handle(cf).unwrap(),
            "001",
            serde_json::to_vec(&fixture[key])?,
        )?;
    }
    let config = Config {
        mode: crypto_cta_manager::runtime_mode::RuntimeMode::CnFutures,
        database: crypto_cta_manager::config::DatabaseConfig {
            url_env: "CN_TEST_ONLY".into(),
            max_connections: 4,
        },
        secondary_root: tmp.path().join("secondary"),
        reference_root: None,
        market_data: None,
        reference_service: None,
        sync: None,
        sync_export: None,
        recent_rows: 20,
        stale_secs: 30,
        source_viewers: std::collections::BTreeMap::from([(
            "hidden".into(),
            vec!["operator".into()],
        )]),
        ctaker_plan_readers: Default::default(),
        sources: vec![
            Source {
                id: "sim".into(),
                account: "测试".into(),
                adapter: "yida".into(),
                rocksdb_path: primary.clone(),
                enabled: true,
                sync: None,
            },
            Source {
                id: "hidden".into(),
                account: "隐藏".into(),
                adapter: "ctp".into(),
                rocksdb_path: tmp.path().join("hidden"),
                enabled: true,
                sync: None,
            },
        ],
    };
    web::register_sources(&pool, &config).await?;
    let catalog: Vec<(String, String)> =
        sqlx::query_as("SELECT source_id, venue_label FROM cta_order_sources ORDER BY source_id")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        catalog,
        vec![
            ("hidden".into(), "ctp".into()),
            ("sim".into(), "yida".into())
        ]
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let frontend = tmp.path().join("frontend");
    std::fs::create_dir_all(frontend.join("assets"))?;
    std::fs::write(frontend.join("index.html"), "<title>国内期货 CTA</title>")?;
    std::fs::write(frontend.join("assets/app.js"), "console.log('native')")?;
    let app = web::with_frontend(web::Service::new(config, pool.clone())?.router(), &frontend)?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = Client::new();
    let page = client.get(format!("{address}/manager/")).send().await?;
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.text().await?.contains("国内期货 CTA"));
    let asset = client
        .get(format!("{address}/manager/assets/app.js"))
        .send()
        .await?;
    assert_eq!(asset.status(), StatusCode::OK);
    assert!(
        asset.headers()["content-type"]
            .to_str()?
            .contains("javascript")
    );
    assert_eq!(asset.text().await?, "console.log('native')");
    assert_eq!(
        client
            .get(format!("{address}/manager/api/native/sources"))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{address}/manager/api/does-not-exist"))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    let status: Value = client
        .get(format!("{address}/manager/api/auth/status"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(status["setup_required"], true);
    assert_eq!(
        client
            .get(format!("{address}/api/native/sources"))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = client
        .post(format!("{address}/api/auth/register"))
        .json(&json!({"username":"admin","password":"native-test-password"}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(cookie.starts_with("cn_cta_session="));
    let bootstrap: Value = response.json().await?;
    let admin_id = bootstrap["user"]["user_id"]
        .as_i64()
        .context("bootstrap identity")?;
    auth::set_sources(
        &pool,
        admin_id,
        &[auth::SourceGrantInput {
            source_id: "hidden".into(),
            access_level: "configure".into(),
        }],
        &["sim".to_owned(), "hidden".to_owned()]
            .into_iter()
            .collect(),
    )
    .await?;
    let login = client
        .post(format!("{address}/api/auth/login"))
        .json(&json!({"username":"admin","password":"native-test-password"}))
        .send()
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let login: Value = login.json().await?;
    assert!(
        login["user"]["source_grants"]
            .as_array()
            .unwrap()
            .is_empty(),
        "host-hidden account grants must not leak through login"
    );
    let user = auth::create_user(
        &pool,
        auth::CreateUserRequest {
            username: "reader".into(),
            password: "native-test-password".into(),
            role: None,
        },
    )
    .await?;
    let other = auth::create_user(
        &pool,
        auth::CreateUserRequest {
            username: "other".into(),
            password: "native-test-password".into(),
            role: None,
        },
    )
    .await?;
    let reader_token = auth::create_session(&pool, user.user_id).await?.0;
    let reader_cookie = format!("cn_cta_session={reader_token}");
    let other_token = auth::create_session(&pool, other.user_id).await?.0;
    let other_cookie = format!("cn_cta_session={other_token}");
    let get = |path: &str, cookie: &str| {
        client
            .get(format!("{address}{path}"))
            .header("Cookie", cookie.to_owned())
    };
    let post = |path: &str, cookie: &str, value: Value| {
        client
            .post(format!("{address}{path}"))
            .header("Cookie", cookie.to_owned())
            .json(&value)
    };
    assert!(
        get("/api/native/sources", &reader_cookie)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?
            .is_empty()
    );
    assert_eq!(
        get("/api/native/sources/sim/snapshot", &reader_cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    for path in [
        "/api/native/sources/sim/market?venue=SHFE&symbol=cu2612&trading_day=20261117",
        "/api/native/sources/sim/rates?venue=SHFE&symbol=cu2612&at_us=1794895200000000",
    ] {
        assert_eq!(
            get(path, &reader_cookie).send().await?.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(format!("{address}{path}"))
                .send()
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let sources = get("/api/native/sources", &cookie)
        .send()
        .await?
        .json::<Vec<Value>>()
        .await?;
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["source_id"], "sim");
    assert_eq!(sources[0]["execution_enabled"], false);
    assert_eq!(
        get("/api/native/sources/hidden/snapshot", &cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let grants = |level: &str| json!({"grants":[{"user_id":user.user_id,"access_level":level}]});
    assert_eq!(
        client
            .put(format!("{address}/api/native/sources/sim/grants"))
            .header("Cookie", &cookie)
            .json(&grants("view"))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    let observed = get("/api/native/sources/sim/snapshot", &reader_cookie)
        .send()
        .await?;
    assert_eq!(observed.status(), StatusCode::OK);
    let observed: Value = observed.json().await?;
    assert_eq!(observed["total_orders"], 1);
    assert_eq!(observed["total_fills"], 1);
    let target =
        json!({"strategy_name":"native_alpha","targets":{"SHFE:cu2612":{"qty":1,"signal":0}}});
    assert_eq!(
        post("/api/native/strategies", &reader_cookie, target.clone())
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    assert!(
        get("/api/native/strategies", &other_cookie)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?
            .is_empty()
    );
    assert_eq!(
        post("/api/native/strategies", &other_cookie, target.clone())
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut invalid = target.clone();
    invalid["targets"]["SHFE:cu2612"]["qty"] = json!(0.5);
    assert_eq!(
        post("/api/native/strategies", &reader_cookie, invalid)
            .send()
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    let grant_url = format!("{address}/api/native/strategies/native_alpha/grants");
    assert_eq!(client.put(&grant_url).header("Cookie",&reader_cookie).json(&json!({"grants":[{"user_id":other.user_id,"access_level":"view"}],"open_visibility":false})).send().await?.status(),StatusCode::OK);
    assert_eq!(
        get("/api/native/strategies", &other_cookie)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?
            .len(),
        1
    );
    assert_eq!(
        post("/api/native/strategies", &other_cookie, target.clone())
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(client.put(&grant_url).header("Cookie",&reader_cookie).json(&json!({"grants":[{"user_id":other.user_id,"access_level":"configure"}],"open_visibility":false})).send().await?.status(),StatusCode::OK);
    assert_eq!(
        post("/api/native/strategies", &other_cookie, target)
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get("/api/native/sources/sim/snapshot", &other_cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(format!("{address}/api/native/sources/sim/snapshot"))
            .header("Cookie", &cookie)
            .send()
            .await?
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        client
            .put(format!("{address}/api/native/sources/sim/grants"))
            .header("Cookie", &cookie)
            .json(&json!({"grants":[]}))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get("/api/native/sources/sim/snapshot", &reader_cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    // The live primary remains open and writable, with its factual rows unchanged.
    assert_eq!(
        serde_json::from_slice::<Value>(
            &db.get_cf(db.cf_handle("cn_uniform_orders").unwrap(), "001")?
                .unwrap()
        )?,
        fixture["order"]
    );
    db.put_cf(
        db.cf_handle("cn_trading_events").unwrap(),
        "002",
        b"test-primary-still-writable",
    )?;
    server.abort();
    let _ = server.await;
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}
