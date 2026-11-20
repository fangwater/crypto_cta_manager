//! Authenticated, source-scoped target batch and attribution anchor APIs.
//! Exec owns the live strategy ledger in its uniform-order records; no order
//! transport, counter snapshot or fill import here.
use super::*;
use crate::native_futures::netting::{self as model, store};
use axum::extract::Query;
use serde::Deserialize;
use std::collections::BTreeSet;

pub(super) fn routes() -> Router<Service> {
    Router::new()
        .route(
            "/api/native/sources/{id}/netting",
            get(read).post(initialize),
        )
        .route("/api/native/sources/{id}/target-batches", post(batch))
        .route(
            "/api/native/sources/{id}/netting/extend-scope",
            post(extend_scope),
        )
        .route("/api/native/sources/{id}/netting/rebase", post(rebase))
        .route(
            "/api/native/sources/{id}/netting/execution-plan",
            get(execution_plan),
        )
        .route("/api/native/sources/{id}/netting/events", get(events))
}

fn failure(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<model::Conflict>().is_some() {
        return ApiError(StatusCode::CONFLICT, error.to_string());
    }
    tracing::warn!(error=?error,"native batch/attribution rejected");
    if error.downcast_ref::<sqlx::Error>().is_some() {
        return error.into();
    }
    bad(error)
}

pub(super) async fn authorize(
    s: &Service,
    user: &AuthUser,
    access: &SourceAccess,
    id: &str,
    names: impl Iterator<Item = &String>,
    write: bool,
) -> Result<(), ApiError> {
    if !(if write {
        access.configure.contains(id)
    } else {
        access.view.contains(id)
    }) {
        return Err(forbidden());
    }
    if !s
        .config
        .sources
        .iter()
        .any(|source| source.enabled && source.id == id)
    {
        return Err(forbidden());
    }
    for name in names {
        let rights = strategy_catalog::load_position_access(&s.pool, name)
            .await?
            .ok_or_else(forbidden)?;
        let allowed = if write {
            rights.user_can_configure(user.user_id)
        } else {
            rights.user_can_view(user.user_id)
        };
        if !user.is_admin() && !allowed {
            return Err(forbidden());
        }
    }
    Ok(())
}

async fn authorized(
    s: &Service,
    user: &AuthUser,
    access: &SourceAccess,
    id: &str,
    write: bool,
) -> Result<model::Portfolio, ApiError> {
    // Check account access before any catalog existence or attribution data leak.
    authorize(s, user, access, id, std::iter::empty(), write).await?;
    let state = store::load(&s.pool, id)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "账户尚未初始化策略归因账本".into()))?;
    authorize(s, user, access, id, state.targets.keys(), write).await?;
    Ok(state)
}

async fn with_source<T: Send + 'static>(
    s: &Service,
    id: &str,
    read: impl FnOnce(&crate::native_futures::config::Source, &rocksdb::DB) -> anyhow::Result<T>
    + Send
    + 'static,
) -> Result<T, ApiError> {
    let source = s
        .config
        .sources
        .iter()
        .find(|source| source.enabled && source.id == id)
        .cloned()
        .ok_or_else(forbidden)?;
    let rocks = s.rocks.clone();
    let guard = s.readers[id].clone().lock_owned().await;
    Ok(tokio::task::spawn_blocking(move || {
        reader::with_db(&rocks, &source, |db| {
            let _guard = guard;
            read(&source, db)
        })
    })
    .await
    .map_err(anyhow::Error::from)??)
}

async fn initialize(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<model::Initialize>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    authorize(
        &s,
        &user,
        &access,
        &id,
        request
            .strategy_groups
            .values()
            .flat_map(|members| members.iter()),
        true,
    )
    .await?;
    if let Some(state) = store::load(&s.pool, &id).await? {
        authorize(&s, &user, &access, &id, state.targets.keys(), true).await?;
        return body(
            store::initialize(&s.pool, &state, &request, user.user_id)
                .await
                .map_err(failure)?,
        );
    }
    let note = request.note.clone();
    let (anchor, exec_ledger) = with_source(&s, &id, move |source, db| {
        model::Anchor::capture(source, db, note)
    })
    .await?;
    let state =
        model::Portfolio::initialize(&id, &request, &exec_ledger, anchor).map_err(failure)?;
    body(
        store::initialize(&s.pool, &state, &request, user.user_id)
            .await
            .map_err(failure)?,
    )
}

async fn read(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    let state = authorized(&s, &user, &access, &id, false).await?;
    // Live strategy positions fold Exec's records, as crypto reads Exec RocksDB.
    let exec_ledger = with_source(&s, &id, |_, db| {
        crate::native_futures::records::strategy_positions(db, i64::MAX)
    })
    .await?;
    body(
        json!({"portfolio":state,"net_targets":state.net_targets()?,"exec_ledger":exec_ledger,
        "account_basis":"portfolio.positions are the anchor allocation; exec_ledger folds Exec's cn_uniform_orders"}),
    )
}

async fn rebase(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<model::Rebase>,
) -> ApiResult {
    if !user.is_admin() {
        return Err(forbidden());
    }
    model::valid_id(&request.event_id).map_err(bad)?;
    let state = authorized(&s, &user, &access, &id, true).await?;
    if let Some(receipt) = store::replay(
        &s.pool,
        &id,
        &format!("rebase:{}", request.event_id),
        &request,
    )
    .await
    .map_err(failure)?
    {
        return body(receipt);
    }
    model::require(
        state.revision == request.expected_revision,
        "portfolio has changed",
    )
    .map_err(failure)?;

    let note = request.note.clone();
    let trading_day = request.trading_day.clone();
    let inventory = request.inventory.clone();
    let (candidate, anchor, exec_ledger) = with_source(&s, &id, move |source, db| {
        let (anchor, exec_ledger) = model::Anchor::capture(source, db, note.clone())?;
        let mut candidate = nav::source::candidate(source, db, trading_day, inventory, note)?;
        candidate.anchor.snapshot_us = anchor.snapshot_us;
        Ok((candidate, anchor, exec_ledger))
    })
    .await?;
    let nav = nav::service::price_anchor(candidate, s.market_data.as_deref())
        .await
        .map_err(bad)?;
    let _guard = s.catalog_lock.lock().await;
    let latest_access = auth::allowed_source_access(
        &s.pool,
        &user,
        &s.config.source_ids(),
        &s.config.source_viewers,
    )
    .await?;
    authorized(&s, &user, &latest_access, &id, true).await?;
    let receipt = store::rebase(
        &s.pool,
        &id,
        &request,
        &nav,
        anchor,
        &exec_ledger,
        user.user_id,
        &user.username,
    )
    .await
    .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
}

async fn extend_scope(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<model::ExtendScope>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    let state = authorized(&s, &user, &access, &id, true).await?;
    authorize(&s, &user, &access, &id, request.members.iter(), true).await?;
    model::valid_id(&request.event_id).map_err(bad)?;
    if let Some(receipt) = store::replay(
        &s.pool,
        &id,
        &format!("scope:{}", request.event_id),
        &request,
    )
    .await
    .map_err(failure)?
    {
        return body(receipt);
    }
    let note = request.note.clone();
    let current = state.anchor;
    let (anchor, exec_ledger) = with_source(&s, &id, move |source, db| {
        current.verify(source, db)?;
        model::Anchor::capture(source, db, note)
    })
    .await?;
    let receipt = store::extend_scope(&s.pool, &id, &request, &anchor, &exec_ledger, user.user_id)
        .await
        .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
}

async fn batch(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    scope: Option<Extension<super::netting_machine::PublisherScope>>,
    Json(request): Json<model::TargetBatch>,
) -> ApiResult {
    if scope.is_some_and(|Extension(scope)| scope.strategy_group != request.strategy_group) {
        return Err(forbidden());
    }
    let _guard = s.catalog_lock.lock().await;
    authorize(&s, &user, &access, &id, std::iter::empty(), true).await?;
    let state = store::load(&s.pool, &id)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "账户尚未初始化策略归因账本".into()))?;
    let group = state
        .strategy_groups
        .get(&request.strategy_group)
        .ok_or_else(|| bad("未知策略组"))?;
    authorize(&s, &user, &access, &id, group.members.iter(), true).await?;
    if request.strategies.keys().collect::<BTreeSet<_>>()
        != group.members.iter().collect::<BTreeSet<_>>()
    {
        return Err(bad("批次必须包含该组全部已登记策略"));
    }
    if let Some(receipt) = store::replay(
        &s.pool,
        &id,
        &format!("batch:{}", request.batch_id),
        &request,
    )
    .await
    .map_err(failure)?
    {
        return body(receipt);
    }
    // Like crypto targets, acceptance never waits on the Exec replica: Exec
    // checks the trading day and account itself when it admits the target.
    let receipt = store::batch(&s.pool, &id, &request, user.user_id)
        .await
        .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    #[serde(default = "before_first")]
    after: i64,
    #[serde(default = "page_size")]
    limit: i64,
}
fn before_first() -> i64 {
    -1
}
fn page_size() -> i64 {
    50
}
async fn events(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Query(query): Query<History>,
) -> ApiResult {
    authorized(&s, &user, &access, &id, false).await?;
    body(
        store::history(&s.pool, &id, query.after, query.limit)
            .await
            .map_err(failure)?,
    )
}

async fn execution_plan(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    let state = authorized(&s, &user, &access, &id, false).await?;
    body(state.execution_plan()?)
}
