use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{AppConfig, SourceConfig};
use crate::model::{UniformOrderEvent, decode_uniform_order, venue_name};
use crate::rocks_source;
use crate::snapshot::PositionSnapshot;

pub type VenueMarkOverrides = BTreeMap<(String, i16), f64>;
pub type SourceMarkOverrides = BTreeMap<String, VenueMarkOverrides>;
pub type SourcePositionSnapshots = BTreeMap<String, PositionSnapshot>;

const NAV_TICK_INTERVAL_US: i64 = 15 * 60 * 1_000_000;
const BATCH_EXEC_FROM_KEY_PREFIX: &str = "batch_exec:";
const INITIAL_POSITION_STRATEGY: &str = "__initial_position__";
const UNATTRIBUTED_STRATEGY: &str = "__unattributed__";

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct NavTotals {
    pub fill_count: u64,
    pub volume_quote: f64,
    pub realized_pnl_before_fee_quote: f64,
    pub estimated_trading_fee_quote: f64,
    pub realized_pnl_after_fee_quote: f64,
    pub floating_pnl_quote: f64,
    pub nav_change_before_fee_quote: f64,
    pub nav_change_after_fee_quote: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkPriceSource {
    LatestFill,
    InitialSnapshot,
    Override,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitialReferencePriceSource {
    Configured,
    FirstFill,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VenueNavReport {
    pub venue_code: i16,
    pub venue: String,
    pub mark_price: f64,
    pub mark_price_source: MarkPriceSource,
    pub initial_quantity: f64,
    pub initial_reference_price: Option<f64>,
    pub initial_reference_price_source: Option<InitialReferencePriceSource>,
    pub long_quantity: f64,
    pub short_quantity: f64,
    pub net_quantity: f64,
    pub long_position_value_quote: f64,
    pub short_position_value_quote: f64,
    pub net_position_value_quote: f64,
    pub first_fill_ts_us: Option<i64>,
    pub last_fill_ts_us: Option<i64>,
    #[serde(flatten)]
    pub totals: NavTotals,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SymbolNavReport {
    pub symbol: String,
    pub venue_count: usize,
    pub initial_net_quantity: f64,
    pub long_quantity: f64,
    pub short_quantity: f64,
    pub net_quantity: f64,
    pub long_position_value_quote: f64,
    pub short_position_value_quote: f64,
    pub net_position_value_quote: f64,
    #[serde(flatten)]
    pub totals: NavTotals,
    pub venues: Vec<VenueNavReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceNavReport {
    pub source_id: String,
    pub account: String,
    pub configured_venue: String,
    pub estimated_fee_rate: f64,
    pub initial_position_snapshot_ts_us: Option<i64>,
    pub initial_position_count: usize,
    pub order_event_count: u64,
    pub ignored_at_or_before_snapshot_event_count: u64,
    pub ignored_non_fill_event_count: u64,
    pub first_fill_ts_us: Option<i64>,
    pub last_fill_ts_us: Option<i64>,
    #[serde(flatten)]
    pub totals: NavTotals,
    pub symbols: Vec<SymbolNavReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AggregateSymbolNavReport {
    pub symbol: String,
    pub source_count: usize,
    pub venue_count: usize,
    pub initial_net_quantity: f64,
    pub long_quantity: f64,
    pub short_quantity: f64,
    pub net_quantity: f64,
    pub long_position_value_quote: f64,
    pub short_position_value_quote: f64,
    pub net_position_value_quote: f64,
    #[serde(flatten)]
    pub totals: NavTotals,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AggregateNavReport {
    #[serde(flatten)]
    pub totals: NavTotals,
    pub symbols: Vec<AggregateSymbolNavReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NavReport {
    pub valuation: &'static str,
    pub source_count: usize,
    pub aggregate: AggregateNavReport,
    pub sources: Vec<SourceNavReport>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct NavTimelinePoint {
    pub ts_us: i64,
    #[serde(flatten)]
    pub totals: NavTotals,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SymbolNavTimeline {
    pub symbol: String,
    pub points: Vec<NavTimelinePoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StrategyNavTimeline {
    pub strategy: String,
    pub symbol_count: usize,
    pub gross_position_value_quote: f64,
    pub net_position_value_quote: f64,
    pub summary: NavTotals,
    pub points: Vec<NavTimelinePoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NavTimelineReport {
    pub valuation: &'static str,
    pub earliest_start_ts_us: i64,
    pub start_ts_us: i64,
    pub end_ts_us: i64,
    pub selected_source_ids: Vec<String>,
    pub available_symbols: Vec<String>,
    pub selected_symbols: Vec<String>,
    pub available_strategies: Vec<String>,
    pub summary: NavTotals,
    pub symbols: Vec<AggregateSymbolNavReport>,
    pub points: Vec<NavTimelinePoint>,
    pub symbol_points: Vec<SymbolNavTimeline>,
    pub strategy_points: Vec<StrategyNavTimeline>,
    pub sampled: bool,
}

#[derive(Clone, Debug)]
pub struct NavTimelineRequest {
    pub start_ts_us: Option<i64>,
    pub end_ts_us: i64,
    pub selected_source_ids: Vec<String>,
    pub selected_symbols: Vec<String>,
    pub max_points: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Debug)]
struct PreparedFill {
    event: UniformOrderEvent,
    side: Side,
    fill_ts_us: i64,
}

#[derive(Clone, Debug)]
struct PreparedSourceEvents {
    snapshot_ts_us: Option<i64>,
    initial_states: BTreeMap<(String, i16), VenueState>,
    fill_events: Vec<PreparedFill>,
    order_event_count: u64,
    ignored_at_or_before_snapshot_event_count: u64,
    ignored_non_fill_event_count: u64,
}

#[derive(Clone, Debug)]
enum TimelineEvent {
    Snapshot {
        source_index: usize,
        source_id: String,
        ts_us: i64,
        symbols: Vec<String>,
    },
    Fill {
        source_index: usize,
        source_id: String,
        fill: PreparedFill,
    },
}

impl TimelineEvent {
    fn ts_us(&self) -> i64 {
        match self {
            Self::Snapshot { ts_us, .. } => *ts_us,
            Self::Fill { fill, .. } => fill.fill_ts_us,
        }
    }

    fn source_index(&self) -> usize {
        match self {
            Self::Snapshot { source_index, .. } | Self::Fill { source_index, .. } => *source_index,
        }
    }

    fn source_id(&self) -> &str {
        match self {
            Self::Snapshot { source_id, .. } | Self::Fill { source_id, .. } => source_id,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Snapshot { .. } => 0,
            Self::Fill { .. } => 1,
        }
    }

    fn record_tie_breaker(&self) -> (&str, i64) {
        match self {
            Self::Snapshot { .. } => ("", i64::MIN),
            Self::Fill { fill, .. } => (&fill.event.record_key, fill.event.event_ts_us),
        }
    }

    fn affected_symbols(&self) -> Vec<&str> {
        match self {
            Self::Snapshot { symbols, .. } => symbols.iter().map(String::as_str).collect(),
            Self::Fill { fill, .. } => vec![fill.event.symbol.as_str()],
        }
    }
}

#[derive(Clone, Debug)]
struct TimelineSourceState {
    source_id: String,
    fee_rate: f64,
    snapshot_ts_us: Option<i64>,
    pending_initial_states: Option<BTreeMap<(String, i16), VenueState>>,
    pending_initial_strategy_states: Option<BTreeMap<(String, String, i16), VenueState>>,
    states: BTreeMap<(String, i16), VenueState>,
    strategy_states: BTreeMap<(String, String, i16), VenueState>,
    latest_marks: BTreeMap<(String, i16), f64>,
}

impl TimelineSourceState {
    fn activate_snapshot_at(&mut self, ts_us: i64) {
        if self
            .snapshot_ts_us
            .is_some_and(|snapshot_ts| snapshot_ts <= ts_us)
            && let Some(initial_states) = self.pending_initial_states.take()
        {
            self.latest_marks.extend(
                initial_states
                    .iter()
                    .map(|(key, state)| (key.clone(), state.latest_fill_price)),
            );
            self.states = initial_states;
        }
        if self
            .snapshot_ts_us
            .is_some_and(|snapshot_ts| snapshot_ts <= ts_us)
            && let Some(initial_states) = self.pending_initial_strategy_states.take()
        {
            self.strategy_states = initial_states;
        }
    }

    fn apply_fill(&mut self, fill: &PreparedFill) -> Result<()> {
        self.activate_snapshot_at(fill.fill_ts_us);
        let key = (fill.event.symbol.clone(), fill.event.venue_code);
        let state = self
            .states
            .entry(key.clone())
            .or_insert_with(|| VenueState::new(&fill.event));
        state
            .apply_fill(&fill.event, fill.side, self.fee_rate, fill.fill_ts_us)
            .with_context(|| {
                format!(
                    "failed to apply source {} record {}",
                    self.source_id, fill.event.record_key
                )
            })?;

        let strategy = strategy_from_from_key(&fill.event.from_key_text);
        let strategy_key = (strategy, fill.event.symbol.clone(), fill.event.venue_code);
        let strategy_state = self
            .strategy_states
            .entry(strategy_key)
            .or_insert_with(|| VenueState::new(&fill.event));
        strategy_state
            .apply_fill(&fill.event, fill.side, self.fee_rate, fill.fill_ts_us)
            .with_context(|| {
                format!(
                    "failed to apply strategy attribution for source {} record {}",
                    self.source_id, fill.event.record_key
                )
            })?;
        self.latest_marks.insert(key, fill.event.price);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct Lot {
    entry_price: f64,
    quantity: f64,
}

#[derive(Clone, Debug, Default)]
struct QuantityFifo {
    longs: VecDeque<Lot>,
    shorts: VecDeque<Lot>,
    realized_pnl: f64,
}

impl QuantityFifo {
    fn apply_fill(&mut self, side: Side, price: f64, quantity: f64) {
        let (realized_pnl, remaining_quantity) = match side {
            Side::Buy => close_fifo(&mut self.shorts, price, quantity, -1.0),
            Side::Sell => close_fifo(&mut self.longs, price, quantity, 1.0),
        };
        if remaining_quantity > 0.0 {
            let lot = Lot {
                entry_price: price,
                quantity: remaining_quantity,
            };
            match side {
                Side::Buy => self.longs.push_back(lot),
                Side::Sell => self.shorts.push_back(lot),
            }
        }
        self.realized_pnl += realized_pnl;
    }

    fn floating_pnl(&self, mark_price: f64) -> f64 {
        self.longs
            .iter()
            .map(|lot| (mark_price - lot.entry_price) * lot.quantity)
            .chain(
                self.shorts
                    .iter()
                    .map(|lot| (lot.entry_price - mark_price) * lot.quantity),
            )
            .sum()
    }

    fn long_quantity(&self) -> f64 {
        self.longs.iter().map(|lot| lot.quantity).sum()
    }

    fn short_quantity(&self) -> f64 {
        self.shorts.iter().map(|lot| lot.quantity).sum()
    }
}

#[derive(Clone, Debug)]
struct VenueState {
    venue_code: i16,
    venue: String,
    fifo: QuantityFifo,
    initial_quantity: f64,
    initial_reference_price: Option<f64>,
    initial_reference_price_source: Option<InitialReferencePriceSource>,
    fill_count: u64,
    volume_quote: f64,
    estimated_fee_quote: f64,
    first_fill_ts_us: Option<i64>,
    last_fill_ts_us: Option<i64>,
    latest_fill_price: f64,
}

#[derive(Clone, Debug)]
struct FirstFillReference {
    price: f64,
    venue: String,
}

impl VenueState {
    fn new(event: &UniformOrderEvent) -> Self {
        Self {
            venue_code: event.venue_code,
            venue: event.venue.clone(),
            fifo: QuantityFifo::default(),
            initial_quantity: 0.0,
            initial_reference_price: None,
            initial_reference_price_source: None,
            fill_count: 0,
            volume_quote: 0.0,
            estimated_fee_quote: 0.0,
            first_fill_ts_us: None,
            last_fill_ts_us: None,
            latest_fill_price: event.price,
        }
    }

    fn from_initial_position(
        venue_code: i16,
        venue: String,
        quantity: f64,
        reference_price: f64,
        reference_price_source: InitialReferencePriceSource,
    ) -> Self {
        let mut fifo = QuantityFifo::default();
        let side = if quantity > 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };
        fifo.apply_fill(side, reference_price, quantity.abs());
        Self {
            venue_code,
            venue,
            fifo,
            initial_quantity: quantity,
            initial_reference_price: Some(reference_price),
            initial_reference_price_source: Some(reference_price_source),
            fill_count: 0,
            volume_quote: 0.0,
            estimated_fee_quote: 0.0,
            first_fill_ts_us: None,
            last_fill_ts_us: None,
            latest_fill_price: reference_price,
        }
    }

    fn apply_fill(
        &mut self,
        event: &UniformOrderEvent,
        side: Side,
        fee_rate: f64,
        fill_ts_us: i64,
    ) -> Result<()> {
        if self.venue != event.venue {
            bail!(
                "venue code {} changed name from {:?} to {:?}",
                self.venue_code,
                self.venue,
                event.venue
            );
        }
        let notional = event.price * event.amount_update;
        let fee = notional * fee_rate;
        if !notional.is_finite() || !fee.is_finite() {
            bail!("fill notional or estimated fee overflowed");
        }

        self.fifo.apply_fill(side, event.price, event.amount_update);
        self.fill_count = self
            .fill_count
            .checked_add(1)
            .context("fill count overflowed u64")?;
        self.volume_quote += notional;
        self.estimated_fee_quote += fee;
        if !self.volume_quote.is_finite() || !self.estimated_fee_quote.is_finite() {
            bail!("cumulative volume or estimated fee overflowed");
        }
        self.first_fill_ts_us.get_or_insert(fill_ts_us);
        self.last_fill_ts_us = Some(fill_ts_us);
        self.latest_fill_price = event.price;
        Ok(())
    }

    fn report(&self, mark_override: Option<f64>) -> VenueNavReport {
        let (mark_price, mark_price_source) = match mark_override {
            Some(mark_price) => (mark_price, MarkPriceSource::Override),
            None if self.fill_count > 0 => (self.latest_fill_price, MarkPriceSource::LatestFill),
            None => (self.latest_fill_price, MarkPriceSource::InitialSnapshot),
        };
        let floating_pnl = self.fifo.floating_pnl(mark_price);
        let long_quantity = self.fifo.long_quantity();
        let short_quantity = self.fifo.short_quantity();
        let net_quantity = long_quantity - short_quantity;
        let realized_before_fee = self.fifo.realized_pnl;
        let realized_after_fee = realized_before_fee - self.estimated_fee_quote;
        let nav_before_fee = realized_before_fee + floating_pnl;
        let nav_after_fee = nav_before_fee - self.estimated_fee_quote;

        VenueNavReport {
            venue_code: self.venue_code,
            venue: self.venue.clone(),
            mark_price: clean_zero(mark_price),
            mark_price_source,
            initial_quantity: clean_zero(self.initial_quantity),
            initial_reference_price: self.initial_reference_price.map(clean_zero),
            initial_reference_price_source: self.initial_reference_price_source,
            long_quantity: clean_zero(long_quantity),
            short_quantity: clean_zero(short_quantity),
            net_quantity: clean_zero(net_quantity),
            long_position_value_quote: clean_zero(long_quantity * mark_price),
            short_position_value_quote: clean_zero(short_quantity * mark_price),
            net_position_value_quote: clean_zero(net_quantity * mark_price),
            first_fill_ts_us: self.first_fill_ts_us,
            last_fill_ts_us: self.last_fill_ts_us,
            totals: NavTotals {
                fill_count: self.fill_count,
                volume_quote: clean_zero(self.volume_quote),
                realized_pnl_before_fee_quote: clean_zero(realized_before_fee),
                estimated_trading_fee_quote: clean_zero(self.estimated_fee_quote),
                realized_pnl_after_fee_quote: clean_zero(realized_after_fee),
                floating_pnl_quote: clean_zero(floating_pnl),
                nav_change_before_fee_quote: clean_zero(nav_before_fee),
                nav_change_after_fee_quote: clean_zero(nav_after_fee),
            },
        }
    }
}

#[derive(Default)]
struct SymbolReportBuilder {
    totals: NavTotals,
    initial_net_quantity: f64,
    long_quantity: f64,
    short_quantity: f64,
    long_position_value_quote: f64,
    short_position_value_quote: f64,
    net_position_value_quote: f64,
    venues: Vec<VenueNavReport>,
}

impl SymbolReportBuilder {
    fn push(&mut self, venue: VenueNavReport) {
        self.totals.add(venue.totals);
        self.initial_net_quantity += venue.initial_quantity;
        self.long_quantity += venue.long_quantity;
        self.short_quantity += venue.short_quantity;
        self.long_position_value_quote += venue.long_position_value_quote;
        self.short_position_value_quote += venue.short_position_value_quote;
        self.net_position_value_quote += venue.net_position_value_quote;
        self.venues.push(venue);
    }

    fn finish(self, symbol: String) -> SymbolNavReport {
        SymbolNavReport {
            symbol,
            venue_count: self.venues.len(),
            initial_net_quantity: clean_zero(self.initial_net_quantity),
            long_quantity: clean_zero(self.long_quantity),
            short_quantity: clean_zero(self.short_quantity),
            net_quantity: clean_zero(self.long_quantity - self.short_quantity),
            long_position_value_quote: clean_zero(self.long_position_value_quote),
            short_position_value_quote: clean_zero(self.short_position_value_quote),
            net_position_value_quote: clean_zero(self.net_position_value_quote),
            totals: self.totals.cleaned(),
            venues: self.venues,
        }
    }
}

#[derive(Default)]
struct AggregateSymbolBuilder {
    source_count: usize,
    venue_count: usize,
    totals: NavTotals,
    initial_net_quantity: f64,
    long_quantity: f64,
    short_quantity: f64,
    long_position_value_quote: f64,
    short_position_value_quote: f64,
    net_position_value_quote: f64,
}

impl AggregateSymbolBuilder {
    fn push(&mut self, symbol: &SymbolNavReport) {
        self.source_count += 1;
        self.venue_count += symbol.venue_count;
        self.totals.add(symbol.totals);
        self.initial_net_quantity += symbol.initial_net_quantity;
        self.long_quantity += symbol.long_quantity;
        self.short_quantity += symbol.short_quantity;
        self.long_position_value_quote += symbol.long_position_value_quote;
        self.short_position_value_quote += symbol.short_position_value_quote;
        self.net_position_value_quote += symbol.net_position_value_quote;
    }

    fn finish(self, symbol: String) -> AggregateSymbolNavReport {
        AggregateSymbolNavReport {
            symbol,
            source_count: self.source_count,
            venue_count: self.venue_count,
            initial_net_quantity: clean_zero(self.initial_net_quantity),
            long_quantity: clean_zero(self.long_quantity),
            short_quantity: clean_zero(self.short_quantity),
            net_quantity: clean_zero(self.long_quantity - self.short_quantity),
            long_position_value_quote: clean_zero(self.long_position_value_quote),
            short_position_value_quote: clean_zero(self.short_position_value_quote),
            net_position_value_quote: clean_zero(self.net_position_value_quote),
            totals: self.totals.cleaned(),
        }
    }
}

impl NavTotals {
    fn add(&mut self, other: Self) {
        self.fill_count = self.fill_count.saturating_add(other.fill_count);
        self.volume_quote += other.volume_quote;
        self.realized_pnl_before_fee_quote += other.realized_pnl_before_fee_quote;
        self.estimated_trading_fee_quote += other.estimated_trading_fee_quote;
        self.realized_pnl_after_fee_quote += other.realized_pnl_after_fee_quote;
        self.floating_pnl_quote += other.floating_pnl_quote;
        self.nav_change_before_fee_quote += other.nav_change_before_fee_quote;
        self.nav_change_after_fee_quote += other.nav_change_after_fee_quote;
    }

    fn cleaned(mut self) -> Self {
        self.volume_quote = clean_zero(self.volume_quote);
        self.realized_pnl_before_fee_quote = clean_zero(self.realized_pnl_before_fee_quote);
        self.estimated_trading_fee_quote = clean_zero(self.estimated_trading_fee_quote);
        self.realized_pnl_after_fee_quote = clean_zero(self.realized_pnl_after_fee_quote);
        self.floating_pnl_quote = clean_zero(self.floating_pnl_quote);
        self.nav_change_before_fee_quote = clean_zero(self.nav_change_before_fee_quote);
        self.nav_change_after_fee_quote = clean_zero(self.nav_change_after_fee_quote);
        self
    }

    fn difference(self, baseline: Self) -> Self {
        Self {
            fill_count: self.fill_count.saturating_sub(baseline.fill_count),
            volume_quote: self.volume_quote - baseline.volume_quote,
            realized_pnl_before_fee_quote: self.realized_pnl_before_fee_quote
                - baseline.realized_pnl_before_fee_quote,
            estimated_trading_fee_quote: self.estimated_trading_fee_quote
                - baseline.estimated_trading_fee_quote,
            realized_pnl_after_fee_quote: self.realized_pnl_after_fee_quote
                - baseline.realized_pnl_after_fee_quote,
            floating_pnl_quote: self.floating_pnl_quote - baseline.floating_pnl_quote,
            nav_change_before_fee_quote: self.nav_change_before_fee_quote
                - baseline.nav_change_before_fee_quote,
            nav_change_after_fee_quote: self.nav_change_after_fee_quote
                - baseline.nav_change_after_fee_quote,
        }
        .cleaned()
    }
}

fn prepare_source_events(
    source: &SourceConfig,
    events: impl IntoIterator<Item = UniformOrderEvent>,
    snapshot: Option<&PositionSnapshot>,
) -> Result<PreparedSourceEvents> {
    if let Some(snapshot) = snapshot {
        snapshot.validate()?;
        if snapshot.source_id != source.id {
            bail!(
                "position snapshot source {} does not match configured source {}",
                snapshot.source_id,
                source.id
            );
        }
    }

    let mut events = events.into_iter().collect::<Vec<_>>();
    events.sort_by(|left, right| {
        fifo_ts_us(left)
            .cmp(&fifo_ts_us(right))
            .then_with(|| left.event_ts_us.cmp(&right.event_ts_us))
            .then_with(|| left.record_key.cmp(&right.record_key))
    });

    let order_event_count = u64::try_from(events.len()).context("order event count exceeds u64")?;
    let snapshot_ts_us = snapshot.map(|value| value.snapshot_ts_us);
    let mut ignored_at_or_before_snapshot_event_count = 0_u64;
    let mut ignored_non_fill_event_count = 0_u64;
    let mut fill_events = Vec::new();
    let mut first_fill_references = BTreeMap::<(String, i16), FirstFillReference>::new();
    for event in events {
        if snapshot_ts_us.is_some_and(|snapshot_ts_us| fifo_ts_us(&event) <= snapshot_ts_us) {
            ignored_at_or_before_snapshot_event_count = ignored_at_or_before_snapshot_event_count
                .checked_add(1)
                .context("pre-snapshot event count overflowed u64")?;
            continue;
        }
        let Some(side) = validated_fill_side(source, &event)? else {
            ignored_non_fill_event_count = ignored_non_fill_event_count
                .checked_add(1)
                .context("ignored event count overflowed u64")?;
            continue;
        };
        let fill_ts_us = fifo_ts_us(&event);
        first_fill_references
            .entry((event.symbol.clone(), event.venue_code))
            .or_insert_with(|| FirstFillReference {
                price: event.price,
                venue: event.venue.clone(),
            });
        fill_events.push(PreparedFill {
            event,
            side,
            fill_ts_us,
        });
    }

    let mut initial_states = BTreeMap::<(String, i16), VenueState>::new();
    for position in snapshot
        .map(|value| value.positions.as_slice())
        .unwrap_or_default()
    {
        let key = (position.symbol.clone(), position.venue_code);
        let first_fill = first_fill_references.get(&key);
        let (reference_price, reference_price_source) = match position.reference_price {
            Some(price) => {
                validate_positive(price, "initial position reference price").with_context(
                    || {
                        format!(
                            "source {} initial position {} venue {} is invalid",
                            source.id, position.symbol, position.venue_code
                        )
                    },
                )?;
                (price, InitialReferencePriceSource::Configured)
            }
            None => {
                let first_fill = first_fill.with_context(|| {
                    format!(
                        "source {} initial position {} venue {} needs reference_price because it has no fill",
                        source.id, position.symbol, position.venue_code
                    )
                })?;
                (first_fill.price, InitialReferencePriceSource::FirstFill)
            }
        };
        let venue = first_fill
            .map(|fill| fill.venue.clone())
            .unwrap_or_else(|| venue_name(position.venue_code as u8));
        if initial_states
            .insert(
                key,
                VenueState::from_initial_position(
                    position.venue_code,
                    venue,
                    position.quantity,
                    reference_price,
                    reference_price_source,
                ),
            )
            .is_some()
        {
            bail!(
                "source {} has duplicate initial position for {} venue {}",
                source.id,
                position.symbol,
                position.venue_code
            );
        }
    }

    Ok(PreparedSourceEvents {
        snapshot_ts_us,
        initial_states,
        fill_events,
        order_event_count,
        ignored_at_or_before_snapshot_event_count,
        ignored_non_fill_event_count,
    })
}

pub fn estimate_source_events(
    source: &SourceConfig,
    events: impl IntoIterator<Item = UniformOrderEvent>,
    mark_overrides: &VenueMarkOverrides,
) -> Result<SourceNavReport> {
    estimate_source_events_with_snapshot(source, events, mark_overrides, None)
}

pub fn estimate_source_events_with_snapshot(
    source: &SourceConfig,
    events: impl IntoIterator<Item = UniformOrderEvent>,
    mark_overrides: &VenueMarkOverrides,
    snapshot: Option<&PositionSnapshot>,
) -> Result<SourceNavReport> {
    let fee_rate = source.nav_fee_rate()?;
    validate_fee_rate(fee_rate, &source.id)?;
    for ((symbol, venue_code), mark_price) in mark_overrides {
        validate_positive(*mark_price, "mark price").with_context(|| {
            format!(
                "invalid mark override for source {} symbol {} venue {}",
                source.id, symbol, venue_code
            )
        })?;
    }

    let PreparedSourceEvents {
        snapshot_ts_us,
        initial_states: mut states,
        fill_events,
        order_event_count,
        ignored_at_or_before_snapshot_event_count,
        ignored_non_fill_event_count,
    } = prepare_source_events(source, events, snapshot)?;

    let mut first_fill_ts_us = None;
    let mut last_fill_ts_us = None;
    for fill in fill_events {
        let key = (fill.event.symbol.clone(), fill.event.venue_code);
        let state = states
            .entry(key)
            .or_insert_with(|| VenueState::new(&fill.event));
        state
            .apply_fill(&fill.event, fill.side, fee_rate, fill.fill_ts_us)
            .with_context(|| {
                format!(
                    "failed to apply source {} record {}",
                    source.id, fill.event.record_key
                )
            })?;
        first_fill_ts_us.get_or_insert(fill.fill_ts_us);
        last_fill_ts_us = Some(fill.fill_ts_us);
    }

    for key in mark_overrides.keys() {
        if !states.contains_key(key) {
            bail!(
                "mark override for source {} symbol {} venue {} has no position or fills",
                source.id,
                key.0,
                key.1
            );
        }
    }

    let mut symbol_builders = BTreeMap::<String, SymbolReportBuilder>::new();
    for ((symbol, venue_code), state) in states {
        let mark_override = mark_overrides.get(&(symbol.clone(), venue_code)).copied();
        symbol_builders
            .entry(symbol)
            .or_default()
            .push(state.report(mark_override));
    }

    let symbols = symbol_builders
        .into_iter()
        .map(|(symbol, builder)| builder.finish(symbol))
        .collect::<Vec<_>>();
    let mut totals = NavTotals::default();
    for symbol in &symbols {
        totals.add(symbol.totals);
    }

    Ok(SourceNavReport {
        source_id: source.id.clone(),
        account: source.account.clone(),
        configured_venue: source.venue.clone(),
        estimated_fee_rate: fee_rate,
        initial_position_snapshot_ts_us: snapshot_ts_us,
        initial_position_count: snapshot.map_or(0, |value| value.positions.len()),
        order_event_count,
        ignored_at_or_before_snapshot_event_count,
        ignored_non_fill_event_count,
        first_fill_ts_us,
        last_fill_ts_us,
        totals: totals.cleaned(),
        symbols,
    })
}

fn validated_fill_side(source: &SourceConfig, event: &UniformOrderEvent) -> Result<Option<Side>> {
    if !event.amount_update.is_finite() || event.amount_update < 0.0 {
        bail!(
            "source {} record {} has invalid amount_update {}",
            source.id,
            event.record_key,
            event.amount_update
        );
    }
    if event.amount_update == 0.0 {
        return Ok(None);
    }
    validate_positive(event.price, "fill price").with_context(|| {
        format!(
            "source {} record {} cannot be applied",
            source.id, event.record_key
        )
    })?;
    if event.symbol.trim().is_empty() {
        bail!(
            "source {} record {} has an empty symbol",
            source.id,
            event.record_key
        );
    }
    match event.side_code {
        1 => Ok(Some(Side::Buy)),
        2 => Ok(Some(Side::Sell)),
        value => bail!(
            "source {} record {} has unsupported side code {}",
            source.id,
            event.record_key,
            value
        ),
    }
}

pub fn rebuild_nav_from_rocksdb(
    config: &AppConfig,
    selected_source_ids: &[String],
) -> Result<NavReport> {
    rebuild_nav_from_rocksdb_with_marks(config, selected_source_ids, &SourceMarkOverrides::new())
}

pub fn rebuild_nav_from_rocksdb_with_marks(
    config: &AppConfig,
    selected_source_ids: &[String],
    mark_overrides: &SourceMarkOverrides,
) -> Result<NavReport> {
    rebuild_nav_from_rocksdb_with_inputs(
        config,
        selected_source_ids,
        mark_overrides,
        &SourcePositionSnapshots::new(),
    )
}

pub fn rebuild_nav_from_rocksdb_with_snapshots(
    config: &AppConfig,
    selected_source_ids: &[String],
    snapshots: &SourcePositionSnapshots,
) -> Result<NavReport> {
    rebuild_nav_from_rocksdb_with_inputs(
        config,
        selected_source_ids,
        &SourceMarkOverrides::new(),
        snapshots,
    )
}

pub fn rebuild_nav_timeline_from_rocksdb_with_snapshots(
    config: &AppConfig,
    request: NavTimelineRequest,
    snapshots: &SourcePositionSnapshots,
) -> Result<NavTimelineReport> {
    if request.end_ts_us < 0 {
        bail!("end timestamp must not be negative");
    }
    let selected = select_sources(config, &request.selected_source_ids)?;
    let selected_ids = selected
        .iter()
        .map(|source| source.id.as_str())
        .collect::<BTreeSet<_>>();
    for source_id in snapshots.keys() {
        if !selected_ids.contains(source_id.as_str()) {
            bail!("position snapshots contain unselected source {source_id}");
        }
    }

    let mut runtimes = Vec::<TimelineSourceState>::with_capacity(selected.len());
    let mut timeline_events = Vec::<TimelineEvent>::new();
    let mut available_symbols = BTreeSet::<String>::new();
    let mut available_strategies = BTreeSet::<String>::new();
    let mut source_anchors = Vec::<i64>::new();
    let selected_source_ids = selected
        .iter()
        .map(|source| source.id.clone())
        .collect::<Vec<_>>();

    for source in selected {
        let fee_rate = source.nav_fee_rate()?;
        validate_fee_rate(fee_rate, &source.id)?;
        let records = rocks_source::read_uniform_orders(&source.rocksdb_path, 0, i64::MAX)
            .with_context(|| format!("failed to read source {} RocksDB", source.id))?;
        let mut events = Vec::with_capacity(records.len());
        for record in records {
            events.push(
                decode_uniform_order(&record.key, &record.value).with_context(|| {
                    format!(
                        "source {} contains an undecodable uniform order at key {:?}",
                        source.id,
                        String::from_utf8_lossy(&record.key)
                    )
                })?,
            );
        }
        let prepared = prepare_source_events(source, events, snapshots.get(&source.id))
            .with_context(|| format!("failed to prepare source {} timeline", source.id))?;
        let first_fill_ts_us = prepared
            .fill_events
            .iter()
            .find(|fill| fill.fill_ts_us <= request.end_ts_us)
            .map(|fill| fill.fill_ts_us);
        let snapshot_ts_us = prepared
            .snapshot_ts_us
            .filter(|snapshot_ts| *snapshot_ts <= request.end_ts_us);
        if let Some(anchor) = snapshot_ts_us.or(first_fill_ts_us) {
            source_anchors.push(anchor);
        }

        let source_index = runtimes.len();
        let initial_symbols = prepared
            .initial_states
            .keys()
            .map(|(symbol, _)| symbol.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(snapshot_ts_us) = snapshot_ts_us {
            available_symbols.extend(initial_symbols.iter().cloned());
            if !prepared.initial_states.is_empty() {
                available_strategies.insert(INITIAL_POSITION_STRATEGY.to_string());
            }
            timeline_events.push(TimelineEvent::Snapshot {
                source_index,
                source_id: source.id.clone(),
                ts_us: snapshot_ts_us,
                symbols: initial_symbols,
            });
        }
        for fill in prepared.fill_events {
            if fill.fill_ts_us <= request.end_ts_us {
                available_symbols.insert(fill.event.symbol.clone());
                available_strategies.insert(strategy_from_from_key(&fill.event.from_key_text));
                timeline_events.push(TimelineEvent::Fill {
                    source_index,
                    source_id: source.id.clone(),
                    fill,
                });
            }
        }
        let initial_strategy_states = prepared
            .initial_states
            .iter()
            .map(|((symbol, venue_code), state)| {
                (
                    (
                        INITIAL_POSITION_STRATEGY.to_string(),
                        symbol.clone(),
                        *venue_code,
                    ),
                    state.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        runtimes.push(TimelineSourceState {
            source_id: source.id.clone(),
            fee_rate,
            snapshot_ts_us: prepared.snapshot_ts_us,
            pending_initial_states: prepared.snapshot_ts_us.map(|_| prepared.initial_states),
            pending_initial_strategy_states: prepared
                .snapshot_ts_us
                .map(|_| initial_strategy_states),
            states: BTreeMap::new(),
            strategy_states: BTreeMap::new(),
            latest_marks: BTreeMap::new(),
        });
    }

    let earliest_start_ts_us = source_anchors
        .into_iter()
        .min()
        .unwrap_or(request.end_ts_us);
    let start_ts_us = request.start_ts_us.unwrap_or(earliest_start_ts_us);
    if start_ts_us < earliest_start_ts_us {
        bail!(
            "start timestamp must be greater than or equal to earliest source timestamp {earliest_start_ts_us}"
        );
    }
    if request.end_ts_us < start_ts_us {
        bail!("end timestamp must be greater than or equal to start timestamp");
    }

    let available_symbols = available_symbols.into_iter().collect::<Vec<_>>();
    let available_strategies = available_strategies.into_iter().collect::<Vec<_>>();
    let selected_symbols =
        select_timeline_symbols(&available_symbols, request.selected_symbols.into_iter())?;
    let selected_symbol_set = selected_symbols.iter().cloned().collect::<BTreeSet<_>>();

    timeline_events.sort_by(|left, right| {
        let left_tie_breaker = left.record_tie_breaker();
        let right_tie_breaker = right.record_tie_breaker();
        left.ts_us()
            .cmp(&right.ts_us())
            .then_with(|| left.rank().cmp(&right.rank()))
            .then_with(|| left.source_id().cmp(right.source_id()))
            .then_with(|| left_tie_breaker.1.cmp(&right_tie_breaker.1))
            .then_with(|| left_tie_breaker.0.cmp(right_tie_breaker.0))
    });

    for runtime in &mut runtimes {
        runtime.activate_snapshot_at(start_ts_us);
    }
    let split = timeline_events.partition_point(|event| event.ts_us() < start_ts_us);
    for event in &timeline_events[..split] {
        apply_timeline_event(&mut runtimes, event)?;
    }

    let baselines = timeline_totals_by_symbol(&runtimes);
    let strategy_baselines = timeline_totals_by_strategy_symbol(&runtimes);
    let mut points = vec![timeline_point(
        start_ts_us,
        &runtimes,
        &baselines,
        &selected_symbol_set,
    )];
    let mut points_by_symbol = selected_symbols
        .iter()
        .cloned()
        .map(|symbol| {
            let point = timeline_symbol_point(start_ts_us, &symbol, &runtimes, &baselines);
            (symbol, vec![point])
        })
        .collect::<BTreeMap<_, _>>();
    let mut points_by_strategy = available_strategies
        .iter()
        .cloned()
        .map(|strategy| {
            let point = timeline_strategy_point(
                start_ts_us,
                &strategy,
                &runtimes,
                &strategy_baselines,
                &selected_symbol_set,
            );
            (strategy, vec![point])
        })
        .collect::<BTreeMap<_, _>>();

    for event in &timeline_events[split..] {
        if event.ts_us() > request.end_ts_us {
            break;
        }
        apply_timeline_event(&mut runtimes, event)?;
        let affected_symbols = event.affected_symbols();
        if affected_symbols
            .iter()
            .any(|symbol| selected_symbol_set.contains(*symbol))
        {
            push_or_replace_timeline_point(
                &mut points,
                timeline_point(event.ts_us(), &runtimes, &baselines, &selected_symbol_set),
            );
            for symbol in affected_symbols {
                if let Some(symbol_points) = points_by_symbol.get_mut(symbol) {
                    push_or_replace_timeline_point(
                        symbol_points,
                        timeline_symbol_point(event.ts_us(), symbol, &runtimes, &baselines),
                    );
                }
            }
            for (strategy, strategy_points) in &mut points_by_strategy {
                push_or_replace_timeline_point(
                    strategy_points,
                    timeline_strategy_point(
                        event.ts_us(),
                        strategy,
                        &runtimes,
                        &strategy_baselines,
                        &selected_symbol_set,
                    ),
                );
            }
        }
    }
    for runtime in &mut runtimes {
        runtime.activate_snapshot_at(request.end_ts_us);
    }
    push_or_replace_timeline_point(
        &mut points,
        timeline_point(
            request.end_ts_us,
            &runtimes,
            &baselines,
            &selected_symbol_set,
        ),
    );
    for symbol in &selected_symbols {
        if let Some(symbol_points) = points_by_symbol.get_mut(symbol) {
            push_or_replace_timeline_point(
                symbol_points,
                timeline_symbol_point(request.end_ts_us, symbol, &runtimes, &baselines),
            );
        }
    }
    for (strategy, strategy_points) in &mut points_by_strategy {
        push_or_replace_timeline_point(
            strategy_points,
            timeline_strategy_point(
                request.end_ts_us,
                strategy,
                &runtimes,
                &strategy_baselines,
                &selected_symbol_set,
            ),
        );
    }

    let current_totals = timeline_totals_by_symbol(&runtimes);
    let mut symbols = timeline_symbol_reports(&runtimes);
    for symbol in &mut symbols {
        let current = current_totals
            .get(&symbol.symbol)
            .copied()
            .unwrap_or_default();
        let baseline = baselines.get(&symbol.symbol).copied().unwrap_or_default();
        symbol.totals = current.difference(baseline);
    }
    let mut summary = NavTotals::default();
    for symbol in symbols
        .iter()
        .filter(|symbol| selected_symbol_set.contains(&symbol.symbol))
    {
        summary.add(symbol.totals);
    }
    summary = summary.cleaned();

    let points =
        resample_timeline_points(points, start_ts_us, request.end_ts_us, NAV_TICK_INTERVAL_US);
    let resampled_point_count = points.len();
    let points = downsample_timeline_points(points, request.max_points.max(2));
    let mut sampled = points.len() < resampled_point_count;
    let symbol_max_points = request.max_points.clamp(100, 800);
    let symbol_points = selected_symbols
        .iter()
        .map(|symbol| {
            let raw = points_by_symbol.remove(symbol).unwrap_or_default();
            let resampled =
                resample_timeline_points(raw, start_ts_us, request.end_ts_us, NAV_TICK_INTERVAL_US);
            let resampled_len = resampled.len();
            let points = downsample_timeline_points(resampled, symbol_max_points);
            sampled |= points.len() < resampled_len;
            SymbolNavTimeline {
                symbol: symbol.clone(),
                points,
            }
        })
        .collect();
    let strategy_points = available_strategies
        .iter()
        .map(|strategy| {
            let raw = points_by_strategy.remove(strategy).unwrap_or_default();
            let resampled =
                resample_timeline_points(raw, start_ts_us, request.end_ts_us, NAV_TICK_INTERVAL_US);
            let resampled_len = resampled.len();
            let points = downsample_timeline_points(resampled, symbol_max_points);
            sampled |= points.len() < resampled_len;
            let (symbol_count, gross_position_value_quote, net_position_value_quote) =
                timeline_strategy_position_values(strategy, &runtimes, &selected_symbol_set);
            StrategyNavTimeline {
                strategy: strategy.clone(),
                symbol_count,
                gross_position_value_quote,
                net_position_value_quote,
                summary: points.last().map(|point| point.totals).unwrap_or_default(),
                points,
            }
        })
        .collect();

    Ok(NavTimelineReport {
        valuation: "quantity_fifo_window_delta",
        earliest_start_ts_us,
        start_ts_us,
        end_ts_us: request.end_ts_us,
        selected_source_ids,
        available_symbols,
        selected_symbols,
        available_strategies,
        summary,
        symbols,
        points,
        symbol_points,
        strategy_points,
        sampled,
    })
}

fn rebuild_nav_from_rocksdb_with_inputs(
    config: &AppConfig,
    selected_source_ids: &[String],
    mark_overrides: &SourceMarkOverrides,
    snapshots: &SourcePositionSnapshots,
) -> Result<NavReport> {
    let selected = select_sources(config, selected_source_ids)?;
    let selected_ids = selected
        .iter()
        .map(|source| source.id.as_str())
        .collect::<BTreeSet<_>>();
    for source_id in mark_overrides.keys() {
        if !selected_ids.contains(source_id.as_str()) {
            bail!("mark overrides contain unselected source {source_id}");
        }
    }
    for source_id in snapshots.keys() {
        if !selected_ids.contains(source_id.as_str()) {
            bail!("position snapshots contain unselected source {source_id}");
        }
    }

    let empty_marks = VenueMarkOverrides::new();
    let mut sources = Vec::with_capacity(selected.len());
    for source in selected {
        let records = rocks_source::read_uniform_orders(&source.rocksdb_path, 0, i64::MAX)
            .with_context(|| format!("failed to read source {} RocksDB", source.id))?;
        let mut events = Vec::with_capacity(records.len());
        for record in records {
            let event = decode_uniform_order(&record.key, &record.value).with_context(|| {
                format!(
                    "source {} contains an undecodable uniform order at key {:?}",
                    source.id,
                    String::from_utf8_lossy(&record.key)
                )
            })?;
            events.push(event);
        }
        let source_marks = mark_overrides.get(&source.id).unwrap_or(&empty_marks);
        sources.push(estimate_source_events_with_snapshot(
            source,
            events,
            source_marks,
            snapshots.get(&source.id),
        )?);
    }
    Ok(aggregate_source_reports(sources))
}

fn apply_timeline_event(runtimes: &mut [TimelineSourceState], event: &TimelineEvent) -> Result<()> {
    let runtime = &mut runtimes[event.source_index()];
    match event {
        TimelineEvent::Snapshot { ts_us, .. } => {
            runtime.activate_snapshot_at(*ts_us);
            Ok(())
        }
        TimelineEvent::Fill { fill, .. } => runtime.apply_fill(fill),
    }
}

fn select_timeline_symbols(
    available_symbols: &[String],
    requested_symbols: impl IntoIterator<Item = String>,
) -> Result<Vec<String>> {
    let available = available_symbols.iter().cloned().collect::<BTreeSet<_>>();
    let mut requested = requested_symbols
        .into_iter()
        .map(|symbol| symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();
    requested.sort();
    requested.dedup();
    if requested.is_empty() {
        return Ok(available_symbols.to_vec());
    }
    let selected = requested
        .into_iter()
        .filter(|symbol| available.contains(symbol))
        .collect::<Vec<_>>();
    if selected.is_empty() && !available_symbols.is_empty() {
        bail!("none of the requested symbols exist in the selected time range");
    }
    Ok(selected)
}

fn timeline_totals_by_symbol(runtimes: &[TimelineSourceState]) -> BTreeMap<String, NavTotals> {
    let mut totals = BTreeMap::<String, NavTotals>::new();
    for runtime in runtimes {
        for ((symbol, _), state) in &runtime.states {
            totals
                .entry(symbol.clone())
                .or_default()
                .add(state.report(None).totals);
        }
    }
    totals
}

fn timeline_totals_by_strategy_symbol(
    runtimes: &[TimelineSourceState],
) -> BTreeMap<(String, String), NavTotals> {
    let mut totals = BTreeMap::<(String, String), NavTotals>::new();
    for runtime in runtimes {
        for ((strategy, symbol, venue_code), state) in &runtime.strategy_states {
            let mark = runtime
                .latest_marks
                .get(&(symbol.clone(), *venue_code))
                .copied();
            totals
                .entry((strategy.clone(), symbol.clone()))
                .or_default()
                .add(state.report(mark).totals);
        }
    }
    totals
}

fn timeline_strategy_position_values(
    strategy: &str,
    runtimes: &[TimelineSourceState],
    selected_symbols: &BTreeSet<String>,
) -> (usize, f64, f64) {
    let mut symbols = BTreeSet::new();
    let mut gross_position_value_quote = 0.0;
    let mut net_position_value_quote = 0.0;
    for runtime in runtimes {
        for ((state_strategy, symbol, venue_code), state) in &runtime.strategy_states {
            if state_strategy != strategy || !selected_symbols.contains(symbol) {
                continue;
            }
            let mark = runtime
                .latest_marks
                .get(&(symbol.clone(), *venue_code))
                .copied();
            let report = state.report(mark);
            if report.long_quantity.abs() > 1e-12 || report.short_quantity.abs() > 1e-12 {
                symbols.insert(symbol.clone());
            }
            gross_position_value_quote +=
                report.long_position_value_quote.abs() + report.short_position_value_quote.abs();
            net_position_value_quote += report.net_position_value_quote;
        }
    }
    (
        symbols.len(),
        clean_zero(gross_position_value_quote),
        clean_zero(net_position_value_quote),
    )
}

fn timeline_symbol_reports(runtimes: &[TimelineSourceState]) -> Vec<AggregateSymbolNavReport> {
    let mut aggregate = BTreeMap::<String, AggregateSymbolBuilder>::new();
    for runtime in runtimes {
        let mut source_symbols = BTreeMap::<String, SymbolReportBuilder>::new();
        for ((symbol, _), state) in &runtime.states {
            source_symbols
                .entry(symbol.clone())
                .or_default()
                .push(state.report(None));
        }
        for (symbol, builder) in source_symbols {
            aggregate
                .entry(symbol.clone())
                .or_default()
                .push(&builder.finish(symbol));
        }
    }
    aggregate
        .into_iter()
        .map(|(symbol, builder)| builder.finish(symbol))
        .collect()
}

fn timeline_point(
    ts_us: i64,
    runtimes: &[TimelineSourceState],
    baselines: &BTreeMap<String, NavTotals>,
    selected_symbols: &BTreeSet<String>,
) -> NavTimelinePoint {
    let current = timeline_totals_by_symbol(runtimes);
    let mut totals = NavTotals::default();
    for symbol in selected_symbols {
        totals.add(
            current
                .get(symbol)
                .copied()
                .unwrap_or_default()
                .difference(baselines.get(symbol).copied().unwrap_or_default()),
        );
    }
    NavTimelinePoint {
        ts_us,
        totals: totals.cleaned(),
    }
}

fn timeline_symbol_point(
    ts_us: i64,
    symbol: &str,
    runtimes: &[TimelineSourceState],
    baselines: &BTreeMap<String, NavTotals>,
) -> NavTimelinePoint {
    let current = timeline_totals_by_symbol(runtimes);
    NavTimelinePoint {
        ts_us,
        totals: current
            .get(symbol)
            .copied()
            .unwrap_or_default()
            .difference(baselines.get(symbol).copied().unwrap_or_default()),
    }
}

fn timeline_strategy_point(
    ts_us: i64,
    strategy: &str,
    runtimes: &[TimelineSourceState],
    baselines: &BTreeMap<(String, String), NavTotals>,
    selected_symbols: &BTreeSet<String>,
) -> NavTimelinePoint {
    let current = timeline_totals_by_strategy_symbol(runtimes);
    let mut totals = NavTotals::default();
    for symbol in selected_symbols {
        let key = (strategy.to_string(), symbol.clone());
        totals.add(
            current
                .get(&key)
                .copied()
                .unwrap_or_default()
                .difference(baselines.get(&key).copied().unwrap_or_default()),
        );
    }
    NavTimelinePoint {
        ts_us,
        totals: totals.cleaned(),
    }
}

fn strategy_from_from_key(from_key: &str) -> String {
    let Some(strategy) = from_key.strip_prefix(BATCH_EXEC_FROM_KEY_PREFIX) else {
        return UNATTRIBUTED_STRATEGY.to_string();
    };
    let valid = !strategy.is_empty()
        && strategy.len() <= 256
        && strategy
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        strategy.to_string()
    } else {
        UNATTRIBUTED_STRATEGY.to_string()
    }
}

fn push_or_replace_timeline_point(points: &mut Vec<NavTimelinePoint>, point: NavTimelinePoint) {
    if let Some(last) = points.last_mut()
        && last.ts_us == point.ts_us
    {
        *last = point;
        return;
    }
    points.push(point);
}

fn resample_timeline_points(
    points: Vec<NavTimelinePoint>,
    start_ts_us: i64,
    end_ts_us: i64,
    interval_us: i64,
) -> Vec<NavTimelinePoint> {
    if points.is_empty() || interval_us <= 0 || end_ts_us < start_ts_us {
        return points;
    }

    let mut sampled = Vec::with_capacity(
        usize::try_from((end_ts_us - start_ts_us) / interval_us + 2).unwrap_or(points.len()),
    );
    let mut source_index = 0;
    let mut current = points[0];
    let mut push_at = |ts_us: i64, sampled: &mut Vec<NavTimelinePoint>| {
        while source_index + 1 < points.len() && points[source_index + 1].ts_us <= ts_us {
            source_index += 1;
            current = points[source_index];
        }
        current.ts_us = ts_us;
        sampled.push(current);
    };

    push_at(start_ts_us, &mut sampled);
    let mut tick_ts_us = start_ts_us
        .div_euclid(interval_us)
        .saturating_add(1)
        .saturating_mul(interval_us);
    while tick_ts_us < end_ts_us {
        push_at(tick_ts_us, &mut sampled);
        let next_tick_ts_us = tick_ts_us.saturating_add(interval_us);
        if next_tick_ts_us == tick_ts_us {
            break;
        }
        tick_ts_us = next_tick_ts_us;
    }
    if end_ts_us > start_ts_us {
        push_at(end_ts_us, &mut sampled);
    }
    sampled
}

fn downsample_timeline_points(
    points: Vec<NavTimelinePoint>,
    max_points: usize,
) -> Vec<NavTimelinePoint> {
    const VALUE_SELECTORS: [fn(&NavTimelinePoint) -> f64; 4] = [
        |point| point.totals.nav_change_before_fee_quote,
        |point| point.totals.nav_change_after_fee_quote,
        |point| point.totals.realized_pnl_before_fee_quote,
        |point| point.totals.floating_pnl_quote,
    ];

    if points.len() <= max_points || max_points < 10 {
        return points;
    }
    let interior = &points[1..points.len() - 1];
    let bucket_count = ((max_points - 2) / (VALUE_SELECTORS.len() * 2)).max(1);
    let bucket_size = interior.len().div_ceil(bucket_count);
    let mut sampled = Vec::with_capacity(max_points);
    sampled.push(points[0]);
    for bucket in interior.chunks(bucket_size) {
        let mut extrema = Vec::with_capacity(VALUE_SELECTORS.len() * 2);
        for value in VALUE_SELECTORS {
            if let Some(point) = bucket.iter().min_by(|left, right| {
                value(left)
                    .partial_cmp(&value(right))
                    .unwrap_or(Ordering::Equal)
            }) {
                extrema.push(*point);
            }
            if let Some(point) = bucket.iter().max_by(|left, right| {
                value(left)
                    .partial_cmp(&value(right))
                    .unwrap_or(Ordering::Equal)
            }) {
                extrema.push(*point);
            }
        }
        extrema.sort_by_key(|point| point.ts_us);
        extrema.dedup_by_key(|point| point.ts_us);
        sampled.extend(extrema);
    }
    sampled.push(*points.last().expect("non-empty points"));
    sampled
}

fn select_sources<'a>(
    config: &'a AppConfig,
    selected_source_ids: &[String],
) -> Result<Vec<&'a SourceConfig>> {
    let requested = selected_source_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if requested.len() != selected_source_ids.len() {
        bail!("selected source IDs contain duplicates");
    }
    if !requested.is_empty() {
        for source_id in &requested {
            let source = config
                .sources
                .iter()
                .find(|source| source.id == *source_id)
                .with_context(|| format!("selected source {} is not configured", source_id))?;
            if !source.enabled {
                bail!("selected source {} is disabled", source_id);
            }
        }
    }

    let selected = config
        .sources
        .iter()
        .filter(|source| {
            source.enabled && (requested.is_empty() || requested.contains(source.id.as_str()))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no enabled sources selected for NAV reconstruction");
    }
    Ok(selected)
}

fn aggregate_source_reports(sources: Vec<SourceNavReport>) -> NavReport {
    let mut totals = NavTotals::default();
    let mut symbols = BTreeMap::<String, AggregateSymbolBuilder>::new();
    for source in &sources {
        totals.add(source.totals);
        for symbol in &source.symbols {
            symbols
                .entry(symbol.symbol.clone())
                .or_default()
                .push(symbol);
        }
    }
    let symbols = symbols
        .into_iter()
        .map(|(symbol, builder)| builder.finish(symbol))
        .collect();

    NavReport {
        valuation: "quantity_fifo",
        source_count: sources.len(),
        aggregate: AggregateNavReport {
            totals: totals.cleaned(),
            symbols,
        },
        sources,
    }
}

fn close_fifo(
    lots: &mut VecDeque<Lot>,
    close_price: f64,
    mut quantity: f64,
    direction: f64,
) -> (f64, f64) {
    let mut realized_pnl = 0.0;
    while quantity > 0.0 {
        let Some(lot) = lots.front_mut() else {
            break;
        };
        let matched_quantity = quantity.min(lot.quantity);
        realized_pnl += direction * (close_price - lot.entry_price) * matched_quantity;
        quantity -= matched_quantity;
        lot.quantity -= matched_quantity;
        if lot.quantity == 0.0 {
            lots.pop_front();
        }
    }
    (realized_pnl, quantity)
}

fn validate_fee_rate(value: f64, source_id: &str) -> Result<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        bail!("source {source_id} estimated_fee_rate must be finite and nonnegative")
    }
}

fn fifo_ts_us(event: &UniformOrderEvent) -> i64 {
    if event.update_ts_us > 0 {
        event.update_ts_us
    } else {
        event.event_ts_us
    }
}

fn validate_positive(value: f64, field: &str) -> Result<()> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        bail!("{field} must be finite and positive, got {value}")
    }
}

fn clean_zero(value: f64) -> f64 {
    if value.abs() < 1e-12 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rocksdb::{ColumnFamilyDescriptor, DB, Options};

    use super::*;
    use crate::config::{DatabaseConfig, IngestionConfig, OrderConfigSettings};
    use crate::model::UNIFORM_ORDERS_CF;
    use crate::snapshot::SnapshotPosition;

    fn source(id: &str, fee_rate: Option<f64>) -> SourceConfig {
        SourceConfig {
            id: id.to_string(),
            account: id.to_string(),
            venue: "multi-venue".to_string(),
            rocksdb_path: PathBuf::from(format!("/tmp/{id}/persist_manager")),
            enabled: true,
            start_ts_us: None,
            poll_interval_secs: None,
            estimated_fee_rate: fee_rate,
            gateway_prefix: None,
            exec_config_url: None,
            ipc_namespace: None,
            account_ipc_service: None,
            share_unit_usdt: None,
        }
    }

    fn position_snapshot(
        source_id: &str,
        snapshot_ts_us: i64,
        quantity: f64,
        reference_price: Option<f64>,
    ) -> PositionSnapshot {
        PositionSnapshot {
            source_id: source_id.to_string(),
            snapshot_ts_us,
            positions: vec![SnapshotPosition {
                symbol: "BTCUSDT".to_string(),
                venue_code: 1,
                quantity,
                reference_price,
            }],
        }
    }

    fn event(
        ts_us: i64,
        symbol: &str,
        venue_code: i16,
        side_code: i16,
        price: f64,
        quantity: f64,
    ) -> UniformOrderEvent {
        UniformOrderEvent {
            record_key: format!("{ts_us:020}"),
            event_ts_us: ts_us,
            recv_ts_us: ts_us,
            symbol: symbol.to_string(),
            create_ts_us: ts_us,
            update_ts_us: ts_us,
            signal_ts_us: ts_us,
            submit_ts_us: ts_us,
            local_ts_us: ts_us,
            market_ts_us: ts_us,
            client_order_id: ts_us,
            venue_code,
            venue: format!("venue-{venue_code}"),
            order_type_code: 1,
            order_type: "LIMIT".to_string(),
            side_code,
            side: if side_code == 1 { "BUY" } else { "SELL" }.to_string(),
            price,
            price_offset: 0.0,
            amount_initial: quantity,
            amount_update: quantity,
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

    fn encode_event(event: &UniformOrderEvent) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&event.recv_ts_us.to_le_bytes());
        payload.extend_from_slice(&(event.symbol.len() as u16).to_le_bytes());
        payload.extend_from_slice(event.symbol.as_bytes());
        for value in [
            event.create_ts_us,
            event.update_ts_us,
            event.signal_ts_us,
            event.submit_ts_us,
            event.local_ts_us,
            event.market_ts_us,
            event.client_order_id,
        ] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&[
            event.venue_code as u8,
            event.order_type_code as u8,
            event.side_code as u8,
        ]);
        for value in [
            event.price,
            event.price_offset,
            event.amount_initial,
            event.amount_update,
        ] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.push(event.status_code as u8);
        payload.extend_from_slice(&(event.from_key.len() as u32).to_le_bytes());
        payload.extend_from_slice(&event.from_key);
        payload
    }

    fn event_at(
        record_ts_us: i64,
        fill_ts_us: i64,
        symbol: &str,
        venue_code: i16,
        side_code: i16,
        price: f64,
        quantity: f64,
    ) -> UniformOrderEvent {
        let mut event = event(record_ts_us, symbol, venue_code, side_code, price, quantity);
        event.update_ts_us = fill_ts_us;
        event
    }

    fn strategy_event_at(
        record_ts_us: i64,
        fill_ts_us: i64,
        symbol: &str,
        venue_code: i16,
        side_code: i16,
        price: f64,
        quantity: f64,
        strategy: &str,
    ) -> UniformOrderEvent {
        let mut event = event_at(
            record_ts_us,
            fill_ts_us,
            symbol,
            venue_code,
            side_code,
            price,
            quantity,
        );
        event.from_key_text = format!("batch_exec:{strategy}");
        event.from_key = event.from_key_text.as_bytes().to_vec();
        event
    }

    fn write_events(path: &std::path::Path, events: &[UniformOrderEvent]) {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db = DB::open_cf_descriptors(
            &options,
            path,
            vec![ColumnFamilyDescriptor::new(
                UNIFORM_ORDERS_CF,
                Options::default(),
            )],
        )
        .unwrap();
        let column_family = db.cf_handle(UNIFORM_ORDERS_CF).unwrap();
        for event in events {
            db.put_cf(
                column_family,
                event.record_key.as_bytes(),
                encode_event(event),
            )
            .unwrap();
        }
    }

    fn app_config(sources: Vec<SourceConfig>) -> AppConfig {
        AppConfig {
            database: DatabaseConfig {
                url_env: "CTA_NAV_TEST_DATABASE_URL_MUST_NOT_BE_READ".to_string(),
                max_connections: 1,
            },
            ingestion: IngestionConfig::default(),
            order_config: OrderConfigSettings::default(),
            sources,
        }
    }

    fn source_at(id: &str, path: &std::path::Path, fee_rate: f64) -> SourceConfig {
        SourceConfig {
            rocksdb_path: path.to_path_buf(),
            ..source(id, Some(fee_rate))
        }
    }

    fn timeline_request(
        start_ts_us: i64,
        end_ts_us: i64,
        selected_source_ids: Vec<String>,
        selected_symbols: Vec<String>,
    ) -> NavTimelineRequest {
        NavTimelineRequest {
            start_ts_us: Some(start_ts_us),
            end_ts_us,
            selected_source_ids,
            selected_symbols,
            max_points: 3_000,
        }
    }

    fn estimate(events: Vec<UniformOrderEvent>) -> SourceNavReport {
        estimate_source_events(&source("trade01", Some(0.0)), events, &BTreeMap::new()).unwrap()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "actual={actual}, expected={expected}"
        );
    }

    #[test]
    fn quantity_fifo_closes_oldest_lots_and_handles_reversal() {
        let report = estimate(vec![
            event(1, "BTCUSDT", 1, 1, 100.0, 10.0),
            event(2, "BTCUSDT", 1, 2, 110.0, 15.0),
            event(3, "BTCUSDT", 1, 1, 90.0, 2.0),
        ]);
        let venue = &report.symbols[0].venues[0];

        assert_close(venue.totals.realized_pnl_before_fee_quote, 140.0);
        assert_close(venue.totals.floating_pnl_quote, 60.0);
        assert_close(venue.totals.nav_change_before_fee_quote, 200.0);
        assert_close(venue.long_quantity, 0.0);
        assert_close(venue.short_quantity, 3.0);
    }

    #[test]
    fn estimates_fees_on_every_fill_and_reports_before_and_after_fee() {
        let report = estimate_source_events(
            &source("trade01", Some(0.001)),
            vec![
                event(1, "BTCUSDT", 1, 1, 100.0, 1.0),
                event(2, "BTCUSDT", 1, 2, 110.0, 1.0),
            ],
            &BTreeMap::new(),
        )
        .unwrap();

        assert_close(report.totals.volume_quote, 210.0);
        assert_close(report.totals.realized_pnl_before_fee_quote, 10.0);
        assert_close(report.totals.estimated_trading_fee_quote, 0.21);
        assert_close(report.totals.realized_pnl_after_fee_quote, 9.79);
        assert_close(report.totals.nav_change_after_fee_quote, 9.79);
    }

    #[test]
    fn initial_position_seeds_fifo_without_volume_or_fee() {
        let source = source("trade01", Some(0.001));
        let snapshot = position_snapshot("trade01", 1, -2.0, Some(100.0));
        let report = estimate_source_events_with_snapshot(
            &source,
            vec![event(2, "BTCUSDT", 1, 1, 110.0, 1.0)],
            &BTreeMap::new(),
            Some(&snapshot),
        )
        .unwrap();
        let venue = &report.symbols[0].venues[0];

        assert_eq!(report.initial_position_count, 1);
        assert_eq!(report.initial_position_snapshot_ts_us, Some(1));
        assert_close(venue.initial_quantity, -2.0);
        assert_eq!(venue.initial_reference_price, Some(100.0));
        assert_eq!(
            venue.initial_reference_price_source,
            Some(InitialReferencePriceSource::Configured)
        );
        assert_close(venue.net_quantity, -1.0);
        assert_close(venue.totals.volume_quote, 110.0);
        assert_close(venue.totals.estimated_trading_fee_quote, 0.11);
        assert_close(venue.totals.realized_pnl_before_fee_quote, -10.0);
        assert_close(venue.totals.floating_pnl_quote, -10.0);
        assert_close(venue.totals.nav_change_before_fee_quote, -20.0);
    }

    #[test]
    fn initial_position_defaults_reference_to_first_fill_price() {
        let source = source("trade01", Some(0.0));
        let snapshot = position_snapshot("trade01", 1, 2.0, None);
        let report = estimate_source_events_with_snapshot(
            &source,
            vec![
                event(2, "BTCUSDT", 1, 2, 120.0, 1.0),
                event(3, "BTCUSDT", 1, 2, 130.0, 1.0),
            ],
            &BTreeMap::new(),
            Some(&snapshot),
        )
        .unwrap();
        let venue = &report.symbols[0].venues[0];

        assert_eq!(venue.initial_reference_price, Some(120.0));
        assert_eq!(
            venue.initial_reference_price_source,
            Some(InitialReferencePriceSource::FirstFill)
        );
        assert_close(venue.totals.realized_pnl_before_fee_quote, 10.0);
        assert_close(venue.net_quantity, 0.0);
    }

    #[test]
    fn configured_initial_reference_supports_a_snapshot_without_later_fills() {
        let source = source("trade01", Some(0.0));
        let snapshot = position_snapshot("trade01", 1, -2.0, Some(100.0));
        let report = estimate_source_events_with_snapshot(
            &source,
            Vec::new(),
            &BTreeMap::new(),
            Some(&snapshot),
        )
        .unwrap();
        let venue = &report.symbols[0].venues[0];

        assert_eq!(venue.mark_price_source, MarkPriceSource::InitialSnapshot);
        assert_eq!(venue.first_fill_ts_us, None);
        assert_eq!(venue.last_fill_ts_us, None);
        assert_close(venue.net_quantity, -2.0);
        assert_close(venue.totals.nav_change_before_fee_quote, 0.0);
    }

    #[test]
    fn initial_position_without_reference_requires_a_later_fill() {
        let source = source("trade01", Some(0.0));
        let snapshot = position_snapshot("trade01", 1, -2.0, None);
        assert!(
            estimate_source_events_with_snapshot(
                &source,
                Vec::new(),
                &BTreeMap::new(),
                Some(&snapshot)
            )
            .unwrap_err()
            .to_string()
            .contains("needs reference_price")
        );
    }

    #[test]
    fn snapshot_excludes_earlier_rocksdb_events() {
        let source = source("trade01", Some(0.0));
        let snapshot = position_snapshot("trade01", 2, -2.0, Some(100.0));
        let report = estimate_source_events_with_snapshot(
            &source,
            vec![
                event(1, "BTCUSDT", 1, 2, 90.0, 5.0),
                event(3, "BTCUSDT", 1, 1, 110.0, 1.0),
            ],
            &BTreeMap::new(),
            Some(&snapshot),
        )
        .unwrap();

        assert_eq!(report.ignored_at_or_before_snapshot_event_count, 1);
        assert_close(report.symbols[0].net_quantity, -1.0);
    }

    #[test]
    fn venue_fifos_never_match_each_other() {
        let report = estimate(vec![
            event(1, "BTCUSDT", 1, 1, 100.0, 1.0),
            event(2, "BTCUSDT", 3, 2, 110.0, 1.0),
        ]);
        let symbol = &report.symbols[0];

        assert_eq!(symbol.venue_count, 2);
        assert_close(symbol.totals.realized_pnl_before_fee_quote, 0.0);
        assert_close(symbol.long_quantity, 1.0);
        assert_close(symbol.short_quantity, 1.0);
        assert_close(symbol.net_quantity, 0.0);
    }

    #[test]
    fn symbols_never_match_each_other() {
        let report = estimate(vec![
            event(1, "BTCUSDT", 1, 1, 100.0, 1.0),
            event(2, "ETHUSDT", 1, 2, 110.0, 1.0),
        ]);

        assert_eq!(report.symbols.len(), 2);
        assert_close(report.totals.realized_pnl_before_fee_quote, 0.0);
    }

    #[test]
    fn latest_fill_price_marks_each_venue_by_default() {
        let report = estimate(vec![
            event(1, "BTCUSDT", 1, 1, 100.0, 1.0),
            event(2, "BTCUSDT", 1, 1, 120.0, 1.0),
        ]);
        let venue = &report.symbols[0].venues[0];

        assert_close(venue.mark_price, 120.0);
        assert_eq!(venue.mark_price_source, MarkPriceSource::LatestFill);
        assert_close(venue.totals.floating_pnl_quote, 20.0);
    }

    #[test]
    fn fifo_uses_trade_update_time_instead_of_persistence_order() {
        let mut later_buy_persisted_first = event(1, "BTCUSDT", 1, 1, 200.0, 1.0);
        later_buy_persisted_first.update_ts_us = 2;
        let mut earlier_buy_persisted_second = event(2, "BTCUSDT", 1, 1, 100.0, 1.0);
        earlier_buy_persisted_second.update_ts_us = 1;
        let sell = event(3, "BTCUSDT", 1, 2, 150.0, 1.0);

        let report = estimate(vec![
            later_buy_persisted_first,
            earlier_buy_persisted_second,
            sell,
        ]);
        let venue = &report.symbols[0].venues[0];

        assert_close(venue.totals.realized_pnl_before_fee_quote, 50.0);
        assert_close(venue.long_quantity, 1.0);
        assert_close(venue.totals.floating_pnl_quote, -50.0);
    }

    #[test]
    fn explicit_mark_override_revalues_open_lots() {
        let mut marks = VenueMarkOverrides::new();
        marks.insert(("BTCUSDT".to_string(), 1), 130.0);
        let report = estimate_source_events(
            &source("trade01", Some(0.0)),
            vec![
                event(1, "BTCUSDT", 1, 1, 100.0, 1.0),
                event(2, "BTCUSDT", 1, 1, 120.0, 1.0),
            ],
            &marks,
        )
        .unwrap();
        let venue = &report.symbols[0].venues[0];

        assert_eq!(venue.mark_price_source, MarkPriceSource::Override);
        assert_close(venue.totals.floating_pnl_quote, 40.0);
    }

    #[test]
    fn source_fifos_never_match_during_aggregation() {
        let trade01 = estimate_source_events(
            &source("trade01", Some(0.0)),
            vec![event(1, "BTCUSDT", 1, 1, 100.0, 1.0)],
            &BTreeMap::new(),
        )
        .unwrap();
        let trade02 = estimate_source_events(
            &source("trade02", Some(0.0)),
            vec![event(1, "BTCUSDT", 1, 2, 110.0, 1.0)],
            &BTreeMap::new(),
        )
        .unwrap();
        let report = aggregate_source_reports(vec![trade01, trade02]);
        let symbol = &report.aggregate.symbols[0];

        assert_eq!(symbol.source_count, 2);
        assert_close(symbol.totals.realized_pnl_before_fee_quote, 0.0);
        assert_close(symbol.long_quantity, 1.0);
        assert_close(symbol.short_quantity, 1.0);
    }

    #[test]
    fn ignores_non_fill_events_but_rejects_invalid_fill_data() {
        let mut no_fill = event(1, "BTCUSDT", 1, 1, 0.0, 0.0);
        no_fill.status = "NEW".to_string();
        let report = estimate(vec![no_fill]);
        assert_eq!(report.order_event_count, 1);
        assert_eq!(report.ignored_non_fill_event_count, 1);
        assert!(report.symbols.is_empty());

        let invalid = event(1, "BTCUSDT", 1, 1, f64::NAN, 1.0);
        assert!(
            estimate_source_events(
                &source("trade01", Some(0.0)),
                vec![invalid],
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn nav_requires_a_valid_configured_fee_rate() {
        assert!(
            estimate_source_events(&source("trade01", None), Vec::new(), &BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("estimated_fee_rate")
        );
        assert!(
            estimate_source_events(
                &source("trade01", Some(-0.001)),
                Vec::new(),
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn timeline_uses_pre_window_fills_and_resets_the_window_to_zero() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        write_events(
            &path,
            &[
                event_at(1, 1, "BTCUSDT", 1, 1, 100.0, 1.0),
                event_at(3, 3, "BTCUSDT", 1, 2, 110.0, 1.0),
            ],
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.0)]),
            timeline_request(2, 4, Vec::new(), Vec::new()),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();

        assert_eq!(report.points.first().unwrap().ts_us, 2);
        assert_close(
            report
                .points
                .first()
                .unwrap()
                .totals
                .nav_change_before_fee_quote,
            0.0,
        );
        assert_close(report.summary.nav_change_before_fee_quote, 10.0);
        assert_close(
            report
                .points
                .last()
                .unwrap()
                .totals
                .nav_change_before_fee_quote,
            10.0,
        );
    }

    #[test]
    fn timeline_counts_a_fill_exactly_at_the_window_start() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        write_events(
            &path,
            &[
                event_at(1, 1, "BTCUSDT", 1, 1, 100.0, 1.0),
                event_at(2, 2, "BTCUSDT", 1, 2, 110.0, 1.0),
            ],
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.0)]),
            timeline_request(2, 3, Vec::new(), Vec::new()),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();

        assert_eq!(report.summary.fill_count, 1);
        assert_close(report.summary.realized_pnl_before_fee_quote, 10.0);
    }

    #[test]
    fn timeline_fee_after_nav_differs_by_the_estimated_window_fees() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        write_events(
            &path,
            &[
                event_at(1, 1, "BTCUSDT", 1, 1, 100.0, 1.0),
                event_at(2, 2, "BTCUSDT", 1, 2, 110.0, 1.0),
            ],
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.001)]),
            timeline_request(1, 2, Vec::new(), Vec::new()),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();

        assert_close(report.summary.nav_change_before_fee_quote, 10.0);
        assert_close(report.summary.estimated_trading_fee_quote, 0.21);
        assert_close(report.summary.nav_change_after_fee_quote, 9.79);
    }

    #[test]
    fn timeline_splits_one_source_nav_by_batch_exec_strategy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        let strategy_a = "CTA_SK_C40V6PosT1_LXY_filter_Position";
        let strategy_b = "CTA_SK_C40V6PosV5_V2_LXY_filter_Position";
        write_events(
            &path,
            &[
                strategy_event_at(1, 1, "BTCUSDT", 1, 1, 100.0, 1.0, strategy_a),
                strategy_event_at(2, 2, "BTCUSDT", 1, 2, 110.0, 1.0, strategy_b),
                strategy_event_at(3, 3, "BTCUSDT", 1, 2, 120.0, 1.0, strategy_a),
            ],
        );

        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.0)]),
            timeline_request(1, 3, Vec::new(), Vec::new()),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();

        assert_eq!(
            report.available_strategies,
            vec![strategy_a.to_string(), strategy_b.to_string()]
        );
        let strategy_a_nav = report
            .strategy_points
            .iter()
            .find(|strategy| strategy.strategy == strategy_a)
            .unwrap()
            .summary
            .nav_change_before_fee_quote;
        let strategy_b_nav = report
            .strategy_points
            .iter()
            .find(|strategy| strategy.strategy == strategy_b)
            .unwrap()
            .summary
            .nav_change_before_fee_quote;
        assert_close(strategy_a_nav, 20.0);
        assert_close(strategy_b_nav, -10.0);
        assert_close(
            strategy_a_nav + strategy_b_nav,
            report.summary.nav_change_before_fee_quote,
        );
    }

    #[test]
    fn timeline_keeps_system_close_and_initial_position_attribution_separate() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        write_events(
            &path,
            &[strategy_event_at(
                1,
                200,
                "BTCUSDT",
                1,
                2,
                110.0,
                1.0,
                "system_position_close",
            )],
        );
        let mut snapshots = SourcePositionSnapshots::new();
        snapshots.insert(
            "trade01".to_string(),
            position_snapshot("trade01", 100, 1.0, Some(100.0)),
        );

        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.0)]),
            timeline_request(100, 200, Vec::new(), Vec::new()),
            &snapshots,
        )
        .unwrap();

        assert_eq!(
            report.available_strategies,
            vec![
                INITIAL_POSITION_STRATEGY.to_string(),
                "system_position_close".to_string(),
            ]
        );
        let attributed_nav = report
            .strategy_points
            .iter()
            .map(|strategy| strategy.summary.nav_change_before_fee_quote)
            .sum::<f64>();
        assert_close(attributed_nav, report.summary.nav_change_before_fee_quote);
        assert_close(report.summary.nav_change_before_fee_quote, 10.0);
    }

    #[test]
    fn strategy_from_key_accepts_only_stable_batch_exec_names() {
        assert_eq!(
            strategy_from_from_key("batch_exec:CTA_SK_C40V6PosT1_LXY_filter_Position"),
            "CTA_SK_C40V6PosT1_LXY_filter_Position"
        );
        assert_eq!(
            strategy_from_from_key("batch_exec:system_position_close"),
            "system_position_close"
        );
        assert_eq!(strategy_from_from_key("other:alpha"), UNATTRIBUTED_STRATEGY);
        assert_eq!(strategy_from_from_key("batch_exec:"), UNATTRIBUTED_STRATEGY);
    }

    #[test]
    fn timeline_symbol_selection_changes_only_the_portfolio_curve() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        write_events(
            &path,
            &[
                event_at(1, 10, "BTCUSDT", 1, 1, 100.0, 1.0),
                event_at(2, 20, "BTCUSDT", 1, 2, 110.0, 1.0),
                event_at(3, 10, "ETHUSDT", 1, 1, 200.0, 1.0),
                event_at(4, 20, "ETHUSDT", 1, 2, 220.0, 1.0),
            ],
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.0)]),
            timeline_request(10, 30, Vec::new(), vec!["BTCUSDT".to_string()]),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();

        assert_eq!(report.available_symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(report.selected_symbols, vec!["BTCUSDT"]);
        assert_eq!(report.symbols.len(), 2);
        assert_eq!(report.symbol_points.len(), 1);
        assert_close(report.summary.nav_change_before_fee_quote, 10.0);
    }

    #[test]
    fn timeline_never_matches_positions_between_sources() {
        let temp = tempfile::tempdir().unwrap();
        let trade01_path = temp.path().join("trade01");
        let trade02_path = temp.path().join("trade02");
        write_events(
            &trade01_path,
            &[event_at(1, 1, "BTCUSDT", 1, 1, 100.0, 1.0)],
        );
        write_events(
            &trade02_path,
            &[event_at(1, 1, "BTCUSDT", 1, 2, 110.0, 1.0)],
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![
                source_at("trade01", &trade01_path, 0.0),
                source_at("trade02", &trade02_path, 0.0),
            ]),
            timeline_request(1, 2, Vec::new(), Vec::new()),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();
        let btc = &report.symbols[0];

        assert_eq!(btc.source_count, 2);
        assert_close(btc.totals.realized_pnl_before_fee_quote, 0.0);
        assert_close(btc.long_quantity, 1.0);
        assert_close(btc.short_quantity, 1.0);
    }

    #[test]
    fn timeline_activates_each_source_snapshot_before_its_later_fills() {
        let temp = tempfile::tempdir().unwrap();
        let early_path = temp.path().join("early");
        let snapshot_path = temp.path().join("snapshot");
        write_events(&early_path, &[event_at(1, 50, "ETHUSDT", 1, 1, 200.0, 1.0)]);
        write_events(
            &snapshot_path,
            &[event_at(2, 150, "BTCUSDT", 1, 2, 110.0, 1.0)],
        );
        let mut snapshots = SourcePositionSnapshots::new();
        snapshots.insert(
            "snapshot".to_string(),
            position_snapshot("snapshot", 100, 1.0, Some(100.0)),
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![
                source_at("early", &early_path, 0.0),
                source_at("snapshot", &snapshot_path, 0.0),
            ]),
            timeline_request(50, 200, Vec::new(), vec!["BTCUSDT".to_string()]),
            &snapshots,
        )
        .unwrap();
        let btc = report
            .symbols
            .iter()
            .find(|symbol| symbol.symbol == "BTCUSDT")
            .unwrap();

        assert_close(btc.totals.realized_pnl_before_fee_quote, 10.0);
        assert_close(btc.net_quantity, 0.0);
        assert_close(report.summary.nav_change_before_fee_quote, 10.0);
    }

    #[test]
    fn timeline_returns_window_bounds_and_fifteen_minute_ticks() {
        const MINUTE_US: i64 = 60 * 1_000_000;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trade01");
        write_events(
            &path,
            &[event_at(1, 100 * 1_000_000, "BTCUSDT", 1, 1, 100.0, 1.0)],
        );
        let report = rebuild_nav_timeline_from_rocksdb_with_snapshots(
            &app_config(vec![source_at("trade01", &path, 0.0)]),
            timeline_request(100 * 1_000_000, 1_810 * 1_000_000, Vec::new(), Vec::new()),
            &SourcePositionSnapshots::new(),
        )
        .unwrap();
        let timestamps = report
            .points
            .iter()
            .map(|point| point.ts_us)
            .collect::<Vec<_>>();

        assert_eq!(
            timestamps,
            vec![
                100 * 1_000_000,
                15 * MINUTE_US,
                30 * MINUTE_US,
                1_810 * 1_000_000,
            ]
        );
    }

    #[test]
    fn same_timestamp_timeline_points_are_replaced() {
        let mut points = vec![NavTimelinePoint {
            ts_us: 10,
            totals: NavTotals::default(),
        }];
        let replacement = NavTimelinePoint {
            ts_us: 10,
            totals: NavTotals {
                nav_change_before_fee_quote: 12.0,
                ..NavTotals::default()
            },
        };

        push_or_replace_timeline_point(&mut points, replacement);

        assert_eq!(points, vec![replacement]);
    }

    #[test]
    fn rebuilds_from_rocksdb_without_reading_the_database_url() {
        let temp = tempfile::tempdir().unwrap();
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db = DB::open_cf_descriptors(
            &options,
            temp.path(),
            vec![ColumnFamilyDescriptor::new(
                UNIFORM_ORDERS_CF,
                Options::default(),
            )],
        )
        .unwrap();
        let column_family = db.cf_handle(UNIFORM_ORDERS_CF).unwrap();
        for fill in [
            event(1, "BTCUSDT", 1, 1, 100.0, 1.0),
            event(2, "BTCUSDT", 1, 2, 110.0, 1.0),
        ] {
            db.put_cf(
                column_family,
                fill.record_key.as_bytes(),
                encode_event(&fill),
            )
            .unwrap();
        }

        let source = SourceConfig {
            rocksdb_path: temp.path().to_path_buf(),
            ..source("trade01", Some(0.001))
        };
        let config = AppConfig {
            database: DatabaseConfig {
                url_env: "CTA_NAV_TEST_DATABASE_URL_MUST_NOT_BE_READ".to_string(),
                max_connections: 1,
            },
            ingestion: IngestionConfig::default(),
            order_config: OrderConfigSettings::default(),
            sources: vec![source],
        };

        let report = rebuild_nav_from_rocksdb(&config, &[]).unwrap();

        assert_eq!(report.source_count, 1);
        assert_close(report.aggregate.totals.nav_change_before_fee_quote, 10.0);
        assert_close(report.aggregate.totals.nav_change_after_fee_quote, 9.79);
    }
}
