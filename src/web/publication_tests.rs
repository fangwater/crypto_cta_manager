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
async fn auth_sessions_update_immediately_without_interrupting_publishers() {
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
    let name = format!(
        "cta_auth_publish_test_{}_{}",
        std::process::id(),
        unix_now_us()
    );
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
    let (_dir, mut state, _, _, _) = super::tests::timeline_test_state(90);
    state.pool = pool.clone();
    state
        .targets
        .load(&pool, &state.position_archive)
        .await
        .unwrap();

    let registered = auth_register(
        State(state.clone()),
        HeaderMap::new(),
        Json(auth::RegisterRequest {
            username: "session-admin".into(),
            password: "test-password".into(),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("{}", e.0));
    assert_eq!(registered.status(), StatusCode::OK);
    let cookie = registered.headers()[header::SET_COOKIE].to_str().unwrap();
    let initial_token = auth::extract_session_cookie(Some(cookie)).unwrap();
    let owner = state
        .targets
        .session(&initial_token)
        .expect("registration caches the session immediately");
    assert!(owner.is_admin());
    assert!(state.targets.catalog().is_ok());
    assert_eq!(
        publish(state.clone(), owner.clone(), "live", 1.0)
            .await
            .status(),
        StatusCode::OK
    );
    let new_user = auth_create_user(
        State(state.clone()),
        Extension(owner.clone()),
        Json(auth::CreateUserRequest {
            username: "session-user".into(),
            password: "test-password".into(),
            role: Some("admin".into()),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("{}", e.0));
    assert_eq!(new_user.status(), StatusCode::OK);
    assert_eq!(
        publish(state.clone(), owner.clone(), "live", 2.0)
            .await
            .status(),
        StatusCode::OK
    );

    // Hold real login SQL, then publish while the handler is waiting for PG.
    let mut blocked = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE cta_users IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocked)
        .await
        .unwrap();
    let login_state = state.clone();
    let login = tokio::spawn(async move {
        auth_login(
            State(login_state),
            Json(auth::LoginRequest {
                username: "session-admin".into(),
                password: "test-password".into(),
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("{}", e.0))
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT user_id, username, password_hash, role%')")
                .fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(
        publish(state.clone(), owner.clone(), "live", 3.0)
            .await
            .status(),
        StatusCode::OK
    );
    blocked.rollback().await.unwrap();
    let response = login.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let token = auth::extract_session_cookie(Some(cookie)).unwrap();
    let cached = state
        .targets
        .session(&token)
        .expect("login caches the session immediately");
    assert_eq!(
        publish(state.clone(), cached, "live", 4.0).await.status(),
        StatusCode::OK
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("cta_session={token}").parse().unwrap(),
    );
    assert_eq!(
        auth_logout(State(state.clone()), headers)
            .await
            .unwrap_or_else(|e| panic!("{}", e.0))
            .status(),
        StatusCode::NO_CONTENT
    );
    assert!(state.targets.session(&token).is_none());
    assert!(auth::load_session(&pool, &token).await.unwrap().is_none());
    assert_eq!(
        publish(state.clone(), owner.clone(), "live", 5.0)
            .await
            .status(),
        StatusCode::OK
    );
    // Configuration reloads retain active sessions and cannot resurrect logout.
    state
        .targets
        .load(&pool, &state.position_archive)
        .await
        .unwrap();
    assert!(state.targets.session(&initial_token).is_some());
    assert!(state.targets.session(&token).is_none());
    let failed = auth_login(
        State(state.clone()),
        Json(auth::LoginRequest {
            username: "session-admin".into(),
            password: "wrong-password".into(),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("{}", e.0));
    assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        publish(state.clone(), owner.clone(), "live", 6.0)
            .await
            .status(),
        StatusCode::OK
    );

    // Role changes still invalidate publication authorization, including all
    // of this user's remaining sessions, until the configuration reload.
    let demoted = auth_user_role(
        State(state.clone()),
        Extension(owner.clone()),
        Extension(VisibleSources(BTreeSet::new())),
        Path(owner.user_id),
        Json(auth::SetRoleRequest {
            role: "user".into(),
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("{}", e.0));
    assert_eq!(demoted.status(), StatusCode::OK);
    assert_eq!(
        publish(state.clone(), owner, "live", 7.0).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    state
        .targets
        .load(&pool, &state.position_archive)
        .await
        .unwrap();
    assert!(!state.targets.session(&initial_token).unwrap().is_admin());
    assert!(state.targets.session(&token).is_none());

    drop(state);
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires isolated CTA_SCHEMA_TEST_SOCKET PostgreSQL on port 15433"]
async fn locked_pg_mirror_does_not_block_existing_targets_and_creator_race() {
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
    state
        .targets
        .load(&pool, &state.position_archive)
        .await
        .unwrap();
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

    // The latest-target PG row may remain locked while the same strategy
    // continues publishing and recording successive durable target updates.
    let mut row_lock = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM cta_position_strategies WHERE strategy_name='slow' FOR UPDATE")
        .execute(&mut *row_lock)
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            publish(state.clone(), owner.clone(), "slow", 1.0)
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    let mirror_targets = state.targets.clone();
    let mirror_pool = pool.clone();
    let mirror = tokio::spawn(async move { mirror_targets.flush(&mirror_pool).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'UPDATE cta_position_strategies%')")
                .fetch_one(&pool).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            publish(state.clone(), owner.clone(), "slow", 2.0)
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            publish(state.clone(), other.clone(), "fast", 2.0)
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    let before: serde_json::Value = sqlx::query_scalar(
        "SELECT targets FROM cta_position_strategies WHERE strategy_name='slow'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(before, serde_json::json!({}));
    assert_eq!(
        state.targets.current("slow").unwrap().targets["BTCUSDT"].qty,
        2.0
    );
    row_lock.rollback().await.unwrap();
    mirror.await.unwrap().unwrap();
    state.targets.flush(&pool).await.unwrap();
    let targets: serde_json::Value = sqlx::query_scalar(
        "SELECT targets FROM cta_position_strategies WHERE strategy_name='slow'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(targets["BTCUSDT"]["qty"], 2.0);
    state
        .targets
        .flush_archive(&state.position_archive)
        .await
        .unwrap();
    assert_eq!(state.position_archive.scan_from(0).unwrap().len(), 3);

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
    state
        .targets
        .flush_archive(&state.position_archive)
        .await
        .unwrap();
    assert_eq!(state.position_archive.scan_from(0).unwrap().len(), 4);

    // A name created outside this process after its last metadata snapshot is
    // still owned by its creator; cold creation must not overwrite that row.
    sqlx::query("INSERT INTO cta_position_strategies(strategy_name,targets,created_by_user_id,updated_at_us) VALUES ('external','{\"BTCUSDT\":{\"qty\":33,\"signal\":0}}',2,1)").execute(&pool).await.unwrap();
    assert_eq!(
        publish(
            state.clone(),
            AuthUser {
                user_id: 1,
                username: "creator-a".into(),
                role: "user".into()
            },
            "external",
            99.0
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let external: serde_json::Value = sqlx::query_scalar(
        "SELECT targets FROM cta_position_strategies WHERE strategy_name='external'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(external["BTCUSDT"]["qty"], 33);

    // A delayed mirror must not overwrite a new strategy reusing a deleted name.
    sqlx::query(
        "INSERT INTO cta_position_strategies(strategy_name,updated_at_us) VALUES ('recreated',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let stale = Arc::new(target_state::Targets::default());
    stale.load(&pool, &state.position_archive).await.unwrap();
    let mut old = stale.catalog().unwrap().entries["recreated"].clone();
    old.strategy.updated_at_us = unix_now_us();
    old.strategy.targets.insert(
        "BTCUSDT".into(),
        crate::order_config::TargetPosition::new(55.0, 0).unwrap(),
    );
    stale.accept(old);
    sqlx::query("DELETE FROM cta_position_strategies WHERE strategy_name='recreated'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO cta_position_strategies(strategy_name,targets,updated_at_us) VALUES ('recreated','{\"BTCUSDT\":{\"qty\":77,\"signal\":0}}',1)").execute(&pool).await.unwrap();
    stale.flush(&pool).await.unwrap();
    let target: serde_json::Value = sqlx::query_scalar(
        "SELECT targets FROM cta_position_strategies WHERE strategy_name='recreated'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(target["BTCUSDT"]["qty"], 77);

    drop(state);
    pool.close().await;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
