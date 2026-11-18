mod netting;
mod netting_machine;
mod netting_plan;

use super::{config::Config, cost, market_data::Feed, nav, reader, reference, reference_data};
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
    serve::ListenerExt,
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};
use tokio::sync::watch;

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
    config: Arc<RuntimeConfig>,
    pool: PgPool,
    readers: Arc<BTreeMap<String, Arc<tokio::sync::Mutex<reader::Cache>>>>,
    rocks: Arc<crate::rocks_source::ReaderPool>,
    catalog_lock: Arc<tokio::sync::Mutex<()>>,
    market_data: Option<Arc<Feed>>,
    reference_service: Option<Arc<reference_data::Feed>>,
    plan_changes: Arc<BTreeMap<String, watch::Sender<()>>>,
    plan_shutdown: watch::Sender<bool>,
}

struct RuntimeConfig {
    settings: Config,
    ctaker_plan_readers: BTreeMap<String, [u8; 32]>,
}

impl std::ops::Deref for RuntimeConfig {
    type Target = Config;

    fn deref(&self) -> &Config {
        &self.settings
    }
}

impl Service {
    pub fn new(config: Config, pool: PgPool) -> Result<Self> {
        config.validate()?;
        let ctaker_plan_readers = netting_plan::credentials(&config)?;
        let plan_changes = ctaker_plan_readers
            .keys()
            .map(|source| (source.clone(), watch::channel(()).0))
            .collect();
        let rocks = Arc::new(crate::rocks_source::ReaderPool::new(
            &config.secondary_root,
            &config
                .sources
                .iter()
                .map(|s| s.rocksdb_path.as_path())
                .collect::<Vec<_>>(),
        )?);
        let market_data = config
            .market_data
            .clone()
            .map(Feed::new)
            .transpose()?
            .map(Arc::new);
        let reference_service = config
            .reference_service
            .clone()
            .map(reference_data::Feed::new)
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
            config: Arc::new(RuntimeConfig {
                settings: config,
                ctaker_plan_readers,
            }),
            pool,
            rocks,
            catalog_lock: Arc::default(),
            market_data,
            reference_service,
            plan_changes: Arc::new(plan_changes),
            plan_shutdown: watch::channel(false).0,
        })
    }
    fn plan_changed(&self, source: &str) {
        if let Some(wake) = self.plan_changes.get(source) {
            wake.send_replace(());
        }
    }

    pub fn router(self) -> Router {
        let plan_router = netting_plan::routes(self.clone());
        Router::new()
            .route("/api/auth/status", get(auth_status))
            .route("/api/auth/register", post(auth_register))
            .route("/api/auth/login", post(auth_login))
            .route("/api/auth/logout", post(auth_logout))
            .route("/api/auth/users", get(users).post(create_user))
            .route("/api/auth/users/{id}/role", put(set_role))
            .route("/api/native/sources", get(sources))
            .route("/api/native/sources/{id}/snapshot", get(snapshot))
            .route("/api/native/sources/{id}/market", get(market_day))
            .route("/api/native/sources/{id}/rates", get(account_rates))
            .route(
                "/api/native/sources/{id}/nav-snapshots",
                get(nav_snapshots).post(capture_nav_snapshot),
            )
            .route("/api/native/sources/{id}/nav", get(native_nav))
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
            .merge(netting::routes())
            .merge(netting_machine::routes())
            .with_state(self.clone())
            .route_layer(middleware::from_fn_with_state(self, authenticate))
            .route_layer(tower_http::compression::CompressionLayer::new().gzip(true))
            .merge(plan_router)
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
    let service = Service::new(config, pool)?;
    let plan_shutdown = service.plan_shutdown.clone();
    let mut app = service.router();
    if let Some(root) = frontend_root {
        app = with_frontend(app, root)?;
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("bind domestic Manager API")?
        .tap_io(|socket| {
            if let Err(error) = socket.set_nodelay(true) {
                tracing::debug!(%error, "could not enable TCP_NODELAY for Manager connection");
            }
        });
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            plan_shutdown.send_replace(true);
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
    if request.headers().contains_key("X-CTA-Publish-Token") {
        let headers = request.headers().clone();
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let (user, access, scope) =
            match netting_machine::authenticate_machine(&service, &headers, &method, &path).await {
                Ok(principal) => principal,
                Err(error) => return error.into_response(),
            };
        request.extensions_mut().insert(user);
        request.extensions_mut().insert(Arc::new(access));
        request.extensions_mut().insert(scope);
        let mut response = next.run(request).await;
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        return response;
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
    let rocks = s.rocks.clone();
    let lock = s.readers[&id].clone();
    let guard = lock.lock_owned().await;
    let limit = s.config.recent_rows;
    let stale = s.config.stale_secs;
    let observed = tokio::task::spawn_blocking(move || {
        let mut cache = guard;
        reader::with_db(&rocks, &source, |db| cache.read(&source, db, limit, stale))
    })
    .await
    .map_err(anyhow::Error::from)??;
    body(observed)
}

#[derive(serde::Deserialize)]
struct CostQuery {
    trading_day: String,
}

#[derive(serde::Deserialize)]
struct MarketQuery {
    trading_day: String,
    venue: String,
    symbol: String,
}
async fn market_day(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<MarketQuery>,
) -> ApiResult {
    if !access.view.contains(&id) {
        return Err(forbidden());
    }
    reference::valid_day(&query.trading_day).map_err(bad)?;
    super::validate_market(&format!("{}:{}", query.venue, query.symbol)).map_err(bad)?;
    let feed = s
        .market_data
        .as_ref()
        .context("native market-replay is not configured")?;
    body(
        feed.day(
            &query.venue,
            &query.symbol,
            &query.trading_day,
            super::now_us(),
        )
        .await?,
    )
}

#[derive(serde::Deserialize)]
struct RatesQuery {
    venue: String,
    symbol: String,
    at_us: i64,
}
async fn account_rates(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RatesQuery>,
) -> ApiResult {
    if !access.view.contains(&id) {
        return Err(forbidden());
    }
    super::validate_market(&format!("{}:{}", query.venue, query.symbol)).map_err(bad)?;
    let feed = s
        .reference_service
        .as_ref()
        .context("reference_service is not configured")?;
    let rates = feed
        .rates(&id, &query.venue, &query.symbol, query.at_us)
        .await?;
    body(
        json!({"source_id":id,"at_us":query.at_us,"currency":"CNY","rates":rates,"fee_basis":"account_rate_observation"}),
    )
}

async fn nav_snapshots(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    if !access.view.contains(&id) {
        return Err(forbidden());
    }
    body(nav::store::list(&s.pool, &id).await?)
}
#[derive(serde::Deserialize)]
struct CaptureNav {
    expected_state_us: i64,
    note: String,
}
async fn capture_nav_snapshot(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<CaptureNav>,
) -> ApiResult {
    if !access.configure.contains(&id) {
        return Err(forbidden());
    }
    if request.note.trim().is_empty() || request.note.len() > 4096 {
        return Err(bad("请填写起始快照说明，最多 4096 字节"));
    }
    let source = s
        .config
        .sources
        .iter()
        .find(|v| v.enabled && v.id == id)
        .cloned()
        .ok_or_else(forbidden)?;
    let rocks = s.rocks.clone();
    let lock = s.readers[&id].clone();
    let mut cache = lock.lock_owned().await;
    let limit = s.config.recent_rows;
    let stale = s.config.stale_secs;
    let candidate = tokio::task::spawn_blocking(move || {
        reader::with_db(&rocks, &source, |db| {
            let snapshot = cache.read(&source, db, limit, stale)?;
            nav::source::candidate(
                &source,
                db,
                &snapshot,
                request.expected_state_us,
                request.note,
            )
        })
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(bad)?;
    let anchor = nav::service::price_anchor(candidate, s.market_data.as_deref())
        .await
        .map_err(bad)?;
    let _barrier = s.catalog_lock.lock().await;
    let latest = auth::allowed_source_access(
        &s.pool,
        &user,
        &s.config.source_ids(),
        &s.config.source_viewers,
    )
    .await?;
    if !latest.configure.contains(&id) {
        return Err(forbidden());
    }
    nav::store::insert(&s.pool, &anchor, &user.username)
        .await
        .map_err(bad)?;
    body(anchor)
}
#[derive(serde::Deserialize)]
struct NavQuery {
    start_ms: i64,
    end_ms: i64,
    max_points: Option<usize>,
}
async fn native_nav(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<NavQuery>,
) -> ApiResult {
    if !access.view.contains(&id) {
        return Err(forbidden());
    }
    let (Some(start), Some(end)) = (
        query.start_ms.checked_mul(1000),
        query.end_ms.checked_mul(1000),
    ) else {
        return Err(bad("时间范围溢出"));
    };
    let max_points = query.max_points.unwrap_or(1000);
    if start <= 0
        || end < start
        || end - start > 31 * 86400 * 1_000_000i64
        || !(2..=5000).contains(&max_points)
    {
        return Err(bad(
            "请选择起始快照之后、最多 31 天的时间范围；点数为 2–5000",
        ));
    }
    let Some(anchor) = nav::store::load(&s.pool, &id, start).await? else {
        return body(nav::Report::unavailable(
            &id,
            0,
            start,
            end,
            0,
            "所选起点之前没有原生起始持仓快照；请先建立快照或选择其后的时间范围".into(),
        ));
    };
    let source = s
        .config
        .sources
        .iter()
        .find(|v| v.enabled && v.id == id)
        .cloned()
        .ok_or_else(forbidden)?;
    let rocks = s.rocks.clone();
    let lock = s.readers[&id].clone();
    let mut cache = lock.lock_owned().await;
    let limit = s.config.recent_rows;
    let stale = s.config.stale_secs;
    let read_anchor = anchor.clone();
    let inputs = tokio::task::spawn_blocking(move || {
        reader::with_db(&rocks, &source, |db| {
            let snapshot = cache.read(&source, db, limit, stale)?;
            ensure!(
                snapshot.projection_errors == 0,
                "原生投影存在错误，暂停净值回放"
            );
            let mut through = snapshot
                .nav_anchor_observed_us
                .context("missing complete account observation time")?;
            let fills = nav::source::fills(&source, db, &read_anchor, end)?;
            // Lifecycle/heartbeat records cannot extend financial coverage while
            // reconnect recovery is incomplete. Only account cuts or verified
            // factual live trades establish a later usable account horizon.
            for fill in fills.iter().filter(|fill| fill.live_origin) {
                through = through.max(fill.received_us);
            }
            Ok((fills, through))
        })
    })
    .await
    .map_err(anyhow::Error::from)?;
    let (fills, through) = match inputs {
        Ok(v) => v,
        Err(e) => {
            if e.downcast_ref::<super::sync::ReplicaBusy>().is_some() {
                return Err(e.into());
            }
            tracing::warn!(error=?e,"native NAV source cut unavailable");
            return body(nav::Report::unavailable(
                &id,
                anchor.snapshot_us,
                start,
                end,
                0,
                "原生执行证据不完整、起始证据发生变化或来源世代不匹配；请检查同步和起始快照".into(),
            ));
        }
    };
    body(
        nav::service::timeline(
            &anchor,
            fills,
            s.market_data.as_deref(),
            s.reference_service.as_deref(),
            start,
            end,
            through,
            max_points,
        )
        .await,
    )
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
    let rocks = s.rocks.clone();
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
        let facts = reader::with_db(&rocks, &source, |db| {
            reader::read_cost_facts_from_db(&source, db, &read_day)
        })?;
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
    let _barrier = s.catalog_lock.lock().await;
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
    let managed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cta_native_netting_members WHERE strategy_name=$1)",
    )
    .bind(&r.strategy_name)
    .fetch_one(&s.pool)
    .await
    .map_err(anyhow::Error::from)?;
    if managed {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "该策略属于批量归因组合，请整批提交目标".into(),
        ));
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
