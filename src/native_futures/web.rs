use super::{config::Config, cost, market_data::Feed, reader, reference};
use crate::{
    auth::{self, AuthUser, SourceAccess},
    strategy_catalog::{self, SavePositionStrategyRequest},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{any, get, post, put},
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

const COOKIE: &str = "cn_cta_session";
type ApiResult = std::result::Result<Json<Value>, ApiError>;
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if error.downcast_ref::<super::sync::ReplicaBusy>().is_some() {
            return Self(StatusCode::SERVICE_UNAVAILABLE, error.to_string());
        }
        tracing::error!(error=?error,"domestic Manager request failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "请求失败，请查看管理器日志".into(),
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
fn forbidden() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "没有操作权限".into())
}
fn bad(message: impl ToString) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_string())
}
fn body(value: impl Serialize) -> ApiResult {
    Ok(Json(
        serde_json::to_value(value).map_err(anyhow::Error::from)?,
    ))
}
fn token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|v| v.strip_prefix(&format!("{COOKIE}=")).map(str::to_owned))
}

#[derive(Clone)]
pub struct Service {
    config: Arc<Config>,
    pool: PgPool,
    readers: Arc<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
    catalog_lock: Arc<tokio::sync::Mutex<()>>,
    market_data: Option<Arc<Feed>>,
}

impl Service {
    pub fn new(config: Config, pool: PgPool) -> Result<Self> {
        config.validate()?;
        let market_data = config
            .market_data
            .clone()
            .map(Feed::new)
            .transpose()?
            .map(Arc::new);
        Ok(Self {
            readers: Arc::new(
                config
                    .sources
                    .iter()
                    .map(|s| (s.id.clone(), Arc::default()))
                    .collect(),
            ),
            config: Arc::new(config),
            pool,
            catalog_lock: Arc::default(),
            market_data,
        })
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/auth/status", get(auth_status))
            .route("/api/auth/register", post(auth_register))
            .route("/api/auth/login", post(auth_login))
            .route("/api/auth/logout", post(auth_logout))
            .route("/api/auth/users", get(users).post(create_user))
            .route("/api/auth/users/{id}/role", put(set_role))
            .route("/api/native/sources", get(sources))
            .route("/api/native/sources/{id}/snapshot", get(snapshot))
            .route("/api/native/sources/{id}/cost", get(cost_report))
            .route(
                "/api/native/sources/{id}/grants",
                get(source_grants).put(save_source_grants),
            )
            .route(
                "/api/native/strategies",
                get(strategies).post(save_strategy),
            )
            .route(
                "/api/native/strategies/{name}/grants",
                get(strategy_grants).put(save_strategy_grants),
            )
            .with_state(self.clone())
            .route_layer(middleware::from_fn_with_state(self, authenticate))
            .route_layer(tower_http::compression::CompressionLayer::new().gzip(true))
    }
}

/// Mount built assets outside the API authentication middleware. Both API
/// prefixes retain the same session/source permissions and JSON responses.
pub fn with_frontend(api: Router, root: &std::path::Path) -> Result<Router> {
    let root = root
        .canonicalize()
        .context("resolve native frontend root")?;
    let index = root.join("index.html");
    ensure!(
        index.is_file(),
        "native frontend root must contain index.html"
    );
    let api = api.route("/api/{*path}", any(|| async { StatusCode::NOT_FOUND }));
    let files = tower_http::services::ServeDir::new(&root)
        .not_found_service(tower_http::services::ServeFile::new(index));
    Ok(Router::new()
        .route("/", get(|| async { Redirect::temporary("/manager/") }))
        .merge(api.clone())
        .nest("/manager", api)
        .nest_service("/manager", files))
}

pub async fn serve(
    config: Config,
    pool: PgPool,
    bind: SocketAddr,
    frontend_root: Option<&std::path::Path>,
) -> Result<()> {
    ensure!(
        bind.ip().is_loopback(),
        "domestic Manager API must bind to loopback behind its own authenticated gateway"
    );
    let mut app = Service::new(config, pool)?.router();
    if let Some(root) = frontend_root {
        app = with_frontend(app, root)?;
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("bind domestic Manager API")?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serve domestic Manager API")
}

pub async fn register_sources(pool: &PgPool, config: &Config) -> Result<()> {
    for source in &config.sources {
        sqlx::query("INSERT INTO cta_order_sources(source_id,account_label,venue_label,rocksdb_path,enabled,estimated_fee_rate,maker_fee_rate,taker_fee_rate,theoretical_twap_fee_rate) VALUES($1,$2,$3,$4,$5,0,0,0,0) ON CONFLICT(source_id) DO UPDATE SET account_label=EXCLUDED.account_label,venue_label=EXCLUDED.venue_label,rocksdb_path=EXCLUDED.rocksdb_path,enabled=EXCLUDED.enabled,updated_at=now()")
            .bind(&source.id).bind(&source.account).bind(&source.adapter).bind(source.rocksdb_path.to_string_lossy().as_ref()).bind(source.enabled).execute(pool).await.context("register domestic source catalog")?;
    }
    Ok(())
}

async fn authenticate(
    State(service): State<Service>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if matches!(
        request.uri().path(),
        "/api/auth/status" | "/api/auth/register" | "/api/auth/login" | "/api/auth/logout"
    ) {
        return next.run(request).await;
    }
    let Some(token) = token(request.headers()) else {
        return ApiError(StatusCode::UNAUTHORIZED, "请先登录".into()).into_response();
    };
    let user = match auth::load_session(&service.pool, &token).await {
        Ok(Some(u)) => u,
        Ok(None) => return ApiError(StatusCode::UNAUTHORIZED, "请重新登录".into()).into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let access = match auth::allowed_source_access(
        &service.pool,
        &user,
        &service.config.source_ids(),
        &service.config.source_viewers,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return ApiError::from(e).into_response(),
    };
    request.extensions_mut().insert(user);
    request.extensions_mut().insert(Arc::new(access));
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

async fn auth_status(State(s): State<Service>, headers: HeaderMap) -> ApiResult {
    let user = match token(&headers) {
        Some(t) => auth::load_session(&s.pool, &t).await?,
        None => None,
    };
    let view = if let Some(u) = user {
        let mut v = auth::user_view(&s.pool, &u).await?;
        let allowed = auth::allowed_source_access(
            &s.pool,
            &u,
            &s.config.source_ids(),
            &s.config.source_viewers,
        )
        .await?;
        v.source_grants
            .retain(|g| allowed.view.contains(&g.source_id));
        Some(v)
    } else {
        None
    };
    body(
        json!({"authenticated":view.is_some(),"setup_required":!auth::has_users(&s.pool).await?,"user":view}),
    )
}
async fn session(s: &Service, user: AuthUser) -> std::result::Result<Response, ApiError> {
    let (token, _) = auth::create_session(&s.pool, user.user_id).await?;
    let access = auth::allowed_source_access(
        &s.pool,
        &user,
        &s.config.source_ids(),
        &s.config.source_viewers,
    )
    .await?;
    let mut user = auth::user_view(&s.pool, &user).await?;
    user.source_grants
        .retain(|g| access.view.contains(&g.source_id));
    Ok((
        [(
            header::SET_COOKIE,
            auth::session_cookie(&token).replacen("cta_session=", &format!("{COOKIE}="), 1),
        )],
        Json(json!({"user":user})),
    )
        .into_response())
}
async fn auth_register(
    State(s): State<Service>,
    Json(r): Json<auth::RegisterRequest>,
) -> std::result::Result<Response, ApiError> {
    let user = auth::register(&s.pool, r).await.map_err(bad)?;
    session(&s, user).await
}
async fn auth_login(
    State(s): State<Service>,
    Json(r): Json<auth::LoginRequest>,
) -> std::result::Result<Response, ApiError> {
    let user = auth::login(&s.pool, r)
        .await
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "账号或密码错误".into()))?;
    session(&s, user).await
}
async fn auth_logout(
    State(s): State<Service>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    if let Some(t) = token(&headers) {
        auth::delete_session(&s.pool, &t).await?;
    }
    Ok((
        [(
            header::SET_COOKIE,
            auth::clear_session_cookie().replacen("cta_session=", &format!("{COOKIE}="), 1),
        )],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}
async fn users(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
) -> ApiResult {
    let mut rows = auth::list_users(&s.pool).await?;
    for row in &mut rows {
        row.source_grants
            .retain(|g| user.is_admin() && access.configure.contains(&g.source_id));
    }
    body(rows)
}
async fn create_user(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Json(r): Json<auth::CreateUserRequest>,
) -> ApiResult {
    if !user.is_admin() {
        return Err(forbidden());
    }
    body(auth::create_user(&s.pool, r).await.map_err(bad)?)
}
async fn set_role(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<i64>,
    Json(r): Json<auth::SetRoleRequest>,
) -> ApiResult {
    if !user.is_admin() {
        return Err(forbidden());
    }
    let mut saved = auth::set_role(&s.pool, id, &r.role).await.map_err(bad)?;
    saved
        .source_grants
        .retain(|g| access.configure.contains(&g.source_id));
    body(saved)
}
async fn sources(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
) -> ApiResult {
    body(s.config.sources.iter().filter(|source|source.enabled && access.view.contains(&source.id)).map(|source|json!({"source_id":source.id,"account":source.account,"adapter":source.adapter,"access_level":if access.configure.contains(&source.id){"configure"}else{"view"},"execution_enabled":false})).collect::<Vec<_>>())
}
async fn snapshot(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    if !access.view.contains(&id) {
        return Err(forbidden());
    }
    let source = s
        .config
        .sources
        .iter()
        .find(|source| source.enabled && source.id == id)
        .cloned()
        .ok_or_else(forbidden)?;
    let secondary = s.config.secondary_root.join(&id);
    let lock = s.readers[&id].clone();
    let guard = lock.lock_owned().await;
    let limit = s.config.recent_rows;
    let stale = s.config.stale_secs;
    let observed = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        reader::read(&source, &secondary, limit, stale)
    })
    .await
    .map_err(anyhow::Error::from)??;
    body(observed)
}

#[derive(serde::Deserialize)]
struct CostQuery {
    trading_day: String,
}

async fn cost_report(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CostQuery>,
) -> ApiResult {
    if !access.view.contains(&id) {
        return Err(forbidden());
    }
    reference::valid_day(&query.trading_day).map_err(bad)?;
    let source = s
        .config
        .sources
        .iter()
        .find(|source| source.enabled && source.id == id)
        .cloned()
        .ok_or_else(forbidden)?;
    let config = s.config.clone();
    let lock = s.readers[&id].clone();
    let guard = lock.lock_owned().await;
    let day = query.trading_day;
    let read_day = day.clone();
    let read_id = id.clone();
    let (mut pack, facts, reference_error) = tokio::task::spawn_blocking(move || -> Result<_> {
        let _guard = guard;
        let loaded = if config.reference_root.is_some() {
            reference::load(&config, &read_id, &read_day)
        } else {
            Ok(None)
        };
        let (pack, error) = match loaded {
            Ok(pack) => (pack, false),
            Err(e) => {
                tracing::warn!(error=?e,"domestic reference unavailable");
                (None, true)
            }
        };
        let facts =
            reader::read_cost_facts(&source, &config.secondary_root.join(&read_id), &read_day)?;
        Ok((pack, facts, error))
    })
    .await
    .map_err(anyhow::Error::from)??;
    let mut market_error = None;
    if let (Some(feed), Some(pack)) = (&s.market_data, &mut pack) {
        if let Err(e) = feed.augment(pack, super::now_us()).await {
            tracing::warn!(error=?e,"domestic market data unavailable");
            market_error = Some("行情接口或缓存暂不可用；保留已覆盖参考与事实成交".to_owned());
        }
    }
    let mut report = tokio::task::spawn_blocking(move || {
        cost::build(&id, &day, pack.as_ref(), facts, super::now_us())
    })
    .await
    .map_err(anyhow::Error::from)??;
    report.market_data_error = market_error;
    if reference_error {
        report.reason = Some("参考文件读取或校验失败；保留事实成交，请检查数据维护记录".into());
    }
    body(report)
}
async fn source_grants(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    if !access.configure.contains(&id) {
        return Err(forbidden());
    }
    body(auth::list_source_grants(&s.pool, &id).await?)
}
async fn save_source_grants(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(r): Json<auth::SetSourceGrantsRequest>,
) -> ApiResult {
    if !access.configure.contains(&id) {
        return Err(forbidden());
    }
    body(
        auth::set_source_grants(&s.pool, &id, &r.grants, &s.config.source_ids())
            .await
            .map_err(bad)?,
    )
}
async fn strategies(State(s): State<Service>, Extension(user): Extension<AuthUser>) -> ApiResult {
    let visibility = strategy_catalog::list_position_visibility(&s.pool)
        .await?
        .into_iter()
        .map(|v| (v.strategy_name.clone(), v))
        .collect::<BTreeMap<_, _>>();
    let mut rows = vec![];
    for strategy in strategy_catalog::list_position_strategies(&s.pool).await? {
        if let Some(v) = visibility
            .get(&strategy.strategy_name)
            .filter(|v| user.is_admin() || v.user_can_view(user.user_id))
        {
            rows.push(json!({"strategy":strategy,"can_configure":user.is_admin() || v.user_can_configure(user.user_id)}));
        }
    }
    body(rows)
}
fn validate_strategy(r: &SavePositionStrategyRequest) -> Result<()> {
    crate::order_config::validate_strategy_name(&r.strategy_name).map_err(anyhow::Error::msg)?;
    ensure!(
        r.symbol_order_strategy_overrides.is_empty(),
        "native order-template overrides require a later execution integration"
    );
    for (market, target) in &r.targets {
        super::validate_market(market)?;
        ensure!(
            target.qty.is_finite()
                && target.qty.fract() == 0.0
                && target.qty.abs() <= i32::MAX as f64,
            "native targets must be signed integer lots"
        );
        ensure!(
            !target.clear || target.qty == 0.0,
            "clear requires zero lots"
        );
        crate::order_config::validate_target_signal(target.signal).map_err(anyhow::Error::msg)?;
    }
    Ok(())
}
async fn save_strategy(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Json(r): Json<SavePositionStrategyRequest>,
) -> ApiResult {
    validate_strategy(&r).map_err(bad)?;
    let _guard = s.catalog_lock.lock().await;
    let now = super::now_us();
    if strategy_catalog::load_position_access(&s.pool, &r.strategy_name)
        .await?
        .is_none()
    {
        strategy_catalog::create_position_strategy(&s.pool, &r, user.user_id, now).await?;
    }
    let access = strategy_catalog::load_position_access(&s.pool, &r.strategy_name)
        .await?
        .ok_or_else(forbidden)?;
    if !user.is_admin() && !access.user_can_configure(user.user_id) {
        return Err(forbidden());
    }
    body(strategy_catalog::store_position_strategy(&s.pool, &r, Some(user.user_id), now).await?)
}
async fn strategy_grants(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Path(name): Path<String>,
) -> ApiResult {
    let access = strategy_catalog::load_position_access(&s.pool, &name)
        .await?
        .ok_or_else(forbidden)?;
    if !user.is_admin() && !access.user_can_manage_grants(user.user_id) {
        return Err(forbidden());
    }
    body(strategy_catalog::position_access_view(&s.pool, &name).await?)
}
async fn save_strategy_grants(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Path(name): Path<String>,
    Json(r): Json<strategy_catalog::SavePositionGrantsRequest>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    let access = strategy_catalog::load_position_access(&s.pool, &name)
        .await?
        .ok_or_else(forbidden)?;
    if !user.is_admin() && !access.user_can_manage_grants(user.user_id) {
        return Err(forbidden());
    }
    strategy_catalog::set_position_grants(&s.pool, &name, &r.grants, r.open_visibility)
        .await
        .map_err(bad)?;
    body(strategy_catalog::position_access_view(&s.pool, &name).await?)
}

#[cfg(test)]
pub(super) fn validate_test_strategy(r: &SavePositionStrategyRequest) -> Result<()> {
    validate_strategy(r)
}
