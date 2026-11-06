//! The live publisher reads a prepared catalog. PostgreSQL is a coalesced mirror,
//! never a dependency of an existing strategy's target update.
use super::*;
use std::sync::{
    Mutex, RwLock as SyncRwLock,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub(super) struct Entry {
    pub strategy: strategy_catalog::PositionStrategy,
    pub created_at_us: i64,
    pub access: strategy_catalog::PositionAccess,
}

#[derive(Clone, Default)]
pub(super) struct Catalog {
    pub entries: BTreeMap<String, Entry>,
    pub orders: Vec<strategy_catalog::OrderStrategy>,
    pub bindings: Vec<strategy_catalog::AccountBinding>,
    pub fees: BTreeMap<String, TheoreticalFeeRates>,
    pub fallback_hashes: Vec<String>,
    pub sessions: BTreeMap<Vec<u8>, (AuthUser, i64)>,
}

#[derive(Default)]
pub(super) struct Targets {
    catalog: SyncRwLock<Arc<Catalog>>,
    current: SyncRwLock<BTreeMap<String, Entry>>,
    ready: AtomicBool,
    pending: Mutex<BTreeMap<String, Entry>>,
    creation: tokio::sync::Mutex<()>,
    pub changed: tokio::sync::Notify,
    pub reload: tokio::sync::Notify,
}

pub(super) struct Edit {
    _guard: tokio::sync::OwnedRwLockWriteGuard<()>,
    targets: Arc<Targets>,
}

impl Drop for Edit {
    fn drop(&mut self) {
        self.targets.reload.notify_one();
    }
}

impl Targets {
    pub async fn edit(self: &Arc<Self>, lock: &Arc<RwLock<()>>) -> Edit {
        let guard = lock.clone().write_owned().await;
        self.ready.store(false, Ordering::Release);
        Edit {
            _guard: guard,
            targets: self.clone(),
        }
    }

    pub fn catalog(&self) -> Result<Arc<Catalog>> {
        anyhow::ensure!(
            self.ready.load(Ordering::Acquire),
            "publication catalog is refreshing; retry the target update"
        );
        Ok(self.catalog.read().unwrap().clone())
    }

    pub fn current(&self, name: &str) -> Option<strategy_catalog::PositionStrategy> {
        self.current
            .read()
            .unwrap()
            .get(name)
            .map(|e| e.strategy.clone())
    }

    pub fn session(&self, token: &str) -> Result<Option<AuthUser>> {
        let catalog = self.catalog()?;
        Ok(catalog
            .sessions
            .get(&auth::token_hash(token))
            .filter(|(_, expires)| *expires > unix_now_us())
            .map(|(user, _)| user.clone()))
    }

    pub fn accept(&self, entry: Entry) {
        let name = entry.strategy.strategy_name.clone();
        self.current
            .write()
            .unwrap()
            .insert(name.clone(), entry.clone());
        self.pending.lock().unwrap().insert(name, entry);
        self.changed.notify_one();
    }

    pub fn remember(&self, entry: Entry) {
        self.current
            .write()
            .unwrap()
            .insert(entry.strategy.strategy_name.clone(), entry);
    }

    pub fn install(&self, mut catalog: Catalog, archive: &PositionArchive) -> Result<()> {
        // A cold strategy creation may reload metadata alongside other live
        // strategies. Never replace a target accepted while PG was being read.
        let mut current = self.current.write().unwrap();
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|name, entry| {
            catalog
                .entries
                .get(name)
                .is_some_and(|e| e.created_at_us == entry.created_at_us)
        });
        for (name, entry) in &mut catalog.entries {
            let mirrored_at = entry.strategy.updated_at_us;
            if let Some(current) = archive.current_target(name, entry.created_at_us)? {
                if current.updated_at_us > entry.strategy.updated_at_us {
                    entry.strategy = current;
                    pending.insert(name.clone(), entry.clone());
                }
            }
            if let Some(live) = current.get(name) {
                if live.created_at_us == entry.created_at_us
                    && live.strategy.updated_at_us > entry.strategy.updated_at_us
                {
                    entry.strategy = live.strategy.clone();
                }
            }
            if entry.strategy.updated_at_us > mirrored_at {
                pending.insert(name.clone(), entry.clone());
            }
        }
        *current = catalog.entries.clone();
        *self.catalog.write().unwrap() = Arc::new(catalog);
        self.ready.store(true, Ordering::Release);
        if !pending.is_empty() {
            self.changed.notify_one();
        }
        Ok(())
    }

    pub async fn load(&self, pool: &PgPool, archive: &PositionArchive) -> Result<()> {
        let mut catalog = Catalog::default();
        let identities = sqlx::query("SELECT strategy_name, (extract(epoch FROM created_at)*1000000)::bigint AS created_at_us FROM cta_position_strategies")
            .fetch_all(pool).await?.into_iter().map(|r| Ok((r.try_get::<String,_>("strategy_name")?, r.try_get::<i64,_>("created_at_us")?)))
            .collect::<Result<BTreeMap<_,_>>>()?;
        for strategy in strategy_catalog::list_position_strategies(pool).await? {
            let name = strategy.strategy_name.clone();
            let access = strategy_catalog::load_position_access(pool, &name)
                .await?
                .context("publication strategy disappeared")?;
            catalog.entries.insert(
                name.clone(),
                Entry {
                    strategy,
                    access,
                    created_at_us: identities[&name],
                },
            );
            catalog
                .bindings
                .extend(strategy_catalog::list_active_bindings_for_position(pool, &name).await?);
        }
        catalog.orders = strategy_catalog::list_order_strategies(pool).await?;
        catalog.fees = postgres::load_theoretical_twap_fee_rates(pool).await?;
        catalog.fallback_hashes =
            sqlx::query_scalar("SELECT token_hash FROM cta_publish_fallback_tokens")
                .fetch_all(pool)
                .await?;
        for row in sqlx::query("SELECT s.token_hash, (extract(epoch FROM s.expires_at)*1000000)::bigint AS expires_at_us, u.user_id,u.username,u.role FROM cta_auth_sessions s JOIN cta_users u ON u.user_id=s.user_id WHERE s.expires_at>now() AND NOT u.disabled")
            .fetch_all(pool).await? {
            catalog.sessions.insert(row.try_get("token_hash")?, (AuthUser { user_id: row.try_get("user_id")?, username: row.try_get("username")?, role: row.try_get("role")? }, row.try_get("expires_at_us")?));
        }
        self.install(catalog, archive)
    }

    pub async fn flush(&self, pool: &PgPool) -> Result<()> {
        // One latest entry per strategy bounds memory even during a long PG outage.
        let pending = self.pending.lock().unwrap().clone();
        let mut failure = None;
        for (name, entry) in pending {
            let strategy = &entry.strategy;
            // UPDATE cannot resurrect deleted strategies; creation identity also
            // prevents an old queued update from touching a recreated name.
            let result = tokio::time::timeout(Duration::from_secs(3), sqlx::query("UPDATE cta_position_strategies SET targets=$2, symbol_order_strategy_overrides=$3, updated_at_us=$4 WHERE strategy_name=$1 AND (extract(epoch FROM created_at)*1000000)::bigint=$5 AND updated_at_us <= $4")
                .bind(&name).bind(serde_json::to_value(&strategy.targets)?)
                .bind(serde_json::to_value(&strategy.symbol_order_strategy_overrides)?)
                .bind(strategy.updated_at_us).bind(entry.created_at_us).execute(pool)).await
                .map_err(anyhow::Error::from).and_then(|r| r.map_err(anyhow::Error::from));
            if let Err(error) = result {
                failure.get_or_insert(error);
                continue;
            }
            let mut queue = self.pending.lock().unwrap();
            if queue.get(&name).is_some_and(|e| {
                e.created_at_us == entry.created_at_us
                    && e.strategy.updated_at_us == strategy.updated_at_us
            }) {
                queue.remove(&name);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

pub(super) async fn publish(
    state: WebState,
    user: Option<Extension<AuthUser>>,
    headers: HeaderMap,
    request: SavePositionStrategyRequest,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    let session_user = user.as_ref().map(|Extension(user)| user);
    let mut catalog = match state.targets.catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: error.to_string(),
                }),
            )
                .into_response());
        }
    };
    if !catalog.entries.contains_key(&request.strategy_name) {
        // Strategy creation is a configuration operation. Machine tokens cannot
        // create names. Existing live targets never enter this PostgreSQL path.
        let Some(creator) = session_user else {
            return Ok(forbidden("login required to create a position strategy"));
        };
        if let Err(message) = strategy_catalog::validate_targets(&request.targets).and_then(|_| {
            strategy_catalog::validate_symbol_order_strategy_overrides(
                &request.targets,
                &request.symbol_order_strategy_overrides,
            )
        }) {
            return Ok(bad_request(message));
        }
        if let Err(message) = require_experimental_algorithm_token(
            &headers,
            changed_override_uses_experimental_algorithm(&request, None, &catalog.orders),
        ) {
            return Ok(forbidden(message));
        }
        if request
            .symbol_order_strategy_overrides
            .values()
            .any(|name| !catalog.orders.iter().any(|o| &o.strategy_name == name))
        {
            return Ok(bad_request(
                "unknown order strategy in symbol overrides".into(),
            ));
        }
        let _creation = state.targets.creation.lock().await;
        if !state
            .targets
            .catalog()?
            .entries
            .contains_key(&request.strategy_name)
        {
            strategy_catalog::create_position_strategy(
                &state.pool,
                &request,
                creator.user_id,
                unix_now_us(),
            )
            .await?;
            state
                .targets
                .load(&state.pool, &state.position_archive)
                .await?;
        }
        catalog = state.targets.catalog()?;
    }
    let mut entry = catalog
        .entries
        .get(&request.strategy_name)
        .context("position strategy is unavailable")?
        .clone();
    if let Some(current) = state.targets.current(&request.strategy_name) {
        entry.strategy = current;
    }
    let provided = headers
        .get(PUBLISH_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok());
    let matches = entry
        .access
        .publish_token_hash
        .as_deref()
        .is_some_and(|hash| auth::publish_token_matches(provided, hash))
        || catalog
            .fallback_hashes
            .iter()
            .any(|hash| auth::publish_token_matches(provided, hash));
    if !position_publish_allowed(session_user, Some(&entry.access), matches) {
        return Ok(forbidden(
            "position publish requires an authorized session or publish token",
        ));
    }
    if let Err(message) = require_experimental_algorithm_token(
        &headers,
        changed_override_uses_experimental_algorithm(
            &request,
            Some(&entry.strategy),
            &catalog.orders,
        ),
    ) {
        return Ok(forbidden(message));
    }
    if let Err(message) = strategy_catalog::validate_targets(&request.targets) {
        return Ok(bad_request(message));
    }
    if let Err(message) = strategy_catalog::validate_symbol_order_strategy_overrides(
        &request.targets,
        &request.symbol_order_strategy_overrides,
    ) {
        return Ok(bad_request(message));
    }
    let named: BTreeMap<_, _> = catalog
        .orders
        .iter()
        .map(|o| (o.strategy_name.as_str(), &o.order_parameters))
        .collect();
    let mut overrides = BTreeMap::new();
    for (symbol, name) in &request.symbol_order_strategy_overrides {
        let Some(parameters) = named.get(name.as_str()) else {
            return Ok(bad_request(format!("unknown order strategy: {name}")));
        };
        overrides.insert(symbol.clone(), (*parameters).clone());
    }
    let saved = strategy_catalog::PositionStrategy {
        strategy_name: request.strategy_name,
        targets: request.targets,
        symbol_order_strategy_overrides: request.symbol_order_strategy_overrides,
        updated_at_us: unix_now_us().max(entry.strategy.updated_at_us.saturating_add(1)),
    };
    let mut accounts = Vec::new();
    let mut plans = Vec::new();
    for binding in catalog
        .bindings
        .iter()
        .filter(|b| b.position_strategy_name == saved.strategy_name && b.shares > 0.0)
    {
        let source = match resolve_publish_source(&state.config, &binding.source_id) {
            Ok(source) => source,
            Err(error) => return Ok(publish_failure_response(error)),
        };
        for symbol in saved.targets.keys().chain(overrides.keys()) {
            if let Err(error) = crate::exec_routing::validate_target_symbol(source, symbol) {
                return Ok(bad_request(error.to_string()));
            }
        }
        let order = named
            .get(binding.order_strategy_name.as_str())
            .context("binding order strategy is missing")?;
        let projection =
            match state
                .redis_runtime
                .project_targets(source, &saved.targets, binding.shares)
            {
                Ok(projection) => projection,
                Err(error) => {
                    return Ok((
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(ErrorResponse {
                            error: error.to_string(),
                        }),
                    )
                        .into_response());
                }
            };
        let targets = projection.as_ref().map_or_else(
            || strategy_catalog::scale_targets(&saved.targets, binding.shares),
            |p| p.targets.clone(),
        );
        let parameters = projection
            .as_ref()
            .map_or_else(|| overrides.clone(), |p| p.remap(&overrides));
        if let Err(message) = strategy_catalog::validate_targets(&targets) {
            return Ok(bad_request(message));
        }
        if let Err(error) = state
            .redis_runtime
            .validate_cta_targets(&source.id, &targets)
        {
            return Ok(bad_request(error.to_string()));
        }
        if let Err(error) = state.account_modes.check(source, &targets) {
            return Ok(publish_failure_response(error));
        }
        if let Err(error) = crate::exec_routing::validate_grid_account(source, order, &parameters) {
            return Ok(bad_request(error.to_string()));
        }
        let mut archived = crate::position_archive::published_account(
            &source.id,
            &binding.binding_name,
            binding.shares,
        );
        archived.target_projection = projection;
        archived.set_theoretical_fees(Some(
            catalog
                .fees
                .get(&source.id)
                .context("missing archived account fees")?
                .clone(),
        ));
        let frozen = archived.target_projection.clone();
        accounts.push(archived);
        plans.push((
            binding.clone(),
            (*order).clone(),
            parameters,
            targets,
            frozen,
        ));
    }
    let prepare_ms = started.elapsed().as_millis() as u64;
    let archive = state.position_archive.clone();
    let strategy = saved.clone();
    let identity = entry.created_at_us;
    let archived_accounts = accounts.clone();
    let message = tokio::task::spawn_blocking(move || {
        archive.append_target(&strategy, identity, archived_accounts)
    })
    .await??;
    let archive_ms = started.elapsed().as_millis() as u64 - prepare_ms;
    entry.strategy = saved.clone();
    state.targets.remember(entry.clone());
    // No SQL, account snapshot HTTP, or catalog reread can precede these writes.
    let mut jobs = tokio::task::JoinSet::new();
    for (binding, order, overrides, targets, frozen) in plans {
        let state = state.clone();
        let strategy_name = saved.strategy_name.clone();
        jobs.spawn(async move {
            let started = Instant::now();
            let source = state
                .config
                .sources
                .iter()
                .find(|s| s.id == binding.source_id)
                .unwrap();
            let result = async {
                if let Some(projection) = frozen { projection.ensure_fresh(unix_now_us())?; }
                state.redis_runtime.publish_strategy(source, &binding.binding_name, &order, &overrides, &targets).await
            }.await;
            match result {
                Ok(published) => {
                    info!(%strategy_name, source_id = %binding.source_id, binding_name = %binding.binding_name, shares = binding.shares,
                        duration_ms = started.elapsed().as_millis() as u64, "published bound account after position update");
                    if let Some(ts) = published.updated_at_us {
                        state
                            .reload_notify
                            .notify(source, &published.strategy_name, ts);
                    }
                    BindingPublishResult {
                        source_id: binding.source_id,
                        binding_name: binding.binding_name,
                        shares: binding.shares,
                        published: Some(published),
                        error: None,
                    }
                }
                Err(error) => {
                    warn!(%strategy_name, source_id = %binding.source_id, binding_name = %binding.binding_name, %error,
                        duration_ms = started.elapsed().as_millis() as u64, "bound-account publish failed after position update");
                    BindingPublishResult {
                    source_id: binding.source_id,
                    binding_name: binding.binding_name,
                    shares: binding.shares,
                    published: None,
                    error: Some(error.to_string()),
                    }
                },
            }
        });
    }
    let mut publishes = Vec::new();
    let mut task_failed = false;
    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(result) => publishes.push(result),
            Err(error) => {
                task_failed = true;
                error!(%error, "Exec target publisher task failed");
            }
        }
    }
    publishes.sort_by(|a, b| (&a.source_id, &a.binding_name).cmp(&(&b.source_id, &b.binding_name)));
    state.targets.accept(entry);
    if !message.published_accounts.is_empty()
        && let Ok(permit) = state.factual_jobs.clone().try_acquire_owned()
    {
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut positions = Vec::new();
            for account in &message.published_accounts {
                if let Some(position) = load_factual_position(
                    &state,
                    &account.source_id,
                    &message.strategy.strategy_name,
                )
                .await
                {
                    positions.push(position);
                }
            }
            let archive = state.position_archive.clone();
            let result = tokio::task::spawn_blocking(move || {
                archive.enrich_factual_positions(message, positions)
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                warn!("factual position archive enrichment failed");
            }
        });
    }
    info!(strategy_name = %saved.strategy_name, updated_at_us = saved.updated_at_us,
        preparation_ms = prepare_ms, snapshot_ms = 0u64, archive_ms, publish_ms = started.elapsed().as_millis() as u64 - prepare_ms - archive_ms,
        account_count = publishes.len(), "position strategy publish stages completed");
    if task_failed || publishes.iter().any(|p| p.error.is_some()) {
        return Ok((StatusCode::BAD_GATEWAY, Json(ErrorResponse { error: "target retained in RocksDB; at least one Exec publication failed; retry the target update".into() })).into_response());
    }
    if let Some(user) = session_user {
        let configured = state.config.sources.iter().map(|s| s.id.clone()).collect();
        let visible = auth::visible_source_boundary(
            &configured,
            &state.config.dashboard.source_viewers,
            Some(&user.username),
        );
        publishes.retain(|p| visible.contains(&p.source_id));
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

pub(super) fn spawn(state: &WebState) {
    let reload_state = state.clone();
    tokio::spawn(async move {
        loop {
            reload_state.targets.reload.notified().await;
            loop {
                let _guard = reload_state.configuration_lock.write().await;
                match reload_state
                    .targets
                    .load(&reload_state.pool, &reload_state.position_archive)
                    .await
                {
                    Ok(()) => break,
                    Err(error) => {
                        warn!(%error, "publication catalog reload failed; target admission remains closed")
                    }
                }
                drop(_guard);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    });
    let mirror_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! { _ = interval.tick() => {}, _ = mirror_state.targets.changed.notified() => {} }
            if let Err(error) = mirror_state.targets.flush(&mirror_state.pool).await {
                warn!(%error, "current target PG mirror delayed; durable targets retained for retry");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, qty: f64, ts: i64) -> Entry {
        Entry {
            strategy: strategy_catalog::PositionStrategy {
                strategy_name: name.into(),
                targets: BTreeMap::from([(
                    "BTCUSDT".into(),
                    crate::order_config::TargetPosition::new(qty, 0).unwrap(),
                )]),
                symbol_order_strategy_overrides: BTreeMap::new(),
                updated_at_us: ts,
            },
            created_at_us: 7,
            access: strategy_catalog::PositionAccess {
                created_by_user_id: Some(1),
                publish_token_hash: None,
                open_visibility: false,
                manager_user_ids: vec![],
                grants: BTreeMap::new(),
            },
        }
    }

    async fn send(state: &WebState, name: &str, qty: f64, token: Option<&str>) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            headers.insert(PUBLISH_TOKEN_HEADER, token.parse().unwrap());
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            save_position_strategy(
                State(state.clone()),
                ConnectInfo("127.0.0.1:1".parse().unwrap()),
                None,
                headers,
                Json(SavePositionStrategyRequest {
                    strategy_name: name.into(),
                    targets: BTreeMap::from([(
                        "BTCUSDT".into(),
                        crate::order_config::TargetPosition::new(qty, 0).unwrap(),
                    )]),
                    symbol_order_strategy_overrides: BTreeMap::new(),
                }),
            ),
        )
        .await
        .expect("live target must not wait for PostgreSQL")
        .unwrap_or_else(|e| panic!("{}", e.0))
    }

    #[tokio::test]
    async fn closed_postgres_does_not_block_targets_and_recovery_uses_rocksdb() {
        let (_dir, state, _, _, _) = super::super::tests::timeline_test_state(90);
        state.pool.close().await;
        let mut catalog = Catalog::default();
        catalog.entries.insert("live".into(), entry("live", 0.0, 1));
        state
            .targets
            .install(catalog.clone(), &state.position_archive)
            .unwrap();
        assert_eq!(
            send(&state, "live", 1.0, None).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&state, "live", 2.0, None).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&state, "unknown", 3.0, None).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(state.targets.pending.lock().unwrap().len(), 1);
        assert!(state.targets.flush(&state.pool).await.is_err());
        assert_eq!(
            state.targets.current("live").unwrap().targets["BTCUSDT"].qty,
            2.0
        );
        assert_eq!(state.position_archive.scan_from(0).unwrap().len(), 2);
        let recovered = Targets::default();
        recovered
            .install(catalog.clone(), &state.position_archive)
            .unwrap();
        assert_eq!(
            recovered.current("live").unwrap().targets["BTCUSDT"].qty,
            2.0
        );
        assert_eq!(recovered.pending.lock().unwrap().len(), 1);
        // A deleted and recreated catalog name cannot inherit its predecessor's target.
        catalog.entries.get_mut("live").unwrap().created_at_us = 8;
        recovered.install(catalog, &state.position_archive).unwrap();
        assert_eq!(
            recovered.current("live").unwrap().targets["BTCUSDT"].qty,
            0.0
        );
        assert!(recovered.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn configuration_edits_close_admission_and_revoked_tokens_are_not_cached() {
        let (_dir, state, _, _, _) = super::super::tests::timeline_test_state(90);
        state.pool.close().await;
        let mut catalog = Catalog::default();
        let mut protected = entry("protected", 0.0, 1);
        protected.access.publish_token_hash = Some(auth::publish_token_hash("old-token"));
        catalog.entries.insert("protected".into(), protected);
        catalog.sessions.insert(
            auth::token_hash("expired"),
            (
                AuthUser {
                    user_id: 1,
                    username: "test".into(),
                    role: "user".into(),
                },
                1,
            ),
        );
        state
            .targets
            .install(catalog.clone(), &state.position_archive)
            .unwrap();
        assert!(state.targets.session("expired").unwrap().is_none());
        assert_eq!(
            send(&state, "protected", 1.0, Some("wrong-token"))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(&state, "protected", 1.0, Some("old-token"))
                .await
                .status(),
            StatusCode::OK
        );
        let editing = state.targets.edit(&state.configuration_lock).await;
        assert_eq!(
            send(&state, "protected", 2.0, Some("old-token"))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(editing);
        assert_eq!(
            send(&state, "protected", 2.0, Some("old-token"))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        catalog
            .entries
            .get_mut("protected")
            .unwrap()
            .access
            .publish_token_hash = Some(auth::publish_token_hash("new-token"));
        state
            .targets
            .install(catalog, &state.position_archive)
            .unwrap();
        assert_eq!(
            send(&state, "protected", 2.0, Some("old-token"))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(&state, "protected", 2.0, Some("new-token"))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(state.position_archive.scan_from(0).unwrap().len(), 2);
    }

    #[tokio::test]
    #[ignore = "requires EXEC_REDIS_TEST_SERVER_BIN pointing to an isolated redis-server binary"]
    async fn exec_receives_latest_scaled_targets_with_pg_closed_and_snapshot_stalled() {
        use redis::AsyncCommands;
        use std::process::{Command, Stdio};
        struct Server(std::process::Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (_dir, mut state, source_id, _, _) = super::super::tests::timeline_test_state(90);
        let _server = Server(
            Command::new(std::env::var("EXEC_REDIS_TEST_SERVER_BIN").unwrap())
                .args([
                    "--bind",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--save",
                    "",
                    "--appendonly",
                    "no",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let url = format!("redis://127.0.0.1:{port}/0");
        let client = redis::Client::open(url.clone()).unwrap();
        let mut connection = None;
        for _ in 0..100 {
            if let Ok(c) = client.get_multiplexed_async_connection().await {
                connection = Some(c);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut connection = connection.expect("isolated Redis startup");
        state.redis_runtime = RedisRuntime::connect(crate::config::RedisSettings {
            url,
            ..Default::default()
        })
        .unwrap();
        state.pool.close().await;
        let source = &mut Arc::make_mut(&mut state.config).sources[0];
        source.exec_config_url = Some("http://127.0.0.1:1".into());
        source.ipc_namespace = Some(format!(
            "target_publish_test_{}_{}",
            std::process::id(),
            unix_now_us()
        ));
        // Never allow HTTP snapshot collection onto the critical path.
        let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        source.exec_viz_url = Some(format!("http://{}", stalled.local_addr().unwrap()));
        let mut catalog = Catalog::default();
        catalog.entries.insert("live".into(), entry("live", 0.0, 1));
        catalog.orders.push(strategy_catalog::OrderStrategy {
            strategy_name: "twap".into(),
            order_parameters: Default::default(),
            updated_at_us: 1,
        });
        catalog.bindings.push(strategy_catalog::AccountBinding {
            source_id: source_id.clone(),
            binding_name: "live".into(),
            position_strategy_name: "live".into(),
            order_strategy_name: "twap".into(),
            shares: 2.0,
            updated_at_us: 1,
        });
        catalog.fees.insert(source_id.clone(), 0.0001.into());
        state
            .targets
            .install(catalog.clone(), &state.position_archive)
            .unwrap();
        // A missing startup mode still blocks nonzero targets before any write.
        assert_eq!(
            send(&state, "live", 1.0, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(state.position_archive.scan_from(0).unwrap().is_empty());
        state.account_modes = Arc::new(account_mode::AccountModes::verified_fixture(&source_id));
        for qty in [1.0, 2.0] {
            assert_eq!(
                send(&state, "live", qty, None).await.status(),
                StatusCode::OK
            );
            let raw: String = connection
                .get(format!("{source_id}:binance-futures:batch_exec:live"))
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(value["targets"]["BTCUSDT"]["qty"], qty * 2.0);
        }
        let messages = state.position_archive.scan_from(0).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].published_accounts[0].shares, 2.0);
        assert_eq!(messages[1].strategy.targets["BTCUSDT"].qty, 2.0);
        // Removing the binding from the prepared catalog cannot publish its old shares.
        catalog.bindings.clear();
        state
            .targets
            .install(catalog, &state.position_archive)
            .unwrap();
        assert_eq!(
            send(&state, "live", 3.0, None).await.status(),
            StatusCode::OK
        );
        let raw: String = connection
            .get(format!("{source_id}:binance-futures:batch_exec:live"))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()["targets"]["BTCUSDT"]["qty"],
            4.0
        );
        drop(stalled);
    }
}
