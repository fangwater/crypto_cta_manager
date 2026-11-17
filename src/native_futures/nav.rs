//! Native lots/offset FIFO NAV change. Initial gross inventory is explicit;
//! exchange equity, settlement cashflows and strategy ownership are not inferred.
use super::{reference, reference_data::AccountRates};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub mod service;
pub mod source;
pub mod store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}
impl Side {
    fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
    fn sign(self) -> f64 {
        if self == Self::Buy { 1.0 } else { -1.0 }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Offset {
    Open,
    Close,
    CloseToday,
    CloseYesterday,
}
impl Offset {
    pub fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
            Self::CloseToday => "close_today",
            Self::CloseYesterday => "close_yesterday",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InitialPosition {
    pub venue: String,
    pub symbol: String,
    pub side: Side,
    pub today: i64,
    pub yesterday: i64,
    pub reference_price: f64,
    pub contract_multiplier: f64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub source_id: String,
    pub snapshot_us: i64,
    pub trading_day: String,
    pub evidence_sequence: i64,
    pub evidence_sha256: String,
    pub store_id: String,
    pub positions: Vec<InitialPosition>,
    pub note: String,
}
impl Anchor {
    pub fn validate(&self) -> Result<()> {
        reference::valid_day(&self.trading_day)?;
        ensure!(
            !self.source_id.is_empty()
                && self.snapshot_us > 0
                && self.evidence_sequence > 0
                && !self.store_id.is_empty()
                && self.evidence_sha256.len() == 64
                && self.evidence_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                && !self.note.trim().is_empty()
                && self.note.len() <= 4096
                && self.positions.len() <= 512,
            "invalid native NAV anchor"
        );
        let mut unique = BTreeSet::new();
        for p in &self.positions {
            super::validate_market(&format!("{}:{}", p.venue, p.symbol))?;
            ensure!(
                p.today >= 0
                    && p.yesterday >= 0
                    && p.today.checked_add(p.yesterday).is_some_and(|q| q > 0)
                    && p.reference_price.is_finite()
                    && p.reference_price > 0.0
                    && p.contract_multiplier.is_finite()
                    && p.contract_multiplier > 0.0
                    && unique.insert((&p.venue, &p.symbol, p.side)),
                "invalid or duplicate native initial gross position"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct Fill {
    pub key: Vec<u8>,
    pub source_id: String,
    pub venue: String,
    pub symbol: String,
    pub trading_day: String,
    pub trade_id: String,
    pub side: Side,
    pub offset: Offset,
    pub lots: i64,
    pub price: f64,
    pub received_us: i64,
    pub evidence_sequence: i64,
    pub commission_cny: Option<f64>,
    /// Verified live report or an explicitly unambiguous projection. Recovery
    /// queries without an original UTC trade time cannot be placed after an anchor.
    pub live_origin: bool,
}
#[derive(Debug, Clone)]
struct Lot {
    day: Option<String>,
    lots: i64,
    price: f64,
}
#[derive(Debug, Clone, Default)]
struct MarketLedger {
    long: VecDeque<Lot>,
    short: VecDeque<Lot>,
    realized: f64,
    multiplier: f64,
}
impl MarketLedger {
    fn position(&self, side: Side) -> &VecDeque<Lot> {
        if side == Side::Buy {
            &self.long
        } else {
            &self.short
        }
    }
    fn position_mut(&mut self, side: Side) -> &mut VecDeque<Lot> {
        if side == Side::Buy {
            &mut self.long
        } else {
            &mut self.short
        }
    }
    fn apply(&mut self, fill: &Fill) -> Result<()> {
        if fill.offset == Offset::Open {
            self.position(fill.side)
                .iter()
                .try_fold(fill.lots, |n, p| {
                    n.checked_add(p.lots)
                        .context("native open lot quantity overflow")
                })?;
            self.position_mut(fill.side).push_back(Lot {
                day: Some(fill.trading_day.clone()),
                lots: fill.lots,
                price: fill.price,
            });
            return Ok(());
        }
        let held = fill.side.opposite();
        let eligible = |lot: &Lot| match fill.offset {
            Offset::CloseToday => lot.day.as_deref() == Some(fill.trading_day.as_str()),
            Offset::CloseYesterday => lot
                .day
                .as_deref()
                .is_none_or(|d| d < fill.trading_day.as_str()),
            _ => true,
        };
        let available = self
            .position(held)
            .iter()
            .filter(|p| eligible(p))
            .try_fold(0i64, |sum, p| {
                sum.checked_add(p.lots)
                    .context("native lot quantity overflow")
            })?;
        ensure!(
            available >= fill.lots,
            "native close exceeds matching source/market/side/date inventory; an initial position or earlier fill is missing"
        );
        let mut remaining = fill.lots;
        let mut realized = 0.0;
        for lot in self.position_mut(held) {
            if remaining == 0 {
                break;
            }
            if !eligible(lot) {
                continue;
            }
            let qty = remaining.min(lot.lots);
            realized += qty as f64 * (fill.price - lot.price) * held.sign();
            lot.lots -= qty;
            remaining -= qty;
        }
        self.position_mut(held).retain(|p| p.lots > 0);
        self.realized += realized * self.multiplier;
        ensure!(self.realized.is_finite(), "native realized PnL overflow");
        Ok(())
    }
    fn floating(&self, price: f64) -> Result<f64> {
        let value = self
            .long
            .iter()
            .map(|p| p.lots as f64 * (price - p.price))
            .sum::<f64>()
            + self
                .short
                .iter()
                .map(|p| p.lots as f64 * (p.price - price))
                .sum::<f64>();
        let value = value * self.multiplier;
        ensure!(value.is_finite(), "native floating PnL overflow");
        Ok(value)
    }
    fn quantities(&self) -> (i64, i64) {
        (
            self.long.iter().map(|p| p.lots).sum(),
            self.short.iter().map(|p| p.lots).sum(),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct MarketPrices {
    pub multiplier: f64,
    /// Provider-final completed-minute end -> factual close.
    pub closes: BTreeMap<i64, f64>,
    /// Shanghai seconds since midnight, end exclusive; may cross midnight.
    pub sessions: Vec<(u32, u32)>,
}
impl MarketPrices {
    pub fn mark(&self, at_us: i64) -> Result<f64> {
        let minute = at_us.div_euclid(60_000_000) * 60_000_000;
        let requested = minute;
        let (time, price) = self
            .closes
            .range(..=requested)
            .next_back()
            .context("缺少持仓计价所需的已完成分钟")?;
        ensure!(*time % 60_000_000 == 0, "unaligned native close mark");
        if *time != requested {
            ensure!(
                !self.sessions.is_empty(),
                "缺少交易时段，不能判断行情缺口是否为休市"
            );
            let span = (requested - *time) / 1_000_000;
            let begin = (*time / 1_000_000 + 8 * 3600).rem_euclid(86400);
            let finish = begin + span;
            let mut skipped_active = false;
            for (start, end) in &self.sessions {
                let segments = if start < end {
                    vec![(*start as i64, *end as i64)]
                } else {
                    vec![(*start as i64, 86400), (0, *end as i64)]
                };
                for (start, end) in segments {
                    if span >= 86400 {
                        skipped_active = true;
                        break;
                    }
                    for shift in [0, 86400] {
                        skipped_active |= begin.max(start + shift) < finish.min(end + shift);
                    }
                }
            }
            ensure!(
                !skipped_active,
                "缺少应有交易分钟或停市日历依据，不能跨越缺口沿用旧价"
            );
        }
        ensure!(
            price.is_finite() && *price > 0.0,
            "invalid native minute mark"
        );
        Ok(*price)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub timestamp_us: i64,
    pub nav_before_fees_cny: f64,
    pub realized_cny: f64,
    pub floating_cny: f64,
    pub fees_cny: Option<f64>,
    pub nav_after_fees_cny: Option<f64>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Fees {
    pub fill_count: usize,
    pub actual_count: usize,
    pub estimated_count: usize,
    pub missing_count: usize,
    pub actual_known_cny: f64,
    pub estimated_known_cny: f64,
}
impl Fees {
    fn add(&mut self, fill: &Fill, rates: Option<&AccountRates>) -> Result<()> {
        self.fill_count += 1;
        if let Some(fee) = fill.commission_cny {
            self.actual_count += 1;
            self.actual_known_cny += fee;
        } else if let Some(fee) = rates.and_then(|r| {
            r.estimated_fee(fill.offset.key(), fill.lots, fill.price, fill.received_us)
        }) {
            self.estimated_count += 1;
            self.estimated_known_cny += fee;
        } else {
            self.missing_count += 1;
        }
        ensure!(
            self.actual_known_cny.is_finite() && self.estimated_known_cny.is_finite(),
            "native fee sum overflow"
        );
        Ok(())
    }
    pub fn total(&self) -> Option<f64> {
        (self.missing_count == 0).then_some(self.actual_known_cny + self.estimated_known_cny)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct MarketSummary {
    pub venue: String,
    pub symbol: String,
    pub long_lots: i64,
    pub short_lots: i64,
    pub realized_cny: f64,
    pub floating_cny: f64,
    pub nav_before_fees_cny: f64,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub source_id: String,
    pub ready: bool,
    pub reason: Option<String>,
    pub currency: &'static str,
    pub valuation: &'static str,
    pub snapshot_us: Option<i64>,
    pub start_us: i64,
    pub end_us: i64,
    pub source_through_us: Option<i64>,
    pub fees: Fees,
    pub summary: Option<Point>,
    pub markets: Vec<MarketSummary>,
    pub points: Vec<Point>,
}
impl Report {
    pub fn unavailable(
        source: &str,
        anchor: i64,
        start: i64,
        end: i64,
        through: i64,
        reason: String,
    ) -> Self {
        Self {
            source_id: source.into(),
            ready: false,
            reason: Some(reason),
            currency: "CNY",
            valuation: "native_lot_fifo_completed_minute_close",
            snapshot_us: (anchor > 0).then_some(anchor),
            start_us: start,
            end_us: end,
            source_through_us: (through > 0).then_some(through),
            fees: Fees::default(),
            summary: None,
            markets: vec![],
            points: vec![],
        }
    }
}

pub struct Inputs<'a> {
    pub anchor: &'a Anchor,
    pub fills: &'a [Fill],
    pub prices: &'a BTreeMap<(String, String), MarketPrices>,
    pub rates: &'a BTreeMap<(String, String), Vec<AccountRates>>,
    pub start_us: i64,
    pub end_us: i64,
    pub source_through_us: i64,
    pub max_points: usize,
}
fn valuation(
    ledgers: &BTreeMap<(String, String), MarketLedger>,
    prices: &BTreeMap<(String, String), MarketPrices>,
    at: i64,
) -> Result<BTreeMap<(String, String), (f64, f64)>> {
    ledgers
        .iter()
        .map(|(key, ledger)| {
            let floating = if ledger.long.is_empty() && ledger.short.is_empty() {
                0.0
            } else {
                ledger.floating(prices.get(key).context("缺少该合约行情")?.mark(at)?)?
            };
            Ok((key.clone(), (ledger.realized, floating)))
        })
        .collect()
}
fn totals(values: &BTreeMap<(String, String), (f64, f64)>) -> (f64, f64) {
    values
        .values()
        .fold((0.0, 0.0), |(r, f), (a, b)| (r + a, f + b))
}
fn point(
    at: i64,
    values: &BTreeMap<(String, String), (f64, f64)>,
    baseline: (f64, f64),
    fees: &Fees,
) -> Point {
    let (realized, floating) = totals(values);
    let (r, f) = (realized - baseline.0, floating - baseline.1);
    Point {
        timestamp_us: at,
        nav_before_fees_cny: r + f,
        realized_cny: r,
        floating_cny: f,
        fees_cny: fees.total(),
        nav_after_fees_cny: fees.total().map(|v| r + f - v),
    }
}

pub fn build(input: Inputs<'_>) -> Result<Report> {
    let Inputs {
        anchor,
        fills,
        prices,
        rates,
        start_us,
        end_us,
        source_through_us,
        max_points,
    } = input;
    anchor.validate()?;
    ensure!(
        start_us >= anchor.snapshot_us
            && start_us <= end_us
            && end_us <= source_through_us
            && (2..=5000).contains(&max_points),
        "invalid native NAV window or execution history has not covered its end"
    );
    let mut ledgers: BTreeMap<(String, String), MarketLedger> = BTreeMap::new();
    for p in &anchor.positions {
        let key = (p.venue.clone(), p.symbol.clone());
        ensure!(
            prices
                .get(&key)
                .is_some_and(|v| v.multiplier == p.contract_multiplier),
            "起始持仓乘数与计价行情冲突或缺少行情"
        );
        let ledger = ledgers.entry(key).or_default();
        ensure!(
            ledger.multiplier == 0.0 || ledger.multiplier == p.contract_multiplier,
            "conflicting initial contract multiplier"
        );
        ledger.multiplier = p.contract_multiplier;
        if p.yesterday > 0 {
            ledger.position_mut(p.side).push_back(Lot {
                day: None,
                lots: p.yesterday,
                price: p.reference_price,
            });
        }
        if p.today > 0 {
            ledger.position_mut(p.side).push_back(Lot {
                day: Some(anchor.trading_day.clone()),
                lots: p.today,
                price: p.reference_price,
            });
        }
    }
    let mut ordered: Vec<_> = fills
        .iter()
        .filter(|f| f.evidence_sequence > anchor.evidence_sequence && f.received_us <= end_us)
        .collect();
    ordered.sort_by(|a, b| {
        a.received_us
            .cmp(&b.received_us)
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut identities = BTreeSet::new();
    for fill in &ordered {
        ensure!(
            fill.source_id == anchor.source_id
                && fill.live_origin
                && fill.received_us >= anchor.snapshot_us
                && fill.trading_day >= anchor.trading_day,
            "成交来源、历史补报或时间边界不明确，不能将恢复查询当作起始快照之后的新成交"
        );
        super::validate_market(&format!("{}:{}", fill.venue, fill.symbol))?;
        reference::valid_day(&fill.trading_day)?;
        ensure!(
            fill.lots > 0
                && fill.price.is_finite()
                && fill.price > 0.0
                && fill.commission_cny.is_none_or(f64::is_finite)
                && !fill.trade_id.is_empty()
                && identities.insert((
                    &fill.venue,
                    &fill.symbol,
                    &fill.trading_day,
                    &fill.trade_id,
                    fill.side
                )),
            "invalid or duplicate native NAV fill"
        );
        let key = (fill.venue.clone(), fill.symbol.clone());
        let multiplier = prices
            .get(&key)
            .context("缺少成交合约的已验证乘数和行情")?
            .multiplier;
        ensure!(
            multiplier.is_finite() && multiplier > 0.0,
            "invalid native contract multiplier"
        );
        let ledger = ledgers.entry(key).or_default();
        ensure!(
            ledger.multiplier == 0.0 || ledger.multiplier == multiplier,
            "initial and market contract multipliers conflict"
        );
        ledger.multiplier = multiplier;
    }
    let mut next = 0;
    while next < ordered.len() && ordered[next].received_us < start_us {
        let fill = ordered[next];
        ledgers
            .get_mut(&(fill.venue.clone(), fill.symbol.clone()))
            .unwrap()
            .apply(fill)?;
        next += 1;
    }
    let initial = valuation(&ledgers, prices, start_us)?;
    let baseline = totals(&initial);
    let mut fees = Fees::default();
    let mut points = vec![];
    if start_us < end_us {
        points.push(point(start_us, &initial, baseline, &fees));
    }
    let mut ticks = BTreeSet::from([end_us]);
    for market in prices.values() {
        ticks.extend(
            market
                .closes
                .range((
                    std::ops::Bound::Excluded(start_us),
                    std::ops::Bound::Included(end_us),
                ))
                .map(|(t, _)| *t),
        );
    }
    ticks.extend(
        ordered[next..]
            .iter()
            .filter(|f| f.received_us > start_us)
            .map(|f| f.received_us),
    );
    ensure!(ticks.len() <= 600_000, "native NAV sample budget exceeded");
    for tick in ticks {
        while next < ordered.len() && ordered[next].received_us <= tick {
            let fill = ordered[next];
            let key = (fill.venue.clone(), fill.symbol.clone());
            ledgers.get_mut(&key).unwrap().apply(fill)?;
            let observed = rates.get(&key).and_then(|v| {
                v.iter()
                    .filter(|r| r.observed_at_us <= fill.received_us)
                    .max_by_key(|r| r.observed_at_us)
            });
            if let Some(r) = observed {
                ensure!(
                    r.venue == fill.venue
                        && r.symbol == fill.symbol
                        && r.contract_multiplier == ledgers[&key].multiplier,
                    "fee observation belongs to another market or multiplier"
                );
            }
            fees.add(fill, observed)?;
            next += 1;
        }
        let values = valuation(&ledgers, prices, tick)?;
        points.push(point(tick, &values, baseline, &fees));
    }
    let final_values = valuation(&ledgers, prices, end_us)?;
    let summary = point(end_us, &final_values, baseline, &fees);
    ensure!(
        [
            summary.nav_before_fees_cny,
            summary.realized_cny,
            summary.floating_cny
        ]
        .iter()
        .all(|v| v.is_finite()),
        "native NAV sum overflow"
    );
    let markets = ledgers
        .iter()
        .map(|((venue, symbol), ledger)| {
            let key = (venue.clone(), symbol.clone());
            let (r, f) = final_values[&key];
            let (r0, f0) = initial.get(&key).copied().unwrap_or_default();
            let (long_lots, short_lots) = ledger.quantities();
            MarketSummary {
                venue: venue.clone(),
                symbol: symbol.clone(),
                long_lots,
                short_lots,
                realized_cny: r - r0,
                floating_cny: f - f0,
                nav_before_fees_cny: r + f - r0 - f0,
            }
        })
        .collect();
    // Retain endpoints and extrema per bucket; all underlying fills/minutes were
    // processed before presentation downsampling.
    if points.len() > max_points {
        let count = (max_points - 2) / 2;
        let width = (points.len() - 2).div_ceil(count.max(1));
        let mut selected = BTreeSet::from([0, points.len() - 1]);
        if count > 0 {
            for begin in (1..points.len() - 1).step_by(width) {
                let end = (begin + width).min(points.len() - 1);
                let key = |i: usize| {
                    points[i]
                        .nav_after_fees_cny
                        .unwrap_or(points[i].nav_before_fees_cny)
                };
                let lo = (begin..end)
                    .min_by(|a, b| key(*a).total_cmp(&key(*b)))
                    .unwrap();
                let hi = (begin..end)
                    .max_by(|a, b| key(*a).total_cmp(&key(*b)))
                    .unwrap();
                selected.insert(lo);
                selected.insert(hi);
            }
        }
        points = points
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| selected.contains(&i).then_some(p))
            .collect();
    }
    let reason = (fees.missing_count > 0).then(|| {
        "部分成交的 CNY 实收费用或开平费率依据不完整；保留费用前净值，费用后净值为未知".into()
    });
    Ok(Report {
        source_id: anchor.source_id.clone(),
        ready: true,
        reason,
        currency: "CNY",
        valuation: "native_lot_fifo_completed_minute_close",
        snapshot_us: Some(anchor.snapshot_us),
        start_us,
        end_us,
        source_through_us: Some(source_through_us),
        fees,
        summary: Some(summary),
        markets,
        points,
    })
}

#[cfg(test)]
mod tests;
