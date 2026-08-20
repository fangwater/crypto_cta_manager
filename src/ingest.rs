use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use sqlx::postgres::PgPool;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::config::{AppConfig, IngestionConfig, SourceConfig};
use crate::model::{DecodeFailure, decode_uniform_order};
use crate::{postgres, rocks_source};

pub async fn run(config: AppConfig, once: bool, migrate_only: bool) -> Result<()> {
    let database_url = config.database_url()?;
    let pool = postgres::connect(&database_url, config.database.max_connections).await?;
    postgres::migrate(&pool).await?;
    postgres::register_sources(&pool, &config.sources).await?;

    if migrate_only {
        info!(
            sources = config.sources.len(),
            "migrations and source registration complete"
        );
        return Ok(());
    }

    let enabled_sources = config
        .sources
        .into_iter()
        .filter(|source| source.enabled)
        .collect::<Vec<_>>();
    if once {
        return run_once(pool, config.ingestion, enabled_sources).await;
    }
    run_continuously(pool, config.ingestion, enabled_sources).await
}

async fn run_once(
    pool: PgPool,
    defaults: IngestionConfig,
    sources: Vec<SourceConfig>,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    for source in sources {
        let pool = pool.clone();
        let defaults = defaults.clone();
        tasks.spawn(async move { sync_source_once(&pool, &defaults, &source).await });
    }

    let mut failures = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(summary)) => log_summary(&summary),
            Ok(Err(error)) => failures.push(format!("{error:#}")),
            Err(error) => failures.push(format!("source task failed: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "one or more sources failed: {}",
            failures.join("; ")
        ))
    }
}

async fn run_continuously(
    pool: PgPool,
    defaults: IngestionConfig,
    sources: Vec<SourceConfig>,
) -> Result<()> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    for source in sources {
        let pool = pool.clone();
        let defaults = defaults.clone();
        let shutdown = shutdown_rx.clone();
        tasks.spawn(async move { source_loop(pool, defaults, source, shutdown).await });
    }

    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for shutdown signal")?;
    info!("shutdown requested");
    let _ = shutdown_tx.send(true);

    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!(error = %error, "source worker exited with error"),
            Err(error) => error!(error = %error, "source worker task failed"),
        }
    }
    Ok(())
}

async fn source_loop(
    pool: PgPool,
    defaults: IngestionConfig,
    source: SourceConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let interval = Duration::from_secs(source.poll_interval_secs(&defaults));
    info!(source_id = %source.id, account = %source.display_name(), venue = %source.venue, ?interval, "source worker started");
    loop {
        match sync_source_once(&pool, &defaults, &source).await {
            Ok(summary) => log_summary(&summary),
            Err(error) => {
                let message = format!("{error:#}");
                error!(source_id = %source.id, error = %message, "source poll failed");
                if let Err(status_error) =
                    postgres::record_source_error(&pool, &source.id, &message).await
                {
                    warn!(source_id = %source.id, error = %status_error, "failed to persist source error status");
                }
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

#[derive(Debug)]
struct PollSummary {
    source_id: String,
    scan_start_ts_us: i64,
    scan_end_ts_us: i64,
    raw_records: usize,
    decoded_records: usize,
    decode_failures: usize,
}

async fn sync_source_once(
    pool: &PgPool,
    defaults: &IngestionConfig,
    source: &SourceConfig,
) -> Result<PollSummary> {
    let checkpoint = postgres::load_checkpoint(pool, &source.id).await?;
    let initial_start = source.start_ts_us.unwrap_or(0);
    let next_ts_us = checkpoint.unwrap_or(initial_start).max(initial_start);
    let now_ts_us = unix_time_us()?;
    let safety_lag_us = seconds_to_micros(defaults.safety_lag_secs)?;
    let scan_end_ts_us = now_ts_us.saturating_sub(safety_lag_us);
    if scan_end_ts_us <= next_ts_us {
        return Ok(PollSummary {
            source_id: source.id.clone(),
            scan_start_ts_us: next_ts_us,
            scan_end_ts_us,
            raw_records: 0,
            decoded_records: 0,
            decode_failures: 0,
        });
    }

    let overlap_us = seconds_to_micros(defaults.overlap_secs)?;
    let scan_start_ts_us = next_ts_us.saturating_sub(overlap_us).max(initial_start);
    let path = source.rocksdb_path.clone();
    let records = tokio::task::spawn_blocking(move || {
        rocks_source::read_uniform_orders(&path, scan_start_ts_us, scan_end_ts_us)
    })
    .await
    .with_context(|| format!("RocksDB reader task failed for {}", source.id))??;

    let raw_records = records.len();
    let mut events = Vec::with_capacity(raw_records);
    let mut failures = Vec::new();
    for record in records {
        match decode_uniform_order(&record.key, &record.value) {
            Ok(event) => events.push(event),
            Err(error) => failures.push(DecodeFailure {
                record_key: record.key,
                wire_payload: record.value,
                error: format!("{error:#}"),
            }),
        }
    }

    postgres::persist_poll(
        pool,
        &source.id,
        scan_start_ts_us,
        scan_end_ts_us,
        &events,
        &failures,
    )
    .await?;

    Ok(PollSummary {
        source_id: source.id.clone(),
        scan_start_ts_us,
        scan_end_ts_us,
        raw_records,
        decoded_records: events.len(),
        decode_failures: failures.len(),
    })
}

fn log_summary(summary: &PollSummary) {
    info!(
        source_id = %summary.source_id,
        scan_start_ts_us = summary.scan_start_ts_us,
        scan_end_ts_us = summary.scan_end_ts_us,
        raw_records = summary.raw_records,
        decoded_records = summary.decoded_records,
        decode_failures = summary.decode_failures,
        "source poll complete"
    );
}

fn unix_time_us() -> Result<i64> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_micros();
    i64::try_from(micros).context("current Unix timestamp exceeds i64")
}

fn seconds_to_micros(seconds: u64) -> Result<i64> {
    let micros = seconds
        .checked_mul(1_000_000)
        .context("duration in microseconds overflowed u64")?;
    i64::try_from(micros).context("duration in microseconds exceeds i64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_conversion_is_checked() {
        assert_eq!(seconds_to_micros(60).unwrap(), 60_000_000);
        assert!(seconds_to_micros(u64::MAX).is_err());
    }
}
