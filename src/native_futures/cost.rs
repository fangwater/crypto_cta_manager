//! Actual-quantity cost comparison against manually maintained market evidence.
//! No strategy attribution, synthetic fills, account NAV or execution side effects.
use super::{
    nav::Side,
    reader::CostFacts,
    reference::{self, MINUTE_US, Model, Pack, Signal},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(super) mod acquisition;

#[derive(Debug, Serialize)]
pub struct SignalPrice {
    pub signal_id: String,
    pub venue: String,
    pub symbol: String,
    pub side: String,
    pub offset: String,
    pub received_us: i64,
    pub expected_lots: i64,
    pub priced_lots: i64,
    pub model: Model,
    pub window_minutes: usize,
    pub sample_open_us: Vec<i64>,
    pub sample_prices: Vec<f64>,
    pub sample_weights: Vec<f64>,
    pub theoretical_price: Option<f64>,
    pub virtual_turnover_cny: Option<f64>,
    pub virtual_commission_cny: Option<f64>,
    pub pending: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FillCost {
    pub venue: String,
    pub symbol: String,
    pub trading_day: String,
    pub trade_id: String,
    pub received_us: i64,
    pub session: Option<String>,
    pub order_ref: Option<String>,
    pub side: String,
    pub offset: String,
    pub lots: i64,
    pub actual_price: f64,
    pub contract_multiplier: Option<f64>,
    pub signal_id: Option<String>,
    pub theoretical_price: Option<f64>,
    pub actual_commission: Option<f64>,
    pub commission_currency: Option<String>,
    pub theoretical_commission_cny: Option<f64>,
    pub price_shortfall_cny: Option<f64>,
    pub fee_shortfall_cny: Option<f64>,
    pub total_shortfall_cny: Option<f64>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub replication: Option<super::sync::ReplicaStatus>,
    pub source_id: String,
    pub trading_day: String,
    pub generated_at_us: i64,
    pub reference_available: bool,
    pub reference_sha256: Option<String>,
    pub provenance: Option<String>,
    pub reason: Option<String>,
    pub price_basis: &'static str,
    pub market_data_error: Option<String>,
    pub window_clock: &'static str,
    pub complete: bool,
    pub fill_count: usize,
    pub priced_fill_count: usize,
    pub fee_compared_fill_count: usize,
    pub total_lots: i64,
    pub priced_lots: i64,
    /// Complete totals stay null when any factual fill lacks coverage.
    pub price_shortfall_cny: Option<f64>,
    pub fee_shortfall_cny: Option<f64>,
    pub total_shortfall_cny: Option<f64>,
    /// Explicit subtotals for covered fills, never presented as whole-account cost.
    pub covered_price_shortfall_cny: Option<f64>,
    pub covered_fee_shortfall_cny: Option<f64>,
    pub signals: Vec<SignalPrice>,
    pub fills: Vec<FillCost>,
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

struct ReferenceIndex<'a> {
    contracts: BTreeMap<(&'a str, &'a str), &'a reference::Contract>,
    sessions: BTreeMap<(&'a str, &'a str), Vec<(i64, i64)>>,
    bars: BTreeMap<(&'a str, &'a str, i64), &'a reference::Bar>,
}
impl<'a> ReferenceIndex<'a> {
    fn new(pack: &'a Pack) -> Result<Self> {
        let contracts = pack
            .contracts
            .iter()
            .map(|c| ((c.venue.as_str(), c.symbol.as_str()), c))
            .collect();
        let mut sessions: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for s in &pack.sessions {
            sessions
                .entry((s.venue.as_str(), s.symbol.as_str()))
                .or_default()
                .push((
                    reference::timestamp(&s.start)?,
                    reference::timestamp(&s.end)?,
                ));
        }
        for windows in sessions.values_mut() {
            windows.sort_unstable();
        }
        let bars = pack
            .bars
            .iter()
            .map(|b| {
                Ok((
                    (
                        b.venue.as_str(),
                        b.symbol.as_str(),
                        reference::timestamp(&b.open_time)?,
                    ),
                    b,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            contracts,
            sessions,
            bars,
        })
    }
}

fn signal_price(index: &ReferenceIndex<'_>, signal: &Signal, now_us: i64) -> Result<SignalPrice> {
    let received = reference::timestamp(&signal.received_at)?;
    let mut result = SignalPrice {
        signal_id: signal.id.clone(),
        venue: signal.venue.clone(),
        symbol: signal.symbol.clone(),
        side: signal.side.clone(),
        offset: signal.offset.clone(),
        received_us: received,
        expected_lots: signal.lots,
        priced_lots: 0,
        model: signal.model,
        window_minutes: signal.window_minutes,
        sample_open_us: vec![],
        sample_prices: vec![],
        sample_weights: vec![],
        theoretical_price: None,
        virtual_turnover_cny: None,
        virtual_commission_cny: None,
        pending: false,
        reason: None,
    };
    let fail = |mut r: SignalPrice, reason: &str| {
        r.reason = Some(reason.into());
        r
    };
    let market = (signal.venue.as_str(), signal.symbol.as_str());
    let Some(contract) = index.contracts.get(&market) else {
        return Ok(fail(result, "缺少合约乘数与价格精度"));
    };
    let first = received.div_euclid(MINUTE_US) * MINUTE_US
        + if received % MINUTE_US == 0 {
            0
        } else {
            MINUTE_US
        };
    let Some(sessions) = index.sessions.get(&market) else {
        return Ok(fail(result, "缺少该合约的交易时段"));
    };
    if received < sessions[0].0 || received >= sessions.last().unwrap().1 {
        return Ok(fail(result, "信号时间不在所声明交易日的时段范围内"));
    }
    for &(start, end) in sessions {
        let mut t = first.max(start);
        while t + MINUTE_US <= end && result.sample_open_us.len() < signal.window_minutes {
            result.sample_open_us.push(t);
            t += MINUTE_US;
        }
        if result.sample_open_us.len() == signal.window_minutes {
            break;
        }
    }
    if result.sample_open_us.len() != signal.window_minutes {
        return Ok(fail(result, "本交易日剩余交易分钟不足执行窗口"));
    }
    if result.sample_open_us.last().unwrap() + MINUTE_US > now_us {
        result.pending = true;
        return Ok(fail(result, "执行窗口尚未全部收盘"));
    }
    let mut volumes = vec![];
    for i in 0..result.sample_open_us.len() {
        let t = result.sample_open_us[i];
        let Some(bar) = index.bars.get(&(market.0, market.1, t)) else {
            return Ok(fail(result, &format!("缺少执行窗口分钟线：open_us={t}")));
        };
        if bar.volume_lots == 0.0 {
            return Ok(fail(result, "执行窗口包含零成交分钟，无法提供成交均价基准"));
        }
        result
            .sample_prices
            .push(bar.turnover / bar.volume_lots / contract.contract_multiplier);
        volumes.push(bar.volume_lots);
    }
    result.sample_weights = match signal.model {
        Model::Twap => vec![1.0 / signal.window_minutes as f64; signal.window_minutes],
        Model::Vwap => {
            let sum: f64 = volumes.iter().sum();
            volumes.iter().map(|v| v / sum).collect()
        }
    };
    let price: f64 = result
        .sample_prices
        .iter()
        .zip(&result.sample_weights)
        .map(|(p, w)| p * w)
        .sum();
    ensure!(
        price.is_finite() && price > 0.0,
        "nonfinite theoretical price"
    );
    result.theoretical_price = Some(price);
    let turnover = signal.lots as f64 * price * contract.contract_multiplier;
    ensure!(turnover.is_finite(), "nonfinite virtual turnover");
    result.virtual_turnover_cny = Some(turnover);
    result.virtual_commission_cny = contract
        .fees
        .get(&signal.offset)
        .map(|fee| fee.amount(signal.lots, price, contract.contract_multiplier));
    ensure!(
        result.virtual_commission_cny.is_none_or(f64::is_finite),
        "nonfinite virtual commission"
    );
    Ok(result)
}

/// Only the execution windows requested by explicit signals need downloading.
/// This uses the same calendar and alignment as pricing, including missing bars.
pub fn required_minutes(pack: &Pack, now_us: i64) -> Result<BTreeSet<(String, String, i64)>> {
    pack.validate()?;
    let index = ReferenceIndex::new(pack)?;
    let mut result = BTreeSet::new();
    for signal in &pack.signals {
        let price = signal_price(&index, signal, now_us)?;
        for t in price.sample_open_us {
            if t + MINUTE_US <= now_us {
                result.insert((signal.venue.clone(), signal.symbol.clone(), t));
            }
        }
    }
    Ok(result)
}

pub fn build(
    source: &str,
    day: &str,
    pack: Option<&Pack>,
    facts: CostFacts,
    now_us: i64,
) -> Result<Report> {
    reference::valid_day(day)?;
    if let Some(pack) = pack {
        pack.validate()?;
        ensure!(
            pack.source_id == source && pack.trading_day == day,
            "reference source/day mismatch"
        );
    }
    let reference = pack.map(ReferenceIndex::new).transpose()?;
    let mut signals = pack
        .zip(reference.as_ref())
        .map(|(p, index)| {
            p.signals
                .iter()
                .map(|s| signal_price(index, s, now_us))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let signal_indices: BTreeMap<_, _> = signals
        .iter()
        .enumerate()
        .map(|(i, s)| (s.signal_id.clone(), i))
        .collect();
    let mut orders = BTreeMap::new();
    for observed in &facts.orders {
        let order = &observed.order;
        ensure!(
            order.trading_day == day,
            "order outside requested trading day"
        );
        let key = (
            order.venue.as_str(),
            order.order.session.as_str(),
            order.order.order_ref.as_str(),
        );
        ensure!(
            orders.insert(key, order).is_none(),
            "duplicate complete native order identity"
        );
    }
    let links: BTreeMap<_, _> = pack
        .map(|p| {
            p.order_links
                .iter()
                .map(|l| {
                    (
                        (l.venue.as_str(), l.session.as_str(), l.order_ref.as_str()),
                        l.signal_id.as_str(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut fills = vec![];
    let mut identities = BTreeSet::new();
    for trade in &facts.fills {
        let (venue, symbol, trade_id) = (
            trade.venue.as_str(),
            trade.symbol.as_str(),
            trade.trade_id.as_str(),
        );
        let side = side_name(trade.side);
        let offset = trade.offset.key();
        ensure!(
            trade.trading_day == day && identities.insert((venue, symbol, trade_id, side)),
            "duplicate or out-of-day native fill"
        );
        let (lots, price) = (trade.lots, trade.price);
        let mut row = FillCost {
            venue: venue.into(),
            symbol: symbol.into(),
            trading_day: day.into(),
            trade_id: trade_id.into(),
            received_us: trade.received_us,
            session: Some(trade.order.session.clone()),
            order_ref: Some(trade.order.order_ref.clone()),
            side: side.into(),
            offset: offset.into(),
            lots,
            actual_price: price,
            contract_multiplier: None,
            signal_id: None,
            theoretical_price: None,
            // Exec records commissions in CNY; absent means unknown.
            actual_commission: trade.commission_cny,
            commission_currency: trade.commission_cny.map(|_| "CNY".to_owned()),
            theoretical_commission_cny: None,
            price_shortfall_cny: None,
            fee_shortfall_cny: None,
            total_shortfall_cny: None,
            reason: None,
        };
        let order_key = (
            venue,
            trade.order.session.as_str(),
            trade.order.order_ref.as_str(),
        );
        let contract = reference
            .as_ref()
            .and_then(|index| index.contracts.get(&(venue, symbol)));
        // Records carry no instrument rules; the maintained reference (or the
        // market-data multiplier filled in later) supplies the multiplier.
        row.contract_multiplier = contract.map(|c| c.contract_multiplier);
        row.signal_id = links.get(&order_key).map(|id| (*id).to_owned());
        let compare = || -> std::result::Result<(usize, f64, f64, Option<f64>), String> {
            let pack = pack.ok_or("尚未维护本账户／交易日的参考数据")?;
            let signal_id = links
                .get(&order_key)
                .ok_or("未提供该报单的显式信号参考关联")?;
            let index = *signal_indices.get(*signal_id).ok_or("信号参考不存在")?;
            let signal = &pack.signals[index];
            let order = orders.get(&order_key).ok_or("缺少完整身份对应的委托记录")?;
            if order.symbol != symbol
                || order.venue != venue
                || order.side != trade.side
                || order.offset != trade.offset
                || signal.symbol != symbol
                || signal.side != side
                || signal.offset != offset
            {
                return Err("参考信号、完整委托与成交的合约／方向／开平不一致".into());
            }
            let received = reference::timestamp(&signal.received_at).map_err(|e| e.to_string())?;
            if order.created_us < received || trade.received_us < received {
                return Err("委托或成交的接收时间早于参考信号".into());
            }
            let theoretical = signals[index].theoretical_price.ok_or_else(|| {
                signals[index]
                    .reason
                    .clone()
                    .unwrap_or("理论价格不可用".into())
            })?;
            let contract = contract.ok_or("缺少合约规则")?;
            let shortfall = if trade.side == Side::Buy { 1.0 } else { -1.0 }
                * lots as f64
                * contract.contract_multiplier
                * (price - theoretical);
            let fee = contract
                .fees
                .get(offset)
                .map(|f| f.amount(lots, theoretical, contract.contract_multiplier));
            if !shortfall.is_finite() || fee.is_some_and(|v| !v.is_finite()) {
                return Err("成本金额超出可计算范围".into());
            }
            Ok((index, theoretical, shortfall, fee))
        };
        match compare() {
            Ok((index, theoretical, shortfall, fee)) => {
                row.signal_id = Some(signals[index].signal_id.clone());
                signals[index].priced_lots = signals[index]
                    .priced_lots
                    .checked_add(lots)
                    .context("matched lots overflow")?;
                row.theoretical_price = Some(theoretical);
                row.price_shortfall_cny = Some(shortfall);
                row.theoretical_commission_cny = fee;
                row.fee_shortfall_cny = fee.zip(row.actual_commission).map(|(t, a)| a - t);
                row.total_shortfall_cny = row.fee_shortfall_cny.map(|f| shortfall + f);
                row.reason = if fee.is_none() {
                    Some("缺少对应开平类型的理论手续费规则".into())
                } else if row.actual_commission.is_none() {
                    Some("事实手续费未报告，保留未知".into())
                } else {
                    None
                };
            }
            Err(reason) => row.reason = Some(reason),
        }
        fills.push(row);
    }
    fills.sort_by(|a, b| {
        (&a.venue, &a.symbol, &a.trade_id, &a.side).cmp(&(
            &b.venue,
            &b.symbol,
            &b.trade_id,
            &b.side,
        ))
    });
    let covered = |extract: fn(&FillCost) -> Option<f64>| -> Result<Option<f64>> {
        let values: Vec<_> = fills.iter().filter_map(extract).collect();
        let sum: f64 = values.iter().sum();
        ensure!(sum.is_finite(), "nonfinite cost aggregate");
        Ok((!values.is_empty()).then_some(sum))
    };
    let priced = fills
        .iter()
        .filter(|f| f.price_shortfall_cny.is_some())
        .count();
    let fee_compared = fills
        .iter()
        .filter(|f| f.fee_shortfall_cny.is_some())
        .count();
    let covered_price = covered(|f| f.price_shortfall_cny)?;
    let covered_fee = covered(|f| f.fee_shortfall_cny)?;
    let whole_price = (priced == fills.len()).then_some(covered_price).flatten();
    let whole_fee = (fee_compared == fills.len())
        .then_some(covered_fee)
        .flatten();
    let sum_lots = |only_priced: bool| {
        fills
            .iter()
            .filter(|f| !only_priced || f.price_shortfall_cny.is_some())
            .try_fold(0_i64, |sum, f| {
                sum.checked_add(f.lots).context("total lots overflow")
            })
    };
    let total_lots = sum_lots(false)?;
    let priced_lots = sum_lots(true)?;
    let complete = pack.is_some()
        && !signals.is_empty()
        && signals.iter().all(|s| s.theoretical_price.is_some())
        && fills.iter().all(|f| f.total_shortfall_cny.is_some());
    Ok(Report {
        replication: facts.replication,
        source_id: source.into(),
        trading_day: day.into(),
        generated_at_us: now_us,
        reference_available: pack.is_some(),
        reference_sha256: pack
            .map(|p| serde_json::to_vec_pretty(p).map(|bytes| hex::encode(Sha256::digest(bytes))))
            .transpose()?,
        provenance: pack.map(|p| p.provenance.clone()),
        reason: if pack.is_none() {
            Some("尚未维护本账户／交易日的参考数据".into())
        } else if signals.is_empty() {
            Some("尚未维护信号参考".into())
        } else {
            None
        },
        price_basis: "minute_turnover_cny / (minute_volume_lots * contract_multiplier)",
        market_data_error: None,
        window_clock: "complete_trading_minutes_in_explicit_sessions_same_trading_day",
        complete,
        fill_count: fills.len(),
        priced_fill_count: priced,
        fee_compared_fill_count: fee_compared,
        total_lots,
        priced_lots,
        price_shortfall_cny: whole_price,
        fee_shortfall_cny: whole_fee,
        total_shortfall_cny: whole_price.zip(whole_fee).map(|(p, f)| p + f),
        covered_price_shortfall_cny: covered_price,
        covered_fee_shortfall_cny: covered_fee,
        signals,
        fills,
    })
}

#[cfg(test)]
mod tests;
