//! Daily broker equity persistence against the explicit current schema.
use anyhow::{Context, Result, ensure};
use crypto_cta_manager::{
    native_futures::funds::{BrokerFunds, DailyEquity, store},
    postgres,
};
use sqlx::{
    AssertSqlSafe,
    postgres::{PgConnectOptions, PgPoolOptions},
};

fn funds(day: &str, observed_us: i64, balance: f64) -> BrokerFunds {
    BrokerFunds {
        observed_us,
        trading_day: day.into(),
        currency: "CNY".into(),
        pre_balance: 500_000.0,
        deposit: 0.0,
        withdraw: 0.0,
        balance,
        available: balance,
        withdraw_quota: balance,
        curr_margin: 90_000.0,
        frozen_margin: 0.0,
        frozen_cash: 0.0,
        frozen_commission: 0.0,
        position_profit: 0.0,
        close_profit: 0.0,
        commission: 0.0,
    }
}
fn daily(day: &str, observed_us: i64, balance: f64, positions: Option<(f64, i64)>) -> DailyEquity {
    DailyEquity {
        source_id: "cn_funds_test".into(),
        funds: funds(day, observed_us, balance),
        long_notional: positions.map(|(n, _)| n),
        short_notional: positions.map(|(n, _)| n / 2.0),
        positions_at_us: positions.map(|(_, at)| at),
    }
}

#[tokio::test]
#[ignore = "requires an isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL cluster"]
async fn daily_equity_keeps_the_latest_observation_and_retains_replayed_notionals() -> Result<()> {
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
        "cn_funds_test_{}_{}",
        std::process::id(),
        crypto_cta_manager::native_futures::now_us()
    );
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.database(&name))
        .await?;
    postgres::initialize(&pool).await?;
    sqlx::query(
        "INSERT INTO cta_order_sources(source_id,account_label,venue_label,rocksdb_path)
         VALUES('cn_funds_test','test','ctp','/nonexistent/cn_funds_test')",
    )
    .execute(&pool)
    .await?;

    let base = 1_795_415_513_000_000;
    store::upsert(
        &pool,
        &daily("20261120", base, 499_000.0, Some((100.0, base))),
    )
    .await?;
    store::upsert(
        &pool,
        &daily("20261123", base + 10, 501_000.0, Some((300.0, base + 9))),
    )
    .await?;
    // A later observation without a replayed position keeps the day's notionals.
    store::upsert(&pool, &daily("20261123", base + 20, 502_000.0, None)).await?;
    // An older observation never replaces a newer one.
    store::upsert(
        &pool,
        &daily("20261123", base + 15, 1.0, Some((1.0, base + 15))),
    )
    .await?;
    let rows = store::list(&pool, "cn_funds_test").await?;
    ensure!(rows.len() == 2, "{rows:?}");
    ensure!(rows[0].funds.trading_day == "20261120" && rows[1].funds.trading_day == "20261123");
    ensure!(rows[1].funds.balance == 502_000.0 && rows[1].funds.observed_us == base + 20);
    ensure!(
        rows[1].long_notional == Some(300.0)
            && rows[1].short_notional == Some(150.0)
            && rows[1].positions_at_us == Some(base + 9),
        "{:?}",
        rows[1]
    );
    // Invalid funds are refused before reaching the table.
    ensure!(
        store::upsert(&pool, &daily("2026-11-24", base + 30, 1.0, None))
            .await
            .is_err()
    );

    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await?;
    Ok(())
}
