use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use crate::account_ipc::LiveEquityHub;
use crate::config::{AppConfig, FeeRates, SourceConfig};
use crate::manager_db::ManagerDb;
use crate::order_config::{
    ExecConfigClient, ExecConfigError, OrderStrategyView, SaveOrderParametersRequest,
    validate_strategy_name,
};
use crate::position_archive::PositionArchive;
use crate::redis_runtime::RedisRuntime;
use crate::reload_notify::ReloadNotifyHub;
use crate::strategy_catalog::{
    self, SaveBindingRequest, SaveBindingSharesRequest, SaveEstimatedFeeRateRequest,
    SaveFeeRatesRequest, SaveOrderStrategyRequest, SavePositionStrategyRequest,
    SaveSymbolContractLeverageRequest,
};
use crate::twap::TwapStore;
use crate::viz_snapshot::{SourceFactualPositions, VizSnapshotClient};
use crate::{nav, postgres};

const NO_STORE: [(header::HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];
const MANAGER_PUBLISH_CLIENT: &[u8] = include_bytes!("../scripts/manager_publish_client.py");

#[derive(Clone, Debug, Serialize)]
pub struct DashboardSnapshot {
    pub generated_at_us: i64,
    pub generation_duration_ms: u64,
    pub refresh_interval_secs: u64,
    pub accounts: Vec<DashboardAccount>,
    pub report: nav::NavReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct DashboardAccount {
    pub source_id: String,
    pub account: String,
    pub venue: String,
    pub enabled: bool,
    pub gateway_prefix: Option<String>,
    pub configurable: bool,
    pub live_equity_usdt: Option<f64>,
    pub live_equity_status: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineSnapshot {
    pub generated_at_us: i64,
    pub generation_duration_ms: u64,
    pub report: nav::NavTimelineReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub source_count: usize,
    pub generated_at_us: i64,
    pub last_attempt_at_us: i64,
    pub refresh_interval_secs: u64,
    pub last_refresh_error: Option<String>,
}

#[derive(Debug)]
struct CacheState {
    dashboard: DashboardSnapshot,
    nav_histories: Arc<nav::NavSourceHistories>,
    position_snapshots: Arc<nav::SourcePositionSnapshots>,
    last_attempt_at_us: i64,
    last_refresh_error: Option<String>,
}

struct DashboardBuild {
    dashboard: DashboardSnapshot,
    nav_histories: Arc<nav::NavSourceHistories>,
    position_snapshots: Arc<nav::SourcePositionSnapshots>,
}

#[derive(Clone)]
struct WebState {
    cache: Arc<RwLock<CacheState>>,
    config: Arc<AppConfig>,
    pool: PgPool,
    exec_config: ExecConfigClient,
    redis_runtime: RedisRuntime,
    reload_notify: ReloadNotifyHub,
    live_equity: LiveEquityHub,
    position_archive: Arc<PositionArchive>,
    twap: Arc<TwapStore>,
    viz_snapshot: VizSnapshotClient,
    refresh_interval_secs: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    source_ids: Option<String>,
    symbols: Option<String>,
    max_points: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionCostQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    #[serde(alias = "windowSecs")]
    window_sec: Option<u64>,
    source_ids: Option<String>,
    strategy_name: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
struct ExecutionCostSnapshot {
    generated_at_us: i64,
    generation_duration_ms: u64,
    report: crate::execution_cost::ExecutionCostReport,
}

#[derive(Debug, Deserialize)]
struct StrategyQuery {
    name: String,
}

#[derive(Debug, Serialize)]
struct StrategyListResponse {
    source_id: String,
    strategies: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AuthResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct BindingPublishResult {
    source_id: String,
    binding_name: String,
    shares: f64,
    published: Option<OrderStrategyView>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SavedPositionStrategyResponse {
    #[serde(flatten)]
    strategy: strategy_catalog::PositionStrategy,
    publishes: Vec<BindingPublishResult>,
}

struct ApiError(anyhow::Error);

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error!(error = ?self.0, "CTA web API request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal server error".to_string(),
            }),
        )
            .into_response()
    }
}

pub async fn serve(config: AppConfig, bind: SocketAddr, refresh_interval_secs: u64) -> Result<()> {
    if refresh_interval_secs == 0 {
        anyhow::bail!("web refresh interval must be greater than zero");
    }

    let database_url = config.database_url()?;
    let pool = postgres::connect(&database_url, config.database.max_connections).await?;
    postgres::migrate(&pool).await?;
    postgres::register_sources(&pool, &config.sources).await?;
    let exec_config = ExecConfigClient::new(config.order_config.request_timeout_secs)?;
    let redis_runtime = RedisRuntime::connect(config.redis.clone())?;
    redis_runtime.spawn_keepalive();
    let reload_notify = ReloadNotifyHub::spawn();
    let live_equity = LiveEquityHub::spawn(&config.sources);
    let viz_snapshot = VizSnapshotClient::new(config.order_config.request_timeout_secs)?;
    let manager_db = ManagerDb::open(&config.twap.rocksdb_path)?;
    let position_archive = Arc::new(PositionArchive::open(manager_db.clone())?);
    let twap = Arc::new(TwapStore::from_db(
        manager_db.clone(),
        config.twap.retain_days.max(1),
    )?);
    crate::twap::spawn_with_db(pool.clone(), config.twap.clone(), manager_db);
    let first_build = build_dashboard(&config, &pool, refresh_interval_secs, &live_equity).await?;
    let cache = Arc::new(RwLock::new(CacheState {
        last_attempt_at_us: first_build.dashboard.generated_at_us,
        dashboard: first_build.dashboard,
        nav_histories: first_build.nav_histories,
        position_snapshots: first_build.position_snapshots,
        last_refresh_error: None,
    }));

    let refresh_cache = Arc::clone(&cache);
    let refresh_config = config.clone();
    let refresh_pool = pool.clone();
    let refresh_live = live_equity.clone();
    tokio::spawn(async move {
        refresh_loop(
            refresh_config,
            refresh_pool,
            refresh_cache,
            refresh_live,
            refresh_interval_secs,
        )
        .await;
    });

    let app = Router::new()
        .route("/api/health", get(health))
        .route(
            "/api/manager_publish_client.py",
            get(manager_publish_client),
        )
        .route("/api/dashboard", get(dashboard))
        .route("/api/timeline", get(timeline))
        .route("/api/catalog/execution-cost", get(execution_cost))
        .route("/api/order-config/auth", post(order_config_auth))
        .route(
            "/api/order-config/{source_id}/strategies",
            get(order_config_strategies),
        )
        .route(
            "/api/order-config/{source_id}/strategy",
            get(order_config_strategy),
        )
        .route(
            "/api/order-config/{source_id}/order-parameters",
            post(save_order_parameters),
        )
        .route(
            "/api/catalog/position-strategies",
            get(list_position_strategies).post(save_position_strategy),
        )
        .route(
            "/api/catalog/position-strategies/{name}",
            delete(delete_position_strategy),
        )
        .route(
            "/api/catalog/order-strategies",
            get(list_order_strategies).post(save_order_strategy),
        )
        .route(
            "/api/catalog/order-strategies/{name}",
            delete(delete_order_strategy),
        )
        .route("/api/catalog/accounts/{source_id}", get(get_account_studio))
        .route(
            "/api/catalog/accounts/{source_id}/estimated-fee-rate",
            put(save_account_estimated_fee_rate),
        )
        .route(
            "/api/catalog/accounts/{source_id}/fee-rates",
            put(save_account_fee_rates),
        )
        .route(
            "/api/catalog/accounts/{source_id}/contract-leverage",
            get(get_account_symbol_contract_leverage).put(save_account_symbol_contract_leverage),
        )
        .route(
            "/api/catalog/accounts/{source_id}/bindings",
            post(save_account_binding),
        )
        .route(
            "/api/catalog/accounts/{source_id}/bindings/{binding_name}",
            delete(delete_account_binding),
        )
        .route(
            "/api/catalog/accounts/{source_id}/bindings/{binding_name}/shares",
            put(save_account_binding_shares),
        )
        .route(
            "/api/catalog/accounts/{source_id}/bindings/{binding_name}/publish",
            post(publish_account_binding),
        )
        .with_state(WebState {
            cache,
            config: Arc::new(config),
            pool,
            exec_config,
            redis_runtime,
            reload_notify,
            live_equity,
            position_archive,
            twap,
            viz_snapshot,
            refresh_interval_secs,
        })
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind CTA web API to {bind}"))?;
    info!(%bind, refresh_interval_secs, "CTA web API started");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("CTA web API stopped unexpectedly")
}

async fn dashboard(State(state): State<WebState>) -> impl IntoResponse {
    let mut dashboard = state.cache.read().await.dashboard.clone();
    let now_ms = unix_now_ms();
    for account in &mut dashboard.accounts {
        if let Some(snapshot) = state.live_equity.get(&account.source_id) {
            account.live_equity_usdt = Some(snapshot.equity_usdt);
            account.live_equity_status = Some(live_equity_status(snapshot.ts_ms, now_ms));
        }
    }
    (NO_STORE, Json(dashboard))
}

async fn manager_publish_client() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/x-python; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"manager_publish_client.py\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        MANAGER_PUBLISH_CLIENT,
    )
}

async fn health(State(state): State<WebState>) -> impl IntoResponse {
    let cache = state.cache.read().await;
    let response = HealthResponse {
        status: if cache.last_refresh_error.is_some() {
            "degraded"
        } else {
            "ok"
        },
        source_count: cache.dashboard.report.source_count,
        generated_at_us: cache.dashboard.generated_at_us,
        last_attempt_at_us: cache.last_attempt_at_us,
        refresh_interval_secs: cache.dashboard.refresh_interval_secs,
        last_refresh_error: cache.last_refresh_error.clone(),
    };
    (NO_STORE, Json(response))
}

async fn timeline(
    State(state): State<WebState>,
    Query(query): Query<TimelineQuery>,
) -> Result<Response, ApiError> {
    let selected_source_ids = parse_csv(query.source_ids.as_deref(), false);
    if let Err(message) = resolve_sources(&state.config, &selected_source_ids) {
        return Ok(bad_request(message));
    }
    let selected_symbols = parse_csv(query.symbols.as_deref(), true);
    let start_ts_us = match query.start_ms {
        Some(value) => match milliseconds_to_microseconds(value, "startMs") {
            Ok(value) => Some(value),
            Err(message) => return Ok(bad_request(message)),
        },
        None => None,
    };
    let end_ts_us = match milliseconds_to_microseconds(
        query.end_ms.unwrap_or_else(|| unix_now_us() / 1_000),
        "endMs",
    ) {
        Ok(value) => value,
        Err(message) => return Ok(bad_request(message)),
    };
    if start_ts_us.is_some_and(|start_ts_us| end_ts_us < start_ts_us) {
        return Ok(bad_request(
            "endMs must be greater than or equal to startMs".to_string(),
        ));
    }

    let started = Instant::now();
    let (histories, snapshots, data_generated_at_us) = {
        let cache = state.cache.read().await;
        (
            Arc::clone(&cache.nav_histories),
            Arc::clone(&cache.position_snapshots),
            cache.dashboard.generated_at_us,
        )
    };
    let fee_rates = postgres::load_fee_rates(&state.pool).await?;
    let request = nav::NavTimelineRequest {
        start_ts_us,
        end_ts_us,
        selected_source_ids,
        selected_symbols,
        max_points: query.max_points.unwrap_or(3_000).clamp(200, 10_000),
    };
    let config = Arc::clone(&state.config)
        .as_ref()
        .clone()
        .with_fee_rates(&fee_rates);
    let report = tokio::task::spawn_blocking(move || {
        nav::rebuild_nav_timeline_from_histories_with_snapshots(
            &config, request, &snapshots, &histories,
        )
    })
    .await
    .context("CTA timeline rebuild task failed")?;
    let report = match report {
        Ok(report) => report,
        Err(error) if is_timeline_request_error(&error) => {
            return Ok(bad_request(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    Ok((
        NO_STORE,
        Json(TimelineSnapshot {
            generated_at_us: data_generated_at_us,
            generation_duration_ms,
            report,
        }),
    )
        .into_response())
}

async fn execution_cost(
    State(state): State<WebState>,
    Query(query): Query<ExecutionCostQuery>,
) -> Result<Response, ApiError> {
    let selected_source_ids = parse_csv(query.source_ids.as_deref(), false);
    if let Err(message) = resolve_sources(&state.config, &selected_source_ids) {
        return Ok(bad_request(message));
    }
    let start_received_at_us = match query.start_ms {
        Some(value) => match milliseconds_to_microseconds(value, "startMs") {
            Ok(value) => value,
            Err(message) => return Ok(bad_request(message)),
        },
        None => 1,
    };
    let end_received_at_us = match query.end_ms {
        Some(value) => match milliseconds_to_microseconds(value, "endMs") {
            Ok(value) => Some(value),
            Err(message) => return Ok(bad_request(message)),
        },
        None => None,
    };
    if end_received_at_us.is_some_and(|end| end < start_received_at_us) {
        return Ok(bad_request(
            "endMs must be greater than or equal to startMs".to_string(),
        ));
    }
    let window_secs = query
        .window_sec
        .unwrap_or(crate::execution_cost::DEFAULT_WINDOW_SECS);
    let strategy_name = query
        .strategy_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(name) = strategy_name
        && let Err(message) = validate_strategy_name(name)
    {
        return Ok(bad_request(message));
    }
    let page = query.page.unwrap_or(1);
    let page_size = query
        .page_size
        .unwrap_or(crate::execution_cost::DEFAULT_PAGE_SIZE);
    if page == 0 {
        return Ok(bad_request("page must be greater than zero".to_string()));
    }
    if page_size == 0 || page_size > crate::execution_cost::MAX_PAGE_SIZE {
        return Ok(bad_request(format!(
            "pageSize must be between 1 and {}",
            crate::execution_cost::MAX_PAGE_SIZE
        )));
    }

    let started = Instant::now();
    let config = Arc::clone(&state.config);
    let archive = Arc::clone(&state.position_archive);
    let twap = Arc::clone(&state.twap);
    let (histories, generated_at_us) = {
        let cache = state.cache.read().await;
        (
            Arc::clone(&cache.nav_histories),
            cache.dashboard.generated_at_us,
        )
    };
    let source_ids = selected_source_ids.clone();
    let strategy_name = strategy_name.map(str::to_string);
    let report = tokio::task::spawn_blocking(move || {
        crate::execution_cost::report_execution_cost(
            &config,
            &archive,
            &twap,
            start_received_at_us,
            end_received_at_us,
            window_secs,
            generated_at_us,
            &source_ids,
            strategy_name.as_deref(),
            page,
            page_size,
            &histories,
        )
    })
    .await
    .context("CTA execution-cost rebuild task failed")?;
    let report = match report {
        Ok(report) => report,
        Err(error) if is_execution_cost_request_error(&error) => {
            return Ok(bad_request(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    Ok((
        NO_STORE,
        Json(ExecutionCostSnapshot {
            generated_at_us,
            generation_duration_ms,
            report,
        }),
    )
        .into_response())
}

async fn order_config_auth() -> Response {
    (NO_STORE, Json(AuthResponse { ok: true })).into_response()
}

async fn order_config_strategies(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
) -> Response {
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return response,
    };
    match state
        .exec_config
        .list_strategies(source.exec_config_url.as_deref().unwrap_or_default())
        .await
    {
        Ok(strategies) => (
            NO_STORE,
            Json(StrategyListResponse {
                source_id,
                strategies,
            }),
        )
            .into_response(),
        Err(error) => exec_config_error_response(&error),
    }
}

async fn order_config_strategy(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    Query(query): Query<StrategyQuery>,
) -> Response {
    if let Err(message) = validate_strategy_name(&query.name) {
        return bad_request(message);
    }
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return response,
    };
    match state
        .exec_config
        .load_strategy(
            &source_id,
            source.exec_config_url.as_deref().unwrap_or_default(),
            &query.name,
        )
        .await
    {
        Ok(strategy) => (NO_STORE, Json(strategy)).into_response(),
        Err(error) => exec_config_error_response(&error),
    }
}

async fn save_order_parameters(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<SaveOrderParametersRequest>,
) -> Result<Response, ApiError> {
    if let Err(message) = validate_strategy_name(&request.strategy_name) {
        return Ok(bad_request(message));
    }
    let Some(expected_updated_at_us) = request.expected_updated_at_us else {
        return Ok(bad_request(
            "expected_updated_at_us is required for order parameter updates".to_string(),
        ));
    };
    if expected_updated_at_us <= 0 {
        return Ok(bad_request(
            "expected_updated_at_us must be positive".to_string(),
        ));
    }
    if let Err(message) = request.order_parameters.validate() {
        return Ok(bad_request(message));
    }
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return Ok(response),
    };
    let exec_config_url = source.exec_config_url.as_deref().unwrap_or_default();
    let previous = match state
        .exec_config
        .load_strategy(&source_id, exec_config_url, &request.strategy_name)
        .await
    {
        Ok(previous) => previous,
        Err(error) => return Ok(exec_config_error_response(&error)),
    };
    if previous.updated_at_us != Some(expected_updated_at_us) {
        return Ok((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "strategy config changed after it was loaded; reload before saving"
                    .to_string(),
            }),
        )
            .into_response());
    }
    let previous_json = serde_json::to_string(&previous.order_parameters)?;
    let requested_json = serde_json::to_string(&request.order_parameters)?;
    let client_addr = client_addr.ip().to_string();
    let audit_id = postgres::begin_exec_order_config_audit(
        &state.pool,
        &source_id,
        &request.strategy_name,
        &client_addr,
        request.expected_updated_at_us,
        &previous_json,
        &requested_json,
    )
    .await?;

    let mut saved = match state
        .exec_config
        .save_order_parameters(&source_id, exec_config_url, &request)
        .await
    {
        Ok(saved) => saved,
        Err(error) => {
            if let Err(audit_error) = postgres::complete_exec_order_config_audit(
                &state.pool,
                audit_id,
                "failed",
                None,
                Some(error.public_message()),
            )
            .await
            {
                error!(audit_id, error = ?audit_error, "failed to record rejected order config update");
            }
            return Ok(exec_config_error_response(&error));
        }
    };
    saved.target_count = previous.target_count;
    saved.nonzero_target_count = previous.nonzero_target_count;
    if let Err(audit_error) = postgres::complete_exec_order_config_audit(
        &state.pool,
        audit_id,
        "applied",
        saved.updated_at_us,
        None,
    )
    .await
    {
        error!(audit_id, error = ?audit_error, "order config changed but audit completion failed");
    }
    if let Some(updated_at_us) = saved.updated_at_us {
        state
            .reload_notify
            .notify(source, &saved.strategy_name, updated_at_us);
    } else {
        warn!(
            source_id,
            strategy_name = %saved.strategy_name,
            "order-parameter Redis write confirmed without updated_at_us; skip notify"
        );
    }
    info!(
        audit_id,
        source_id,
        strategy_name = request.strategy_name,
        client_addr,
        updated_at_us = saved.updated_at_us,
        "applied Exec order parameter update"
    );
    Ok((NO_STORE, Json(saved)).into_response())
}

async fn list_position_strategies(State(state): State<WebState>) -> Result<Response, ApiError> {
    let strategies = strategy_catalog::list_position_strategies(&state.pool).await?;
    Ok((NO_STORE, Json(strategies)).into_response())
}

async fn save_position_strategy(
    State(state): State<WebState>,
    Json(request): Json<SavePositionStrategyRequest>,
) -> Result<Response, ApiError> {
    match strategy_catalog::upsert_position_strategy(&state.pool, &request, unix_now_us()).await {
        Ok(saved) => {
            let factual_positions = load_factual_positions(&state, &saved.strategy_name).await;
            let published_accounts = load_published_accounts(&state, &saved.strategy_name).await;
            if let Err(error) = state.position_archive.append(
                saved.updated_at_us,
                &saved,
                factual_positions,
                published_accounts,
            ) {
                error!(
                    strategy_name = %saved.strategy_name,
                    updated_at_us = saved.updated_at_us,
                    error = %error,
                    "position strategy saved but RocksDB archive write failed"
                );
                return Err(error.into());
            }
            let publishes = publish_bound_accounts(&state, &saved.strategy_name).await;
            if let Some(failed) = publishes.iter().find(|item| item.error.is_some()) {
                let source_id = failed.source_id.as_str();
                let binding_name = failed.binding_name.as_str();
                let error = failed.error.as_deref().unwrap_or("publish failed");
                error!(
                    strategy_name = %saved.strategy_name,
                    source_id,
                    binding_name,
                    error,
                    "position strategy saved but bound-account publish failed"
                );
                return Ok((
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse {
                        error: format!(
                            "position strategy saved, but publish failed for {source_id}/{binding_name}: {error}"
                        ),
                    }),
                )
                    .into_response());
            }
            Ok((
                NO_STORE,
                Json(SavedPositionStrategyResponse {
                    strategy: saved,
                    publishes,
                }),
            )
                .into_response())
        }
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn delete_position_strategy(
    State(state): State<WebState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    match strategy_catalog::delete_position_strategy(&state.pool, &name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT.into_response()),
        Ok(false) => Ok(not_found("position strategy was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn list_order_strategies(State(state): State<WebState>) -> Result<Response, ApiError> {
    let strategies = strategy_catalog::list_order_strategies(&state.pool).await?;
    Ok((NO_STORE, Json(strategies)).into_response())
}

async fn save_order_strategy(
    State(state): State<WebState>,
    Json(request): Json<SaveOrderStrategyRequest>,
) -> Result<Response, ApiError> {
    match strategy_catalog::upsert_order_strategy(&state.pool, &request, unix_now_us()).await {
        Ok(saved) => Ok((NO_STORE, Json(saved)).into_response()),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn delete_order_strategy(
    State(state): State<WebState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    match strategy_catalog::delete_order_strategy(&state.pool, &name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT.into_response()),
        Ok(false) => Ok(not_found("order strategy was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

fn unix_now_ms() -> i64 {
    unix_now_us() / 1_000
}

fn live_equity_status(snapshot_ts_ms: i64, now_ms: i64) -> &'static str {
    if now_ms.saturating_sub(snapshot_ts_ms).max(0) > 45_000 {
        "stale"
    } else {
        "ok"
    }
}

async fn get_account_studio(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    match strategy_catalog::load_account_studio(&state.pool, &source_id).await {
        Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn save_account_estimated_fee_rate(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    Json(request): Json<SaveEstimatedFeeRateRequest>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    if let Err(error) = strategy_catalog::validate_estimated_fee_rate(request.estimated_fee_rate) {
        return Ok(bad_request(error));
    }
    match postgres::save_estimated_fee_rate(&state.pool, &source_id, request.estimated_fee_rate)
        .await
    {
        Ok(()) => {
            info!(
                source_id,
                estimated_fee_rate = request.estimated_fee_rate,
                "account estimated fee rate updated"
            );
            if let Err(error) = refresh_dashboard_cache(&state).await {
                error!(source_id, error = %error, "dashboard refresh after fee update failed");
                return Ok(catalog_error(error));
            }
            match strategy_catalog::load_account_studio(&state.pool, &source_id).await {
                Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
                Err(error) => Ok(catalog_error(error)),
            }
        }
        Err(error) => {
            error!(
                source_id,
                estimated_fee_rate = request.estimated_fee_rate,
                error = %error,
                "account estimated fee rate update failed"
            );
            Ok(catalog_error(error))
        }
    }
}

async fn save_account_fee_rates(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    Json(request): Json<SaveFeeRatesRequest>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    if let Err(error) =
        strategy_catalog::validate_account_fee_rates(request.maker_fee_rate, request.taker_fee_rate)
    {
        return Ok(bad_request(error));
    }
    let rates = FeeRates {
        maker: request.maker_fee_rate,
        taker: request.taker_fee_rate,
    };
    match postgres::save_fee_rates(&state.pool, &source_id, rates).await {
        Ok(()) => {
            info!(
                source_id,
                maker_fee_rate = rates.maker,
                taker_fee_rate = rates.taker,
                "account maker/taker fee rates updated"
            );
            if let Err(error) = refresh_dashboard_cache(&state).await {
                error!(source_id, error = %error, "dashboard refresh after fee update failed");
                return Ok(catalog_error(error));
            }
            match strategy_catalog::load_account_studio(&state.pool, &source_id).await {
                Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
                Err(error) => Ok(catalog_error(error)),
            }
        }
        Err(error) => {
            error!(
                source_id,
                maker_fee_rate = rates.maker,
                taker_fee_rate = rates.taker,
                error = %error,
                "account maker/taker fee rate update failed"
            );
            Ok(catalog_error(error))
        }
    }
}

async fn refresh_dashboard_cache(state: &WebState) -> Result<()> {
    let attempted_at_us = unix_now_us();
    let build = build_dashboard(
        &state.config,
        &state.pool,
        state.refresh_interval_secs,
        &state.live_equity,
    )
    .await?;
    let mut cache = state.cache.write().await;
    cache.last_attempt_at_us = attempted_at_us;
    cache.dashboard = build.dashboard;
    cache.nav_histories = build.nav_histories;
    cache.position_snapshots = build.position_snapshots;
    cache.last_refresh_error = None;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ContractLeverageQuery {
    symbol: Option<String>,
}

async fn get_account_symbol_contract_leverage(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    Query(query): Query<ContractLeverageQuery>,
) -> Result<Response, ApiError> {
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return Ok(response),
    };
    let symbol = query
        .symbol
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    if symbol.is_empty() {
        return Ok(bad_request("symbol is required".to_string()));
    }
    if let Err(error) = strategy_catalog::validate_contract_symbol(&symbol) {
        return Ok(bad_request(error));
    }
    match crate::exchange_leverage::get_symbol_contract_leverage(source, &symbol).await {
        Ok(mut result) => {
            match strategy_catalog::load_symbol_contract_leverage(&state.pool, &source_id, &symbol)
                .await
            {
                Ok(recorded) => result.recorded_contract_leverage = recorded,
                Err(error) => {
                    warn!(
                        source_id,
                        symbol = %symbol,
                        error = %error,
                        "exchange contract leverage queried, but local catalog read failed"
                    );
                }
            }
            info!(
                source_id,
                symbol = %result.symbol,
                contract_leverage = result.contract_leverage,
                recorded_contract_leverage = result.recorded_contract_leverage,
                endpoint = %result.endpoint,
                "account symbol contract leverage queried from exchange"
            );
            Ok((NO_STORE, Json(result)).into_response())
        }
        Err(error) => {
            error!(
                source_id,
                symbol = %symbol,
                error = %format!("{error:#}"),
                "account symbol contract leverage query failed"
            );
            Ok((
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse {
                    error: format!("{error:#}"),
                }),
            )
                .into_response())
        }
    }
}

async fn save_account_symbol_contract_leverage(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    Json(mut request): Json<SaveSymbolContractLeverageRequest>,
) -> Result<Response, ApiError> {
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return Ok(response),
    };
    request.symbol = request.symbol.trim().to_ascii_uppercase();
    if let Err(error) = strategy_catalog::validate_contract_symbol(&request.symbol) {
        return Ok(bad_request(error));
    }
    if let Err(error) = strategy_catalog::validate_contract_leverage(request.contract_leverage) {
        return Ok(bad_request(error));
    }
    match crate::exchange_leverage::set_symbol_contract_leverage(source, &request).await {
        Ok(result) => {
            if let Err(error) = strategy_catalog::save_symbol_contract_leverage(
                &state.pool,
                &source_id,
                &request,
                unix_now_us(),
            )
            .await
            {
                warn!(
                    source_id,
                    symbol = %request.symbol,
                    contract_leverage = request.contract_leverage,
                    error = %error,
                    "exchange contract leverage set, but local catalog write failed"
                );
            }
            info!(
                source_id,
                symbol = %result.symbol,
                contract_leverage = result.contract_leverage,
                endpoint = %result.endpoint,
                "account symbol contract leverage set on exchange"
            );
            Ok((NO_STORE, Json(result)).into_response())
        }
        Err(error) => {
            error!(
                source_id,
                symbol = %request.symbol,
                contract_leverage = request.contract_leverage,
                error = %format!("{error:#}"),
                "account symbol contract leverage set failed"
            );
            Ok((
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse {
                    error: format!("{error:#}"),
                }),
            )
                .into_response())
        }
    }
}

async fn save_account_binding(
    State(state): State<WebState>,
    Path(source_id): Path<String>,
    Json(request): Json<SaveBindingRequest>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    match strategy_catalog::save_binding(&state.pool, &source_id, &request, unix_now_us()).await {
        Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn save_account_binding_shares(
    State(state): State<WebState>,
    Path((source_id, binding_name)): Path<(String, String)>,
    Json(request): Json<SaveBindingSharesRequest>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    match strategy_catalog::save_binding_shares(
        &state.pool,
        &source_id,
        &binding_name,
        &request,
        unix_now_us(),
    )
    .await
    {
        Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn delete_account_binding(
    State(state): State<WebState>,
    Path((source_id, binding_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    match strategy_catalog::delete_binding(&state.pool, &source_id, &binding_name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT.into_response()),
        Ok(false) => Ok(not_found("binding was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn publish_account_binding(
    State(state): State<WebState>,
    Path((source_id, binding_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    match publish_binding(&state, &source_id, &binding_name).await {
        Ok(published) => Ok((NO_STORE, Json(published)).into_response()),
        Err(error) => Ok(publish_failure_response(error)),
    }
}

async fn publish_bound_accounts(
    state: &WebState,
    strategy_name: &str,
) -> Vec<BindingPublishResult> {
    let bindings =
        match strategy_catalog::list_bindings_for_position(&state.pool, strategy_name).await {
            Ok(bindings) => bindings,
            Err(error) => {
                warn!(
                    strategy_name,
                    error = %error,
                    "failed to list bound accounts for position publish"
                );
                return vec![BindingPublishResult {
                    source_id: String::new(),
                    binding_name: strategy_name.to_string(),
                    shares: 0.0,
                    published: None,
                    error: Some("failed to list bound accounts".to_string()),
                }];
            }
        };
    let mut publishes = Vec::with_capacity(bindings.len());
    for binding in bindings {
        match publish_binding(state, &binding.source_id, &binding.binding_name).await {
            Ok(published) => {
                info!(
                    strategy_name,
                    source_id = %binding.source_id,
                    binding_name = %binding.binding_name,
                    shares = binding.shares,
                    "published bound account after position update"
                );
                publishes.push(BindingPublishResult {
                    source_id: binding.source_id,
                    binding_name: binding.binding_name,
                    shares: binding.shares,
                    published: Some(published),
                    error: None,
                });
            }
            Err(error) => {
                warn!(
                    strategy_name,
                    source_id = %binding.source_id,
                    binding_name = %binding.binding_name,
                    error = %error.message,
                    "bound-account publish failed after position update"
                );
                publishes.push(BindingPublishResult {
                    source_id: binding.source_id,
                    binding_name: binding.binding_name,
                    shares: binding.shares,
                    published: None,
                    error: Some(error.message),
                });
            }
        }
    }
    publishes
}

struct PublishFailure {
    status: StatusCode,
    message: String,
}

async fn publish_binding(
    state: &WebState,
    source_id: &str,
    binding_name: &str,
) -> std::result::Result<OrderStrategyView, PublishFailure> {
    let source = match resolve_publish_source(&state.config, source_id) {
        Ok(source) => source,
        Err(error) => return Err(error),
    };
    let loaded = strategy_catalog::load_binding_parts(&state.pool, source_id, binding_name)
        .await
        .map_err(|error| PublishFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        })?;
    let Some((position, order, shares)) = loaded else {
        return Err(PublishFailure {
            status: StatusCode::NOT_FOUND,
            message: "binding was not found".to_string(),
        });
    };
    let targets = strategy_catalog::scale_targets(&position.targets, shares);
    let published = state
        .redis_runtime
        .publish_strategy(source, binding_name, &order.order_parameters, &targets)
        .await
        .map_err(|error| {
            let message = error.to_string();
            let status = if message.contains("reserved")
                || message.contains("removal already requested")
                || message.contains("must be")
                || message.contains("invalid")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            PublishFailure { status, message }
        })?;
    if let Some(updated_at_us) = published.updated_at_us {
        state
            .reload_notify
            .notify(source, &published.strategy_name, updated_at_us);
    } else {
        warn!(
            source_id,
            strategy_name = %published.strategy_name,
            "Redis write confirmed without updated_at_us; skip notify and keep 30s poll fallback"
        );
    }
    Ok(published)
}

fn resolve_publish_source<'a>(
    config: &'a AppConfig,
    source_id: &str,
) -> std::result::Result<&'a SourceConfig, PublishFailure> {
    let Some(source) = config.sources.iter().find(|source| source.id == source_id) else {
        return Err(PublishFailure {
            status: StatusCode::BAD_REQUEST,
            message: format!("unknown source_id: {source_id}"),
        });
    };
    if !source.enabled {
        return Err(PublishFailure {
            status: StatusCode::BAD_REQUEST,
            message: format!("source_id is disabled: {source_id}"),
        });
    }
    if source.exec_config_url.is_none() {
        return Err(PublishFailure {
            status: StatusCode::NOT_FOUND,
            message: "order configuration is unavailable for this source".to_string(),
        });
    }
    Ok(source)
}

fn publish_failure_response(error: PublishFailure) -> Response {
    (
        error.status,
        Json(ErrorResponse {
            error: error.message,
        }),
    )
        .into_response()
}

async fn load_published_accounts(
    state: &WebState,
    strategy_name: &str,
) -> Vec<crate::position_archive::ArchivedPublishedAccount> {
    let snapshots =
        match strategy_catalog::list_publish_snapshots_for_position(&state.pool, strategy_name)
            .await
        {
            Ok(snapshots) => snapshots,
            Err(error) => {
                warn!(
                    strategy_name,
                    error = %error,
                    "failed to list bound-account shares for position update archive"
                );
                return Vec::new();
            }
        };
    snapshots
        .into_iter()
        .map(|snapshot| {
            crate::position_archive::published_account(
                snapshot.source_id,
                snapshot.binding_name,
                snapshot.shares,
            )
        })
        .collect()
}

async fn load_factual_positions(
    state: &WebState,
    strategy_name: &str,
) -> Vec<SourceFactualPositions> {
    let source_ids =
        match strategy_catalog::list_binding_source_ids_for_position(&state.pool, strategy_name)
            .await
        {
            Ok(source_ids) => source_ids,
            Err(error) => {
                warn!(
                    strategy_name,
                    error = %error,
                    "failed to list bound sources for position update archive"
                );
                return Vec::new();
            }
        };
    let mut out = Vec::new();
    for source_id in source_ids {
        let Some(source) = state
            .config
            .sources
            .iter()
            .find(|source| source.id == source_id && source.enabled)
        else {
            continue;
        };
        let Some(viz_url) = source.exec_viz_origin() else {
            continue;
        };
        match state
            .viz_snapshot
            .load_strategy_positions(&source_id, viz_url, strategy_name)
            .await
        {
            Ok(positions) => out.push(positions),
            Err(error) => warn!(
                source_id,
                strategy_name,
                error = %error,
                "Exec Viz snapshot factual positions unavailable"
            ),
        }
    }
    out
}

fn catalog_error(error: anyhow::Error) -> Response {
    let message = error.to_string();
    let status = if message.contains("exceeds")
        || message.contains("unknown")
        || message.contains("invalid")
        || message.contains("must be")
        || message.contains("violates")
    {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(ErrorResponse { error: message })).into_response()
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}

fn resolve_order_config_source<'a>(
    config: &'a AppConfig,
    source_id: &str,
) -> std::result::Result<&'a SourceConfig, Response> {
    let Some(source) = config.sources.iter().find(|source| source.id == source_id) else {
        return Err(bad_request(format!("unknown source_id: {source_id}")));
    };
    if !source.enabled {
        return Err(bad_request(format!("source_id is disabled: {source_id}")));
    }
    if source.exec_config_url.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "order configuration is unavailable for this source".to_string(),
            }),
        )
            .into_response());
    }
    Ok(source)
}

fn exec_config_error_response(error: &ExecConfigError) -> Response {
    let status = match error.status() {
        Some(StatusCode::BAD_REQUEST) => StatusCode::BAD_REQUEST,
        Some(StatusCode::UNAUTHORIZED) => StatusCode::BAD_GATEWAY,
        Some(StatusCode::NOT_FOUND) => StatusCode::NOT_FOUND,
        Some(StatusCode::CONFLICT) => StatusCode::CONFLICT,
        Some(StatusCode::SERVICE_UNAVAILABLE) => StatusCode::BAD_GATEWAY,
        _ => StatusCode::BAD_GATEWAY,
    };
    let message = if status == StatusCode::BAD_GATEWAY {
        "Exec Config service is unavailable".to_string()
    } else {
        error.public_message().to_string()
    };
    (status, Json(ErrorResponse { error: message })).into_response()
}

fn parse_csv(value: Option<&str>, uppercase: bool) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if uppercase {
                value.to_ascii_uppercase()
            } else {
                value.to_string()
            }
        })
        .collect()
}

fn resolve_sources<'a>(
    config: &'a AppConfig,
    selected_source_ids: &[String],
) -> std::result::Result<Vec<&'a SourceConfig>, String> {
    let requested = selected_source_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if requested.len() != selected_source_ids.len() {
        return Err("sourceIds must not contain duplicates".to_string());
    }
    for source_id in &requested {
        let Some(source) = config.sources.iter().find(|source| source.id == *source_id) else {
            return Err(format!("sourceIds contains an unknown source: {source_id}"));
        };
        if !source.enabled {
            return Err(format!("sourceIds contains a disabled source: {source_id}"));
        }
    }
    let selected = config
        .sources
        .iter()
        .filter(|source| {
            source.enabled && (requested.is_empty() || requested.contains(source.id.as_str()))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("sourceIds selects no enabled sources".to_string());
    }
    Ok(selected)
}

fn milliseconds_to_microseconds(value: i64, field: &str) -> std::result::Result<i64, String> {
    if value < 0 {
        return Err(format!("{field} must not be negative"));
    }
    value
        .checked_mul(1_000)
        .ok_or_else(|| format!("{field} is too large"))
}

fn is_timeline_request_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("start timestamp")
        || message.starts_with("end timestamp")
        || message.starts_with("none of the requested symbols")
}

fn is_execution_cost_request_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("start timestamp")
        || message.starts_with("end timestamp")
        || message.starts_with("windowSecs")
        || message.starts_with("page")
        || message.starts_with("sourceIds")
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse { error: message }),
    )
        .into_response()
}

async fn refresh_loop(
    config: AppConfig,
    pool: PgPool,
    cache: Arc<RwLock<CacheState>>,
    live_equity: LiveEquityHub,
    refresh_interval_secs: u64,
) {
    let period = Duration::from_secs(refresh_interval_secs);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let attempted_at_us = unix_now_us();
        match build_dashboard(&config, &pool, refresh_interval_secs, &live_equity).await {
            Ok(build) => {
                info!(
                    source_count = build.dashboard.report.source_count,
                    fill_count = build.dashboard.report.aggregate.totals.fill_count,
                    duration_ms = build.dashboard.generation_duration_ms,
                    "refreshed CTA dashboard"
                );
                let mut state = cache.write().await;
                state.last_attempt_at_us = attempted_at_us;
                state.dashboard = build.dashboard;
                state.nav_histories = build.nav_histories;
                state.position_snapshots = build.position_snapshots;
                state.last_refresh_error = None;
            }
            Err(error) => {
                warn!(error = ?error, "failed to refresh CTA dashboard; retaining last good report");
                let mut state = cache.write().await;
                state.last_attempt_at_us = attempted_at_us;
                state.last_refresh_error = Some(error.to_string());
            }
        }
    }
}

async fn build_dashboard(
    config: &AppConfig,
    pool: &PgPool,
    refresh_interval_secs: u64,
    live_equity: &LiveEquityHub,
) -> Result<DashboardBuild> {
    let started = Instant::now();
    let now_ms = unix_now_ms();
    let fee_rates = postgres::load_fee_rates(pool).await?;
    let nav_config = config.clone().with_fee_rates(&fee_rates);
    let accounts = config
        .sources
        .iter()
        .map(|source| {
            let live = live_equity.get(&source.id);
            DashboardAccount {
                source_id: source.id.clone(),
                account: source.display_name().to_string(),
                venue: source.venue.clone(),
                enabled: source.enabled,
                gateway_prefix: source.gateway_prefix.clone(),
                configurable: source.exec_config_url.is_some(),
                live_equity_usdt: live.as_ref().map(|snapshot| snapshot.equity_usdt),
                live_equity_status: live
                    .as_ref()
                    .map(|snapshot| live_equity_status(snapshot.ts_ms, now_ms)),
            }
        })
        .collect();
    let mut snapshots = nav::SourcePositionSnapshots::new();
    for source in config.sources.iter().filter(|source| source.enabled) {
        if let Some(snapshot) = postgres::load_latest_position_snapshot(pool, &source.id).await? {
            snapshots.insert(source.id.clone(), snapshot);
        }
    }

    let (report, histories, snapshots) = tokio::task::spawn_blocking(move || {
        let histories = nav::load_nav_source_histories(&nav_config, &[])?;
        let report = nav::rebuild_nav_from_histories_with_snapshots(
            &nav_config,
            &[],
            &snapshots,
            &histories,
        )?;
        anyhow::Ok((report, histories, snapshots))
    })
    .await
    .context("CTA dashboard rebuild task failed")??;
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    Ok(DashboardBuild {
        dashboard: DashboardSnapshot {
            generated_at_us: unix_now_us(),
            generation_duration_ms: duration_ms,
            refresh_interval_secs,
            accounts,
            report,
        },
        nav_histories: Arc::new(histories),
        position_snapshots: Arc::new(snapshots),
    })
}

fn unix_now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(?error, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(?error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_timestamp_is_positive_microseconds() {
        assert!(unix_now_us() > 1_000_000_000_000_000);
    }

    #[test]
    fn parses_csv_query_values_without_losing_source_identity() {
        assert_eq!(
            parse_csv(Some("trade01, trade02"), false),
            vec!["trade01", "trade02"]
        );
        assert_eq!(
            parse_csv(Some("btcusdt, ETHUSDT"), true),
            vec!["BTCUSDT", "ETHUSDT"]
        );
    }

    #[test]
    fn validates_millisecond_timestamp_conversion() {
        assert_eq!(milliseconds_to_microseconds(123, "startMs"), Ok(123_000));
        assert!(milliseconds_to_microseconds(-1, "startMs").is_err());
        assert!(milliseconds_to_microseconds(i64::MAX, "endMs").is_err());
    }

    #[test]
    fn resolve_publish_source_requires_enabled_exec_config() {
        let mut config = crate::config::AppConfig {
            database: crate::config::DatabaseConfig {
                url_env: "CRYPTO_CTA_LOCAL_DATABASE_URL".into(),
                max_connections: 1,
            },
            ingestion: crate::config::IngestionConfig::default(),
            order_config: crate::config::OrderConfigSettings::default(),
            redis: crate::config::RedisSettings::default(),
            twap: crate::config::TwapConfig::default(),
            sources: vec![crate::config::SourceConfig {
                id: "binance_exec_trade01".into(),
                account: "trade01".into(),
                alias: None,
                venue: "binance-futures".into(),
                rocksdb_path: std::path::PathBuf::from("/tmp/missing"),
                enabled: true,
                start_ts_us: None,
                poll_interval_secs: None,
                estimated_fee_rate: None,
                maker_fee_rate: None,
                taker_fee_rate: None,
                gateway_prefix: Some("/exec_trade01".into()),
                exec_config_url: Some("http://127.0.0.1:18161/".into()),
                exec_viz_url: None,
                ipc_namespace: None,
                account_ipc_service: None,
                legacy_share_unit_usdt: None,
                env_path: None,
            }],
        };
        assert!(resolve_publish_source(&config, "binance_exec_trade01").is_ok());
        config.sources[0].enabled = false;
        let disabled = resolve_publish_source(&config, "binance_exec_trade01").unwrap_err();
        assert_eq!(disabled.status, StatusCode::BAD_REQUEST);
        config.sources[0].enabled = true;
        config.sources[0].exec_config_url = None;
        let missing = resolve_publish_source(&config, "binance_exec_trade01").unwrap_err();
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let unknown = resolve_publish_source(&config, "missing").unwrap_err();
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn manager_publish_client_download_is_the_checked_in_script() {
        let script = std::str::from_utf8(MANAGER_PUBLISH_CLIENT).unwrap();
        assert!(script.contains("put-position"));
        assert!(script.contains("automatically republishes every bound"));
        assert!(script.contains("Manager writes Redis on a reconnecting long connection"));
        assert!(script.contains(r#"{"strategy_name":"CTA_A","targets":{"BTCUSDT":-0.006}}"#));
        assert!(script.contains("catalog/accounts/"));
        assert!(script.contains("/bindings/"));
        assert!(script.contains("/publish"));
        assert!(script.contains(r#""el01": "http://172.16.30.42:10041/manager/api/""#));
        assert!(script.contains(r#""jp-meta": "http://13.115.227.29:4191/manager/api/""#));
        assert!(script.contains("--target"));
        assert!(!script.contains("/exec_trade01/config/api/strategy"));
        assert_eq!(
            MANAGER_PUBLISH_CLIENT,
            include_bytes!("../scripts/manager_publish_client.py")
        );
    }
}
