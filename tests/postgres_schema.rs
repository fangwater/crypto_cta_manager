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
