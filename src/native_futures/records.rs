//! The domestic Exec store: two append-only logs in the crypto layouts, keyed
//! by a 20-digit microsecond timestamp and never rewritten. `cn_uniform_orders`
//! holds order lifecycle records (`amount_update = 0`), per-strategy fill shares
//! and internal-cross legs; `cn_trade_updates` holds each factual exchange fill
//! once. Domestic facts travel in the shared record extension.
use super::nav::{Offset, Side};
use crate::model::{self, PositionEffect, RecordExtension};
use anyhow::{Context, Result, bail, ensure};
use rocksdb::{DB, Direction, IteratorMode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub mod fixture;

pub const ORDERS: &str = "cn_uniform_orders";
pub const TRADES: &str = "cn_trade_updates";
pub const COLUMN_FAMILIES: [&str; 2] = [ORDERS, TRADES];
/// A key is `max(received, previous + 1)`: never before its receipt time and,
/// after a burst sharing one microsecond, at most slightly later.
pub const KEY_SLACK_US: i64 = 60_000_000;

/// Order type of an internal cross leg; no exchange order exists.
const INTERNAL_CROSS: i16 = crate::model::INTERNAL_CROSS_ORDER_TYPE_CODE;
const LIMIT: i16 = 1;
const MARKET: i16 = 3;

/// Domestic exchanges as crypto `TradingVenue` codes.
pub const VENUE_CODES: [(&str, u8); 6] = [
    ("CFFEX", 64),
    ("SHFE", 65),
    ("INE", 66),
    ("DCE", 67),
    ("CZCE", 68),
    ("GFEX", 69),
];

pub fn venue(code: i16) -> Result<&'static str> {
    VENUE_CODES
        .iter()
        .find(|(_, c)| i16::from(*c) == code)
        .map(|(name, _)| *name)
        .with_context(|| format!("unsupported domestic venue code {code}"))
}

pub fn key(us: i64) -> String {
    format!("{us:020}")
}

pub fn key_us(key: &[u8]) -> Result<i64> {
    ensure!(
        key.len() == 20 && key.iter().all(u8::is_ascii_digit),
        "domestic record key is not a 20-digit timestamp"
    );
    std::str::from_utf8(key)?
        .parse()
        .context("domestic record key exceeds i64")
}

fn side(code: i16) -> Result<Side> {
    match code {
        1 => Ok(Side::Buy),
        2 => Ok(Side::Sell),
        _ => bail!("invalid domestic side code {code}"),
    }
}

fn offset(effect: Option<PositionEffect>) -> Result<Offset> {
    Ok(match effect.context("missing domestic position effect")? {
        PositionEffect::Open => Offset::Open,
        PositionEffect::Close => Offset::Close,
        PositionEffect::CloseToday => Offset::CloseToday,
        PositionEffect::CloseYesterday => Offset::CloseYesterday,
    })
}

/// Whole lots stored as `f64` in the crypto layouts.
fn lots(value: f64, field: &str) -> Result<i64> {
    ensure!(
        value.is_finite() && value > 0.0 && value.fract() == 0.0 && value <= f64::from(i32::MAX),
        "invalid domestic {field} lots"
    );
    Ok(value as i64)
}

fn price(value: f64) -> Result<f64> {
    ensure!(
        value.is_finite() && value > 0.0,
        "invalid domestic fill price"
    );
    Ok(value)
}

fn trading_day(extension: &RecordExtension) -> Result<String> {
    let day = extension
        .trading_day
        .clone()
        .context("missing domestic trading day")?;
    super::reference::valid_day(&day)?;
    Ok(day)
}

fn commission(extension: &RecordExtension) -> Result<Option<f64>> {
    ensure!(
        extension.commission.is_none_or(f64::is_finite),
        "invalid domestic commission"
    );
    Ok(extension.commission)
}

/// Native order identity, split from `client_order_text` at the last `/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct OrderKey {
    pub session: String,
    pub order_ref: String,
}

impl OrderKey {
    fn parse(extension: &RecordExtension) -> Result<Self> {
        let text = extension
            .client_order_text
            .as_deref()
            .context("missing native client order text")?;
        let (session, order_ref) = text
            .rsplit_once('/')
            .with_context(|| format!("invalid native client order text {text}"))?;
        ensure!(!order_ref.is_empty(), "empty native order reference");
        Ok(Self {
            session: session.into(),
            order_ref: order_ref.into(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    /// Terminal without (further) fills; a domestic rejection.
    Expired,
}

impl OrderStatus {
    fn from_code(code: i16) -> Result<Self> {
        Ok(match code {
            1 => Self::New,
            2 => Self::PartiallyFilled,
            3 => Self::Filled,
            4 => Self::Canceled,
            5 => Self::Expired,
            _ => bail!("invalid domestic order status {code}"),
        })
    }
    pub fn terminal(self) -> bool {
        matches!(self, Self::Filled | Self::Canceled | Self::Expired)
    }
}

/// One order lifecycle record; the latest per `client_order_id` is current.
#[derive(Debug, Clone, Serialize)]
pub struct Order {
    pub key_us: i64,
    pub client_order_id: i64,
    pub venue: String,
    pub symbol: String,
    pub trading_day: String,
    pub order: OrderKey,
    pub side: Side,
    pub offset: Offset,
    pub order_type: &'static str,
    pub lots: i64,
    pub limit_price: f64,
    pub status: OrderStatus,
    /// `exec`, `exec:<batch_id>` or `observed`.
    pub from_key: String,
    pub created_us: i64,
    pub updated_us: i64,
}

/// A strategy share of an exchange fill, or one internal-cross leg.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyFill {
    pub key_us: i64,
    pub client_order_id: i64,
    pub venue: String,
    pub symbol: String,
    pub trading_day: String,
    /// Strategy name, or [`super::netting::UNALLOCATED`] for the remainder.
    pub strategy: String,
    pub side: Side,
    pub lots: i64,
    pub price: f64,
    pub internal_cross: bool,
    /// Exchange-fill facts; absent for internal crosses.
    pub offset: Option<Offset>,
    pub trade_id: Option<String>,
    pub fill_lots: Option<i64>,
    /// This share's CNY commission (crosses carry none).
    pub commission_cny: Option<f64>,
    pub order: Option<OrderKey>,
}

impl StrategyFill {
    pub fn market(&self) -> String {
        format!("{}:{}", self.venue, self.symbol)
    }
    pub fn signed_lots(&self) -> i64 {
        match self.side {
            Side::Buy => self.lots,
            Side::Sell => -self.lots,
        }
    }
}

pub enum Uniform {
    Order(Order),
    Fill(StrategyFill),
}

pub fn decode_uniform(key: &[u8], value: &[u8]) -> Result<Uniform> {
    let key_us = key_us(key)?;
    let (event, extension) = model::decode_native_uniform_order(key, value)?;
    let venue = venue(event.venue_code)?.to_owned();
    super::validate_market(&format!("{venue}:{}", event.symbol))?;
    let side = side(event.side_code)?;
    let trading_day = trading_day(&extension)?;
    let status = OrderStatus::from_code(event.status_code)?;
    if event.amount_update == 0.0 {
        ensure!(
            matches!(event.order_type_code, LIMIT | MARKET),
            "invalid domestic order type {}",
            event.order_type_code
        );
        ensure!(
            event.price.is_finite() && event.price >= 0.0,
            "invalid domestic limit price"
        );
        return Ok(Uniform::Order(Order {
            key_us,
            client_order_id: event.client_order_id,
            venue,
            symbol: event.symbol,
            trading_day,
            order: OrderKey::parse(&extension)?,
            side,
            offset: offset(extension.position_effect)?,
            order_type: if event.order_type_code == LIMIT {
                "limit"
            } else {
                "market"
            },
            lots: lots(event.amount_initial, "order")?,
            limit_price: event.price,
            status,
            from_key: event.from_key_text,
            created_us: event.create_ts_us,
            updated_us: event.update_ts_us,
        }));
    }
    ensure!(
        !event.from_key_text.is_empty(),
        "strategy fill without a strategy"
    );
    let internal_cross = event.order_type_code == INTERNAL_CROSS;
    let share = lots(event.amount_update, "strategy fill")?;
    let (offset, trade_id, fill_lots, order) = if internal_cross {
        ensure!(
            status == OrderStatus::Filled && event.amount_initial == event.amount_update,
            "invalid domestic internal cross leg"
        );
        (None, None, None, None)
    } else {
        let fill_lots = lots(
            extension
                .last_filled_quantity
                .context("strategy fill lacks its exchange fill lots")?,
            "exchange fill",
        )?;
        ensure!(
            share <= fill_lots,
            "strategy share exceeds its exchange fill"
        );
        let trade_id = extension
            .trade_id
            .clone()
            .filter(|id| !id.is_empty())
            .context("strategy fill lacks its trade id")?;
        (
            Some(offset(extension.position_effect)?),
            Some(trade_id),
            Some(fill_lots),
            Some(OrderKey::parse(&extension)?),
        )
    };
    Ok(Uniform::Fill(StrategyFill {
        key_us,
        client_order_id: event.client_order_id,
        venue,
        symbol: event.symbol,
        trading_day,
        strategy: event.from_key_text,
        side,
        lots: share,
        price: price(event.price)?,
        internal_cross,
        offset,
        trade_id,
        fill_lots,
        commission_cny: commission(&extension)?,
        order,
    }))
}

/// One factual exchange fill.
#[derive(Debug, Clone, Serialize)]
pub struct Trade {
    pub key_us: i64,
    pub received_us: i64,
    pub client_order_id: i64,
    /// Exchange system id parsed as an integer; 0 when unknown.
    pub order_id: i64,
    pub venue: String,
    pub symbol: String,
    pub trading_day: String,
    pub trade_id: String,
    pub order: OrderKey,
    pub side: Side,
    pub offset: Offset,
    pub lots: i64,
    pub price: f64,
    pub commission_cny: Option<f64>,
    /// The order's cumulative filled lots after this fill.
    pub cumulative_lots: i64,
    pub order_status: Option<OrderStatus>,
}

impl Trade {
    pub fn market(&self) -> String {
        format!("{}:{}", self.venue, self.symbol)
    }
}

pub fn decode_trade(key: &[u8], value: &[u8]) -> Result<Trade> {
    let key_us = key_us(key)?;
    let (event, extension) = model::decode_native_trade_update(key, value)?;
    let venue = venue(event.venue_code)?.to_owned();
    super::validate_market(&format!("{venue}:{}", event.symbol))?;
    let cumulative = event.cumulative_filled_quantity;
    ensure!(
        cumulative.is_finite() && cumulative >= 0.0 && cumulative.fract() == 0.0,
        "invalid domestic cumulative lots"
    );
    Ok(Trade {
        key_us,
        received_us: event.recv_ts_us,
        client_order_id: event.client_order_id,
        order_id: event.order_id,
        venue,
        symbol: event.symbol,
        trading_day: trading_day(&extension)?,
        trade_id: extension
            .trade_id
            .clone()
            .filter(|id| !id.is_empty())
            .context("domestic trade update lacks its trade id")?,
        order: OrderKey::parse(&extension)?,
        side: side(event.side_code)?,
        offset: offset(extension.position_effect)?,
        lots: lots(
            extension
                .last_filled_quantity
                .context("domestic trade update lacks its fill lots")?,
            "fill",
        )?,
        price: price(event.price)?,
        commission_cny: commission(&extension)?,
        cumulative_lots: cumulative as i64,
        order_status: event.status_code.map(OrderStatus::from_code).transpose()?,
    })
}

/// Visits records with a key strictly after `after_us`, in key order, until
/// `visit` returns `false`.
pub fn scan(
    db: &DB,
    cf: &str,
    after_us: i64,
    mut visit: impl FnMut(&[u8], &[u8]) -> Result<bool>,
) -> Result<()> {
    let start = key(after_us
        .checked_add(1)
        .context("domestic record key overflow")?);
    for item in db.iterator_cf(
        db.cf_handle(cf)
            .with_context(|| format!("execution store is missing {cf}"))?,
        IteratorMode::From(start.as_bytes(), Direction::Forward),
    ) {
        let (key, value) = item.with_context(|| format!("scan {cf}"))?;
        if !visit(&key, &value)
            .with_context(|| format!("decode {cf} record {}", String::from_utf8_lossy(&key)))?
        {
            break;
        }
    }
    Ok(())
}

/// Last key of a log, 0 when it is empty.
pub fn tip(db: &DB, cf: &str) -> Result<i64> {
    db.iterator_cf(
        db.cf_handle(cf)
            .with_context(|| format!("execution store is missing {cf}"))?,
        IteratorMode::End,
    )
    .next()
    .transpose()?
    .map_or(Ok(0), |(key, _)| key_us(&key))
}

/// Named-strategy signed lots per contract through `through_us` (inclusive),
/// folded from the strategy fill shares and cross legs. The remainder is not
/// included; zero rows are omitted.
pub fn strategy_positions(
    db: &DB,
    through_us: i64,
) -> Result<BTreeMap<String, BTreeMap<String, i64>>> {
    let mut positions: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    scan(db, ORDERS, 0, |key, value| {
        if key_us(key)? > through_us {
            return Ok(false);
        }
        if let Uniform::Fill(fill) = decode_uniform(key, value)?
            && fill.strategy != super::netting::UNALLOCATED
        {
            let lots = positions
                .entry(fill.market())
                .or_default()
                .entry(fill.strategy.clone())
                .or_default();
            *lots = lots
                .checked_add(fill.signed_lots())
                .context("strategy position overflow")?;
        }
        Ok(true)
    })?;
    for row in positions.values_mut() {
        row.retain(|_, lots| *lots != 0);
    }
    positions.retain(|_, row| !row.is_empty());
    Ok(positions)
}

/// Stable identity of the execution store generation that anchors refer to.
pub fn generation(source: &super::config::Source, db: &DB) -> Result<String> {
    if source.sync.is_some() {
        Ok(format!(
            "sync:{}",
            super::sync::replica_status(db, &source.id)?
                .store_id
                .context("replica lacks origin store identity")?
        ))
    } else {
        Ok(format!(
            "native:{}",
            hex::encode(Sha256::digest(std::fs::read(db.path().join("IDENTITY"))?))
        ))
    }
}

#[cfg(test)]
mod tests;
