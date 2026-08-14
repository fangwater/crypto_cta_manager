use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use crate::config::{AppConfig, SourceConfig};
use crate::{nav, postgres};

const NO_STORE: [(header::HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];

#[derive(Clone, Debug, Serialize)]
pub struct DashboardSnapshot {
    pub generated_at_us: i64,
    pub generation_duration_ms: u64,
    pub refresh_interval_secs: u64,
    pub accounts: Vec<DashboardAccount>,
    pub report: nav::NavReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct DashboardAccount {
    pub source_id: String,
    pub account: String,
    pub venue: String,
    pub enabled: bool,
    pub gateway_prefix: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineSnapshot {
    pub generated_at_us: i64,
    pub generation_duration_ms: u64,
    pub report: nav::NavTimelineReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub source_count: usize,
    pub generated_at_us: i64,
    pub last_attempt_at_us: i64,
    pub refresh_interval_secs: u64,
    pub last_refresh_error: Option<String>,
}

#[derive(Debug)]
struct CacheState {
    dashboard: DashboardSnapshot,
    last_attempt_at_us: i64,
    last_refresh_error: Option<String>,
}

#[derive(Clone)]
struct WebState {
    cache: Arc<RwLock<CacheState>>,
    config: Arc<AppConfig>,
    pool: PgPool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    source_ids: Option<String>,
    symbols: Option<String>,
    max_points: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

struct ApiError(anyhow::Error);

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error!(error = ?self.0, "CTA web API request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal server error".to_string(),
            }),
        )
            .into_response()
    }
}

pub async fn serve(config: AppConfig, bind: SocketAddr, refresh_interval_secs: u64) -> Result<()> {
    if refresh_interval_secs == 0 {
        anyhow::bail!("web refresh interval must be greater than zero");
    }

    let database_url = config.database_url()?;
    let pool = postgres::connect(&database_url, config.database.max_connections).await?;
    let first_dashboard = build_dashboard(&config, &pool, refresh_interval_secs).await?;
    let cache = Arc::new(RwLock::new(CacheState {
        last_attempt_at_us: first_dashboard.generated_at_us,
        dashboard: first_dashboard,
        last_refresh_error: None,
    }));

    let refresh_cache = Arc::clone(&cache);
    let refresh_config = config.clone();
    let refresh_pool = pool.clone();
    tokio::spawn(async move {
        refresh_loop(
            refresh_config,
            refresh_pool,
            refresh_cache,
            refresh_interval_secs,
        )
        .await;
    });

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/dashboard", get(dashboard))
        .route("/api/timeline", get(timeline))
        .with_state(WebState {
            cache,
            config: Arc::new(config),
            pool,
        })
        .layer(TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind CTA web API to {bind}"))?;
    info!(%bind, refresh_interval_secs, "CTA web API started");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("CTA web API stopped unexpectedly")
}

async fn dashboard(State(state): State<WebState>) -> impl IntoResponse {
    let dashboard = state.cache.read().await.dashboard.clone();
    (NO_STORE, Json(dashboard))
}

async fn health(State(state): State<WebState>) -> impl IntoResponse {
    let cache = state.cache.read().await;
    let response = HealthResponse {
        status: if cache.last_refresh_error.is_some() {
            "degraded"
        } else {
            "ok"
        },
        source_count: cache.dashboard.report.source_count,
        generated_at_us: cache.dashboard.generated_at_us,
        last_attempt_at_us: cache.last_attempt_at_us,
        refresh_interval_secs: cache.dashboard.refresh_interval_secs,
        last_refresh_error: cache.last_refresh_error.clone(),
    };
    (NO_STORE, Json(response))
}

async fn timeline(
    State(state): State<WebState>,
    Query(query): Query<TimelineQuery>,
) -> Result<Response, ApiError> {
    let selected_source_ids = parse_csv(query.source_ids.as_deref(), false);
    let selected_sources = match resolve_sources(&state.config, &selected_source_ids) {
        Ok(sources) => sources,
        Err(message) => return Ok(bad_request(message)),
    };
    let selected_symbols = parse_csv(query.symbols.as_deref(), true);
    let start_ts_us = match query.start_ms {
        Some(value) => match milliseconds_to_microseconds(value, "startMs") {
            Ok(value) => Some(value),
            Err(message) => return Ok(bad_request(message)),
        },
        None => None,
    };
    let end_ts_us = match milliseconds_to_microseconds(
        query.end_ms.unwrap_or_else(|| unix_now_us() / 1_000),
        "endMs",
    ) {
        Ok(value) => value,
        Err(message) => return Ok(bad_request(message)),
    };
    if start_ts_us.is_some_and(|start_ts_us| end_ts_us < start_ts_us) {
        return Ok(bad_request(
            "endMs must be greater than or equal to startMs".to_string(),
        ));
    }

    let started = Instant::now();
    let mut snapshots = nav::SourcePositionSnapshots::new();
    for source in selected_sources {
        if let Some(snapshot) =
            postgres::load_latest_position_snapshot(&state.pool, &source.id).await?
        {
            snapshots.insert(source.id.clone(), snapshot);
        }
    }
    let request = nav::NavTimelineRequest {
        start_ts_us,
        end_ts_us,
        selected_source_ids,
        selected_symbols,
        max_points: query.max_points.unwrap_or(3_000).clamp(200, 10_000),
    };
    let config = Arc::clone(&state.config);
    let report = tokio::task::spawn_blocking(move || {
        nav::rebuild_nav_timeline_from_rocksdb_with_snapshots(&config, request, &snapshots)
    })
    .await
    .context("CTA timeline rebuild task failed")?;
    let report = match report {
        Ok(report) => report,
        Err(error) if is_timeline_request_error(&error) => {
            return Ok(bad_request(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    let generation_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    Ok((
        NO_STORE,
        Json(TimelineSnapshot {
            generated_at_us: unix_now_us(),
            generation_duration_ms,
            report,
        }),
    )
        .into_response())
}

fn parse_csv(value: Option<&str>, uppercase: bool) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if uppercase {
                value.to_ascii_uppercase()
            } else {
                value.to_string()
            }
        })
        .collect()
}

fn resolve_sources<'a>(
    config: &'a AppConfig,
    selected_source_ids: &[String],
) -> std::result::Result<Vec<&'a SourceConfig>, String> {
    let requested = selected_source_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if requested.len() != selected_source_ids.len() {
        return Err("sourceIds must not contain duplicates".to_string());
    }
    for source_id in &requested {
        let Some(source) = config.sources.iter().find(|source| source.id == *source_id) else {
            return Err(format!("sourceIds contains an unknown source: {source_id}"));
        };
        if !source.enabled {
            return Err(format!("sourceIds contains a disabled source: {source_id}"));
        }
    }
    let selected = config
        .sources
        .iter()
        .filter(|source| {
            source.enabled && (requested.is_empty() || requested.contains(source.id.as_str()))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("sourceIds selects no enabled sources".to_string());
    }
    Ok(selected)
}

fn milliseconds_to_microseconds(value: i64, field: &str) -> std::result::Result<i64, String> {
    if value < 0 {
        return Err(format!("{field} must not be negative"));
    }
    value
        .checked_mul(1_000)
        .ok_or_else(|| format!("{field} is too large"))
}

fn is_timeline_request_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("start timestamp")
        || message.starts_with("end timestamp")
        || message.starts_with("none of the requested symbols")
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse { error: message }),
    )
        .into_response()
}

async fn refresh_loop(
    config: AppConfig,
    pool: PgPool,
    cache: Arc<RwLock<CacheState>>,
    refresh_interval_secs: u64,
) {
    let period = Duration::from_secs(refresh_interval_secs);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let attempted_at_us = unix_now_us();
        match build_dashboard(&config, &pool, refresh_interval_secs).await {
            Ok(dashboard) => {
                info!(
                    source_count = dashboard.report.source_count,
                    fill_count = dashboard.report.aggregate.totals.fill_count,
                    duration_ms = dashboard.generation_duration_ms,
                    "refreshed CTA dashboard"
                );
                let mut state = cache.write().await;
                state.last_attempt_at_us = attempted_at_us;
                state.dashboard = dashboard;
                state.last_refresh_error = None;
            }
            Err(error) => {
                warn!(error = ?error, "failed to refresh CTA dashboard; retaining last good report");
                let mut state = cache.write().await;
                state.last_attempt_at_us = attempted_at_us;
                state.last_refresh_error = Some(error.to_string());
            }
        }
    }
}

async fn build_dashboard(
    config: &AppConfig,
    pool: &PgPool,
    refresh_interval_secs: u64,
) -> Result<DashboardSnapshot> {
    let started = Instant::now();
    let accounts = config
        .sources
        .iter()
        .map(|source| DashboardAccount {
            source_id: source.id.clone(),
            account: source.account.clone(),
            venue: source.venue.clone(),
            enabled: source.enabled,
            gateway_prefix: source.gateway_prefix.clone(),
        })
        .collect();
    let mut snapshots = nav::SourcePositionSnapshots::new();
    for source in config.sources.iter().filter(|source| source.enabled) {
        if let Some(snapshot) = postgres::load_latest_position_snapshot(pool, &source.id).await? {
            snapshots.insert(source.id.clone(), snapshot);
        }
    }

    let config = config.clone();
    let report = tokio::task::spawn_blocking(move || {
        nav::rebuild_nav_from_rocksdb_with_snapshots(&config, &[], &snapshots)
    })
    .await
    .context("CTA dashboard rebuild task failed")??;
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    Ok(DashboardSnapshot {
        generated_at_us: unix_now_us(),
        generation_duration_ms: duration_ms,
        refresh_interval_secs,
        accounts,
        report,
    })
}

fn unix_now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(?error, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(?error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_timestamp_is_positive_microseconds() {
        assert!(unix_now_us() > 1_000_000_000_000_000);
    }

    #[test]
    fn parses_csv_query_values_without_losing_source_identity() {
        assert_eq!(
            parse_csv(Some("trade01, trade02"), false),
            vec!["trade01", "trade02"]
        );
        assert_eq!(
            parse_csv(Some("btcusdt, ETHUSDT"), true),
            vec!["BTCUSDT", "ETHUSDT"]
        );
    }

    #[test]
    fn validates_millisecond_timestamp_conversion() {
        assert_eq!(milliseconds_to_microseconds(123, "startMs"), Ok(123_000));
        assert!(milliseconds_to_microseconds(-1, "startMs").is_err());
        assert!(milliseconds_to_microseconds(i64::MAX, "endMs").is_err());
    }
}
