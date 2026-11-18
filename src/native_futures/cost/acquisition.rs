//! Adapt verified domestic cost evidence to the shared browser's acquisition report.
use super::{FillCost, MINUTE_US, Report, SignalPrice};
use crate::acquisition_cost::AcquisitionRowSort;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone)]
pub(in crate::native_futures) struct Query {
    pub source_id: String,
    pub now_us: i64,
    pub start_us: i64,
    pub end_us: i64,
    pub strategy_name: Option<String>,
    pub row_strategy: Option<String>,
    pub row_symbol: Option<String>,
    pub row_sort: AcquisitionRowSort,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Default)]
struct Sum {
    amount: f64,
    missing: bool,
}

impl Sum {
    fn add(&mut self, value: Option<f64>) {
        if let Some(value) = value {
            self.amount += value;
        } else {
            self.missing = true;
        }
    }

    fn complete(&self) -> Option<f64> {
        (!self.missing && self.amount.is_finite()).then_some(self.amount)
    }
}

#[derive(Default)]
struct Matched {
    count: usize,
    lots: i64,
    reference_turnover: f64,
    actual_turnover: f64,
    actual_fee: Sum,
    virtual_fee: Sum,
    price: f64,
    first_minute: f64,
    drift: f64,
}

struct Comparable<'a> {
    fill: &'a FillCost,
    actual_turnover: f64,
    reference_turnover: f64,
    price: f64,
    first_minute: f64,
    delay_us: i64,
}

impl<'a> Comparable<'a> {
    fn new(fill: &'a FillCost, signal: &SignalPrice) -> Option<Self> {
        let actual_turnover = actual_turnover(fill)?;
        let multiple = fill.contract_multiplier?;
        let virtual_price = fill.theoretical_price?;
        let price = fill.price_shortfall_cny?;
        let first = *signal.sample_prices.first()?;
        let quantity = fill.lots as f64 * multiple;
        let first_minute =
            if fill.side == "buy" { 1.0 } else { -1.0 } * quantity * (fill.actual_price - first);
        Some(Self {
            fill,
            actual_turnover,
            reference_turnover: quantity * virtual_price,
            price,
            first_minute,
            delay_us: fill.received_us - signal.received_us,
        })
    }
}

fn actual_turnover(fill: &FillCost) -> Option<f64> {
    let turnover = fill.lots as f64 * fill.actual_price * fill.contract_multiplier?;
    turnover.is_finite().then_some(turnover)
}

fn actual_fee(fill: &FillCost) -> Option<f64> {
    fill.actual_commission
        .filter(|_| fill.commission_currency.as_deref() == Some("CNY"))
}

impl Matched {
    fn add(&mut self, matched: &Comparable<'_>) {
        self.count += 1;
        self.lots += matched.fill.lots;
        self.reference_turnover += matched.reference_turnover;
        self.actual_turnover += matched.actual_turnover;
        self.actual_fee.add(actual_fee(matched.fill));
        self.virtual_fee
            .add(matched.fill.theoretical_commission_cny);
        self.price += matched.price;
        self.first_minute += matched.first_minute;
        self.drift += matched.price - matched.first_minute;
    }

    fn fee_gap(&self) -> Option<f64> {
        (self.count != 0).then_some(())?;
        Some(self.actual_fee.complete()? - self.virtual_fee.complete()?)
    }

    fn after_fee(&self) -> Option<f64> {
        self.fee_gap().map(|fee| self.price + fee)
    }

    fn breakdown(&self, bucket: &str) -> Value {
        json!({
            "bucket": bucket,
            "fill_count": self.count,
            "reference_turnover_quote": self.reference_turnover,
            "actual_turnover_quote": self.actual_turnover,
            "actual_fee_quote": self.actual_fee.complete(),
            "virtual_fee_quote": self.virtual_fee.complete(),
            "price_shortfall_quote": self.price,
            "first_minute_shortfall_quote": self.first_minute,
            "five_sample_drift_quote": self.drift,
            "price_shortfall_bps": bps(self.price, self.reference_turnover),
            "after_fee_shortfall_bps": self.after_fee().and_then(|v| bps(v, self.reference_turnover)),
            "first_minute_shortfall_bps": bps(self.first_minute, self.reference_turnover),
            "after_fee_shortfall_quote": self.after_fee(),
        })
    }
}

fn bps(cost: f64, turnover: f64) -> Option<f64> {
    (turnover > 0.0).then(|| cost / turnover * 10_000.0)
}

fn breakdown<'a>(groups: BTreeMap<&'a str, Matched>) -> Vec<Value> {
    groups
        .into_iter()
        .map(|(key, matched)| matched.breakdown(key))
        .collect()
}

struct Target<'a> {
    day: &'a str,
    signal: &'a SignalPrice,
    matched: Matched,
    fills: Vec<Comparable<'a>>,
}

impl Target<'_> {
    fn key(&self) -> (i64, &str, &str, &str, &str) {
        let s = self.signal;
        (s.received_us, self.day, &s.venue, &s.symbol, &s.signal_id)
    }

    fn row(&self, q: &Query) -> Value {
        let s = self.signal;
        let m = &self.matched;
        let start = s.sample_open_us[0];
        let end = s.sample_open_us.last().copied().unwrap_or(start) + MINUTE_US;
        // The released chart derives wall-clock sample times from its window.
        // Keep closed intervals as gaps rather than moving observed prices.
        let minutes = ((end - start) / MINUTE_US) as usize;
        let mut prices = vec![None; minutes];
        let mut weights = vec![0.0; minutes];
        for ((t, price), weight) in s
            .sample_open_us
            .iter()
            .zip(&s.sample_prices)
            .zip(&s.sample_weights)
        {
            let index = ((*t - start) / MINUTE_US) as usize;
            prices[index] = Some(*price);
            weights[index] = *weight;
        }
        let model = match s.model {
            super::Model::Twap => "native_twap",
            super::Model::Vwap => "native_vwap",
        };
        let sign = if s.side == "buy" { 1 } else { -1 };
        let actual_vwap = (m.lots > 0).then(|| {
            self.fills
                .iter()
                .map(|f| f.fill.actual_price * f.fill.lots as f64)
                .sum::<f64>()
                / m.lots as f64
        });
        json!({
            "execution_model": model,
            "execution_window_secs": (end - start) / 1_000_000,
            "native_trading_minutes": s.window_minutes,
            "reference_prices": [{"execution_model": model, "price": s.theoretical_price}],
            "source_id": q.source_id,
            "binding_name": format!("{}:{}", self.day, s.signal_id),
            "native_signal_id": s.signal_id,
            "strategy_name": null,
            "trading_day": self.day,
            "symbol": s.symbol,
            "venue": s.venue,
            "offset": s.offset,
            "received_at_us": s.received_us,
            "virtual_execution_ts_us": end,
            "delta_qty": sign * s.expected_lots,
            "sample_open_us": s.sample_open_us,
            "sample_prices": prices,
            "sample_quantity_weights": weights,
            "zero_volume_fallback_sample_count": 0,
            "zero_volume_skipped_sample_count": 0,
            "virtual_vwap": s.theoretical_price,
            "virtual_turnover_quote": s.virtual_turnover_cny,
            "virtual_fee_quote": s.virtual_commission_cny,
            "actual_matched_qty": sign * m.lots,
            "actual_vwap": actual_vwap,
            "actual_matched_turnover_quote": m.actual_turnover,
            "actual_matched_fee_quote": m.actual_fee.complete(),
            "matched_fill_count": m.count,
            "fill_ratio": m.lots as f64 / s.expected_lots as f64,
            "price_shortfall_quote": (m.count > 0).then_some(m.price),
            "fee_shortfall_quote": m.fee_gap(),
            "after_fee_shortfall_quote": m.after_fee(),
            "price_shortfall_bps": bps(m.price, m.reference_turnover),
            "after_fee_shortfall_bps": m.after_fee().and_then(|v| bps(v, m.reference_turnover)),
            "execution": self.execution(end),
        })
    }

    fn execution(&self, end: i64) -> Value {
        let ordered = &self.fills;
        let lots = self.matched.lots as f64;
        let average_delay = (lots > 0.0).then(|| {
            ordered
                .iter()
                .map(|f| f.fill.lots as f64 * f.delay_us as f64)
                .sum::<f64>()
                / lots
        });
        let after_window = (lots > 0.0).then(|| {
            ordered
                .iter()
                .filter(|f| f.fill.received_us >= end)
                .map(|f| f.fill.lots)
                .sum::<i64>() as f64
                / lots
        });
        let indices = crate::acquisition_cost::execution_sample_indices(ordered.len(), |a, b| {
            ordered[a]
                .fill
                .actual_price
                .total_cmp(&ordered[b].fill.actual_price)
        });
        let fills: Vec<_> = indices
            .iter()
            .map(|&i| {
                let c = &ordered[i];
                let f = c.fill;
                json!({
                    "fill_ts_us": f.received_us,
                    "trade_id": f.trade_id,
                    "order_ref": f.order_ref,
                    "session": f.session,
                    "offset": f.offset,
                "reported_commission": f.actual_commission,
                "commission_currency": f.commission_currency,
                    "actual_price": f.actual_price,
                    "actual_qty": f.lots,
                    "liquidity": "unclassified",
                    "target_delay_us": c.delay_us,
                    "order_delay_us": null,
                })
            })
            .collect();
        json!({
            "first_fill_ts_us": ordered.first().map(|f| f.fill.received_us),
            "last_fill_ts_us": ordered.last().map(|f| f.fill.received_us),
            "qty_weighted_target_delay_us": average_delay,
            "qty_weighted_order_delay_us": null,
            "after_window_qty_ratio": after_window,
            "fills_sampled": fills.len() < ordered.len(),
            "fills": fills,
        })
    }
}

pub(in crate::native_futures) fn report(
    q: &Query,
    reports: &[Report],
    mut warnings: Vec<String>,
    factual_through_us: Option<i64>,
) -> Value {
    let mut targets = Vec::new();
    let mut selected = BTreeMap::new();
    let mut virtual_turnover = Sum::default();
    let mut virtual_fee = Sum::default();
    let mut priced_count = 0;
    let mut pending_count = 0;
    let mut missing_count = 0;
    let mut actual_turnover_sum = Sum::default();
    let mut actual_fee_sum = Sum::default();
    let mut unmatched_count = 0;
    let mut unmatched_turnover = Sum::default();
    let mut actual_count = 0;
    let mut actual_lots = 0_i64;
    let mut all = Matched::default();
    let mut by_symbol = BTreeMap::<&str, Matched>::new();
    let mut by_side = BTreeMap::<&str, Matched>::new();
    let mut by_offset = BTreeMap::<&str, Matched>::new();
    let mut by_target_delay = BTreeMap::<&str, Matched>::new();
    let mut by_symbol_target_delay = BTreeMap::<String, Matched>::new();

    for day in reports {
        for s in &day.signals {
            let in_range = (q.start_us..=q.end_us).contains(&s.received_us);
            if in_range {
                match (s.theoretical_price, s.pending) {
                    (Some(_), _) => {
                        priced_count += 1;
                        virtual_turnover.add(s.virtual_turnover_cny);
                        virtual_fee.add(s.virtual_commission_cny);
                    }
                    (None, true) => pending_count += 1,
                    (None, false) => missing_count += 1,
                }
            }
            if s.theoretical_price.is_some() {
                selected.insert(
                    (day.trading_day.as_str(), s.signal_id.as_str()),
                    targets.len(),
                );
                targets.push(Target {
                    day: &day.trading_day,
                    signal: s,
                    matched: Matched::default(),
                    fills: Vec::new(),
                });
            }
        }
        for f in &day.fills {
            actual_count += 1;
            actual_lots += f.lots;
            actual_turnover_sum.add(actual_turnover(f));
            actual_fee_sum.add(actual_fee(f));
            let target = f
                .signal_id
                .as_deref()
                .and_then(|id| selected.get(&(day.trading_day.as_str(), id)))
                .map(|&index| &mut targets[index]);
            if let Some(target) = target
                && let Some(m) = Comparable::new(f, target.signal)
            {
                let delay = crate::acquisition_cost::delay_bucket(m.delay_us);
                all.add(&m);
                by_symbol.entry(&f.symbol).or_default().add(&m);
                by_side.entry(&f.side).or_default().add(&m);
                by_offset.entry(&f.offset).or_default().add(&m);
                by_target_delay.entry(delay).or_default().add(&m);
                by_symbol_target_delay
                    .entry(format!("{}:{delay}", f.symbol))
                    .or_default()
                    .add(&m);
                target.matched.add(&m);
                target.fills.push(m);
            } else {
                unmatched_count += 1;
                unmatched_turnover.add(actual_turnover(f));
            }
        }
    }
    targets.retain(|t| t.signal.received_us >= q.start_us || t.matched.count > 0);
    targets.sort_by(|a, b| a.key().cmp(&b.key()));
    for t in &mut targets {
        t.fills.sort_by(|a, b| {
            (a.fill.received_us, a.fill.trade_id.as_str())
                .cmp(&(b.fill.received_us, b.fill.trade_id.as_str()))
        });
    }
    let mut point_matched = Matched::default();
    let mut point_virtual = 0.0;
    let mut points = BTreeMap::new();
    for t in &targets {
        if t.signal.received_us >= q.start_us {
            point_virtual += t.signal.virtual_turnover_cny.unwrap_or_default();
        }
        for f in &t.fills {
            point_matched.add(f);
        }
        let ts = t.signal.received_us.max(q.start_us);
        points.insert(
            ts,
            json!({
                "ts_us": ts,
                "virtual_turnover_quote": point_virtual,
                "actual_matched_turnover_quote": point_matched.actual_turnover,
                "price_shortfall_quote": point_matched.price,
                "after_fee_shortfall_quote": point_matched.after_fee(),
            }),
        );
    }

    let actual_notional = actual_turnover_sum.complete();
    let virtual_notional = (priced_count > 0)
        .then(|| virtual_turnover.complete())
        .flatten();
    if reports.iter().all(|r| !r.reference_available) && q.strategy_name.is_none() {
        warnings.push("所选区间暂无可用的原生执行基准；保留已入库的事实成交。".into());
    }
    if actual_turnover_sum.missing {
        warnings
            .push("部分事实成交缺少已确认的合约乘数，事实总成交额及成交额覆盖率不可用。".into());
    }
    if actual_fee_sum.missing || virtual_fee.missing || all.fee_gap().is_none() && all.count > 0 {
        warnings.push(
            "部分手数缺少已确认的人民币事实手续费或对应开平的理论规则；相关费用及费后滑点不可用。"
                .into(),
        );
    }
    if targets.iter().any(|t| {
        t.signal
            .sample_open_us
            .windows(2)
            .any(|w| w[1] - w[0] != MINUTE_US)
    }) {
        warnings.push("理论基准按交易日及交易时段计价，价格路径中的空档可能为休市。".into());
    }

    let comparable_delta_count = targets.iter().filter(|t| t.matched.count > 0).count();
    let mut details: Vec<_> = targets
        .iter()
        .filter(|t| {
            q.row_strategy.is_none()
                && q.row_symbol.as_deref().is_none_or(|symbol| {
                    t.signal.symbol == symbol
                        || format!("{}:{}", t.signal.venue, t.signal.symbol) == symbol
                })
        })
        .collect();
    if q.row_sort == AcquisitionRowSort::PriceShortfall {
        details.sort_by(|a, b| {
            (a.matched.count > 0)
                .cmp(&(b.matched.count > 0))
                .then_with(|| a.matched.price.total_cmp(&b.matched.price))
                .then_with(|| a.key().cmp(&b.key()))
        });
    }
    let count = details.len();
    let end = count.saturating_sub((q.page - 1).saturating_mul(q.page_size));
    let rows: Vec<_> = details[end.saturating_sub(q.page_size)..end]
        .iter()
        .map(|t| t.row(q))
        .collect();
    let scoped_facts = q.strategy_name.is_none();
    let factual_total = |amount: Option<f64>| scoped_facts.then_some(amount).flatten();

    let totals = json!({
            "virtual_delta_count": priced_count,
            "missing_virtual_delta_count": missing_count,
            "pending_virtual_delta_count": pending_count,
            "zero_volume_fallback_delta_count": 0, "zero_volume_skipped_delta_count": 0,
            "zero_volume_fallback_sample_count": 0, "zero_volume_skipped_sample_count": 0,
            "legacy_fee_delta_count": 0,
            "comparable_delta_count": comparable_delta_count,
            "virtual_turnover_quote": virtual_notional,
            "virtual_fee_quote": (priced_count > 0).then(|| virtual_fee.complete()).flatten(),
            "actual_fill_count": actual_count, "actual_lots": actual_lots,
            "actual_turnover_quote": factual_total(actual_notional),
            "actual_fee_quote": factual_total(actual_fee_sum.complete()),
            "actual_matched_turnover_quote": factual_total(Some(all.actual_turnover)),
            "actual_matched_fee_quote": factual_total(all.actual_fee.complete()),
            "matched_virtual_turnover_quote": factual_total(Some(all.reference_turnover)),
            "matched_fill_count": all.count, "matched_lots": all.lots,
            "unmatched_fill_count": unmatched_count,
            "unmatched_fill_notional_quote": factual_total(unmatched_turnover.complete()),
            "opposite_fill_count": null, "opposite_fill_notional_quote": null,
        "stale_reference_fill_count": null, "stale_reference_fill_notional_quote": null,
            "price_shortfall_quote": (all.count > 0).then_some(all.price),
            "first_minute_shortfall_quote": (all.count > 0).then_some(all.first_minute),
            "five_sample_drift_quote": (all.count > 0).then_some(all.drift),
            "fee_shortfall_quote": all.fee_gap(), "after_fee_shortfall_quote": all.after_fee(),
            "price_shortfall_bps": bps(all.price, all.reference_turnover),
            "after_fee_shortfall_bps": all.after_fee().and_then(|v| bps(v, all.reference_turnover)),
            "first_minute_shortfall_bps": bps(all.first_minute, all.reference_turnover),
            "matched_turnover_coverage": virtual_notional.map(|v| all.reference_turnover / v),
            "actual_fill_reference_coverage": factual_total(actual_notional.map(|v| if v > 0.0 { all.actual_turnover / v } else { 0.0 })),
    });
    json!({
        "execution_model": "native_reference",
        "execution_window_secs": 0,
        "generated_at_us": q.now_us,
        "price_basis": "minute_turnover_cny / (minute_volume_lots * contract_multiplier)",
        "fee_basis": "reported_cny_commission_and_explicit_native_offset_fee_rule",
        "start_received_at_us": q.start_us,
        "end_received_at_us": q.end_us,
        "factual_through_us": factual_through_us,
        "source_ids": [q.source_id],
        "strategy_name": q.strategy_name,
        "quote_currency": "CNY", "quantity_unit": "lot",
        "page": q.page, "page_size": q.page_size,
        "page_count": count.div_ceil(q.page_size), "returned_row_count": rows.len(),
        "row_symbol": q.row_symbol, "row_strategy": q.row_strategy, "row_sort": q.row_sort,
        "filtered_row_count": count,
        "totals": totals,
        "points": points.into_values().collect::<Vec<_>>(),
        "warnings": warnings,
        "by_strategy": [], "by_symbol": breakdown(by_symbol), "by_side": breakdown(by_side),
        "by_offset": breakdown(by_offset), "by_target_delay": breakdown(by_target_delay),
        "by_symbol_target_delay": by_symbol_target_delay.into_iter().map(|(key, v)| v.breakdown(&key)).collect::<Vec<_>>(),
        "by_liquidity": [], "by_order_delay": [], "by_symbol_liquidity": [],
        "by_target_signal": [], "by_execution_mode": [],
        "worst_fills": worst_fills(q, &targets), "rows": rows,
    })
}

fn worst_fills(q: &Query, targets: &[Target<'_>]) -> Vec<Value> {
    const MAX: usize = 25;
    let mut fills: Vec<_> = targets
        .iter()
        .flat_map(|t| t.fills.iter().map(move |f| (t, f)))
        .collect();
    fills.sort_by(|(a_target, a), (b_target, b)| {
        b.price
            .total_cmp(&a.price)
            .then_with(|| a_target.key().cmp(&b_target.key()))
            .then_with(|| a.fill.trade_id.cmp(&b.fill.trade_id))
    });
    fills.into_iter().take(MAX).map(|(target, c)| {
        let f = c.fill;
        json!({
            "source_id": q.source_id, "strategy_name": null,
            "symbol": f.symbol, "venue": f.venue, "trading_day": f.trading_day,
            "trade_id": f.trade_id, "session": f.session, "order_ref": f.order_ref,
            "target_received_at_us": target.signal.received_us, "order_signal_ts_us": null,
            "fill_ts_us": f.received_us, "client_order_id": null,
            "side": f.side, "offset": f.offset, "liquidity": "unclassified",
                "reported_commission": f.actual_commission, "commission_currency": f.commission_currency,
            "target_signal": null, "execution_mode": null,
            "actual_qty": f.lots, "actual_price": f.actual_price, "virtual_price": f.theoretical_price,
            "target_delay_us": c.delay_us, "order_delay_us": null,
            "reference_turnover_quote": c.reference_turnover, "price_shortfall_quote": c.price,
            "first_minute_shortfall_quote": c.first_minute, "five_sample_drift_quote": c.price - c.first_minute,
            "price_shortfall_bps": bps(c.price, c.reference_turnover),
        })
    }).collect()
}
