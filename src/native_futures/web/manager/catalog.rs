use super::*;
use crate::{postgres, shares_history, virtual_accounts};
use axum::extract::Query;
use serde::Deserialize;
use std::collections::BTreeSet;

pub(super) fn routes() -> Router<Service> {
    Router::new()
        .route("/api/catalog/accounts/{id}", get(account))
        .route("/api/catalog/accounts/{id}/alias", put(account_alias))
        .route(
            "/api/catalog/accounts/{id}/contract-leverage",
            get(no_contract_leverage).put(no_contract_leverage),
        )
        .route("/api/catalog/accounts/{id}/fee-rates", put(no_flat_fees))
        .route(
            "/api/catalog/accounts/{id}/configuration",
            put(no_account_follow),
        )
        .route(
            "/api/catalog/accounts/{id}/shares-history",
            get(account_shares_history),
        )
        .route(
            "/api/catalog/accounts/{id}/grants",
            get(source_grants).put(save_source_grants),
        )
        .route("/api/auth/users/{id}/sources", put(user_sources))
        .route(
            "/api/catalog/position-strategies",
            get(position_strategies).post(save_position),
        )
        .route(
            "/api/catalog/position-strategies-access",
            get(position_access),
        )
        .route(
            "/api/catalog/position-strategies/{name}",
            axum::routing::delete(delete_position),
        )
        .route(
            "/api/catalog/position-strategies/{name}/grants",
            put(save_position_grants),
        )
        .route(
            "/api/catalog/position-strategies/{name}/managers",
            put(save_position_managers),
        )
        .route(
            "/api/catalog/position-strategies/{name}/publish-token",
            put(no_position_token),
        )
        .route(
            "/api/catalog/position-strategies/{name}/publish-token/reset",
            post(no_position_token),
        )
        .route(
            "/api/catalog/order-strategies",
            get(order_strategies).post(save_order_strategy),
        )
        .route(
            "/api/catalog/order-strategies/{name}",
            axum::routing::delete(delete_order_strategy),
        )
        .route(
            "/api/catalog/virtual-accounts",
            get(virtual_list).post(create_virtual),
        )
        .route("/api/catalog/virtual-accounts/users", get(virtual_users))
        .route(
            "/api/catalog/virtual-accounts/{id}",
            put(save_virtual).delete(delete_virtual),
        )
        .route(
            "/api/catalog/virtual-accounts/{id}/grants",
            put(save_virtual_grants),
        )
        .route(
            "/api/account-overview/external-access",
            get(empty_admin_catalog),
        )
        .route(
            "/api/catalog/publish-tokens",
            get(empty_admin_catalog).post(no_fallback_token),
        )
        .route(
            "/api/catalog/publish-tokens/{id}",
            axum::routing::delete(no_fallback_token),
        )
}

/// Portfolio ownership limits catalog visibility as well as the native read API.
pub(in super::super) struct Scope<'a> {
    user: &'a AuthUser,
    access: &'a SourceAccess,
    strategies: BTreeMap<String, strategy_catalog::PositionVisibility>,
    members: BTreeMap<String, String>,
}

impl<'a> Scope<'a> {
    pub(in super::super) async fn load(
        s: &Service,
        access: &'a SourceAccess,
        user: &'a AuthUser,
    ) -> Result<Self> {
        let members: Vec<(String, String)> =
            sqlx::query_as("SELECT strategy_name, source_id FROM cta_native_netting_members")
                .fetch_all(&s.pool)
                .await?;
        Ok(Self {
            user,
            access,
            strategies: strategy_catalog::list_position_visibility(&s.pool)
                .await?
                .into_iter()
                .map(|entry| (entry.strategy_name.clone(), entry))
                .collect(),
            members: members.into_iter().collect(),
        })
    }

    pub(in super::super) fn can_view(&self, name: &str) -> bool {
        self.members
            .get(name)
            .is_none_or(|source| self.access.view.contains(source))
            && self
                .strategies
                .get(name)
                .is_some_and(|entry| self.user.is_admin() || entry.user_can_view(self.user.user_id))
    }

    pub(in super::super) fn can_configure(&self, name: &str) -> bool {
        self.members
            .get(name)
            .is_none_or(|source| self.access.configure.contains(source))
            && self.strategies.get(name).is_some_and(|entry| {
                self.user.is_admin() || entry.user_can_configure(self.user.user_id)
            })
    }

    pub(in super::super) fn can_edit_targets(&self, name: &str) -> bool {
        self.can_configure(name) && !self.members.contains_key(name)
    }
}

async fn account(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let (guard, user, access) = catalog_actor(&s, &headers).await?;
    get_source(&s, &access, &id)?;
    let scope = Scope::load(&s, &access, &user).await?;
    let portfolio = crate::native_futures::netting::store::load(&s.pool, &id).await?;
    drop(guard);
    let mut bindings = Vec::new();
    let mut netting = Value::Null;
    let mut deleted_members = Vec::new();
    if let Some(portfolio) = portfolio {
        for (group_name, group) in &portfolio.strategy_groups {
            for name in group.members.iter().filter(|name| scope.can_view(name)) {
                bindings.push(json!({
                    "source_id": id, "binding_name": name, "position_strategy_name": name,
                    "order_strategy_name": if s.config.exec_plan_readers.contains_key(&id) { "exec" } else { "" },
                    "shares": null, "updated_at_us": group.decision_us,
                    "strategy_group": group_name, "native_stopped": portfolio.stopped.contains(name),
                }));
            }
        }
        // 删除配置 keeps the name in this account; 重新启用 adds it back to its group.
        let deleted: Vec<_> = portfolio
            .retired
            .iter()
            .filter(|name| scope.can_view(name))
            .collect();
        let groups: BTreeMap<String, String> = sqlx::query_as(
            "SELECT strategy_name,strategy_group FROM cta_native_netting_members WHERE source_id=$1 AND strategy_name = ANY($2)",
        )
        .bind(&id)
        .bind(&deleted)
        .fetch_all(&s.pool)
        .await
        .map_err(anyhow::Error::from)?
        .into_iter()
        .collect();
        for name in deleted {
            deleted_members.push(json!({
                "position_strategy_name": name,
                "strategy_group": groups.get(name),
            }));
        }
        netting = json!({ "revision": portfolio.revision, "deleted": deleted_members });
    }
    body(json!({
        "source_id": id, "bindings": bindings, "configuration": { "mode": "independent" }, "pending_publishes": [],
        "native_netting": netting,
        "estimated_fee_rate": null, "maker_fee_rate": null, "taker_fee_rate": null,
        "theoretical_twap_fee_rate": null, "quote_fee_rates": {},
    }))
}

async fn account_shares_history(
    State(s): State<Service>,
    Extension(user): Extension<AuthUser>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
    Query(query): Query<shares_history::HistoryQuery>,
) -> ApiResult {
    get_source(&s, &access, &id)?;
    query.validate().map_err(bad)?;
    let scope = Scope::load(&s, &access, &user).await?;
    if query
        .strategy_name
        .as_ref()
        .is_some_and(|name| !scope.can_view(name))
    {
        return Err(forbidden());
    }
    // The native allocator stores explicit lots per member. Its event
    // records contain no share adjustments, so there is no shares ledger.
    body(shares_history::SharesHistory {
        current: vec![],
        entries: vec![],
        next_cursor: None,
    })
}

async fn no_contract_leverage(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    method: axum::http::Method,
    Path(id): Path<String>,
) -> ApiResult {
    get_source(&s, &access, &id)?;
    if method == axum::http::Method::PUT && !access.configure.contains(&id) {
        return Err(forbidden());
    }
    Err(ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "国内期货按合约保证金率管理，账户没有可查询或设置的合约杠杆倍数".into(),
    ))
}

async fn no_flat_fees(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    get_source(&s, &access, &id)?;
    if !access.configure.contains(&id) {
        return Err(forbidden());
    }
    Err(ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "国内期货手续费按账户、合约、开平及生效时间核算".into(),
    ))
}

async fn no_account_follow(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Path(id): Path<String>,
) -> ApiResult {
    get_source(&s, &access, &id)?;
    if !access.configure.contains(&id) {
        return Err(forbidden());
    }
    Err(ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "国内期货账户的策略按账户和归因组整批配置".into(),
    ))
}

#[derive(Deserialize)]
struct Alias {
    alias: Option<String>,
}

async fn account_alias(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<Alias>,
) -> ApiResult {
    let (_guard, _, access) = catalog_actor(&s, &headers).await?;
    if !access.configure.contains(&id) {
        return Err(forbidden());
    }
    let source = get_source(&s, &access, &id)?;
    let alias = postgres::normalize_source_alias(request.alias.as_deref()).map_err(bad)?;
    postgres::save_source_alias(&s.pool, &id, alias.as_deref()).await?;
    body(
        json!({"source_id": id, "account": alias.as_deref().unwrap_or(&source.account), "display_alias": alias}),
    )
}

async fn user_sources(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(request): Json<auth::SetSourcesRequest>,
) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    if !user.is_admin()
        || request
            .grants
            .iter()
            .any(|grant| !access.configure.contains(grant.source_id.trim()))
    {
        return Err(forbidden());
    }
    let mut result = auth::set_sources(&s.pool, user_id, &request.grants, &access.configure)
        .await
        .map_err(bad)?;
    result
        .source_grants
        .retain(|grant| access.configure.contains(&grant.source_id));
    body(result)
}

async fn position_strategies(State(s): State<Service>, headers: HeaderMap) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    let scope = Scope::load(&s, &access, &user).await?;
    let mut rows = Vec::new();
    for mut strategy in strategy_catalog::list_position_strategies(&s.pool).await? {
        let name = &strategy.strategy_name;
        if scope.can_view(name) {
            let can_publish = scope.can_edit_targets(name);
            strategy.symbol_order_strategy_overrides.clear();
            let mut row = serde_json::to_value(strategy).map_err(anyhow::Error::from)?;
            row["can_publish"] = Value::Bool(can_publish);
            rows.push(row);
        }
    }
    body(rows)
}

async fn save_position(
    State(s): State<Service>,
    headers: HeaderMap,
    Json(request): Json<SavePositionStrategyRequest>,
) -> ApiResult {
    let saved = persist_position(&s, &headers, &request).await?;
    let mut row = serde_json::to_value(saved).map_err(anyhow::Error::from)?;
    row["can_publish"] = Value::Bool(true);
    body(row)
}

pub(in super::super) async fn persist_position(
    s: &Service,
    headers: &HeaderMap,
    request: &SavePositionStrategyRequest,
) -> Result<strategy_catalog::PositionStrategy, ApiError> {
    super::super::validate_strategy(request).map_err(bad)?;
    let (_guard, user, access) = catalog_actor(s, headers).await?;
    let mut scope = Scope::load(s, &access, &user).await?;
    let name = &request.strategy_name;
    let now = crate::native_futures::now_us();
    if !scope.strategies.contains_key(name) {
        strategy_catalog::create_position_strategy(&s.pool, request, user.user_id, now).await?;
        scope = Scope::load(s, &access, &user).await?;
    }
    if !scope.can_configure(name) {
        return Err(forbidden());
    }
    if scope.members.contains_key(name) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "该策略属于批量归因组合，请整批提交目标".into(),
        ));
    }
    Ok(
        strategy_catalog::store_position_strategy(&s.pool, request, Some(user.user_id), now)
            .await?,
    )
}

async fn delete_position(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    if !user.is_admin() {
        return Err(forbidden());
    }
    let scope = Scope::load(&s, &access, &user).await?;
    if !scope.can_configure(&name) {
        return Err(forbidden());
    }
    if scope.members.contains_key(&name) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "该策略属于国内期货归因组合".into(),
        ));
    }
    strategy_catalog::delete_position_strategy(&s.pool, &name)
        .await
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn position_access(State(s): State<Service>, headers: HeaderMap) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    let scope = Scope::load(&s, &access, &user).await?;
    let mut rows = strategy_catalog::list_position_access(&s.pool).await?;
    rows.retain(|row| scope.can_configure(&row.strategy_name));
    for row in &mut rows {
        row.publish_token_set = false;
    }
    body(rows)
}

pub(in super::super) async fn position_access_view(
    s: &Service,
    name: &str,
) -> Result<strategy_catalog::PositionAccessView, ApiError> {
    let mut row = strategy_catalog::position_access_view(&s.pool, name)
        .await?
        .ok_or_else(forbidden)?;
    row.publish_token_set = false;
    Ok(row)
}

async fn save_position_grants(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<strategy_catalog::SavePositionGrantsRequest>,
) -> ApiResult {
    body(persist_position_grants(&s, &headers, &name, request).await?)
}

pub(in super::super) async fn persist_position_grants(
    s: &Service,
    headers: &HeaderMap,
    name: &str,
    request: strategy_catalog::SavePositionGrantsRequest,
) -> Result<strategy_catalog::PositionAccessView, ApiError> {
    let (_guard, user, access) = catalog_actor(s, headers).await?;
    let scope = Scope::load(s, &access, &user).await?;
    if !scope.can_configure(name) {
        return Err(forbidden());
    }
    let rights = strategy_catalog::load_position_access(&s.pool, name)
        .await?
        .ok_or_else(forbidden)?;
    if !user.is_admin() {
        let keeps_configure = rights.user_can_publish(user.user_id)
            || request.grants.iter().any(|grant| {
                grant.user_id == user.user_id && grant.access_level.trim() == "configure"
            });
        if !rights.user_can_manage_grants(user.user_id)
            || request.open_visibility != rights.open_visibility
            || !keeps_configure
        {
            return Err(forbidden());
        }
    }
    strategy_catalog::set_position_grants(&s.pool, name, &request.grants, request.open_visibility)
        .await
        .map_err(bad)?;
    position_access_view(s, name).await
}

async fn save_position_managers(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<strategy_catalog::SavePositionManagersRequest>,
) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    if !user.is_admin() {
        return Err(forbidden());
    }
    if !Scope::load(&s, &access, &user).await?.can_configure(&name) {
        return Err(forbidden());
    }
    strategy_catalog::set_position_managers(&s.pool, &name, &request.user_ids)
        .await
        .map_err(bad)?;
    body(position_access_view(&s, &name).await?)
}

async fn no_position_token(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    let scope = Scope::load(&s, &access, &user).await?;
    if !scope.can_configure(&name) {
        return Err(forbidden());
    }
    Err(native_token_error())
}

async fn order_strategies(State(s): State<Service>, headers: HeaderMap) -> ApiResult {
    let (_guard, user, _) = catalog_actor(&s, &headers).await?;
    let owners = strategy_catalog::list_order_strategy_owners(&s.pool).await?;
    let mut rows = Vec::new();
    for strategy in strategy_catalog::list_order_strategies(&s.pool).await? {
        if let Some(owner) = owners
            .get(&strategy.strategy_name)
            .filter(|owner| owner.can_configure(&user))
        {
            let mut row = serde_json::to_value(strategy).map_err(anyhow::Error::from)?;
            row["owner"] = json!(owner.username);
            row["can_configure"] = Value::Bool(true);
            rows.push(row);
        }
    }
    body(rows)
}

async fn save_order_strategy(
    State(s): State<Service>,
    headers: HeaderMap,
    Json(request): Json<strategy_catalog::SaveOrderStrategyRequest>,
) -> ApiResult {
    let (_guard, user, _) = catalog_actor(&s, &headers).await?;
    let owners = strategy_catalog::list_order_strategy_owners(&s.pool).await?;
    if owners
        .get(&request.strategy_name)
        .is_some_and(|owner| !owner.can_configure(&user))
    {
        return Err(forbidden());
    }
    let saved = strategy_catalog::upsert_order_strategy(
        &s.pool,
        &request,
        crate::native_futures::now_us(),
        Some(user.user_id),
    )
    .await
    .map_err(bad)?;
    let mut row = serde_json::to_value(saved).map_err(anyhow::Error::from)?;
    row["owner"] = json!(
        owners
            .get(&request.strategy_name)
            .map_or(Some(user.username.as_str()), |owner| owner
                .username
                .as_deref())
    );
    row["can_configure"] = Value::Bool(true);
    body(row)
}

async fn delete_order_strategy(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (_guard, user, _) = catalog_actor(&s, &headers).await?;
    let owners = strategy_catalog::list_order_strategy_owners(&s.pool).await?;
    if !owners
        .get(&name)
        .is_some_and(|owner| owner.can_configure(&user))
    {
        return Err(forbidden());
    }
    strategy_catalog::delete_order_strategy(&s.pool, &name)
        .await
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn visible_virtuals(
    s: &Service,
    user: &AuthUser,
    access: &SourceAccess,
) -> Result<Vec<virtual_accounts::VirtualAccount>> {
    let scope = Scope::load(s, access, user).await?;
    let orders = strategy_catalog::list_order_strategy_owners(&s.pool).await?;
    let mut accounts = virtual_accounts::list(&s.pool).await?;
    let mut followed: BTreeSet<_> = accounts
        .iter()
        .filter(|account| !account.followers.is_empty())
        .map(|account| account.virtual_id.clone())
        .collect();
    loop {
        let children: Vec<_> = accounts
            .iter()
            .filter(|account| followed.contains(&account.virtual_id))
            .flat_map(|account| {
                account
                    .children
                    .iter()
                    .map(|child| child.virtual_id.clone())
            })
            .collect();
        let before = followed.len();
        followed.extend(children);
        if followed.len() == before {
            break;
        }
    }
    accounts.retain_mut(|account| {
        account.permissions_for(user);
        // Virtual administration does not grant access to a leaf portfolio.
        account.expanded_bindings.iter().all(|binding| {
            scope.can_view(&binding.position_strategy_name)
                && orders
                    .get(&binding.order_strategy_name)
                    .is_some_and(|owner| owner.can_configure(user))
        }) && (account.can_configure || !account.expanded_bindings.is_empty())
    });
    // A child may be private even when it contains no leaves. Hide a parent
    // whose full composition cannot be displayed or safely submitted back.
    loop {
        let ids: BTreeSet<_> = accounts.iter().map(|a| a.virtual_id.clone()).collect();
        let before = accounts.len();
        accounts.retain(|a| {
            a.children
                .iter()
                .all(|child| ids.contains(&child.virtual_id))
        });
        if accounts.len() == before {
            break;
        }
    }
    let ids: BTreeSet<_> = accounts.iter().map(|a| a.virtual_id.clone()).collect();
    for account in &mut accounts {
        account.can_configure &= !followed.contains(&account.virtual_id);
        account.parents.retain(|id| ids.contains(id));
        account
            .followers
            .retain(|follower| access.view.contains(&follower.source_id));
        for follower in &mut account.followers {
            follower.pending_publishes.clear();
        }
        if !account.can_manage_grants && !account.can_configure {
            account.managers.clear();
        }
    }
    Ok(accounts)
}

async fn virtual_list(State(s): State<Service>, headers: HeaderMap) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    body(visible_virtuals(&s, &user, &access).await?)
}

async fn virtual_users(State(s): State<Service>, headers: HeaderMap) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    if !user.is_admin()
        && !visible_virtuals(&s, &user, &access)
            .await?
            .iter()
            .any(|a| a.can_manage_grants)
    {
        return body(Vec::<virtual_accounts::VirtualGrantee>::new());
    }
    body(virtual_accounts::grantees(&s.pool).await?)
}

async fn create_virtual(
    State(s): State<Service>,
    headers: HeaderMap,
    Json(request): Json<virtual_accounts::SaveVirtualAccount>,
) -> ApiResult {
    write_virtual(&s, &headers, None, request).await
}

async fn save_virtual(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<virtual_accounts::SaveVirtualAccount>,
) -> ApiResult {
    write_virtual(&s, &headers, Some(id), request).await
}

async fn write_virtual(
    s: &Service,
    headers: &HeaderMap,
    id: Option<String>,
    request: virtual_accounts::SaveVirtualAccount,
) -> ApiResult {
    let (_guard, user, access) = catalog_actor(s, headers).await?;
    let accounts = visible_virtuals(s, &user, &access).await?;
    if let Some(id) = &id {
        if !accounts
            .iter()
            .any(|account| account.virtual_id == *id && account.can_configure)
        {
            return Err(forbidden());
        }
        if !virtual_accounts::followers(&s.pool, id).await?.is_empty() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "仍有账户跟随此组合，无法修改".into(),
            ));
        }
    }
    if request
        .children
        .iter()
        .flatten()
        .any(|child| !accounts.iter().any(|a| a.virtual_id == child.virtual_id))
    {
        return Err(forbidden());
    }
    let scope = Scope::load(s, &access, &user).await?;
    let expanded = virtual_accounts::preview_virtual(&s.pool, id.as_deref(), &request)
        .await
        .map_err(bad)?;
    if expanded
        .iter()
        .any(|binding| !scope.can_view(&binding.position_strategy_name))
    {
        return Err(forbidden());
    }
    let orders = strategy_catalog::list_order_strategy_owners(&s.pool).await?;
    if expanded.iter().any(|binding| {
        !orders
            .get(&binding.order_strategy_name)
            .is_some_and(|owner| owner.can_configure(&user))
    }) {
        return Err(forbidden());
    }
    let now = crate::native_futures::now_us();
    let id = if let Some(id) = id {
        virtual_accounts::save(&s.pool, &id, &request, now, Some(&user))
            .await
            .map_err(bad)?;
        id
    } else {
        virtual_accounts::create(&s.pool, &request, Some(user.user_id), now)
            .await
            .map_err(bad)?
    };
    body(
        visible_virtuals(s, &user, &access)
            .await?
            .into_iter()
            .find(|account| account.virtual_id == id)
            .ok_or_else(forbidden)?,
    )
}

async fn delete_virtual(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    if !visible_virtuals(&s, &user, &access)
        .await?
        .iter()
        .any(|account| account.virtual_id == id && account.can_configure)
    {
        return Err(forbidden());
    }
    // The shared store also rejects references from parents this user cannot see.
    virtual_accounts::delete(&s.pool, &id).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VirtualGrants {
    user_ids: Vec<i64>,
}

async fn save_virtual_grants(
    State(s): State<Service>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<VirtualGrants>,
) -> ApiResult {
    let (_guard, user, access) = catalog_actor(&s, &headers).await?;
    if !visible_virtuals(&s, &user, &access)
        .await?
        .iter()
        .any(|account| account.virtual_id == id && account.can_manage_grants)
    {
        return Err(forbidden());
    }
    virtual_accounts::set_managers(&s.pool, &id, &request.user_ids)
        .await
        .map_err(bad)?;
    let account = visible_virtuals(&s, &user, &access)
        .await?
        .into_iter()
        .find(|account| account.virtual_id == id)
        .ok_or_else(forbidden)?;
    body(account.managers)
}

async fn empty_admin_catalog(Extension(user): Extension<AuthUser>) -> ApiResult {
    if !user.is_admin() {
        return Err(forbidden());
    }
    body(json!([]))
}

async fn no_fallback_token(Extension(user): Extension<AuthUser>) -> ApiResult {
    if !user.is_admin() {
        return Err(forbidden());
    }
    Err(native_token_error())
}

fn native_token_error() -> ApiError {
    ApiError(
        StatusCode::NOT_IMPLEMENTED,
        "国内期货发布凭据按账户和策略组管理".into(),
    )
}
