//! Native lots/offset FIFO NAV change. Initial gross inventory is explicit;
//! exchange equity, settlement cashflows and strategy ownership are not inferred.
use super::{reference, reference_data::AccountRates};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) mod attribution;
pub(crate) mod comparison;
pub mod service;
pub mod source;
pub mod store;

pub(crate) const MAX_WINDOW_US: i64 = 31 * 86_400 * 1_000_000;
const MINUTE_US: i64 = 60_000_000;

fn evaluation_times(start: i64, end: i64, changes: impl Iterator<Item = i64>) -> BTreeSet<i64> {
    let first_minute_end = (start.div_euclid(MINUTE_US) * MINUTE_US).saturating_add(MINUTE_US);
    let mut times = BTreeSet::from([end]);
    times.extend((first_minute_end..end).step_by(MINUTE_US as usize));
    times.extend(changes.filter(|&at| at > start && at < end));
    times
}

fn sample_tick(at: i64, end: i64, interval_us: Option<i64>) -> bool {
    at == end || interval_us.is_none_or(|interval| at % interval == 0)
}
pub(crate) type Market = (String, String);

pub(crate) fn market_name((venue, contract): &Market) -> String {
    format!("{venue}:{contract}")
}

pub(crate) fn select_markets(
    available: &BTreeSet<Market>,
    requested: &[String],
) -> Result<BTreeSet<Market>> {
    if requested.is_empty() {
        return Ok(available.clone());
    }
    requested
        .iter()
        .map(|name| {
            let mut matches = available.iter().filter(|key| {
                name.eq_ignore_ascii_case(&market_name(key)) || name.eq_ignore_ascii_case(&key.1)
            });
            let market = matches
                .next()
                .with_context(|| format!("所选区间无合约 {name} 的持仓或成交证据"))?;
            ensure!(
                matches.next().is_none(),
                "合约 {name} 对应多个交易所，请选择完整合约"
            );
            Ok(market.clone())
        })
        .collect()
}

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

/// Operator-stated opening gross inventory of one contract side.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub venue: String,
    pub symbol: String,
    pub side: Side,
    pub today: i64,
    pub yesterday: i64,
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
    /// Last `cn_trade_updates` key at the cut; later fills follow the anchor.
    pub trades_key: i64,
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
                && self.trades_key >= 0
                && !self.store_id.is_empty()
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
/// One factual exchange fill from `cn_trade_updates` (or, for strategy NAV,
/// one strategy's share of it from `cn_uniform_orders`).
#[derive(Debug, Clone)]
pub struct Fill {
    /// Record key time; orders fills with the same receipt time.
    pub key_us: i64,
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
    /// Exec's CNY commission for this record; unknown when absent.
    pub commission_cny: Option<f64>,
}

impl Fill {
    fn needs_reference_fee(&self) -> bool {
        self.commission_cny.is_none()
    }

    fn estimated_fee(
        &self,
        multiplier: f64,
        rates: Option<&[AccountRates]>,
    ) -> Result<Option<f64>> {
        if self.commission_cny.is_some() {
            return Ok(None);
        }
        let rate = rates
            .into_iter()
            .flatten()
            .filter(|r| r.observed_at_us <= self.received_us)
            .max_by_key(|r| r.observed_at_us);
        if let Some(rate) = rate {
            ensure!(
                rate.venue == self.venue
                    && rate.symbol == self.symbol
                    && rate.contract_multiplier == multiplier,
                "fee observation belongs to another market or multiplier"
            );
        }
        Ok(rate.and_then(|r| {
            r.estimated_fee(self.offset.key(), self.lots, self.price, self.received_us)
        }))
    }
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
    /// Opening lots: yesterday's have no day, today's carry the anchor's day.
    fn seed(&mut self, position: &InitialPosition, trading_day: &str) {
        if position.yesterday > 0 {
            self.position_mut(position.side).push_back(Lot {
                day: None,
                lots: position.yesterday,
                price: position.reference_price,
            });
        }
        if position.today > 0 {
            self.position_mut(position.side).push_back(Lot {
                day: Some(trading_day.to_owned()),
                lots: position.today,
                price: position.reference_price,
            });
        }
    }
}

/// Fills after the anchor cut in replay order.
fn after_anchor<'a>(anchor: &Anchor, fills: &'a [Fill], end_us: i64) -> Vec<&'a Fill> {
    let mut ordered: Vec<_> = fills
        .iter()
        .filter(|f| f.key_us > anchor.trades_key && f.received_us <= end_us)
        .collect();
    ordered.sort_by_key(|f| (f.received_us, f.key_us));
    ordered
}

/// Long/short lots per contract after replaying the factual fills on the
/// anchor's opening inventory with native open/close semantics.
pub(crate) fn inventory(anchor: &Anchor, fills: &[Fill]) -> Result<BTreeMap<Market, (i64, i64)>> {
    anchor.validate()?;
    let mut ledgers: BTreeMap<Market, MarketLedger> = BTreeMap::new();
    for p in &anchor.positions {
        ledgers
            .entry((p.venue.clone(), p.symbol.clone()))
            .or_default()
            .seed(p, &anchor.trading_day);
    }
    for fill in after_anchor(anchor, fills, i64::MAX) {
        ensure!(
            fill.source_id == anchor.source_id && fill.trading_day >= anchor.trading_day,
            "成交属于其他账户或早于起始快照的交易日"
        );
        let ledger = ledgers
            .entry((fill.venue.clone(), fill.symbol.clone()))
            .or_default();
        // Lots only; realized amounts are not used here.
        ledger.multiplier = 1.0;
        ledger.apply(fill)?;
    }
    Ok(ledgers
        .into_iter()
        .map(|(key, ledger)| (key, ledger.quantities()))
        .filter(|(_, (long, short))| *long != 0 || *short != 0)
        .collect())
}

#[derive(Debug, Clone, Default)]
pub struct MarketPrices {
    pub multiplier: f64,
    /// Provider-final completed-minute end -> factual close.
    pub closes: BTreeMap<i64, f64>,
    /// Shanghai seconds since midnight, end exclusive; may cross midnight.
    pub sessions: Vec<(u32, u32)>,
    /// Execution references by verified minute end, populated only for comparisons.
    pub(crate) references: BTreeMap<i64, ReferenceMinute>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ReferenceMinute {
    pub(crate) trading_day: u32,
    pub(crate) twap: Option<f64>,
    pub(crate) vwap: Option<f64>,
    pub(crate) volume_lots: f64,
    pub(crate) turnover_cny: f64,
}
impl MarketPrices {
    /// The close of the minute containing `at_us`, or the last earlier close
    /// across a gap without trading time. Sessions opening on Saturday or
    /// Sunday never trade (Friday's night session does, as part of Monday's
    /// trading day). After a trading day's final close the position keeps that
    /// day's last mark until the next trading day's first bar, so weekends and
    /// exchange holidays neither move a mark nor need one. The Manager has no
    /// holiday calendar: a holiday is only the absence of provider bars after
    /// a trading day's close, and a weekday gap inside a trading day fails.
    pub fn mark(&self, at_us: i64) -> Result<f64> {
        let requested = at_us.div_euclid(MINUTE_US) * MINUTE_US;
        let (time, price) = self
            .closes
            .range(..=requested)
            .next_back()
            .context("缺少持仓计价所需的已完成分钟")?;
        ensure!(*time % MINUTE_US == 0, "unaligned native close mark");
        if *time != requested {
            ensure!(
                !self.sessions.is_empty(),
                "缺少交易时段，不能判断行情缺口是否为休市"
            );
            ensure!(
                self.closes_trading_day(*time) || !self.trading_minutes_between(*time, requested),
                "缺少应有交易分钟或停市日历依据，不能跨越缺口沿用旧价"
            );
        }
        ensure!(
            price.is_finite() && *price > 0.0,
            "invalid native minute mark"
        );
        Ok(*price)
    }

    /// The minute ending at `minute_end` is the last minute of a trading day
    /// (the end of its latest day session).
    fn closes_trading_day(&self, minute_end: i64) -> bool {
        let close = self
            .sessions
            .iter()
            .filter(|(start, end)| start < end && *start < 18 * 3600)
            .map(|(_, end)| *end)
            .max();
        close.is_some_and(|close| shanghai_second(minute_end) == i64::from(close))
    }

    /// Whether a weekday session overlaps the open interval `(from, to)`.
    fn trading_minutes_between(&self, from: i64, to: i64) -> bool {
        const DAY_US: i64 = 86_400 * 1_000_000;
        let local = |at: i64| (at + 8 * 3600 * 1_000_000).div_euclid(DAY_US);
        // Night windows wrap into the next date; start a day early.
        (local(from) - 1..=local(to)).any(|date| {
            // 1970-01-01 was a Thursday: Monday..Friday are 0..5.
            let weekday = (date + 3).rem_euclid(7) < 5;
            let midnight = date * DAY_US - 8 * 3600 * 1_000_000;
            weekday
                && self.sessions.iter().any(|&(start, end)| {
                    let start = midnight + i64::from(start) * 1_000_000;
                    let mut end = midnight + i64::from(end) * 1_000_000;
                    if end <= start {
                        end += DAY_US;
                    }
                    from.max(start) < to.min(end)
                })
        })
    }
}

/// Shanghai seconds since midnight.
fn shanghai_second(at_us: i64) -> i64 {
    (at_us / 1_000_000 + 8 * 3600).rem_euclid(86_400)
}

#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub timestamp_us: i64,
    pub nav_before_fees_cny: f64,
    pub realized_cny: f64,
    pub floating_cny: f64,
    pub fees_cny: Option<f64>,
    pub nav_after_fees_cny: Option<f64>,
    pub gross_position_cny: f64,
    pub net_position_cny: f64,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Fees {
    pub fill_count: usize,
    pub actual_count: usize,
    pub estimated_count: usize,
    pub missing_count: usize,
    pub actual_known_cny: f64,
    pub estimated_known_cny: f64,
    pub volume_cny: f64,
}
impl Fees {
    fn add(&mut self, volume_cny: f64, actual: Option<f64>, estimated: Option<f64>) -> Result<()> {
        self.fill_count += 1;
        self.volume_cny += volume_cny;
        if let Some(fee) = actual {
            self.actual_count += 1;
            self.actual_known_cny += fee;
        } else if let Some(fee) = estimated {
            self.estimated_count += 1;
            self.estimated_known_cny += fee;
        } else {
            self.missing_count += 1;
        }
        ensure!(
            self.actual_known_cny.is_finite()
                && self.estimated_known_cny.is_finite()
                && (self.actual_known_cny + self.estimated_known_cny).is_finite()
                && self.volume_cny.is_finite(),
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
    pub initial_net_lots: i64,
    pub long_value_cny: f64,
    pub short_value_cny: f64,
    pub fees: Fees,
    pub summary: Point,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<Point>,
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
    pub sampled: bool,
    pub available_symbols: Vec<String>,
    pub selected_symbols: Vec<String>,
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
            sampled: false,
            available_symbols: vec![],
            selected_symbols: vec![],
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
#[derive(Clone, Copy, Debug, Default)]
struct Amounts {
    realized: f64,
    floating: f64,
    long_value: f64,
    short_value: f64,
}

impl std::ops::AddAssign for Amounts {
    fn add_assign(&mut self, other: Self) {
        self.realized += other.realized;
        self.floating += other.floating;
        self.long_value += other.long_value;
        self.short_value += other.short_value;
    }
}

fn valuation(
    ledgers: &BTreeMap<Market, MarketLedger>,
    prices: &BTreeMap<Market, MarketPrices>,
    at: i64,
) -> Result<BTreeMap<Market, Amounts>> {
    ledgers
        .iter()
        .map(|(key, ledger)| {
            let (long, short) = ledger.quantities();
            let mut amounts = Amounts {
                realized: ledger.realized,
                ..Default::default()
            };
            if long != 0 || short != 0 {
                let mark = prices.get(key).context("缺少该合约行情")?.mark(at)?;
                amounts.floating = ledger.floating(mark)?;
                amounts.long_value = long as f64 * ledger.multiplier * mark;
                amounts.short_value = short as f64 * ledger.multiplier * mark;
            }
            Ok((key.clone(), amounts))
        })
        .collect()
}

fn totals<'a>(values: impl Iterator<Item = &'a Amounts>) -> Amounts {
    values.fold(Amounts::default(), |mut result, value| {
        result += *value;
        result
    })
}

fn point(at: i64, value: Amounts, baseline: Amounts, fees: &Fees) -> Result<Point> {
    let realized = value.realized - baseline.realized;
    let floating = value.floating - baseline.floating;
    let nav = realized + floating;
    let fee = fees.total();
    let after_fee = fee.map(|fee| nav - fee);
    let gross = value.long_value + value.short_value;
    let net = value.long_value - value.short_value;
    ensure!(
        [realized, floating, nav, gross, net]
            .into_iter()
            .all(f64::is_finite)
            && after_fee.is_none_or(f64::is_finite),
        "native NAV total overflow"
    );
    Ok(Point {
        timestamp_us: at,
        nav_before_fees_cny: nav,
        realized_cny: realized,
        floating_cny: floating,
        fees_cny: fee,
        nav_after_fees_cny: after_fee,
        gross_position_cny: gross,
        net_position_cny: net,
    })
}

/// Preserve both endpoints and each bucket's extrema while replaying all changes.
struct Sampler {
    points: Vec<Point>,
    total: usize,
    width: usize,
    index: usize,
    min: Option<(usize, Point)>,
    max: Option<(usize, Point)>,
}

impl Sampler {
    fn new(total: usize, limit: usize) -> Self {
        let pairs = (limit - 2) / 2;
        let width = if total <= limit || pairs == 0 {
            0
        } else {
            (total - 2).div_ceil(pairs)
        };
        Self {
            points: Vec::new(),
            total,
            width,
            index: 0,
            min: None,
            max: None,
        }
    }

    fn add(&mut self, point: Point, limit: usize) {
        let index = self.index;
        self.index += 1;
        let last = index + 1 == self.total;
        if last {
            self.flush();
        }
        if index == 0 || last || self.total <= limit {
            self.points.push(point);
            return;
        }
        if self.width == 0 {
            return;
        }
        let nav = |p: &Point| p.nav_after_fees_cny.unwrap_or(p.nav_before_fees_cny);
        if self
            .min
            .as_ref()
            .is_none_or(|(_, old)| nav(&point) < nav(old))
        {
            self.min = Some((index, point.clone()));
        }
        if self
            .max
            .as_ref()
            .is_none_or(|(_, old)| nav(&point) > nav(old))
        {
            self.max = Some((index, point));
        }
        if index.is_multiple_of(self.width) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if let (Some((lo, low)), Some((hi, high))) = (self.min.take(), self.max.take()) {
            if lo <= hi {
                self.points.push(low);
                if hi != lo {
                    self.points.push(high);
                }
            } else {
                self.points.extend([high, low]);
            }
        }
    }
}

pub fn build(input: Inputs<'_>) -> Result<Report> {
    build_selected(input, &[], false, None)
}

fn available_markets(anchor: &Anchor, fills: &[Fill], end_us: i64) -> BTreeSet<Market> {
    anchor
        .positions
        .iter()
        .map(|p| (p.venue.clone(), p.symbol.clone()))
        .chain(
            fills
                .iter()
                .filter(|f| f.key_us > anchor.trades_key && f.received_us <= end_us)
                .map(|f| (f.venue.clone(), f.symbol.clone())),
        )
        .collect()
}

fn build_selected(
    input: Inputs<'_>,
    requested: &[String],
    curves: bool,
    interval_us: Option<i64>,
) -> Result<Report> {
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
    let available = available_markets(anchor, fills, end_us);
    let selected = select_markets(&available, requested)?;
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
        if !selected.contains(&key) {
            continue;
        }
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
        ledger.seed(p, &anchor.trading_day);
    }
    let mut ordered = after_anchor(anchor, fills, end_us);
    let mut identities = BTreeSet::new();
    for fill in &ordered {
        ensure!(
            fill.source_id == anchor.source_id && fill.trading_day >= anchor.trading_day,
            "成交属于其他账户或早于起始快照的交易日"
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
        if !selected.contains(&key) {
            continue;
        }
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
    ordered.retain(|fill| selected.contains(&(fill.venue.clone(), fill.symbol.clone())));
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
    let initial_lots: BTreeMap<_, _> = ledgers
        .iter()
        .map(|(key, ledger)| {
            let (long, short) = ledger.quantities();
            (key.clone(), long - short)
        })
        .collect();
    let baseline = totals(initial.values());
    let mut fees = Fees::default();
    let mut by_market: BTreeMap<_, Fees> = selected
        .iter()
        .map(|key| (key.clone(), Fees::default()))
        .collect();
    let ticks = evaluation_times(
        start_us,
        end_us,
        ordered[next..].iter().map(|fill| fill.received_us),
    );
    ensure!(ticks.len() <= 600_000, "native NAV sample budget exceeded");
    let tick_count = ticks
        .iter()
        .filter(|&&at| sample_tick(at, end_us, interval_us))
        .count()
        + usize::from(start_us < end_us);
    let mut portfolio = Sampler::new(tick_count, max_points);
    let mut market_curves: BTreeMap<_, _> = selected
        .iter()
        .filter(|_| curves)
        .map(|key| (key.clone(), Sampler::new(tick_count, max_points)))
        .collect();

    if start_us < end_us {
        portfolio.add(point(start_us, baseline, baseline, &fees)?, max_points);
        for (key, curve) in &mut market_curves {
            curve.add(
                point(start_us, initial[key], initial[key], &by_market[key])?,
                max_points,
            );
        }
    }
    for tick in ticks {
        while next < ordered.len() && ordered[next].received_us <= tick {
            let fill = ordered[next];
            let key = (fill.venue.clone(), fill.symbol.clone());
            let ledger = ledgers
                .get_mut(&key)
                .context("missing native market ledger")?;
            ledger.apply(fill)?;
            let estimated =
                fill.estimated_fee(ledger.multiplier, rates.get(&key).map(Vec::as_slice))?;
            let volume = fill.price * fill.lots as f64 * ledger.multiplier;
            fees.add(volume, fill.commission_cny, estimated)?;
            by_market
                .get_mut(&key)
                .context("missing native fee ledger")?
                .add(volume, fill.commission_cny, estimated)?;
            next += 1;
        }
        let values = valuation(&ledgers, prices, tick)?;
        if sample_tick(tick, end_us, interval_us) {
            portfolio.add(
                point(tick, totals(values.values()), baseline, &fees)?,
                max_points,
            );
            for (key, curve) in &mut market_curves {
                curve.add(
                    point(tick, values[key], initial[key], &by_market[key])?,
                    max_points,
                );
            }
        }
    }
    let final_values = valuation(&ledgers, prices, end_us)?;
    let summary = point(end_us, totals(final_values.values()), baseline, &fees)?;
    let markets = ledgers
        .into_iter()
        .map(|(key, ledger)| {
            let (long_lots, short_lots) = ledger.quantities();
            let value = final_values[&key];
            let fees = by_market
                .remove(&key)
                .context("missing terminal fee ledger")?;
            let summary = point(end_us, value, initial[&key], &fees)?;
            let points = market_curves
                .remove(&key)
                .map(|curve| curve.points)
                .unwrap_or_default();
            let (venue, symbol) = key.clone();
            Ok(MarketSummary {
                venue,
                symbol,
                long_lots,
                short_lots,
                realized_cny: summary.realized_cny,
                floating_cny: summary.floating_cny,
                nav_before_fees_cny: summary.nav_before_fees_cny,
                initial_net_lots: initial_lots[&key],
                long_value_cny: value.long_value,
                short_value_cny: value.short_value,
                fees,
                summary,
                points,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let points = portfolio.points;
    let sampled = points.len() < tick_count;
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
        sampled,
        available_symbols: available.iter().map(market_name).collect(),
        selected_symbols: selected.iter().map(market_name).collect(),
    })
}

#[cfg(test)]
mod tests;
