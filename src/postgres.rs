use std::path::Path;

use anyhow::{Context, Result};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::{FeeRates, SourceConfig, validate_fee_rates};
use crate::model::{DecodeFailure, SignalBboLeg, UniformOrderEvent};
use crate::snapshot::{
    PositionSnapshot, SnapshotPosition, StrategyPositionSnapshot, StrategySnapshotPosition,
};

const STREAM_NAME: &str = "uniform_orders";

pub async fn connect(database_url: &str, max_connections: u32) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await
        .context("failed to connect to local PostgreSQL")
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    // Keep this call recompiled when the embedded migration set changes.
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("failed to apply PostgreSQL migrations")
}

pub async fn register_sources(pool: &PgPool, sources: &[SourceConfig]) -> Result<()> {
    for source in sources {
        // Seed estimated_fee_rate only on first insert. Later operator edits in
        // PostgreSQL must not be overwritten by toml on every cta_web restart.
        let seed_fee_rates = source.nav_fee_rates().unwrap_or(FeeRates {
            maker: 0.0004,
            taker: 0.0004,
        });
        sqlx::query(
            r#"
            INSERT INTO cta_order_sources (
                source_id, account_label, venue_label, rocksdb_path, enabled,
                estimated_fee_rate, maker_fee_rate, taker_fee_rate,
                theoretical_twap_fee_rate
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (source_id) DO UPDATE SET
                account_label = EXCLUDED.account_label,
                venue_label = EXCLUDED.venue_label,
                rocksdb_path = EXCLUDED.rocksdb_path,
                enabled = EXCLUDED.enabled,
                updated_at = now()
            "#,
        )
        .bind(&source.id)
        .bind(source.display_name())
        .bind(&source.venue)
        .bind(path_text(&source.rocksdb_path))
        .bind(source.enabled)
        .bind(seed_fee_rates.taker)
        .bind(seed_fee_rates.maker)
        .bind(seed_fee_rates.taker)
        .bind(seed_fee_rates.maker * 0.5 + seed_fee_rates.taker * 0.5)
        .execute(pool)
        .await
        .with_context(|| format!("failed to register source {}", source.id))?;
    }
    Ok(())
}

/// Load per-source estimated fee rates from PostgreSQL.
/// Missing rows are omitted; callers should fall back to toml defaults.
pub async fn load_fee_rates(pool: &PgPool) -> Result<std::collections::BTreeMap<String, FeeRates>> {
    let rows = sqlx::query(
        r#"
        SELECT source_id, maker_fee_rate, taker_fee_rate
        FROM cta_order_sources
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to load estimated fee rates")?;
    let mut out = std::collections::BTreeMap::new();
    for row in rows {
        let source_id: String = row
            .try_get("source_id")
            .context("failed to decode source_id for estimated fee rate")?;
        let rates = FeeRates {
            maker: row
                .try_get("maker_fee_rate")
                .with_context(|| format!("failed to decode maker_fee_rate for {source_id}"))?,
            taker: row
                .try_get("taker_fee_rate")
                .with_context(|| format!("failed to decode taker_fee_rate for {source_id}"))?,
        };
        validate_fee_rates(rates)?;
        out.insert(source_id, rates);
    }
    Ok(out)
}

pub async fn load_fee_rate(pool: &PgPool, source_id: &str) -> Result<Option<FeeRates>> {
    let row = sqlx::query(
        r#"
        SELECT maker_fee_rate, taker_fee_rate
        FROM cta_order_sources
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load fee rates for {source_id}"))?;
    row.map(|row| {
        let rates = FeeRates {
            maker: row.try_get("maker_fee_rate")?,
            taker: row.try_get("taker_fee_rate")?,
        };
        validate_fee_rates(rates)?;
        Ok(rates)
    })
    .transpose()
}

pub async fn load_theoretical_twap_fee_rates(
    pool: &PgPool,
) -> Result<std::collections::BTreeMap<String, f64>> {
    let rows = sqlx::query(
        r#"
        SELECT source_id, theoretical_twap_fee_rate
        FROM cta_order_sources
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to load theoretical TWAP fee rates")?;
    let mut out = std::collections::BTreeMap::new();
    for row in rows {
        let source_id: String = row.try_get("source_id")?;
        let fee_rate: f64 = row.try_get("theoretical_twap_fee_rate")?;
        validate_theoretical_twap_fee_rate(fee_rate)?;
        out.insert(source_id, fee_rate);
    }
    Ok(out)
}

pub async fn load_theoretical_twap_fee_rate(pool: &PgPool, source_id: &str) -> Result<Option<f64>> {
    let fee_rate = sqlx::query_scalar(
        r#"
        SELECT theoretical_twap_fee_rate
        FROM cta_order_sources
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load theoretical TWAP fee rate for {source_id}"))?;
    if let Some(fee_rate) = fee_rate {
        validate_theoretical_twap_fee_rate(fee_rate)?;
    }
    Ok(fee_rate)
}

pub async fn save_fee_rates(
    pool: &PgPool,
    source_id: &str,
    rates: FeeRates,
    theoretical_twap_fee_rate: f64,
) -> Result<()> {
    validate_fee_rates(rates)?;
    validate_theoretical_twap_fee_rate(theoretical_twap_fee_rate)?;
    let result = sqlx::query(
        r#"
        UPDATE cta_order_sources
        SET maker_fee_rate = $2,
            taker_fee_rate = $3,
            estimated_fee_rate = $3,
            theoretical_twap_fee_rate = $4,
            updated_at = now()
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .bind(rates.maker)
    .bind(rates.taker)
    .bind(theoretical_twap_fee_rate)
    .execute(pool)
    .await
    .with_context(|| format!("failed to save account fee rates for {source_id}"))?;
    if result.rows_affected() == 0 {
        anyhow::bail!("source {source_id} is not registered in cta_order_sources");
    }
    Ok(())
}

pub async fn save_estimated_fee_rate(
    pool: &PgPool,
    source_id: &str,
    estimated_fee_rate: f64,
) -> Result<()> {
    save_fee_rates(
        pool,
        source_id,
        FeeRates {
            maker: estimated_fee_rate,
            taker: estimated_fee_rate,
        },
        estimated_fee_rate,
    )
    .await
}

fn validate_theoretical_twap_fee_rate(fee_rate: f64) -> Result<()> {
    if !fee_rate.is_finite() {
        anyhow::bail!("theoretical_twap_fee_rate must be finite");
    }
    Ok(())
}

pub async fn load_checkpoint(pool: &PgPool, source_id: &str) -> Result<Option<i64>> {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT next_ts_us
        FROM cta_ingestion_checkpoints
        WHERE source_id = $1 AND stream_name = $2
        "#,
    )
    .bind(source_id)
    .bind(STREAM_NAME)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load checkpoint for {source_id}"))
}

pub async fn create_position_snapshot(
    pool: &PgPool,
    snapshot: &PositionSnapshot,
    note: Option<&str>,
) -> Result<()> {
    snapshot.validate()?;
    let mut transaction = pool.begin().await.with_context(|| {
        format!(
            "failed to begin snapshot transaction for {}",
            snapshot.source_id
        )
    })?;
    sqlx::query(
        r#"
        INSERT INTO cta_position_snapshots (source_id, snapshot_ts_us, note)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(&snapshot.source_id)
    .bind(snapshot.snapshot_ts_us)
    .bind(note)
    .execute(&mut *transaction)
    .await
    .with_context(|| {
        format!(
            "failed to create immutable position snapshot source={} ts_us={}",
            snapshot.source_id, snapshot.snapshot_ts_us
        )
    })?;

    for position in &snapshot.positions {
        sqlx::query(
            r#"
            INSERT INTO cta_position_snapshot_entries (
                source_id, snapshot_ts_us, symbol, venue_code, quantity, reference_price
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(&snapshot.source_id)
        .bind(snapshot.snapshot_ts_us)
        .bind(&position.symbol)
        .bind(position.venue_code)
        .bind(position.quantity)
        .bind(position.reference_price)
        .execute(&mut *transaction)
        .await
        .with_context(|| {
            format!(
                "failed to insert snapshot position source={} symbol={} venue={}",
                snapshot.source_id, position.symbol, position.venue_code
            )
        })?;
    }

    transaction.commit().await.with_context(|| {
        format!(
            "failed to commit position snapshot source={} ts_us={}",
            snapshot.source_id, snapshot.snapshot_ts_us
        )
    })
}

pub async fn load_latest_position_snapshot(
    pool: &PgPool,
    source_id: &str,
) -> Result<Option<PositionSnapshot>> {
    let snapshot_ts_us = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT snapshot_ts_us
        FROM cta_position_snapshots
        WHERE source_id = $1
        ORDER BY snapshot_ts_us DESC
        LIMIT 1
        "#,
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load latest position snapshot for {source_id}"))?;
    let Some(snapshot_ts_us) = snapshot_ts_us else {
        return Ok(None);
    };

    let rows = sqlx::query(
        r#"
        SELECT symbol, venue_code, quantity, reference_price
        FROM cta_position_snapshot_entries
        WHERE source_id = $1 AND snapshot_ts_us = $2
        ORDER BY symbol, venue_code
        "#,
    )
    .bind(source_id)
    .bind(snapshot_ts_us)
    .fetch_all(pool)
    .await
    .with_context(|| {
        format!("failed to load snapshot entries for {source_id} at {snapshot_ts_us}")
    })?;
    let positions = rows
        .into_iter()
        .map(|row| {
            Ok(SnapshotPosition {
                symbol: row.try_get("symbol")?,
                venue_code: row.try_get("venue_code")?,
                quantity: row.try_get("quantity")?,
                reference_price: row.try_get("reference_price")?,
            })
        })
        .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
    let snapshot = PositionSnapshot {
        source_id: source_id.to_string(),
        snapshot_ts_us,
        positions,
    };
    snapshot
        .validate()
        .with_context(|| format!("invalid stored position snapshot for {source_id}"))?;
    Ok(Some(snapshot))
}

pub async fn create_strategy_position_snapshot(
    pool: &PgPool,
    snapshot: &StrategyPositionSnapshot,
    note: Option<&str>,
) -> Result<()> {
    snapshot.validate()?;
    let mut transaction = pool.begin().await.with_context(|| {
        format!(
            "failed to begin strategy position snapshot transaction for {}",
            snapshot.source_id
        )
    })?;
    sqlx::query(
        r#"
        INSERT INTO cta_strategy_position_snapshots (source_id, snapshot_ts_us, note)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(&snapshot.source_id)
    .bind(snapshot.snapshot_ts_us)
    .bind(note)
    .execute(&mut *transaction)
    .await
    .with_context(|| {
        format!(
            "failed to create immutable strategy position snapshot source={} ts_us={}",
            snapshot.source_id, snapshot.snapshot_ts_us
        )
    })?;

    for position in &snapshot.positions {
        sqlx::query(
            r#"
            INSERT INTO cta_strategy_position_snapshot_entries (
                source_id, snapshot_ts_us, strategy_name, symbol, venue_code, quantity, reference_price
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(&snapshot.source_id)
        .bind(snapshot.snapshot_ts_us)
        .bind(&position.strategy_name)
        .bind(&position.symbol)
        .bind(position.venue_code)
        .bind(position.quantity)
        .bind(position.reference_price)
        .execute(&mut *transaction)
        .await
        .with_context(|| {
            format!(
                "failed to insert strategy position snapshot source={} strategy={} symbol={} venue={}",
                snapshot.source_id, position.strategy_name, position.symbol, position.venue_code
            )
        })?;
    }

    transaction.commit().await.with_context(|| {
        format!(
            "failed to commit strategy position snapshot source={} ts_us={}",
            snapshot.source_id, snapshot.snapshot_ts_us
        )
    })
}

pub async fn load_latest_strategy_position_snapshot(
    pool: &PgPool,
    source_id: &str,
) -> Result<Option<StrategyPositionSnapshot>> {
    let snapshot_ts_us = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT snapshot_ts_us
        FROM cta_strategy_position_snapshots
        WHERE source_id = $1
        ORDER BY snapshot_ts_us DESC
        LIMIT 1
        "#,
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to load latest strategy position snapshot for {source_id}"))?;
    let Some(snapshot_ts_us) = snapshot_ts_us else {
        return Ok(None);
    };

    let rows = sqlx::query(
        r#"
        SELECT strategy_name, symbol, venue_code, quantity, reference_price
        FROM cta_strategy_position_snapshot_entries
        WHERE source_id = $1 AND snapshot_ts_us = $2
        ORDER BY strategy_name, symbol, venue_code
        "#,
    )
    .bind(source_id)
    .bind(snapshot_ts_us)
    .fetch_all(pool)
    .await
    .with_context(|| {
        format!(
            "failed to load strategy position snapshot entries for {source_id} at {snapshot_ts_us}"
        )
    })?;
    let positions = rows
        .into_iter()
        .map(|row| {
            Ok(StrategySnapshotPosition {
                strategy_name: row.try_get("strategy_name")?,
                symbol: row.try_get("symbol")?,
                venue_code: row.try_get("venue_code")?,
                quantity: row.try_get("quantity")?,
                reference_price: row.try_get("reference_price")?,
            })
        })
        .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
    let snapshot = StrategyPositionSnapshot {
        source_id: source_id.to_string(),
        snapshot_ts_us,
        positions,
    };
    snapshot
        .validate()
        .with_context(|| format!("invalid stored strategy position snapshot for {source_id}"))?;
    Ok(Some(snapshot))
}

pub async fn begin_exec_order_config_audit(
    pool: &PgPool,
    source_id: &str,
    strategy_name: &str,
    client_addr: &str,
    expected_updated_at_us: Option<i64>,
    previous_order_parameters_json: &str,
    requested_order_parameters_json: &str,
) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO cta_exec_order_config_audit (
            source_id,
            strategy_name,
            client_addr,
            expected_updated_at_us,
            previous_order_parameters,
            requested_order_parameters,
            status
        )
        VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb, 'pending')
        RETURNING audit_id
        "#,
    )
    .bind(source_id)
    .bind(strategy_name)
    .bind(client_addr)
    .bind(expected_updated_at_us)
    .bind(previous_order_parameters_json)
    .bind(requested_order_parameters_json)
    .fetch_one(pool)
    .await
    .with_context(|| {
        format!(
            "failed to begin Exec order config audit source={source_id} strategy={strategy_name}"
        )
    })
}

pub async fn complete_exec_order_config_audit(
    pool: &PgPool,
    audit_id: i64,
    status: &str,
    result_updated_at_us: Option<i64>,
    error: Option<&str>,
) -> Result<()> {
    if !matches!(status, "applied" | "failed") {
        anyhow::bail!("invalid Exec order config audit status: {status}");
    }
    sqlx::query(
        r#"
        UPDATE cta_exec_order_config_audit
        SET status = $2,
            result_updated_at_us = $3,
            error = $4,
            completed_at = now()
        WHERE audit_id = $1 AND status = 'pending'
        "#,
    )
    .bind(audit_id)
    .bind(status)
    .bind(result_updated_at_us)
    .bind(error)
    .execute(pool)
    .await
    .with_context(|| format!("failed to complete Exec order config audit id={audit_id}"))?;
    Ok(())
}

pub async fn persist_poll(
    pool: &PgPool,
    source_id: &str,
    scan_start_ts_us: i64,
    next_ts_us: i64,
    events: &[UniformOrderEvent],
    failures: &[DecodeFailure],
) -> Result<()> {
    let mut transaction = pool
        .begin()
        .await
        .with_context(|| format!("failed to begin ingestion transaction for {source_id}"))?;

    for event in events {
        insert_event(&mut transaction, source_id, event).await?;
    }
    upsert_symbol_index(&mut transaction, source_id, events).await?;
    for failure in failures {
        sqlx::query(
            r#"
            INSERT INTO cta_ingestion_failures (
                source_id, stream_name, record_key, wire_payload, error,
                first_seen_at, last_seen_at, occurrence_count
            )
            VALUES ($1, $2, $3, $4, $5, now(), now(), 1)
            ON CONFLICT (source_id, stream_name, record_key) DO UPDATE SET
                wire_payload = EXCLUDED.wire_payload,
                error = EXCLUDED.error,
                last_seen_at = now(),
                occurrence_count = cta_ingestion_failures.occurrence_count + 1
            "#,
        )
        .bind(source_id)
        .bind(STREAM_NAME)
        .bind(&failure.record_key)
        .bind(&failure.wire_payload)
        .bind(&failure.error)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("failed to persist decode failure for {source_id}"))?;
    }

    sqlx::query(
        r#"
        INSERT INTO cta_ingestion_checkpoints (
            source_id, stream_name, next_ts_us, last_scan_start_ts_us,
            last_event_count, last_decode_failure_count, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, now())
        ON CONFLICT (source_id, stream_name) DO UPDATE SET
            next_ts_us = GREATEST(cta_ingestion_checkpoints.next_ts_us, EXCLUDED.next_ts_us),
            last_scan_start_ts_us = EXCLUDED.last_scan_start_ts_us,
            last_event_count = EXCLUDED.last_event_count,
            last_decode_failure_count = EXCLUDED.last_decode_failure_count,
            updated_at = now()
        "#,
    )
    .bind(source_id)
    .bind(STREAM_NAME)
    .bind(next_ts_us)
    .bind(scan_start_ts_us)
    .bind(i64::try_from(events.len()).unwrap_or(i64::MAX))
    .bind(i64::try_from(failures.len()).unwrap_or(i64::MAX))
    .execute(&mut *transaction)
    .await
    .with_context(|| format!("failed to advance checkpoint for {source_id}"))?;

    sqlx::query(
        r#"
        UPDATE cta_order_sources
        SET last_success_at = now(), last_error = NULL, updated_at = now()
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .execute(&mut *transaction)
    .await
    .with_context(|| format!("failed to update success status for {source_id}"))?;

    transaction
        .commit()
        .await
        .with_context(|| format!("failed to commit ingestion for {source_id}"))
}

#[derive(Clone, Debug)]
pub struct SourceSymbol {
    pub source_id: String,
    pub symbol: String,
    pub venue_code: i16,
    pub venue: String,
    pub first_event_ts_us: Option<i64>,
    pub first_fill_ts_us: Option<i64>,
    pub last_fill_ts_us: Option<i64>,
}

#[derive(Clone, Debug)]
struct SourceSymbolIndex {
    venue: String,
    first_event_ts_us: i64,
    first_fill_ts_us: Option<i64>,
    last_fill_ts_us: Option<i64>,
}

async fn upsert_symbol_index(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: &str,
    events: &[UniformOrderEvent],
) -> Result<()> {
    for ((symbol, venue_code), index) in symbol_index_deltas(events) {
        sqlx::query(
            r#"
            INSERT INTO cta_source_symbols (
                source_id, symbol, venue_code, venue,
                first_event_ts_us, first_fill_ts_us, last_fill_ts_us, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, now())
            ON CONFLICT (source_id, symbol, venue_code) DO UPDATE SET
                venue = EXCLUDED.venue,
                first_event_ts_us = LEAST(
                    cta_source_symbols.first_event_ts_us, EXCLUDED.first_event_ts_us
                ),
                first_fill_ts_us = LEAST(
                    cta_source_symbols.first_fill_ts_us, EXCLUDED.first_fill_ts_us
                ),
                last_fill_ts_us = GREATEST(
                    cta_source_symbols.last_fill_ts_us, EXCLUDED.last_fill_ts_us
                ),
                updated_at = now()
            "#,
        )
        .bind(source_id)
        .bind(&symbol)
        .bind(venue_code)
        .bind(&index.venue)
        .bind(index.first_event_ts_us)
        .bind(index.first_fill_ts_us)
        .bind(index.last_fill_ts_us)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("failed to index symbol {symbol} for {source_id}"))?;
    }
    Ok(())
}

/// Refresh the durable symbol index from the retained RocksDB event history.
pub async fn refresh_source_symbol_index(
    pool: &PgPool,
    histories: &crate::nav::NavSourceHistories,
) -> Result<()> {
    let mut transaction = pool
        .begin()
        .await
        .context("failed to begin symbol index transaction")?;
    for (source_id, history) in histories {
        upsert_symbol_index(&mut transaction, source_id, history.events()).await?;
    }
    transaction
        .commit()
        .await
        .context("failed to commit source symbol index")
}

fn symbol_index_deltas(
    events: &[UniformOrderEvent],
) -> std::collections::BTreeMap<(String, i16), SourceSymbolIndex> {
    let mut index = std::collections::BTreeMap::<(String, i16), SourceSymbolIndex>::new();
    for event in events {
        if event.symbol.is_empty() {
            continue;
        }
        let entry = index
            .entry((event.symbol.clone(), event.venue_code))
            .or_insert_with(|| SourceSymbolIndex {
                venue: event.venue.clone(),
                first_event_ts_us: event.event_ts_us,
                first_fill_ts_us: None,
                last_fill_ts_us: None,
            });
        entry.first_event_ts_us = entry.first_event_ts_us.min(event.event_ts_us);
        if event.amount_update > 0.0 {
            let fill_ts_us = crate::nav::fifo_ts_us(event);
            entry.first_fill_ts_us = Some(
                entry
                    .first_fill_ts_us
                    .map_or(fill_ts_us, |ts| ts.min(fill_ts_us)),
            );
            entry.last_fill_ts_us = Some(
                entry
                    .last_fill_ts_us
                    .map_or(fill_ts_us, |ts| ts.max(fill_ts_us)),
            );
        }
    }
    index
}

pub async fn load_source_symbols(
    pool: &PgPool,
    source_ids: &[String],
) -> Result<Vec<SourceSymbol>> {
    let rows = sqlx::query(
        r#"
        SELECT source_id, symbol, venue_code, venue,
               first_event_ts_us, first_fill_ts_us, last_fill_ts_us
        FROM cta_source_symbols
        WHERE source_id = ANY($1)
        ORDER BY source_id, symbol, venue_code
        "#,
    )
    .bind(source_ids)
    .fetch_all(pool)
    .await
    .context("failed to load source symbols")?;
    rows.into_iter()
        .map(|row| {
            Ok(SourceSymbol {
                source_id: row.try_get("source_id")?,
                symbol: row.try_get("symbol")?,
                venue_code: row.try_get("venue_code")?,
                venue: row.try_get("venue")?,
                first_event_ts_us: row.try_get("first_event_ts_us")?,
                first_fill_ts_us: row.try_get("first_fill_ts_us")?,
                last_fill_ts_us: row.try_get("last_fill_ts_us")?,
            })
        })
        .collect()
}

pub async fn record_source_error(pool: &PgPool, source_id: &str, error: &str) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE cta_order_sources
        SET last_error = $2, last_error_at = now(), updated_at = now()
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .bind(error)
    .execute(pool)
    .await
    .with_context(|| format!("failed to record source error for {source_id}"))?;
    Ok(())
}

async fn insert_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: &str,
    event: &UniformOrderEvent,
) -> Result<()> {
    let open = event.signal_open;
    let hedge = event.signal_hedge;
    sqlx::query(
        r#"
        INSERT INTO cta_uniform_order_events (
            source_id, record_key, event_ts_us, recv_ts_us, symbol,
            create_ts_us, update_ts_us, signal_ts_us, submit_ts_us,
            local_ts_us, market_ts_us, client_order_id,
            venue_code, venue, order_type_code, order_type, side_code, side,
            price, price_offset, amount_initial, amount_update,
            status_code, status, from_key, from_key_text, bbo_spread,
            signal_open_venue_code, signal_open_ts_us,
            signal_open_bid_price, signal_open_bid_quantity,
            signal_open_ask_price, signal_open_ask_quantity,
            signal_hedge_venue_code, signal_hedge_ts_us,
            signal_hedge_bid_price, signal_hedge_bid_quantity,
            signal_hedge_ask_price, signal_hedge_ask_quantity,
            wire_version, wire_payload, ingested_at
        )
        VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9,
            $10, $11, $12,
            $13, $14, $15, $16, $17, $18,
            $19, $20, $21, $22,
            $23, $24, $25, $26, $27,
            $28, $29, $30, $31, $32, $33,
            $34, $35, $36, $37, $38, $39,
            2, $40, now()
        )
        ON CONFLICT (source_id, record_key) DO UPDATE SET
            event_ts_us = EXCLUDED.event_ts_us,
            recv_ts_us = EXCLUDED.recv_ts_us,
            symbol = EXCLUDED.symbol,
            create_ts_us = EXCLUDED.create_ts_us,
            update_ts_us = EXCLUDED.update_ts_us,
            signal_ts_us = EXCLUDED.signal_ts_us,
            submit_ts_us = EXCLUDED.submit_ts_us,
            local_ts_us = EXCLUDED.local_ts_us,
            market_ts_us = EXCLUDED.market_ts_us,
            client_order_id = EXCLUDED.client_order_id,
            venue_code = EXCLUDED.venue_code,
            venue = EXCLUDED.venue,
            order_type_code = EXCLUDED.order_type_code,
            order_type = EXCLUDED.order_type,
            side_code = EXCLUDED.side_code,
            side = EXCLUDED.side,
            price = EXCLUDED.price,
            price_offset = EXCLUDED.price_offset,
            amount_initial = EXCLUDED.amount_initial,
            amount_update = EXCLUDED.amount_update,
            status_code = EXCLUDED.status_code,
            status = EXCLUDED.status,
            from_key = EXCLUDED.from_key,
            from_key_text = EXCLUDED.from_key_text,
            bbo_spread = EXCLUDED.bbo_spread,
            signal_open_venue_code = EXCLUDED.signal_open_venue_code,
            signal_open_ts_us = EXCLUDED.signal_open_ts_us,
            signal_open_bid_price = EXCLUDED.signal_open_bid_price,
            signal_open_bid_quantity = EXCLUDED.signal_open_bid_quantity,
            signal_open_ask_price = EXCLUDED.signal_open_ask_price,
            signal_open_ask_quantity = EXCLUDED.signal_open_ask_quantity,
            signal_hedge_venue_code = EXCLUDED.signal_hedge_venue_code,
            signal_hedge_ts_us = EXCLUDED.signal_hedge_ts_us,
            signal_hedge_bid_price = EXCLUDED.signal_hedge_bid_price,
            signal_hedge_bid_quantity = EXCLUDED.signal_hedge_bid_quantity,
            signal_hedge_ask_price = EXCLUDED.signal_hedge_ask_price,
            signal_hedge_ask_quantity = EXCLUDED.signal_hedge_ask_quantity,
            wire_version = EXCLUDED.wire_version,
            wire_payload = EXCLUDED.wire_payload,
            ingested_at = now()
        "#,
    )
    .bind(source_id)
    .bind(&event.record_key)
    .bind(event.event_ts_us)
    .bind(event.recv_ts_us)
    .bind(&event.symbol)
    .bind(event.create_ts_us)
    .bind(event.update_ts_us)
    .bind(event.signal_ts_us)
    .bind(event.submit_ts_us)
    .bind(event.local_ts_us)
    .bind(event.market_ts_us)
    .bind(event.client_order_id)
    .bind(event.venue_code)
    .bind(&event.venue)
    .bind(event.order_type_code)
    .bind(&event.order_type)
    .bind(event.side_code)
    .bind(&event.side)
    .bind(event.price)
    .bind(event.price_offset)
    .bind(event.amount_initial)
    .bind(event.amount_update)
    .bind(event.status_code)
    .bind(&event.status)
    .bind(&event.from_key)
    .bind(&event.from_key_text)
    .bind(&event.bbo_spread)
    .bind(leg_value(open, |leg| leg.venue_code))
    .bind(leg_value(open, |leg| leg.ts_us))
    .bind(leg_value(open, |leg| leg.bid_price))
    .bind(leg_value(open, |leg| leg.bid_quantity))
    .bind(leg_value(open, |leg| leg.ask_price))
    .bind(leg_value(open, |leg| leg.ask_quantity))
    .bind(leg_value(hedge, |leg| leg.venue_code))
    .bind(leg_value(hedge, |leg| leg.ts_us))
    .bind(leg_value(hedge, |leg| leg.bid_price))
    .bind(leg_value(hedge, |leg| leg.bid_quantity))
    .bind(leg_value(hedge, |leg| leg.ask_price))
    .bind(leg_value(hedge, |leg| leg.ask_quantity))
    .bind(&event.wire_payload)
    .execute(&mut **transaction)
    .await
    .with_context(|| {
        format!(
            "failed to upsert uniform order source={} key={}",
            source_id, event.record_key
        )
    })?;
    Ok(())
}

fn leg_value<T: Copy>(
    leg: Option<SignalBboLeg>,
    project: impl FnOnce(SignalBboLeg) -> T,
) -> Option<T> {
    leg.map(project)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        symbol: &str,
        venue_code: i16,
        event_ts_us: i64,
        update_ts_us: i64,
        amount_update: f64,
    ) -> UniformOrderEvent {
        UniformOrderEvent {
            record_key: format!("{symbol}:{event_ts_us}"),
            event_ts_us,
            recv_ts_us: event_ts_us,
            symbol: symbol.to_string(),
            create_ts_us: 0,
            update_ts_us,
            signal_ts_us: 0,
            submit_ts_us: 0,
            local_ts_us: 0,
            market_ts_us: 0,
            client_order_id: 0,
            venue_code,
            venue: format!("venue-{venue_code}"),
            order_type_code: 1,
            order_type: "LIMIT".to_string(),
            side_code: 1,
            side: "BUY".to_string(),
            price: 1.0,
            price_offset: 0.0,
            amount_initial: amount_update,
            amount_update,
            status_code: 3,
            status: "FILLED".to_string(),
            from_key: Vec::new(),
            from_key_text: String::new(),
            bbo_spread: String::new(),
            signal_open: None,
            signal_hedge: None,
            wire_payload: Vec::new(),
        }
    }

    #[test]
    fn symbol_index_keeps_earliest_event_and_fill_bounds() {
        let index = symbol_index_deltas(&[
            event("BTCUSDT", 1, 10, 12, 1.0),
            event("BTCUSDT", 1, 5, 0, 0.0),
            event("BTCUSDT", 1, 8, 7, 2.0),
            event("ETHUSDT", 1, 3, 0, 0.0),
        ]);

        let btc = &index[&("BTCUSDT".to_string(), 1)];
        assert_eq!(btc.first_event_ts_us, 5);
        assert_eq!(btc.first_fill_ts_us, Some(7));
        assert_eq!(btc.last_fill_ts_us, Some(12));
        let eth = &index[&("ETHUSDT".to_string(), 1)];
        assert_eq!(eth.first_event_ts_us, 3);
        assert_eq!(eth.first_fill_ts_us, None);
        assert_eq!(eth.last_fill_ts_us, None);
        assert_eq!(index.len(), 2);
    }
}
