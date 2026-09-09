use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgRow};

use crate::config::AppConfig;
use crate::nav;

pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const MAX_PAGE_SIZE: usize = 100;
const ZERO_EPSILON: f64 = 1e-12;

#[derive(Clone, Debug, Default, Serialize)]
pub struct AcquisitionCostTotals {
    pub virtual_delta_count: usize,
    pub missing_virtual_delta_count: usize,
    pub comparable_delta_count: usize,
    pub virtual_turnover_usdt: f64,
    pub virtual_fee_usdt: f64,
    pub actual_fill_count: u64,
    pub actual_turnover_usdt: f64,
    pub actual_fee_usdt: f64,
    pub matched_virtual_turnover_usdt: f64,
    pub actual_matched_turnover_usdt: f64,
    pub actual_matched_fee_usdt: f64,
    pub matched_fill_count: u64,
    pub unmatched_fill_count: u64,
    pub unmatched_fill_notional_usdt: f64,
    pub opposite_fill_count: u64,
    pub opposite_fill_notional_usdt: f64,
    pub price_shortfall_usdt: f64,
    pub fee_shortfall_usdt: f64,
    pub after_fee_shortfall_usdt: f64,
    pub price_shortfall_bps: f64,
    pub matched_turnover_coverage: f64,
    pub actual_fill_reference_coverage: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionCostPoint {
    pub ts_us: i64,
    pub virtual_turnover_usdt: f64,
    pub actual_matched_turnover_usdt: f64,
    pub price_shortfall_usdt: f64,
    pub after_fee_shortfall_usdt: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionCostBreakdown {
    pub bucket: String,
    pub fill_count: u64,
    pub reference_turnover_usdt: f64,
    pub actual_turnover_usdt: f64,
    pub actual_fee_usdt: f64,
    pub virtual_fee_usdt: f64,
    pub price_shortfall_usdt: f64,
    pub price_shortfall_bps: f64,
    pub after_fee_shortfall_usdt: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionFillDiagnostic {
    pub source_id: String,
    pub strategy_name: String,
    pub symbol: String,
    pub target_received_at_us: i64,
    pub order_signal_ts_us: i64,
    pub fill_ts_us: i64,
    pub client_order_id: i64,
    pub side: &'static str,
    pub liquidity: &'static str,
    pub actual_qty: f64,
    pub actual_price: f64,
    pub virtual_price: f64,
    pub target_delay_us: i64,
    pub order_delay_us: i64,
    pub reference_turnover_usdt: f64,
    pub price_shortfall_usdt: f64,
    pub price_shortfall_bps: f64,
}

#[derive(Clone, Debug, Default)]
struct BreakdownAccumulator {
    fill_count: u64,
    reference_turnover_usdt: f64,
    actual_turnover_usdt: f64,
    actual_fee_usdt: f64,
    virtual_fee_usdt: f64,
    price_shortfall_usdt: f64,
}

impl BreakdownAccumulator {
    fn add(
        &mut self,
        signed_qty: f64,
        actual_price: f64,
        virtual_price: f64,
        actual_fee: f64,
        virtual_fee_rate: f64,
    ) {
        let reference_turnover = (signed_qty * virtual_price).abs();
        self.fill_count = self.fill_count.saturating_add(1);
        self.reference_turnover_usdt += reference_turnover;
        self.actual_turnover_usdt += (signed_qty * actual_price).abs();
        self.actual_fee_usdt += actual_fee;
        self.virtual_fee_usdt += reference_turnover * virtual_fee_rate;
        self.price_shortfall_usdt += signed_qty * (actual_price - virtual_price);
    }

    fn finish(self, bucket: String) -> AcquisitionCostBreakdown {
        AcquisitionCostBreakdown {
            bucket,
            fill_count: self.fill_count,
            reference_turnover_usdt: self.reference_turnover_usdt,
            actual_turnover_usdt: self.actual_turnover_usdt,
            actual_fee_usdt: self.actual_fee_usdt,
            virtual_fee_usdt: self.virtual_fee_usdt,
            price_shortfall_usdt: self.price_shortfall_usdt,
            price_shortfall_bps: if self.reference_turnover_usdt > 0.0 {
                self.price_shortfall_usdt / self.reference_turnover_usdt * 10_000.0
            } else {
                0.0
            },
            after_fee_shortfall_usdt: self.price_shortfall_usdt + self.actual_fee_usdt
                - self.virtual_fee_usdt,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionCostRow {
    pub source_id: String,
    pub binding_name: String,
    pub strategy_name: String,
    pub symbol: String,
    pub venue: String,
    pub received_at_us: i64,
    pub virtual_execution_ts_us: i64,
    pub delta_qty: f64,
    pub sample_mids: [f64; 5],
    pub virtual_vwap: f64,
    pub virtual_turnover_usdt: f64,
    pub virtual_fee_usdt: f64,
    pub actual_matched_qty: f64,
    pub actual_vwap: Option<f64>,
    pub actual_matched_turnover_usdt: f64,
    pub actual_matched_fee_usdt: f64,
    pub matched_fill_count: u64,
    pub fill_ratio: f64,
    pub price_shortfall_usdt: Option<f64>,
    pub fee_shortfall_usdt: Option<f64>,
    pub after_fee_shortfall_usdt: Option<f64>,
    pub price_shortfall_bps: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionCostReport {
    pub generated_at_us: i64,
    pub price_basis: &'static str,
    pub fee_basis: &'static str,
    pub start_received_at_us: i64,
    pub end_received_at_us: i64,
    pub source_ids: Vec<String>,
    pub strategy_name: Option<String>,
    pub page: usize,
    pub page_size: usize,
    pub page_count: usize,
    pub returned_row_count: usize,
    pub totals: AcquisitionCostTotals,
    pub points: Vec<AcquisitionCostPoint>,
    pub by_symbol: Vec<AcquisitionCostBreakdown>,
    pub by_side: Vec<AcquisitionCostBreakdown>,
    pub by_liquidity: Vec<AcquisitionCostBreakdown>,
    pub by_target_delay: Vec<AcquisitionCostBreakdown>,
    pub by_order_delay: Vec<AcquisitionCostBreakdown>,
    pub by_symbol_liquidity: Vec<AcquisitionCostBreakdown>,
    pub by_symbol_target_delay: Vec<AcquisitionCostBreakdown>,
    pub worst_fills: Vec<AcquisitionFillDiagnostic>,
    pub rows: Vec<AcquisitionCostRow>,
}

#[derive(Clone, Debug)]
struct VirtualFill {
    source_id: String,
    binding_name: String,
    strategy_name: String,
    symbol: String,
    venue: String,
    received_at_us: i64,
    execution_ts_us: i64,
    delta_qty: f64,
    sample_mids: [f64; 5],
    virtual_vwap: f64,
    virtual_fee_usdt: f64,
    virtual_fee_rate: f64,
    actual_matched_qty: f64,
    actual_signed_notional_usdt: f64,
    actual_fee_usdt: f64,
    matched_fill_count: u64,
}

fn decode_virtual_fill(row: PgRow) -> Result<VirtualFill> {
    let sample_value: serde_json::Value = row.try_get("sample_mids")?;
    let samples = serde_json::from_value::<Vec<f64>>(sample_value)
        .context("decode theoretical sample_mids")?;
    let sample_mids: [f64; 5] = samples.try_into().map_err(|values: Vec<f64>| {
        anyhow::anyhow!("expected 5 sample mids, got {}", values.len())
    })?;
    if sample_mids
        .iter()
        .any(|price| !price.is_finite() || *price <= 0.0)
    {
        bail!("theoretical sample mids contain an invalid price");
    }
    Ok(VirtualFill {
        source_id: row.try_get("source_id")?,
        binding_name: row.try_get("binding_name")?,
        strategy_name: row.try_get("position_strategy_name")?,
        symbol: row.try_get("symbol")?,
        venue: row.try_get("venue")?,
        received_at_us: row.try_get("received_at_us")?,
        execution_ts_us: row.try_get("execution_ts_us")?,
        delta_qty: row.try_get("executed_quantity")?,
        sample_mids,
        virtual_vwap: row.try_get("twap_price")?,
        virtual_fee_usdt: row.try_get("fee_quote")?,
        virtual_fee_rate: row.try_get("fee_rate")?,
        actual_matched_qty: 0.0,
        actual_signed_notional_usdt: 0.0,
        actual_fee_usdt: 0.0,
        matched_fill_count: 0,
    })
}

fn page_bounds(count: usize, page: usize, page_size: usize) -> (usize, usize) {
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    let end = count.saturating_sub(offset);
    (end.saturating_sub(page_size), end)
}

fn delay_bucket(delay_us: i64) -> &'static str {
    match delay_us.max(0) {
        0..5_000_000 => "00_00-05s",
        5_000_000..10_000_000 => "01_05-10s",
        10_000_000..30_000_000 => "02_10-30s",
        30_000_000..60_000_000 => "03_30-60s",
        60_000_000..300_000_000 => "04_60-300s",
        _ => "05_after-300s",
    }
}

fn finish_breakdowns(
    values: BTreeMap<String, BreakdownAccumulator>,
    sort_by_shortfall: bool,
) -> Vec<AcquisitionCostBreakdown> {
    let mut output = values
        .into_iter()
        .map(|(bucket, value)| value.finish(bucket))
        .collect::<Vec<_>>();
    if sort_by_shortfall {
        output.sort_by(|left, right| {
            right
                .price_shortfall_usdt
                .partial_cmp(&left.price_shortfall_usdt)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    output
}

pub async fn report_acquisition_cost(
    pool: &PgPool,
    config: &AppConfig,
    histories: &nav::NavSourceHistories,
    start_received_at_us: i64,
    end_received_at_us: i64,
    generated_at_us: i64,
    source_ids: &[String],
    strategy_name: Option<&str>,
    page: usize,
    page_size: usize,
) -> Result<AcquisitionCostReport> {
    if start_received_at_us < 0 || end_received_at_us < start_received_at_us {
        bail!("invalid acquisition-cost timestamp range");
    }
    if page == 0 || page_size == 0 || page_size > MAX_PAGE_SIZE {
        bail!("invalid acquisition-cost pagination");
    }
    let rows = sqlx::query(
        r#"
        SELECT source_id, binding_name, position_strategy_name, symbol, venue,
               received_at_us, execution_ts_us, executed_quantity,
               twap_price, sample_mids, fee_rate, fee_quote
        FROM cta_theoretical_nav_events
        WHERE received_at_us >= $1 AND received_at_us <= $2
          AND (cardinality($3::text[]) = 0 OR source_id = ANY($3))
          AND ($4::text IS NULL OR position_strategy_name = $4)
        ORDER BY received_at_us, source_id, binding_name, symbol
        "#,
    )
    .bind(start_received_at_us)
    .bind(end_received_at_us)
    .bind(source_ids.to_vec())
    .bind(strategy_name)
    .fetch_all(pool)
    .await
    .context("load theoretical acquisition-cost events")?;
    let mut virtual_fills = rows
        .into_iter()
        .map(decode_virtual_fill)
        .collect::<Result<Vec<_>>>()?;
    let missing_virtual_delta_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM cta_theoretical_nav_skips
        WHERE received_at_us >= $1 AND received_at_us <= $2
          AND reason = 'missing_five_slice_mid'
          AND (cardinality($3::text[]) = 0 OR source_id = ANY($3))
          AND ($4::text IS NULL OR position_strategy_name = $4)
        "#,
    )
    .bind(start_received_at_us)
    .bind(end_received_at_us)
    .bind(source_ids.to_vec())
    .bind(strategy_name)
    .fetch_one(pool)
    .await
    .context("count missing theoretical acquisition-cost events")?;

    let mut by_key = BTreeMap::<(String, String, String), Vec<usize>>::new();
    for (index, fill) in virtual_fills.iter().enumerate() {
        by_key
            .entry((
                fill.source_id.clone(),
                fill.strategy_name.clone(),
                fill.symbol.clone(),
            ))
            .or_default()
            .push(index);
    }
    let mut totals = AcquisitionCostTotals {
        virtual_delta_count: virtual_fills.len(),
        missing_virtual_delta_count: usize::try_from(missing_virtual_delta_count.max(0))?,
        ..AcquisitionCostTotals::default()
    };
    let mut by_symbol = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_side = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_liquidity = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_target_delay = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_order_delay = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_symbol_liquidity = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_symbol_target_delay = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut fill_diagnostics = Vec::new();
    for fill in &virtual_fills {
        totals.virtual_turnover_usdt += (fill.delta_qty * fill.virtual_vwap).abs();
        totals.virtual_fee_usdt += fill.virtual_fee_usdt;
    }

    for (source_id, history) in histories {
        if !source_ids.is_empty() && !source_ids.contains(source_id) {
            continue;
        }
        let source = config
            .sources
            .iter()
            .find(|source| source.id == *source_id)
            .with_context(|| format!("missing acquisition-cost source {source_id}"))?;
        for event in history.events().iter().filter(|event| {
            event.update_ts_us >= start_received_at_us
                && event.update_ts_us <= end_received_at_us
                && event.amount_update > 0.0
                && event.price.is_finite()
                && event.price > 0.0
        }) {
            let signed_qty = match event.side_code {
                1 => event.amount_update,
                2 => -event.amount_update,
                _ => continue,
            };
            let event_strategy = nav::strategy_from_from_key(&event.from_key_text);
            if strategy_name.is_some_and(|selected| selected != event_strategy) {
                continue;
            }
            let actual_fee = history.estimated_fee_quote(source, event)?;
            totals.actual_fill_count = totals.actual_fill_count.saturating_add(1);
            totals.actual_turnover_usdt += (signed_qty * event.price).abs();
            totals.actual_fee_usdt += actual_fee;
            let key = (source_id.clone(), event_strategy, event.symbol.clone());
            let Some(indices) = by_key.get(&key) else {
                totals.unmatched_fill_count = totals.unmatched_fill_count.saturating_add(1);
                totals.unmatched_fill_notional_usdt += (signed_qty * event.price).abs();
                continue;
            };
            let signal_ts_us = if event.signal_ts_us > 0 {
                event.signal_ts_us
            } else {
                event.update_ts_us
            };
            let position = indices
                .partition_point(|index| virtual_fills[*index].received_at_us <= signal_ts_us);
            let Some(index) = position.checked_sub(1).map(|position| indices[position]) else {
                totals.unmatched_fill_count = totals.unmatched_fill_count.saturating_add(1);
                totals.unmatched_fill_notional_usdt += (signed_qty * event.price).abs();
                continue;
            };
            let fill = &mut virtual_fills[index];
            if signed_qty * fill.delta_qty <= 0.0 {
                totals.opposite_fill_count = totals.opposite_fill_count.saturating_add(1);
                totals.opposite_fill_notional_usdt += (signed_qty * event.price).abs();
                continue;
            }
            fill.actual_matched_qty += signed_qty;
            fill.actual_signed_notional_usdt += signed_qty * event.price;
            fill.actual_fee_usdt += actual_fee;
            fill.matched_fill_count = fill.matched_fill_count.saturating_add(1);
            let side = if signed_qty > 0.0 { "buy" } else { "sell" };
            let liquidity = history.liquidity_role_name(event);
            let target_delay = delay_bucket(event.update_ts_us - fill.received_at_us);
            let order_delay = delay_bucket(event.update_ts_us - signal_ts_us);
            let reference_turnover = (signed_qty * fill.virtual_vwap).abs();
            let price_shortfall = signed_qty * (event.price - fill.virtual_vwap);
            fill_diagnostics.push(AcquisitionFillDiagnostic {
                source_id: source_id.clone(),
                strategy_name: nav::strategy_from_from_key(&event.from_key_text),
                symbol: event.symbol.clone(),
                target_received_at_us: fill.received_at_us,
                order_signal_ts_us: signal_ts_us,
                fill_ts_us: event.update_ts_us,
                client_order_id: event.client_order_id,
                side,
                liquidity,
                actual_qty: signed_qty,
                actual_price: event.price,
                virtual_price: fill.virtual_vwap,
                target_delay_us: event.update_ts_us - fill.received_at_us,
                order_delay_us: event.update_ts_us - signal_ts_us,
                reference_turnover_usdt: reference_turnover,
                price_shortfall_usdt: price_shortfall,
                price_shortfall_bps: if reference_turnover > 0.0 {
                    price_shortfall / reference_turnover * 10_000.0
                } else {
                    0.0
                },
            });
            for (values, bucket) in [
                (&mut by_symbol, event.symbol.as_str()),
                (&mut by_side, side),
                (&mut by_liquidity, liquidity),
                (&mut by_target_delay, target_delay),
                (&mut by_order_delay, order_delay),
            ] {
                values.entry(bucket.to_string()).or_default().add(
                    signed_qty,
                    event.price,
                    fill.virtual_vwap,
                    actual_fee,
                    fill.virtual_fee_rate,
                );
            }
            for (values, bucket) in [
                (
                    &mut by_symbol_liquidity,
                    format!("{}|{}", event.symbol, liquidity),
                ),
                (
                    &mut by_symbol_target_delay,
                    format!("{}|{}", event.symbol, target_delay),
                ),
            ] {
                values.entry(bucket).or_default().add(
                    signed_qty,
                    event.price,
                    fill.virtual_vwap,
                    actual_fee,
                    fill.virtual_fee_rate,
                );
            }
        }
    }

    let mut output_rows = Vec::with_capacity(virtual_fills.len());
    let mut points: Vec<AcquisitionCostPoint> = Vec::with_capacity(virtual_fills.len());
    let mut cumulative_virtual_turnover = 0.0;
    let mut cumulative_actual_turnover = 0.0;
    let mut cumulative_price_shortfall = 0.0;
    let mut cumulative_after_fee_shortfall = 0.0;
    for fill in virtual_fills {
        let virtual_turnover = (fill.delta_qty * fill.virtual_vwap).abs();
        let matched_turnover = (fill.actual_matched_qty * fill.virtual_vwap).abs();
        let actual_turnover = fill.actual_signed_notional_usdt.abs();
        let actual_vwap = (fill.actual_matched_qty.abs() > ZERO_EPSILON)
            .then(|| fill.actual_signed_notional_usdt / fill.actual_matched_qty);
        let virtual_matched_fee = matched_turnover * fill.virtual_fee_rate;
        let price_shortfall = actual_vwap
            .map(|actual_vwap| fill.actual_matched_qty * (actual_vwap - fill.virtual_vwap));
        let fee_shortfall = price_shortfall.map(|_| fill.actual_fee_usdt - virtual_matched_fee);
        let after_fee_shortfall = price_shortfall
            .zip(fee_shortfall)
            .map(|(price, fee)| price + fee);
        let price_shortfall_bps = price_shortfall.and_then(|shortfall| {
            (matched_turnover > 0.0).then_some(shortfall / matched_turnover * 10_000.0)
        });
        if price_shortfall.is_some() {
            totals.comparable_delta_count += 1;
            totals.matched_virtual_turnover_usdt += matched_turnover;
            totals.actual_matched_turnover_usdt += actual_turnover;
            totals.actual_matched_fee_usdt += fill.actual_fee_usdt;
            totals.matched_fill_count = totals
                .matched_fill_count
                .saturating_add(fill.matched_fill_count);
            totals.price_shortfall_usdt += price_shortfall.unwrap_or_default();
            totals.fee_shortfall_usdt += fee_shortfall.unwrap_or_default();
            totals.after_fee_shortfall_usdt += after_fee_shortfall.unwrap_or_default();
        }
        cumulative_virtual_turnover += virtual_turnover;
        cumulative_actual_turnover += actual_turnover;
        cumulative_price_shortfall += price_shortfall.unwrap_or_default();
        cumulative_after_fee_shortfall += after_fee_shortfall.unwrap_or_default();
        let point = AcquisitionCostPoint {
            ts_us: fill.received_at_us,
            virtual_turnover_usdt: cumulative_virtual_turnover,
            actual_matched_turnover_usdt: cumulative_actual_turnover,
            price_shortfall_usdt: cumulative_price_shortfall,
            after_fee_shortfall_usdt: cumulative_after_fee_shortfall,
        };
        if let Some(last) = points.last_mut()
            && last.ts_us == point.ts_us
        {
            *last = point;
        } else {
            points.push(point);
        }
        output_rows.push(AcquisitionCostRow {
            source_id: fill.source_id,
            binding_name: fill.binding_name,
            strategy_name: fill.strategy_name,
            symbol: fill.symbol,
            venue: fill.venue,
            received_at_us: fill.received_at_us,
            virtual_execution_ts_us: fill.execution_ts_us,
            delta_qty: fill.delta_qty,
            sample_mids: fill.sample_mids,
            virtual_vwap: fill.virtual_vwap,
            virtual_turnover_usdt: virtual_turnover,
            virtual_fee_usdt: fill.virtual_fee_usdt,
            actual_matched_qty: fill.actual_matched_qty,
            actual_vwap,
            actual_matched_turnover_usdt: actual_turnover,
            actual_matched_fee_usdt: fill.actual_fee_usdt,
            matched_fill_count: fill.matched_fill_count,
            fill_ratio: if fill.delta_qty.abs() > 0.0 {
                fill.actual_matched_qty.abs() / fill.delta_qty.abs()
            } else {
                0.0
            },
            price_shortfall_usdt: price_shortfall,
            fee_shortfall_usdt: fee_shortfall,
            after_fee_shortfall_usdt: after_fee_shortfall,
            price_shortfall_bps,
        });
    }
    totals.price_shortfall_bps = if totals.matched_virtual_turnover_usdt > 0.0 {
        totals.price_shortfall_usdt / totals.matched_virtual_turnover_usdt * 10_000.0
    } else {
        0.0
    };
    totals.matched_turnover_coverage = if totals.virtual_turnover_usdt > 0.0 {
        totals.matched_virtual_turnover_usdt / totals.virtual_turnover_usdt
    } else {
        0.0
    };
    totals.actual_fill_reference_coverage = if totals.actual_turnover_usdt > 0.0 {
        totals.actual_matched_turnover_usdt / totals.actual_turnover_usdt
    } else {
        0.0
    };
    let page_count = output_rows.len().div_ceil(page_size);
    let (start, end) = page_bounds(output_rows.len(), page, page_size);
    let rows = output_rows[start..end].to_vec();
    fill_diagnostics.sort_by(|left, right| {
        right
            .price_shortfall_usdt
            .partial_cmp(&left.price_shortfall_usdt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fill_diagnostics.truncate(100);
    Ok(AcquisitionCostReport {
        generated_at_us,
        price_basis: "delta_split_into_five_equal_qty_5s_mid_samples_60s_apart",
        fee_basis: "actual_maker_taker_vs_virtual_frozen_blended_rate",
        start_received_at_us,
        end_received_at_us,
        source_ids: source_ids.to_vec(),
        strategy_name: strategy_name.map(str::to_string),
        page,
        page_size,
        page_count,
        returned_row_count: rows.len(),
        totals,
        points,
        by_symbol: finish_breakdowns(by_symbol, true),
        by_side: finish_breakdowns(by_side, false),
        by_liquidity: finish_breakdowns(by_liquidity, false),
        by_target_delay: finish_breakdowns(by_target_delay, false),
        by_order_delay: finish_breakdowns(by_order_delay, false),
        by_symbol_liquidity: finish_breakdowns(by_symbol_liquidity, true),
        by_symbol_target_delay: finish_breakdowns(by_symbol_target_delay, true),
        worst_fills: fill_diagnostics,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquisition_shortfall_is_side_aware() {
        for (quantity, actual, virtual_price, expected) in
            [(2.0, 101.0, 100.0, 2.0), (-2.0, 99.0, 100.0, 2.0)]
        {
            let shortfall = quantity * (actual - virtual_price);
            assert_eq!(shortfall, expected);
        }
    }

    #[test]
    fn acquisition_pages_are_latest_first() {
        assert_eq!(page_bounds(55, 1, 25), (30, 55));
        assert_eq!(page_bounds(55, 2, 25), (5, 30));
        assert_eq!(page_bounds(55, 3, 25), (0, 5));
    }

    #[test]
    fn acquisition_breakdown_uses_the_same_quantity_for_both_prices() {
        let mut value = BreakdownAccumulator::default();
        value.add(2.0, 101.0, 100.0, 0.02, 0.0002);
        value.add(-3.0, 99.0, 100.0, 0.03, 0.0002);
        let row = value.finish("all".to_string());
        assert_eq!(row.reference_turnover_usdt, 500.0);
        assert_eq!(row.actual_turnover_usdt, 499.0);
        assert_eq!(row.price_shortfall_usdt, 5.0);
        assert_eq!(row.price_shortfall_bps, 100.0);
        assert!((row.virtual_fee_usdt - 0.1).abs() <= f64::EPSILON);
    }

    #[test]
    fn acquisition_delay_buckets_have_stable_boundaries() {
        assert_eq!(delay_bucket(4_999_999), "00_00-05s");
        assert_eq!(delay_bucket(5_000_000), "01_05-10s");
        assert_eq!(delay_bucket(10_000_000), "02_10-30s");
        assert_eq!(delay_bucket(300_000_000), "05_after-300s");
    }
}
