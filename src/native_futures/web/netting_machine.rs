//! A machine token is bound to one source, strategy group and current user.
use super::*;
use crate::native_futures::netting::store;
use axum::extract::Query;
use serde::Deserialize;
use sqlx::Row;

#[derive(Clone)]
pub(super) struct PublisherScope {
    pub strategy_group: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    strategy_group: String,
}

pub(super) fn routes() -> Router<Service> {
    Router::new()
        .route(
            "/api/native/sources/{id}/netting/publication",
            get(publication),
        )
        .route(
            "/api/native/sources/{id}/netting/publish-token",
            post(mint).delete(revoke),
        )
}

pub(super) async fn authenticate_machine(
    s: &Service,
    headers: &HeaderMap,
    method: &axum::http::Method,
    request_path: &str,
) -> Result<(AuthUser, SourceAccess, PublisherScope), ApiError> {
    let path = request_path
        .strip_prefix("/manager")
        .unwrap_or(request_path);
    let (source, suffix) = path
        .strip_prefix("/api/native/sources/")
        .and_then(|p| p.split_once('/'))
        .ok_or_else(forbidden)?;
    let allowed = (method == axum::http::Method::GET && suffix == "netting/publication")
        || (method == axum::http::Method::POST && suffix == "target-batches");
    if !allowed {
        return Err(forbidden());
    }
    let token = headers
        .get("X-CTA-Publish-Token")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "invalid publisher token".into()))?;
    let row=sqlx::query("SELECT p.strategy_group,u.user_id,u.username,u.role FROM cta_native_netting_publishers p JOIN cta_users u ON u.user_id=p.actor_user_id WHERE p.source_id=$1 AND p.token_hash=$2 AND NOT u.disabled")
        .bind(source).bind(auth::publish_token_hash(token)).fetch_optional(&s.pool).await.map_err(anyhow::Error::from)?
        .ok_or_else(||ApiError(StatusCode::UNAUTHORIZED,"invalid publisher token".into()))?;
    let user = AuthUser {
        user_id: row.try_get("user_id").map_err(anyhow::Error::from)?,
        username: row.try_get("username").map_err(anyhow::Error::from)?,
        role: row.try_get("role").map_err(anyhow::Error::from)?,
    };
    let access = auth::allowed_source_access(
        &s.pool,
        &user,
        &s.config.source_ids(),
        &s.config.source_viewers,
    )
    .await?;
    let scope = PublisherScope {
        strategy_group: row.try_get("strategy_group").map_err(anyhow::Error::from)?,
    };
    Ok((user, access, scope))
}

async fn publication(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Query(selection): Query<Selection>,
    scope: Option<Extension<PublisherScope>>,
) -> ApiResult {
    if scope.is_some_and(|Extension(scope)| scope.strategy_group != selection.strategy_group) {
        return Err(forbidden());
    }
    super::netting::authorize(&s, &user, &access, &id, std::iter::empty(), true).await?;
    let state = store::load(&s.pool, &id)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "账户尚未初始化归因组合".into()))?;
    let group = state
        .strategy_groups
        .get(&selection.strategy_group)
        .ok_or_else(forbidden)?;
    super::netting::authorize(&s, &user, &access, &id, group.members.iter(), true).await?;
    body(
        json!({"source_id":id,"strategy_group":selection.strategy_group,"revision":state.revision,
        "members":group.members,"contracts":state.contracts.keys().collect::<Vec<_>>(),"group_decision_us":group.decision_us}),
    )
}

async fn mint(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(selection): Json<Selection>,
) -> ApiResult {
    let _barrier = s.catalog_lock.lock().await;
    super::netting::authorize(&s, &user, &access, &id, std::iter::empty(), true).await?;
    let state = store::load(&s.pool, &id).await?.ok_or_else(forbidden)?;
    let group = state
        .strategy_groups
        .get(&selection.strategy_group)
        .ok_or_else(forbidden)?;
    super::netting::authorize(&s, &user, &access, &id, group.members.iter(), true).await?;
    let token = auth::generate_publish_token();
    sqlx::query("INSERT INTO cta_native_netting_publishers(source_id,strategy_group,actor_user_id,token_hash,created_at_us) VALUES($1,$2,$3,$4,$5) ON CONFLICT(source_id,strategy_group) DO UPDATE SET actor_user_id=EXCLUDED.actor_user_id,token_hash=EXCLUDED.token_hash,created_at_us=EXCLUDED.created_at_us")
        .bind(&id).bind(&selection.strategy_group).bind(user.user_id).bind(auth::publish_token_hash(&token)).bind(crate::native_futures::now_us()).execute(&s.pool).await.map_err(anyhow::Error::from)?;
    body(json!({"source_id":id,"strategy_group":selection.strategy_group,"publish_token":token}))
}

async fn revoke(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(selection): Json<Selection>,
) -> ApiResult {
    let _barrier = s.catalog_lock.lock().await;
    super::netting::authorize(&s, &user, &access, &id, std::iter::empty(), true).await?;
    let state = store::load(&s.pool, &id).await?.ok_or_else(forbidden)?;
    let group = state
        .strategy_groups
        .get(&selection.strategy_group)
        .ok_or_else(forbidden)?;
    super::netting::authorize(&s, &user, &access, &id, group.members.iter(), true).await?;
    sqlx::query(
        "DELETE FROM cta_native_netting_publishers WHERE source_id=$1 AND strategy_group=$2",
    )
    .bind(&id)
    .bind(&selection.strategy_group)
    .execute(&s.pool)
    .await
    .map_err(anyhow::Error::from)?;
    body(json!({"revoked":true}))
}
