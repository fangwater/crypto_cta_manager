//! Handler regressions against the same isolated PostgreSQL fixture as the
//! schema tests. No live API, Redis or exchange requests are made.
use super::*;
use sqlx::AssertSqlSafe;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

async fn publish(state: WebState, user: AuthUser, name: &str, qty: f64) -> Response {
    save_position_strategy(
        State(state),
        ConnectInfo("127.0.0.1:12345".parse().unwrap()),
        Some(Extension(user)),
        HeaderMap::new(),
        Json(SavePositionStrategyRequest {
            strategy_name: name.into(),
            targets: BTreeMap::from([(
                "BTCUSDT".into(),
                crate::order_config::TargetPosition::new(qty, 0).unwrap(),
            )]),
            symbol_order_strategy_overrides: BTreeMap::new(),
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("publish failed: {}", error.0))
}

#[tokio::test]
#[ignore = "requires isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL on port 15433"]
async fn independent_handlers_disconnect_completion_and_creator_race() {
    let socket = std::env::var("CTA_SCHEMA_TEST_SOCKET").unwrap();
    assert!(socket.starts_with("/tmp/cta-schema-test-"));
    assert!(
        std::path::Path::new(&socket)
            .join(".s.PGSQL.15433")
            .exists()
    );
    let options = PgConnectOptions::new()
        .host(&socket)
        .port(15433)
        .username(&std::env::var("USER").unwrap())
        .database("postgres");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let name = format!("cta_publish_test_{}_{}", std::process::id(), unix_now_us());
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options.database(&name))
        .await
        .unwrap();
    postgres::initialize(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO cta_users(user_id,username,password_hash) VALUES
        (1,'creator-a','unused'),(2,'creator-b','unused');
        INSERT INTO cta_position_strategies(strategy_name,created_by_user_id,updated_at_us)
        VALUES ('slow',1,1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (_dir, mut state, _, _, _) = super::tests::timeline_test_state(90);
    state.pool = pool.clone();
    let owner = AuthUser {
        user_id: 1,
        username: "creator-a".into(),
        role: "user".into(),
    };
    let other = AuthUser {
        user_id: 2,
        username: "creator-b".into(),
        role: "user".into(),
    };

    // Block one real handler at its PostgreSQL write. An unrelated handler must
    // still save and archive successfully while that first handler is waiting.
    let mut row_lock = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM cta_position_strategies WHERE strategy_name='slow' FOR UPDATE")
        .execute(&mut *row_lock)
        .await
        .unwrap();
    let slow = tokio::spawn(publish(state.clone(), owner.clone(), "slow", 1.0));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database()
                 AND wait_event_type='Lock' AND query LIKE '%INSERT INTO cta_position_strategies%')")
                .fetch_one(&pool).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            publish(state.clone(), other.clone(), "fast", 2.0)
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );

    // Simulate the HTTP task disappearing after admission. Saving and archival
    // must still complete before a subsequent configuration writer can enter.
    slow.abort();
    assert!(slow.await.unwrap_err().is_cancelled());
    assert!(state.configuration_lock.try_write().is_err());
    row_lock.rollback().await.unwrap();
    let guard = tokio::time::timeout(Duration::from_secs(5), state.configuration_lock.write())
        .await
        .unwrap();
    let targets: serde_json::Value = sqlx::query_scalar(
        "SELECT targets FROM cta_position_strategies WHERE strategy_name='slow'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(targets["BTCUSDT"]["qty"], 1.0);
    let messages = state.position_archive.scan_from(0).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().any(|m| m.strategy.strategy_name == "slow"));
    assert!(messages.iter().any(|m| m.strategy.strategy_name == "fast"));
    drop(guard);

    // Simultaneous same-name creation must recheck authorization after the
    // winner commits. The second user must neither take ownership nor overwrite.
    let (a, b) = tokio::join!(
        publish(state.clone(), owner, "new-name", 3.0),
        publish(state.clone(), other, "new-name", 4.0),
    );
    assert!(matches!(
        (a.status(), b.status()),
        (StatusCode::OK, StatusCode::FORBIDDEN) | (StatusCode::FORBIDDEN, StatusCode::OK)
    ));
    let row: (i64, serde_json::Value) = sqlx::query_as(
        "SELECT created_by_user_id,targets FROM cta_position_strategies WHERE strategy_name='new-name'")
        .fetch_one(&pool).await.unwrap();
    let winner = if a.status() == StatusCode::OK {
        (1, 3.0)
    } else {
        (2, 4.0)
    };
    assert_eq!(row.0, winner.0);
    assert_eq!(row.1["BTCUSDT"]["qty"], winner.1);
    assert_eq!(state.position_archive.scan_from(0).unwrap().len(), 3);

    drop(state);
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
