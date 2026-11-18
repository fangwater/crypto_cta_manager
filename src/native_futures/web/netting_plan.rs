//! Dedicated source-scoped readers see only the calculated account target.
//! The Exec process owns target admission and order submission.
use super::{netting::account_lots, netting::observed, *};
use crate::native_futures::netting::{Quantities, store};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeSet;

const MAX_LEASE_US: i64 = 60_000_000;

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

pub(super) fn routes() -> Router<Service> {
    Router::new().route("/api/native/sources/{id}/netting/ctaker-plan", get(read))
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

async fn read(
    State(service): State<Service>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let Some(value) = headers
        .get("X-CTA-Plan-Token")
        .and_then(|v| v.to_str().ok())
        .filter(|v| valid_token(v))
    else {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "invalid plan credential".into(),
        ));
    };
    if !service
        .config
        .ctaker_plan_readers
        .get(&id)
        .is_some_and(|stored| token_mac(value).verify_slice(stored).is_ok())
    {
        return Err(forbidden());
    }
    let reply = plan(&service, &id).await?;
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

async fn plan(service: &Service, id: &str) -> Result<Option<Plan>, ApiError> {
    let Some(state) = store::load(&service.pool, id).await? else {
        return Ok(None);
    };
    let (Some(day), Some(batch), Some(group)) = (
        state.target_trading_day.as_ref(),
        state.batch_id.as_ref(),
        state
            .last_strategy_group
            .as_ref()
            .and_then(|g| state.strategy_groups.get(g)),
    ) else {
        return Ok(None);
    };
    let now = crate::native_futures::now_us();
    let expires_at_us = group.decision_us.saturating_add(MAX_LEASE_US);
    if !state.external_demand_open || group.decision_us > now || expires_at_us <= now {
        return Ok(None);
    }
    let snapshot = observed(service, id).await?;
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
        return Ok(None);
    }
    let actual = account_lots(&snapshot, &state.contracts)?;
    if !state.account.iter().all(|(market, position)| {
        actual.get(market).copied().unwrap_or_default() == position.quantity()
    }) {
        return Ok(None);
    }
    let targets = state.execution_plan()?.net_targets;
    let current_revision = store::load(&service.pool, id).await?.map(|s| s.revision);
    if current_revision != Some(state.revision) || crate::native_futures::now_us() >= expires_at_us
    {
        return Ok(None);
    }
    Ok(Some(Plan {
        source_id: id.to_owned(),
        batch_id: batch.clone(),
        target_revision: state.target_revision,
        trading_day: day.clone(),
        decision_us: group.decision_us,
        expires_at_us,
        net_targets: targets,
    }))
}
