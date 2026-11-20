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
            "/api/native/sources/{id}/netting/extend-scope",
            post(extend_scope),
        )
        .route("/api/native/sources/{id}/netting/rebase", post(rebase))
        .route(
            "/api/native/sources/{id}/netting/execution-plan",
            get(execution_plan),
        )
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
            "/api/native/sources/{id}/netting/ctaker-fill-allocations",
            post(allocate_ctaker),
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

pub(super) async fn observed(s: &Service, id: &str) -> Result<reader::Snapshot, ApiError> {
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

pub(super) fn account_lots(
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
    let snapshot = observed(&s, &id).await?;
    let fresh = snapshot.state_observed_us.is_some_and(|t| {
        t <= crate::native_futures::now_us()
            && crate::native_futures::now_us() - t <= s.config.stale_secs as i64 * 1_000_000
    });
    if !fresh
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
        state.revision == request.expected_revision && !state.external_demand_open,
        "portfolio has changed or still has unsettled execution",
    )
    .map_err(failure)?;

    let snapshot = observed(&s, &id).await?;
    let expected = request.snapshot_evidence_sequence;
    check_rebase_snapshot(&snapshot, expected, s.config.stale_secs)?;
    let at = snapshot
        .state_observed_us
        .context("missing current observation time")?;
    let note = request.note.clone();
    let candidate = with_source(&s, &id, move |source, db| {
        use crate::native_futures::evidence;
        let proof = evidence::capture_current(source, db, &snapshot, expected, note.clone())?;
        let row: Value = serde_json::from_slice(&evidence::journal_entry(db, expected)?)?;
        model::require(
            evidence::no_unsettled_orders(&row["value"]["state"]),
            "rebase requires an order-free broker snapshot",
        )?;
        nav::source::candidate(source, db, &snapshot, proof.snapshot_us, note)
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
    let latest = observed(&s, &id).await?;
    check_rebase_snapshot(&latest, expected, s.config.stale_secs)?;
    if latest.state_observed_us != Some(at) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "柜台事实快照已变化，请重新读取".into(),
        ));
    }
    let receipt = store::rebase(&s.pool, &id, &request, &nav, user.user_id, &user.username)
        .await
        .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
}

fn check_rebase_snapshot(
    snapshot: &reader::Snapshot,
    expected: i64,
    stale_secs: u64,
) -> Result<(), ApiError> {
    let now = crate::native_futures::now_us();
    model::require(
        snapshot.state_evidence_sequence == Some(expected)
            && snapshot.nav_anchor_evidence_sequence == Some(expected)
            && snapshot.snapshot_current_runtime
            && !snapshot.positions_may_lag
            && !snapshot.replication_stale
            && snapshot
                .state_observed_us
                .is_some_and(|at| at <= now && now - at <= stale_secs as i64 * 1_000_000),
        "rebase requires the current, complete, synchronized account snapshot",
    )
    .map_err(failure)
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
    let snapshot = observed(&s, &id).await?;
    let now = crate::native_futures::now_us();
    if !snapshot
        .state_observed_us
        .is_some_and(|t| t <= now && now - t <= s.config.stale_secs as i64 * 1_000_000)
        || snapshot.state_evidence_sequence != Some(request.snapshot_evidence_sequence)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "需要当前、完整且未变更的账户快照".into(),
        ));
    }
    let note = request.note.clone();
    let current_anchor = state.anchor;
    let contracts = request.contracts.clone();
    let sequence = request.snapshot_evidence_sequence;
    let proof = with_source(&s, &id, move |source, db| {
        use crate::native_futures::evidence::{
            capture_current, no_unsettled_orders, validate_anchor,
        };
        validate_anchor(source, db, &current_anchor)?;
        let proof = capture_current(source, db, &snapshot, sequence, note)?;
        for row in &snapshot.positions {
            let market = format!(
                "{}:{}",
                row["venue"].as_str().context("position venue")?,
                row["symbol"].as_str().context("position symbol")?
            );
            if contracts.contains_key(&market) {
                model::require(
                    row["position"]["today"].as_i64() == Some(0)
                        && row["position"]["yesterday"].as_i64() == Some(0),
                    "new contracts must have no gross factual positions",
                )?;
            }
        }
        let cf = db
            .cf_handle("cn_trading_events")
            .context("native state evidence CF")?;
        let bytes = db
            .get_cf(cf, sequence.to_be_bytes())?
            .context("native state evidence")?;
        let value: Value = serde_json::from_slice(&bytes)?;
        model::require(
            no_unsettled_orders(&value["value"]["state"]),
            "scope extension requires a snapshot with no working orders or unsettled executions",
        )?;
        Ok(proof)
    })
    .await?;
    let receipt = store::extend_scope(&s.pool, &id, &request, &proof, user.user_id)
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
    if let Some(day) = &request.trading_day {
        let snapshot = observed(&s, &id).await?;
        if !snapshot.snapshot_current_runtime
            || snapshot.replication_stale
            || snapshot.trading_day.as_ref() != Some(day)
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "批次交易日与当前事实账户不一致".into(),
            ));
        }
    }
    let anchor = state.anchor.clone();
    with_source(&s, &id, move |source, db| {
        crate::native_futures::evidence::validate_anchor(source, db, &anchor)
    })
    .await?;
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
    let (fill, proof) = read_fill(&s, &id, state.anchor, &request.evidence_key).await?;
    let receipt = store::allocate(&s.pool, &id, &request, &fill, user.user_id, proof.as_ref())
        .await
        .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
}

async fn allocate_ctaker(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Json(mut request): Json<store::AllocateCtakerFill>,
) -> ApiResult {
    let _guard = s.catalog_lock.lock().await;
    let state = authorized(&s, &user, &access, &id, true).await?;
    request.evidence_key.make_ascii_lowercase();
    if let Some(receipt) = store::replay(&s.pool, &id, &request.id()?, &request)
        .await
        .map_err(failure)?
    {
        return body(receipt);
    }
    let (fill, proof) = read_fill(&s, &id, state.anchor, &request.evidence_key).await?;
    let proof = proof.ok_or_else(|| bad("成交缺少可核验的 ctaker 原生报单证据"))?;
    let receipt = store::allocate_ctaker(&s.pool, &id, &request, &fill, &proof, user.user_id)
        .await
        .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
}

async fn read_fill(
    s: &Service,
    id: &str,
    anchor: crate::native_futures::evidence::Anchor,
    key: &str,
) -> Result<
    (
        model::FactualFill,
        Option<crate::native_futures::evidence::CtakerFillProof>,
    ),
    ApiError,
> {
    if key.is_empty() || key.len() > 2048 {
        return Err(bad("invalid factual evidence key"));
    }
    let key = hex::decode(key).map_err(bad)?;
    with_source(s, id, move |source, db| {
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
        let proof = crate::native_futures::evidence::ctaker_fill_proof(source, db, &record)?;
        Ok((model::FactualFill::from_evidence(&record)?, proof))
    })
    .await
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
    if !snapshot.snapshot_current_runtime
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
        anyhow::ensure!(crate::native_futures::evidence::no_unsettled_orders(&value["value"]["state"]),
            "the source still reports active orders or unsettled executions; do not release unknown outcomes");
        Ok(proof)
    }).await?;
    let receipt = store::release(&s.pool, &id, &request, &proof, &quantities, user.user_id)
        .await
        .map_err(failure)?;
    s.plan_changed(&id);
    body(receipt)
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
