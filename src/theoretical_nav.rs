use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Postgres, Row, Transaction};
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::position_archive::{PositionArchive, PositionUpdateMsg};
use crate::twap::{TwapBar, TwapStore};

pub const EXECUTION_WINDOW_SECS: u64 = 300;
const EXECUTION_WINDOW_US: i64 = 300_000_000;
const MINUTE_US: i64 = 60_000_000;
const MARK_INTERVAL_US: i64 = 300_000_000;
const MARK_LOOKBACK_US: i64 = 10_000_000;
const MARK_MAX_AGE_US: i64 = 10_000_000;
const BAR_SETTLE_LAG_US: i64 = 5_000_000;
const MISSING_BAR_GRACE_US: i64 = 120_000_000;
const WORKER_INTERVAL_SECS: u64 = 5;
const PROCESS_BATCH_SIZE: i64 = 64;
const MAX_COMPLETIONS_PER_RUN: usize = 2_048;
const MAX_MARKS_PER_CALL: usize = 512;
const ZERO_EPSILON: f64 = 1e-12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TheoreticalNavPoint {
    pub ts_us: i64,
    pub nav_change_before_fee_quote: f64,
    pub nav_change_after_fee_quote: f64,
    pub estimated_trading_fee_quote: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TheoreticalNavTimeline {
    pub valuation: &'static str,
    pub execution_window_secs: u64,
    pub price_basis: &'static str,
    pub fee_basis: &'static str,
    pub available_from_us: Option<i64>,
    pub latest_point_ts_us: Option<i64>,
    pub points: Vec<TheoreticalNavPoint>,
    pub sampled: bool,
}

impl Default for TheoreticalNavTimeline {
    fn default() -> Self {
        Self {
            valuation: "quantity_fifo_5m_mid_mark_window_delta",
            execution_window_secs: EXECUTION_WINDOW_SECS,
            price_basis: "5m_twap_execution+latest_completed_5s_mid_mark_every_5m",
            fee_basis: "source_theoretical_twap_fee_rate_at_staging",
            available_from_us: None,
            latest_point_ts_us: None,
            points: Vec::new(),
            sampled: false,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingKey {
    source_id: String,
    binding_name: String,
    received_at_us: i64,
    update_seq: i64,
    window_end_us: i64,
}

#[derive(Clone, Debug)]
struct PendingUpdate {
    key: PendingKey,
    position_strategy_name: String,
    window_end_us: i64,
    venue: String,
    fee_rate: f64,
    targets: BTreeMap<String, f64>,
}

#[derive(Clone, Debug)]
struct BindingPosition {
    symbol: String,
    venue: String,
    quantity: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FifoLot {
    seq: i64,
    quantity: f64,
    entry_price: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SymbolState {
    net_quantity: f64,
    realized_pnl_before_fee_quote: f64,
    estimated_trading_fee_quote: f64,
    next_lot_seq: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct AppliedFill {
    lots: VecDeque<FifoLot>,
    net_quantity: f64,
    realized_pnl_before_fee_quote: f64,
    fee_quote: f64,
    cumulative_fee_quote: f64,
    floating_pnl_quote: f64,
    nav_before_fee_quote: f64,
    nav_after_fee_quote: f64,
    next_lot_seq: i64,
}

#[derive(Clone, Debug)]
enum PlannedSymbol {
    Fill {
        symbol: String,
        venue: String,
        previous_quantity: f64,
        target_quantity: f64,
        executed_quantity: f64,
        twap_price: f64,
    },
    Skip {
        symbol: String,
        venue: String,
        reason: &'static str,
    },
}

#[derive(Clone, Debug)]
struct StoredContribution {
    key: String,
    ts_us: i64,
    nav_before_fee: f64,
    nav_after_fee: f64,
    fee: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct LatestTargets {
    position_strategy_name: String,
    venue: String,
    targets: BTreeMap<String, f64>,
    received_at_us: i64,
    update_seq: u32,
}

pub fn spawn(config: AppConfig, pool: PgPool, archive: Arc<PositionArchive>, twap: Arc<TwapStore>) {
    if !config.twap.enabled {
        info!("theoretical 5-minute TWAP NAV materializer disabled with TWAP recorder");
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(WORKER_INTERVAL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) =
                materialize_once(&config, &pool, &archive, &twap, unix_now_us()).await
            {
                warn!(error = %format!("{error:#}"), "theoretical TWAP NAV materialization failed");
            }
        }
    });
}

pub async fn materialize_once(
    config: &AppConfig,
    pool: &PgPool,
    archive: &PositionArchive,
    twap: &TwapStore,
    now_us: i64,
) -> Result<usize> {
    let checkpoint = load_or_initialize_checkpoint(config, pool, now_us).await?;
    let fee_rates = crate::postgres::load_theoretical_twap_fee_rates(pool).await?;
    let messages = archive
        .scan_from(checkpoint.0.max(1))?
        .into_iter()
        .filter(|message| (message.received_at_us, message.seq) > checkpoint)
        .collect::<Vec<_>>();
    stage_messages(config, pool, &messages, &fee_rates).await?;

    let due_before_us = now_us.saturating_sub(BAR_SETTLE_LAG_US);
    let mut completed = 0usize;
    while completed < MAX_COMPLETIONS_PER_RUN {
        let keys = load_due_keys(pool, due_before_us).await?;
        if keys.is_empty() {
            break;
        }
        let mut round_completed = 0usize;
        for key in keys {
            if !materialize_marks_until(pool, twap, &key.source_id, key.window_end_us, now_us)
                .await?
            {
                continue;
            }
            if process_pending(pool, twap, &key, now_us).await? {
                completed += 1;
                round_completed += 1;
                if completed == MAX_COMPLETIONS_PER_RUN {
                    break;
                }
            }
        }
        if round_completed == 0 {
            break;
        }
    }
    let completed_mark_end = now_us.saturating_sub(BAR_SETTLE_LAG_US).saturating_add(1);
    for source in config.sources.iter().filter(|source| source.enabled) {
        let pending_end = earliest_pending_window(pool, &source.id).await?;
        let mark_end = pending_end
            .map(|pending_end| pending_end.min(completed_mark_end))
            .unwrap_or(completed_mark_end);
        materialize_marks_until(pool, twap, &source.id, mark_end, now_us).await?;
    }
    if completed > 0 {
        info!(
            completed,
            "materialized theoretical 5-minute TWAP NAV updates"
        );
    }
    Ok(completed)
}

async fn load_or_initialize_checkpoint(
    config: &AppConfig,
    pool: &PgPool,
    now_us: i64,
) -> Result<(i64, u32)> {
    let row = sqlx::query(
        r#"
        SELECT last_received_at_us, last_seq
        FROM cta_theoretical_nav_checkpoint
        WHERE singleton = true
        "#,
    )
    .fetch_optional(pool)
    .await
    .context("failed to load theoretical NAV archive checkpoint")?;
    if let Some(row) = row {
        let received_at_us: i64 = row.try_get("last_received_at_us")?;
        let seq: i64 = row.try_get("last_seq")?;
        return Ok((received_at_us, u32::try_from(seq)?));
    }

    let retain_us = i64::from(config.twap.retain_days)
        .saturating_mul(86_400)
        .saturating_mul(1_000_000);
    let cutoff_us = now_us.saturating_sub(retain_us).max(1);
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_checkpoint (
            singleton, last_received_at_us, last_seq
        ) VALUES (true, $1, $2)
        ON CONFLICT (singleton) DO NOTHING
        "#,
    )
    .bind(cutoff_us)
    .bind(i64::from(u32::MAX))
    .execute(pool)
    .await
    .context("failed to initialize theoretical NAV archive checkpoint")?;
    Ok((cutoff_us, u32::MAX))
}

async fn stage_messages(
    config: &AppConfig,
    pool: &PgPool,
    messages: &[PositionUpdateMsg],
    fee_rates: &BTreeMap<String, f64>,
) -> Result<usize> {
    if messages.is_empty() {
        return Ok(0);
    }
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin theoretical NAV staging transaction")?;
    let mut latest = load_latest_targets(&mut tx).await?;
    let mut touched = BTreeSet::new();
    let mut staged = 0usize;
    for message in messages {
        for account in &message.published_accounts {
            let Some(source) = config
                .sources
                .iter()
                .find(|source| source.enabled && source.id == account.source_id)
            else {
                continue;
            };
            let targets = normalized_scaled_targets(message, account.effective_shares())
                .with_context(|| {
                    format!(
                        "failed to scale theoretical targets for {}/{}",
                        account.source_id, account.binding_name
                    )
                })?;
            let key = (account.source_id.clone(), account.binding_name.clone());
            let next = LatestTargets {
                position_strategy_name: message.strategy.strategy_name.clone(),
                venue: source.venue.clone(),
                targets,
                received_at_us: message.received_at_us,
                update_seq: message.seq,
            };
            let changed = target_positions_changed(latest.get(&key), &next);
            if changed {
                let fee_rate = fee_rates.get(&source.id).copied().with_context(|| {
                    format!("missing theoretical TWAP fee rate for {}", source.id)
                })?;
                stage_target_change(
                    &mut tx,
                    &account.source_id,
                    &account.binding_name,
                    &next,
                    fee_rate,
                )
                .await?;
                staged = staged.saturating_add(1);
            }
            latest.insert(key.clone(), next);
            touched.insert(key);
        }
    }

    for key in touched {
        save_latest_targets(
            &mut tx,
            &key.0,
            &key.1,
            latest
                .get(&key)
                .context("theoretical latest target disappeared during staging")?,
        )
        .await?;
    }
    let last = messages
        .last()
        .context("non-empty theoretical message batch lost its tail")?;
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_checkpoint (
            singleton, last_received_at_us, last_seq, updated_at
        ) VALUES (true, $1, $2, now())
        ON CONFLICT (singleton) DO UPDATE SET
            last_received_at_us = EXCLUDED.last_received_at_us,
            last_seq = EXCLUDED.last_seq,
            updated_at = now()
        WHERE (cta_theoretical_nav_checkpoint.last_received_at_us,
               cta_theoretical_nav_checkpoint.last_seq)
            < (EXCLUDED.last_received_at_us, EXCLUDED.last_seq)
        "#,
    )
    .bind(last.received_at_us)
    .bind(i64::from(last.seq))
    .execute(&mut *tx)
    .await
    .context("failed to advance theoretical NAV archive checkpoint")?;
    tx.commit()
        .await
        .context("failed to commit theoretical NAV staging transaction")?;
    if staged > 0 {
        info!(
            scanned = messages.len(),
            staged, "staged theoretical target changes"
        );
    }
    Ok(staged)
}

fn normalized_scaled_targets(
    message: &PositionUpdateMsg,
    shares: f64,
) -> Result<BTreeMap<String, f64>> {
    let mut targets = BTreeMap::new();
    for (symbol, target) in &message.strategy.targets {
        let quantity = clean_zero(target.qty * shares);
        if !quantity.is_finite() {
            bail!("theoretical target scaling overflowed");
        }
        if quantity != 0.0 {
            targets.insert(symbol.clone(), quantity);
        }
    }
    Ok(targets)
}

fn normalize_stored_targets(targets: &mut BTreeMap<String, f64>) -> Result<()> {
    if targets.values().any(|quantity| !quantity.is_finite()) {
        bail!("stored theoretical target is not finite");
    }
    targets.retain(|_, quantity| clean_zero(*quantity) != 0.0);
    Ok(())
}

fn target_positions_changed(previous: Option<&LatestTargets>, next: &LatestTargets) -> bool {
    previous.is_none_or(|previous| previous.venue != next.venue || previous.targets != next.targets)
}

async fn load_latest_targets(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<BTreeMap<(String, String), LatestTargets>> {
    let rows = sqlx::query(
        r#"
        SELECT source_id, binding_name, position_strategy_name, venue,
               targets, received_at_us, update_seq
        FROM cta_theoretical_nav_latest_targets
        ORDER BY source_id, binding_name
        FOR UPDATE
        "#,
    )
    .fetch_all(&mut **tx)
    .await
    .context("failed to load latest theoretical targets")?;
    rows.into_iter()
        .map(|row| {
            let source_id: String = row.try_get("source_id")?;
            let binding_name: String = row.try_get("binding_name")?;
            let targets: serde_json::Value = row.try_get("targets")?;
            let mut targets = serde_json::from_value(targets)
                .context("failed to decode latest theoretical targets")?;
            normalize_stored_targets(&mut targets)?;
            Ok((
                (source_id, binding_name),
                LatestTargets {
                    position_strategy_name: row.try_get("position_strategy_name")?,
                    venue: row.try_get("venue")?,
                    targets,
                    received_at_us: row.try_get("received_at_us")?,
                    update_seq: u32::try_from(row.try_get::<i64, _>("update_seq")?)?,
                },
            ))
        })
        .collect()
}

async fn stage_target_change(
    tx: &mut Transaction<'_, Postgres>,
    source_id: &str,
    binding_name: &str,
    next: &LatestTargets,
    fee_rate: f64,
) -> Result<()> {
    let seq = i64::from(next.update_seq);
    sqlx::query(
        r#"
        UPDATE cta_theoretical_nav_pending
        SET window_end_us = LEAST(window_end_us, $3)
        WHERE source_id = $1
          AND binding_name = $2
          AND (received_at_us, update_seq) < ($3, $4)
          AND window_end_us > $3
        "#,
    )
    .bind(source_id)
    .bind(binding_name)
    .bind(next.received_at_us)
    .bind(seq)
    .execute(&mut **tx)
    .await
    .context("failed to truncate superseded theoretical NAV window")?;

    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_pending (
            source_id, binding_name, position_strategy_name,
            received_at_us, update_seq, window_end_us, venue, fee_rate, targets
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (source_id, binding_name, received_at_us, update_seq)
        DO NOTHING
        "#,
    )
    .bind(source_id)
    .bind(binding_name)
    .bind(&next.position_strategy_name)
    .bind(next.received_at_us)
    .bind(seq)
    .bind(next.received_at_us.saturating_add(EXECUTION_WINDOW_US))
    .bind(&next.venue)
    .bind(fee_rate)
    .bind(serde_json::to_value(&next.targets)?)
    .execute(&mut **tx)
    .await
    .context("failed to stage theoretical NAV target change")?;
    Ok(())
}

async fn save_latest_targets(
    tx: &mut Transaction<'_, Postgres>,
    source_id: &str,
    binding_name: &str,
    latest: &LatestTargets,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_latest_targets (
            source_id, binding_name, position_strategy_name, venue, targets,
            received_at_us, update_seq, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now())
        ON CONFLICT (source_id, binding_name) DO UPDATE SET
            position_strategy_name = EXCLUDED.position_strategy_name,
            venue = EXCLUDED.venue,
            targets = EXCLUDED.targets,
            received_at_us = EXCLUDED.received_at_us,
            update_seq = EXCLUDED.update_seq,
            updated_at = now()
        WHERE (cta_theoretical_nav_latest_targets.received_at_us,
               cta_theoretical_nav_latest_targets.update_seq)
            < (EXCLUDED.received_at_us, EXCLUDED.update_seq)
        "#,
    )
    .bind(source_id)
    .bind(binding_name)
    .bind(&latest.position_strategy_name)
    .bind(&latest.venue)
    .bind(serde_json::to_value(&latest.targets)?)
    .bind(latest.received_at_us)
    .bind(i64::from(latest.update_seq))
    .execute(&mut **tx)
    .await
    .context("failed to save latest theoretical targets")?;
    Ok(())
}

async fn load_due_keys(pool: &PgPool, due_before_us: i64) -> Result<Vec<PendingKey>> {
    let rows = sqlx::query(
        r#"
        SELECT p.source_id, p.binding_name, p.received_at_us, p.update_seq,
               p.window_end_us
        FROM cta_theoretical_nav_pending p
        WHERE p.window_end_us <= $1
          AND NOT EXISTS (
              SELECT 1
              FROM cta_theoretical_nav_pending earlier
              WHERE earlier.source_id = p.source_id
                AND earlier.binding_name = p.binding_name
                AND (earlier.received_at_us, earlier.update_seq)
                    < (p.received_at_us, p.update_seq)
          )
        ORDER BY p.window_end_us, p.received_at_us, p.update_seq,
                 p.source_id, p.binding_name
        LIMIT $2
        "#,
    )
    .bind(due_before_us)
    .bind(PROCESS_BATCH_SIZE)
    .fetch_all(pool)
    .await
    .context("failed to load due theoretical NAV updates")?;
    rows.into_iter()
        .map(|row| {
            Ok(PendingKey {
                source_id: row.try_get("source_id")?,
                binding_name: row.try_get("binding_name")?,
                received_at_us: row.try_get("received_at_us")?,
                update_seq: row.try_get("update_seq")?,
                window_end_us: row.try_get("window_end_us")?,
            })
        })
        .collect()
}

async fn earliest_pending_window(pool: &PgPool, source_id: &str) -> Result<Option<i64>> {
    sqlx::query_scalar(
        r#"
        SELECT MIN(window_end_us)
        FROM cta_theoretical_nav_pending
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .fetch_one(pool)
    .await
    .context("failed to load earliest pending theoretical NAV window")
}

async fn materialize_marks_until(
    pool: &PgPool,
    twap: &TwapStore,
    source_id: &str,
    exclusive_end_us: i64,
    now_us: i64,
) -> Result<bool> {
    let mut last_mark_ts_us = load_or_initialize_mark_checkpoint(pool, source_id, exclusive_end_us)
        .await
        .with_context(|| format!("failed to initialize theoretical mark cursor for {source_id}"))?;
    let last_due_mark_ts_us = mark_strictly_before(exclusive_end_us);
    if last_due_mark_ts_us <= last_mark_ts_us {
        return Ok(true);
    }
    if !has_open_position(pool, source_id).await? {
        advance_mark_checkpoint(pool, source_id, last_due_mark_ts_us).await?;
        return Ok(true);
    }
    let mut completed = 0usize;
    loop {
        let next_mark_ts_us = last_mark_ts_us.saturating_add(MARK_INTERVAL_US);
        if next_mark_ts_us >= exclusive_end_us {
            return Ok(true);
        }
        if completed == MAX_MARKS_PER_CALL {
            return Ok(false);
        }
        if !materialize_mark_tick(pool, twap, source_id, next_mark_ts_us, now_us).await? {
            return Ok(false);
        }
        last_mark_ts_us = next_mark_ts_us;
        completed += 1;
    }
}

async fn load_or_initialize_mark_checkpoint(
    pool: &PgPool,
    source_id: &str,
    anchor_ts_us: i64,
) -> Result<i64> {
    let initial = mark_strictly_before(anchor_ts_us);
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_mark_checkpoints (
            source_id, last_mark_ts_us
        ) VALUES ($1, $2)
        ON CONFLICT (source_id) DO NOTHING
        "#,
    )
    .bind(source_id)
    .bind(initial)
    .execute(pool)
    .await?;
    sqlx::query_scalar(
        r#"
        SELECT last_mark_ts_us
        FROM cta_theoretical_nav_mark_checkpoints
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .fetch_one(pool)
    .await
    .context("failed to load theoretical NAV mark checkpoint")
}

fn mark_strictly_before(ts_us: i64) -> i64 {
    ts_us.saturating_sub(1).div_euclid(MARK_INTERVAL_US) * MARK_INTERVAL_US
}

async fn has_open_position(pool: &PgPool, source_id: &str) -> Result<bool> {
    sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM cta_theoretical_nav_symbol_states
            WHERE source_id = $1 AND abs(net_quantity) > $2
        )
        "#,
    )
    .bind(source_id)
    .bind(ZERO_EPSILON)
    .fetch_one(pool)
    .await
    .context("failed to check theoretical open positions")
}

async fn advance_mark_checkpoint(pool: &PgPool, source_id: &str, mark_ts_us: i64) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE cta_theoretical_nav_mark_checkpoints
        SET last_mark_ts_us = $2, updated_at = now()
        WHERE source_id = $1 AND last_mark_ts_us < $2
        "#,
    )
    .bind(source_id)
    .bind(mark_ts_us)
    .execute(pool)
    .await
    .context("failed to advance flat theoretical mark checkpoint")?;
    Ok(())
}

async fn materialize_mark_tick(
    pool: &PgPool,
    twap: &TwapStore,
    source_id: &str,
    mark_ts_us: i64,
    now_us: i64,
) -> Result<bool> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin theoretical NAV mark transaction")?;
    let rows = sqlx::query(
        r#"
        SELECT symbol, venue, mark_price
        FROM cta_theoretical_nav_symbol_states
        WHERE source_id = $1 AND abs(net_quantity) > $2
        ORDER BY symbol, venue
        FOR UPDATE
        "#,
    )
    .bind(source_id)
    .bind(ZERO_EPSILON)
    .fetch_all(&mut *tx)
    .await
    .context("failed to lock open theoretical positions for marking")?;
    let mut changed = false;
    for row in &rows {
        let symbol: String = row.try_get("symbol")?;
        let venue: String = row.try_get("venue")?;
        let previous_mark: Option<f64> = row.try_get("mark_price")?;
        let bars = twap.scan_bars(
            &symbol,
            &venue,
            mark_ts_us.saturating_sub(MARK_LOOKBACK_US).max(1),
            mark_ts_us.saturating_add(1),
        )?;
        let mark = completed_mark_mid(&bars, mark_ts_us);
        let Some(mark) = mark else {
            if now_us <= mark_ts_us.saturating_add(MISSING_BAR_GRACE_US) {
                tx.rollback().await.ok();
                return Ok(false);
            }
            continue;
        };
        if previous_mark.is_none_or(|previous| (previous - mark).abs() > ZERO_EPSILON) {
            changed = true;
        }
        sqlx::query(
            r#"
            UPDATE cta_theoretical_nav_symbol_states
            SET mark_price = $4, updated_at_us = $5
            WHERE source_id = $1 AND symbol = $2 AND venue = $3
            "#,
        )
        .bind(source_id)
        .bind(&symbol)
        .bind(&venue)
        .bind(mark)
        .bind(mark_ts_us)
        .execute(&mut *tx)
        .await?;
    }
    if !rows.is_empty() && changed {
        store_portfolio_point(&mut tx, source_id, mark_ts_us, "mark").await?;
    }
    sqlx::query(
        r#"
        UPDATE cta_theoretical_nav_mark_checkpoints
        SET last_mark_ts_us = $2, updated_at = now()
        WHERE source_id = $1 AND last_mark_ts_us < $2
        "#,
    )
    .bind(source_id)
    .bind(mark_ts_us)
    .execute(&mut *tx)
    .await?;
    tx.commit()
        .await
        .context("failed to commit theoretical NAV mark")?;
    Ok(true)
}

async fn process_pending(
    pool: &PgPool,
    twap: &TwapStore,
    key: &PendingKey,
    now_us: i64,
) -> Result<bool> {
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin theoretical NAV materialization transaction")?;
    let Some(pending) = lock_pending(&mut tx, key).await? else {
        tx.rollback().await.ok();
        return Ok(false);
    };
    let positions = load_binding_positions(&mut tx, &pending.key).await?;
    let mut current = positions
        .iter()
        .map(|position| {
            (
                (position.symbol.clone(), position.venue.clone()),
                position.quantity,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut markets = current.keys().cloned().collect::<BTreeSet<_>>();
    markets.extend(
        pending
            .targets
            .keys()
            .cloned()
            .map(|symbol| (symbol, pending.venue.clone())),
    );

    let mut planned = Vec::new();
    let mut wait_for_bars = false;
    for (symbol, venue) in markets {
        let previous = current
            .remove(&(symbol.clone(), venue.clone()))
            .unwrap_or(0.0);
        let target = if venue == pending.venue {
            pending.targets.get(&symbol).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        let executed = clean_zero(target - previous);
        if executed == 0.0 {
            continue;
        }
        if pending.window_end_us <= pending.key.received_at_us {
            planned.push(PlannedSymbol::Skip {
                symbol,
                venue,
                reason: "superseded_before_execution",
            });
            continue;
        }
        let bars = twap.scan_bars(
            &symbol,
            &venue,
            pending.key.received_at_us.saturating_add(1),
            pending.window_end_us.saturating_add(1),
        )?;
        match complete_window_twap(&bars, pending.key.received_at_us, pending.window_end_us) {
            Some(price) => planned.push(PlannedSymbol::Fill {
                symbol,
                venue,
                previous_quantity: previous,
                target_quantity: target,
                executed_quantity: executed,
                twap_price: price,
            }),
            None if now_us <= pending.window_end_us.saturating_add(MISSING_BAR_GRACE_US) => {
                wait_for_bars = true;
            }
            None => planned.push(PlannedSymbol::Skip {
                symbol,
                venue,
                reason: "missing_complete_minute_twap",
            }),
        }
    }
    if wait_for_bars {
        tx.rollback().await.ok();
        return Ok(false);
    }

    let mut had_fill = false;
    for item in planned {
        match item {
            PlannedSymbol::Fill {
                symbol,
                venue,
                previous_quantity,
                target_quantity,
                executed_quantity,
                twap_price,
            } => {
                had_fill = true;
                apply_fill(
                    &mut tx,
                    &pending,
                    &symbol,
                    &venue,
                    previous_quantity,
                    target_quantity,
                    executed_quantity,
                    twap_price,
                )
                .await?;
                save_binding_position(&mut tx, &pending, &symbol, &venue, target_quantity).await?;
            }
            PlannedSymbol::Skip {
                symbol,
                venue,
                reason,
            } => {
                save_skip(&mut tx, &pending, &symbol, &venue, reason).await?;
            }
        }
    }
    if had_fill {
        store_portfolio_point(
            &mut tx,
            &pending.key.source_id,
            pending.window_end_us,
            "execution",
        )
        .await?;
    }
    delete_pending(&mut tx, &pending.key).await?;
    tx.commit()
        .await
        .context("failed to commit theoretical NAV materialization")?;
    Ok(true)
}

async fn lock_pending(
    tx: &mut Transaction<'_, Postgres>,
    key: &PendingKey,
) -> Result<Option<PendingUpdate>> {
    let row = sqlx::query(
        r#"
        SELECT position_strategy_name, window_end_us, venue, fee_rate, targets
        FROM cta_theoretical_nav_pending
        WHERE source_id = $1 AND binding_name = $2
          AND received_at_us = $3 AND update_seq = $4
        FOR UPDATE
        "#,
    )
    .bind(&key.source_id)
    .bind(&key.binding_name)
    .bind(key.received_at_us)
    .bind(key.update_seq)
    .fetch_optional(&mut **tx)
    .await
    .context("failed to lock theoretical NAV pending update")?;
    row.map(|row| {
        let targets: serde_json::Value = row.try_get("targets")?;
        Ok(PendingUpdate {
            key: key.clone(),
            position_strategy_name: row.try_get("position_strategy_name")?,
            window_end_us: row.try_get("window_end_us")?,
            venue: row.try_get("venue")?,
            fee_rate: row.try_get("fee_rate")?,
            targets: serde_json::from_value(targets)
                .context("failed to decode theoretical NAV pending targets")?,
        })
    })
    .transpose()
}

async fn load_binding_positions(
    tx: &mut Transaction<'_, Postgres>,
    key: &PendingKey,
) -> Result<Vec<BindingPosition>> {
    let rows = sqlx::query(
        r#"
        SELECT symbol, venue, quantity
        FROM cta_theoretical_binding_positions
        WHERE source_id = $1 AND binding_name = $2
        ORDER BY symbol, venue
        FOR UPDATE
        "#,
    )
    .bind(&key.source_id)
    .bind(&key.binding_name)
    .fetch_all(&mut **tx)
    .await
    .context("failed to load theoretical binding positions")?;
    rows.into_iter()
        .map(|row| {
            Ok(BindingPosition {
                symbol: row.try_get("symbol")?,
                venue: row.try_get("venue")?,
                quantity: row.try_get("quantity")?,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn apply_fill(
    tx: &mut Transaction<'_, Postgres>,
    pending: &PendingUpdate,
    symbol: &str,
    venue: &str,
    previous_quantity: f64,
    target_quantity: f64,
    executed_quantity: f64,
    twap_price: f64,
) -> Result<()> {
    let already_stored: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM cta_theoretical_nav_events
            WHERE source_id = $1 AND binding_name = $2
              AND symbol = $3 AND venue = $4
              AND received_at_us = $5 AND update_seq = $6
        )
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(&pending.key.binding_name)
    .bind(symbol)
    .bind(venue)
    .bind(pending.key.received_at_us)
    .bind(pending.key.update_seq)
    .fetch_one(&mut **tx)
    .await?;
    if already_stored {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_symbol_states (
            source_id, symbol, venue, updated_at_us
        ) VALUES ($1, $2, $3, $4)
        ON CONFLICT (source_id, symbol, venue) DO NOTHING
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(symbol)
    .bind(venue)
    .bind(pending.window_end_us)
    .execute(&mut **tx)
    .await?;
    let state_row = sqlx::query(
        r#"
        SELECT net_quantity, realized_pnl_before_fee_quote,
               estimated_trading_fee_quote, next_lot_seq
        FROM cta_theoretical_nav_symbol_states
        WHERE source_id = $1 AND symbol = $2 AND venue = $3
        FOR UPDATE
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(symbol)
    .bind(venue)
    .fetch_one(&mut **tx)
    .await?;
    let state = SymbolState {
        net_quantity: state_row.try_get("net_quantity")?,
        realized_pnl_before_fee_quote: state_row.try_get("realized_pnl_before_fee_quote")?,
        estimated_trading_fee_quote: state_row.try_get("estimated_trading_fee_quote")?,
        next_lot_seq: state_row.try_get("next_lot_seq")?,
    };
    let lot_rows = sqlx::query(
        r#"
        SELECT lot_seq, quantity, entry_price
        FROM cta_theoretical_nav_fifo_lots
        WHERE source_id = $1 AND symbol = $2 AND venue = $3
        ORDER BY lot_seq
        FOR UPDATE
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(symbol)
    .bind(venue)
    .fetch_all(&mut **tx)
    .await?;
    let lots = lot_rows
        .into_iter()
        .map(|row| {
            Ok(FifoLot {
                seq: row.try_get("lot_seq")?,
                quantity: row.try_get("quantity")?,
                entry_price: row.try_get("entry_price")?,
            })
        })
        .collect::<Result<VecDeque<_>>>()?;
    let applied = evaluate_fill(state, lots, executed_quantity, twap_price, pending.fee_rate)?;

    sqlx::query(
        r#"
        DELETE FROM cta_theoretical_nav_fifo_lots
        WHERE source_id = $1 AND symbol = $2 AND venue = $3
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(symbol)
    .bind(venue)
    .execute(&mut **tx)
    .await?;
    for lot in &applied.lots {
        sqlx::query(
            r#"
            INSERT INTO cta_theoretical_nav_fifo_lots (
                source_id, symbol, venue, lot_seq, quantity, entry_price
            ) VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(&pending.key.source_id)
        .bind(symbol)
        .bind(venue)
        .bind(lot.seq)
        .bind(lot.quantity)
        .bind(lot.entry_price)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        r#"
        UPDATE cta_theoretical_nav_symbol_states
        SET net_quantity = $4,
            realized_pnl_before_fee_quote = $5,
            estimated_trading_fee_quote = $6,
            mark_price = $7,
            next_lot_seq = $8,
            updated_at_us = $9
        WHERE source_id = $1 AND symbol = $2 AND venue = $3
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(symbol)
    .bind(venue)
    .bind(applied.net_quantity)
    .bind(applied.realized_pnl_before_fee_quote)
    .bind(applied.cumulative_fee_quote)
    .bind(twap_price)
    .bind(applied.next_lot_seq)
    .bind(pending.window_end_us)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_events (
            source_id, binding_name, position_strategy_name, symbol, venue,
            received_at_us, update_seq, execution_ts_us,
            previous_quantity, target_quantity, executed_quantity,
            twap_price, fee_rate, fee_quote,
            cumulative_realized_pnl_before_fee_quote,
            cumulative_estimated_trading_fee_quote,
            cumulative_floating_pnl_quote,
            cumulative_nav_before_fee_quote,
            cumulative_nav_after_fee_quote
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
            $12, $13, $14, $15, $16, $17, $18, $19
        )
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(&pending.key.binding_name)
    .bind(&pending.position_strategy_name)
    .bind(symbol)
    .bind(venue)
    .bind(pending.key.received_at_us)
    .bind(pending.key.update_seq)
    .bind(pending.window_end_us)
    .bind(previous_quantity)
    .bind(target_quantity)
    .bind(executed_quantity)
    .bind(twap_price)
    .bind(pending.fee_rate)
    .bind(applied.fee_quote)
    .bind(applied.realized_pnl_before_fee_quote)
    .bind(applied.cumulative_fee_quote)
    .bind(applied.floating_pnl_quote)
    .bind(applied.nav_before_fee_quote)
    .bind(applied.nav_after_fee_quote)
    .execute(&mut **tx)
    .await
    .context("failed to append sparse theoretical NAV event")?;
    Ok(())
}

async fn save_binding_position(
    tx: &mut Transaction<'_, Postgres>,
    pending: &PendingUpdate,
    symbol: &str,
    venue: &str,
    target_quantity: f64,
) -> Result<()> {
    if clean_zero(target_quantity) == 0.0 {
        sqlx::query(
            r#"
            DELETE FROM cta_theoretical_binding_positions
            WHERE source_id = $1 AND binding_name = $2
              AND symbol = $3 AND venue = $4
            "#,
        )
        .bind(&pending.key.source_id)
        .bind(&pending.key.binding_name)
        .bind(symbol)
        .bind(venue)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query(
            r#"
            INSERT INTO cta_theoretical_binding_positions (
                source_id, binding_name, symbol, venue, position_strategy_name,
                quantity, updated_at_us, update_seq
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (source_id, binding_name, symbol, venue) DO UPDATE SET
                position_strategy_name = EXCLUDED.position_strategy_name,
                quantity = EXCLUDED.quantity,
                updated_at_us = EXCLUDED.updated_at_us,
                update_seq = EXCLUDED.update_seq
            "#,
        )
        .bind(&pending.key.source_id)
        .bind(&pending.key.binding_name)
        .bind(symbol)
        .bind(venue)
        .bind(&pending.position_strategy_name)
        .bind(target_quantity)
        .bind(pending.key.received_at_us)
        .bind(pending.key.update_seq)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn store_portfolio_point(
    tx: &mut Transaction<'_, Postgres>,
    source_id: &str,
    ts_us: i64,
    point_kind: &str,
) -> Result<()> {
    let row = sqlx::query(
        r#"
        SELECT
            COALESCE(SUM(s.realized_pnl_before_fee_quote), 0)::double precision
                AS realized,
            COALESCE(SUM(s.estimated_trading_fee_quote), 0)::double precision
                AS fee,
            COALESCE(SUM(COALESCE((
                SELECT SUM(l.quantity * (s.mark_price - l.entry_price))
                FROM cta_theoretical_nav_fifo_lots l
                WHERE l.source_id = s.source_id
                  AND l.symbol = s.symbol
                  AND l.venue = s.venue
            ), 0)), 0)::double precision AS floating,
            COUNT(*) FILTER (WHERE abs(s.net_quantity) > $2)::bigint
                AS open_position_count
        FROM cta_theoretical_nav_symbol_states s
        WHERE s.source_id = $1
        "#,
    )
    .bind(source_id)
    .bind(ZERO_EPSILON)
    .fetch_one(&mut **tx)
    .await
    .context("failed to aggregate theoretical portfolio state")?;
    let realized: f64 = row.try_get("realized")?;
    let fee: f64 = row.try_get("fee")?;
    let floating: f64 = row.try_get("floating")?;
    let open_position_count: i64 = row.try_get("open_position_count")?;
    let nav_before = clean_zero(realized + floating);
    let nav_after = clean_zero(nav_before - fee);
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_portfolio_points (
            source_id, ts_us, point_kind, open_position_count,
            cumulative_realized_pnl_before_fee_quote,
            cumulative_estimated_trading_fee_quote,
            cumulative_floating_pnl_quote,
            cumulative_nav_before_fee_quote,
            cumulative_nav_after_fee_quote
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (source_id, ts_us) DO UPDATE SET
            point_kind = EXCLUDED.point_kind,
            open_position_count = EXCLUDED.open_position_count,
            cumulative_realized_pnl_before_fee_quote =
                EXCLUDED.cumulative_realized_pnl_before_fee_quote,
            cumulative_estimated_trading_fee_quote =
                EXCLUDED.cumulative_estimated_trading_fee_quote,
            cumulative_floating_pnl_quote = EXCLUDED.cumulative_floating_pnl_quote,
            cumulative_nav_before_fee_quote = EXCLUDED.cumulative_nav_before_fee_quote,
            cumulative_nav_after_fee_quote = EXCLUDED.cumulative_nav_after_fee_quote
        "#,
    )
    .bind(source_id)
    .bind(ts_us)
    .bind(point_kind)
    .bind(i32::try_from(open_position_count)?)
    .bind(realized)
    .bind(fee)
    .bind(floating)
    .bind(nav_before)
    .bind(nav_after)
    .execute(&mut **tx)
    .await
    .context("failed to store theoretical portfolio NAV point")?;
    Ok(())
}

async fn save_skip(
    tx: &mut Transaction<'_, Postgres>,
    pending: &PendingUpdate,
    symbol: &str,
    venue: &str,
    reason: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO cta_theoretical_nav_skips (
            source_id, binding_name, position_strategy_name, symbol, venue,
            received_at_us, update_seq, window_end_us, reason
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (source_id, binding_name, symbol, venue, received_at_us, update_seq)
        DO NOTHING
        "#,
    )
    .bind(&pending.key.source_id)
    .bind(&pending.key.binding_name)
    .bind(&pending.position_strategy_name)
    .bind(symbol)
    .bind(venue)
    .bind(pending.key.received_at_us)
    .bind(pending.key.update_seq)
    .bind(pending.window_end_us)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn delete_pending(tx: &mut Transaction<'_, Postgres>, key: &PendingKey) -> Result<()> {
    sqlx::query(
        r#"
        DELETE FROM cta_theoretical_nav_pending
        WHERE source_id = $1 AND binding_name = $2
          AND received_at_us = $3 AND update_seq = $4
        "#,
    )
    .bind(&key.source_id)
    .bind(&key.binding_name)
    .bind(key.received_at_us)
    .bind(key.update_seq)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn apply_fifo_fill(
    mut lots: VecDeque<FifoLot>,
    fill_quantity: f64,
    fill_price: f64,
    mut next_lot_seq: i64,
) -> Result<(VecDeque<FifoLot>, f64, i64)> {
    if !fill_quantity.is_finite() || !fill_price.is_finite() || fill_price <= 0.0 {
        bail!("invalid theoretical FIFO fill");
    }
    let mut remaining = fill_quantity;
    let mut realized = 0.0;
    while remaining.abs() > ZERO_EPSILON {
        let Some(front) = lots.front_mut() else {
            break;
        };
        if front.quantity.signum() == remaining.signum() {
            break;
        }
        let matched = front.quantity.abs().min(remaining.abs());
        realized += if front.quantity > 0.0 {
            matched * (fill_price - front.entry_price)
        } else {
            matched * (front.entry_price - fill_price)
        };
        let direction = remaining.signum();
        front.quantity = clean_zero(front.quantity + direction * matched);
        remaining = clean_zero(remaining - direction * matched);
        if front.quantity == 0.0 {
            lots.pop_front();
        }
    }
    if remaining.abs() > ZERO_EPSILON {
        lots.push_back(FifoLot {
            seq: next_lot_seq,
            quantity: remaining,
            entry_price: fill_price,
        });
        next_lot_seq = next_lot_seq
            .checked_add(1)
            .context("theoretical FIFO lot sequence overflowed")?;
    }
    if !realized.is_finite() {
        bail!("theoretical FIFO realized PnL overflowed");
    }
    Ok((lots, clean_zero(realized), next_lot_seq))
}

fn evaluate_fill(
    state: SymbolState,
    lots: VecDeque<FifoLot>,
    fill_quantity: f64,
    fill_price: f64,
    fee_rate: f64,
) -> Result<AppliedFill> {
    if !fee_rate.is_finite() {
        bail!("invalid theoretical fee rate");
    }
    let (lots, realized_increment, next_lot_seq) =
        apply_fifo_fill(lots, fill_quantity, fill_price, state.next_lot_seq)?;
    let net_quantity = clean_zero(lots.iter().map(|lot| lot.quantity).sum());
    let expected_net = clean_zero(state.net_quantity + fill_quantity);
    if !quantities_equal(net_quantity, expected_net) {
        bail!("theoretical FIFO quantity diverged: fifo={net_quantity} expected={expected_net}");
    }
    let realized = clean_zero(state.realized_pnl_before_fee_quote + realized_increment);
    let fee_quote = fill_quantity.abs() * fill_price * fee_rate;
    let cumulative_fee = clean_zero(state.estimated_trading_fee_quote + fee_quote);
    let floating = floating_pnl_at_mark(&lots, fill_price)?;
    let nav_before = clean_zero(realized + floating);
    let nav_after = clean_zero(nav_before - cumulative_fee);
    if ![
        net_quantity,
        realized,
        fee_quote,
        cumulative_fee,
        floating,
        nav_before,
        nav_after,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        bail!("theoretical NAV overflowed");
    }
    Ok(AppliedFill {
        lots,
        net_quantity,
        realized_pnl_before_fee_quote: realized,
        fee_quote,
        cumulative_fee_quote: cumulative_fee,
        floating_pnl_quote: floating,
        nav_before_fee_quote: nav_before,
        nav_after_fee_quote: nav_after,
        next_lot_seq,
    })
}

fn floating_pnl_at_mark(lots: &VecDeque<FifoLot>, mark_price: f64) -> Result<f64> {
    if !mark_price.is_finite() || mark_price <= 0.0 {
        bail!("invalid theoretical mark price");
    }
    let floating = clean_zero(
        lots.iter()
            .map(|lot| lot.quantity * (mark_price - lot.entry_price))
            .sum(),
    );
    if !floating.is_finite() {
        bail!("theoretical floating PnL overflowed");
    }
    Ok(floating)
}

fn complete_window_twap(bars: &[TwapBar], start_us: i64, end_us: i64) -> Option<f64> {
    if end_us <= start_us {
        return None;
    }
    let mut weighted = 0.0;
    let mut total_duration = 0i64;
    let mut bucket_start = start_us;
    while bucket_start < end_us {
        let bucket_end = bucket_start.saturating_add(MINUTE_US).min(end_us);
        let mut sum = 0.0;
        let mut count = 0u32;
        for bar in bars {
            if bar.end_ts_us > bucket_start && bar.end_ts_us <= bucket_end {
                sum += bar.twap;
                count = count.saturating_add(1);
            }
        }
        if count == 0 {
            return None;
        }
        let duration = bucket_end - bucket_start;
        weighted += (sum / f64::from(count)) * duration as f64;
        total_duration = total_duration.saturating_add(duration);
        bucket_start = bucket_end;
    }
    let twap = weighted / total_duration as f64;
    (twap.is_finite() && twap > 0.0).then_some(twap)
}

fn completed_mark_mid(bars: &[TwapBar], mark_ts_us: i64) -> Option<f64> {
    let end = bars.partition_point(|bar| bar.end_ts_us <= mark_ts_us);
    let bar = bars.get(..end)?.last()?;
    let age = mark_ts_us.saturating_sub(bar.end_ts_us);
    (age <= MARK_MAX_AGE_US && bar.twap.is_finite() && bar.twap > 0.0).then_some(bar.twap)
}

pub async fn load_timeline(
    pool: &PgPool,
    start_ts_us: i64,
    end_ts_us: i64,
    source_ids: &[String],
    max_points: usize,
) -> Result<TheoreticalNavTimeline> {
    if start_ts_us < 0 || end_ts_us < start_ts_us {
        bail!("invalid theoretical NAV timeline range");
    }
    let available_from_us: Option<i64> = sqlx::query_scalar(
        r#"
        SELECT MIN(ts_us)
        FROM cta_theoretical_nav_portfolio_points
        WHERE (cardinality($1::text[]) = 0 OR source_id = ANY($1))
        "#,
    )
    .bind(source_ids.to_vec())
    .fetch_one(pool)
    .await
    .context("failed to load theoretical NAV availability")?;
    let Some(available_from_us) = available_from_us else {
        return Ok(TheoreticalNavTimeline::default());
    };
    if available_from_us > end_ts_us {
        return Ok(TheoreticalNavTimeline {
            available_from_us: Some(available_from_us),
            ..TheoreticalNavTimeline::default()
        });
    }

    let baseline_rows = sqlx::query(
        r#"
        SELECT DISTINCT ON (source_id)
            source_id, ts_us,
            cumulative_estimated_trading_fee_quote,
            cumulative_nav_before_fee_quote,
            cumulative_nav_after_fee_quote
        FROM cta_theoretical_nav_portfolio_points
        WHERE ts_us < $1
          AND (cardinality($2::text[]) = 0 OR source_id = ANY($2))
        ORDER BY source_id, ts_us DESC
        "#,
    )
    .bind(start_ts_us)
    .bind(source_ids.to_vec())
    .fetch_all(pool)
    .await
    .context("failed to load theoretical NAV baseline")?;
    let point_rows = sqlx::query(
        r#"
        SELECT source_id, ts_us,
               cumulative_estimated_trading_fee_quote,
               cumulative_nav_before_fee_quote,
               cumulative_nav_after_fee_quote
        FROM cta_theoretical_nav_portfolio_points
        WHERE ts_us >= $1 AND ts_us <= $2
          AND (cardinality($3::text[]) = 0 OR source_id = ANY($3))
        ORDER BY ts_us, source_id
        "#,
    )
    .bind(start_ts_us)
    .bind(end_ts_us)
    .bind(source_ids.to_vec())
    .fetch_all(pool)
    .await
    .context("failed to load theoretical NAV portfolio points")?;
    let baseline = baseline_rows
        .into_iter()
        .map(decode_contribution)
        .collect::<Result<Vec<_>>>()?;
    let stored_points = point_rows
        .into_iter()
        .map(decode_contribution)
        .collect::<Result<Vec<_>>>()?;
    let latest_point_ts_us = baseline
        .iter()
        .chain(&stored_points)
        .map(|row| row.ts_us)
        .max();
    let mut current = baseline
        .into_iter()
        .map(|row| (row.key.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let baseline_totals = contribution_totals(current.values());
    let series_start = start_ts_us.max(available_from_us);
    let mut points = vec![TheoreticalNavPoint {
        ts_us: series_start,
        ..TheoreticalNavPoint::default()
    }];
    let mut index = 0usize;
    while index < stored_points.len() {
        let ts_us = stored_points[index].ts_us;
        while index < stored_points.len() && stored_points[index].ts_us == ts_us {
            let row = stored_points[index].clone();
            current.insert(row.key.clone(), row);
            index += 1;
        }
        let totals = contribution_totals(current.values());
        push_or_replace_point(
            &mut points,
            TheoreticalNavPoint {
                ts_us,
                nav_change_before_fee_quote: clean_zero(totals.0 - baseline_totals.0),
                nav_change_after_fee_quote: clean_zero(totals.1 - baseline_totals.1),
                estimated_trading_fee_quote: clean_zero(totals.2 - baseline_totals.2),
            },
        );
    }
    let last = points.last().copied().unwrap_or_default();
    push_or_replace_point(
        &mut points,
        TheoreticalNavPoint {
            ts_us: end_ts_us,
            ..last
        },
    );
    let original_len = points.len();
    let points = downsample_points(points, max_points.max(2));
    Ok(TheoreticalNavTimeline {
        available_from_us: Some(available_from_us),
        latest_point_ts_us,
        sampled: points.len() < original_len,
        points,
        ..TheoreticalNavTimeline::default()
    })
}

fn decode_contribution(row: PgRow) -> Result<StoredContribution> {
    Ok(StoredContribution {
        key: row.try_get("source_id")?,
        ts_us: row.try_get("ts_us")?,
        nav_before_fee: row.try_get("cumulative_nav_before_fee_quote")?,
        nav_after_fee: row.try_get("cumulative_nav_after_fee_quote")?,
        fee: row.try_get("cumulative_estimated_trading_fee_quote")?,
    })
}

fn contribution_totals<'a>(
    rows: impl IntoIterator<Item = &'a StoredContribution>,
) -> (f64, f64, f64) {
    rows.into_iter().fold((0.0, 0.0, 0.0), |totals, row| {
        (
            totals.0 + row.nav_before_fee,
            totals.1 + row.nav_after_fee,
            totals.2 + row.fee,
        )
    })
}

fn push_or_replace_point(points: &mut Vec<TheoreticalNavPoint>, point: TheoreticalNavPoint) {
    if let Some(last) = points.last_mut()
        && last.ts_us == point.ts_us
    {
        *last = point;
    } else {
        points.push(point);
    }
}

fn downsample_points(
    points: Vec<TheoreticalNavPoint>,
    max_points: usize,
) -> Vec<TheoreticalNavPoint> {
    if points.len() <= max_points || max_points < 6 {
        return points;
    }
    let interior = &points[1..points.len() - 1];
    let bucket_count = ((max_points - 2) / 4).max(1);
    let bucket_size = interior.len().div_ceil(bucket_count);
    let mut sampled = Vec::with_capacity(max_points);
    sampled.push(points[0]);
    for bucket in interior.chunks(bucket_size) {
        let mut extrema = Vec::with_capacity(4);
        for selector in [
            |point: &TheoreticalNavPoint| point.nav_change_before_fee_quote,
            |point: &TheoreticalNavPoint| point.nav_change_after_fee_quote,
        ] {
            if let Some(row) = bucket.iter().min_by(|left, right| {
                selector(left)
                    .partial_cmp(&selector(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                extrema.push(*row);
            }
            if let Some(row) = bucket.iter().max_by(|left, right| {
                selector(left)
                    .partial_cmp(&selector(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                extrema.push(*row);
            }
        }
        extrema.sort_by_key(|row| row.ts_us);
        extrema.dedup_by_key(|row| row.ts_us);
        sampled.extend(extrema);
    }
    sampled.push(*points.last().expect("non-empty theoretical NAV points"));
    sampled
}

fn clean_zero(value: f64) -> f64 {
    if value.abs() <= ZERO_EPSILON {
        0.0
    } else {
        value
    }
}

fn quantities_equal(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= ZERO_EPSILON * scale
}

fn unix_now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(end_ts_us: i64, twap: f64) -> TwapBar {
        TwapBar {
            end_ts_us,
            twap,
            sample_count: 1,
            first_ts_us: end_ts_us - 1_000_000,
        }
    }

    fn latest_targets(targets: BTreeMap<String, f64>) -> LatestTargets {
        LatestTargets {
            position_strategy_name: "cta_a".into(),
            venue: "binance-futures".into(),
            targets,
            received_at_us: 1,
            update_seq: 0,
        }
    }

    #[test]
    fn repeated_target_metadata_does_not_create_an_execution() {
        let previous = latest_targets(BTreeMap::from([("BTCUSDT".into(), 1.0)]));
        let mut repeated = previous.clone();
        repeated.position_strategy_name = "renamed".into();
        repeated.received_at_us = 2;
        repeated.update_seq = 4;
        assert!(!target_positions_changed(Some(&previous), &repeated));

        repeated.targets.insert("BTCUSDT".into(), 2.0);
        assert!(target_positions_changed(Some(&previous), &repeated));
    }

    #[test]
    fn stored_zero_targets_are_equivalent_to_omitted_targets() {
        let mut targets = BTreeMap::from([
            ("BTCUSDT".into(), 0.0),
            ("ETHUSDT".into(), -0.0),
            ("SOLUSDT".into(), 3.0),
        ]);
        normalize_stored_targets(&mut targets).unwrap();
        assert_eq!(targets, BTreeMap::from([("SOLUSDT".into(), 3.0)]));
    }

    #[test]
    fn fifo_quantity_check_scales_with_large_positions() {
        assert!(quantities_equal(100_000.0, 100_000.0 + 1e-8));
        assert!(!quantities_equal(100_000.0, 100_000.0 + 1e-4));
        assert!(!quantities_equal(1.0, 1.0 + 1e-8));
    }

    #[test]
    fn five_minute_twap_uses_equal_minute_buckets() {
        let bars = (1..=5)
            .map(|minute| bar(i64::from(minute) * MINUTE_US, 100.0 + f64::from(minute)))
            .collect::<Vec<_>>();
        assert_eq!(complete_window_twap(&bars, 0, 5 * MINUTE_US), Some(103.0));
    }

    #[test]
    fn twap_requires_at_least_one_bar_in_every_minute() {
        let bars = vec![bar(MINUTE_US, 100.0), bar(3 * MINUTE_US, 103.0)];
        assert_eq!(complete_window_twap(&bars, 0, 3 * MINUTE_US), None);
    }

    #[test]
    fn portfolio_mark_uses_a_recent_completed_five_second_mid() {
        let bars = vec![bar(290_000_000, 99.0), bar(300_000_000, 101.0)];
        assert_eq!(completed_mark_mid(&bars, 300_000_000), Some(101.0));
        assert_eq!(completed_mark_mid(&bars, 301_000_000), Some(101.0));
        assert_eq!(completed_mark_mid(&bars, 311_000_000), None);
    }

    #[test]
    fn mark_cursor_stays_strictly_before_the_exclusive_boundary() {
        assert_eq!(
            mark_strictly_before(5 * MARK_INTERVAL_US),
            4 * MARK_INTERVAL_US
        );
        assert_eq!(
            mark_strictly_before(5 * MARK_INTERVAL_US + 1),
            5 * MARK_INTERVAL_US
        );
    }

    #[test]
    fn open_position_keeps_accruing_pnl_between_signals() {
        let lots = VecDeque::from([
            FifoLot {
                seq: 1,
                quantity: 2.0,
                entry_price: 100.0,
            },
            FifoLot {
                seq: 2,
                quantity: 0.5,
                entry_price: 110.0,
            },
        ]);
        assert_eq!(floating_pnl_at_mark(&lots, 120.0).unwrap(), 45.0);
        assert_eq!(floating_pnl_at_mark(&lots, 90.0).unwrap(), -30.0);
    }

    #[test]
    fn fifo_realizes_long_and_keeps_the_remainder() {
        let lots = VecDeque::from([FifoLot {
            seq: 1,
            quantity: 2.0,
            entry_price: 100.0,
        }]);
        let (lots, realized, next) = apply_fifo_fill(lots, -1.5, 110.0, 2).unwrap();
        assert_eq!(realized, 15.0);
        assert_eq!(next, 2);
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].quantity, 0.5);
    }

    #[test]
    fn fifo_crosses_from_short_to_long() {
        let lots = VecDeque::from([FifoLot {
            seq: 1,
            quantity: -1.0,
            entry_price: 100.0,
        }]);
        let (lots, realized, next) = apply_fifo_fill(lots, 1.5, 90.0, 2).unwrap();
        assert_eq!(realized, 10.0);
        assert_eq!(next, 3);
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].quantity, 0.5);
        assert_eq!(lots[0].entry_price, 90.0);
    }

    #[test]
    fn theoretical_nav_has_before_and_after_fee_values() {
        let applied = evaluate_fill(
            SymbolState {
                next_lot_seq: 1,
                ..SymbolState::default()
            },
            VecDeque::new(),
            2.0,
            100.0,
            0.001,
        )
        .unwrap();
        assert_eq!(applied.nav_before_fee_quote, 0.0);
        assert!((applied.fee_quote - 0.2).abs() < 1e-12);
        assert!((applied.nav_after_fee_quote + 0.2).abs() < 1e-12);

        let rebate = evaluate_fill(
            SymbolState {
                next_lot_seq: 1,
                ..SymbolState::default()
            },
            VecDeque::new(),
            2.0,
            100.0,
            -0.001,
        )
        .unwrap();
        assert!((rebate.nav_after_fee_quote - 0.2).abs() < 1e-12);
    }

    #[test]
    fn repeated_timestamp_replaces_the_sparse_portfolio_point() {
        let mut points = vec![TheoreticalNavPoint {
            ts_us: 10,
            nav_change_before_fee_quote: 1.0,
            ..TheoreticalNavPoint::default()
        }];
        push_or_replace_point(
            &mut points,
            TheoreticalNavPoint {
                ts_us: 10,
                nav_change_before_fee_quote: 2.0,
                ..TheoreticalNavPoint::default()
            },
        );
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].nav_change_before_fee_quote, 2.0);
    }
}
