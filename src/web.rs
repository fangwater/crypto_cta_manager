use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow_ipc::{
    CompressionType,
    writer::{IpcWriteOptions, StreamWriter},
};
use arrow_schema::{DataType, Field, Schema};
use axum::extract::{ConnectInfo, Extension, Path, Query, State};
use axum::http::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use crate::account_ipc::{LiveEquityHub, LiveEquitySnapshot};
use crate::auth::{self, AuthUser};
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
const MANAGER_PNL_SDK: &[u8] = include_bytes!("../scripts/manager_pnl_sdk.py");
const DEFAULT_POSITION_UPDATE_PAGE_SIZE: usize = 100;
const MAX_POSITION_UPDATE_PAGE_SIZE: usize = 1_000;

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
    /// Account-level PnL starts from the immutable account snapshot when present.
    pub account_pnl_start_ts_us: Option<i64>,
    /// Strategy PnL starts from a later immutable allocation anchor when present.
    pub strategy_pnl_start_ts_us: Option<i64>,
    pub live_equity_usdt: Option<f64>,
    pub live_equity_status: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineSnapshot {
    pub generated_at_us: i64,
    pub generation_duration_ms: u64,
    pub report: nav::NavTimelineReport,
    pub theoretical: crate::theoretical_nav::TheoreticalNavTimeline,
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

#[derive(Clone, Debug, Serialize)]
struct ExchangeNavResponse {
    generated_at_us: i64,
    accounts: Vec<ExchangeNavAccount>,
}

#[derive(Clone, Debug, Serialize)]
struct ExchangeNavAccount {
    source_id: String,
    account: String,
    venue: String,
    feed: Option<String>,
    equity_usdt: Option<f64>,
    wallet_balance_usdt: Option<f64>,
    unrealized_pnl_usdt: Option<f64>,
    available_balance_usdt: Option<f64>,
    exchange_ts_ms: Option<i64>,
    status: &'static str,
}

#[derive(Debug)]
struct CacheState {
    dashboard: DashboardSnapshot,
    nav_histories: Arc<nav::NavSourceHistories>,
    position_snapshots: Arc<nav::SourcePositionSnapshots>,
    strategy_position_snapshots: Arc<nav::SourceStrategyPositionSnapshots>,
    last_attempt_at_us: i64,
    last_refresh_error: Option<String>,
}

struct DashboardBuild {
    dashboard: DashboardSnapshot,
    nav_histories: Arc<nav::NavSourceHistories>,
    position_snapshots: Arc<nav::SourcePositionSnapshots>,
    strategy_position_snapshots: Arc<nav::SourceStrategyPositionSnapshots>,
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
    twap_symbols: crate::twap::SharedSymbols,
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
struct SymbolsQuery {
    source_ids: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StrategyPnlQuery {
    source_id: String,
    strategy_name: String,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
struct StrategyPnlSummary {
    generated_at_us: i64,
    generation_duration_ms: u64,
    source_id: String,
    account: String,
    strategy_name: String,
    start_ts_us: i64,
    end_ts_us: i64,
    totals: nav::NavTotals,
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcquisitionCostQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    source_ids: Option<String>,
    strategy_name: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PositionUpdatesQuery {
    after_us: Option<i64>,
    after_seq: Option<u32>,
    limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
struct ExecutionCostSnapshot {
    generated_at_us: i64,
    generation_duration_ms: u64,
    report: crate::execution_cost::ExecutionCostReport,
}

#[derive(Clone, Debug, Serialize)]
struct AcquisitionCostSnapshot {
    generated_at_us: i64,
    generation_duration_ms: u64,
    report: crate::acquisition_cost::AcquisitionCostReport,
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
struct AuthStatusResponse {
    authenticated: bool,
    setup_required: bool,
    user: Option<auth::UserView>,
}

#[derive(Debug, Serialize)]
struct AuthSessionResponse {
    user: auth::UserView,
}

#[derive(Clone, Debug)]
struct VisibleSources(BTreeSet<String>);

const POSITION_STRATEGY_PUBLISH_PATH: &str = "/api/catalog/position-strategies";
const PUBLISH_TOKEN_HEADER: &str = "x-cta-publish-token";
const MAX_PUBLISH_BODY_BYTES: usize = 1 << 20;

#[derive(Clone)]
struct AuthMiddlewareState {
    pool: PgPool,
    configured_source_ids: BTreeSet<String>,
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
    crate::market_rules::spawn(config.sources.clone(), redis_runtime.clone());
    let reload_notify = ReloadNotifyHub::spawn();
    let live_equity = LiveEquityHub::spawn(&config.sources);
    let viz_snapshot = VizSnapshotClient::new(config.order_config.request_timeout_secs)?;
    let manager_db = ManagerDb::open(&config.twap.rocksdb_path)?;
    let position_archive = Arc::new(PositionArchive::open(manager_db.clone())?);
    let twap = Arc::new(TwapStore::from_db(
        manager_db.clone(),
        config.twap.retain_days.max(1),
    )?);
    let twap_symbols = crate::twap::spawn_with_db(pool.clone(), config.twap.clone(), manager_db);
    crate::theoretical_nav::spawn(
        config.clone(),
        pool.clone(),
        Arc::clone(&position_archive),
        Arc::clone(&twap),
    );
    let first_build = build_dashboard(&config, &pool, refresh_interval_secs, &live_equity).await?;
    let cache = Arc::new(RwLock::new(CacheState {
        last_attempt_at_us: first_build.dashboard.generated_at_us,
        dashboard: first_build.dashboard,
        nav_histories: first_build.nav_histories,
        position_snapshots: first_build.position_snapshots,
        strategy_position_snapshots: first_build.strategy_position_snapshots,
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

    let configured_source_ids = config
        .sources
        .iter()
        .map(|source| source.id.clone())
        .collect::<BTreeSet<_>>();
    let app = Router::new()
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/register", post(auth_register))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/auth/verify", get(auth_verify))
        .route("/api/auth/users", get(auth_users).post(auth_create_user))
        .route("/api/auth/users/{user_id}/sources", put(auth_user_sources))
        .route("/api/auth/users/{user_id}/role", put(auth_user_role))
        .route("/api/health", get(health))
        .route("/api/nav/exchange", get(exchange_nav))
        .route(
            "/api/manager_publish_client.py",
            get(manager_publish_client),
        )
        .route("/api/manager_sdk.py", get(manager_sdk))
        .route("/api/manager_pnl_sdk.py", get(manager_sdk))
        .route("/api/dashboard", get(dashboard))
        .route("/api/symbols", get(list_symbols))
        .route("/api/timeline", get(timeline))
        .route("/api/account-timeline", get(account_timeline))
        .route("/api/pnl/account", get(account_pnl_arrow))
        .route("/api/pnl/strategies", get(strategies_pnl_arrow))
        .route("/api/pnl/strategy", get(strategy_pnl))
        .route("/api/pnl/strategy/summary", get(strategy_pnl_summary))
        .route("/api/catalog/execution-cost", get(execution_cost))
        .route("/api/catalog/acquisition-cost", get(acquisition_cost))
        .route("/api/catalog/position-updates", get(position_updates))
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
            "/api/catalog/position-strategies-access",
            get(list_position_access),
        )
        .route(
            "/api/catalog/position-strategies/{name}/publish-token",
            put(save_position_publish_token),
        )
        .route(
            "/api/catalog/position-strategies/{name}/publish-token/reset",
            post(reset_position_publish_token),
        )
        .route(
            "/api/catalog/position-strategies/{name}/managers",
            put(save_position_managers),
        )
        .route(
            "/api/catalog/position-strategies/{name}/viewers",
            put(save_position_viewers),
        )
        .route(
            "/api/catalog/publish-tokens",
            get(list_fallback_tokens).post(add_fallback_token),
        )
        .route(
            "/api/catalog/publish-tokens/{token_id}",
            delete(delete_fallback_token),
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
            "/api/catalog/accounts/{source_id}/exchange-fees",
            get(get_account_exchange_fee_rates),
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
            pool: pool.clone(),
            exec_config,
            redis_runtime,
            reload_notify,
            live_equity,
            position_archive,
            twap,
            twap_symbols,
            viz_snapshot,
            refresh_interval_secs,
        })
        .layer(middleware::from_fn_with_state(
            AuthMiddlewareState {
                pool: pool.clone(),
                configured_source_ids,
            },
            auth_middleware,
        ))
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

async fn auth_middleware(
    State(auth_state): State<AuthMiddlewareState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if matches!(
        path,
        "/api/auth/status"
            | "/api/auth/register"
            | "/api/auth/login"
            | "/api/auth/logout"
            | "/api/symbols"
    ) {
        return next.run(request).await;
    }
    if path == POSITION_STRATEGY_PUBLISH_PATH && request.method() == axum::http::Method::POST {
        return gate_position_publish(&auth_state, peer_addr, request, next).await;
    }
    let token = auth::extract_session_cookie(
        request
            .headers()
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok()),
    );
    let user = match token {
        Some(token) => match auth::load_session(&auth_state.pool, &token).await {
            Ok(Some(user)) => user,
            Ok(None) => return unauthorized("login required"),
            Err(error) => {
                error!(error = ?error, "failed to load authentication session");
                return internal_error();
            }
        },
        None => return unauthorized("login required"),
    };
    let source_id = source_id_from_path(path);
    let read_only = matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    );
    // A source grant is both visibility and account-configuration authority.
    // Global catalog/access mutations remain admin-only, except position target
    // publishes handled above by their creator/manager/token-specific gate.
    let allowed =
        match auth::allowed_source_ids(&auth_state.pool, &user, &auth_state.configured_source_ids)
            .await
        {
            Ok(allowed) => allowed,
            Err(error) => {
                error!(error = ?error, "failed to resolve source permissions");
                return internal_error();
            }
        };
    if let Some(message) = request_permission_error(user.is_admin(), read_only, source_id, &allowed)
    {
        return forbidden(message);
    }
    request.extensions_mut().insert(user);
    request.extensions_mut().insert(VisibleSources(allowed));
    next.run(request).await
}

fn request_permission_error(
    is_admin: bool,
    read_only: bool,
    source_id: Option<&str>,
    allowed_source_ids: &BTreeSet<String>,
) -> Option<&'static str> {
    if is_admin {
        return None;
    }
    if !read_only && source_id.is_none() {
        return Some("administrator permission required");
    }
    if source_id.is_some_and(|source_id| !allowed_source_ids.contains(source_id)) {
        return Some("you are not authorized to view or configure this account");
    }
    None
}

/// Position publishes arrive from machine publishers without a session, so the
/// endpoint keeps its own gate. Any one of these suffices: an admin session, a
/// session of the strategy's creator or an authorized manager, the strategy's
/// own publish token, or a fallback publish token stored in
/// cta_publish_fallback_tokens (accepted for every strategy). A strategy that
/// has no token configured keeps the legacy open push, so deployments stay
/// compatible until tokens are assigned. The body is buffered once so the gate
/// can read strategy_name before the handler parses it again.
async fn gate_position_publish(
    auth_state: &AuthMiddlewareState,
    peer_addr: SocketAddr,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_PUBLISH_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return bad_request("position publish body is too large".to_string()),
    };
    let strategy_name = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("strategy_name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        });

    let mut session_user = None;
    if let Some(token) = auth::extract_session_cookie(
        parts
            .headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok()),
    ) {
        match auth::load_session(&auth_state.pool, &token).await {
            Ok(user) => session_user = user,
            Err(error) => {
                error!(error = ?error, "failed to load authentication session");
                return internal_error();
            }
        }
    }

    let access = match &strategy_name {
        Some(name) => match strategy_catalog::load_position_access(&auth_state.pool, name).await {
            Ok(access) => access,
            Err(error) => {
                error!(error = ?error, "failed to load position strategy access");
                return internal_error();
            }
        },
        None => None,
    };

    let mut allowed = session_user.as_ref().is_some_and(AuthUser::is_admin);
    if !allowed && let (Some(user), Some(access)) = (&session_user, &access) {
        allowed = access.user_can_publish(user.user_id);
    }

    if !allowed {
        let required_hash = access
            .and_then(|access| access.publish_token_hash)
            .filter(|hash| !hash.is_empty());
        allowed = match required_hash {
            None => true,
            Some(hash) => {
                let provided = parts
                    .headers
                    .get(PUBLISH_TOKEN_HEADER)
                    .and_then(|value| value.to_str().ok());
                if auth::publish_token_matches(provided, &hash) {
                    true
                } else {
                    match auth::publish_fallback_token_matches(&auth_state.pool, provided).await {
                        Ok(found) => found,
                        Err(error) => {
                            error!(error = ?error, "failed to check fallback publish token");
                            return internal_error();
                        }
                    }
                }
            }
        };
    }

    if !allowed {
        warn!(
            peer = %peer_addr,
            strategy = strategy_name.as_deref().unwrap_or_default(),
            "rejected position strategy publish"
        );
        return forbidden(
            "position publish requires an admin or authorized session, or the strategy publish token",
        );
    }

    let mut request = Request::from_parts(parts, axum::body::Body::from(bytes));
    if let Some(user) = session_user {
        request.extensions_mut().insert(user);
    }
    next.run(request).await
}

fn source_id_from_path(path: &str) -> Option<&str> {
    if path == "/api/order-config/auth" {
        return None;
    }
    let prefixes = [
        "/api/catalog/accounts/",
        "/api/order-config/",
        "/api/pnl/strategy",
    ];
    let prefix = prefixes.iter().find(|prefix| path.starts_with(**prefix))?;
    let rest = path.strip_prefix(prefix)?.trim_matches('/');
    if *prefix == "/api/order-config/" {
        return rest.split('/').next().filter(|value| !value.is_empty());
    }
    if *prefix == "/api/pnl/strategy" {
        return None;
    }
    rest.split('/').next().filter(|value| !value.is_empty())
}

async fn auth_status(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let user = load_request_user(&state.pool, &headers).await?;
    let setup_required = !auth::has_users(&state.pool).await?;
    let response = AuthStatusResponse {
        authenticated: user.is_some(),
        setup_required,
        user: match user {
            Some(user) => Some(auth::user_view(&state.pool, &user).await?),
            None => None,
        },
    };
    Ok((NO_STORE, Json(response)).into_response())
}

/// Self-registration only bootstraps the very first administrator. Once any
/// user exists, accounts are created by admins through POST /api/auth/users.
async fn auth_register(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<auth::RegisterRequest>,
) -> Result<Response, ApiError> {
    if auth::has_users(&state.pool).await? {
        let user = load_request_user(&state.pool, &headers).await?;
        if !user.as_ref().is_some_and(AuthUser::is_admin) {
            return Ok(forbidden(
                "self-registration is closed; ask an administrator to create your account",
            ));
        }
    }
    let user = match auth::register(&state.pool, request).await {
        Ok(user) => user,
        Err(error) => return Ok(bad_request(error.to_string())),
    };
    let token = auth::create_session(&state.pool, user.user_id).await?;
    let view = auth::user_view(&state.pool, &user).await?;
    Ok((
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&auth::session_cookie(&token))?,
        )],
        NO_STORE,
        Json(AuthSessionResponse { user: view }),
    )
        .into_response())
}

async fn auth_login(
    State(state): State<WebState>,
    Json(request): Json<auth::LoginRequest>,
) -> Result<Response, ApiError> {
    let user = match auth::login(&state.pool, request).await {
        Ok(user) => user,
        Err(_) => return Ok(unauthorized("invalid username or password")),
    };
    let token = auth::create_session(&state.pool, user.user_id).await?;
    let view = auth::user_view(&state.pool, &user).await?;
    Ok((
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&auth::session_cookie(&token))?,
        )],
        NO_STORE,
        Json(AuthSessionResponse { user: view }),
    )
        .into_response())
}

async fn auth_logout(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = auth::extract_session_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok()),
    ) {
        auth::delete_session(&state.pool, &token).await?;
    }
    Ok((
        [(
            header::SET_COOKIE,
            HeaderValue::from_static(auth::clear_session_cookie()),
        )],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

/// Used by the gateway's auth_request for direct Exec Viz/Config links.
/// The source ID is injected by the per-source Nginx location and is never
/// trusted from the browser.
async fn auth_verify(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Some(user) = load_request_user(&state.pool, &headers).await? else {
        return Ok(unauthorized("login required"));
    };
    if !user.is_admin() {
        let source_id = headers
            .get("x-cta-source-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let configured = state
            .config
            .sources
            .iter()
            .map(|source| source.id.clone())
            .collect();
        let allowed = auth::allowed_source_ids(&state.pool, &user, &configured).await?;
        if !allowed.contains(source_id) {
            return Ok(forbidden("you are not authorized to view this account"));
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn auth_users(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Response, ApiError> {
    if !user.is_admin() {
        return Ok(forbidden("administrator permission required"));
    }
    Ok((NO_STORE, Json(auth::list_users(&state.pool).await?)).into_response())
}

async fn auth_create_user(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Json(request): Json<auth::CreateUserRequest>,
) -> Result<Response, ApiError> {
    if !user.is_admin() {
        return Ok(forbidden("administrator permission required"));
    }
    match auth::create_user(&state.pool, request).await {
        Ok(view) => Ok((NO_STORE, Json(view)).into_response()),
        Err(error) => Ok(bad_request(error.to_string())),
    }
}

async fn auth_user_sources(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path(user_id): Path<i64>,
    Json(request): Json<auth::SetSourcesRequest>,
) -> Result<Response, ApiError> {
    if !user.is_admin() {
        return Ok(forbidden("administrator permission required"));
    }
    let configured = state
        .config
        .sources
        .iter()
        .map(|source| source.id.clone())
        .collect();
    match auth::set_sources(&state.pool, user_id, &request.source_ids, &configured).await {
        Ok(view) => Ok((NO_STORE, Json(view)).into_response()),
        Err(error) => Ok(bad_request(error.to_string())),
    }
}

async fn auth_user_role(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path(user_id): Path<i64>,
    Json(request): Json<auth::SetRoleRequest>,
) -> Result<Response, ApiError> {
    if !user.is_admin() {
        return Ok(forbidden("administrator permission required"));
    }
    match auth::set_role(&state.pool, user_id, &request.role).await {
        Ok(view) => Ok((NO_STORE, Json(view)).into_response()),
        Err(error) => Ok(bad_request(error.to_string())),
    }
}

async fn load_request_user(
    pool: &PgPool,
    headers: &HeaderMap,
) -> Result<Option<AuthUser>, ApiError> {
    let Some(token) = auth::extract_session_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok()),
    ) else {
        return Ok(None);
    };
    Ok(auth::load_session(pool, &token).await?)
}

async fn dashboard(
    State(state): State<WebState>,
    Extension(visible): Extension<VisibleSources>,
) -> impl IntoResponse {
    let mut dashboard = state.cache.read().await.dashboard.clone();
    dashboard
        .accounts
        .retain(|account| visible.0.contains(&account.source_id));
    dashboard.report = nav::restrict_report(&dashboard.report, &visible.0);
    let now_ms = unix_now_ms();
    for account in &mut dashboard.accounts {
        if let Some(snapshot) = state.live_equity.get(&account.source_id) {
            account.live_equity_usdt = Some(snapshot.equity_usdt);
            account.live_equity_status = Some(live_equity_status(snapshot.ts_ms, now_ms));
        }
    }
    (NO_STORE, Json(dashboard))
}

#[derive(Clone, Debug, Serialize)]
struct SymbolsResponse {
    generated_at_us: i64,
    sources: Vec<SourceSymbolsView>,
}

#[derive(Clone, Debug, Serialize)]
struct SourceSymbolsView {
    source_id: String,
    symbols: Vec<SymbolView>,
}

#[derive(Clone, Debug, Serialize)]
struct SymbolView {
    symbol: String,
    venue_code: i16,
    venue: Option<String>,
    first_event_ts_us: Option<i64>,
    first_fill_ts_us: Option<i64>,
    last_fill_ts_us: Option<i64>,
}

async fn list_symbols(
    State(state): State<WebState>,
    Query(query): Query<SymbolsQuery>,
) -> Response {
    let selected_source_ids = parse_csv(query.source_ids.as_deref(), false);
    let sources = match resolve_sources(&state.config, &selected_source_ids) {
        Ok(sources) => sources,
        Err(message) => return bad_request(message),
    };
    let source_ids = sources
        .iter()
        .map(|source| source.id.clone())
        .collect::<Vec<_>>();
    let rows = match postgres::load_source_symbols(&state.pool, &source_ids).await {
        Ok(rows) => rows,
        Err(error) => {
            error!(error = %error, "failed to load source symbols");
            return internal_error();
        }
    };
    let (snapshots, strategy_snapshots) = {
        let cache = state.cache.read().await;
        (
            Arc::clone(&cache.position_snapshots),
            Arc::clone(&cache.strategy_position_snapshots),
        )
    };
    (
        NO_STORE,
        Json(SymbolsResponse {
            generated_at_us: unix_now_us(),
            sources: merge_symbol_rows(&sources, rows, &snapshots, &strategy_snapshots),
        }),
    )
        .into_response()
}

fn merge_symbol_rows(
    sources: &[&SourceConfig],
    rows: Vec<postgres::SourceSymbol>,
    snapshots: &nav::SourcePositionSnapshots,
    strategy_snapshots: &nav::SourceStrategyPositionSnapshots,
) -> Vec<SourceSymbolsView> {
    let mut by_source = BTreeMap::<String, BTreeMap<(String, i16), SymbolView>>::new();
    for row in rows {
        by_source
            .entry(row.source_id)
            .or_default()
            .entry((row.symbol.clone(), row.venue_code))
            .or_insert(SymbolView {
                symbol: row.symbol,
                venue_code: row.venue_code,
                venue: Some(row.venue),
                first_event_ts_us: row.first_event_ts_us,
                first_fill_ts_us: row.first_fill_ts_us,
                last_fill_ts_us: row.last_fill_ts_us,
            });
    }
    for source in sources {
        let entries = by_source.entry(source.id.clone()).or_default();
        let anchor_positions = snapshots
            .get(&source.id)
            .map(|snapshot| snapshot.positions.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|position| (position.symbol.as_str(), position.venue_code))
            .chain(
                strategy_snapshots
                    .get(&source.id)
                    .map(|snapshot| snapshot.positions.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|position| (position.symbol.as_str(), position.venue_code)),
            );
        for (symbol, venue_code) in anchor_positions {
            entries
                .entry((symbol.to_string(), venue_code))
                .or_insert_with(|| SymbolView {
                    symbol: symbol.to_string(),
                    venue_code,
                    venue: None,
                    first_event_ts_us: None,
                    first_fill_ts_us: None,
                    last_fill_ts_us: None,
                });
        }
    }
    sources
        .iter()
        .map(|source| SourceSymbolsView {
            source_id: source.id.clone(),
            symbols: by_source
                .remove(&source.id)
                .unwrap_or_default()
                .into_values()
                .collect(),
        })
        .collect()
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

async fn manager_sdk() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/x-python; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"manager_sdk.py\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        MANAGER_PNL_SDK,
    )
}

async fn health(
    State(state): State<WebState>,
    Extension(visible): Extension<VisibleSources>,
) -> impl IntoResponse {
    let cache = state.cache.read().await;
    let response = HealthResponse {
        status: if cache.last_refresh_error.is_some() {
            "degraded"
        } else {
            "ok"
        },
        source_count: cache
            .dashboard
            .report
            .sources
            .iter()
            .filter(|source| visible.0.contains(&source.source_id))
            .count(),
        generated_at_us: cache.dashboard.generated_at_us,
        last_attempt_at_us: cache.last_attempt_at_us,
        refresh_interval_secs: cache.dashboard.refresh_interval_secs,
        last_refresh_error: cache.last_refresh_error.clone(),
    };
    (NO_STORE, Json(response))
}

async fn exchange_nav(
    State(state): State<WebState>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    let selected = match resolve_visible_sources(&state.config, &[], &visible.0) {
        Ok(sources) => sources,
        Err(message) => return Ok(bad_request(message)),
    };
    let now_ms = unix_now_ms();
    let accounts = selected
        .into_iter()
        .map(|source| exchange_nav_account(source, state.live_equity.get(&source.id), now_ms))
        .collect();
    Ok((
        NO_STORE,
        Json(ExchangeNavResponse {
            generated_at_us: unix_now_us(),
            accounts,
        }),
    )
        .into_response())
}

fn exchange_nav_account(
    source: &SourceConfig,
    snapshot: Option<LiveEquitySnapshot>,
    now_ms: i64,
) -> ExchangeNavAccount {
    let status = snapshot
        .as_ref()
        .map(|snapshot| live_equity_status(snapshot.ts_ms, now_ms))
        .unwrap_or("unavailable");
    ExchangeNavAccount {
        source_id: source.id.clone(),
        account: source.display_name().to_string(),
        venue: source.venue.clone(),
        feed: snapshot.as_ref().map(|snapshot| snapshot.source.clone()),
        equity_usdt: snapshot.as_ref().map(|snapshot| snapshot.equity_usdt),
        wallet_balance_usdt: snapshot
            .as_ref()
            .map(|snapshot| snapshot.wallet_balance_usdt),
        unrealized_pnl_usdt: snapshot
            .as_ref()
            .map(|snapshot| snapshot.unrealized_pnl_usdt),
        available_balance_usdt: snapshot
            .as_ref()
            .map(|snapshot| snapshot.available_balance_usdt),
        exchange_ts_ms: snapshot.as_ref().map(|snapshot| snapshot.ts_ms),
        status,
    }
}

async fn timeline(
    State(state): State<WebState>,
    Query(query): Query<TimelineQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    let snapshot = match rebuild_timeline_snapshot(state, query, true, visible.0).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok((NO_STORE, Json(snapshot)).into_response())
}

async fn account_timeline(
    State(state): State<WebState>,
    Query(query): Query<TimelineQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    let snapshot = match rebuild_timeline_snapshot(state, query, false, visible.0).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok((NO_STORE, Json(snapshot)).into_response())
}

async fn account_pnl_arrow(
    State(state): State<WebState>,
    Query(query): Query<TimelineQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    timeline_arrow_response(state, query, false, visible.0).await
}

async fn strategies_pnl_arrow(
    State(state): State<WebState>,
    Query(query): Query<TimelineQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    timeline_arrow_response(state, query, true, visible.0).await
}

async fn timeline_arrow_response(
    state: WebState,
    query: TimelineQuery,
    use_strategy_allocation: bool,
    visible_source_ids: BTreeSet<String>,
) -> Result<Response, ApiError> {
    let snapshot =
        match rebuild_timeline_snapshot(state, query, use_strategy_allocation, visible_source_ids)
            .await?
        {
            Ok(snapshot) => snapshot,
            Err(response) => return Ok(response),
        };
    let payload = if use_strategy_allocation {
        encode_strategy_timeline_arrow(&snapshot)?
    } else {
        encode_account_timeline_arrow(&snapshot)?
    };
    let mut response = payload.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apache.arrow.stream"),
    );
    Ok(response)
}

async fn rebuild_timeline_snapshot(
    state: WebState,
    query: TimelineQuery,
    use_strategy_allocation: bool,
    visible_source_ids: BTreeSet<String>,
) -> Result<std::result::Result<TimelineSnapshot, Response>, ApiError> {
    let requested_source_ids = parse_csv(query.source_ids.as_deref(), false);
    if requested_source_ids
        .iter()
        .any(|source_id| !visible_source_ids.contains(source_id))
    {
        return Ok(Err(forbidden(
            "you are not authorized to view one or more accounts",
        )));
    }
    let selected_source_ids = if requested_source_ids.is_empty() {
        visible_source_ids.into_iter().collect::<Vec<_>>()
    } else {
        requested_source_ids
    };
    if selected_source_ids.is_empty() {
        return Ok(Err(forbidden("you are not authorized to view any account")));
    }
    if let Err(message) = resolve_sources(&state.config, &selected_source_ids) {
        return Ok(Err(bad_request(message)));
    }
    let selected_symbols = parse_csv(query.symbols.as_deref(), true);
    let theoretical_symbols = selected_symbols.clone();
    let start_ts_us = match query.start_ms {
        Some(value) => match milliseconds_to_microseconds(value, "startMs") {
            Ok(value) => Some(value),
            Err(message) => return Ok(Err(bad_request(message))),
        },
        None => None,
    };
    let end_ts_us = match milliseconds_to_microseconds(
        query.end_ms.unwrap_or_else(|| unix_now_us() / 1_000),
        "endMs",
    ) {
        Ok(value) => value,
        Err(message) => return Ok(Err(bad_request(message))),
    };
    if start_ts_us.is_some_and(|start_ts_us| end_ts_us < start_ts_us) {
        return Ok(Err(bad_request(
            "endMs must be greater than or equal to startMs".to_string(),
        )));
    }

    let started = Instant::now();
    let (histories, snapshots, strategy_snapshots, data_generated_at_us) = {
        let cache = state.cache.read().await;
        (
            Arc::clone(&cache.nav_histories),
            Arc::clone(&cache.position_snapshots),
            Arc::clone(&cache.strategy_position_snapshots),
            cache.dashboard.generated_at_us,
        )
    };
    let snapshots = selected_snapshot_map(&snapshots, &selected_source_ids);
    let strategy_snapshots = selected_snapshot_map(&strategy_snapshots, &selected_source_ids);
    let fee_rates = postgres::load_fee_rates(&state.pool).await?;
    let max_points = query.max_points.unwrap_or(3_000).clamp(200, 10_000);
    let request = nav::NavTimelineRequest {
        start_ts_us,
        end_ts_us,
        selected_source_ids,
        selected_symbols,
        max_points,
    };
    let config = Arc::clone(&state.config)
        .as_ref()
        .clone()
        .with_fee_rates(&fee_rates);
    let report = tokio::task::spawn_blocking(move || {
        if use_strategy_allocation {
            nav::rebuild_nav_timeline_from_histories_with_strategy_snapshots(
                &config,
                request,
                &snapshots,
                &strategy_snapshots,
                &histories,
            )
        } else {
            nav::rebuild_nav_timeline_from_histories_with_snapshots(
                &config, request, &snapshots, &histories,
            )
        }
    })
    .await
    .context("CTA timeline rebuild task failed")?;
    let report = match report {
        Ok(report) => report,
        Err(error) if is_timeline_request_error(&error) => {
            return Ok(Err(bad_request(error.to_string())));
        }
        Err(error) => return Err(error.into()),
    };
    let theoretical = if theoretical_symbols.is_empty() {
        crate::theoretical_nav::load_timeline(
            &state.pool,
            report.start_ts_us,
            report.end_ts_us,
            &report.selected_source_ids,
            max_points,
        )
        .await?
    } else {
        crate::theoretical_nav::TheoreticalNavTimeline::default()
    };
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    Ok(Ok(TimelineSnapshot {
        generated_at_us: data_generated_at_us,
        generation_duration_ms,
        report,
        theoretical,
    }))
}

async fn strategy_pnl(
    State(state): State<WebState>,
    Query(query): Query<StrategyPnlQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    if !visible.0.contains(&query.source_id) {
        return Ok(forbidden("you are not authorized to view this account"));
    }
    let request = match parse_strategy_pnl_request(&state.config, query) {
        Ok(request) => request,
        Err(message) => return Ok(bad_request(message)),
    };
    let started = Instant::now();
    let (report, data_generated_at_us) = match rebuild_strategy_pnl_report(&state, request).await {
        Ok(result) => result,
        Err(error) if is_strategy_pnl_request_error(&error) => {
            return Ok(bad_request(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    let payload = encode_strategy_pnl_arrow(&report, data_generated_at_us)?;
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    info!(
        generation_duration_ms,
        response_bytes = payload.len(),
        "served raw strategy PnL Arrow stream"
    );

    let mut response = payload.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apache.arrow.stream"),
    );
    Ok(response)
}

async fn strategy_pnl_summary(
    State(state): State<WebState>,
    Query(query): Query<StrategyPnlQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    if !visible.0.contains(&query.source_id) {
        return Ok(forbidden("you are not authorized to view this account"));
    }
    let request = match parse_strategy_pnl_request(&state.config, query) {
        Ok(request) => request,
        Err(message) => return Ok(bad_request(message)),
    };
    let started = Instant::now();
    let (report, data_generated_at_us) = match rebuild_strategy_pnl_report(&state, request).await {
        Ok(result) => result,
        Err(error) if is_strategy_pnl_request_error(&error) => {
            return Ok(bad_request(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok((
        NO_STORE,
        Json(strategy_pnl_summary_from_report(
            report,
            data_generated_at_us,
            generation_duration_ms,
        )),
    )
        .into_response())
}

fn strategy_pnl_summary_from_report(
    report: nav::StrategyPnlReport,
    generated_at_us: i64,
    generation_duration_ms: u64,
) -> StrategyPnlSummary {
    let totals = report.rows.last().map(|row| row.totals).unwrap_or_default();
    StrategyPnlSummary {
        generated_at_us,
        generation_duration_ms,
        source_id: report.source_id,
        account: report.account,
        strategy_name: report.strategy_name,
        start_ts_us: report.start_ts_us,
        end_ts_us: report.end_ts_us,
        totals,
    }
}

fn parse_strategy_pnl_request(
    config: &AppConfig,
    query: StrategyPnlQuery,
) -> std::result::Result<nav::StrategyPnlRequest, String> {
    let source_id = query.source_id.trim().to_string();
    let strategy_name = query.strategy_name.trim().to_string();
    if source_id.is_empty() {
        return Err("sourceId must not be empty".to_string());
    }
    if strategy_name.is_empty() {
        return Err("strategyName must not be empty".to_string());
    }
    resolve_sources(config, &[source_id.clone()])?;
    let start_ts_us = match milliseconds_to_microseconds(query.start_ms, "startMs") {
        Ok(value) => value,
        Err(message) => return Err(message),
    };
    let end_ts_us = match milliseconds_to_microseconds(query.end_ms, "endMs") {
        Ok(value) => value,
        Err(message) => return Err(message),
    };
    if end_ts_us < start_ts_us {
        return Err("endMs must be greater than or equal to startMs".to_string());
    }
    Ok(nav::StrategyPnlRequest {
        source_id,
        strategy_name,
        start_ts_us,
        end_ts_us,
    })
}

async fn rebuild_strategy_pnl_report(
    state: &WebState,
    request: nav::StrategyPnlRequest,
) -> Result<(nav::StrategyPnlReport, i64)> {
    let (histories, snapshots, strategy_snapshots, data_generated_at_us) = {
        let cache = state.cache.read().await;
        (
            Arc::clone(&cache.nav_histories),
            Arc::clone(&cache.position_snapshots),
            Arc::clone(&cache.strategy_position_snapshots),
            cache.dashboard.generated_at_us,
        )
    };
    let requested_source_ids = vec![request.source_id.clone()];
    let snapshots = selected_snapshot_map(&snapshots, &requested_source_ids);
    let strategy_snapshots = selected_snapshot_map(&strategy_snapshots, &requested_source_ids);
    let config = Arc::clone(&state.config).as_ref().clone();
    let report = tokio::task::spawn_blocking(move || {
        nav::rebuild_strategy_pnl_from_histories_with_strategy_snapshots(
            &config,
            request,
            &snapshots,
            &strategy_snapshots,
            &histories,
        )
    })
    .await
    .context("CTA strategy PnL rebuild task failed")?;
    let report = report?;
    Ok((report, data_generated_at_us))
}

async fn execution_cost(
    State(state): State<WebState>,
    Query(query): Query<ExecutionCostQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    let requested_source_ids = parse_csv(query.source_ids.as_deref(), false);
    if requested_source_ids
        .iter()
        .any(|source_id| !visible.0.contains(source_id))
    {
        return Ok(forbidden(
            "you are not authorized to view one or more accounts",
        ));
    }
    let selected_source_ids = if requested_source_ids.is_empty() {
        visible.0.iter().cloned().collect::<Vec<_>>()
    } else {
        requested_source_ids
    };
    if selected_source_ids.is_empty() {
        return Ok(forbidden("you are not authorized to view any account"));
    }
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
    let fee_rates = postgres::load_fee_rates(&state.pool).await?;
    let config = Arc::clone(&state.config)
        .as_ref()
        .clone()
        .with_fee_rates(&fee_rates);
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

async fn acquisition_cost(
    State(state): State<WebState>,
    Query(query): Query<AcquisitionCostQuery>,
    Extension(visible): Extension<VisibleSources>,
) -> Result<Response, ApiError> {
    let requested_source_ids = parse_csv(query.source_ids.as_deref(), false);
    if requested_source_ids
        .iter()
        .any(|source_id| !visible.0.contains(source_id))
    {
        return Ok(forbidden(
            "you are not authorized to view one or more accounts",
        ));
    }
    let selected_source_ids = if requested_source_ids.is_empty() {
        visible.0.iter().cloned().collect::<Vec<_>>()
    } else {
        requested_source_ids
    };
    if selected_source_ids.is_empty() {
        return Ok(forbidden("you are not authorized to view any account"));
    }
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
    let (histories, generated_at_us) = {
        let cache = state.cache.read().await;
        (
            Arc::clone(&cache.nav_histories),
            cache.dashboard.generated_at_us,
        )
    };
    let end_received_at_us = match query.end_ms {
        Some(value) => match milliseconds_to_microseconds(value, "endMs") {
            Ok(value) => value,
            Err(message) => return Ok(bad_request(message)),
        },
        None => generated_at_us,
    };
    if end_received_at_us < start_received_at_us {
        return Ok(bad_request(
            "endMs must be greater than or equal to startMs".to_string(),
        ));
    }
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
        .unwrap_or(crate::acquisition_cost::DEFAULT_PAGE_SIZE);
    if page == 0 || page_size == 0 || page_size > crate::acquisition_cost::MAX_PAGE_SIZE {
        return Ok(bad_request(format!(
            "page must be positive and pageSize must be between 1 and {}",
            crate::acquisition_cost::MAX_PAGE_SIZE
        )));
    }
    let started = Instant::now();
    let fee_rates = postgres::load_fee_rates(&state.pool).await?;
    let config = Arc::clone(&state.config)
        .as_ref()
        .clone()
        .with_fee_rates(&fee_rates);
    let report = crate::acquisition_cost::report_acquisition_cost(
        &state.pool,
        &config,
        &state.position_archive,
        &histories,
        start_received_at_us,
        end_received_at_us,
        generated_at_us,
        &selected_source_ids,
        strategy_name,
        page,
        page_size,
    )
    .await?;
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok((
        NO_STORE,
        Json(AcquisitionCostSnapshot {
            generated_at_us,
            generation_duration_ms,
            report,
        }),
    )
        .into_response())
}

async fn position_updates(
    State(state): State<WebState>,
    Query(query): Query<PositionUpdatesQuery>,
    Extension(visible): Extension<VisibleSources>,
    Extension(user): Extension<AuthUser>,
) -> Result<Response, ApiError> {
    let after = match (query.after_us, query.after_seq) {
        (None, None) => None,
        (Some(after_us), Some(after_seq)) if after_us >= 0 => Some((after_us, after_seq)),
        (Some(_), Some(_)) => {
            return Ok(bad_request("afterUs must not be negative".to_string()));
        }
        _ => {
            return Ok(bad_request(
                "afterUs and afterSeq must be provided together".to_string(),
            ));
        }
    };
    let limit = query.limit.unwrap_or(DEFAULT_POSITION_UPDATE_PAGE_SIZE);
    if limit == 0 || limit > MAX_POSITION_UPDATE_PAGE_SIZE {
        return Ok(bad_request(format!(
            "limit must be between 1 and {MAX_POSITION_UPDATE_PAGE_SIZE}"
        )));
    }
    // Strategy visibility mirrors the catalog list: non-admin users only read
    // updates of strategies they are allowed to see.
    let allowed_strategies = if user.is_admin() {
        None
    } else {
        let visibility = strategy_catalog::list_position_visibility(&state.pool).await?;
        Some(
            visibility
                .iter()
                .filter(|entry| entry.user_can_view(user.user_id))
                .map(|entry| entry.strategy_name.clone())
                .collect::<std::collections::BTreeSet<String>>(),
        )
    };
    let archive = Arc::clone(&state.position_archive);
    let payload = tokio::task::spawn_blocking(move || {
        archive.raw_json_page_for_sources(
            after,
            limit,
            Some(&visible.0),
            allowed_strategies.as_ref(),
        )
    })
    .await
    .context("position update archive read task failed")??;
    let mut response = payload.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    Ok(response)
}

async fn order_config_auth() -> Response {
    (NO_STORE, Json(AuthResponse { ok: true })).into_response()
}

async fn order_config_strategies(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path(source_id): Path<String>,
) -> Result<Response, ApiError> {
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return Ok(response),
    };
    match state
        .exec_config
        .list_strategies(source.exec_config_url.as_deref().unwrap_or_default())
        .await
    {
        Ok(mut strategies) => {
            if !user.is_admin() {
                let visible = visible_account_binding_names(&state.pool, &user, &source_id).await?;
                strategies.retain(|strategy| visible.contains(strategy));
            }
            Ok((
                NO_STORE,
                Json(StrategyListResponse {
                    source_id,
                    strategies,
                }),
            )
                .into_response())
        }
        Err(error) => Ok(exec_config_error_response(&error)),
    }
}

async fn order_config_strategy(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path(source_id): Path<String>,
    Query(query): Query<StrategyQuery>,
) -> Result<Response, ApiError> {
    if let Err(message) = validate_strategy_name(&query.name) {
        return Ok(bad_request(message));
    }
    if !user.is_admin()
        && !user_can_configure_binding(&state.pool, &user, &source_id, &query.name).await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for this account binding",
        ));
    }
    let source = match resolve_order_config_source(&state.config, &source_id) {
        Ok(source) => source,
        Err(response) => return Ok(response),
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
        Ok(strategy) => Ok((NO_STORE, Json(strategy)).into_response()),
        Err(error) => Ok(exec_config_error_response(&error)),
    }
}

async fn save_order_parameters(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
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
    if !user.is_admin()
        && !user_can_configure_binding(&state.pool, &user, &source_id, &request.strategy_name)
            .await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for this account binding",
        ));
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

/// Admins see every strategy. Other users see a strategy while it keeps open
/// visibility, or when they are its creator, a viewer, or a publish manager.
/// New strategies are private by default.
async fn list_position_strategies(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Response, ApiError> {
    let mut strategies = strategy_catalog::list_position_strategies(&state.pool).await?;
    if !user.is_admin() {
        let visibility = strategy_catalog::list_position_visibility(&state.pool).await?;
        strategies.retain(|strategy| {
            visibility
                .iter()
                .find(|entry| entry.strategy_name == strategy.strategy_name)
                .is_none_or(|entry| entry.user_can_view(user.user_id))
        });
    }
    Ok((NO_STORE, Json(strategies)).into_response())
}

async fn save_position_strategy(
    State(state): State<WebState>,
    user: Option<Extension<AuthUser>>,
    Json(request): Json<SavePositionStrategyRequest>,
) -> Result<Response, ApiError> {
    let created_by = user.map(|Extension(user)| user.user_id);
    match strategy_catalog::upsert_position_strategy(
        &state.pool,
        &request,
        created_by,
        unix_now_us(),
    )
    .await
    {
        Ok(saved) => {
            state.twap_symbols.track(saved.targets.keys());
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

async fn list_position_access(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Response, ApiError> {
    if !user.is_admin() {
        return Ok(forbidden("administrator permission required"));
    }
    Ok((
        NO_STORE,
        Json(strategy_catalog::list_position_access(&state.pool).await?),
    )
        .into_response())
}

async fn save_position_publish_token(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Json(request): Json<strategy_catalog::SavePositionPublishTokenRequest>,
) -> Result<Response, ApiError> {
    let token = request.publish_token.trim();
    if !token.is_empty()
        && let Err(message) = strategy_catalog::validate_publish_token(token)
    {
        return Ok(bad_request(message));
    }
    // Empty clears the token and restores the legacy open push.
    let hash = if token.is_empty() {
        None
    } else {
        Some(auth::publish_token_hash(token))
    };
    match strategy_catalog::set_position_publish_token(&state.pool, &name, hash).await {
        Ok(true) => {}
        Ok(false) => return Ok(not_found("position strategy was not found")),
        Err(error) => return Ok(catalog_error(error)),
    }
    match strategy_catalog::position_access_view(&state.pool, &name).await {
        Ok(Some(view)) => Ok((NO_STORE, Json(view)).into_response()),
        Ok(None) => Ok(not_found("position strategy was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

/// Admin-only reset: generates a fresh random token, stores its hash, and
/// returns the plaintext once so it can be distributed to the publisher.
async fn reset_position_publish_token(
    State(state): State<WebState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let token = auth::generate_publish_token();
    let hash = auth::publish_token_hash(&token);
    match strategy_catalog::set_position_publish_token(&state.pool, &name, Some(hash)).await {
        Ok(true) => {}
        Ok(false) => return Ok(not_found("position strategy was not found")),
        Err(error) => return Ok(catalog_error(error)),
    }
    match strategy_catalog::position_access_view(&state.pool, &name).await {
        Ok(Some(access)) => Ok((
            NO_STORE,
            Json(serde_json::json!({
                "publish_token": token,
                "access": access,
            })),
        )
            .into_response()),
        Ok(None) => Ok(not_found("position strategy was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn save_position_managers(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Json(request): Json<strategy_catalog::SavePositionManagersRequest>,
) -> Result<Response, ApiError> {
    match strategy_catalog::set_position_managers(&state.pool, &name, &request.user_ids).await {
        Ok(true) => {}
        Ok(false) => return Ok(not_found("position strategy was not found")),
        Err(error) => return Ok(catalog_error(error)),
    }
    match strategy_catalog::position_access_view(&state.pool, &name).await {
        Ok(Some(view)) => Ok((NO_STORE, Json(view)).into_response()),
        Ok(None) => Ok(not_found("position strategy was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

/// Replaces the strategy's visibility in one call: `open_visibility` decides
/// whether every logged-in user can see it, `user_ids` grants individual
/// viewers while it stays private.
async fn save_position_viewers(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Json(request): Json<strategy_catalog::SavePositionVisibilityRequest>,
) -> Result<Response, ApiError> {
    match strategy_catalog::set_position_visibility(
        &state.pool,
        &name,
        &request.user_ids,
        request.open_visibility,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return Ok(not_found("position strategy was not found")),
        Err(error) => return Ok(catalog_error(error)),
    }
    match strategy_catalog::position_access_view(&state.pool, &name).await {
        Ok(Some(view)) => Ok((NO_STORE, Json(view)).into_response()),
        Ok(None) => Ok(not_found("position strategy was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

/// Fallback publish tokens live in PostgreSQL (hashed) and are accepted for
/// every strategy in addition to that strategy's own token.
async fn list_fallback_tokens(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Response, ApiError> {
    if !user.is_admin() {
        return Ok(forbidden("administrator permission required"));
    }
    Ok((
        NO_STORE,
        Json(auth::list_publish_tokens(&state.pool).await?),
    )
        .into_response())
}

/// Creates a fallback token. When the body omits publish_token a random one is
/// generated; the plaintext is returned once in the response and only its hash
/// is stored.
async fn add_fallback_token(
    State(state): State<WebState>,
    Json(request): Json<auth::AddPublishTokenRequest>,
) -> Result<Response, ApiError> {
    let token = match request
        .publish_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(token) => {
            if let Err(message) = strategy_catalog::validate_publish_token(token) {
                return Ok(bad_request(message));
            }
            token.to_string()
        }
        None => auth::generate_publish_token(),
    };
    let view =
        match auth::add_publish_token(&state.pool, &request.note, auth::publish_token_hash(&token))
            .await
        {
            Ok(view) => view,
            Err(error) => return Ok(bad_request(error.to_string())),
        };
    Ok((
        NO_STORE,
        Json(serde_json::json!({
            "token_id": view.token_id,
            "note": view.note,
            "created_at": view.created_at,
            "publish_token": token,
        })),
    )
        .into_response())
}

async fn delete_fallback_token(
    State(state): State<WebState>,
    Path(token_id): Path<i64>,
) -> Result<Response, ApiError> {
    match auth::delete_publish_token(&state.pool, token_id).await? {
        true => Ok(StatusCode::NO_CONTENT.into_response()),
        false => Ok(not_found("publish token was not found")),
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
    Extension(user): Extension<AuthUser>,
    Path(source_id): Path<String>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    match load_visible_account_studio(&state.pool, &user, &source_id).await {
        Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn save_account_estimated_fee_rate(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
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
            match load_visible_account_studio(&state.pool, &user, &source_id).await {
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
    Extension(user): Extension<AuthUser>,
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
    let theoretical_twap_fee_rate = match strategy_catalog::resolve_theoretical_twap_fee_rate(
        rates.maker,
        rates.taker,
        request.theoretical_twap_fee_rate,
    ) {
        Ok(value) => value,
        Err(error) => return Ok(bad_request(error)),
    };
    match postgres::save_fee_rates(&state.pool, &source_id, rates, theoretical_twap_fee_rate).await
    {
        Ok(()) => {
            info!(
                source_id,
                maker_fee_rate = rates.maker,
                taker_fee_rate = rates.taker,
                theoretical_twap_fee_rate,
                "account fee rates updated"
            );
            if let Err(error) = refresh_dashboard_cache(&state).await {
                error!(source_id, error = %error, "dashboard refresh after fee update failed");
                return Ok(catalog_error(error));
            }
            match load_visible_account_studio(&state.pool, &user, &source_id).await {
                Ok(studio) => Ok((NO_STORE, Json(studio)).into_response()),
                Err(error) => Ok(catalog_error(error)),
            }
        }
        Err(error) => {
            error!(
                source_id,
                maker_fee_rate = rates.maker,
                taker_fee_rate = rates.taker,
                theoretical_twap_fee_rate,
                error = %error,
                "account fee rate update failed"
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
    cache.strategy_position_snapshots = build.strategy_position_snapshots;
    cache.last_refresh_error = None;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ContractLeverageQuery {
    symbol: Option<String>,
}

async fn get_account_exchange_fee_rates(
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
    match crate::exchange_leverage::get_exchange_fee_rates(source, &symbol).await {
        Ok(result) => {
            info!(
                source_id,
                symbol = %result.symbol,
                vip_tier = result.vip_tier,
                maker_fee_rate = result.maker_fee_rate,
                taker_fee_rate = result.taker_fee_rate,
                account_endpoint = %result.account_endpoint,
                commission_endpoint = %result.commission_endpoint,
                "account exchange fee rates queried"
            );
            Ok((NO_STORE, Json(result)).into_response())
        }
        Err(error) => {
            error!(
                source_id,
                symbol = %symbol,
                error = %format!("{error:#}"),
                "account exchange fee rate query failed"
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
    Extension(user): Extension<AuthUser>,
    Path(source_id): Path<String>,
    Json(request): Json<SaveBindingRequest>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    if !user.is_admin()
        && !user_can_configure_position_strategy(
            &state.pool,
            &user,
            &request.position_strategy_name,
        )
        .await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for this position strategy",
        ));
    }
    if !user.is_admin()
        && let Some((position, _, _, _)) =
            strategy_catalog::load_binding_parts(&state.pool, &source_id, &request.binding_name)
                .await?
        && position.strategy_name != request.position_strategy_name
        && !user_can_configure_position_strategy(&state.pool, &user, &position.strategy_name)
            .await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for the existing account binding",
        ));
    }
    let updated_at_us = unix_now_us();
    let studio = match strategy_catalog::save_binding(
        &state.pool,
        &source_id,
        &request,
        updated_at_us,
    )
    .await
    {
        Ok(studio) => studio,
        Err(error) => return Ok(catalog_error(error)),
    };
    let studio = restrict_account_studio(&state.pool, &user, studio).await?;
    if request.shares == 0.0
        && let Err(failure) =
            stop_binding(&state, &source_id, &request.binding_name, updated_at_us).await
    {
        return Ok(publish_failure_response(failure));
    }
    Ok((NO_STORE, Json(studio)).into_response())
}

async fn save_account_binding_shares(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path((source_id, binding_name)): Path<(String, String)>,
    Json(request): Json<SaveBindingSharesRequest>,
) -> Result<Response, ApiError> {
    if let Err(response) = resolve_order_config_source(&state.config, &source_id) {
        return Ok(response);
    }
    if !user.is_admin()
        && !user_can_configure_binding(&state.pool, &user, &source_id, &binding_name).await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for this account binding",
        ));
    }
    let updated_at_us = unix_now_us();
    let studio = match strategy_catalog::save_binding_shares(
        &state.pool,
        &source_id,
        &binding_name,
        &request,
        updated_at_us,
    )
    .await
    {
        Ok(studio) => studio,
        Err(error) => return Ok(catalog_error(error)),
    };
    let studio = restrict_account_studio(&state.pool, &user, studio).await?;
    if request.shares > 0.0 {
        return Ok((NO_STORE, Json(studio)).into_response());
    }

    if let Err(failure) = stop_binding(&state, &source_id, &binding_name, updated_at_us).await {
        return Ok(publish_failure_response(failure));
    }
    Ok((NO_STORE, Json(studio)).into_response())
}

async fn delete_account_binding(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path((source_id, binding_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    if !user.is_admin()
        && !user_can_configure_binding(&state.pool, &user, &source_id, &binding_name).await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for this account binding",
        ));
    }
    match strategy_catalog::delete_binding(&state.pool, &source_id, &binding_name).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT.into_response()),
        Ok(false) => Ok(not_found("binding was not found")),
        Err(error) => Ok(catalog_error(error)),
    }
}

async fn publish_account_binding(
    State(state): State<WebState>,
    Extension(user): Extension<AuthUser>,
    Path((source_id, binding_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    if !user.is_admin()
        && !user_can_configure_binding(&state.pool, &user, &source_id, &binding_name).await?
    {
        return Ok(forbidden(
            "strategy visibility permission required for this account binding",
        ));
    }
    let shares = strategy_catalog::load_binding_parts(&state.pool, &source_id, &binding_name)
        .await?
        .map(|loaded| loaded.3);
    if shares == Some(0.0) {
        return match stop_binding(&state, &source_id, &binding_name, unix_now_us()).await {
            Ok(published) => Ok((NO_STORE, Json(published)).into_response()),
            Err(error) => Ok(publish_failure_response(error)),
        };
    }
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
        match strategy_catalog::list_active_bindings_for_position(&state.pool, strategy_name).await
        {
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

async fn user_can_configure_binding(
    pool: &PgPool,
    user: &AuthUser,
    source_id: &str,
    binding_name: &str,
) -> Result<bool, ApiError> {
    let Some((position, _, _, _)) =
        strategy_catalog::load_binding_parts(pool, source_id, binding_name).await?
    else {
        return Ok(false);
    };
    user_can_configure_position_strategy(pool, user, &position.strategy_name).await
}

async fn load_visible_account_studio(
    pool: &PgPool,
    user: &AuthUser,
    source_id: &str,
) -> Result<strategy_catalog::AccountStudio> {
    let studio = strategy_catalog::load_account_studio(pool, source_id).await?;
    restrict_account_studio(pool, user, studio).await
}

async fn restrict_account_studio(
    pool: &PgPool,
    user: &AuthUser,
    mut studio: strategy_catalog::AccountStudio,
) -> Result<strategy_catalog::AccountStudio> {
    if user.is_admin() {
        return Ok(studio);
    }
    let visible = strategy_catalog::list_position_visibility(pool)
        .await?
        .into_iter()
        .filter(|strategy| strategy.user_can_view(user.user_id))
        .map(|strategy| strategy.strategy_name)
        .collect::<BTreeSet<_>>();
    retain_visible_account_bindings(&mut studio, &visible);
    Ok(studio)
}

fn retain_visible_account_bindings(
    studio: &mut strategy_catalog::AccountStudio,
    visible_strategy_names: &BTreeSet<String>,
) {
    studio
        .bindings
        .retain(|binding| visible_strategy_names.contains(&binding.position_strategy_name));
}

async fn visible_account_binding_names(
    pool: &PgPool,
    user: &AuthUser,
    source_id: &str,
) -> Result<BTreeSet<String>> {
    Ok(load_visible_account_studio(pool, user, source_id)
        .await?
        .bindings
        .into_iter()
        .map(|binding| binding.binding_name)
        .collect())
}

async fn user_can_configure_position_strategy(
    pool: &PgPool,
    user: &AuthUser,
    strategy_name: &str,
) -> Result<bool, ApiError> {
    let Some(access) = strategy_catalog::load_position_access(pool, strategy_name).await? else {
        return Ok(false);
    };
    Ok(access.user_can_view(user.user_id))
}

struct PublishFailure {
    status: StatusCode,
    message: String,
}

async fn stop_binding(
    state: &WebState,
    source_id: &str,
    binding_name: &str,
    updated_at_us: i64,
) -> std::result::Result<OrderStrategyView, PublishFailure> {
    // Keep the strategy in Exec and publish zero under its original name so
    // close fills remain attributable instead of becoming SYSTEM_POSITION_CLOSE.
    let loaded = strategy_catalog::load_binding_parts(&state.pool, source_id, binding_name)
        .await
        .map_err(|error| PublishFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!(
                "shares were saved as zero, but the binding could not be loaded: {error}"
            ),
        })?
        .ok_or_else(|| PublishFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "shares were saved as zero, but the binding disappeared".to_string(),
        })?;
    let position = loaded.0;
    let factual_positions = load_factual_position(state, source_id, binding_name)
        .await
        .into_iter()
        .collect();
    state
        .position_archive
        .append(
            updated_at_us,
            &position,
            factual_positions,
            vec![crate::position_archive::published_account(
                source_id,
                binding_name,
                0.0,
            )],
        )
        .map_err(|error| {
            error!(
                source_id,
                binding_name,
                error = %error,
                "binding stopped but stop event archive write failed before zero target publish"
            );
            PublishFailure {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: "shares were saved as zero, but the stop event archive failed; the zero target was not published, retry the stop or manual publish".to_string(),
            }
        })?;
    let published = publish_binding(state, source_id, binding_name)
        .await
        .map_err(|mut failure| {
            failure.message = format!(
                "shares were saved as zero, but the zero target publish failed: {}; retry the stop or manual publish",
                failure.message
            );
            failure
        })?;
    info!(
        source_id,
        binding_name, "binding stopped with an attributed zero target"
    );
    Ok(published)
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
    let Some((position, order, symbol_order_parameters, shares)) = loaded else {
        return Err(PublishFailure {
            status: StatusCode::NOT_FOUND,
            message: "binding was not found".to_string(),
        });
    };
    let targets = strategy_catalog::scale_targets(&position.targets, shares);
    let symbol_overrides = symbol_order_parameters
        .into_iter()
        .filter_map(|(symbol, selected)| {
            let override_parameters = crate::order_config::OrderParameterOverrides::from_templates(
                &order.order_parameters,
                &selected,
            );
            (!override_parameters.is_empty()).then_some((symbol, override_parameters))
        })
        .collect();
    let published = state
        .redis_runtime
        .publish_strategy(
            source,
            binding_name,
            &order.order_parameters,
            &symbol_overrides,
            &targets,
        )
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
    let source_ids = match strategy_catalog::list_active_binding_source_ids_for_position(
        &state.pool,
        strategy_name,
    )
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
        if let Some(positions) = load_factual_position(state, &source_id, strategy_name).await {
            out.push(positions);
        }
    }
    out
}

async fn load_factual_position(
    state: &WebState,
    source_id: &str,
    strategy_name: &str,
) -> Option<SourceFactualPositions> {
    let source = state
        .config
        .sources
        .iter()
        .find(|source| source.id == source_id && source.enabled)?;
    let viz_url = source.exec_viz_origin()?;
    match state
        .viz_snapshot
        .load_strategy_positions(source_id, viz_url, strategy_name)
        .await
    {
        Ok(positions) => Some(positions),
        Err(error) => {
            warn!(
                source_id,
                strategy_name,
                error = %error,
                "Exec Viz snapshot factual positions unavailable"
            );
            None
        }
    }
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

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "internal server error".to_string(),
        }),
    )
        .into_response()
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

fn selected_snapshot_map<T: Clone>(
    snapshots: &BTreeMap<String, T>,
    selected_source_ids: &[String],
) -> BTreeMap<String, T> {
    if selected_source_ids.is_empty() {
        return snapshots.clone();
    }
    snapshots
        .iter()
        .filter(|(source_id, _)| selected_source_ids.iter().any(|id| id == *source_id))
        .map(|(source_id, snapshot)| (source_id.clone(), snapshot.clone()))
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

fn resolve_visible_sources<'a>(
    config: &'a AppConfig,
    selected_source_ids: &[String],
    visible_source_ids: &BTreeSet<String>,
) -> std::result::Result<Vec<&'a SourceConfig>, String> {
    if selected_source_ids.is_empty() {
        return Ok(config
            .sources
            .iter()
            .filter(|source| source.enabled && visible_source_ids.contains(&source.id))
            .collect());
    }
    if selected_source_ids
        .iter()
        .any(|source_id| !visible_source_ids.contains(source_id))
    {
        return Err("you are not authorized to view one or more accounts".to_string());
    }
    resolve_sources(config, selected_source_ids)
}

fn milliseconds_to_microseconds(value: i64, field: &str) -> std::result::Result<i64, String> {
    if value < 0 {
        return Err(format!("{field} must not be negative"));
    }
    value
        .checked_mul(1_000)
        .ok_or_else(|| format!("{field} is too large"))
}

const PNL_ARROW_BATCH_ROWS: usize = 65_536;
const TIMELINE_ARROW_SCHEMA_VERSION: &str = "1";

fn timeline_arrow_metadata(snapshot: &TimelineSnapshot, dataset: &str) -> HashMap<String, String> {
    HashMap::from([
        ("compression".to_string(), "zstd".to_string()),
        ("dataset".to_string(), dataset.to_string()),
        (
            "end_ts_us".to_string(),
            snapshot.report.end_ts_us.to_string(),
        ),
        (
            "generated_at_us".to_string(),
            snapshot.generated_at_us.to_string(),
        ),
        (
            "schema_version".to_string(),
            TIMELINE_ARROW_SCHEMA_VERSION.to_string(),
        ),
        (
            "selected_source_ids".to_string(),
            snapshot.report.selected_source_ids.join(","),
        ),
        (
            "selected_symbols".to_string(),
            snapshot.report.selected_symbols.join(","),
        ),
        (
            "start_ts_us".to_string(),
            snapshot.report.start_ts_us.to_string(),
        ),
        (
            "valuation".to_string(),
            snapshot.report.valuation.to_string(),
        ),
    ])
}

fn timeline_totals_fields() -> Vec<Field> {
    vec![
        Field::new("fill_count", DataType::UInt64, false),
        Field::new("volume_quote", DataType::Float64, false),
        Field::new("maker_fill_count", DataType::UInt64, false),
        Field::new("maker_volume_quote", DataType::Float64, false),
        Field::new("taker_fill_count", DataType::UInt64, false),
        Field::new("taker_volume_quote", DataType::Float64, false),
        Field::new("unknown_liquidity_fill_count", DataType::UInt64, false),
        Field::new("unknown_liquidity_volume_quote", DataType::Float64, false),
        Field::new("realized_pnl_before_fee_quote", DataType::Float64, false),
        Field::new("estimated_trading_fee_quote", DataType::Float64, false),
        Field::new("realized_pnl_after_fee_quote", DataType::Float64, false),
        Field::new("floating_pnl_quote", DataType::Float64, false),
        Field::new("nav_change_before_fee_quote", DataType::Float64, false),
        Field::new("nav_change_after_fee_quote", DataType::Float64, false),
    ]
}

fn timeline_totals_arrays<'a>(
    totals: impl IntoIterator<Item = &'a nav::NavTotals>,
) -> Vec<ArrayRef> {
    let totals = totals.into_iter().collect::<Vec<_>>();
    vec![
        Arc::new(UInt64Array::from(
            totals
                .iter()
                .map(|value| value.fill_count)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.volume_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(UInt64Array::from(
            totals
                .iter()
                .map(|value| value.maker_fill_count)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.maker_volume_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(UInt64Array::from(
            totals
                .iter()
                .map(|value| value.taker_fill_count)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.taker_volume_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(UInt64Array::from(
            totals
                .iter()
                .map(|value| value.unknown_liquidity_fill_count)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.unknown_liquidity_volume_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.realized_pnl_before_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.estimated_trading_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.realized_pnl_after_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.floating_pnl_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.nav_change_before_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            totals
                .iter()
                .map(|value| value.nav_change_after_fee_quote)
                .collect::<Vec<_>>(),
        )),
    ]
}

fn arrow_write_options() -> Result<IpcWriteOptions> {
    IpcWriteOptions::default()
        .try_with_compression(Some(CompressionType::ZSTD))?
        .try_with_compression_level(Some(3))
        .map_err(Into::into)
}

fn encode_account_timeline_arrow(snapshot: &TimelineSnapshot) -> Result<Vec<u8>> {
    let mut fields = vec![Field::new("ts_us", DataType::Int64, false)];
    fields.extend(timeline_totals_fields());
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        timeline_arrow_metadata(snapshot, "account_pnl"),
    ));
    let mut payload = Vec::new();
    {
        let mut writer = StreamWriter::try_new_with_options(
            &mut payload,
            schema.as_ref(),
            arrow_write_options()?,
        )?;
        for rows in snapshot.report.points.chunks(PNL_ARROW_BATCH_ROWS) {
            let mut columns: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(
                rows.iter().map(|row| row.ts_us).collect::<Vec<_>>(),
            ))];
            columns.extend(timeline_totals_arrays(rows.iter().map(|row| &row.totals)));
            writer.write(&RecordBatch::try_new(Arc::clone(&schema), columns)?)?;
        }
        writer.finish()?;
    }
    Ok(payload)
}

fn encode_strategy_timeline_arrow(snapshot: &TimelineSnapshot) -> Result<Vec<u8>> {
    let mut fields = vec![
        Field::new("strategy_name", DataType::Utf8, false),
        Field::new("ts_us", DataType::Int64, false),
    ];
    fields.extend(timeline_totals_fields());
    let mut metadata = timeline_arrow_metadata(snapshot, "strategy_pnl");
    metadata.insert(
        "strategy_count".to_string(),
        snapshot.report.strategy_points.len().to_string(),
    );
    metadata.insert("row_shape".to_string(), "long".to_string());
    let schema = Arc::new(Schema::new_with_metadata(fields, metadata));
    let rows = snapshot
        .report
        .strategy_points
        .iter()
        .flat_map(|strategy| {
            strategy
                .points
                .iter()
                .map(move |point| (strategy.strategy.as_str(), point))
        })
        .collect::<Vec<_>>();
    let mut payload = Vec::new();
    {
        let mut writer = StreamWriter::try_new_with_options(
            &mut payload,
            schema.as_ref(),
            arrow_write_options()?,
        )?;
        for batch_rows in rows.chunks(PNL_ARROW_BATCH_ROWS) {
            let mut columns: Vec<ArrayRef> = vec![
                Arc::new(StringArray::from(
                    batch_rows
                        .iter()
                        .map(|(strategy, _)| *strategy)
                        .collect::<Vec<_>>(),
                )),
                Arc::new(Int64Array::from(
                    batch_rows
                        .iter()
                        .map(|(_, point)| point.ts_us)
                        .collect::<Vec<_>>(),
                )),
            ];
            columns.extend(timeline_totals_arrays(
                batch_rows.iter().map(|(_, point)| &point.totals),
            ));
            writer.write(&RecordBatch::try_new(Arc::clone(&schema), columns)?)?;
        }
        writer.finish()?;
    }
    Ok(payload)
}

fn encode_strategy_pnl_arrow(
    report: &nav::StrategyPnlReport,
    generated_at_us: i64,
) -> Result<Vec<u8>> {
    let metadata = HashMap::from([
        ("account".to_string(), report.account.clone()),
        ("compression".to_string(), "zstd".to_string()),
        ("dataset".to_string(), "strategy_fill_pnl".to_string()),
        ("end_ts_us".to_string(), report.end_ts_us.to_string()),
        ("generated_at_us".to_string(), generated_at_us.to_string()),
        (
            "schema_version".to_string(),
            TIMELINE_ARROW_SCHEMA_VERSION.to_string(),
        ),
        ("source_id".to_string(), report.source_id.clone()),
        ("start_ts_us".to_string(), report.start_ts_us.to_string()),
        ("strategy_name".to_string(), report.strategy_name.clone()),
        (
            "valuation".to_string(),
            "quantity_fifo_window_delta".to_string(),
        ),
    ]);
    let schema = Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("source_id", DataType::Utf8, false),
            Field::new("account", DataType::Utf8, false),
            Field::new("strategy_name", DataType::Utf8, false),
            Field::new("row_kind", DataType::Utf8, false),
            Field::new("ts_us", DataType::Int64, false),
            Field::new("record_key", DataType::Utf8, true),
            Field::new("symbol", DataType::Utf8, true),
            Field::new("venue", DataType::Utf8, true),
            Field::new("fill_count", DataType::UInt64, false),
            Field::new("realized_pnl_before_fee_quote", DataType::Float64, false),
            Field::new("estimated_trading_fee_quote", DataType::Float64, false),
            Field::new("realized_pnl_after_fee_quote", DataType::Float64, false),
            Field::new("floating_pnl_quote", DataType::Float64, false),
            Field::new("nav_change_before_fee_quote", DataType::Float64, false),
            Field::new("nav_change_after_fee_quote", DataType::Float64, false),
        ],
        metadata,
    ));
    let options = IpcWriteOptions::default()
        .try_with_compression(Some(CompressionType::ZSTD))?
        .try_with_compression_level(Some(3))?;
    let mut payload = Vec::new();
    {
        let mut writer =
            StreamWriter::try_new_with_options(&mut payload, schema.as_ref(), options)?;
        for rows in report.rows.chunks(PNL_ARROW_BATCH_ROWS) {
            writer.write(&strategy_pnl_record_batch(
                Arc::clone(&schema),
                report,
                rows,
            )?)?;
        }
        writer.finish()?;
    }
    Ok(payload)
}

fn strategy_pnl_record_batch(
    schema: Arc<Schema>,
    report: &nav::StrategyPnlReport,
    rows: &[nav::StrategyPnlRow],
) -> Result<RecordBatch> {
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec![
            report.source_id.as_str();
            rows.len()
        ])),
        Arc::new(StringArray::from(vec![report.account.as_str(); rows.len()])),
        Arc::new(StringArray::from(vec![
            report.strategy_name.as_str();
            rows.len()
        ])),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|row| row.row_kind.as_str())
                .collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter().map(|row| row.ts_us).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|row| row.record_key.as_deref())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|row| row.symbol.as_deref())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|row| row.venue.as_deref())
                .collect::<Vec<_>>(),
        )),
        Arc::new(UInt64Array::from(
            rows.iter()
                .map(|row| row.totals.fill_count)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|row| row.totals.realized_pnl_before_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|row| row.totals.estimated_trading_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|row| row.totals.realized_pnl_after_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|row| row.totals.floating_pnl_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|row| row.totals.nav_change_before_fee_quote)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|row| row.totals.nav_change_after_fee_quote)
                .collect::<Vec<_>>(),
        )),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

fn is_timeline_request_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("start timestamp")
        || message.starts_with("end timestamp")
        || message.starts_with("none of the requested symbols")
}

fn is_strategy_pnl_request_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("start timestamp")
        || message.starts_with("end timestamp")
        || message.starts_with("strategy_name")
        || message.starts_with("source_id")
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
                state.strategy_position_snapshots = build.strategy_position_snapshots;
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
    let mut snapshots = nav::SourcePositionSnapshots::new();
    let mut strategy_snapshots = nav::SourceStrategyPositionSnapshots::new();
    for source in config.sources.iter().filter(|source| source.enabled) {
        if let Some(snapshot) = postgres::load_latest_position_snapshot(pool, &source.id).await? {
            snapshots.insert(source.id.clone(), snapshot);
        }
        if let Some(snapshot) =
            postgres::load_latest_strategy_position_snapshot(pool, &source.id).await?
        {
            strategy_snapshots.insert(source.id.clone(), snapshot);
        }
    }
    let accounts = config
        .sources
        .iter()
        .map(|source| {
            let live = live_equity.get(&source.id);
            let account_pnl_start_ts_us = snapshots
                .get(&source.id)
                .map(|snapshot| snapshot.snapshot_ts_us);
            let strategy_pnl_start_ts_us = strategy_snapshots
                .get(&source.id)
                .filter(|snapshot| {
                    account_pnl_start_ts_us
                        .is_none_or(|account_start| snapshot.snapshot_ts_us >= account_start)
                })
                .map(|snapshot| snapshot.snapshot_ts_us)
                .or(account_pnl_start_ts_us);
            DashboardAccount {
                source_id: source.id.clone(),
                account: source.display_name().to_string(),
                venue: source.venue.clone(),
                enabled: source.enabled,
                gateway_prefix: source.gateway_prefix.clone(),
                configurable: source.exec_config_url.is_some(),
                account_pnl_start_ts_us,
                strategy_pnl_start_ts_us,
                live_equity_usdt: live.as_ref().map(|snapshot| snapshot.equity_usdt),
                live_equity_status: live
                    .as_ref()
                    .map(|snapshot| live_equity_status(snapshot.ts_ms, now_ms)),
            }
        })
        .collect();

    let (report, histories, snapshots, strategy_snapshots) =
        tokio::task::spawn_blocking(move || {
            let histories = nav::load_nav_source_histories(&nav_config, &[])?;
            let report = nav::rebuild_nav_from_histories_with_strategy_snapshots(
                &nav_config,
                &[],
                &snapshots,
                &strategy_snapshots,
                &histories,
            )?;
            anyhow::Ok((report, histories, snapshots, strategy_snapshots))
        })
        .await
        .context("CTA dashboard rebuild task failed")??;
    if let Err(error) = postgres::refresh_source_symbol_index(pool, &histories).await {
        warn!(error = %error, "failed to refresh source symbol index");
    }
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
        strategy_position_snapshots: Arc::new(strategy_snapshots),
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
    use std::io::Cursor;

    use arrow_array::Array;
    use arrow_ipc::reader::StreamReader;

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

    fn timeline_snapshot_for_arrow() -> TimelineSnapshot {
        let mut totals = nav::NavTotals::default();
        totals.fill_count = 3;
        totals.realized_pnl_before_fee_quote = 11.25;
        totals.floating_pnl_quote = -2.5;
        totals.estimated_trading_fee_quote = 0.75;
        totals.nav_change_before_fee_quote = 8.75;
        totals.nav_change_after_fee_quote = 8.0;
        let points = vec![
            nav::NavTimelinePoint {
                ts_us: 1_000,
                gross_position_value_quote: 0.0,
                net_position_value_quote: 0.0,
                totals: nav::NavTotals::default(),
            },
            nav::NavTimelinePoint {
                ts_us: 2_000,
                gross_position_value_quote: 100.0,
                net_position_value_quote: 100.0,
                totals,
            },
        ];
        TimelineSnapshot {
            generated_at_us: 3_000,
            generation_duration_ms: 1,
            report: nav::NavTimelineReport {
                valuation: "quantity_fifo_window_delta",
                earliest_start_ts_us: 1_000,
                start_ts_us: 1_000,
                end_ts_us: 2_000,
                selected_source_ids: vec!["trade01".to_string()],
                available_symbols: vec!["BTCUSDT".to_string()],
                selected_symbols: vec!["BTCUSDT".to_string()],
                available_strategies: vec!["cta_alpha".to_string()],
                summary: totals,
                symbols: Vec::new(),
                points: points.clone(),
                symbol_points: Vec::new(),
                strategy_points: vec![nav::StrategyNavTimeline {
                    strategy: "cta_alpha".to_string(),
                    symbol_count: 1,
                    gross_position_value_quote: 100.0,
                    net_position_value_quote: 100.0,
                    summary: totals,
                    points,
                }],
                sampled: false,
            },
            theoretical: crate::theoretical_nav::TheoreticalNavTimeline::default(),
        }
    }

    #[test]
    fn account_timeline_arrow_is_a_versioned_table_with_separate_pnl_columns() {
        let payload = encode_account_timeline_arrow(&timeline_snapshot_for_arrow()).unwrap();
        let mut reader = StreamReader::try_new_buffered(Cursor::new(payload), None).unwrap();
        assert_eq!(
            reader.schema().metadata().get("dataset"),
            Some(&"account_pnl".to_string())
        );
        assert_eq!(
            reader.schema().metadata().get("schema_version"),
            Some(&TIMELINE_ARROW_SCHEMA_VERSION.to_string())
        );
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 2);
        let realized = batch
            .column_by_name("realized_pnl_before_fee_quote")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let floating = batch
            .column_by_name("floating_pnl_quote")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(realized.value(1).to_bits(), 11.25_f64.to_bits());
        assert_eq!(floating.value(1).to_bits(), (-2.5_f64).to_bits());
    }

    #[test]
    fn strategy_timeline_arrow_is_a_long_table() {
        let payload = encode_strategy_timeline_arrow(&timeline_snapshot_for_arrow()).unwrap();
        let mut reader = StreamReader::try_new_buffered(Cursor::new(payload), None).unwrap();
        assert_eq!(
            reader.schema().metadata().get("dataset"),
            Some(&"strategy_pnl".to_string())
        );
        assert_eq!(
            reader.schema().metadata().get("row_shape"),
            Some(&"long".to_string())
        );
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 2);
        let strategies = batch
            .column_by_name("strategy_name")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(strategies.value(0), "cta_alpha");
        assert_eq!(strategies.value(1), "cta_alpha");
    }

    #[test]
    fn strategy_pnl_arrow_stream_round_trips_without_losing_float_values() {
        let mut totals = nav::NavTotals::default();
        totals.nav_change_after_fee_quote = 12.345_678_901;
        let report = nav::StrategyPnlReport {
            source_id: "trade01".to_string(),
            account: "account-01".to_string(),
            strategy_name: "cta_alpha".to_string(),
            start_ts_us: 1_000,
            end_ts_us: 2_000,
            rows: vec![
                nav::StrategyPnlRow {
                    row_kind: nav::StrategyPnlRowKind::WindowStart,
                    ts_us: 1_000,
                    record_key: None,
                    symbol: None,
                    venue: None,
                    totals: nav::NavTotals::default(),
                },
                nav::StrategyPnlRow {
                    row_kind: nav::StrategyPnlRowKind::WindowEnd,
                    ts_us: 2_000,
                    record_key: None,
                    symbol: None,
                    venue: None,
                    totals,
                },
            ],
        };

        let payload = encode_strategy_pnl_arrow(&report, 3_000).unwrap();
        let mut reader = StreamReader::try_new_buffered(Cursor::new(payload), None).unwrap();
        assert_eq!(
            reader.schema().metadata().get("compression"),
            Some(&"zstd".to_string())
        );
        assert_eq!(
            reader.schema().metadata().get("strategy_name"),
            Some(&"cta_alpha".to_string())
        );
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 2);
        let kinds = batch
            .column_by_name("row_kind")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(kinds.value(0), "window_start");
        assert_eq!(kinds.value(1), "window_end");
        let after_fee = batch
            .column_by_name("nav_change_after_fee_quote")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(after_fee.value(1).to_bits(), 12.345_678_901_f64.to_bits());
        assert!(reader.next().is_none());
    }

    #[test]
    fn strategy_pnl_summary_uses_the_terminal_window_totals() {
        let mut terminal = nav::NavTotals::default();
        terminal.fill_count = 3;
        terminal.nav_change_after_fee_quote = 12.5;
        let report = nav::StrategyPnlReport {
            source_id: "trade01".to_string(),
            account: "account-01".to_string(),
            strategy_name: "cta_alpha".to_string(),
            start_ts_us: 1_000,
            end_ts_us: 2_000,
            rows: vec![
                nav::StrategyPnlRow {
                    row_kind: nav::StrategyPnlRowKind::WindowStart,
                    ts_us: 1_000,
                    record_key: None,
                    symbol: None,
                    venue: None,
                    totals: nav::NavTotals::default(),
                },
                nav::StrategyPnlRow {
                    row_kind: nav::StrategyPnlRowKind::WindowEnd,
                    ts_us: 2_000,
                    record_key: None,
                    symbol: None,
                    venue: None,
                    totals: terminal,
                },
            ],
        };

        let summary = strategy_pnl_summary_from_report(report, 3_000, 4);
        assert_eq!(summary.generated_at_us, 3_000);
        assert_eq!(summary.generation_duration_ms, 4);
        assert_eq!(summary.totals, terminal);
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
            monitor: crate::config::MonitorConfig::default(),
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
        assert!(script.contains("automatically republishes every active"));
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

    #[test]
    fn symbols_merge_index_rows_with_snapshot_anchors() {
        let source = crate::config::SourceConfig {
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
            gateway_prefix: None,
            exec_config_url: None,
            exec_viz_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            legacy_share_unit_usdt: None,
            env_path: None,
        };
        let config = crate::config::AppConfig {
            database: crate::config::DatabaseConfig {
                url_env: "CRYPTO_CTA_LOCAL_DATABASE_URL".into(),
                max_connections: 1,
            },
            ingestion: crate::config::IngestionConfig::default(),
            order_config: crate::config::OrderConfigSettings::default(),
            redis: crate::config::RedisSettings::default(),
            twap: crate::config::TwapConfig::default(),
            monitor: crate::config::MonitorConfig::default(),
            sources: vec![source],
        };
        let sources = resolve_sources(&config, &[]).unwrap();
        let rows = vec![postgres::SourceSymbol {
            source_id: "binance_exec_trade01".into(),
            symbol: "BTCUSDT".into(),
            venue_code: 1,
            venue: "binance-futures".into(),
            first_event_ts_us: Some(10),
            first_fill_ts_us: Some(12),
            last_fill_ts_us: Some(20),
        }];
        let mut snapshots = nav::SourcePositionSnapshots::new();
        snapshots.insert(
            "binance_exec_trade01".to_string(),
            crate::snapshot::PositionSnapshot {
                source_id: "binance_exec_trade01".into(),
                snapshot_ts_us: 1,
                positions: vec![crate::snapshot::SnapshotPosition {
                    symbol: "ETHUSDT".into(),
                    venue_code: 1,
                    quantity: 1.0,
                    reference_price: None,
                }],
            },
        );

        let views = merge_symbol_rows(
            &sources,
            rows,
            &snapshots,
            &nav::SourceStrategyPositionSnapshots::new(),
        );

        assert_eq!(views.len(), 1);
        let symbols = &views[0].symbols;
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].symbol, "BTCUSDT");
        assert_eq!(symbols[0].venue.as_deref(), Some("binance-futures"));
        assert_eq!(symbols[0].first_fill_ts_us, Some(12));
        assert_eq!(symbols[1].symbol, "ETHUSDT");
        assert_eq!(symbols[1].venue, None);
        assert_eq!(symbols[1].first_fill_ts_us, None);
    }

    #[test]
    fn position_access_token_rules() {
        let open = crate::strategy_catalog::PositionAccess {
            created_by_user_id: None,
            publish_token_hash: None,
            open_visibility: true,
            manager_user_ids: vec![],
            viewer_user_ids: vec![],
        };
        assert!(!open.token_required());
        assert!(!open.user_can_publish(7));
        assert!(open.user_can_view(99));

        let private = crate::strategy_catalog::PositionAccess {
            created_by_user_id: Some(3),
            open_visibility: false,
            ..open.clone()
        };
        assert!(private.user_can_view(3));
        assert!(!private.user_can_view(99));

        let protected = crate::strategy_catalog::PositionAccess {
            created_by_user_id: Some(3),
            publish_token_hash: Some(auth::publish_token_hash("s3cret-token")),
            open_visibility: false,
            manager_user_ids: vec![7],
            viewer_user_ids: vec![11],
        };
        assert!(protected.token_required());
        assert!(protected.user_can_publish(3));
        assert!(protected.user_can_publish(7));
        assert!(!protected.user_can_publish(8));
        assert!(protected.user_can_view(3));
        assert!(protected.user_can_view(7));
        assert!(protected.user_can_view(11));
        assert!(!protected.user_can_view(8));
        assert!(auth::publish_token_matches(
            Some("s3cret-token"),
            protected.publish_token_hash.as_deref().unwrap()
        ));
        assert!(!auth::publish_token_matches(
            Some("wrong-token"),
            protected.publish_token_hash.as_deref().unwrap()
        ));
        assert!(!auth::publish_token_matches(
            None,
            protected.publish_token_hash.as_deref().unwrap()
        ));
    }

    #[test]
    fn source_grants_allow_only_their_account_mutations() {
        let allowed = BTreeSet::from(["binance_exec_trade01".to_string()]);
        assert_eq!(request_permission_error(false, true, None, &allowed), None);
        assert_eq!(
            request_permission_error(false, false, Some("binance_exec_trade01"), &allowed),
            None
        );
        assert_eq!(
            request_permission_error(false, false, Some("binance_exec_trade02"), &allowed),
            Some("you are not authorized to view or configure this account")
        );
        assert_eq!(
            request_permission_error(false, false, None, &allowed),
            Some("administrator permission required")
        );
        assert_eq!(
            request_permission_error(true, false, Some("binance_exec_trade02"), &allowed),
            None
        );
    }

    #[test]
    fn account_studio_only_keeps_bindings_for_visible_strategies() {
        let mut studio = crate::strategy_catalog::AccountStudio {
            source_id: "binance_exec_trade06".to_string(),
            estimated_fee_rate: 0.0004,
            maker_fee_rate: 0.0002,
            taker_fee_rate: 0.0004,
            theoretical_twap_fee_rate: 0.0004,
            bindings: vec![
                crate::strategy_catalog::AccountBinding {
                    source_id: "binance_exec_trade06".to_string(),
                    binding_name: "sk_strategy".to_string(),
                    position_strategy_name: "sk_strategy".to_string(),
                    order_strategy_name: "default_order".to_string(),
                    shares: 1.0,
                    updated_at_us: 1,
                },
                crate::strategy_catalog::AccountBinding {
                    source_id: "binance_exec_trade06".to_string(),
                    binding_name: "prc_strategy".to_string(),
                    position_strategy_name: "prc_strategy".to_string(),
                    order_strategy_name: "default_order".to_string(),
                    shares: 1.0,
                    updated_at_us: 1,
                },
            ],
        };
        retain_visible_account_bindings(&mut studio, &BTreeSet::from(["sk_strategy".to_string()]));
        assert_eq!(studio.bindings.len(), 1);
        assert_eq!(studio.bindings[0].binding_name, "sk_strategy");
    }
}
