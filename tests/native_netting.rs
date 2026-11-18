//! Isolated PostgreSQL only; no exchange connection, SDK or live database.
use anyhow::{Context, Result, ensure};
use crypto_cta_manager::{
    native_futures::{
        netting::{self, store},
        now_us,
    },
    postgres,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    AssertSqlSafe,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::collections::{BTreeMap, BTreeSet};

#[tokio::test]
#[ignore = "requires isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL cluster"]
async fn atomic_batches_survive_reconnects_deduplicate_fills_and_serialize_writers() -> Result<()> {
    let socket = std::env::var("CTA_SCHEMA_TEST_SOCKET")?;
    ensure!(
        socket.starts_with("/tmp/cta-schema-test-")
            && std::path::Path::new(&socket)
                .join(".s.PGSQL.15433")
                .exists(),
        "isolated socket on 15433 required"
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
    let database = format!("cn_netting_{}_{}", std::process::id(), now_us());
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.clone().database(&database))
        .await?;
    postgres::initialize(&pool).await?;
    sqlx::raw_sql("INSERT INTO cta_users(user_id,username,password_hash,role) VALUES(1,'operator','test-only','admin'); INSERT INTO cta_order_sources(source_id,account_label,venue_label,rocksdb_path) VALUES('source','test','ctp','/isolated/test'); INSERT INTO cta_position_strategies(strategy_name,updated_at_us,created_by_user_id) VALUES('a-1h',1,1),('b-1h',1,1);").execute(&pool).await?;
    // Rehearse explicit maintenance on an existing catalog, not only fresh DDL.
    sqlx::raw_sql("DROP TABLE cta_native_netting_publishers; DROP TABLE cta_native_netting_demands; DROP TABLE cta_native_netting_members; DROP TABLE cta_native_netting_events; DROP TABLE cta_native_netting_portfolios;").execute(&pool).await?;
    sqlx::raw_sql(include_str!("../scripts/apply_native_netting_schema.sql"))
        .execute(&pool)
        .await?;
    let catalog_count: i64 = sqlx::query_scalar("SELECT count(*) FROM cta_position_strategies")
        .fetch_one(&pool)
        .await?;
    assert_eq!(catalog_count, 2);
    let temp = tempfile::tempdir()?;
    let primary = temp.path().join("execution-evidence");
    let mut native_options = rocksdb::Options::default();
    native_options.create_if_missing(true);
    native_options.create_missing_column_families(true);
    let native = rocksdb::DB::open_cf(
        &native_options,
        &primary,
        ["cn_trading_events", "cn_uniform_orders", "cn_trade_facts"],
    )?;
    let native_source = crypto_cta_manager::native_futures::config::Source {
        id: "source".into(),
        account: "test".into(),
        adapter: "ctp".into(),
        rocksdb_path: primary.clone(),
        enabled: true,
        sync: None,
    };
    let anchor_us = now_us() - 10_000_000;
    let evidence = serde_json::to_vec(
        &json!({"sequence":10,"received_us":anchor_us,"projection_error":null,"value":{"type":"reconciled","state":{"ready":false,"connected":true,"reason":"settlement not confirmed; read-only queries remain available","positions":[],"orders":[],"trading_day":"20261117"}}}),
    )?;
    native.put_cf(
        native.cf_handle("cn_trading_events").unwrap(),
        10i64.to_be_bytes(),
        &evidence,
    )?;
    let market = "SHFE:sn2612";
    let initial = netting::Initialize {
        strategy_groups: BTreeMap::from([(
            "test".into(),
            BTreeSet::from(["a-1h".into(), "b-1h".into()]),
        )]),
        contracts: BTreeMap::from([(market.into(), netting::Contract { multiplier: 1.0 })]),
        positions: BTreeMap::new(),
        snapshot_evidence_sequence: 10,
        note: "verified flat test account".into(),
    };
    let state = netting::Portfolio::initialize(
        "source",
        &initial,
        &BTreeMap::new(),
        crypto_cta_manager::native_futures::evidence::Anchor {
            source_id: "source".into(),
            snapshot_us: anchor_us,
            trading_day: "20261117".into(),
            evidence_sequence: 10,
            evidence_sha256: hex::encode(Sha256::digest(&evidence)),
            store_id: crypto_cta_manager::native_futures::evidence::generation(
                &native_source,
                &native,
            )?,
            note: initial.note.clone(),
        },
    )?;
    let config: crypto_cta_manager::native_futures::config::Config = toml::from_str(&format!(
        "mode = 'cn_futures'\nsecondary_root = '{}'\n[[sources]]\nid = 'source'\naccount = 'test'\nadapter = 'ctp'\nrocksdb_path = '{}'\n",
        temp.path().join("secondary").display(),
        primary.display()
    ))?;
    // Exercise real HTTP admission with complete reconciliation while broker
    // settlement remains unconfirmed. Initialization must not enable trading.
    {
        let service =
            crypto_cta_manager::native_futures::web::Service::new(config.clone(), pool.clone())?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!(
            "http://{}/api/native/sources/source/netting",
            listener.local_addr()?
        );
        let server = tokio::spawn(async move { axum::serve(listener, service.router()).await });
        let session = crypto_cta_manager::auth::create_session(&pool, 1).await?.0;
        let accepted = reqwest::Client::builder()
            .no_proxy()
            .build()?
            .post(url)
            .header("Cookie", format!("cn_cta_session={session}"))
            .json(&initial)
            .send()
            .await?;
        let status = accepted.status();
        let receipt: Value = accepted.json().await?;
        assert_eq!(status, reqwest::StatusCode::OK, "{receipt}");
        assert_eq!(receipt["revision"], 0);
        server.abort();
        let _ = server.await;
    }
    store::initialize(&pool, &state, &initial, 1).await?;
    let mut request = netting::TargetBatch {
        strategy_group: "test".into(),
        signal_times: BTreeMap::new(),
        batch_id: "hour1".into(),
        expected_revision: 0,
        decision_us: now_us() - 100_000,
        strategies: BTreeMap::from([
            ("a-1h".into(), BTreeMap::from([(market.into(), 2)])),
            ("b-1h".into(), BTreeMap::from([(market.into(), -1)])),
        ]),
        quotes: BTreeMap::from([(
            market.into(),
            netting::Quote {
                bid: 99.0,
                ask: 101.0,
                observed_us: now_us() - 200_000,
                source: "isolated_bbo".into(),
            },
        )]),
    };
    let receipt = store::batch(&pool, "source", &request, 1).await?;
    assert_eq!(receipt["internal_crosses"][0]["lots"], 1);
    assert_eq!(receipt["net_targets"][market]["residual_lots"], 1);
    assert_eq!(store::batch(&pool, "source", &request, 1).await?, receipt);
    let mut changed = request.clone();
    changed
        .strategies
        .get_mut("a-1h")
        .unwrap()
        .insert(market.into(), 3);
    assert!(store::batch(&pool, "source", &changed, 1).await.is_err());
    let before = store::load(&pool, "source").await?.context("portfolio")?;
    let mut invalid = request.clone();
    invalid.batch_id = "invalid".into();
    invalid.expected_revision = 1;
    invalid.decision_us += 1;
    invalid.strategies.remove("b-1h");
    assert!(store::batch(&pool, "source", &invalid, 1).await.is_err());
    assert_eq!(
        netting::fingerprint(&before)?,
        netting::fingerprint(&store::load(&pool, "source").await?.unwrap())?
    );
    let raw: Value = serde_json::from_str(include_str!("fixtures/yida_native_records.json"))?;
    let mut raw = raw["fill"].clone();
    raw["fill"]["identity"]["symbol"] = json!("sn2612");
    raw["received_us"] = json!(now_us());
    let fill = netting::FactualFill::from_evidence(&raw)?;
    let allocation = store::AllocateFill {
        event_id: "fill1".into(),
        batch_id: "hour1".into(),
        expected_revision: 1,
        evidence_key: "01".into(),
        allocations: BTreeMap::from([("a-1h".into(), 1)]),
    };
    let received = store::allocate(&pool, "source", &allocation, &fill, 1).await?;
    assert_eq!(received["net_targets"][market]["residual_lots"], 0);
    assert_eq!(
        store::allocate(&pool, "source", &allocation, &fill, 1).await?,
        received
    );
    let mut duplicate = allocation.clone();
    duplicate.event_id = "another-id".into();
    duplicate.expected_revision = 2;
    assert!(
        store::allocate(&pool, "source", &duplicate, &fill, 1)
            .await
            .is_err()
    );
    let mut overfill = fill.clone();
    overfill.identity = "other_trade".into();
    assert!(
        store::allocate(&pool, "source", &duplicate, &overfill, 1)
            .await
            .is_err()
    );
    let saved = netting::fingerprint(&store::load(&pool, "source").await?.unwrap())?;
    pool.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.database(&database))
        .await?;
    assert_eq!(
        saved,
        netting::fingerprint(&store::load(&pool, "source").await?.unwrap())?
    );
    request.batch_id = "hour2".into();
    request.expected_revision = 2;
    request.decision_us = now_us() - 100_000;
    request.quotes.get_mut(market).unwrap().observed_us = request.decision_us;
    let mut competing = request.clone();
    competing.batch_id = "hour2_competing".into();
    let (a, b) = tokio::join!(
        store::batch(&pool, "source", &request, 1),
        store::batch(&pool, "source", &competing, 1)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(store::history(&pool, "source", -1, 100).await?.len(), 4);
    let targets: Value = sqlx::query_scalar(
        "SELECT targets FROM cta_position_strategies WHERE strategy_name='a-1h'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(targets[market]["qty"], 2);
    // Verify group tokens and new complete publications against an SDK-free
    // synthetic native evidence database, never an actual account or SDK.
    sqlx::raw_sql("UPDATE cta_users SET role='admin' WHERE user_id=1; INSERT INTO cta_users(user_id,username,password_hash) VALUES(2,'viewer','test-only'); INSERT INTO cta_user_source_permissions(user_id,source_id,access_level) VALUES(2,'source','configure');").execute(&pool).await?;
    let admin_token = crypto_cta_manager::auth::create_session(&pool, 1).await?.0;
    let viewer_token = crypto_cta_manager::auth::create_session(&pool, 2).await?.0;
    let service = crypto_cta_manager::native_futures::web::Service::new(config, pool.clone())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, service.router()).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let root = format!("{address}/api/native/sources/source");
    assert_eq!(
        client.get(format!("{root}/netting")).send().await?.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{root}/netting"))
            .header("Cookie", format!("cn_cta_session={viewer_token}"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let account_state: Value = client
        .get(format!("{root}/netting"))
        .header("Cookie", format!("cn_cta_session={admin_token}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(account_state["portfolio"]["revision"], 3);
    assert_eq!(account_state["execution_enabled"], false);
    assert_eq!(
        client
            .post(format!("{address}/api/native/strategies"))
            .header("Cookie", format!("cn_cta_session={admin_token}"))
            .json(&json!({"strategy_name":"a-1h","targets":{"SHFE:sn2612":{"qty":0,"signal":0}}}))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        client
            .post(format!("{root}/target-batches"))
            .header("Cookie", format!("cn_cta_session={viewer_token}"))
            .json(&request)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let mut missing = request.clone();
    missing.strategies.remove("a-1h");
    assert_eq!(
        client
            .post(format!("{root}/target-batches"))
            .header("Cookie", format!("cn_cta_session={admin_token}"))
            .json(&missing)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    // A long-lived machine token may publish only its registered account/group.
    let issued: Value = client
        .post(format!("{root}/netting/publish-token"))
        .header("Cookie", format!("cn_cta_session={admin_token}"))
        .json(&json!({"strategy_group":"test"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let machine = issued["publish_token"]
        .as_str()
        .context("one-time publish token")?;
    let metadata: Value = client
        .get(format!("{root}/netting/publication?strategy_group=test"))
        .header("X-CTA-Publish-Token", machine)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(metadata["revision"], 3);
    assert_eq!(metadata["members"].as_array().unwrap().len(), 2);
    let original:Value=sqlx::query_scalar("SELECT request FROM cta_native_netting_events WHERE source_id='source' AND event_id='batch:hour1'").fetch_one(&pool).await?;
    let replay: Value = client
        .post(format!("{root}/target-batches"))
        .header("X-CTA-Publish-Token", machine)
        .json(&original)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(replay["batch_id"], "hour1");
    assert_eq!(replay["execution_plan"]["net_targets"][market], 1);
    assert_eq!(
        client
            .get(format!("{root}/netting"))
            .header("X-CTA-Publish-Token", machine)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!(
                "{root}/netting/publication?strategy_group=cn-futures-02"
            ))
            .header("X-CTA-Publish-Token", machine)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let mut wrong = original.clone();
    wrong["strategy_group"] = json!("cn-futures-02");
    assert_eq!(
        client
            .post(format!("{root}/target-batches"))
            .header("X-CTA-Publish-Token", machine)
            .json(&wrong)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let mut fresh = request.clone();
    fresh.batch_id = "machine-new".into();
    fresh.expected_revision = 3;
    fresh.decision_us = now_us() - 1000;
    fresh.quotes.get_mut(market).unwrap().observed_us = fresh.decision_us;
    let accepted: Value = client
        .post(format!("{root}/target-batches"))
        .header("X-CTA-Publish-Token", machine)
        .json(&fresh)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(accepted["batch_id"], "machine-new");
    assert_eq!(accepted["execution_plan"]["target_revision"], 4);
    let plan: Value = client
        .get(format!("{root}/netting/execution-plan"))
        .header("Cookie", format!("cn_cta_session={admin_token}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(plan["net_targets"][market], 1);
    assert_eq!(plan["target_revision"], 4);
    assert_eq!(plan["publishable"], false);
    sqlx::query("UPDATE cta_users SET role='user' WHERE user_id=1")
        .execute(&pool)
        .await?;
    assert_eq!(
        client
            .get(format!("{root}/netting/publication?strategy_group=test"))
            .header("X-CTA-Publish-Token", machine)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE cta_users SET role='admin' WHERE user_id=1")
        .execute(&pool)
        .await?;
    client
        .delete(format!("{root}/netting/publish-token"))
        .header("Cookie", format!("cn_cta_session={admin_token}"))
        .json(&json!({"strategy_group":"test"}))
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        client
            .get(format!("{root}/netting/publication?strategy_group=test"))
            .header("X-CTA-Publish-Token", machine)
            .send()
            .await?
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let history: Vec<Value> = client
        .get(format!("{root}/netting/events"))
        .header("Cookie", format!("cn_cta_session={admin_token}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(history.len(), 5);
    server.abort();
    let current = store::load(&pool, "source").await?.unwrap();
    let mut pending = request.clone();
    pending.batch_id = "hour3".into();
    pending.expected_revision = current.revision;
    pending.decision_us = now_us();
    pending.quotes.get_mut(market).unwrap().observed_us = pending.decision_us;
    pending
        .strategies
        .get_mut("a-1h")
        .unwrap()
        .insert(market.into(), 3);
    store::batch(&pool, "source", &pending, 1).await?;
    let mut blocked = pending.clone();
    blocked.batch_id = "hour4".into();
    blocked.expected_revision = current.revision + 1;
    blocked.decision_us += 1;
    assert!(store::batch(&pool, "source", &blocked, 1).await.is_err());
    let release = store::ReleaseDemand {
        event_id: "canceled-hour3".into(),
        expected_revision: current.revision + 1,
        note: "fresh order-free snapshot confirms canceled remainder".into(),
    };
    let mut proof = current.anchor.clone();
    proof.evidence_sequence = 100;
    proof.snapshot_us = now_us();
    proof.note = release.note.clone();
    assert!(
        store::release(
            &pool,
            "source",
            &release,
            &proof,
            &BTreeMap::from([(market.into(), 2)]),
            1
        )
        .await
        .is_err()
    );
    let released = store::release(
        &pool,
        "source",
        &release,
        &proof,
        &BTreeMap::from([(market.into(), 1)]),
        1,
    )
    .await?;
    assert_eq!(
        store::release(
            &pool,
            "source",
            &release,
            &proof,
            &BTreeMap::from([(market.into(), 1)]),
            1
        )
        .await?,
        released
    );
    let restored = store::load(&pool, "source").await?.unwrap();
    assert!(!restored.external_demand_open);
    assert!(restored.strategy_groups["test"].held);
    assert_eq!(restored.targets["a-1h"][market], 3);
    let plan = restored.execution_plan()?;
    assert!(!plan.publishable);
    assert_eq!(plan.net_targets[market], 1);
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}
