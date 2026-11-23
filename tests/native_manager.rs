//! Only an isolated test PostgreSQL socket is accepted. No trading SDK is linked.
use anyhow::{Context, Result, ensure};
use crypto_cta_manager::{
    auth,
    native_futures::{
        config::{Config, Source},
        nav::Offset,
        records::{self, fixture},
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
    let db = fixture::open(&primary);
    // Synthetic Yida-shaped records in the Exec binary layouts.
    let at = crypto_cta_manager::native_futures::now_us();
    let placed = fixture::Order::new(
        "SHFE:cu2612",
        "20261112",
        "yida:20261112:group:1",
        "10",
        crypto_cta_manager::native_futures::nav::Side::Buy,
        Offset::Open,
        1,
        105_000.0,
    );
    let fill = fixture::Fill {
        trade_id: "18446744073709551614".into(),
        lots: 1,
        price: 105_000.0,
        commission: None,
        order_id: 0,
    };
    let lifecycle = placed.lifecycle(fixture::FILLED, "exec", at, at + 2);
    fixture::put_trade(
        &db,
        at + 1,
        &placed.trade(&fill, 1, Some(fixture::FILLED), at + 1),
    );
    fixture::put_order(&db, at + 2, &lifecycle);
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
        exec_plan_readers: Default::default(),
        execution_monitor_urls: [(
            "sim".into(),
            "https://viz.example.test/?source=paper".into(),
        )]
        .into(),
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
    for route in [
        "/manager/",
        "/manager/workspace",
        "/manager/accounts/",
        "/manager/exec/",
        "/manager/virtual/",
        "/manager/docs/",
        "/manager/account/?source=sim",
        "/manager/net-value/",
        "/manager/acquisition-cost/?source=sim",
        "/manager/config/position/",
        "/manager/config/order/",
        "/manager/config/bindings/",
        "/manager/admin/",
    ] {
        let page = client.get(format!("{address}{route}")).send().await?;
        assert_eq!(page.status(), StatusCode::OK, "{route}");
        assert!(page.text().await?.contains("国内期货 CTA"), "{route}");
    }
    let no_redirect = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    for route in ["/manager/auto-earn", "/manager/auto-earn/"] {
        let page = no_redirect.get(format!("{address}{route}")).send().await?;
        assert_eq!(page.status(), StatusCode::TEMPORARY_REDIRECT, "{route}");
        assert_eq!(page.headers()["location"], "/manager/workspace/", "{route}");
    }
    let missing = client
        .get(format!("{address}/manager/assets/missing.js"))
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert!(!missing.text().await?.contains("国内期货 CTA"));
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
    assert_eq!(status["auto_earn_available"], false);
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
    assert_eq!(
        client
            .post(format!("{address}/manager/api/auth/register"))
            .json(&json!({"username":"unexpected","password":"native-test-password"}))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    for api in [
        "/manager/api/health",
        "/manager/api/account-overview",
        "/manager/api/dashboard?includeSymbols=false",
        "/manager/api/catalog/position-strategies",
        "/manager/api/catalog/order-strategies",
        "/manager/api/catalog/virtual-accounts",
        "/manager/api/catalog/acquisition-cost?sourceIds=sim",
    ] {
        let response = client
            .get(format!("{address}{api}"))
            .header("cookie", &cookie)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "{api}");
        assert!(
            response.headers()["content-type"]
                .to_str()?
                .starts_with("application/json")
        );
    }
    let overview: Value = client
        .get(format!("{address}/manager/api/account-overview"))
        .header("cookie", &cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(overview["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(overview["accounts"][0]["source_id"], "sim");
    assert_eq!(overview["accounts"][0]["quote_currency"], "CNY");
    assert_eq!(overview["accounts"][0]["collection_status"], "loading");
    assert!(
        overview["accounts"][0]
            .get("collection_status_reason")
            .is_some()
    );
    // No runtime status or SDK valuation; a local source without a funds
    // relay reports every broker funds field as unavailable.
    for field in ["execution_status", "sdk_market_value"] {
        assert!(overview["accounts"][0].get(field).is_none(), "{field}");
    }
    for field in [
        "funds_status",
        "equity",
        "yesterday_equity",
        "today_profit",
        "margin",
        "max_drawdown_15d_percent",
    ] {
        assert!(overview["accounts"][0][field].is_null(), "{field}");
    }
    let dashboard: Value = client
        .get(format!(
            "{address}/manager/api/dashboard?includeSymbols=false"
        ))
        .header("cookie", &cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(dashboard["accounts"].as_array().unwrap().len(), 1);
    let account = &dashboard["accounts"][0];
    assert_eq!(account["source_id"], "sim");
    assert_eq!(
        account["execution_monitor_url"],
        "https://viz.example.test/?source=paper"
    );
    assert_eq!(account["quote_currency"], "CNY");
    assert_eq!(account["can_edit_alias"], true);
    assert_eq!(account["configurable"], false);
    assert!(account["open_position_symbol_count"].is_null());
    assert!(account["account_funds"].is_null());
    assert_eq!(account["live_equity_status"], "unavailable");
    assert!(dashboard["report"]["sources"][0]["nav_change_after_fee_quote"].is_null());
    use crypto_cta_manager::native_futures::nav::{self, Anchor, InitialPosition, Side};
    nav::store::insert(
        &pool,
        &Anchor {
            source_id: "sim".into(),
            snapshot_us: 1_794_895_200_000_000,
            trading_day: "20261117".into(),
            trades_key: 0,
            store_id: "native:isolated-fixture".into(),
            positions: vec![
                InitialPosition {
                    venue: "SHFE".into(),
                    symbol: "cu2612".into(),
                    side: Side::Buy,
                    today: 1,
                    yesterday: 2,
                    reference_price: 105_000.0,
                    contract_multiplier: 5.0,
                },
                InitialPosition {
                    venue: "SHFE".into(),
                    symbol: "cu2612".into(),
                    side: Side::Sell,
                    today: 0,
                    yesterday: 1,
                    reference_price: 105_000.0,
                    contract_multiplier: 5.0,
                },
            ],
            note: "Isolated snapshot fixture".into(),
        },
        "admin",
    )
    .await?;
    let historical: Value = client
        .get(format!(
            "{address}/manager/api/account-overview?date=2026-11-17"
        ))
        .header("Cookie", &cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(historical["dates"], json!(["2026-11-17"]));
    let row = &historical["accounts"][0];
    assert_eq!(row["collection_status"], "historical");
    assert_eq!(row["long_position"], 1_575_000.0);
    assert_eq!(row["short_position"], 525_000.0);
    assert_eq!(row["net_position"], 1_050_000.0);
    assert!(row["equity"].is_null());
    let hidden = client
        .get(format!("{address}/manager/api/catalog/accounts/hidden"))
        .header("cookie", &cookie)
        .send()
        .await?;
    assert_eq!(hidden.status(), StatusCode::FORBIDDEN);

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
    assert_eq!(
        get("/api/catalog/accounts/sim/contract-leverage", &cookie)
            .send()
            .await?
            .status(),
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        get(
            "/api/catalog/accounts/sim/contract-leverage",
            &reader_cookie
        )
        .send()
        .await?
        .status(),
        StatusCode::FORBIDDEN
    );
    let virtual_path = "/api/catalog/virtual-accounts";
    let order_path = "/api/catalog/order-strategies";
    let template = json!({
        "strategy_name": "private_execution_template",
        "order_parameters": crypto_cta_manager::order_config::OrderParameters::default(),
    });
    let order: Value = post(order_path, &reader_cookie, template.clone())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(order["owner"], "reader");
    assert_eq!(order["can_configure"], true);
    assert!(
        get(order_path, &other_cookie)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?
            .is_empty()
    );
    assert_eq!(
        post(order_path, &other_cookie, template)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let composition: Value = post(
        virtual_path,
        &reader_cookie,
        json!({"name":"私有组合","bindings":[],"children":[]}),
    )
    .send()
    .await?
    .error_for_status()?
    .json()
    .await?;
    let virtual_id = composition["virtual_id"]
        .as_str()
        .context("saved Virtual account")?;
    assert_eq!(composition["can_configure"], true);
    assert_eq!(composition["can_manage_grants"], true);
    assert!(
        get(virtual_path, &other_cookie)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?
            .is_empty()
    );
    assert!(
        get("/api/catalog/virtual-accounts/users", &other_cookie)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?
            .is_empty()
    );
    let virtual_url = format!("{address}{virtual_path}/{virtual_id}");
    assert_eq!(
        client
            .put(&virtual_url)
            .header("Cookie", &other_cookie)
            .json(&json!({"name":"unauthorized","bindings":[],"children":[]}))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    for (assigned, expected) in [
        (json!([other.user_id]), StatusCode::OK),
        (json!([]), StatusCode::FORBIDDEN),
    ] {
        client
            .put(format!("{virtual_url}/grants"))
            .header("Cookie", &reader_cookie)
            .json(&json!({"user_ids": assigned}))
            .send()
            .await?
            .error_for_status()?;
        assert_eq!(
            client
                .put(&virtual_url)
                .header("Cookie", &other_cookie)
                .json(&json!({"name":"托管组合","bindings":[],"children":[]}))
                .send()
                .await?
                .status(),
            expected
        );
    }
    assert_eq!(
        client
            .delete(&virtual_url)
            .header("Cookie", &reader_cookie)
            .send()
            .await?
            .status(),
        StatusCode::NO_CONTENT
    );
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
    assert_eq!(observed["orders"][0]["phase"], "filled");
    assert_eq!(observed["orders"][0]["order"]["order"]["order_ref"], "10");
    assert_eq!(observed["fills"][0]["trade_id"], "18446744073709551614");
    assert!(observed["fills"][0]["commission_cny"].is_null());
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
    // Account-specific native strategy visibility still applies to an admin
    // when the host configuration excludes the strategy's source.
    sqlx::query("INSERT INTO cta_native_netting_portfolios(source_id,state,updated_at_us) VALUES('hidden','{}'::jsonb,1)")
        .execute(&pool).await?;
    sqlx::query("INSERT INTO cta_native_netting_members(source_id,strategy_group,strategy_name) VALUES('hidden','group','native_alpha')")
        .execute(&pool).await?;
    for path in [
        "/api/catalog/position-strategies",
        "/api/catalog/position-strategies-access",
        "/api/native/strategies",
    ] {
        assert!(
            get(path, &cookie)
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<Value>>()
                .await?
                .is_empty()
        );
    }
    for path in ["/api/catalog/position-strategies", "/api/native/strategies"] {
        assert_eq!(
            post(
                path,
                &cookie,
                json!({"strategy_name":"native_alpha","targets":{}})
            )
            .send()
            .await?
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    for path in [
        "/api/catalog/position-strategies/native_alpha/grants",
        "/api/native/strategies/native_alpha/grants",
    ] {
        assert_eq!(
            client
                .put(format!("{address}{path}"))
                .header("Cookie", &cookie)
                .json(&json!({"grants":[],"open_visibility":false}))
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        get("/api/native/strategies/native_alpha/grants", &cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    // The owner can see the same strategy after acquiring source view
    // permission. Editing still requires the account's separate grant.
    sqlx::query("INSERT INTO cta_native_netting_portfolios(source_id,state,updated_at_us) VALUES('sim','{}'::jsonb,1)")
        .execute(&pool).await?;
    sqlx::query(
        "UPDATE cta_native_netting_members SET source_id='sim' WHERE strategy_name='native_alpha'",
    )
    .execute(&pool)
    .await?;
    assert_eq!(
        client
            .put(format!("{address}/api/catalog/accounts/sim/grants"))
            .header("Cookie", &cookie)
            .json(&grants("view"))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    let visible: Vec<Value> = get("/api/native/strategies", &reader_cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["strategy"]["strategy_name"], "native_alpha");
    assert_eq!(visible[0]["can_configure"], false);
    assert_eq!(
        get("/api/native/strategies/native_alpha/grants", &reader_cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    // The live primary remains open and writable, with its factual rows unchanged.
    assert_eq!(
        db.get_cf(db.cf_handle(records::ORDERS).unwrap(), records::key(at + 2))?
            .as_deref(),
        Some(lifecycle.as_slice())
    );
    fixture::put_order(
        &db,
        at + 3,
        &placed.lifecycle(fixture::FILLED, "exec", at, at + 3),
    );
    server.abort();
    let _ = server.await;
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}
