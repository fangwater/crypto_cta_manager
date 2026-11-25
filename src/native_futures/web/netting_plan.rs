//! Dedicated source-scoped readers see the complete strategy targets. Like
//! crypto, the Exec process owns netting, attribution, admission and orders;
//! delivery never waits on the Exec replica or Manager-side attribution.
use super::*;
use crate::native_futures::netting::{Strategies, store};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{collections::BTreeSet, pin::Pin, time::Duration};
use tonic::Status;

#[allow(dead_code)]
mod wire {
    tonic::include_proto!("cn_futures_target");
}
use wire::{
    ContractTarget, PlanStatus, PlanSubscription, PlanUpdate, PositionTarget, StrategyTarget,
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
        .exec_plan_readers
        .iter()
        .map(|(source, env)| {
            let value = std::env::var(env).context("missing exec plan reader credential")?;
            ensure!(valid_token(&value), "invalid exec plan reader credential");
            let hash = hash(&value);
            ensure!(hashes.insert(hash), "exec plan reader credentials must be distinct from all source sync and plan readers");
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
    mac.update(b"cn-futures/exec-plan-reader");
    mac
}

pub(super) fn routes(service: Service) -> Router {
    let stream = ManagerPlansServer::new(PlanStream(service.clone()))
        .max_decoding_message_size(MAX_MESSAGE)
        .max_encoding_message_size(MAX_MESSAGE);
    Router::new()
        .route("/api/native/sources/{id}/netting/exec-plan", get(read))
        .with_state(service)
        .route_service("/cn_futures_target.ManagerPlans/Watch", stream)
}

#[derive(Serialize)]
struct Plan {
    source_id: String,
    batch_id: String,
    target_revision: i64,
    decision_us: i64,
    strategies: Strategies,
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

    fn http_response(self) -> Result<Response, ApiError> {
        let mut response = match self.target {
            Some(plan) => Json(plan).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
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
        .exec_plan_readers
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
    plan(&service, &id, false).await?.http_response()
}

async fn plan(service: &Service, id: &str, observe_only: bool) -> Result<PlanCheck, ApiError> {
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
    let payload = |decision_us| Plan {
        source_id: id.to_owned(),
        batch_id: batch.clone(),
        target_revision: state.target_revision,
        decision_us,
        strategies: state.targets.clone(),
    };
    if observe_only {
        // Batches keep the latest group decision; stop/retire carry event time.
        let observed = state.decision_us.max(state.plan_decision_us.unwrap_or(0));
        return Ok(check(
            PlanStatus::ObserveOnly,
            Some(payload(observed)),
            None,
        ));
    }
    let Some(decision_us) = state.exec_decision_us() else {
        return Ok(check(PlanStatus::MissingTradingDay, None, None));
    };
    if decision_us > crate::native_futures::now_us() {
        return Ok(check(PlanStatus::DecisionInFuture, None, Some(decision_us)));
    }
    // No lease: a ready plan stands until the next batch or member event
    // replaces it. An empty roster is a ready plan with zero strategy targets.
    Ok(check(PlanStatus::Ready, Some(payload(decision_us)), None))
}

impl TryFrom<Plan> for PositionTarget {
    type Error = anyhow::Error;

    fn try_from(plan: Plan) -> Result<Self> {
        Ok(Self {
            source_id: plan.source_id,
            batch_id: plan.batch_id,
            target_revision: plan.target_revision,
            decision_us: plan.decision_us.try_into()?,
            strategy_targets: plan
                .strategies
                .into_iter()
                .map(|(strategy, rows)| {
                    Ok(StrategyTarget {
                        strategy,
                        targets: rows
                            .into_iter()
                            .map(|(market, lots)| {
                                Ok(ContractTarget {
                                    market,
                                    lots: lots.try_into()?,
                                })
                            })
                            .collect::<Result<_>>()?,
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
                let check = plan(&service, &id, observe_only).await
                    .map_err(|_| Status::unavailable("read Manager plan"))?;
                let recheck_us = check.recheck_us;
                let wait_for_deadline = check.status == PlanStatus::DecisionInFuture;
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
            (PlanStatus::MissingTradingDay, "missing_trading_day"),
            (PlanStatus::DecisionInFuture, "decision_in_future"),
        ] {
            let check = PlanCheck {
                status,
                batch_id: "batch-7".into(),
                target_revision: 7,
                target: None,
                recheck_us: None,
            };
            let response = check.http_response().unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(response.headers()["x-cta-plan-status"], name);
            assert_eq!(response.headers()["x-cta-plan-batch-id"], "batch-7");
            assert_eq!(response.headers()["x-cta-plan-target-revision"], "7");
        }
    }

    #[test]
    fn an_empty_roster_is_a_ready_flatten_plan_with_zero_strategy_targets() {
        let plan = || Plan {
            source_id: "source".into(),
            batch_id: "retire:drop".into(),
            target_revision: 9,
            decision_us: 1_794_000_000_000_000,
            strategies: Strategies::new(),
        };
        let target = PositionTarget::try_from(plan()).unwrap();
        assert!(target.strategy_targets.is_empty());
        assert_eq!(target.target_revision, 9);
        let check = PlanCheck {
            status: PlanStatus::Ready,
            batch_id: "retire:drop".into(),
            target_revision: 9,
            target: Some(plan()),
            recheck_us: None,
        };
        let update = check.into_update().unwrap();
        assert_eq!(update.status, PlanStatus::Ready as i32);
        assert!(update.target.unwrap().strategy_targets.is_empty());
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
