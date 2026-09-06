use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row, postgres::PgPool};
use tokio::sync::Mutex;

use crate::config::{AppConfig, SourceConfig};
use crate::nav::{self, NavSourceHistories, SourcePositionSnapshots};
use crate::snapshot::PositionSnapshot;

const DEFAULT_MAX_POINTS: usize = 1_000;
const MAX_POINTS: usize = 4_000;
const GRID_MS: i64 = 15 * 60 * 1_000;
const DAY_US: i64 = 24 * 60 * 60 * 1_000_000;

/// Persistent daily state for position-history requests.  This is intentionally
/// separate from the immutable operator supplied position snapshots.
#[derive(Clone, Default)]
pub struct DailyPositionHistory {
    refresh: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredPosition {
    symbol: String,
    venue_code: i16,
    venue: String,
    quantity: f64,
    last_price: Option<f64>,
    valuation_source: Option<String>,
    last_fill_ts_us: Option<i64>,
    last_fill_event_ts_us: Option<i64>,
    last_fill_record_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecentRecord {
    record_key: String,
    fingerprint: String,
}

struct BootstrapCheckpoint {
    day_start_us: i64,
    fills_recv_end_us: i64,
    positions: Vec<StoredPosition>,
}

struct BootstrapPlan {
    effective_anchor_ts_us: Option<i64>,
    checkpoints: Vec<BootstrapCheckpoint>,
    recent_records: Vec<RecentRecord>,
}

#[derive(Clone, Debug)]
struct ScannedSource {
    events: Vec<crate::model::UniformOrderEvent>,
}

struct DailyCheckpointMetadata {
    source: SourceConfig,
    effective_anchor_ts_us: Option<i64>,
    day_start_us: i64,
    fills_recv_end_us: i64,
    positions: Vec<StoredPosition>,
    has_original_snapshot: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionHistoryResponse {
    pub generated_at_us: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub selected_source_ids: Vec<String>,
    pub selected_symbols: Vec<String>,
    pub available_sources: Vec<AvailableSource>,
    pub available_symbols: Vec<String>,
    pub leverage_basis: String,
    pub current_equity: CurrentEquityMetadata,
    pub portfolio_points: Vec<PortfolioPoint>,
    pub account_points: Vec<AccountPoint>,
    pub symbol_points: Vec<SymbolPoint>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CurrentEquityMetadata {
    pub equity_usdt: Option<f64>,
    pub availability: String,
    pub accounts: Vec<CurrentEquityAccount>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CurrentEquityAccount {
    pub source_id: String,
    pub equity_usdt: Option<f64>,
    pub ts_ms: Option<i64>,
    pub availability: String,
}

#[derive(Debug, Clone)]
pub struct CurrentEquityInput {
    pub source_id: String,
    pub equity_usdt: Option<f64>,
    pub ts_ms: Option<i64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct AvailableSource {
    pub source_id: String,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioPoint {
    pub ts_ms: i64,
    pub equity_usdt: Option<f64>,
    pub gross_notional_usdt: Option<f64>,
    pub gross_leverage: Option<f64>,
    pub availability: String,
    pub missing_source_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct AccountPoint {
    pub ts_ms: i64,
    pub source_id: String,
    pub equity_usdt: Option<f64>,
    pub gross_notional_usdt: Option<f64>,
    pub gross_leverage: Option<f64>,
    pub availability: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct SymbolPoint {
    pub ts_ms: i64,
    pub source_id: String,
    pub symbol: String,
    pub venue: String,
    pub quantity: Option<f64>,
    pub signed_notional_usdt: Option<f64>,
    pub gross_notional_usdt: Option<f64>,
    pub leverage_contribution: Option<f64>,
    pub valuation_price: Option<f64>,
    pub valuation_source: String,
    pub availability: String,
}

#[derive(Clone, Debug)]
struct PositionEvent {
    ts_us: i64,
    quantity: f64,
    last_price: Option<f64>,
    valuation_source: Option<&'static str>,
}
#[derive(Clone, Debug)]
struct PositionSeries {
    venue: String,
    events: Vec<PositionEvent>,
}
#[derive(Clone, Debug)]
struct SourceIndex {
    anchor_ts_us: Option<i64>,
    available_anchor_ts_us: Option<i64>,
    last_ts_us: Option<i64>,
    event_times_ms: Vec<i64>,
    positions: BTreeMap<(String, i16), PositionSeries>,
}
#[derive(Clone, Debug, Default)]
pub struct PositionHistoryIndex {
    sources: BTreeMap<String, SourceIndex>,
}

impl PositionHistoryIndex {
    pub fn build(
        config: &AppConfig,
        histories: &NavSourceHistories,
        snapshots: &SourcePositionSnapshots,
    ) -> Result<Self> {
        let mut sources = BTreeMap::new();
        for source in config.sources.iter().filter(|source| source.enabled) {
            let index = build_source(
                source,
                snapshots.get(&source.id),
                histories
                    .get(&source.id)
                    .map(|history| history.events())
                    .unwrap_or_default(),
            )?;
            sources.insert(source.id.clone(), index);
        }
        Ok(Self { sources })
    }

    pub fn load(
        &self,
        start_ms: i64,
        end_ms: i64,
        source_ids: Vec<String>,
        symbols: Vec<String>,
        max_points: Option<usize>,
        generated_at_us: i64,
    ) -> Result<PositionHistoryResponse> {
        let end_ms = end_ms.min(generated_at_us.saturating_div(1_000));
        if start_ms < 0 || end_ms <= start_ms {
            bail!("invalid position history range");
        }
        if max_points == Some(0) {
            bail!("maxPoints must be at least one");
        }
        let max_points = max_points.unwrap_or(DEFAULT_MAX_POINTS).min(MAX_POINTS);
        let selected_source_ids = source_ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let selected_symbols = symbols
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let definitions = self.definitions(&selected_source_ids, &selected_symbols);
        let available_sources = selected_source_ids
            .iter()
            .filter_map(|source_id| {
                let source = self.sources.get(source_id)?;
                let anchor = ceil_us_to_ms(source.available_anchor_ts_us?);
                let last = source.last_ts_us.map(ceil_us_to_ms).unwrap_or(anchor);
                Some(AvailableSource {
                    source_id: source_id.clone(),
                    first_ts_ms: anchor,
                    last_ts_ms: last,
                })
            })
            .collect();
        let available_symbols = self
            .definitions(&selected_source_ids, &[])
            .into_iter()
            .map(|(_, symbol, _, _)| symbol)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let event_times =
            self.event_times_in_range(&selected_source_ids, start_ms, end_ms, max_points);
        let timestamps = sample_timestamps(start_ms, end_ms, max_points, &event_times);
        let mut portfolio_points = Vec::with_capacity(timestamps.len());
        let mut account_points = Vec::with_capacity(timestamps.len() * selected_source_ids.len());
        let mut symbol_points = Vec::with_capacity(timestamps.len() * definitions.len());
        for ts_ms in timestamps {
            let ts_us = ts_ms.saturating_mul(1_000);
            let mut gross_total = 0.0;
            let mut missing = Vec::new();
            for source_id in &selected_source_ids {
                let valid = self.sources.get(source_id).is_some_and(|source| {
                    source.anchor_ts_us.is_some_and(|anchor| ts_us >= anchor)
                });
                if !valid {
                    missing.push(source_id.clone());
                    account_points.push(missing_account(ts_ms, source_id));
                    continue;
                }
                let source = &self.sources[source_id];
                let gross = source
                    .positions
                    .iter()
                    .filter(|((symbol, _), _)| {
                        selected_symbols.is_empty()
                            || selected_symbols.binary_search(symbol).is_ok()
                    })
                    .try_fold(0.0, |total, (_, series)| {
                        as_of(series, ts_us).3.map(|value| total + value)
                    });
                match gross {
                    Some(gross) => {
                        gross_total += gross;
                        account_points.push(AccountPoint {
                            ts_ms,
                            source_id: source_id.clone(),
                            equity_usdt: None,
                            gross_notional_usdt: Some(clean(gross)),
                            gross_leverage: None,
                            availability: "missing_equity".into(),
                        });
                    }
                    None => {
                        missing.push(source_id.clone());
                        account_points.push(missing_mark_account(ts_ms, source_id));
                    }
                }
            }
            portfolio_points.push(PortfolioPoint {
                ts_ms,
                equity_usdt: None,
                gross_notional_usdt: missing.is_empty().then_some(clean(gross_total)),
                gross_leverage: None,
                availability: if missing.is_empty() {
                    "missing_equity".into()
                } else {
                    "incomplete".into()
                },
                missing_source_ids: missing,
            });
            for (source_id, symbol, venue_code, venue) in &definitions {
                let valid = self.sources.get(source_id).is_some_and(|source| {
                    source.anchor_ts_us.is_some_and(|anchor| ts_us >= anchor)
                });
                if !valid {
                    symbol_points.push(missing_symbol(ts_ms, source_id, symbol, venue));
                    continue;
                }
                let (quantity, price, signed, gross, valuation_source) = as_of(
                    &self.sources[source_id].positions[&(symbol.clone(), *venue_code)],
                    ts_us,
                );
                symbol_points.push(SymbolPoint {
                    ts_ms,
                    source_id: source_id.clone(),
                    symbol: symbol.clone(),
                    venue: venue.clone(),
                    quantity: Some(clean(quantity)),
                    signed_notional_usdt: signed.map(clean),
                    gross_notional_usdt: gross.map(clean),
                    leverage_contribution: None,
                    valuation_price: price,
                    valuation_source: valuation_source.into(),
                    availability: if gross.is_some() {
                        "missing_equity".into()
                    } else {
                        "missing_mark".into()
                    },
                });
            }
        }
        Ok(PositionHistoryResponse {
            generated_at_us,
            start_ms,
            end_ms,
            selected_source_ids: selected_source_ids.clone(),
            selected_symbols,
            available_sources,
            available_symbols,
            leverage_basis: "current_account_equity".into(),
            current_equity: CurrentEquityMetadata {
                equity_usdt: None,
                availability: "incomplete".into(),
                accounts: selected_source_ids
                    .iter()
                    .map(|source_id| CurrentEquityAccount {
                        source_id: source_id.clone(),
                        equity_usdt: None,
                        ts_ms: None,
                        availability: "missing".into(),
                    })
                    .collect(),
            },
            portfolio_points,
            account_points,
            symbol_points,
        })
    }

    fn definitions(
        &self,
        source_ids: &[String],
        symbols: &[String],
    ) -> Vec<(String, String, i16, String)> {
        source_ids
            .iter()
            .filter_map(|id| self.sources.get(id).map(|source| (id, source)))
            .flat_map(|(id, source)| {
                source
                    .positions
                    .iter()
                    .filter(move |((symbol, _), _)| {
                        symbols.is_empty() || symbols.binary_search(symbol).is_ok()
                    })
                    .map(move |((symbol, venue), series)| {
                        (id.clone(), symbol.clone(), *venue, series.venue.clone())
                    })
            })
            .collect()
    }

    fn event_times_in_range(
        &self,
        source_ids: &[String],
        start_ms: i64,
        end_ms: i64,
        max_points: usize,
    ) -> Vec<i64> {
        let mut ranges = Vec::with_capacity(source_ids.len());
        let mut count = 0_usize;
        for source_id in source_ids {
            let Some(source) = self.sources.get(source_id) else {
                continue;
            };
            let from = source.event_times_ms.partition_point(|ts| *ts < start_ms);
            let to = source.event_times_ms.partition_point(|ts| *ts <= end_ms);
            count = count.saturating_add(to.saturating_sub(from));
            if count > max_points {
                return Vec::new();
            }
            ranges.push((&source.event_times_ms, from, to));
        }
        let mut result = Vec::with_capacity(count);
        for (times, from, to) in ranges {
            result.extend_from_slice(&times[from..to]);
        }
        result.sort_unstable();
        result.dedup();
        result
    }
}

impl PositionHistoryResponse {
    /// Apply the response-time account monitor snapshot to historical position
    /// points. Historical position reconstruction intentionally has no equity
    /// state of its own.
    pub fn apply_current_equity(&mut self, inputs: Vec<CurrentEquityInput>, now_ms: i64) {
        let inputs = inputs
            .into_iter()
            .map(|input| (input.source_id.clone(), input))
            .collect::<BTreeMap<_, _>>();
        let accounts = self
            .selected_source_ids
            .iter()
            .map(|source_id| {
                let input = inputs.get(source_id);
                let availability = current_equity_availability(
                    input.and_then(|input| input.equity_usdt),
                    input.and_then(|input| input.ts_ms),
                    now_ms,
                );
                CurrentEquityAccount {
                    source_id: source_id.clone(),
                    equity_usdt: input
                        .and_then(|input| input.equity_usdt)
                        .filter(|value| value.is_finite()),
                    ts_ms: input.and_then(|input| input.ts_ms),
                    availability: availability.into(),
                }
            })
            .collect::<Vec<_>>();
        let all_fresh_finite = accounts
            .iter()
            .all(|account| matches!(account.availability.as_str(), "ok" | "nonpositive"));
        let raw_portfolio_equity = all_fresh_finite
            .then(|| {
                accounts
                    .iter()
                    .filter_map(|account| account.equity_usdt)
                    .sum::<f64>()
            })
            .filter(|equity| equity.is_finite());
        let portfolio_equity = raw_portfolio_equity.filter(|equity| *equity > 0.0);
        let portfolio_availability = if portfolio_equity.is_some() {
            "ok"
        } else if raw_portfolio_equity.is_some() {
            "nonpositive"
        } else {
            "incomplete"
        };
        let account_by_id = accounts
            .iter()
            .map(|account| (account.source_id.as_str(), account))
            .collect::<BTreeMap<_, _>>();
        for point in &mut self.account_points {
            let account = account_by_id
                .get(point.source_id.as_str())
                .expect("selected account");
            point.equity_usdt = account.equity_usdt;
            if point.gross_notional_usdt.is_some() && !historical_unavailable(&point.availability) {
                point.availability = point_equity_availability(&account.availability).into();
                point.gross_leverage = (account.availability == "ok")
                    .then(|| {
                        safe_ratio(
                            point.gross_notional_usdt.unwrap_or_default(),
                            account.equity_usdt.unwrap(),
                        )
                    })
                    .flatten();
                if account.availability == "ok" && point.gross_leverage.is_none() {
                    point.availability = "incomplete".into();
                }
            }
        }
        for point in &mut self.portfolio_points {
            point.equity_usdt = raw_portfolio_equity;
            if point.gross_notional_usdt.is_some() && point.missing_source_ids.is_empty() {
                point.availability = point_equity_availability(portfolio_availability).into();
                point.gross_leverage = portfolio_equity.and_then(|equity| {
                    safe_ratio(point.gross_notional_usdt.unwrap_or_default(), equity)
                });
                if portfolio_equity.is_some() && point.gross_leverage.is_none() {
                    point.availability = "incomplete".into();
                }
            }
        }
        for point in &mut self.symbol_points {
            if point.gross_notional_usdt.is_some() && !historical_unavailable(&point.availability) {
                point.availability = point_equity_availability(portfolio_availability).into();
                point.leverage_contribution = portfolio_equity.and_then(|equity| {
                    safe_ratio(point.gross_notional_usdt.unwrap_or_default(), equity)
                });
                if portfolio_equity.is_some() && point.leverage_contribution.is_none() {
                    point.availability = "incomplete".into();
                }
            }
        }
        self.current_equity = CurrentEquityMetadata {
            equity_usdt: raw_portfolio_equity,
            availability: portfolio_availability.into(),
            accounts,
        };
    }
}

impl DailyPositionHistory {
    /// Advance derived daily state without constructing a browser response.
    pub async fn refresh_only(
        &self,
        pool: &PgPool,
        config: &AppConfig,
        snapshots: &SourcePositionSnapshots,
        generated_at_us: i64,
    ) -> Result<()> {
        let cutoff_us = generated_at_us.saturating_sub(safety_lag_us(config));
        let selected = config
            .sources
            .iter()
            .filter(|source| source.enabled)
            .cloned()
            .collect();
        let _guard = self.refresh.lock().await;
        self.sync_selected(pool, config, snapshots, selected, cutoff_us)
            .await
    }

    /// Synchronize the receive-time tail and build the requested response from
    /// the closest UTC-day state plus RocksDB events in range. The mutex keeps
    /// one history refresh and query reconstruction coherent at a time.
    pub async fn load(
        &self,
        pool: &PgPool,
        config: &AppConfig,
        snapshots: &SourcePositionSnapshots,
        start_ms: i64,
        end_ms: i64,
        source_ids: Vec<String>,
        symbols: Vec<String>,
        max_points: Option<usize>,
        generated_at_us: i64,
    ) -> Result<PositionHistoryResponse> {
        let cutoff_us = generated_at_us.saturating_sub(safety_lag_us(config));
        let end_ms = end_ms.min(cutoff_us.saturating_div(1_000));
        if start_ms < 0 || end_ms <= start_ms {
            bail!("invalid position history range");
        }
        let _guard = self.refresh.lock().await;
        let source_ids = if source_ids.is_empty() {
            config
                .sources
                .iter()
                .filter(|source| source.enabled)
                .map(|source| source.id.clone())
                .collect()
        } else {
            source_ids
        };
        let selected = config
            .sources
            .iter()
            .filter(|source| source.enabled && source_ids.contains(&source.id))
            .cloned()
            .collect::<Vec<_>>();
        self.sync_selected(pool, config, snapshots, selected.clone(), cutoff_us)
            .await?;

        let mut metadata = Vec::new();
        for source in selected {
            let has_original_snapshot = snapshots.contains_key(&source.id);
            if let Some(metadata_row) = load_daily_checkpoint_metadata(
                pool,
                source,
                has_original_snapshot,
                start_ms.saturating_mul(1_000),
                end_ms.saturating_mul(1_000),
            )
            .await?
            {
                metadata.push(metadata_row);
            }
        }
        let mut sources = BTreeMap::new();
        let mut first_error = None;
        for metadata in metadata {
            match build_daily_source(metadata, config.clone(), cutoff_us).await {
                Ok((source_id, index)) => {
                    sources.insert(source_id, index);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        PositionHistoryIndex { sources }
            .load(start_ms, end_ms, source_ids, symbols, max_points, cutoff_us)
    }

    async fn sync_selected(
        &self,
        pool: &PgPool,
        config: &AppConfig,
        snapshots: &SourcePositionSnapshots,
        selected: Vec<SourceConfig>,
        cutoff_us: i64,
    ) -> Result<()> {
        let mut first_error = None;
        for source in selected {
            let snapshot = snapshots.get(&source.id).cloned();
            if let Err(error) = self
                .sync_source(pool, config, &source, snapshot.as_ref(), cutoff_us)
                .await
            {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    async fn sync_source(
        &self,
        pool: &PgPool,
        config: &AppConfig,
        source: &SourceConfig,
        snapshot: Option<&PositionSnapshot>,
        cutoff_us: i64,
    ) -> Result<()> {
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&source.id)
            .execute(&mut *tx)
            .await?;
        self.sync_source_inner(&mut tx, config, source, snapshot, cutoff_us)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn sync_source_inner(
        &self,
        connection: &mut PgConnection,
        config: &AppConfig,
        source: &SourceConfig,
        snapshot: Option<&PositionSnapshot>,
        cutoff_us: i64,
    ) -> Result<()> {
        let fingerprint = anchor_fingerprint(source, snapshot)?;
        let state = sqlx::query(
            "SELECT anchor_fingerprint, effective_anchor_ts_us, scanned_recv_ts_us, recent_records \
             FROM cta_position_history_sources WHERE source_id = $1",
        )
        .bind(&source.id)
        .fetch_optional(&mut *connection)
        .await?;
        let Some(state) = state else {
            return self
                .bootstrap(connection, config, source, snapshot, fingerprint, cutoff_us)
                .await;
        };
        let old_fingerprint: String = state.try_get("anchor_fingerprint")?;
        if old_fingerprint != fingerprint {
            sqlx::query("DELETE FROM cta_position_history_daily_checkpoints WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            sqlx::query("DELETE FROM cta_position_history_sources WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            return self
                .bootstrap(connection, config, source, snapshot, fingerprint, cutoff_us)
                .await;
        }
        let watermark: i64 = state.try_get("scanned_recv_ts_us")?;
        let effective_anchor_ts_us: Option<i64> = state.try_get("effective_anchor_ts_us")?;
        if cutoff_us <= watermark {
            return Ok(());
        }
        let recent: Vec<RecentRecord> = serde_json::from_value(state.try_get("recent_records")?)
            .context("invalid position-history receive overlap ledger")?;
        let overlap_us = overlap_us(config);
        let from = watermark.saturating_sub(overlap_us).max(0);
        let scan_source = source.clone();
        let path = source.rocksdb_path.clone();
        let scanned = tokio::task::spawn_blocking(move || {
            scan_source_events(&scan_source, &path, from, cutoff_us)
        })
        .await
        .context("position history incremental reader failed")??;
        let known = recent
            .into_iter()
            .map(|record| (record.record_key, record.fingerprint))
            .collect::<BTreeMap<_, _>>();
        let mut changed = false;
        for event in &scanned.events {
            let digest = event_fingerprint(event)?;
            if let Some(old) = known.get(&event.record_key) {
                if old != &digest {
                    changed = true;
                }
            }
        }
        // A changed payload cannot be corrected from a delta without retaining
        // raw fills. Rebuild from the immutable RocksDB source instead.
        if changed {
            sqlx::query("DELETE FROM cta_position_history_daily_checkpoints WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            sqlx::query("DELETE FROM cta_position_history_sources WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            return self
                .bootstrap(connection, config, source, snapshot, fingerprint, cutoff_us)
                .await;
        }
        let mut ledger = known;
        let mut late = Vec::new();
        for event in &scanned.events {
            let digest = event_fingerprint(event)?;
            if ledger.insert(event.record_key.clone(), digest).is_none() {
                late.push(event.clone());
            }
        }
        let fills = late
            .iter()
            .filter_map(|event| fill_from_event(source, event).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|fill| snapshot.is_none_or(|anchor| fill.ts_us > anchor.snapshot_ts_us))
            .collect::<Vec<_>>();
        let requires_rebuild = late
            .iter()
            .try_fold(false, |required, event| -> Result<bool> {
                let Some(fill) = fill_from_event(source, event)? else {
                    return Ok(required);
                };
                Ok(required
                    || (snapshot.is_none()
                        && effective_anchor_ts_us.is_some_and(|anchor| fill.ts_us < anchor))
                    || snapshot.is_some_and(|anchor| fill.ts_us <= anchor.snapshot_ts_us))
            })?;
        if requires_rebuild {
            sqlx::query("DELETE FROM cta_position_history_daily_checkpoints WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            sqlx::query("DELETE FROM cta_position_history_sources WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            return self
                .bootstrap(connection, config, source, snapshot, fingerprint, cutoff_us)
                .await;
        }
        let mut recv_end_by_day = BTreeMap::<i64, i64>::new();
        for fill in &fills {
            recv_end_by_day
                .entry(floor_day(fill.ts_us))
                .and_modify(|end| *end = (*end).max(fill.recv_ts_us.saturating_add(1)))
                .or_insert(fill.recv_ts_us.saturating_add(1));
        }
        for (day, recv_end) in recv_end_by_day {
            sqlx::query("UPDATE cta_position_history_daily_checkpoints SET fills_recv_end_us = GREATEST(fills_recv_end_us, $3) WHERE source_id = $1 AND day_start_us = $2")
                .bind(&source.id)
                .bind(day)
                .bind(recv_end)
                .execute(&mut *connection)
                .await?;
        }
        let latest_checkpoint_day = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT max(day_start_us) FROM cta_position_history_daily_checkpoints WHERE source_id = $1",
        )
        .bind(&source.id)
        .fetch_one(&mut *connection)
        .await?;
        if let Some(latest_checkpoint_day) = latest_checkpoint_day {
            for fill in fills {
                if floor_day(fill.ts_us) < latest_checkpoint_day {
                    self.apply_late_fill(connection, &source.id, fill).await?;
                }
            }
        }
        let checkpoint_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cta_position_history_daily_checkpoints WHERE source_id = $1",
        )
        .bind(&source.id)
        .fetch_one(&mut *connection)
        .await?;
        if checkpoint_count == 0 && !scanned.events.is_empty() {
            sqlx::query("DELETE FROM cta_position_history_sources WHERE source_id = $1")
                .bind(&source.id)
                .execute(&mut *connection)
                .await?;
            return self
                .bootstrap(connection, config, source, snapshot, fingerprint, cutoff_us)
                .await;
        }
        self.ensure_days(connection, source, snapshot, cutoff_us)
            .await?;
        let keep_from = cutoff_us.saturating_sub(overlap_us);
        let recent = ledger
            .into_iter()
            .filter(|(key, _)| key.parse::<i64>().is_ok_and(|ts| ts >= keep_from))
            .map(|(record_key, fingerprint)| RecentRecord {
                record_key,
                fingerprint,
            })
            .collect::<Vec<_>>();
        sqlx::query("UPDATE cta_position_history_sources SET scanned_recv_ts_us=$2, recent_records=$3, updated_at=now() WHERE source_id=$1")
            .bind(&source.id).bind(cutoff_us).bind(serde_json::to_value(recent)?).execute(&mut *connection).await?;
        Ok(())
    }

    async fn bootstrap(
        &self,
        connection: &mut PgConnection,
        config: &AppConfig,
        source: &SourceConfig,
        snapshot: Option<&PositionSnapshot>,
        fingerprint: String,
        cutoff_us: i64,
    ) -> Result<()> {
        if !source.rocksdb_path.is_dir() {
            // A reserved/missing source is empty for this request. Do not
            // advance its receive cursor, so history is discovered on arrival.
            return Ok(());
        }
        let start = source.start_ts_us.unwrap_or(0);
        let source_for_scan = source.clone();
        let path = source.rocksdb_path.clone();
        let events = tokio::task::spawn_blocking(move || {
            read_events(&source_for_scan, &path, start, cutoff_us)
        })
        .await
        .context("position history initial reader failed")??;
        let source_for_plan = source.clone();
        let snapshot_for_plan = snapshot.cloned();
        let overlap_us = overlap_us(config);
        let plan = tokio::task::spawn_blocking(move || {
            build_bootstrap_plan(
                &source_for_plan,
                snapshot_for_plan.as_ref(),
                events,
                cutoff_us,
                overlap_us,
            )
        })
        .await
        .context("position history initial planner failed")??;
        for checkpoint in plan.checkpoints {
            sqlx::query("INSERT INTO cta_position_history_daily_checkpoints (source_id, day_start_us, anchor_fingerprint, fills_recv_end_us, positions) VALUES ($1,$2,$3,$4,$5)")
                .bind(&source.id).bind(checkpoint.day_start_us).bind(&fingerprint).bind(checkpoint.fills_recv_end_us).bind(serde_json::to_value(checkpoint.positions)?).execute(&mut *connection).await?;
        }
        sqlx::query("INSERT INTO cta_position_history_sources (source_id, anchor_fingerprint, effective_anchor_ts_us, scanned_recv_ts_us, recent_records) VALUES ($1,$2,$3,$4,$5)")
            .bind(&source.id).bind(&fingerprint).bind(plan.effective_anchor_ts_us).bind(cutoff_us).bind(serde_json::to_value(plan.recent_records)?).execute(&mut *connection).await?;
        Ok(())
    }

    async fn apply_late_fill(
        &self,
        connection: &mut PgConnection,
        source_id: &str,
        fill: Fill,
    ) -> Result<()> {
        let day = floor_day(fill.ts_us);
        let rows = sqlx::query("SELECT day_start_us, positions FROM cta_position_history_daily_checkpoints WHERE source_id=$1 AND day_start_us > $2 ORDER BY day_start_us")
            .bind(source_id).bind(day).fetch_all(&mut *connection).await?;
        for row in rows {
            let day_start: i64 = row.try_get("day_start_us")?;
            let mut positions: Vec<StoredPosition> =
                serde_json::from_value(row.try_get("positions")?)?;
            let mut state = positions
                .drain(..)
                .map(|item| ((item.symbol.clone(), item.venue_code), item))
                .collect::<BTreeMap<_, _>>();
            apply_state(&mut state, &fill);
            sqlx::query("UPDATE cta_position_history_daily_checkpoints SET positions=$3, completed_at=now() WHERE source_id=$1 AND day_start_us=$2")
                .bind(source_id).bind(day_start).bind(serde_json::to_value(state.values().collect::<Vec<_>>())?).execute(&mut *connection).await?;
        }
        Ok(())
    }

    async fn ensure_days(
        &self,
        connection: &mut PgConnection,
        source: &SourceConfig,
        snapshot: Option<&PositionSnapshot>,
        cutoff_us: i64,
    ) -> Result<()> {
        let target = floor_day(cutoff_us);
        let row = sqlx::query("SELECT day_start_us, anchor_fingerprint, positions FROM cta_position_history_daily_checkpoints WHERE source_id=$1 ORDER BY day_start_us DESC LIMIT 1")
            .bind(&source.id).fetch_optional(&mut *connection).await?;
        let Some(row) = row else { return Ok(()) };
        let mut day: i64 = row.try_get("day_start_us")?;
        let fingerprint: String = row.try_get("anchor_fingerprint")?;
        let positions: Vec<StoredPosition> = serde_json::from_value(row.try_get("positions")?)?;
        let mut state = positions
            .into_iter()
            .map(|item| ((item.symbol.clone(), item.venue_code), item))
            .collect::<BTreeMap<_, _>>();
        while day < target {
            let next = day.saturating_add(DAY_US);
            let scan_source = source.clone();
            let path = source.rocksdb_path.clone();
            let events = tokio::task::spawn_blocking(move || {
                // Receipt time can be later than the fill's event day.
                read_events(&scan_source, &path, day, cutoff_us)
            })
            .await
            .context("position history daily rollover reader failed")??;
            let mut fills = events
                .iter()
                .filter_map(|event| fill_from_event(source, event).transpose())
                .collect::<Result<Vec<_>>>()?;
            fills.retain(|fill| {
                fill.ts_us >= day
                    && fill.ts_us < next
                    && snapshot.is_none_or(|anchor| fill.ts_us > anchor.snapshot_ts_us)
            });
            fills.sort_by(|left, right| {
                left.ts_us
                    .cmp(&right.ts_us)
                    .then_with(|| left.event_ts_us.cmp(&right.event_ts_us))
                    .then_with(|| left.record_key.cmp(&right.record_key))
            });
            for fill in &fills {
                apply_state(&mut state, fill);
            }
            day = next;
            sqlx::query("INSERT INTO cta_position_history_daily_checkpoints (source_id, day_start_us, anchor_fingerprint, fills_recv_end_us, positions) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
                .bind(&source.id).bind(day).bind(&fingerprint).bind(day.saturating_add(DAY_US)).bind(serde_json::to_value(state.values().collect::<Vec<_>>())?).execute(&mut *connection).await?;
        }
        Ok(())
    }
}

async fn load_daily_checkpoint_metadata(
    pool: &PgPool,
    source: SourceConfig,
    has_original_snapshot: bool,
    start_us: i64,
    end_us: i64,
) -> Result<Option<DailyCheckpointMetadata>> {
    let effective_anchor_ts_us = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT effective_anchor_ts_us FROM cta_position_history_sources WHERE source_id = $1",
    )
    .bind(&source.id)
    .fetch_optional(pool)
    .await?
    .flatten();
    let row = sqlx::query(
        "SELECT day_start_us, positions, fills_recv_end_us FROM cta_position_history_daily_checkpoints \
         WHERE source_id = $1 AND day_start_us <= $2 ORDER BY day_start_us DESC LIMIT 1",
    )
    .bind(&source.id)
    .bind(floor_day(start_us))
    .fetch_optional(pool)
    .await?;
    let row = match row {
        Some(row) => row,
        None => match sqlx::query(
            "SELECT day_start_us, positions, fills_recv_end_us FROM cta_position_history_daily_checkpoints \
             WHERE source_id = $1 ORDER BY day_start_us ASC LIMIT 1",
        )
        .bind(&source.id)
        .fetch_optional(pool)
        .await?
        {
            Some(row) => row,
            None => return Ok(None),
        },
    };
    let day_start_us: i64 = row.try_get("day_start_us")?;
    let max_recv_end_us: i64 = sqlx::query_scalar(
        "SELECT coalesce(max(fills_recv_end_us), 0) FROM cta_position_history_daily_checkpoints \
         WHERE source_id = $1 AND day_start_us >= $2 AND day_start_us <= $3",
    )
    .bind(&source.id)
    .bind(day_start_us)
    .bind(floor_day(end_us))
    .fetch_one(pool)
    .await?;
    Ok(Some(DailyCheckpointMetadata {
        source,
        effective_anchor_ts_us,
        day_start_us,
        fills_recv_end_us: max_recv_end_us,
        positions: serde_json::from_value(row.try_get("positions")?)
            .context("invalid persisted daily position checkpoint")?,
        has_original_snapshot,
    }))
}

async fn build_daily_source(
    metadata: DailyCheckpointMetadata,
    config: AppConfig,
    cutoff_us: i64,
) -> Result<(String, SourceIndex)> {
    tokio::task::spawn_blocking(move || {
        let tail_start = metadata
            .day_start_us
            .saturating_sub(overlap_us(&config))
            .max(0);
        let tail_end = cutoff_us.min(
            metadata
                .fills_recv_end_us
                .max(metadata.day_start_us.saturating_add(DAY_US)),
        );
        let events = read_events(
            &metadata.source,
            &metadata.source.rocksdb_path,
            tail_start,
            tail_end,
        )?;
        let source_id = metadata.source.id.clone();
        build_source_from_daily_checkpoint(
            &metadata.source,
            metadata.day_start_us,
            metadata.effective_anchor_ts_us,
            metadata.has_original_snapshot,
            metadata.positions,
            &events,
        )
        .map(|index| (source_id, index))
    })
    .await
    .context("position history tail reader failed")?
}

#[derive(Clone, Debug)]
struct Fill {
    recv_ts_us: i64,
    ts_us: i64,
    event_ts_us: i64,
    record_key: String,
    symbol: String,
    venue_code: i16,
    price: f64,
    signed_quantity: f64,
    venue: String,
}

fn fill_from_event(
    source: &SourceConfig,
    event: &crate::model::UniformOrderEvent,
) -> Result<Option<Fill>> {
    Ok(nav::historical_fills(source, std::slice::from_ref(event))?
        .into_iter()
        .next()
        .map(|fill| Fill {
            recv_ts_us: event.recv_ts_us,
            ts_us: fill.ts_us,
            event_ts_us: fill.event_ts_us,
            record_key: fill.record_key,
            symbol: fill.symbol,
            venue_code: fill.venue_code,
            price: fill.price,
            signed_quantity: fill.signed_quantity,
            venue: fill.venue,
        }))
}

fn build_bootstrap_plan(
    source: &SourceConfig,
    snapshot: Option<&PositionSnapshot>,
    events: Vec<crate::model::UniformOrderEvent>,
    cutoff_us: i64,
    overlap_us: i64,
) -> Result<BootstrapPlan> {
    let mut fills = events
        .iter()
        .filter_map(|event| fill_from_event(source, event).transpose())
        .collect::<Result<Vec<_>>>()?;
    fills.sort_by(|left, right| {
        left.ts_us
            .cmp(&right.ts_us)
            .then_with(|| left.event_ts_us.cmp(&right.event_ts_us))
            .then_with(|| left.record_key.cmp(&right.record_key))
    });
    let anchor_ts = snapshot
        .map(|value| value.snapshot_ts_us)
        .or_else(|| fills.first().map(|fill| fill.ts_us));
    let recent_records = events
        .iter()
        .filter(|event| event.recv_ts_us >= cutoff_us.saturating_sub(overlap_us))
        .map(|event| {
            Ok(RecentRecord {
                record_key: event.record_key.clone(),
                fingerprint: event_fingerprint(event)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let Some(anchor_ts) = anchor_ts else {
        return Ok(BootstrapPlan {
            effective_anchor_ts_us: None,
            checkpoints: Vec::new(),
            recent_records: Vec::new(),
        });
    };
    let mut state = BTreeMap::<(String, i16), StoredPosition>::new();
    for position in snapshot
        .map(|value| value.positions.as_slice())
        .unwrap_or_default()
    {
        let prior = fills
            .iter()
            .filter(|fill| {
                fill.ts_us <= anchor_ts
                    && fill.symbol == position.symbol
                    && fill.venue_code == position.venue_code
            })
            .last();
        state.insert(
            (position.symbol.clone(), position.venue_code),
            StoredPosition {
                symbol: position.symbol.clone(),
                venue_code: position.venue_code,
                venue: crate::model::venue_name(position.venue_code as u8),
                quantity: position.quantity,
                last_price: prior.map(|fill| fill.price).or(position.reference_price),
                valuation_source: prior.map(|_| "last_fill".to_string()).or_else(|| {
                    position
                        .reference_price
                        .map(|_| "initial_reference".to_string())
                }),
                last_fill_ts_us: prior.map(|fill| fill.ts_us),
                last_fill_event_ts_us: prior.map(|fill| fill.event_ts_us),
                last_fill_record_key: prior.map(|fill| fill.record_key.clone()),
            },
        );
    }
    let mut recv_end_by_day = BTreeMap::<i64, i64>::new();
    for fill in &fills {
        if snapshot.is_none_or(|anchor| fill.ts_us > anchor.snapshot_ts_us) {
            let day = floor_day(fill.ts_us);
            recv_end_by_day
                .entry(day)
                .and_modify(|end| *end = (*end).max(fill.recv_ts_us.saturating_add(1)))
                .or_insert(fill.recv_ts_us.saturating_add(1));
        }
    }
    let mut fill_index = 0;
    while snapshot.is_some() && fill_index < fills.len() && fills[fill_index].ts_us <= anchor_ts {
        fill_index += 1;
    }
    let mut checkpoints = Vec::new();
    let mut day = floor_day(anchor_ts);
    let today = floor_day(cutoff_us);
    while day <= today {
        checkpoints.push(BootstrapCheckpoint {
            day_start_us: day,
            fills_recv_end_us: recv_end_by_day
                .get(&day)
                .copied()
                .unwrap_or_else(|| day.saturating_add(DAY_US)),
            positions: state.values().cloned().collect(),
        });
        let next = day.saturating_add(DAY_US);
        while fill_index < fills.len() && fills[fill_index].ts_us < next {
            apply_state(&mut state, &fills[fill_index]);
            fill_index += 1;
        }
        day = next;
    }
    Ok(BootstrapPlan {
        effective_anchor_ts_us: Some(anchor_ts),
        checkpoints,
        recent_records,
    })
}

fn apply_state(state: &mut BTreeMap<(String, i16), StoredPosition>, fill: &Fill) {
    let item = state
        .entry((fill.symbol.clone(), fill.venue_code))
        .or_insert_with(|| StoredPosition {
            symbol: fill.symbol.clone(),
            venue_code: fill.venue_code,
            venue: fill.venue.clone(),
            quantity: 0.0,
            last_price: None,
            valuation_source: None,
            last_fill_ts_us: None,
            last_fill_event_ts_us: None,
            last_fill_record_key: None,
        });
    item.quantity += fill.signed_quantity;
    let newer = item
        .last_fill_ts_us
        .zip(item.last_fill_event_ts_us)
        .is_none_or(|(ts, event_ts)| {
            (fill.ts_us, fill.event_ts_us, fill.record_key.as_str())
                > (
                    ts,
                    event_ts,
                    item.last_fill_record_key.as_deref().unwrap_or(""),
                )
        });
    if newer {
        item.last_price = Some(fill.price);
        item.valuation_source = Some("last_fill".into());
        item.last_fill_ts_us = Some(fill.ts_us);
        item.last_fill_event_ts_us = Some(fill.event_ts_us);
        item.last_fill_record_key = Some(fill.record_key.clone());
    }
}

fn floor_day(ts_us: i64) -> i64 {
    ts_us.max(0).saturating_div(DAY_US).saturating_mul(DAY_US)
}

fn safety_lag_us(config: &AppConfig) -> i64 {
    i64::try_from(config.ingestion.safety_lag_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
}

fn overlap_us(config: &AppConfig) -> i64 {
    i64::try_from(config.ingestion.overlap_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
}

fn anchor_fingerprint(
    source: &SourceConfig,
    snapshot: Option<&PositionSnapshot>,
) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        source.rocksdb_path.to_string_lossy(),
        source.start_ts_us,
        snapshot,
    ))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
fn event_fingerprint(event: &crate::model::UniformOrderEvent) -> Result<String> {
    // The source model deliberately does not serialize its wire payload. Hash
    // every field that affects position state and chronology instead.
    let text = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        event.record_key,
        event.recv_ts_us,
        event.event_ts_us,
        event.update_ts_us,
        event.symbol,
        event.venue_code,
        event.side_code,
        event.price,
        event.amount_update,
        event.venue,
    );
    Ok(hex::encode(Sha256::digest(text.as_bytes())))
}
fn read_events(
    source: &SourceConfig,
    path: &std::path::Path,
    start: i64,
    end: i64,
) -> Result<Vec<crate::model::UniformOrderEvent>> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    scan_source_events(source, path, start, end).map(|scan| scan.events)
}
fn scan_source_events(
    source: &SourceConfig,
    path: &std::path::Path,
    start: i64,
    end: i64,
) -> Result<ScannedSource> {
    let events = read_events_unchecked(source, path, start, end)?;
    Ok(ScannedSource { events })
}
fn read_events_unchecked(
    source: &SourceConfig,
    path: &std::path::Path,
    start: i64,
    end: i64,
) -> Result<Vec<crate::model::UniformOrderEvent>> {
    crate::rocks_source::read_uniform_orders(path, start, end)?
        .into_iter()
        .map(|record| {
            crate::model::decode_uniform_order(&record.key, &record.value).with_context(|| {
                format!("failed to decode position-history record for {}", source.id)
            })
        })
        .collect()
}

fn build_source(
    source: &SourceConfig,
    snapshot: Option<&PositionSnapshot>,
    events: &[crate::model::UniformOrderEvent],
) -> Result<SourceIndex> {
    if let Some(snapshot) = snapshot {
        snapshot.validate()?;
    }
    let fills = nav::historical_fills(source, events)?;
    let anchor_ts_us = snapshot
        .map(|snapshot| snapshot.snapshot_ts_us)
        .or_else(|| fills.first().map(|fill| fill.ts_us));
    let Some(anchor_ts_us) = anchor_ts_us else {
        return Ok(SourceIndex {
            anchor_ts_us: None,
            available_anchor_ts_us: None,
            last_ts_us: None,
            event_times_ms: Vec::new(),
            positions: BTreeMap::new(),
        });
    };
    let pre_anchor_prices = fills
        .iter()
        .filter(|fill| fill.ts_us <= anchor_ts_us)
        .map(|fill| ((fill.symbol.clone(), fill.venue_code), fill.price))
        .collect::<BTreeMap<_, _>>();
    let mut positions = BTreeMap::new();
    for position in snapshot
        .map(|snapshot| snapshot.positions.as_slice())
        .unwrap_or_default()
    {
        let key = (position.symbol.clone(), position.venue_code);
        let (last_price, valuation_source) = match pre_anchor_prices.get(&key).copied() {
            Some(price) => (Some(price), Some("last_fill")),
            None => (
                position.reference_price,
                position.reference_price.map(|_| "initial_reference"),
            ),
        };
        positions.insert(
            key,
            PositionSeries {
                venue: crate::model::venue_name(position.venue_code as u8),
                events: vec![PositionEvent {
                    ts_us: anchor_ts_us,
                    quantity: position.quantity,
                    last_price,
                    valuation_source,
                }],
            },
        );
    }
    for fill in fills {
        if snapshot.is_some() && fill.ts_us <= anchor_ts_us {
            continue;
        }
        let series = positions
            .entry((fill.symbol.clone(), fill.venue_code))
            .or_insert_with(|| PositionSeries {
                venue: fill.venue.clone(),
                events: vec![PositionEvent {
                    ts_us: anchor_ts_us,
                    quantity: 0.0,
                    last_price: None,
                    valuation_source: None,
                }],
            });
        let quantity =
            series.events.last().expect("anchor event exists").quantity + fill.signed_quantity;
        series.events.push(PositionEvent {
            ts_us: fill.ts_us,
            quantity,
            last_price: Some(fill.price),
            valuation_source: Some("last_fill"),
        });
    }
    let last_ts_us = positions
        .values()
        .filter_map(|series| series.events.last().map(|event| event.ts_us))
        .max()
        .or(Some(anchor_ts_us));
    let mut event_times_ms = positions
        .values()
        .flat_map(|series| series.events.iter().map(|event| ceil_us_to_ms(event.ts_us)))
        .collect::<Vec<_>>();
    event_times_ms.sort_unstable();
    event_times_ms.dedup();
    Ok(SourceIndex {
        anchor_ts_us: Some(anchor_ts_us),
        available_anchor_ts_us: Some(anchor_ts_us),
        last_ts_us,
        event_times_ms,
        positions,
    })
}

fn build_source_from_daily_checkpoint(
    source: &SourceConfig,
    day_start_us: i64,
    effective_anchor_ts_us: Option<i64>,
    has_original_snapshot: bool,
    positions: Vec<StoredPosition>,
    events: &[crate::model::UniformOrderEvent],
) -> Result<SourceIndex> {
    let anchor_ts_us = effective_anchor_ts_us
        .filter(|anchor| floor_day(*anchor) == day_start_us)
        .unwrap_or(day_start_us);
    let mut state = BTreeMap::new();
    for position in positions {
        state.insert(
            (position.symbol.clone(), position.venue_code),
            PositionSeries {
                venue: position.venue,
                events: vec![PositionEvent {
                    ts_us: anchor_ts_us,
                    quantity: position.quantity,
                    last_price: position.last_price,
                    valuation_source: match position.valuation_source.as_deref() {
                        Some("last_fill") => Some("last_fill"),
                        Some("initial_reference") => Some("initial_reference"),
                        _ => None,
                    },
                }],
            },
        );
    }
    for fill in nav::historical_fills(source, events)? {
        if fill.ts_us < day_start_us || (has_original_snapshot && fill.ts_us <= anchor_ts_us) {
            continue;
        }
        let series = state
            .entry((fill.symbol.clone(), fill.venue_code))
            .or_insert_with(|| PositionSeries {
                venue: fill.venue.clone(),
                events: vec![PositionEvent {
                    ts_us: anchor_ts_us,
                    quantity: 0.0,
                    last_price: None,
                    valuation_source: None,
                }],
            });
        let quantity = series
            .events
            .last()
            .expect("daily checkpoint event exists")
            .quantity
            + fill.signed_quantity;
        series.events.push(PositionEvent {
            ts_us: fill.ts_us,
            quantity,
            last_price: Some(fill.price),
            valuation_source: Some("last_fill"),
        });
    }
    let last_ts_us = state
        .values()
        .filter_map(|series| series.events.last().map(|event| event.ts_us))
        .max()
        .or(Some(anchor_ts_us));
    let mut event_times_ms = state
        .values()
        .flat_map(|series| series.events.iter().map(|event| ceil_us_to_ms(event.ts_us)))
        .collect::<Vec<_>>();
    event_times_ms.sort_unstable();
    event_times_ms.dedup();
    Ok(SourceIndex {
        anchor_ts_us: Some(anchor_ts_us),
        available_anchor_ts_us: effective_anchor_ts_us.or(Some(anchor_ts_us)),
        last_ts_us,
        event_times_ms,
        positions: state,
    })
}
fn as_of(
    series: &PositionSeries,
    ts_us: i64,
) -> (f64, Option<f64>, Option<f64>, Option<f64>, &'static str) {
    let event = &series.events[series
        .events
        .partition_point(|event| event.ts_us <= ts_us)
        .saturating_sub(1)];
    let source = event.valuation_source.unwrap_or("unavailable");
    if event.quantity.abs() < 1e-12 {
        return (0.0, event.last_price, Some(0.0), Some(0.0), source);
    }
    let signed = event.last_price.map(|price| event.quantity * price);
    (
        event.quantity,
        event.last_price,
        signed,
        signed.map(f64::abs),
        source,
    )
}
fn sample_timestamps(start: i64, end: i64, max: usize, events: &[i64]) -> Vec<i64> {
    if max == 1 {
        return vec![end];
    }
    let first_grid = start
        .saturating_div(GRID_MS)
        .saturating_add(1)
        .saturating_mul(GRID_MS);
    let interior = if first_grid >= end {
        0
    } else {
        end.saturating_sub(first_grid)
            .saturating_div(GRID_MS)
            .saturating_add(1)
    };
    if interior.saturating_add(2) <= max as i64 {
        let mut points = Vec::with_capacity(interior as usize + 2);
        points.push(start);
        for index in 0..interior {
            points.push(first_grid.saturating_add(index.saturating_mul(GRID_MS)));
        }
        points.push(end);
        points.extend_from_slice(events);
        points.sort_unstable();
        points.dedup();
        if points.len() <= max {
            return points;
        }
    }
    (0..max)
        .map(|index| interpolate_ms(start, end, index, max - 1))
        .collect()
}

fn ceil_us_to_ms(value: i64) -> i64 {
    value.saturating_add(999).saturating_div(1_000)
}
fn interpolate_ms(start: i64, end: i64, numerator: usize, denominator: usize) -> i64 {
    let span = i128::from(end) - i128::from(start);
    (i128::from(start) + span * numerator as i128 / denominator.max(1) as i128)
        .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}
fn missing_account(ts_ms: i64, source_id: &str) -> AccountPoint {
    AccountPoint {
        ts_ms,
        source_id: source_id.into(),
        equity_usdt: None,
        gross_notional_usdt: None,
        gross_leverage: None,
        availability: "missing_anchor".into(),
    }
}
fn missing_mark_account(ts_ms: i64, source_id: &str) -> AccountPoint {
    AccountPoint {
        ts_ms,
        source_id: source_id.into(),
        equity_usdt: None,
        gross_notional_usdt: None,
        gross_leverage: None,
        availability: "missing_mark".into(),
    }
}
fn missing_symbol(ts_ms: i64, source_id: &str, symbol: &str, venue: &str) -> SymbolPoint {
    SymbolPoint {
        ts_ms,
        source_id: source_id.into(),
        symbol: symbol.into(),
        venue: venue.into(),
        quantity: None,
        signed_notional_usdt: None,
        gross_notional_usdt: None,
        leverage_contribution: None,
        valuation_price: None,
        valuation_source: "unavailable".into(),
        availability: "missing_anchor".into(),
    }
}
fn clean(value: f64) -> f64 {
    if value.abs() < 1e-12 { 0.0 } else { value }
}

fn current_equity_availability(
    equity_usdt: Option<f64>,
    ts_ms: Option<i64>,
    now_ms: i64,
) -> &'static str {
    let Some(equity_usdt) = equity_usdt else {
        return "missing";
    };
    if !equity_usdt.is_finite() {
        return "nonfinite";
    }
    let Some(ts_ms) = ts_ms else {
        return "missing";
    };
    if now_ms.saturating_sub(ts_ms).max(0) > 45_000 {
        return "stale";
    }
    if equity_usdt <= 0.0 {
        return "nonpositive";
    }
    "ok"
}

fn point_equity_availability(availability: &str) -> &'static str {
    match availability {
        "ok" => "ok",
        "stale" => "stale_equity",
        "nonpositive" => "nonpositive_equity",
        "incomplete" => "incomplete",
        "missing" | "nonfinite" => "missing_equity",
        _ => "missing_equity",
    }
}

fn historical_unavailable(availability: &str) -> bool {
    matches!(availability, "missing_anchor" | "missing_mark")
}

fn safe_ratio(numerator: f64, denominator: f64) -> Option<f64> {
    let value = numerator / denominator;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UniformOrderEvent;
    use crate::snapshot::SnapshotPosition;
    use std::path::PathBuf;

    fn source(id: &str) -> SourceConfig {
        SourceConfig {
            id: id.into(),
            account: id.into(),
            alias: None,
            venue: "binance-futures".into(),
            rocksdb_path: PathBuf::from("/tmp/history-test"),
            enabled: true,
            start_ts_us: None,
            poll_interval_secs: None,
            estimated_fee_rate: None,
            maker_fee_rate: None,
            taker_fee_rate: None,
            gateway_prefix: None,
            exec_config_url: None,
            exec_viz_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            legacy_share_unit_usdt: None,
            env_path: None,
        }
    }
    fn fill(ts_us: i64, side_code: i16, price: f64, amount_update: f64) -> UniformOrderEvent {
        UniformOrderEvent {
            record_key: format!("{ts_us:020}"),
            event_ts_us: ts_us,
            recv_ts_us: ts_us,
            symbol: "BTCUSDT".into(),
            create_ts_us: ts_us,
            update_ts_us: ts_us,
            signal_ts_us: 0,
            submit_ts_us: 0,
            local_ts_us: 0,
            market_ts_us: 0,
            client_order_id: ts_us,
            venue_code: 1,
            venue: "binance-futures".into(),
            order_type_code: 3,
            order_type: "MARKET".into(),
            side_code,
            side: String::new(),
            price,
            price_offset: 0.0,
            amount_initial: amount_update,
            amount_update,
            status_code: 0,
            status: String::new(),
            from_key: Vec::new(),
            from_key_text: String::new(),
            bbo_spread: String::new(),
            signal_open: None,
            signal_hedge: None,
            wire_payload: Vec::new(),
        }
    }
    fn snapshot(id: &str, quantity: f64, price: Option<f64>) -> PositionSnapshot {
        PositionSnapshot {
            source_id: id.into(),
            snapshot_ts_us: 1_000_000,
            positions: vec![SnapshotPosition {
                symbol: "BTCUSDT".into(),
                venue_code: 1,
                quantity,
                reference_price: price,
            }],
        }
    }

    #[test]
    fn applies_pre_window_fills_without_future_price_and_preserves_zero_close() {
        let index = build_source(
            &source("a"),
            Some(&snapshot("a", 1.0, None)),
            &[fill(2_000_000, 1, 10.0, 1.0), fill(3_000_000, 2, 12.0, 2.0)],
        )
        .unwrap();
        let series = index.positions.get(&("BTCUSDT".into(), 1)).unwrap();
        assert_eq!(
            as_of(series, 1_500_000),
            (1.0, None, None, None, "unavailable")
        );
        assert_eq!(
            as_of(series, 2_500_000),
            (2.0, Some(10.0), Some(20.0), Some(20.0), "last_fill")
        );
        assert_eq!(
            as_of(series, 3_500_000),
            (0.0, Some(12.0), Some(0.0), Some(0.0), "last_fill")
        );
    }

    #[test]
    fn source_positions_do_not_net_across_accounts() {
        let a = build_source(
            &source("a"),
            Some(&snapshot("a", 1.0, Some(100.0))),
            &[
                fill(2_000_000, 1, 100.0, 1.0),
                fill(2_000_001, 2, 100.0, 1.0),
            ],
        )
        .unwrap();
        let b = build_source(
            &source("b"),
            Some(&snapshot("b", -1.0, Some(200.0))),
            &[
                fill(2_000_000, 1, 200.0, 1.0),
                fill(2_000_001, 2, 200.0, 1.0),
            ],
        )
        .unwrap();
        let index = PositionHistoryIndex {
            sources: BTreeMap::from([("a".into(), a), ("b".into(), b)]),
        };
        let report = index
            .load(
                1_500,
                2_500,
                vec!["a".into(), "b".into()],
                vec![],
                Some(10),
                3_000_000,
            )
            .unwrap();
        let point = report
            .portfolio_points
            .iter()
            .find(|point| point.ts_ms == 2_500)
            .unwrap();
        assert_eq!(point.gross_notional_usdt, Some(300.0));
        assert_eq!(point.equity_usdt, None);
        assert_eq!(point.availability, "missing_equity");
    }

    #[test]
    fn event_candidate_collection_stays_bounded() {
        let index = PositionHistoryIndex {
            sources: BTreeMap::from([(
                "a".into(),
                SourceIndex {
                    anchor_ts_us: Some(1),
                    available_anchor_ts_us: Some(1),
                    last_ts_us: Some(10_000),
                    event_times_ms: (1..=10_000).collect(),
                    positions: BTreeMap::new(),
                },
            )]),
        };
        assert!(
            index
                .event_times_in_range(&["a".into()], 1, 10_000, 100)
                .is_empty()
        );
        assert_eq!(
            index.event_times_in_range(&["a".into()], 1, 3, 100),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn first_fill_is_visible_only_at_its_ceil_millisecond() {
        let source_index =
            build_source(&source("a"), None, &[fill(2_000_900, 1, 10.0, 1.0)]).unwrap();
        let index = PositionHistoryIndex {
            sources: BTreeMap::from([("a".into(), source_index)]),
        };
        let report = index
            .load(2_000, 2_001, vec!["a".into()], vec![], Some(10), 3_000_000)
            .unwrap();
        let before = report
            .account_points
            .iter()
            .find(|point| point.ts_ms == 2_000)
            .unwrap();
        assert_eq!(before.availability, "missing_anchor");
        let after = report
            .symbol_points
            .iter()
            .find(|point| point.ts_ms == 2_001)
            .unwrap();
        assert_eq!(after.quantity, Some(1.0));
        assert_eq!(after.valuation_price, Some(10.0));
    }

    #[test]
    fn pre_anchor_fill_price_overrides_reference_without_replaying_quantity() {
        let source_index = build_source(
            &source("a"),
            Some(&snapshot("a", 1.0, Some(100.0))),
            &[fill(500_000, 1, 90.0, 2.0)],
        )
        .unwrap();
        let series = source_index.positions.get(&("BTCUSDT".into(), 1)).unwrap();
        assert_eq!(
            as_of(series, 1_000_000),
            (1.0, Some(90.0), Some(90.0), Some(90.0), "last_fill")
        );
    }

    fn response_for_current_equity() -> PositionHistoryResponse {
        PositionHistoryResponse {
            generated_at_us: 1_000_000,
            start_ms: 1_000,
            end_ms: 1_000,
            selected_source_ids: vec!["a".into(), "b".into()],
            selected_symbols: vec![],
            available_sources: vec![],
            available_symbols: vec![],
            leverage_basis: "current_account_equity".into(),
            current_equity: CurrentEquityMetadata {
                equity_usdt: None,
                availability: "incomplete".into(),
                accounts: vec![],
            },
            portfolio_points: vec![PortfolioPoint {
                ts_ms: 1_000,
                equity_usdt: None,
                gross_notional_usdt: Some(300.0),
                gross_leverage: None,
                availability: "missing_equity".into(),
                missing_source_ids: vec![],
            }],
            account_points: vec![
                AccountPoint {
                    ts_ms: 1_000,
                    source_id: "a".into(),
                    equity_usdt: None,
                    gross_notional_usdt: Some(100.0),
                    gross_leverage: None,
                    availability: "missing_equity".into(),
                },
                AccountPoint {
                    ts_ms: 1_000,
                    source_id: "b".into(),
                    equity_usdt: None,
                    gross_notional_usdt: Some(200.0),
                    gross_leverage: None,
                    availability: "missing_equity".into(),
                },
            ],
            symbol_points: vec![
                symbol_point("a", "BTCUSDT", 40.0),
                symbol_point("a", "ETHUSDT", 60.0),
                symbol_point("b", "BTCUSDT", 200.0),
            ],
        }
    }

    fn symbol_point(source_id: &str, symbol: &str, gross: f64) -> SymbolPoint {
        SymbolPoint {
            ts_ms: 1_000,
            source_id: source_id.into(),
            symbol: symbol.into(),
            venue: "binance-futures".into(),
            quantity: Some(1.0),
            signed_notional_usdt: Some(gross),
            gross_notional_usdt: Some(gross),
            leverage_contribution: None,
            valuation_price: Some(gross),
            valuation_source: "last_fill".into(),
            availability: "missing_equity".into(),
        }
    }

    #[test]
    fn current_equity_sets_account_portfolio_and_additive_symbol_leverage() {
        let mut response = response_for_current_equity();
        response.apply_current_equity(
            vec![
                CurrentEquityInput {
                    source_id: "a".into(),
                    equity_usdt: Some(50.0),
                    ts_ms: Some(1_000),
                },
                CurrentEquityInput {
                    source_id: "b".into(),
                    equity_usdt: Some(100.0),
                    ts_ms: Some(1_000),
                },
            ],
            1_000,
        );
        assert_eq!(response.current_equity.equity_usdt, Some(150.0));
        assert_eq!(response.current_equity.availability, "ok");
        assert_eq!(response.portfolio_points[0].equity_usdt, Some(150.0));
        assert_eq!(response.portfolio_points[0].gross_leverage, Some(2.0));
        assert_eq!(response.account_points[0].gross_leverage, Some(2.0));
        assert_eq!(response.account_points[1].gross_leverage, Some(2.0));
        let contributions = response
            .symbol_points
            .iter()
            .map(|point| point.leverage_contribution.unwrap())
            .sum::<f64>();
        assert_eq!(contributions, 2.0);
    }

    #[test]
    fn invalid_current_equity_never_partially_denominates_or_erases_history_state() {
        for (a_equity, b_equity, expected, portfolio_status) in [
            (Some(50.0), None, "missing", "incomplete"),
            (Some(50.0), Some(f64::NAN), "nonfinite", "incomplete"),
            (Some(0.0), Some(0.0), "nonpositive", "nonpositive"),
            (Some(-50.0), Some(-100.0), "nonpositive", "nonpositive"),
        ] {
            let mut response = response_for_current_equity();
            response.account_points[0].availability = "missing_anchor".into();
            response.account_points[0].gross_notional_usdt = None;
            response.apply_current_equity(
                vec![
                    CurrentEquityInput {
                        source_id: "a".into(),
                        equity_usdt: a_equity,
                        ts_ms: Some(1_000),
                    },
                    CurrentEquityInput {
                        source_id: "b".into(),
                        equity_usdt: b_equity,
                        ts_ms: Some(1_000),
                    },
                ],
                1_000,
            );
            assert_eq!(response.current_equity.availability, portfolio_status);
            assert_eq!(
                response.current_equity.equity_usdt,
                (portfolio_status == "nonpositive").then_some(if a_equity.unwrap() == 0.0 {
                    0.0
                } else {
                    -150.0
                })
            );
            assert_eq!(response.current_equity.accounts[1].availability, expected);
            assert_eq!(response.portfolio_points[0].gross_leverage, None);
            assert_eq!(response.account_points[0].availability, "missing_anchor");
            assert_eq!(response.account_points[1].gross_leverage, None);
        }

        let mut stale = response_for_current_equity();
        stale.apply_current_equity(
            vec![
                CurrentEquityInput {
                    source_id: "a".into(),
                    equity_usdt: Some(50.0),
                    ts_ms: Some(1_000),
                },
                CurrentEquityInput {
                    source_id: "b".into(),
                    equity_usdt: Some(100.0),
                    ts_ms: Some(1_000),
                },
            ],
            46_001,
        );
        assert_eq!(stale.current_equity.accounts[0].availability, "stale");
        assert_eq!(stale.portfolio_points[0].gross_leverage, None);
        assert_eq!(stale.account_points[0].availability, "stale_equity");

        stale.apply_current_equity(
            vec![
                CurrentEquityInput {
                    source_id: "a".into(),
                    equity_usdt: Some(50.0),
                    ts_ms: Some(46_001),
                },
                CurrentEquityInput {
                    source_id: "b".into(),
                    equity_usdt: Some(100.0),
                    ts_ms: Some(46_001),
                },
            ],
            46_001,
        );
        assert_eq!(stale.account_points[0].availability, "ok");
        assert_eq!(stale.account_points[0].gross_leverage, Some(2.0));
    }

    #[test]
    fn missing_peer_and_overflowing_ratio_do_not_publish_leverage() {
        let mut missing_peer = response_for_current_equity();
        missing_peer.apply_current_equity(
            vec![CurrentEquityInput {
                source_id: "a".into(),
                equity_usdt: Some(50.0),
                ts_ms: Some(1_000),
            }],
            1_000,
        );
        assert_eq!(missing_peer.symbol_points[0].availability, "incomplete");
        assert_eq!(missing_peer.symbol_points[0].leverage_contribution, None);

        let mut overflow = response_for_current_equity();
        overflow.account_points[0].gross_notional_usdt = Some(f64::MAX);
        overflow.apply_current_equity(
            vec![
                CurrentEquityInput {
                    source_id: "a".into(),
                    equity_usdt: Some(f64::MIN_POSITIVE),
                    ts_ms: Some(1_000),
                },
                CurrentEquityInput {
                    source_id: "b".into(),
                    equity_usdt: Some(100.0),
                    ts_ms: Some(1_000),
                },
            ],
            1_000,
        );
        assert_eq!(overflow.account_points[0].gross_leverage, None);
        assert_eq!(overflow.account_points[0].availability, "incomplete");
    }
}
