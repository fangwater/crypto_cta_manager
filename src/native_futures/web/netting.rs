//! Authenticated, source-scoped batch and attribution APIs. No order transport.
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
            "/api/native/sources/{id}/netting/release-demands",
            post(release_demands),
        )
        .route("/api/native/sources/{id}/netting/events", get(events))
        .route(
            "/api/native/sources/{id}/netting/fill-allocations",
            post(allocate),
        )
        .route(
            "/api/native/sources/{id}/netting/valuation",
            post(valuation),
        )
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

async fn authorize(
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

async fn observed(s: &Service, id: &str) -> Result<reader::Snapshot, ApiError> {
    let source = s
        .config
        .sources
        .iter()
        .find(|s| s.enabled && s.id == id)
        .cloned()
        .ok_or_else(forbidden)?;
    let rocks = s.rocks.clone();
    let mut guard = s.readers[id].clone().lock_owned().await;
    let stale = s.config.stale_secs;
    tokio::task::spawn_blocking(move || {
        reader::with_db(&rocks, &source, |db| guard.read(&source, db, 1, stale))
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(ApiError::from)
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

fn account_lots(
    snapshot: &reader::Snapshot,
    contracts: &BTreeMap<String, model::Contract>,
) -> Result<model::Quantities, ApiError> {
    let mut quantities = model::Quantities::new();
    for row in &snapshot.positions {
        let market = format!(
            "{}:{}",
            row["venue"].as_str().context("position venue")?,
            row["symbol"].as_str().context("position symbol")?
        );
        if !contracts.contains_key(&market) {
            continue;
        }
        let p = &row["position"];
        let today = p["today"].as_i64().context("position today")?;
        let yesterday = p["yesterday"].as_i64().context("position yesterday")?;
        if today < 0 || yesterday < 0 {
            return Err(bad("negative factual gross position"));
        }
        let sign = match row["side"].as_str() {
            Some("buy") => 1,
            Some("sell") => -1,
            _ => return Err(bad("unknown factual position side")),
        };
        let qty = today
            .checked_add(yesterday)
            .and_then(|q| q.checked_mul(sign))
            .context("gross position overflow")?;
        let value = quantities.entry(market).or_default();
        *value = value.checked_add(qty).context("net position overflow")?;
    }
    Ok(quantities)
}

async fn initialize(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<model::Initialize>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    authorize(&s, &user, &access, &id, request.strategies.iter(), true).await?;
    if let Some(state) = store::load(&s.pool, &id).await? {
        authorize(&s, &user, &access, &id, state.targets.keys(), true).await?;
        return body(
            store::initialize(&s.pool, &state, &request, user.user_id)
                .await
                .map_err(failure)?,
        );
    }
    let snapshot = observed(&s, &id).await?;
    let fresh = snapshot.state_observed_us.is_some_and(|t| {
        t <= crate::native_futures::now_us()
            && crate::native_futures::now_us() - t <= s.config.stale_secs as i64 * 1_000_000
    });
    if !fresh
        || !snapshot.snapshot_ready
        || !snapshot.snapshot_current_runtime
        || snapshot.positions_may_lag
        || snapshot.replication_stale
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "需要完整、当前运行代且无成交滞后的事实持仓快照".into(),
        ));
    }
    if snapshot.state_evidence_sequence != Some(request.snapshot_evidence_sequence) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "持仓证据已变化，请重新读取快照".into(),
        ));
    }
    let quantities = account_lots(&snapshot, &request.contracts)?;
    let note = request.note.clone();
    let expected = request.snapshot_evidence_sequence;
    let anchor = with_source(&s, &id, move |source, db| {
        crate::native_futures::evidence::capture_current(source, db, &snapshot, expected, note)
    })
    .await?;
    let state =
        model::Portfolio::initialize(&id, &request, &quantities, anchor).map_err(failure)?;
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
    body(
        json!({"portfolio":state,"net_targets":state.net_targets()?,"external_demands":state.external_demands(),"execution_enabled":false,
        "account_basis":"explicit_snapshot_allocation_plus_imported_fills"}),
    )
}

async fn batch(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<model::TargetBatch>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    let state = authorized(&s, &user, &access, &id, true).await?;
    if request.strategies.keys().collect::<BTreeSet<_>>()
        != state.targets.keys().collect::<BTreeSet<_>>()
    {
        return Err(bad("批次必须包含全部已登记策略"));
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
    let anchor = state.anchor.clone();
    with_source(&s, &id, move |source, db| {
        crate::native_futures::evidence::validate_anchor(source, db, &anchor)
    })
    .await?;
    body(
        store::batch(&s.pool, &id, &request, user.user_id)
            .await
            .map_err(failure)?,
    )
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

async fn allocate(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<store::AllocateFill>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    let state = authorized(&s, &user, &access, &id, true).await?;
    if let Some(receipt) = store::replay(
        &s.pool,
        &id,
        &format!("fill:{}", request.event_id),
        &request,
    )
    .await
    .map_err(failure)?
    {
        return body(receipt);
    }
    if request.evidence_key.is_empty() || request.evidence_key.len() > 2048 {
        return Err(bad("invalid factual evidence key"));
    }
    let key = hex::decode(&request.evidence_key).map_err(bad)?;
    let anchor = state.anchor.clone();
    let record = with_source(&s, &id, move |source, db| {
        crate::native_futures::evidence::validate_anchor(source, db, &anchor)?;
        let cf = db.cf_handle("cn_trade_facts").context("factual trade CF")?;
        let bytes = db
            .get_cf(cf, key)?
            .context("factual fill evidence not found")?;
        let record: Value =
            serde_json::from_slice(&bytes).context("decode factual trade evidence")?;
        let sequence = record["evidence_seq"]
            .as_i64()
            .context("fill evidence sequence")?;
        anyhow::ensure!(
            crate::native_futures::evidence::live_origin(source, db, &record, sequence)?,
            "recovered or unverifiable trade time cannot establish a post-anchor factual fill"
        );
        Ok(record)
    })
    .await?;
    let fill = model::FactualFill::from_evidence(&record).map_err(failure)?;
    body(
        store::allocate(&s.pool, &id, &request, &fill, user.user_id)
            .await
            .map_err(failure)?,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marks {
    at_us: i64,
    quotes: BTreeMap<String, model::Quote>,
}
async fn valuation(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<Marks>,
) -> ApiResult {
    let state = authorized(&s, &user, &access, &id, false).await?;
    let now = crate::native_futures::now_us();
    if request.at_us > now || now - request.at_us > 120_000_000 {
        return Err(bad("valuation time must be within the last 120 seconds"));
    }
    if !request.quotes.keys().eq(state.contracts.keys()) {
        return Err(bad("valuation requires the complete contract vector"));
    }
    let mut strategies = BTreeMap::new();
    let mut account = BTreeMap::new();
    for (market, contract) in &state.contracts {
        let mark = request.quotes[market].mid(request.at_us).map_err(failure)?;
        account.insert(
            market.clone(),
            state.account[market]
                .valued(mark, contract.multiplier)
                .map_err(failure)?,
        );
    }
    for (name, rows) in &state.positions {
        let mut values = BTreeMap::new();
        for (market, p) in rows {
            values.insert(
                market.clone(),
                p.valued(
                    request.quotes[market].mid(request.at_us).map_err(failure)?,
                    state.contracts[market].multiplier,
                )
                .map_err(failure)?,
            );
        }
        strategies.insert(name.clone(), values);
    }
    body(
        json!({"source_id":id,"revision":state.revision,"at_us":request.at_us,"quotes":request.quotes,
        "basis":"virtual_strategy_attribution","strategies":strategies,"account":account}),
    )
}

async fn release_demands(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(request): Json<store::ReleaseDemand>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    let state = authorized(&s, &user, &access, &id, true).await?;
    if let Some(receipt) = store::replay(
        &s.pool,
        &id,
        &format!("release:{}", request.event_id),
        &request,
    )
    .await
    .map_err(failure)?
    {
        return body(receipt);
    }
    let snapshot = observed(&s, &id).await?;
    let now = crate::native_futures::now_us();
    if !snapshot.snapshot_ready
        || !snapshot.snapshot_current_runtime
        || snapshot.positions_may_lag
        || snapshot.replication_stale
        || !snapshot
            .state_observed_us
            .is_some_and(|t| t <= now && now - t <= s.config.stale_secs as i64 * 1_000_000)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "需要当前、完整且无成交滞后的账户快照，不能释放结果未知的需求".into(),
        ));
    }
    let quantities = account_lots(&snapshot, &state.contracts)?;
    let note = request.note.clone();
    let anchor = state.anchor.clone();
    let proof=with_source(&s,&id,move |source,db| {
        crate::native_futures::evidence::validate_anchor(source,db,&anchor)?;
        let proof=crate::native_futures::evidence::capture_current(source,db,&snapshot,snapshot.state_evidence_sequence.context("snapshot sequence")?,note)?;
        let cf=db.cf_handle("cn_trading_events").context("native state evidence CF")?;
        let bytes=db.get_cf(cf,proof.evidence_sequence.to_be_bytes())?.context("native state evidence")?;
        let value:Value=serde_json::from_slice(&bytes)?;
        anyhow::ensure!(value["value"]["state"]["orders"].as_array().is_some_and(|orders|orders.is_empty()),
            "the source still reports orders or lacks a complete order-free state; do not release unknown outcomes");
        Ok(proof)
    }).await?;
    body(
        store::release(&s.pool, &id, &request, &proof, &quantities, user.user_id)
            .await
            .map_err(failure)?,
    )
}
