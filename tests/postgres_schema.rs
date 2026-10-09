//! Run against an isolated PostgreSQL 16 cluster with:
//! CTA_SCHEMA_TEST_SOCKET=/tmp/cta-schema-test-... cargo test --test postgres_schema -- --ignored
//! Each test creates and removes its own database. TCP and production sockets
//! are deliberately excluded.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use crypto_cta_manager::postgres;
use crypto_cta_manager::snapshot::{PositionSnapshot, SnapshotPosition};
use sqlx::AssertSqlSafe;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    name: String,
}

impl TestDatabase {
    async fn create() -> Result<Self> {
        let socket = std::env::var("CTA_SCHEMA_TEST_SOCKET")
            .context("set CTA_SCHEMA_TEST_SOCKET to an isolated test cluster")?;
        ensure!(
            socket.starts_with("/tmp/cta-schema-test-")
                && Path::new(&socket).join(".s.PGSQL.15433").exists(),
            "only isolated /tmp/cta-schema-test-* sockets on port 15433 are allowed"
        );
        let user = std::env::var("USER").context("USER must name the local test cluster owner")?;
        let options = PgConnectOptions::new()
            .host(&socket)
            .port(15433)
            .username(&user)
            .database("postgres");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let name = format!(
            "cta_schema_test_{}_{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        // Database identifiers contain only a fixed prefix, PID and timestamp.
        // No user-supplied string enters the DDL.
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.database(&name))
            .await?;
        Ok(Self { admin, pool, name })
    }

    async fn remove(self) -> Result<()> {
        self.pool.close().await;
        sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {}", self.name)))
            .execute(&self.admin)
            .await?;
        self.admin.close().await;
        Ok(())
    }
}

async fn seed_business_data(pool: &PgPool) -> Result<PositionSnapshot> {
    sqlx::raw_sql(
        r#"
        INSERT INTO cta_order_sources (source_id, account_label, venue_label, rocksdb_path)
        VALUES ('test-source', 'test-account', 'binance-futures', '/tmp/test-exec');
        INSERT INTO cta_users (username, password_hash)
        VALUES ('schema-test', 'dummy-test-hash');
        INSERT INTO cta_position_strategies (strategy_name, updated_at_us)
        VALUES ('test-strategy', 1);
        INSERT INTO cta_position_strategy_grants (strategy_name, user_id, access_level)
        SELECT 'test-strategy', user_id, 'configure' FROM cta_users;
        "#,
    )
    .execute(pool)
    .await?;
    let snapshot = PositionSnapshot {
        source_id: "test-source".to_string(),
        snapshot_ts_us: 1_000,
        positions: vec![SnapshotPosition {
            symbol: "BTCUSDT".to_string(),
            venue_code: 1,
            quantity: -2.0,
            reference_price: Some(60_000.0),
        }],
    };
    postgres::create_position_snapshot(pool, &snapshot, Some("test anchor")).await?;
    Ok(snapshot)
}

async fn assert_business_data(pool: &PgPool, snapshot: &PositionSnapshot) -> Result<()> {
    assert_eq!(
        postgres::load_latest_position_snapshot(pool, "test-source").await?,
        Some(snapshot.clone())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cta_position_strategy_grants WHERE access_level = 'configure'"
        )
        .fetch_one(pool)
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn initializes_current_schema_without_ingestion_or_migration_tables() -> Result<()> {
    let database = TestDatabase::create().await?;
    postgres::initialize(&database.pool).await?;
    let absent: bool = sqlx::query_scalar(
        "SELECT to_regclass('cta_uniform_order_events') IS NULL
             AND to_regclass('cta_ingestion_checkpoints') IS NULL
             AND to_regclass('cta_ingestion_failures') IS NULL
             AND to_regclass('_sqlx_migrations') IS NULL
             AND to_regclass('cta_theoretical_nav_events') IS NULL
             AND to_regclass('cta_theoretical_nav_pending') IS NULL",
    )
    .fetch_one(&database.pool)
    .await?;
    assert!(absent);
    let snapshot = seed_business_data(&database.pool).await?;
    assert_business_data(&database.pool, &snapshot).await?;
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn repeat_initialization_fails_and_preserves_business_data() -> Result<()> {
    let database = TestDatabase::create().await?;
    postgres::initialize(&database.pool).await?;
    let snapshot = seed_business_data(&database.pool).await?;
    assert!(postgres::initialize(&database.pool).await.is_err());
    assert_business_data(&database.pool, &snapshot).await?;
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn initialization_failure_rolls_back_preceding_ddl() -> Result<()> {
    let database = TestDatabase::create().await?;
    sqlx::raw_sql("CREATE TABLE cta_order_sources (source_id text PRIMARY KEY)")
        .execute(&database.pool)
        .await?;
    assert!(postgres::initialize(&database.pool).await.is_err());
    let objects: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'S') ORDER BY c.relname",
    )
    .fetch_all(&database.pool)
    .await?;
    assert_eq!(objects, vec!["cta_order_sources"]);
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn manager_operations_ignore_old_migration_history() -> Result<()> {
    let database = TestDatabase::create().await?;
    postgres::initialize(&database.pool).await?;
    let snapshot = seed_business_data(&database.pool).await?;
    sqlx::raw_sql(
        "CREATE TABLE _sqlx_migrations (version bigint, checksum bytea, success boolean);
         INSERT INTO _sqlx_migrations VALUES (-99, decode('00', 'hex'), false)",
    )
    .execute(&database.pool)
    .await?;
    assert_business_data(&database.pool, &snapshot).await?;
    assert!(
        postgres::load_fee_rate(&database.pool, "test-source")
            .await?
            .is_some()
    );
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn virtual_numbers_allocate_concurrently_and_alias_edits_preserve_identity() -> Result<()> {
    use crypto_cta_manager::virtual_accounts::{self, AccountConfiguration, SaveVirtualAccount};

    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    seed_business_data(&db.pool).await?;
    let legacy = SaveVirtualAccount {
        name: "Legacy alias".into(),
        bindings: vec![],
    };
    virtual_accounts::save(&db.pool, "model", &legacy, 1).await?;
    let mut tasks = Vec::new();
    for index in 1..=8 {
        let pool = db.pool.clone();
        tasks.push(tokio::spawn(async move {
            let request = SaveVirtualAccount {
                name: format!("Alias {index}"),
                bindings: vec![],
            };
            let id = virtual_accounts::create(&pool, &request, index).await?;
            Ok::<_, anyhow::Error>((id, request.name))
        }));
    }
    let mut created = std::collections::BTreeMap::new();
    for task in tasks {
        let (id, name) = task.await??;
        assert!(created.insert(id, name).is_none());
    }
    assert_eq!(
        created.keys().cloned().collect::<Vec<_>>(),
        (1..=8)
            .map(|number| format!("virtual{number:02}"))
            .collect::<Vec<_>>()
    );
    for account in virtual_accounts::list(&db.pool).await? {
        if account.virtual_id == "model" {
            assert_eq!(account.name, legacy.name);
        } else {
            assert_eq!(account.name, created[&account.virtual_id]);
        }
    }
    let follow = AccountConfiguration::Follow {
        virtual_id: "virtual01".into(),
        multiplier: 2.0,
    };
    virtual_accounts::set_configuration(&db.pool, "test-source", &follow, 10).await?;
    virtual_accounts::save(
        &db.pool,
        "virtual01",
        &SaveVirtualAccount {
            name: "Updated alias".into(),
            bindings: vec![],
        },
        11,
    )
    .await?;
    assert_eq!(
        virtual_accounts::configuration(&db.pool, "test-source").await?,
        follow
    );
    let accounts = virtual_accounts::list(&db.pool).await?;
    assert_eq!(accounts.len(), 9);
    assert_eq!(
        accounts
            .iter()
            .find(|a| a.virtual_id == "virtual01")
            .unwrap()
            .name,
        "Updated alias"
    );
    let next = virtual_accounts::create(&db.pool, &legacy, 12).await?;
    assert_eq!(next, "virtual09");
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn virtual_follow_replaces_scales_stops_and_preserves_durable_delivery() -> Result<()> {
    use crypto_cta_manager::order_config::OrderParameters;
    use crypto_cta_manager::strategy_catalog::{
        self, SaveBindingRequest, SaveOrderStrategyRequest,
    };
    use crypto_cta_manager::virtual_accounts::{
        self, AccountConfiguration, SaveVirtualAccount, VirtualBinding,
    };
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    seed_business_data(&db.pool).await?;
    sqlx::raw_sql("INSERT INTO cta_position_strategies (strategy_name,updated_at_us) VALUES ('second',1), ('third',1); INSERT INTO cta_order_sources (source_id,account_label,venue_label,rocksdb_path) VALUES ('other-source','other','binance-futures','/tmp/other-exec')")
        .execute(&db.pool).await?;
    for name in ["order-a", "order-b"] {
        strategy_catalog::upsert_order_strategy(
            &db.pool,
            &SaveOrderStrategyRequest {
                strategy_name: name.into(),
                order_parameters: OrderParameters::default(),
            },
            1,
        )
        .await?;
    }
    let independent = SaveBindingRequest {
        binding_name: "second".into(),
        position_strategy_name: "second".into(),
        order_strategy_name: "order-a".into(),
        shares: 9.0,
    };
    strategy_catalog::save_binding(&db.pool, "test-source", &independent, 1).await?;
    let template = |shares, order: &str| SaveVirtualAccount {
        name: "Model".into(),
        bindings: vec![VirtualBinding {
            binding_name: "test-strategy".into(),
            position_strategy_name: "test-strategy".into(),
            order_strategy_name: order.into(),
            shares,
        }],
    };
    virtual_accounts::save(&db.pool, "model", &template(2.0, "order-a"), 2).await?;
    // Virtual identity never enters the Exec account catalog.
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cta_order_sources WHERE source_id = 'model'"
        )
        .fetch_one(&db.pool)
        .await?,
        0
    );
    for (source, multiplier) in [("test-source", 3.0), ("other-source", 0.5)] {
        virtual_accounts::set_configuration(
            &db.pool,
            source,
            &AccountConfiguration::Follow {
                virtual_id: "model".into(),
                multiplier,
            },
            3,
        )
        .await?;
    }
    let virtual_list = virtual_accounts::list(&db.pool).await?;
    assert_eq!(virtual_list[0].followers.len(), 2);
    assert!(
        virtual_list[0]
            .followers
            .iter()
            .any(|f| f.source_id == "test-source"
                && f.multiplier == 3.0
                && f.pending_publishes.len() == 2)
    );
    let studio = strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    assert_eq!(
        studio
            .bindings
            .iter()
            .find(|b| b.binding_name == "test-strategy")
            .unwrap()
            .shares,
        6.0
    );
    assert_eq!(
        studio
            .bindings
            .iter()
            .find(|b| b.binding_name == "second")
            .unwrap()
            .shares,
        0.0
    );
    assert_eq!(studio.pending_publishes.len(), 2);
    sqlx::query("DELETE FROM cta_follow_publish_queue WHERE source_id = 'test-source'")
        .execute(&db.pool)
        .await?;
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &AccountConfiguration::Follow {
            virtual_id: "model".into(),
            multiplier: 3.0,
        },
        3,
    )
    .await?;
    // Explicit follow application republishes even unchanged desired bindings
    // and retained zero stops, repairing any pre-existing runtime divergence.
    assert_eq!(
        virtual_accounts::pending(&db.pool, "test-source")
            .await?
            .len(),
        2
    );
    assert!(
        strategy_catalog::save_binding(&db.pool, "test-source", &independent, 4)
            .await
            .is_err()
    );
    assert!(virtual_accounts::delete(&db.pool, "model").await.is_err());
    // Persist failure status, then read it through a newly connected pool.
    sqlx::query("UPDATE cta_follow_publish_queue SET archived = true, error = 'runtime unavailable' WHERE source_id = 'test-source' AND binding_name = 'test-strategy'").execute(&db.pool).await?;
    let restarted_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*db.pool.connect_options()).clone())
        .await?;
    assert!(
        virtual_accounts::pending(&restarted_pool, "test-source")
            .await?
            .iter()
            .any(|p| p.error.as_deref() == Some("runtime unavailable"))
    );
    restarted_pool.close().await;
    virtual_accounts::save(&db.pool, "model", &template(4.0, "order-b"), 5).await?;
    let studio = strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    let active = studio
        .bindings
        .iter()
        .find(|b| b.binding_name == "test-strategy")
        .unwrap();
    assert_eq!(active.shares, 12.0);
    assert_eq!(active.order_strategy_name, "order-b");
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "other-source")
            .await?
            .bindings[0]
            .shares,
        2.0
    );
    assert!(!sqlx::query_scalar::<_, bool>("SELECT archived FROM cta_follow_publish_queue WHERE source_id = 'test-source' AND binding_name = 'test-strategy'").fetch_one(&db.pool).await?);
    // An incompatible replacement must roll back every follower and the template.
    let incompatible = SaveVirtualAccount {
        name: "bad".into(),
        bindings: vec![VirtualBinding {
            binding_name: "test-strategy".into(),
            position_strategy_name: "third".into(),
            order_strategy_name: "order-a".into(),
            shares: 7.0,
        }],
    };
    assert!(
        virtual_accounts::save(&db.pool, "model", &incompatible, 6)
            .await
            .is_err()
    );
    assert_eq!(
        virtual_accounts::list(&db.pool).await?[0].bindings[0].position_strategy_name,
        "test-strategy"
    );
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source")
            .await?
            .bindings
            .iter()
            .find(|b| b.binding_name == "test-strategy")
            .unwrap()
            .shares,
        12.0
    );
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &AccountConfiguration::Follow {
            virtual_id: "model".into(),
            multiplier: 0.0,
        },
        7,
    )
    .await?;
    assert!(
        strategy_catalog::load_account_studio(&db.pool, "test-source")
            .await?
            .bindings
            .iter()
            .all(|b| b.shares == 0.0)
    );
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &AccountConfiguration::Independent,
        8,
    )
    .await?;
    assert!(matches!(
        virtual_accounts::configuration(&db.pool, "test-source").await?,
        AccountConfiguration::Independent
    ));
    // Pending stops cannot be deleted before delivery; detaching preserves them.
    assert!(
        strategy_catalog::delete_binding(&db.pool, "test-source", "test-strategy")
            .await
            .is_err()
    );
    virtual_accounts::save(
        &db.pool,
        "model",
        &SaveVirtualAccount {
            name: "Model".into(),
            bindings: vec![],
        },
        9,
    )
    .await?;
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "other-source")
            .await?
            .bindings[0]
            .shares,
        0.0
    );
    virtual_accounts::set_configuration(
        &db.pool,
        "other-source",
        &AccountConfiguration::Independent,
        10,
    )
    .await?;
    assert!(virtual_accounts::delete(&db.pool, "model").await?);
    // Acknowledging deliveries makes the retained independent bindings editable.
    sqlx::query("DELETE FROM cta_follow_publish_queue")
        .execute(&db.pool)
        .await?;
    strategy_catalog::save_binding(&db.pool, "test-source", &independent, 11).await?;
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source")
            .await?
            .bindings
            .iter()
            .find(|b| b.binding_name == "second")
            .unwrap()
            .shares,
        9.0
    );
    db.remove().await
}
