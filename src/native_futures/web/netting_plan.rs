//! Dedicated source-scoped readers see only the calculated account target.
//! The Exec process owns target admission and order submission.
use super::{netting::account_lots, netting::observed, *};
use crate::native_futures::{
    netting::{Quantities, store},
    reader::Snapshot,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{collections::BTreeSet, pin::Pin, time::Duration};
use tonic::Status;

#[allow(dead_code)]
mod wire {
    tonic::include_proto!("cn_futures_target");
}
use wire::{
    ContractTarget, PlanStatus, PlanSubscription, PlanUpdate, PositionTarget,
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

struct PlanCheck {
    status: PlanStatus,
    batch_id: String,
    target_revision: i64,
    target: Option<Plan>,
    recheck_us: Option<i64>,
}

impl PlanCheck {
    fn without_batch(status: PlanStatus) -> Self {
        Self {
            status,
            batch_id: String::new(),
            target_revision: 0,
            target: None,
            recheck_us: None,
        }
    }

    fn into_update(self) -> Result<PlanUpdate> {
        Ok(PlanUpdate {
            status: self.status.into(),
            batch_id: self.batch_id,
            target_revision: self.target_revision,
            target: self.target.map(PositionTarget::try_from).transpose()?,
        })
    }

    fn http_response(self, error: Option<ApiError>) -> Result<Response, ApiError> {
        let mut response = match (error, self.target) {
            (Some(error), _) => error.into_response(),
            (None, Some(plan)) => Json(plan).into_response(),
            (None, None) => StatusCode::NO_CONTENT.into_response(),
        };
        let headers = response.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        let status = self
            .status
            .as_str_name()
            .trim_start_matches("PLAN_STATUS_")
            .to_ascii_lowercase();
        headers.insert(
            "x-cta-plan-status",
            header::HeaderValue::from_str(&status).map_err(anyhow::Error::from)?,
        );
        if !self.batch_id.is_empty() {
            headers.insert(
                "x-cta-plan-batch-id",
                header::HeaderValue::from_str(&self.batch_id).map_err(anyhow::Error::from)?,
            );
            headers.insert(
                "x-cta-plan-target-revision",
                header::HeaderValue::from(self.target_revision),
            );
        }
        Ok(response)
    }
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
    match plan(&service, &id, false, false).await {
        Ok(result) => result.http_response(None),
        Err(error) if error.0 == StatusCode::SERVICE_UNAVAILABLE => {
            PlanCheck::without_batch(PlanStatus::ReplicaNotReady).http_response(Some(error))
        }
        Err(error) => Err(error),
    }
}

async fn plan(
    service: &Service,
    id: &str,
    wait_for_replica: bool,
    observe_only: bool,
) -> Result<PlanCheck, ApiError> {
    let Some(state) = store::load(&service.pool, id).await? else {
        return Ok(PlanCheck::without_batch(PlanStatus::NoBatch));
    };
    let Some(batch) = state.batch_id.as_ref() else {
        return Ok(PlanCheck::without_batch(PlanStatus::NoBatch));
    };
    let check = |status, target, recheck_us| PlanCheck {
        status,
        batch_id: batch.clone(),
        target_revision: state.target_revision,
        target,
        recheck_us,
    };
    let payload = |trading_day, decision_us, expires_at_us| -> Result<Plan> {
        Ok(Plan {
            source_id: id.to_owned(),
            batch_id: batch.clone(),
            target_revision: state.target_revision,
            trading_day,
            decision_us,
            expires_at_us,
            net_targets: state.execution_plan()?.net_targets,
        })
    };
    if observe_only {
        let status = if state.external_demand_open {
            PlanStatus::ObserveOnly
        } else {
            PlanStatus::NoExternalDemand
        };
        return Ok(check(
            status,
            Some(payload(
                state.target_trading_day.clone().unwrap_or_default(),
                state.decision_us,
                0,
            )?),
            None,
        ));
    }
    if !state.external_demand_open {
        return Ok(check(PlanStatus::NoExternalDemand, None, None));
    }
    let Some(day) = state.target_trading_day.as_ref() else {
        return Ok(check(PlanStatus::MissingTradingDay, None, None));
    };
    let Some((decision_us, expires_at_us)) = state.ctaker_lease() else {
        return Ok(check(PlanStatus::EvidenceError, None, None));
    };
    let now = crate::native_futures::now_us();
    if decision_us > now {
        return Ok(check(PlanStatus::DecisionInFuture, None, Some(decision_us)));
    }
    if expires_at_us <= now {
        return Ok(check(PlanStatus::LeaseExpired, None, None));
    }
    let pending = |status| check(status, None, Some(expires_at_us));
    let snapshot = match observed(service, id).await {
        Err(error) if wait_for_replica && error.0 == StatusCode::SERVICE_UNAVAILABLE => {
            return Ok(pending(PlanStatus::ReplicaNotReady));
        }
        outcome => outcome?,
    };
    if let Some(status) = snapshot_blocker(
        &snapshot,
        day,
        service.config.stale_secs,
        crate::native_futures::now_us(),
    ) {
        return Ok(pending(status));
    }
    let actual = account_lots(&snapshot, &state.contracts)?;
    if !state.account.iter().all(|(market, position)| {
        actual.get(market).copied().unwrap_or_default() == position.quantity()
    }) {
        return Ok(pending(PlanStatus::AccountPositionsUnconfirmed));
    }
    let current_revision = store::load(&service.pool, id).await?.map(|s| s.revision);
    if current_revision != Some(state.revision) {
        return Ok(pending(PlanStatus::Refreshing));
    }
    if crate::native_futures::now_us() >= expires_at_us {
        return Ok(check(PlanStatus::LeaseExpired, None, None));
    }
    let freshness_deadline = snapshot.state_observed_us.map(|observed| {
        observed
            .saturating_add(service.config.stale_secs as i64 * 1_000_000)
            .saturating_add(1)
    });
    Ok(check(
        PlanStatus::Ready,
        Some(payload(day.clone(), decision_us, expires_at_us)?),
        Some(
            freshness_deadline
                .unwrap_or(expires_at_us)
                .min(expires_at_us),
        ),
    ))
}

fn snapshot_blocker(
    snapshot: &Snapshot,
    day: &str,
    stale_secs: u64,
    now: i64,
) -> Option<PlanStatus> {
    if snapshot.replication_stale {
        Some(PlanStatus::ReplicaNotReady)
    } else if snapshot.projection_errors != 0 {
        Some(PlanStatus::EvidenceError)
    } else if !snapshot.snapshot_ready || !snapshot.snapshot_current_runtime {
        Some(PlanStatus::AccountNotReady)
    } else if snapshot.trading_day.as_deref() != Some(day) {
        Some(PlanStatus::TradingDayMismatch)
    } else if snapshot.positions_may_lag {
        Some(PlanStatus::AccountPositionsUnconfirmed)
    } else if !snapshot
        .state_observed_us
        .is_some_and(|at| at <= now && now.saturating_sub(at) <= stale_secs as i64 * 1_000_000)
    {
        Some(PlanStatus::AccountSnapshotStale)
    } else {
        None
    }
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
                let check = plan(&service, &id, true, observe_only).await
                    .map_err(|_| Status::unavailable("read Manager plan"))?;
                let recheck_us = check.recheck_us;
                let wait_for_deadline = matches!(check.status, PlanStatus::Ready | PlanStatus::DecisionInFuture);
                let update = check.into_update()
                    .map_err(|_| Status::internal("invalid Manager target"))?;
                if previous.as_ref() != Some(&update) {
                    previous = Some(update.clone());
                    yield update;
                }
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = changes.changed() => delay = Duration::from_millis(25),
                    _ = retry_snapshot(recheck_us, delay, wait_for_deadline) => {
                        delay = (delay * 2).min(Duration::from_secs(1));
                    }
                }
            }
        };
        Ok(tonic::Response::new(Box::pin(updates)))
    }
}

async fn retry_snapshot(deadline: Option<i64>, delay: Duration, wait_for_deadline: bool) {
    if let Some(deadline) = deadline {
        let remaining = deadline.saturating_sub(crate::native_futures::now_us());
        let remaining = Duration::from_micros(remaining.max(0) as u64);
        tokio::time::sleep(if wait_for_deadline {
            remaining
        } else {
            delay.min(remaining)
        })
        .await;
    } else {
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::manager_plans_client::ManagerPlansClient;

    #[test]
    fn empty_http_plans_expose_distinct_statuses_without_a_target() {
        for (status, name) in [
            (PlanStatus::NoExternalDemand, "no_external_demand"),
            (PlanStatus::AccountNotReady, "account_not_ready"),
            (PlanStatus::LeaseExpired, "lease_expired"),
        ] {
            let check = PlanCheck {
                status,
                batch_id: "batch-7".into(),
                target_revision: 7,
                target: None,
                recheck_us: None,
            };
            let response = check.http_response(None).unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(response.headers()["x-cta-plan-status"], name);
            assert_eq!(response.headers()["x-cta-plan-batch-id"], "batch-7");
            assert_eq!(response.headers()["x-cta-plan-target-revision"], "7");
        }
    }

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
