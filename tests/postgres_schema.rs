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
        postgres::load_account_fee_rates(&database.pool, "test-source")
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
            let id = virtual_accounts::create(&pool, &request, None, index).await?;
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
        follows: vec![crypto_cta_manager::virtual_accounts::AccountFollow {
            virtual_id: "virtual01".into(),
            multiplier: 2.0,
        }],
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
    let next = virtual_accounts::create(&db.pool, &legacy, None, 12).await?;
    assert_eq!(next, "virtual09");
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn multiple_virtual_follows_migrate_combine_recompute_and_rollback_atomically() -> Result<()>
{
    use crypto_cta_manager::order_config::OrderParameters;
    use crypto_cta_manager::strategy_catalog::{self, SaveOrderStrategyRequest};
    use crypto_cta_manager::virtual_accounts::{
        self, AccountConfiguration, AccountFollow, SaveVirtualAccount, VirtualBinding,
    };
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    seed_business_data(&db.pool).await?;
    for order in ["order-a", "order-b"] {
        strategy_catalog::upsert_order_strategy(
            &db.pool,
            &SaveOrderStrategyRequest {
                strategy_name: order.into(),
                order_parameters: OrderParameters::default(),
            },
            1,
            None,
        )
        .await?;
    }
    let binding = |name: &str, shares| VirtualBinding {
        binding_name: name.into(),
        position_strategy_name: "test-strategy".into(),
        order_strategy_name: "order-a".into(),
        shares,
    };
    let template = |shares| SaveVirtualAccount {
        name: "A".into(),
        bindings: vec![binding("shared", shares), binding("only-a", 4.0)],
    };
    virtual_accounts::save(&db.pool, "a", &template(2.0), 1).await?;
    virtual_accounts::save(
        &db.pool,
        "b",
        &SaveVirtualAccount {
            name: "B".into(),
            bindings: vec![binding("shared", 5.0), binding("only-b", 6.0)],
        },
        1,
    )
    .await?;
    // Recreate the old single-follow key and prove maintenance preserves its rows.
    sqlx::raw_sql("ALTER TABLE cta_account_follows DROP CONSTRAINT cta_account_follows_pkey; ALTER TABLE cta_account_follows ADD PRIMARY KEY (source_id);
        INSERT INTO cta_account_follows VALUES ('test-source','a',3,1);")
        .execute(&db.pool).await?;
    let config = |a, b| AccountConfiguration::Follow {
        follows: vec![
            AccountFollow {
                virtual_id: "a".into(),
                multiplier: a,
            },
            AccountFollow {
                virtual_id: "b".into(),
                multiplier: b,
            },
        ],
    };
    sqlx::raw_sql(include_str!(
        "../scripts/apply_multiple_virtual_follows_schema.sql"
    ))
    .execute(&db.pool)
    .await?;
    assert_eq!(
        virtual_accounts::configuration(&db.pool, "test-source").await?,
        AccountConfiguration::Follow {
            follows: vec![AccountFollow {
                virtual_id: "a".into(),
                multiplier: 3.0
            }]
        }
    );
    virtual_accounts::set_configuration(&db.pool, "test-source", &config(3.0, 0.5), 2).await?;
    let studio = strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    assert_eq!(studio.configuration, config(3.0, 0.5));
    assert_eq!(
        studio
            .bindings
            .iter()
            .map(|b| (b.binding_name.as_str(), b.shares))
            .collect::<Vec<_>>(),
        vec![("only-a", 12.0), ("only-b", 3.0), ("shared", 8.5)]
    );
    assert_eq!(studio.pending_publishes.len(), 3);
    // The old JOIN produced duplicate queue inserts for multiple follows.
    sqlx::query("DELETE FROM cta_follow_publish_queue")
        .execute(&db.pool)
        .await?;
    virtual_accounts::enqueue_order_followers(&db.pool, "order-a").await?;
    assert_eq!(
        virtual_accounts::pending(&db.pool, "test-source")
            .await?
            .len(),
        3
    );
    virtual_accounts::save(&db.pool, "a", &template(4.0), 3).await?;
    let updated = strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    assert_eq!(
        updated
            .bindings
            .iter()
            .find(|b| b.binding_name == "shared")
            .unwrap()
            .shares,
        14.5
    );
    // Validate unsaved edits against all contributions, then test durable rollback.
    let mut conflict = template(4.0);
    conflict.bindings[0].order_strategy_name = "order-b".into();
    if let AccountConfiguration::Follow { follows } = &updated.configuration {
        assert!(
            virtual_accounts::preview_bindings(&db.pool, follows, Some(("a", &conflict.bindings)))
                .await
                .is_err()
        );
    }
    assert!(
        virtual_accounts::save(&db.pool, "a", &conflict, 4)
            .await
            .is_err()
    );
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source").await?,
        updated
    );
    assert_eq!(
        virtual_accounts::list(&db.pool).await?[0].bindings[1].order_strategy_name,
        "order-a"
    );
    let bad_config = AccountConfiguration::Follow {
        follows: vec![
            AccountFollow {
                virtual_id: "a".into(),
                multiplier: 1.0,
            },
            AccountFollow {
                virtual_id: "a".into(),
                multiplier: 2.0,
            },
        ],
    };
    assert!(
        virtual_accounts::set_configuration(&db.pool, "test-source", &bad_config, 4)
            .await
            .is_err()
    );
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source").await?,
        updated
    );
    virtual_accounts::set_configuration(&db.pool, "test-source", &config(0.0, 0.5), 5).await?;
    let paused = strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    assert_eq!(
        paused
            .bindings
            .iter()
            .find(|b| b.binding_name == "shared")
            .unwrap()
            .shares,
        2.5
    );
    assert_eq!(
        paused
            .bindings
            .iter()
            .find(|b| b.binding_name == "only-a")
            .unwrap()
            .shares,
        0.0
    );
    // Removing B's shared row keeps A's contribution when A is resumed.
    virtual_accounts::set_configuration(&db.pool, "test-source", &config(3.0, 0.5), 6).await?;
    virtual_accounts::save(
        &db.pool,
        "b",
        &SaveVirtualAccount {
            name: "B".into(),
            bindings: vec![binding("only-b", 6.0)],
        },
        7,
    )
    .await?;
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source")
            .await?
            .bindings
            .iter()
            .find(|b| b.binding_name == "shared")
            .unwrap()
            .shares,
        12.0
    );
    // Detaching one virtual stops only its exclusive names and preserves the rest.
    let only_b = AccountConfiguration::Follow {
        follows: vec![AccountFollow {
            virtual_id: "b".into(),
            multiplier: 0.5,
        }],
    };
    virtual_accounts::set_configuration(&db.pool, "test-source", &only_b, 8).await?;
    let detached = strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    assert_eq!(
        detached
            .bindings
            .iter()
            .map(|b| b.shares)
            .collect::<Vec<_>>(),
        vec![0.0, 3.0, 0.0]
    );
    assert!(virtual_accounts::delete(&db.pool, "a").await?);
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &AccountConfiguration::Independent,
        9,
    )
    .await?;
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source")
            .await?
            .bindings,
        detached.bindings
    );
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
            None,
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
                follows: vec![crypto_cta_manager::virtual_accounts::AccountFollow {
                    virtual_id: "model".into(),
                    multiplier,
                }],
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
            follows: vec![crypto_cta_manager::virtual_accounts::AccountFollow {
                virtual_id: "model".into(),
                multiplier: 3.0,
            }],
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
            follows: vec![crypto_cta_manager::virtual_accounts::AccountFollow {
                virtual_id: "model".into(),
                multiplier: 0.0,
            }],
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

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn virtual_owner_and_manager_grants_survive_edits_and_revoke_immediately() -> Result<()> {
    use crypto_cta_manager::virtual_accounts::{self, SaveVirtualAccount};
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    let ids: Vec<i64> = sqlx::query_scalar("INSERT INTO cta_users (username,password_hash) VALUES ('owner','fixture'),('guest','fixture') RETURNING user_id")
        .fetch_all(&db.pool).await?;
    let request = SaveVirtualAccount {
        name: "Owned".into(),
        bindings: vec![],
    };
    let id = virtual_accounts::create(&db.pool, &request, Some(ids[0]), 1).await?;
    let guest = crypto_cta_manager::auth::AuthUser {
        user_id: ids[1],
        username: "guest".into(),
        role: "user".into(),
    };
    virtual_accounts::set_managers(&db.pool, &id, &[ids[1]]).await?;
    virtual_accounts::save(
        &db.pool,
        &id,
        &SaveVirtualAccount {
            name: "Renamed".into(),
            bindings: vec![],
        },
        2,
    )
    .await?;
    let mut account = virtual_accounts::list(&db.pool).await?.remove(0);
    assert_eq!(account.created_by_user_id, Some(ids[0]));
    assert_eq!(account.owner_username.as_deref(), Some("owner"));
    account.permissions_for(&guest);
    assert!(account.can_configure && !account.can_manage_grants);
    assert!(
        virtual_accounts::set_managers(&db.pool, &id, &[i64::MAX])
            .await
            .is_err()
    );
    assert_eq!(
        virtual_accounts::list(&db.pool).await?[0].managers[0].user_id,
        ids[1]
    );
    virtual_accounts::set_managers(&db.pool, &id, &[]).await?;
    let mut account = virtual_accounts::list(&db.pool).await?.remove(0);
    account.permissions_for(&guest);
    assert!(!account.can_configure && !account.can_manage_grants);
    assert!(virtual_accounts::delete(&db.pool, &id).await?);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cta_virtual_account_managers")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn quote_fee_settings_roundtrip_preserve_defaults_and_explicit_clear() -> Result<()> {
    use crypto_cta_manager::config::{FeeRates, QuoteFeeRates};
    use std::collections::BTreeMap;
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    let snapshot = seed_business_data(&db.pool).await?;
    // Exercise explicit maintenance against the previous schema too.
    sqlx::raw_sql("ALTER TABLE cta_order_sources DROP COLUMN quote_fee_rates")
        .execute(&db.pool)
        .await?;
    sqlx::raw_sql(include_str!("../scripts/apply_quote_fee_schema.sql"))
        .execute(&db.pool)
        .await?;
    let defaults = FeeRates {
        maker: 0.0001,
        taker: 0.0003,
        by_quote: BTreeMap::new(),
    };
    let quotes = BTreeMap::from([(
        "USDC".into(),
        QuoteFeeRates {
            maker_fee_rate: 0.0,
            taker_fee_rate: 0.000165,
            theoretical_twap_fee_rate: 0.0000825,
        },
    )]);
    postgres::save_fee_rates(&db.pool, "test-source", &defaults, 0.0004, Some(&quotes)).await?;
    let (actual, theory) = postgres::load_analysis_fee_rates(&db.pool).await?;
    assert_eq!(
        actual["test-source"].for_symbol("BTCUSDT"),
        (0.0001, 0.0003)
    );
    assert_eq!(actual["test-source"].for_symbol("BTCUSDC"), (0.0, 0.000165));
    assert_eq!(theory["test-source"].for_symbol("BTCUSDC"), 0.0000825);
    // Old API clients omit overrides. They must not silently erase them.
    postgres::save_estimated_fee_rate(&db.pool, "test-source", -0.0001).await?;
    let studio =
        crypto_cta_manager::strategy_catalog::load_account_studio(&db.pool, "test-source").await?;
    assert_eq!(studio.quote_fee_rates, quotes);
    assert_eq!(studio.maker_fee_rate, -0.0001);
    postgres::save_fee_rates(
        &db.pool,
        "test-source",
        &defaults,
        0.0004,
        Some(&BTreeMap::new()),
    )
    .await?;
    let (rates, _) = postgres::load_account_fee_rates(&db.pool, "test-source")
        .await?
        .unwrap();
    assert_eq!(rates.for_symbol("BTCUSDC"), (0.0001, 0.0003));
    assert_business_data(&db.pool, &snapshot).await?;
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn daily_equity_is_atomic_source_scoped_and_runs_once_per_shanghai_day() -> Result<()> {
    use crypto_cta_manager::account_overview::daily::{self, EquityAnchor};
    let database = TestDatabase::create().await?;
    postgres::initialize(&database.pool).await?;
    let snapshot = seed_business_data(&database.pool).await?;
    let day = 86_400_000;
    let start = crypto_cta_manager::account_overview::day_start(20 * day);
    let build = |end, equity| {
        let nav = (0..=((end - start) / day))
            .map(|i| (start + i * day, i as f64 * 10.0))
            .collect::<Vec<_>>();
        daily::reconstruct(
            "test-source",
            EquityAnchor {
                day_ms: end,
                ts_ms: end,
                equity,
                nav_change_quote: 0.0,
                coverage_start_ms: start,
                nav_generated_at_us: end * 1000,
                funding_mode: "estimated".into(),
                strategy_allocation: true,
                valuation: "quantity_fifo_closed_1m_window_delta".into(),
                computed_at_ms: end,
                drawdown_15d_percent: None,
            },
            &nav,
        )
        .unwrap()
    };
    let rows = build(start + 2 * day, 120.0);
    assert!(daily::save(&database.pool, &rows).await?);
    assert!(!daily::save(&database.pool, &build(start + 2 * day, 999.0)).await?);
    assert_eq!(daily::load(&database.pool, "test-source").await?, rows);
    assert!(
        daily::load(&database.pool, "hidden-source")
            .await?
            .is_empty()
    );
    let next = build(start + 3 * day, 130.0);
    let mut invalid = next.clone();
    invalid.last_mut().unwrap().boundary_ts_ms += 1;
    assert!(daily::save(&database.pool, &invalid).await.is_err());
    assert_eq!(daily::load(&database.pool, "test-source").await?, rows);
    let (a, b) = tokio::join!(
        daily::save(&database.pool, &next),
        daily::save(&database.pool, &next)
    );
    assert_ne!(a?, b?);
    assert_eq!(daily::load(&database.pool, "test-source").await?, next);
    assert_business_data(&database.pool, &snapshot).await?;
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn explicit_daily_equity_schema_preserves_existing_catalog_and_snapshots() -> Result<()> {
    let database = TestDatabase::create().await?;
    postgres::initialize(&database.pool).await?;
    let snapshot = seed_business_data(&database.pool).await?;
    sqlx::raw_sql("DROP TABLE cta_account_equity_daily")
        .execute(&database.pool)
        .await?;
    sqlx::raw_sql(include_str!("../scripts/apply_account_equity_schema.sql"))
        .execute(&database.pool)
        .await?;
    assert_business_data(&database.pool, &snapshot).await?;
    assert!(
        crypto_cta_manager::account_overview::daily::load(&database.pool, "test-source")
            .await?
            .is_empty()
    );
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shanghai_settlement_maintenance_invalidates_utc_derived_rows_only() -> Result<()> {
    let database = TestDatabase::create().await?;
    postgres::initialize(&database.pool).await?;
    let snapshot = seed_business_data(&database.pool).await?;
    // Reproduce the previously deployed UTC table and a persisted UTC generation.
    sqlx::raw_sql("ALTER TABLE cta_account_equity_daily DROP CONSTRAINT cta_account_equity_daily_boundary_ts_ms_check; ALTER TABLE cta_account_equity_daily ADD CONSTRAINT cta_account_equity_daily_boundary_ts_ms_check CHECK (boundary_ts_ms >= 0 AND boundary_ts_ms % 86400000 = 0); INSERT INTO cta_account_equity_daily(source_id,boundary_ts_ms,equity,nav_change_quote,anchor) VALUES('test-source',1792368000000,100,0,'{}')")
        .execute(&database.pool).await?;
    sqlx::raw_sql(include_str!("../scripts/apply_account_equity_shanghai.sql"))
        .execute(&database.pool)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cta_account_equity_daily")
        .fetch_one(&database.pool)
        .await?;
    assert_eq!(count, 0);
    let insert = "INSERT INTO cta_account_equity_daily(source_id,boundary_ts_ms,equity,nav_change_quote,anchor) VALUES('test-source',$1,100,0,'{}')";
    assert!(
        sqlx::query(insert)
            .bind(1_792_368_000_000_i64)
            .execute(&database.pool)
            .await
            .is_err()
    );
    sqlx::query(insert)
        .bind(1_792_339_200_000_i64)
        .execute(&database.pool)
        .await?;
    // Re-running maintenance preserves already reconstructed Shanghai records.
    sqlx::raw_sql(include_str!("../scripts/apply_account_equity_shanghai.sql"))
        .execute(&database.pool)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cta_account_equity_daily")
        .fetch_one(&database.pool)
        .await?;
    assert_eq!(count, 1);
    assert_business_data(&database.pool, &snapshot).await?;
    database.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn grid_schema_maintenance_and_catalog_preserve_existing_orders() -> Result<()> {
    use crypto_cta_manager::order_config::{ExecutionAlgorithm, OrderParameters};
    use crypto_cta_manager::strategy_catalog::{self, SaveOrderStrategyRequest};
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    let old = SaveOrderStrategyRequest {
        strategy_name: "old-batch".into(),
        order_parameters: OrderParameters::default(),
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &old, 1, None).await?;
    // Recreate the pre-Grid schema, then exercise the explicit maintenance SQL.
    sqlx::raw_sql("ALTER TABLE cta_order_strategies DROP COLUMN grid; ALTER TABLE cta_order_strategies DROP CONSTRAINT cta_order_strategies_algorithm_check; ALTER TABLE cta_order_strategies ADD CONSTRAINT cta_order_strategies_algorithm_check CHECK (algorithm IN ('batch','pov','chase','ctaker'))")
        .execute(&db.pool).await?;
    sqlx::raw_sql(include_str!("../scripts/apply_grid_order_schema.sql"))
        .execute(&db.pool)
        .await?;
    let mut parameters = OrderParameters::default();
    parameters.algorithm = ExecutionAlgorithm::Grid;
    parameters.grid.spot.orders_per_batch = 2;
    parameters.grid.spot.maker_timeout_ms = 2000;
    parameters.grid.futures.orders_per_batch = 5;
    parameters.grid.futures.maker_timeout_ms = 9000;
    let request = SaveOrderStrategyRequest {
        strategy_name: "vol-grid".into(),
        order_parameters: parameters.clone(),
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &request, 2, None).await?;
    let stored = strategy_catalog::list_order_strategies(&db.pool).await?;
    assert_eq!(
        stored
            .iter()
            .find(|item| item.strategy_name == "old-batch")
            .unwrap()
            .order_parameters,
        old.order_parameters
    );
    assert_eq!(
        stored
            .iter()
            .find(|item| item.strategy_name == "vol-grid")
            .unwrap()
            .order_parameters,
        parameters
    );
    parameters.grid.futures.maker_timeout_ms = 12000;
    strategy_catalog::upsert_order_strategy(
        &db.pool,
        &SaveOrderStrategyRequest {
            strategy_name: "vol-grid".into(),
            order_parameters: parameters.clone(),
        },
        3,
        None,
    )
    .await?;
    let stored = strategy_catalog::list_order_strategies(&db.pool).await?;
    assert_eq!(
        stored
            .iter()
            .find(|item| item.strategy_name == "vol-grid")
            .unwrap()
            .order_parameters,
        parameters
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn order_template_ownership_preserves_shared_templates_and_creator() -> Result<()> {
    use crypto_cta_manager::{
        auth::AuthUser,
        order_config::OrderParameters,
        strategy_catalog::{self, SaveOrderStrategyRequest},
    };
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    let request = SaveOrderStrategyRequest {
        strategy_name: "shared".into(),
        order_parameters: OrderParameters::default(),
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &request, 1, None).await?;
    sqlx::raw_sql("ALTER TABLE cta_order_strategies DROP COLUMN created_by_user_id")
        .execute(&db.pool)
        .await?;
    sqlx::raw_sql(include_str!(
        "../scripts/apply_order_strategy_ownership.sql"
    ))
    .execute(&db.pool)
    .await?;
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO cta_users(username,password_hash) VALUES('lgq','test') RETURNING user_id",
    )
    .fetch_one(&db.pool)
    .await?;
    let owner = AuthUser {
        user_id,
        username: "lgq".into(),
        role: "user".into(),
    };
    let other = AuthUser {
        user_id: user_id + 1,
        username: "other".into(),
        role: "user".into(),
    };
    let admin = AuthUser {
        role: "admin".into(),
        ..other.clone()
    };
    let owned = SaveOrderStrategyRequest {
        strategy_name: "lgq_grid".into(),
        ..request
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &owned, 2, Some(user_id)).await?;
    // Subsequent saves never transfer or clear the creator.
    strategy_catalog::upsert_order_strategy(&db.pool, &owned, 3, None).await?;
    let owners = strategy_catalog::list_order_strategy_owners(&db.pool).await?;
    assert_eq!(owners["lgq_grid"].username.as_deref(), Some("lgq"));
    assert!(owners["lgq_grid"].can_configure(&owner));
    assert!(owners["lgq_grid"].can_configure(&admin));
    assert!(!owners["lgq_grid"].can_configure(&other));
    assert!(owners["shared"].can_configure(&other));
    assert_eq!(
        strategy_catalog::list_order_strategies(&db.pool)
            .await?
            .len(),
        2
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn grid_market_maintenance_preserves_execution_values_and_independent_edits() -> Result<()> {
    use crypto_cta_manager::{
        order_config::{ExecutionAlgorithm, OrderParameters},
        strategy_catalog::{self, SaveOrderStrategyRequest},
    };
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    let request = SaveOrderStrategyRequest {
        strategy_name: "lgq_grid".into(),
        order_parameters: OrderParameters {
            algorithm: ExecutionAlgorithm::Grid,
            single_order_usdt: 125.0,
            orders_per_batch: 4,
            maker_timeout_ms: 2500,
            max_maker_requotes: 6,
            ..Default::default()
        },
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &request, 10, None).await?;
    sqlx::raw_sql("UPDATE cta_order_strategies SET grid = '{\"open_buy_vol_scale\":[0,1],\"open_sell_vol_scale\":[0,1],\"open_offset_lower\":0,\"vol_stale_ms\":15000,\"quote_stale_ms\":1000}'::jsonb")
        .execute(&db.pool).await?;
    sqlx::raw_sql(include_str!("../scripts/apply_grid_market_parameters.sql"))
        .execute(&db.pool)
        .await?;
    let mut saved = strategy_catalog::list_order_strategies(&db.pool)
        .await?
        .remove(0);
    assert_eq!(saved.updated_at_us, 10);
    assert_eq!(saved.order_parameters.single_order_usdt, 125.0);
    assert_eq!(saved.order_parameters.grid.spot.orders_per_batch, 4);
    assert_eq!(saved.order_parameters.grid.futures.orders_per_batch, 4);
    assert_eq!(saved.order_parameters.grid.spot.maker_timeout_ms, 2500);
    assert_eq!(saved.order_parameters.grid.futures.maker_timeout_ms, 2500);
    assert_eq!(saved.order_parameters.grid.spot.max_maker_requotes, Some(6));
    assert_eq!(saved.order_parameters.grid.futures.max_maker_requotes, None);
    assert_eq!(saved.order_parameters.grid.spot.vol_multiplier, 1.0);
    assert_eq!(saved.order_parameters.grid.futures.vol_multiplier, 1.0);
    saved.order_parameters.grid.spot.orders_per_batch = 2;
    saved.order_parameters.grid.futures.orders_per_batch = 5;
    saved.order_parameters.grid.futures.maker_timeout_ms = 9000;
    saved.order_parameters.grid.spot.vol_multiplier = 0.5;
    saved.order_parameters.grid.spot.max_maker_requotes = None;
    saved.order_parameters.grid.futures.vol_multiplier = 2.0;
    saved.order_parameters.grid.futures.max_maker_requotes = Some(4);
    let updated = SaveOrderStrategyRequest {
        strategy_name: saved.strategy_name,
        order_parameters: saved.order_parameters.clone(),
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &updated, 20, None).await?;
    sqlx::raw_sql(include_str!("../scripts/apply_grid_market_parameters.sql"))
        .execute(&db.pool)
        .await?;
    assert_eq!(
        strategy_catalog::list_order_strategies(&db.pool)
            .await?
            .remove(0)
            .order_parameters,
        saved.order_parameters
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn grid_controls_maintenance_preserves_existing_market_settings_and_null_limits() -> Result<()>
{
    use crypto_cta_manager::{
        order_config::{ExecutionAlgorithm, OrderParameters},
        strategy_catalog::{self, SaveOrderStrategyRequest},
    };
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    let request = SaveOrderStrategyRequest {
        strategy_name: "market-grid".into(),
        order_parameters: OrderParameters {
            algorithm: ExecutionAlgorithm::Grid,
            ..Default::default()
        },
    };
    strategy_catalog::upsert_order_strategy(&db.pool, &request, 10, None).await?;
    // Existing per-market catalog: no multiplier or futures round control yet.
    sqlx::raw_sql(
        r#"UPDATE cta_order_strategies SET grid =
        '{"spot":{"orders_per_batch":2,"maker_timeout_ms":2500,"max_maker_requotes":4},
          "futures":{"orders_per_batch":5,"maker_timeout_ms":9000}}'::jsonb"#,
    )
    .execute(&db.pool)
    .await?;
    sqlx::raw_sql(include_str!("../scripts/apply_grid_market_parameters.sql"))
        .execute(&db.pool)
        .await?;
    let saved = strategy_catalog::list_order_strategies(&db.pool)
        .await?
        .remove(0);
    assert_eq!(saved.updated_at_us, 10);
    let grid = &saved.order_parameters.grid;
    assert_eq!(grid.spot.orders_per_batch, 2);
    assert_eq!(grid.spot.maker_timeout_ms, 2500);
    assert_eq!(grid.spot.max_maker_requotes, Some(4));
    assert_eq!(grid.futures.orders_per_batch, 5);
    assert_eq!(grid.futures.maker_timeout_ms, 9000);
    let stored: serde_json::Value = sqlx::query_scalar("SELECT grid FROM cta_order_strategies")
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(stored["spot"]["vol_multiplier"], 1.0);
    assert_eq!(stored["futures"]["vol_multiplier"], 1.0);
    assert_eq!(
        stored["futures"].get("max_maker_requotes"),
        Some(&serde_json::Value::Null)
    );
    // A partially completed maintenance must retain explicit unlimited and finite limits.
    sqlx::raw_sql(r#"UPDATE cta_order_strategies SET grid =
        '{"spot":{"orders_per_batch":2,"maker_timeout_ms":2500,"max_maker_requotes":null},
          "futures":{"orders_per_batch":5,"maker_timeout_ms":9000,"vol_multiplier":2.5,"max_maker_requotes":0}}'::jsonb"#)
        .execute(&db.pool).await?;
    for _ in 0..2 {
        sqlx::raw_sql(include_str!("../scripts/apply_grid_market_parameters.sql"))
            .execute(&db.pool)
            .await?;
    }
    let saved = strategy_catalog::list_order_strategies(&db.pool)
        .await?
        .remove(0);
    assert_eq!(saved.updated_at_us, 10);
    assert_eq!(saved.order_parameters.grid.spot.max_maker_requotes, None);
    assert_eq!(saved.order_parameters.grid.spot.vol_multiplier, 1.0);
    assert_eq!(
        saved.order_parameters.grid.futures.max_maker_requotes,
        Some(0)
    );
    assert_eq!(saved.order_parameters.grid.futures.vol_multiplier, 2.5);
    db.remove().await
}
