use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, TheoreticalFeeRates};
use crate::nav;

pub const DEFAULT_PAGE_SIZE: usize = 25;
pub const MAX_PAGE_SIZE: usize = 100;
const ZERO_EPSILON: f64 = 1e-12;
const MAX_VIRTUAL_REFERENCE_AGE_US: i64 = 300_000_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionRowSort {
    #[default]
    Latest,
    PriceShortfall,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AcquisitionCostTotals {
    pub virtual_delta_count: usize,
    pub missing_virtual_delta_count: usize,
    pub pending_virtual_delta_count: usize,
    pub legacy_fee_delta_count: usize,
    pub zero_volume_fallback_sample_count: usize,
    pub zero_volume_skipped_sample_count: usize,
    pub zero_volume_fallback_delta_count: usize,
    pub zero_volume_skipped_delta_count: usize,
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
    pub stale_reference_fill_count: u64,
    pub stale_reference_fill_notional_usdt: f64,
    pub price_shortfall_usdt: f64,
    pub first_minute_shortfall_usdt: f64,
    pub five_sample_drift_usdt: f64,
    pub fee_shortfall_usdt: f64,
    pub after_fee_shortfall_usdt: f64,
    pub price_shortfall_bps: f64,
    pub after_fee_shortfall_bps: f64,
    pub first_minute_shortfall_bps: f64,
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
    pub first_minute_shortfall_usdt: f64,
    pub five_sample_drift_usdt: f64,
    pub price_shortfall_bps: f64,
    pub after_fee_shortfall_bps: f64,
    pub first_minute_shortfall_bps: f64,
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
    pub target_signal: i32,
    pub execution_mode: &'static str,
    pub actual_qty: f64,
    pub actual_price: f64,
    pub virtual_price: f64,
    pub target_delay_us: i64,
    pub order_delay_us: i64,
    pub reference_turnover_usdt: f64,
    pub price_shortfall_usdt: f64,
    pub first_minute_shortfall_usdt: f64,
    pub five_sample_drift_usdt: f64,
    pub price_shortfall_bps: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionExecutionFill {
    pub fill_ts_us: i64,
    pub actual_price: f64,
    pub actual_qty: f64,
    pub liquidity: &'static str,
    pub target_delay_us: i64,
    pub order_delay_us: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AcquisitionExecution {
    pub first_fill_ts_us: Option<i64>,
    pub last_fill_ts_us: Option<i64>,
    pub qty_weighted_target_delay_us: Option<f64>,
    pub qty_weighted_order_delay_us: Option<f64>,
    pub after_window_qty_ratio: Option<f64>,
    pub fills_sampled: bool,
    pub fills: Vec<AcquisitionExecutionFill>,
}

// Summaries use every matched fill; only the plot is bounded. Preserve temporal
// endpoints and price extrema, then spread the remaining samples across time.
fn execution_summary(
    fills: &[&AcquisitionFillDiagnostic],
    window_end_us: i64,
) -> AcquisitionExecution {
    const MAX_PLOT_FILLS: usize = 200;
    if fills.is_empty() {
        return AcquisitionExecution::default();
    }
    let mut ordered = fills.to_vec();
    ordered.sort_by_key(|fill| fill.fill_ts_us);
    let qty: f64 = ordered.iter().map(|fill| fill.actual_qty.abs()).sum();
    let weighted_delay = |order: bool| {
        (qty > 0.0).then(|| {
            ordered
                .iter()
                .map(|fill| {
                    let delay = if order {
                        fill.order_delay_us
                    } else {
                        fill.target_delay_us
                    };
                    fill.actual_qty.abs() * delay as f64
                })
                .sum::<f64>()
                / qty
        })
    };
    let mut indices = std::collections::BTreeSet::new();
    if ordered.len() <= MAX_PLOT_FILLS {
        indices.extend(0..ordered.len());
    } else {
        for i in 0..MAX_PLOT_FILLS - 2 {
            indices.insert(i * (ordered.len() - 1) / (MAX_PLOT_FILLS - 3));
        }
        if let Some((index, _)) = ordered
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.actual_price.total_cmp(&b.1.actual_price))
        {
            indices.insert(index);
        }
        if let Some((index, _)) = ordered
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.actual_price.total_cmp(&b.1.actual_price))
        {
            indices.insert(index);
        }
    }
    AcquisitionExecution {
        first_fill_ts_us: ordered.first().map(|fill| fill.fill_ts_us),
        last_fill_ts_us: ordered.last().map(|fill| fill.fill_ts_us),
        qty_weighted_target_delay_us: weighted_delay(false),
        qty_weighted_order_delay_us: weighted_delay(true),
        after_window_qty_ratio: (qty > 0.0).then(|| {
            ordered
                .iter()
                .filter(|fill| fill.fill_ts_us >= window_end_us)
                .map(|fill| fill.actual_qty.abs())
                .sum::<f64>()
                / qty
        }),
        fills_sampled: ordered.len() > indices.len(),
        fills: indices
            .into_iter()
            .map(|index| {
                let fill = ordered[index];
                AcquisitionExecutionFill {
                    fill_ts_us: fill.fill_ts_us,
                    actual_price: fill.actual_price,
                    actual_qty: fill.actual_qty,
                    liquidity: fill.liquidity,
                    target_delay_us: fill.target_delay_us,
                    order_delay_us: fill.order_delay_us,
                }
            })
            .collect(),
    }
}

#[derive(Clone, Debug, Default)]
struct BreakdownAccumulator {
    fill_count: u64,
    reference_turnover_usdt: f64,
    actual_turnover_usdt: f64,
    actual_fee_usdt: f64,
    virtual_fee_usdt: f64,
    price_shortfall_usdt: f64,
    first_minute_shortfall_usdt: f64,
    five_sample_drift_usdt: f64,
}

impl BreakdownAccumulator {
    fn add(
        &mut self,
        signed_qty: f64,
        actual_price: f64,
        virtual_price: f64,
        first_minute: f64,
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
        self.first_minute_shortfall_usdt += signed_qty * (actual_price - first_minute);
        self.five_sample_drift_usdt += signed_qty * (first_minute - virtual_price);
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
            first_minute_shortfall_usdt: self.first_minute_shortfall_usdt,
            five_sample_drift_usdt: self.five_sample_drift_usdt,
            price_shortfall_bps: if self.reference_turnover_usdt > 0.0 {
                self.price_shortfall_usdt / self.reference_turnover_usdt * 10_000.0
            } else {
                0.0
            },
            after_fee_shortfall_bps: if self.reference_turnover_usdt > 0.0 {
                (self.price_shortfall_usdt + self.actual_fee_usdt - self.virtual_fee_usdt)
                    / self.reference_turnover_usdt
                    * 10_000.0
            } else {
                0.0
            },
            first_minute_shortfall_bps: if self.reference_turnover_usdt > 0.0 {
                self.first_minute_shortfall_usdt / self.reference_turnover_usdt * 10_000.0
            } else {
                0.0
            },
            after_fee_shortfall_usdt: self.price_shortfall_usdt + self.actual_fee_usdt
                - self.virtual_fee_usdt,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExecutionReferencePrice {
    pub execution_model: crate::theoretical_execution::ExecutionModel,
    pub price: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionCostRow {
    pub execution_model: crate::theoretical_execution::ExecutionModel,
    pub execution_window_secs: u64,
    pub reference_prices: Vec<ExecutionReferencePrice>,
    pub source_id: String,
    pub binding_name: String,
    pub strategy_name: String,
    pub symbol: String,
    pub venue: String,
    pub received_at_us: i64,
    pub virtual_execution_ts_us: i64,
    pub delta_qty: f64,
    pub sample_prices: Vec<Option<f64>>,
    pub sample_quantity_weights: Vec<f64>,
    pub zero_volume_fallback_sample_count: usize,
    pub zero_volume_skipped_sample_count: usize,
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
    pub after_fee_shortfall_bps: Option<f64>,
    pub execution: AcquisitionExecution,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcquisitionCostReport {
    pub execution_model: Option<crate::theoretical_execution::ExecutionModel>,
    pub execution_window_secs: u64,
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
    /// Only the detail rows are filtered; totals/breakdowns retain the query scope.
    pub row_symbol: Option<String>,
    pub row_strategy: Option<String>,
    pub row_sort: AcquisitionRowSort,
    pub filtered_row_count: usize,
    pub totals: AcquisitionCostTotals,
    pub points: Vec<AcquisitionCostPoint>,
    pub warnings: Vec<String>,
    pub by_strategy: Vec<AcquisitionCostBreakdown>,
    pub by_symbol: Vec<AcquisitionCostBreakdown>,
    pub by_side: Vec<AcquisitionCostBreakdown>,
    pub by_liquidity: Vec<AcquisitionCostBreakdown>,
    pub by_target_delay: Vec<AcquisitionCostBreakdown>,
    pub by_order_delay: Vec<AcquisitionCostBreakdown>,
    pub by_symbol_liquidity: Vec<AcquisitionCostBreakdown>,
    pub by_symbol_target_delay: Vec<AcquisitionCostBreakdown>,
    pub by_target_signal: Vec<AcquisitionCostBreakdown>,
    pub by_execution_mode: Vec<AcquisitionCostBreakdown>,
    pub worst_fills: Vec<AcquisitionFillDiagnostic>,
    pub rows: Vec<AcquisitionCostRow>,
}

#[derive(Clone, Debug)]
struct VirtualFill {
    reference_prices: Vec<ExecutionReferencePrice>,
    execution_model: crate::theoretical_execution::ExecutionModel,
    source_id: String,
    binding_name: String,
    strategy_name: String,
    symbol: String,
    venue: String,
    received_at_us: i64,
    execution_ts_us: i64,
    delta_qty: f64,
    sample_prices: Vec<Option<f64>>,
    sample_quantity_weights: Vec<f64>,
    first_price: f64,
    zero_volume_fallback_sample_count: usize,
    zero_volume_skipped_sample_count: usize,
    virtual_vwap: f64,
    virtual_fee_usdt: f64,
    virtual_fee_rate: f64,
    target_signal: i32,
    actual_matched_qty: f64,
    actual_signed_notional_usdt: f64,
    actual_fee_usdt: f64,
    matched_fill_count: u64,
}

fn page_bounds(count: usize, page: usize, page_size: usize) -> (usize, usize) {
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    let end = count.saturating_sub(offset);
    (end.saturating_sub(page_size), end)
}

fn select_row_indices(
    rows: &[AcquisitionCostRow],
    symbol: Option<&str>,
    strategy: Option<&str>,
    sort: AcquisitionRowSort,
) -> Vec<usize> {
    let mut indices: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| symbol.is_none_or(|symbol| row.symbol == symbol))
        .filter(|(_, row)| strategy.is_none_or(|strategy| row.strategy_name == strategy))
        .map(|(index, _)| index)
        .collect();
    if sort == AcquisitionRowSort::PriceShortfall {
        // Ascending internally, as with chronological rows: page 1 takes the end.
        // Missing comparisons precede even negative shortfalls; they are not zero.
        indices.sort_by(|&left, &right| {
            rows[left]
                .price_shortfall_usdt
                .partial_cmp(&rows[right].price_shortfall_usdt)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.cmp(&right))
        });
    }
    indices
}

fn detail_page(
    output_rows: &[AcquisitionCostRow],
    output_indices: &[usize],
    fill_diagnostics: &[(usize, AcquisitionFillDiagnostic)],
    row_symbol: Option<&str>,
    row_strategy: Option<&str>,
    row_sort: AcquisitionRowSort,
    page: usize,
    page_size: usize,
) -> (Vec<AcquisitionCostRow>, usize) {
    let selected_indices = select_row_indices(output_rows, row_symbol, row_strategy, row_sort);
    let filtered_row_count = selected_indices.len();
    let (start, end) = page_bounds(filtered_row_count, page, page_size);
    let selected_page = &selected_indices[start..end];
    let mut rows: Vec<_> = selected_page
        .iter()
        .map(|&index| output_rows[index].clone())
        .collect();
    let page_indices: Vec<_> = selected_page
        .iter()
        .map(|&index| output_indices[index])
        .collect();
    let mut page_fills: BTreeMap<usize, Vec<&AcquisitionFillDiagnostic>> = page_indices
        .iter()
        .map(|index| (*index, Vec::new()))
        .collect();
    for (index, diagnostic) in fill_diagnostics {
        if let Some(fills) = page_fills.get_mut(index) {
            fills.push(diagnostic);
        }
    }
    for (row, index) in rows.iter_mut().zip(&page_indices) {
        row.execution = execution_summary(&page_fills[index], row.virtual_execution_ts_us);
    }
    (rows, filtered_row_count)
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
    theoretical_fees: BTreeMap<String, TheoreticalFeeRates>,
    config: &AppConfig,
    targets: &std::sync::Arc<crate::theoretical_nav::TheoreticalTargetCache>,
    klines: &crate::kline::KlineStore,
    histories: &nav::NavSourceHistories,
    start_received_at_us: i64,
    end_received_at_us: i64,
    generated_at_us: i64,
    source_ids: &[String],
    strategy_name: Option<&str>,
    page: usize,
    page_size: usize,
    row_symbol: Option<&str>,
    row_strategy: Option<&str>,
    row_sort: AcquisitionRowSort,
    reference_model: crate::theoretical_execution::ExecutionModel,
) -> Result<AcquisitionCostReport> {
    if start_received_at_us < 0 || end_received_at_us < start_received_at_us {
        bail!("invalid acquisition-cost timestamp range");
    }
    if page == 0 || page_size == 0 || page_size > MAX_PAGE_SIZE {
        bail!("invalid acquisition-cost pagination");
    }
    let (deltas, mut warnings, execution_models) = crate::theoretical_nav::prepare_acquisition(
        theoretical_fees,
        targets,
        klines,
        start_received_at_us,
        end_received_at_us,
        source_ids,
        strategy_name,
        reference_model,
    )
    .await?;
    let analysis_now_us = crate::kline::now_us();
    let mut virtual_fills = Vec::new();
    let mut unavailable = BTreeMap::<(String, String, String), Vec<i64>>::new();
    let mut missing_virtual_delta_count = 0;
    let mut pending_virtual_delta_count = 0;
    let mut legacy_fee_delta_count = 0;
    let mut zero_volume_fallback_sample_count = 0;
    let mut zero_volume_fallback_delta_count = 0;
    let mut zero_volume_skipped_sample_count = 0;
    let mut zero_volume_skipped_delta_count = 0;
    for delta in deltas {
        let execution_ts_us = delta.execution_ts_us();
        let pricing_end_ts_us = delta.pricing_end_ts_us();
        let samples = delta.prices(klines)?;
        if samples
            .as_ref()
            .is_some_and(|samples| samples.average().is_none())
        {
            let warning = delta.execution_model.missing_volume_reason(&delta.symbol);
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        let samples = samples.filter(|samples| samples.average().is_some());
        let Some(priced) = samples else {
            unavailable
                .entry((delta.source_id, delta.strategy_name, delta.symbol))
                .or_default()
                .push(delta.received_at_us);
            if delta.received_at_us >= start_received_at_us {
                if pricing_end_ts_us > analysis_now_us {
                    pending_virtual_delta_count += 1;
                } else {
                    missing_virtual_delta_count += 1;
                }
            }
            continue;
        };
        let sample_prices = priced.prices.clone();
        if delta.received_at_us >= start_received_at_us {
            zero_volume_fallback_sample_count += priced.zero_volume_fallback_sample_count;
            zero_volume_skipped_sample_count += priced.zero_volume_skipped_sample_count;
            zero_volume_skipped_delta_count +=
                usize::from(priced.zero_volume_skipped_sample_count > 0);
            zero_volume_fallback_delta_count +=
                usize::from(priced.zero_volume_fallback_sample_count > 0);
        }
        if delta.legacy_fee {
            legacy_fee_delta_count += 1;
        }
        let virtual_vwap = priced
            .average()
            .context("virtual schedule has no priced minutes")?;
        let reference_models =
            if delta.execution_model == crate::theoretical_execution::ExecutionModel::Twap5m {
                vec![delta.execution_model]
            } else {
                vec![
                    crate::theoretical_execution::ExecutionModel::Vwap3m,
                    crate::theoretical_execution::ExecutionModel::Twap3m,
                    crate::theoretical_execution::ExecutionModel::InstantTaker,
                ]
            };
        let reference_prices = reference_models
            .into_iter()
            .map(|model| {
                Ok(ExecutionReferencePrice {
                    execution_model: model,
                    price: klines
                        .window_prices(
                            &delta.symbol,
                            model.open_ts_us(delta.received_at_us),
                            model,
                        )?
                        .and_then(|prices| prices.average()),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        virtual_fills.push(VirtualFill {
            reference_prices,
            execution_model: delta.execution_model,
            source_id: delta.source_id,
            binding_name: delta.binding_name,
            strategy_name: delta.strategy_name,
            symbol: delta.symbol,
            venue: delta.venue,
            received_at_us: delta.received_at_us,
            execution_ts_us,
            delta_qty: delta.delta_qty,
            sample_prices,
            sample_quantity_weights: priced.quantity_weights.clone(),
            first_price: priced
                .first_price()
                .context("virtual schedule has no arrival price")?,
            zero_volume_fallback_sample_count: priced.zero_volume_fallback_sample_count,
            zero_volume_skipped_sample_count: priced.zero_volume_skipped_sample_count,
            virtual_vwap,
            virtual_fee_usdt: delta.delta_qty.abs() * virtual_vwap * delta.fee_rate,
            virtual_fee_rate: delta.fee_rate,
            target_signal: delta.target_signal,
            actual_matched_qty: 0.0,
            actual_signed_notional_usdt: 0.0,
            actual_fee_usdt: 0.0,
            matched_fill_count: 0,
        });
    }
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
        virtual_delta_count: virtual_fills
            .iter()
            .filter(|f| f.received_at_us >= start_received_at_us)
            .count(),
        missing_virtual_delta_count,
        pending_virtual_delta_count,
        legacy_fee_delta_count,
        zero_volume_fallback_sample_count,
        zero_volume_fallback_delta_count,
        zero_volume_skipped_sample_count,
        zero_volume_skipped_delta_count,
        ..AcquisitionCostTotals::default()
    };
    let mut by_strategy = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_symbol = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_side = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_liquidity = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_target_delay = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_order_delay = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_symbol_liquidity = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_symbol_target_delay = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_target_signal = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut by_execution_mode = BTreeMap::<String, BreakdownAccumulator>::new();
    let mut fill_diagnostics = Vec::new();
    for fill in virtual_fills
        .iter()
        .filter(|f| f.received_at_us >= start_received_at_us)
    {
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
            if unavailable.get(&key).is_some_and(|times| {
                times
                    .iter()
                    .any(|ts| *ts >= fill.received_at_us && *ts <= signal_ts_us)
            }) {
                totals.unmatched_fill_count += 1;
                totals.unmatched_fill_notional_usdt += (signed_qty * event.price).abs();
                continue;
            }
            if signal_ts_us.saturating_sub(fill.received_at_us) > MAX_VIRTUAL_REFERENCE_AGE_US {
                totals.stale_reference_fill_count =
                    totals.stale_reference_fill_count.saturating_add(1);
                totals.stale_reference_fill_notional_usdt += (signed_qty * event.price).abs();
                continue;
            }
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
            let execution_mode = match (liquidity, fill.target_signal.abs()) {
                ("taker", 1) => "forced_signal_taker",
                ("taker", _) => "fallback_or_other_taker",
                ("maker", _) => "maker",
                _ => "unclassified",
            };
            let reference_turnover = (signed_qty * fill.virtual_vwap).abs();
            let price_shortfall = signed_qty * (event.price - fill.virtual_vwap);
            let first_minute_shortfall = signed_qty * (event.price - fill.first_price);
            let five_sample_drift = signed_qty * (fill.first_price - fill.virtual_vwap);
            totals.first_minute_shortfall_usdt += first_minute_shortfall;
            totals.five_sample_drift_usdt += five_sample_drift;
            fill_diagnostics.push((
                index,
                AcquisitionFillDiagnostic {
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
                    target_signal: fill.target_signal,
                    execution_mode,
                    reference_turnover_usdt: reference_turnover,
                    price_shortfall_usdt: price_shortfall,
                    first_minute_shortfall_usdt: first_minute_shortfall,
                    five_sample_drift_usdt: five_sample_drift,
                    price_shortfall_bps: if reference_turnover > 0.0 {
                        price_shortfall / reference_turnover * 10_000.0
                    } else {
                        0.0
                    },
                },
            ));
            for (values, bucket) in [
                (&mut by_strategy, fill.strategy_name.as_str()),
                (&mut by_symbol, event.symbol.as_str()),
                (&mut by_side, side),
                (&mut by_liquidity, liquidity),
                (&mut by_target_delay, target_delay),
                (&mut by_order_delay, order_delay),
                (
                    &mut by_target_signal,
                    match fill.target_signal {
                        -2 => "signal_-2",
                        -1 => "signal_-1",
                        0 => "signal_0",
                        1 => "signal_1",
                        2 => "signal_2",
                        _ => "signal_invalid",
                    },
                ),
                (&mut by_execution_mode, execution_mode),
            ] {
                values.entry(bucket.to_string()).or_default().add(
                    signed_qty,
                    event.price,
                    fill.virtual_vwap,
                    fill.first_price,
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
                    fill.first_price,
                    actual_fee,
                    fill.virtual_fee_rate,
                );
            }
        }
    }

    let mut output_rows = Vec::with_capacity(virtual_fills.len());
    let mut output_indices = Vec::with_capacity(virtual_fills.len());
    let mut points: Vec<AcquisitionCostPoint> = Vec::with_capacity(virtual_fills.len());
    let mut cumulative_virtual_turnover = 0.0;
    let mut cumulative_actual_turnover = 0.0;
    let mut cumulative_price_shortfall = 0.0;
    let mut cumulative_after_fee_shortfall = 0.0;
    for (index, fill) in virtual_fills.into_iter().enumerate() {
        if fill.received_at_us < start_received_at_us && fill.matched_fill_count == 0 {
            continue;
        }
        let virtual_turnover = if fill.received_at_us >= start_received_at_us {
            (fill.delta_qty * fill.virtual_vwap).abs()
        } else {
            0.0
        };
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
            ts_us: fill.received_at_us.max(start_received_at_us),
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
        output_indices.push(index);
        output_rows.push(AcquisitionCostRow {
            execution_model: fill.execution_model,
            execution_window_secs: fill.execution_model.window_secs(),
            reference_prices: fill.reference_prices,
            source_id: fill.source_id,
            binding_name: fill.binding_name,
            strategy_name: fill.strategy_name,
            symbol: fill.symbol,
            venue: fill.venue,
            received_at_us: fill.received_at_us,
            virtual_execution_ts_us: fill.execution_ts_us,
            delta_qty: fill.delta_qty,
            sample_prices: fill.sample_prices,
            sample_quantity_weights: fill.sample_quantity_weights,
            zero_volume_fallback_sample_count: fill.zero_volume_fallback_sample_count,
            zero_volume_skipped_sample_count: fill.zero_volume_skipped_sample_count,
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
            after_fee_shortfall_bps: after_fee_shortfall.and_then(|shortfall| {
                (matched_turnover > 0.0).then_some(shortfall / matched_turnover * 10_000.0)
            }),
            execution: AcquisitionExecution::default(),
        });
    }
    totals.price_shortfall_bps = if totals.matched_virtual_turnover_usdt > 0.0 {
        totals.price_shortfall_usdt / totals.matched_virtual_turnover_usdt * 10_000.0
    } else {
        0.0
    };
    totals.after_fee_shortfall_bps = if totals.matched_virtual_turnover_usdt > 0.0 {
        totals.after_fee_shortfall_usdt / totals.matched_virtual_turnover_usdt * 10_000.0
    } else {
        0.0
    };
    totals.first_minute_shortfall_bps = if totals.matched_virtual_turnover_usdt > 0.0 {
        totals.first_minute_shortfall_usdt / totals.matched_virtual_turnover_usdt * 10_000.0
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
    let (rows, filtered_row_count) = detail_page(
        &output_rows,
        &output_indices,
        &fill_diagnostics,
        row_symbol,
        row_strategy,
        row_sort,
        page,
        page_size,
    );
    let page_count = filtered_row_count.div_ceil(page_size);
    let mut fill_diagnostics: Vec<_> = fill_diagnostics.into_iter().map(|(_, fill)| fill).collect();
    fill_diagnostics.sort_by(|left, right| {
        right
            .price_shortfall_usdt
            .partial_cmp(&left.price_shortfall_usdt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fill_diagnostics.truncate(100);
    Ok(AcquisitionCostReport {
        execution_model: crate::theoretical_execution::scope_model(&execution_models),
        execution_window_secs: crate::theoretical_execution::scope_window_secs(&execution_models),
        generated_at_us,
        price_basis: crate::theoretical_execution::scope_model(&execution_models).map_or(
            "source_specific_execution",
            crate::theoretical_execution::ExecutionModel::price_basis,
        ),
        fee_basis: if crate::theoretical_execution::scope_model(&execution_models)
            == Some(crate::theoretical_execution::ExecutionModel::InstantTaker)
        {
            "actual_maker_taker_vs_archived_taker_rate_or_current_taker_legacy_fallback"
        } else {
            "actual_maker_taker_vs_archived_theoretical_rate_or_current_legacy_fallback"
        },
        start_received_at_us,
        end_received_at_us,
        source_ids: source_ids.to_vec(),
        strategy_name: strategy_name.map(str::to_string),
        page,
        page_size,
        page_count,
        returned_row_count: rows.len(),
        row_symbol: row_symbol.map(str::to_owned),
        row_strategy: row_strategy.map(str::to_owned),
        row_sort,
        filtered_row_count,
        totals,
        points,
        warnings,
        by_strategy: finish_breakdowns(by_strategy, true),
        by_symbol: finish_breakdowns(by_symbol, true),
        by_side: finish_breakdowns(by_side, false),
        by_liquidity: finish_breakdowns(by_liquidity, false),
        by_target_delay: finish_breakdowns(by_target_delay, false),
        by_order_delay: finish_breakdowns(by_order_delay, false),
        by_symbol_liquidity: finish_breakdowns(by_symbol_liquidity, true),
        by_symbol_target_delay: finish_breakdowns(by_symbol_target_delay, true),
        by_target_signal: finish_breakdowns(by_target_signal, false),
        by_execution_mode: finish_breakdowns(by_execution_mode, false),
        worst_fills: fill_diagnostics,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail_row(symbol: &str, received_at_us: i64, shortfall: Option<f64>) -> AcquisitionCostRow {
        AcquisitionCostRow {
            execution_model: crate::theoretical_execution::ExecutionModel::Twap5m,
            execution_window_secs: 300,
            reference_prices: Vec::new(),
            sample_quantity_weights: vec![0.2; 5],
            source_id: "source".into(),
            binding_name: "binding".into(),
            strategy_name: "strategy".into(),
            symbol: symbol.into(),
            venue: "binance-futures".into(),
            received_at_us,
            virtual_execution_ts_us: received_at_us + 300_000_000,
            delta_qty: 1.0,
            sample_prices: vec![Some(1.0); 5],
            zero_volume_fallback_sample_count: 0,
            zero_volume_skipped_sample_count: 0,
            virtual_vwap: 1.0,
            virtual_turnover_usdt: 1.0,
            virtual_fee_usdt: 0.0,
            actual_matched_qty: 1.0,
            actual_vwap: shortfall.map(|v| 1.0 + v),
            actual_matched_turnover_usdt: 1.0,
            actual_matched_fee_usdt: 0.0,
            matched_fill_count: 1,
            fill_ratio: 1.0,
            price_shortfall_usdt: shortfall,
            fee_shortfall_usdt: Some(0.0),
            after_fee_shortfall_usdt: shortfall,
            price_shortfall_bps: shortfall.map(|v| v * 10_000.0),
            after_fee_shortfall_bps: shortfall.map(|v| v * 10_000.0),
            execution: AcquisitionExecution::default(),
        }
    }

    #[test]
    fn symbol_drilldown_finds_old_worst_delta_and_preserves_fill_identity() {
        let mut rows = vec![
            detail_row("CTKUSDT", 1, Some(12.0)),
            detail_row("CTKUSDT", 2, Some(-1.0)),
        ];
        rows.extend((3..60).map(|i| detail_row("BTCUSDT", i, Some(1.0))));
        rows.push(detail_row("CTKUSDT", 60, Some(2.0)));
        // Original virtual-fill indices need not equal output-row offsets.
        let indices: Vec<_> = (100..100 + rows.len()).collect();
        let diagnostics = vec![
            (100, diagnostic(123, 1.0, 13.0)),
            (160 - 1, diagnostic(456, 1.0, 3.0)),
        ];
        let (latest, total) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            None,
            None,
            AcquisitionRowSort::Latest,
            1,
            25,
        );
        assert_eq!(total, 60);
        assert!(!latest.iter().any(|r| r.received_at_us == 1));
        let (worst, count) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            Some("CTKUSDT"),
            None,
            AcquisitionRowSort::PriceShortfall,
            1,
            2,
        );
        assert_eq!(count, 3);
        assert_eq!(
            worst.iter().map(|r| r.received_at_us).collect::<Vec<_>>(),
            [60, 1]
        );
        assert_eq!(worst[0].execution.first_fill_ts_us, Some(456));
        assert_eq!(worst[1].execution.first_fill_ts_us, Some(123));
        let (rest, _) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            Some("CTKUSDT"),
            None,
            AcquisitionRowSort::PriceShortfall,
            2,
            2,
        );
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].received_at_us, 2);
        assert!(rest[0].execution.fills.is_empty());
        assert_eq!(rows[0].price_shortfall_usdt, Some(12.0));
        assert!(rows[0].execution.fills.is_empty());
        assert_eq!(
            rows.len(),
            60,
            "detail selection must not mutate the aggregate inputs"
        );
    }

    #[test]
    fn strategy_drilldown_finds_small_behind_mid_pages_without_mixing_fills() {
        let mut small = detail_row("CTKUSDT", 10, Some(12.0));
        small.strategy_name = "rbf_small".into();
        let mut other_symbol = small.clone();
        other_symbol.symbol = "BTCUSDT".into();
        let mut rows = vec![small, other_symbol];
        rows.extend((10..50).map(|ts| {
            let mut row = detail_row("CTKUSDT", ts, Some(1.0));
            row.strategy_name = "rbf_mid_1min".into();
            row
        }));
        let indices: Vec<_> = (500..500 + rows.len()).collect();
        let diagnostics = vec![
            (500, diagnostic(111, 1.0, 13.0)),
            (501, diagnostic(222, 1.0, 13.0)),
            (502, diagnostic(333, 1.0, 2.0)),
        ];
        let (latest, _) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            None,
            None,
            AcquisitionRowSort::Latest,
            1,
            25,
        );
        assert!(latest.iter().all(|row| row.strategy_name == "rbf_mid_1min"));
        let (selected, count) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            Some("CTKUSDT"),
            Some("rbf_small"),
            AcquisitionRowSort::PriceShortfall,
            1,
            25,
        );
        assert_eq!(count, 1);
        assert_eq!(selected[0].strategy_name, "rbf_small");
        assert_eq!(selected[0].symbol, "CTKUSDT");
        assert_eq!(selected[0].execution.first_fill_ts_us, Some(111));
        assert_eq!(selected[0].execution.fills.len(), 1);
        let (all_symbols, count) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            None,
            Some("rbf_small"),
            AcquisitionRowSort::Latest,
            1,
            25,
        );
        assert_eq!(count, 2);
        assert!(
            all_symbols
                .iter()
                .all(|row| row.strategy_name == "rbf_small")
        );
        let (unknown, _) = detail_page(
            &rows,
            &indices,
            &diagnostics,
            None,
            Some("small"),
            AcquisitionRowSort::Latest,
            1,
            25,
        );
        assert!(unknown.is_empty(), "strategy identity is an exact match");
        assert_eq!(rows.len(), 42);
        assert!(rows.iter().all(|row| row.execution.fills.is_empty()));
    }

    #[test]
    fn detail_sort_keeps_unknown_after_negative_costs_and_handles_empty_pages() {
        let rows = vec![
            detail_row("CTKUSDT", 1, None),
            detail_row("CTKUSDT", 2, Some(-2.0)),
            detail_row("CTKUSDT", 3, Some(-2.0)),
        ];
        let (page, total) = detail_page(
            &rows,
            &[0, 1, 2],
            &[],
            Some("CTKUSDT"),
            None,
            AcquisitionRowSort::PriceShortfall,
            1,
            2,
        );
        assert_eq!(total, 3);
        assert_eq!(
            page.iter().map(|r| r.received_at_us).collect::<Vec<_>>(),
            [2, 3]
        );
        let (empty, count) = detail_page(
            &rows,
            &[0, 1, 2],
            &[],
            Some("CTK"),
            None,
            AcquisitionRowSort::Latest,
            1,
            25,
        );
        assert!(empty.is_empty());
        assert_eq!(count, 0, "symbols must match exactly");
        let (empty, count) = detail_page(
            &rows,
            &[0, 1, 2],
            &[],
            None,
            None,
            AcquisitionRowSort::PriceShortfall,
            20,
            25,
        );
        assert!(empty.is_empty());
        assert_eq!(count, 3);
    }

    fn diagnostic(ts: i64, qty: f64, price: f64) -> AcquisitionFillDiagnostic {
        AcquisitionFillDiagnostic {
            source_id: "source".into(),
            strategy_name: "strategy".into(),
            symbol: "TESTUSDT".into(),
            target_received_at_us: 0,
            order_signal_ts_us: ts / 2,
            fill_ts_us: ts,
            client_order_id: ts,
            side: if qty > 0.0 { "buy" } else { "sell" },
            liquidity: "maker",
            target_signal: 0,
            execution_mode: "maker",
            actual_qty: qty,
            actual_price: price,
            virtual_price: 1.0,
            target_delay_us: ts,
            order_delay_us: ts / 2,
            reference_turnover_usdt: qty.abs(),
            price_shortfall_usdt: qty * (price - 1.0),
            first_minute_shortfall_usdt: 0.0,
            five_sample_drift_usdt: 0.0,
            price_shortfall_bps: 0.0,
        }
    }

    #[test]
    fn execution_diagnostics_weight_by_absolute_quantity_and_include_window_boundary() {
        for sign in [1.0, -1.0] {
            let early = diagnostic(10_000_000, sign, 1.0);
            let late = diagnostic(300_000_000, 3.0 * sign, 2.0);
            let summary = execution_summary(&[&late, &early], 300_000_000);
            assert_eq!(summary.first_fill_ts_us, Some(10_000_000));
            assert_eq!(summary.last_fill_ts_us, Some(300_000_000));
            assert_eq!(summary.qty_weighted_target_delay_us, Some(227_500_000.0));
            assert_eq!(summary.qty_weighted_order_delay_us, Some(113_750_000.0));
            assert_eq!(summary.after_window_qty_ratio, Some(0.75));
            assert!(!summary.fills_sampled);
            assert_eq!(summary.fills[0].actual_qty, sign);
        }
    }

    #[test]
    fn execution_plot_sampling_preserves_extrema_without_sampling_statistics() {
        let mut fills: Vec<_> = (0..1000)
            .map(|i| diagnostic(i * 1_000_000, 1.0, 1.0))
            .collect();
        fills[501].actual_price = 0.5;
        fills[502].actual_price = 2.0;
        fills.reverse();
        let summary = execution_summary(&fills.iter().collect::<Vec<_>>(), 500_000_000);
        assert!(summary.fills_sampled);
        assert!(summary.fills.len() <= 200);
        assert_eq!(summary.fills.first().unwrap().fill_ts_us, 0);
        assert_eq!(summary.fills.last().unwrap().fill_ts_us, 999_000_000);
        assert!(summary.fills.iter().any(|fill| fill.actual_price == 0.5));
        assert!(summary.fills.iter().any(|fill| fill.actual_price == 2.0));
        assert_eq!(summary.qty_weighted_target_delay_us, Some(499_500_000.0));
        assert_eq!(summary.after_window_qty_ratio, Some(0.5));
    }

    #[test]
    fn empty_execution_is_unavailable_rather_than_zero_delay() {
        let summary = execution_summary(&[], 300_000_000);
        assert!(summary.first_fill_ts_us.is_none());
        assert!(summary.qty_weighted_target_delay_us.is_none());
        assert!(summary.after_window_qty_ratio.is_none());
        assert!(summary.fills.is_empty());
    }

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
        value.add(2.0, 101.0, 100.0, 100.5, 0.02, 0.0002);
        value.add(-3.0, 99.0, 100.0, 99.5, 0.03, 0.0002);
        let row = value.finish("all".to_string());
        assert_eq!(row.reference_turnover_usdt, 500.0);
        assert_eq!(row.actual_turnover_usdt, 499.0);
        assert_eq!(row.price_shortfall_usdt, 5.0);
        assert_eq!(
            row.price_shortfall_usdt,
            row.first_minute_shortfall_usdt + row.five_sample_drift_usdt
        );
        assert_eq!(row.price_shortfall_bps, 100.0);
        assert!((row.after_fee_shortfall_bps - 99.0).abs() < 1e-10);
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
