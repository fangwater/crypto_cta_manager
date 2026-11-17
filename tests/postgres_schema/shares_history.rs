use super::{TestDatabase, assert_business_data, seed_business_data};
use anyhow::Result;
use crypto_cta_manager::{
    auth::AuthUser,
    order_config::OrderParameters,
    postgres,
    shares_history::{self, HistoryQuery, SharesHistory},
    strategy_catalog::{
        self, SaveBindingRequest, SaveBindingSharesRequest, SaveOrderStrategyRequest,
    },
    virtual_accounts::{
        self, AccountConfiguration, AccountFollow, SaveVirtualAccount, VirtualBinding,
    },
};
use sqlx::postgres::PgPool;

fn actor(id: i64) -> AuthUser {
    AuthUser {
        user_id: id,
        username: format!("operator-{id}"),
        role: "user".into(),
    }
}

fn binding(shares: f64) -> SaveBindingRequest {
    SaveBindingRequest {
        binding_name: "test-strategy".into(),
        position_strategy_name: "test-strategy".into(),
        order_strategy_name: "order".into(),
        shares,
    }
}

async fn database() -> Result<TestDatabase> {
    let db = TestDatabase::create().await?;
    postgres::initialize(&db.pool).await?;
    seed_business_data(&db.pool).await?;
    strategy_catalog::upsert_order_strategy(
        &db.pool,
        &SaveOrderStrategyRequest {
            strategy_name: "order".into(),
            order_parameters: OrderParameters::default(),
        },
        1,
        None,
    )
    .await?;
    strategy_catalog::save_binding(
        &db.pool,
        "test-source",
        &binding(1.0),
        1_000_000,
        Some(&actor(1)),
    )
    .await?;
    Ok(db)
}

async fn change(pool: &PgPool, shares: f64, at: i64, user: i64) -> Result<()> {
    strategy_catalog::save_binding_shares(
        pool,
        "test-source",
        "test-strategy",
        &SaveBindingSharesRequest { shares },
        at,
        Some(&actor(user)),
    )
    .await?;
    Ok(())
}

async fn history(pool: &PgPool) -> Result<SharesHistory> {
    shares_history::list(pool, &["test-source".into()], &HistoryQuery::default()).await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shares_history_merges_a_rolling_minute_and_hides_round_trips() -> Result<()> {
    let db = database().await?;
    change(&db.pool, 2.0, 61_000_001, 1).await?;
    change(&db.pool, 3.0, 121_000_001, 2).await?; // inclusive 60s
    change(&db.pool, 4.0, 180_000_001, 1).await?; // rolling from last actual change
    let entries = history(&db.pool).await?.entries;
    assert_eq!(entries.len(), 2);
    let round = &entries[0];
    assert_eq!((round.initial_shares, round.final_shares), (1.0, 4.0));
    assert_eq!(
        (round.first_changed_at_us, round.last_changed_at_us),
        (61_000_001, 180_000_001)
    );
    assert_eq!(round.change_count, 3);
    assert_eq!(
        round.actors.iter().map(|u| u.user_id).collect::<Vec<_>>(),
        [1, 2]
    );
    let round_id = round.change_id;
    change(&db.pool, 1.0, 181_000_001, 2).await?;
    assert_eq!(history(&db.pool).await?.entries.len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cta_account_strategy_shares_history")
            .fetch_one(&db.pool)
            .await?,
        2
    );
    change(&db.pool, 5.0, 182_000_001, 1).await?;
    assert_eq!(history(&db.pool).await?.entries[0].change_id, round_id);
    change(&db.pool, 5.0, 222_000_001, 2).await?; // unchanged saves do not extend window
    change(&db.pool, 6.0, 242_000_002, 1).await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current[0].shares, 6.0);
    assert_eq!(result.entries.len(), 3);
    assert_eq!(
        (
            result.entries[0].initial_shares,
            result.entries[0].final_shares
        ),
        (5.0, 6.0)
    );
    assert_eq!(result.entries[1].change_id, round_id);
    assert_eq!(result.entries[1].change_count, 5);
    assert_eq!(result.entries[1].last_changed_at_us, 182_000_001);
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shares_history_is_atomic_and_serializes_concurrent_account_changes() -> Result<()> {
    let db = database().await?;
    let mut tx = db.pool.begin().await?;
    let before = shares_history::capture(&mut tx, "test-source").await?;
    sqlx::query("UPDATE cta_account_strategy_bindings SET shares = 9, updated_at_us = 61000001")
        .execute(&mut *tx)
        .await?;
    shares_history::record(&mut tx, before, 61_000_001, Some(&actor(9)), "manual").await?;
    tx.rollback().await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.current[0].shares, 1.0);
    let (a, b) = tokio::join!(
        change(&db.pool, 2.0, 121_000_000, 1),
        change(&db.pool, 3.0, 121_000_000, 2)
    );
    a?;
    b?;
    let result = history(&db.pool).await?;
    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].initial_shares, 1.0);
    assert_eq!(result.entries[0].change_count, 2);
    assert_eq!(result.entries[0].actors.len(), 2);
    assert_eq!(result.entries[0].final_shares, result.current[0].shares);
    strategy_catalog::save_binding_shares(
        &db.pool,
        "test-source",
        "test-strategy",
        &SaveBindingSharesRequest { shares: 4.0 },
        300_000_000,
        None,
    )
    .await?;
    assert!(history(&db.pool).await?.entries[0].actors.is_empty());
    // Missing history storage must roll back the allocation mutation as well.
    sqlx::query("ALTER TABLE cta_account_strategy_shares_history RENAME TO unavailable_history")
        .execute(&db.pool)
        .await?;
    assert!(change(&db.pool, 8.0, 400_000_000, 1).await.is_err());
    assert_eq!(
        strategy_catalog::load_account_studio(&db.pool, "test-source")
            .await?
            .bindings[0]
            .shares,
        4.0
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shares_history_sums_aliases_for_the_same_actual_strategy() -> Result<()> {
    let db = database().await?;
    let mut alias = binding(2.0);
    alias.binding_name = "another-publish-name".into();
    strategy_catalog::save_binding(&db.pool, "test-source", &alias, 2_000_000, Some(&actor(2)))
        .await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current.len(), 1);
    assert_eq!(result.current[0].strategy_name, "test-strategy");
    assert_eq!(result.current[0].shares, 3.0);
    assert_eq!(result.entries.len(), 1);
    assert_eq!(
        (
            result.entries[0].initial_shares,
            result.entries[0].final_shares
        ),
        (0.0, 3.0)
    );
    change(&db.pool, 0.0, 3_000_000, 1).await?;
    assert!(strategy_catalog::delete_binding(&db.pool, "test-source", "test-strategy").await?);
    strategy_catalog::save_binding(&db.pool, "test-source", &binding(5.0), 4_000_000, None).await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current[0].shares, 7.0);
    assert_eq!(result.entries.len(), 1); // publication-binding lifetime is irrelevant
    assert_eq!(result.entries[0].change_count, 4);
    assert_eq!(
        (
            result.entries[0].initial_shares,
            result.entries[0].final_shares
        ),
        (0.0, 7.0)
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shares_history_isolates_actual_accounts_and_paginates_strategies_with_equal_times()
-> Result<()> {
    let db = database().await?;
    sqlx::raw_sql("INSERT INTO cta_order_sources (source_id,account_label,venue_label,rocksdb_path) VALUES ('other-source','other','binance-futures','/tmp/other-exec'); INSERT INTO cta_position_strategies (strategy_name,updated_at_us) VALUES ('other-strategy',1)")
        .execute(&db.pool).await?;
    strategy_catalog::save_binding(&db.pool, "other-source", &binding(7.0), 1_000_000, None)
        .await?;
    let mut second = binding(2.0);
    second.binding_name = "second-binding".into();
    second.position_strategy_name = "other-strategy".into();
    strategy_catalog::save_binding(&db.pool, "test-source", &second, 1_000_000, None).await?;
    let mut query = HistoryQuery {
        limit: Some(1),
        ..Default::default()
    };
    let first = shares_history::list(&db.pool, &["test-source".into()], &query).await?;
    assert_eq!(first.entries[0].strategy_name, "other-strategy");
    assert_eq!(first.current.len(), 2);
    let cursor = first.next_cursor.unwrap();
    query.before_at_us = Some(cursor.at_us);
    query.before_id = Some(cursor.change_id);
    let next = shares_history::list(&db.pool, &["test-source".into()], &query).await?;
    assert_eq!(next.entries[0].strategy_name, "test-strategy");
    assert!(next.next_cursor.is_none());
    query = HistoryQuery {
        strategy_name: Some("test-strategy".into()),
        ..Default::default()
    };
    let other = shares_history::list(&db.pool, &["other-source".into()], &query).await?;
    assert_eq!(other.current[0].shares, 7.0);
    assert_eq!(other.entries[0].final_shares, 7.0);
    assert!(
        shares_history::list(&db.pool, &[], &query)
            .await?
            .current
            .is_empty()
    );
    let combined = shares_history::list(
        &db.pool,
        &["test-source".into(), "other-source".into()],
        &query,
    )
    .await?;
    assert_eq!(combined.current.len(), 2); // separate venue/execution sources are never added
    assert!(
        HistoryQuery {
            before_id: Some(1),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        HistoryQuery {
            limit: Some(201),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shares_history_expands_nested_virtuals_and_records_only_final_leaf_totals() -> Result<()> {
    let db = database().await?;
    let leaf = |name: &str, shares| SaveVirtualAccount {
        name: name.into(),
        children: None,
        bindings: vec![VirtualBinding {
            binding_name: name.into(),
            position_strategy_name: "test-strategy".into(),
            order_strategy_name: "order".into(),
            shares,
        }],
    };
    virtual_accounts::save(
        &db.pool,
        "left",
        &leaf("alias-left", 2.0),
        10_000_000,
        Some(&actor(1)),
    )
    .await?;
    virtual_accounts::save(
        &db.pool,
        "right",
        &leaf("alias-right", 3.0),
        10_000_000,
        Some(&actor(1)),
    )
    .await?;
    let root = |left, right| SaveVirtualAccount {
        name: "Root".into(),
        bindings: vec![],
        children: Some(vec![
            AccountFollow {
                virtual_id: "left".into(),
                multiplier: left,
            },
            AccountFollow {
                virtual_id: "right".into(),
                multiplier: right,
            },
        ]),
    };
    virtual_accounts::save(
        &db.pool,
        "root",
        &root(2.0, 2.0),
        10_000_000,
        Some(&actor(1)),
    )
    .await?;
    assert_eq!(history(&db.pool).await?.entries.len(), 1); // unfollowed Virtual edits are not actual allocations
    let follow = |multiplier| AccountConfiguration::Follow {
        follows: vec![AccountFollow {
            virtual_id: "root".into(),
            multiplier,
        }],
    };
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &follow(0.5),
        61_000_001,
        Some(&actor(2)),
    )
    .await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current.len(), 1);
    assert_eq!(result.current[0].strategy_name, "test-strategy");
    assert_eq!(result.current[0].shares, 5.0); // (2*2 + 3*2)*0.5
    assert_eq!(result.entries[0].change_count, 1); // three materialized binding mutations become one account change
    assert_eq!(
        (
            result.entries[0].initial_shares,
            result.entries[0].final_shares
        ),
        (1.0, 5.0)
    );
    virtual_accounts::save(
        &db.pool,
        "root",
        &root(5.0, 0.0),
        100_000_000,
        Some(&actor(9)),
    )
    .await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current[0].shares, 5.0); // contribution paths changed, actual strategy total did not
    assert_eq!(result.entries[0].change_count, 1);
    assert_eq!(result.entries[0].last_changed_at_us, 61_000_001);
    assert_eq!(result.entries[0].actors[0].user_id, 2);
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &follow(1.0),
        121_000_002,
        Some(&actor(2)),
    )
    .await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current[0].shares, 10.0);
    assert_eq!(result.entries.len(), 3); // >60s from last actual total change
    assert_eq!(
        (
            result.entries[0].initial_shares,
            result.entries[0].final_shares
        ),
        (5.0, 10.0)
    );
    virtual_accounts::set_configuration(
        &db.pool,
        "test-source",
        &AccountConfiguration::Independent,
        130_000_000,
        Some(&actor(2)),
    )
    .await?;
    virtual_accounts::save(
        &db.pool,
        "root",
        &root(0.0, 0.0),
        140_000_000,
        Some(&actor(9)),
    )
    .await?;
    assert_eq!(history(&db.pool).await?.current[0].shares, 10.0); // detached Virtual cannot change the actual account
    db.remove().await
}

#[tokio::test]
#[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
async fn shares_history_maintenance_preserves_current_allocations_without_inventing_history()
-> Result<()> {
    let db = database().await?;
    let snapshot = postgres::load_latest_position_snapshot(&db.pool, "test-source")
        .await?
        .unwrap();
    sqlx::query("DROP TABLE cta_account_strategy_shares_history")
        .execute(&db.pool)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../scripts/apply_account_strategy_shares_history_schema.sql"
    ))
    .execute(&db.pool)
    .await?;
    assert_business_data(&db.pool, &snapshot).await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.current[0].shares, 1.0);
    assert!(result.entries.is_empty());
    change(&db.pool, 2.0, 70_000_000, 1).await?;
    let result = history(&db.pool).await?;
    assert_eq!(result.entries.len(), 1);
    assert_eq!(
        (
            result.entries[0].initial_shares,
            result.entries[0].final_shares
        ),
        (1.0, 2.0)
    );
    db.remove().await
}
