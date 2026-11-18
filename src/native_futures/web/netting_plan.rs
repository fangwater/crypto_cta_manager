//! Dedicated source-scoped readers see only the calculated account target.
//! The Exec process owns target admission and order submission.
use super::{netting::account_lots, netting::observed, *};
use crate::native_futures::netting::{Quantities, store};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{collections::BTreeSet, pin::Pin, time::Duration};
use tonic::Status;

#[allow(dead_code)]
mod wire {
    tonic::include_proto!("cn_futures_target");
}
use wire::{
    ContractTarget, PlanSubscription, PlanUpdate, PositionTarget,
    manager_plans_server::{ManagerPlans, ManagerPlansServer},
};

const PLAN_TOKEN: &str = "x-cta-plan-token";
const MAX_MESSAGE: usize = 16 * 1024;

pub(super) fn credentials(config: &Config) -> Result<BTreeMap<String, [u8; 32]>> {
    let mut hashes = BTreeSet::new();
    for env in config
        .sources
        .iter()
        .filter_map(|s| s.sync.as_ref().map(|sync| sync.token_env.as_str()))
        .chain(
            config
                .sync_export
                .iter()
                .map(|sync| sync.token_env.as_str()),
        )
    {
        if let Ok(value) = std::env::var(env) {
            hashes.insert(hash(&value));
        }
    }
    config
        .ctaker_plan_readers
        .iter()
        .map(|(source, env)| {
            let value = std::env::var(env).context("missing ctaker plan reader credential")?;
            ensure!(valid_token(&value), "invalid ctaker plan reader credential");
            let hash = hash(&value);
            ensure!(hashes.insert(hash), "ctaker plan reader credentials must be distinct from all source sync and plan readers");
            Ok((source.clone(), hash))
        })
        .collect()
}

fn valid_token(value: &str) -> bool {
    (32..=256).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic())
}

fn hash(value: &str) -> [u8; 32] {
    token_mac(value).finalize().into_bytes().into()
}

fn token_mac(value: &str) -> Hmac<Sha256> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(value.as_bytes()).expect("HMAC accepts any key length");
    mac.update(b"cn-futures/ctaker-plan-reader");
    mac
}

pub(super) fn routes(service: Service) -> Router {
    let stream = ManagerPlansServer::new(PlanStream(service.clone()))
        .max_decoding_message_size(MAX_MESSAGE)
        .max_encoding_message_size(MAX_MESSAGE);
    Router::new()
        .route("/api/native/sources/{id}/netting/ctaker-plan", get(read))
        .with_state(service)
        .route_service("/cn_futures_target.ManagerPlans/Watch", stream)
}

#[derive(Serialize)]
struct Plan {
    source_id: String,
    batch_id: String,
    target_revision: i64,
    trading_day: String,
    decision_us: i64,
    expires_at_us: i64,
    net_targets: Quantities,
}

fn authorize_reader(service: &Service, id: &str, token: Option<&str>) -> Result<(), ApiError> {
    let value = token
        .filter(|v| valid_token(v))
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "invalid plan credential".into()))?;
    if !service
        .config
        .ctaker_plan_readers
        .get(id)
        .is_some_and(|stored| token_mac(value).verify_slice(stored).is_ok())
    {
        return Err(forbidden());
    }
    Ok(())
}

async fn read(
    State(service): State<Service>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    authorize_reader(
        &service,
        &id,
        headers.get(PLAN_TOKEN).and_then(|v| v.to_str().ok()),
    )?;
    let (reply, _) = plan(&service, &id, false).await?;
    let mut response = match reply {
        Some(plan) => Json(plan).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

// The deadline is returned only when an active plan is waiting for account evidence.
async fn plan(
    service: &Service,
    id: &str,
    wait_for_replica: bool,
) -> Result<(Option<Plan>, Option<i64>), ApiError> {
    let Some(state) = store::load(&service.pool, id).await? else {
        return Ok((None, None));
    };
    let (Some(day), Some(batch), Some((decision_us, expires_at_us))) = (
        state.target_trading_day.as_ref(),
        state.batch_id.as_ref(),
        state.ctaker_lease(),
    ) else {
        return Ok((None, None));
    };
    let now = crate::native_futures::now_us();
    if !state.external_demand_open || decision_us > now || expires_at_us <= now {
        return Ok((None, None));
    }
    let pending = (None, Some(expires_at_us));
    let snapshot = match observed(service, id).await {
        Err(error) if wait_for_replica && error.0 == StatusCode::SERVICE_UNAVAILABLE => {
            return Ok(pending);
        }
        outcome => outcome?,
    };
    if !snapshot.snapshot_ready
        || !snapshot.snapshot_current_runtime
        || snapshot.replication_stale
        || snapshot.positions_may_lag
        || snapshot.projection_errors != 0
        || snapshot.trading_day.as_ref() != Some(day)
        || !snapshot
            .state_observed_us
            .is_some_and(|at| at <= now && now - at <= service.config.stale_secs as i64 * 1_000_000)
    {
        return Ok(pending);
    }
    let actual = account_lots(&snapshot, &state.contracts)?;
    if !state.account.iter().all(|(market, position)| {
        actual.get(market).copied().unwrap_or_default() == position.quantity()
    }) {
        return Ok(pending);
    }
    let targets = state.execution_plan()?.net_targets;
    let current_revision = store::load(&service.pool, id).await?.map(|s| s.revision);
    if current_revision != Some(state.revision) || crate::native_futures::now_us() >= expires_at_us
    {
        return Ok(pending);
    }
    Ok((
        Some(Plan {
            source_id: id.to_owned(),
            batch_id: batch.clone(),
            target_revision: state.target_revision,
            trading_day: day.clone(),
            decision_us,
            expires_at_us,
            net_targets: targets,
        }),
        None,
    ))
}

async fn observe_plan(service: &Service, id: &str) -> Result<Option<Plan>, ApiError> {
    let Some(state) = store::load(&service.pool, id).await? else {
        return Ok(None);
    };
    let Some(batch) = state.batch_id.as_ref() else {
        return Ok(None);
    };
    Ok(Some(Plan {
        source_id: id.to_owned(),
        batch_id: batch.clone(),
        target_revision: state.target_revision,
        trading_day: state.target_trading_day.clone().unwrap_or_default(),
        decision_us: state.decision_us,
        expires_at_us: 0,
        net_targets: state.execution_plan()?.net_targets,
    }))
}

impl TryFrom<Plan> for PositionTarget {
    type Error = anyhow::Error;

    fn try_from(plan: Plan) -> Result<Self> {
        Ok(Self {
            source_id: plan.source_id,
            batch_id: plan.batch_id,
            target_revision: plan.target_revision,
            trading_day: plan.trading_day,
            decision_us: plan.decision_us.try_into()?,
            expires_at_us: plan.expires_at_us.try_into()?,
            net_targets: plan
                .net_targets
                .into_iter()
                .map(|(market, lots)| {
                    Ok(ContractTarget {
                        market,
                        lots: lots.try_into()?,
                    })
                })
                .collect::<Result<_>>()?,
        })
    }
}

#[derive(Clone)]
struct PlanStream(Service);

#[tonic::async_trait]
impl ManagerPlans for PlanStream {
    type WatchStream = Pin<Box<dyn tokio_stream::Stream<Item = Result<PlanUpdate, Status>> + Send>>;

    async fn watch(
        &self,
        request: tonic::Request<PlanSubscription>,
    ) -> Result<tonic::Response<Self::WatchStream>, Status> {
        let id = &request.get_ref().source_id;
        authorize_reader(
            &self.0,
            id,
            request
                .metadata()
                .get(PLAN_TOKEN)
                .and_then(|v| v.to_str().ok()),
        )
        .map_err(|error| match error.0 {
            StatusCode::UNAUTHORIZED => Status::unauthenticated("invalid plan credential"),
            _ => Status::permission_denied("source plan access denied"),
        })?;
        let mut changes = self.0.plan_changes[id].subscribe();
        let mut shutdown = self.0.plan_shutdown.subscribe();
        let PlanSubscription {
            source_id: id,
            observe_only,
        } = request.into_inner();
        let service = self.0.clone();
        let updates = async_stream::try_stream! {
            let mut previous = None;
            let mut delay = Duration::from_millis(25);
            while !*shutdown.borrow() {
                changes.borrow_and_update();
                let (plan, deadline) = if observe_only {
                    observe_plan(&service, &id).await.map(|plan| (plan, None))
                } else {
                    plan(&service, &id, true).await
                }.map_err(|_| Status::unavailable("read Manager plan"))?;
                let target = plan.map(PositionTarget::try_from).transpose()
                    .map_err(|_| Status::internal("invalid Manager target"))?;
                if previous.as_ref() != Some(&target) {
                    previous = Some(target.clone());
                    yield PlanUpdate { target };
                }
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = changes.changed() => delay = Duration::from_millis(25),
                    _ = retry_snapshot(deadline, delay) => {
                        delay = (delay * 2).min(Duration::from_secs(1));
                    }
                }
            }
        };
        Ok(tonic::Response::new(Box::pin(updates)))
    }
}

async fn retry_snapshot(deadline: Option<i64>, delay: Duration) {
    if let Some(deadline) = deadline {
        let remaining = deadline.saturating_sub(crate::native_futures::now_us());
        tokio::time::sleep(delay.min(Duration::from_micros(remaining.max(0) as u64))).await;
    } else {
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::manager_plans_client::ManagerPlansClient;

    #[tokio::test]
    async fn manager_http_port_routes_grpc_with_source_scoped_plan_auth() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(&format!(
            "mode = 'cn_futures'\nsecondary_root = '{}'\n[[sources]]\nid = 'source'\naccount = 'test'\nadapter = 'ctp'\nrocksdb_path = '{}'\n",
            dir.path().join("secondary").display(),
            dir.path().join("evidence").display(),
        )).unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let service = Service::new(config, pool).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, service.router()).await });
        let mut client = ManagerPlansClient::connect(origin).await.unwrap();
        let request = || {
            tonic::Request::new(PlanSubscription {
                source_id: "source".into(),
                observe_only: false,
            })
        };
        let error = client.watch(request()).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unauthenticated);

        let mut request = request();
        request.metadata_mut().insert(
            PLAN_TOKEN,
            "fixture-plan-token-without-source-grant".parse().unwrap(),
        );
        let error = client.watch(request).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        server.abort();
    }
}
