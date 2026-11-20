//! Binary encoder mirroring the domestic Exec writer (`cn_futures::persist`):
//! stored uniform orders and trade updates followed by the record extension.
//! Used by tests and synthetic fixtures only; Manager never writes Exec stores.
use super::{super::nav, ORDERS, TRADES, VENUE_CODES, key};
use crate::model::{PositionEffect, RecordExtension};
use rocksdb::{DB, Options};

pub const NEW: u8 = 1;
pub const PARTIALLY_FILLED: u8 = 2;
pub const FILLED: u8 = 3;
pub const CANCELED: u8 = 4;
pub const EXPIRED: u8 = 5;

const LIMIT: u8 = 1;
const INTERNAL_CROSS: u8 = 2;

fn venue_index(venue: &str) -> usize {
    VENUE_CODES
        .iter()
        .position(|(name, _)| *name == venue)
        .expect("domestic fixture venue")
}

fn side_code(side: nav::Side) -> u8 {
    match side {
        nav::Side::Buy => 1,
        nav::Side::Sell => 2,
    }
}

fn effect(offset: nav::Offset) -> PositionEffect {
    match offset {
        nav::Offset::Open => PositionEffect::Open,
        nav::Offset::Close => PositionEffect::Close,
        nav::Offset::CloseToday => PositionEffect::CloseToday,
        nav::Offset::CloseYesterday => PositionEffect::CloseYesterday,
    }
}

/// Stable 63-bit FNV-1a id of (trading day, venue, session, order ref), as Exec computes it.
pub fn client_order_id(trading_day: &str, venue: &str, session: &str, order_ref: &str) -> i64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let venue = [venue_index(venue) as u8];
    for part in [
        trading_day.as_bytes(),
        &venue,
        session.as_bytes(),
        order_ref.as_bytes(),
    ] {
        for byte in (part.len() as u16).to_le_bytes().iter().chain(part) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    (hash & i64::MAX as u64) as i64
}

struct Uniform<'a> {
    symbol: &'a str,
    created_us: i64,
    at_us: i64,
    client_order_id: i64,
    venue: &'a str,
    order_type: u8,
    side: nav::Side,
    price: f64,
    amount_init: f64,
    amount_update: f64,
    status: u8,
    from_key: &'a str,
    extension: RecordExtension,
}

fn uniform(record: Uniform<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&record.at_us.to_le_bytes());
    out.extend_from_slice(&(record.symbol.len() as u16).to_le_bytes());
    out.extend_from_slice(record.symbol.as_bytes());
    for value in [
        record.created_us,
        record.at_us,
        0,
        record.created_us,
        record.at_us,
        0,
        record.client_order_id,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&[
        VENUE_CODES[venue_index(record.venue)].1,
        record.order_type,
        side_code(record.side),
    ]);
    for value in [record.price, 0.0, record.amount_init, record.amount_update] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.push(record.status);
    out.extend_from_slice(&(record.from_key.len() as u32).to_le_bytes());
    out.extend_from_slice(record.from_key.as_bytes());
    // Empty BBO spread text, an all-absent signal BBO and unknown fill role.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&[0; 83]);
    out.push(0);
    record.extension.encode(&mut out);
    out
}

/// One native order as Exec identifies it.
#[derive(Debug, Clone)]
pub struct Order {
    pub venue: String,
    pub symbol: String,
    pub trading_day: String,
    pub session: String,
    pub order_ref: String,
    pub side: nav::Side,
    pub offset: nav::Offset,
    pub lots: i64,
    pub limit_price: f64,
}

/// One factual exchange fill of an [`Order`].
#[derive(Debug, Clone)]
pub struct Fill {
    pub trade_id: String,
    pub lots: i64,
    pub price: f64,
    pub commission: Option<f64>,
    pub order_id: i64,
}

impl Order {
    /// `market` is `EXCHANGE:symbol`.
    #[allow(clippy::too_many_arguments)] // Mirrors every field of one Exec fill share.
    pub fn new(
        market: &str,
        trading_day: &str,
        session: &str,
        order_ref: &str,
        side: nav::Side,
        offset: nav::Offset,
        lots: i64,
        limit_price: f64,
    ) -> Self {
        let (venue, symbol) = market.split_once(':').expect("EXCHANGE:symbol");
        Self {
            venue: venue.into(),
            symbol: symbol.into(),
            trading_day: trading_day.into(),
            session: session.into(),
            order_ref: order_ref.into(),
            side,
            offset,
            lots,
            limit_price,
        }
    }

    pub fn client_order_id(&self) -> i64 {
        client_order_id(
            &self.trading_day,
            &self.venue,
            &self.session,
            &self.order_ref,
        )
    }

    pub fn text(&self) -> String {
        format!("{}/{}", self.session, self.order_ref)
    }

    /// Lifecycle record: status change of the order, no fill quantity.
    pub fn lifecycle(&self, status: u8, from_key: &str, created_us: i64, at_us: i64) -> Vec<u8> {
        uniform(Uniform {
            symbol: &self.symbol,
            created_us,
            at_us,
            client_order_id: self.client_order_id(),
            venue: &self.venue,
            order_type: LIMIT,
            side: self.side,
            price: self.limit_price,
            amount_init: self.lots as f64,
            amount_update: 0.0,
            status,
            from_key,
            extension: RecordExtension {
                position_effect: Some(effect(self.offset)),
                trading_day: Some(self.trading_day.clone()),
                client_order_text: Some(self.text()),
                ..Default::default()
            },
        })
    }

    /// One strategy's share of `fill`, with its prorated commission.
    pub fn share(
        &self,
        fill: &Fill,
        strategy: &str,
        lots: i64,
        commission: Option<f64>,
        status: u8,
        at_us: i64,
    ) -> Vec<u8> {
        uniform(Uniform {
            symbol: &self.symbol,
            created_us: at_us,
            at_us,
            client_order_id: self.client_order_id(),
            venue: &self.venue,
            order_type: LIMIT,
            side: self.side,
            price: fill.price,
            amount_init: self.lots as f64,
            amount_update: lots as f64,
            status,
            from_key: strategy,
            extension: RecordExtension {
                position_effect: Some(effect(self.offset)),
                trading_day: Some(self.trading_day.clone()),
                trade_id: Some(fill.trade_id.clone()),
                last_filled_quantity: Some(fill.lots as f64),
                commission,
                client_order_text: Some(self.text()),
            },
        })
    }

    /// The factual exchange fill, stored once.
    pub fn trade(&self, fill: &Fill, cumulative: i64, status: Option<u8>, at_us: i64) -> Vec<u8> {
        let mut out = Vec::new();
        for value in [at_us, at_us, at_us] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&(self.symbol.len() as u32).to_le_bytes());
        out.extend_from_slice(self.symbol.as_bytes());
        out.extend_from_slice(&fill.order_id.to_le_bytes());
        out.extend_from_slice(&self.client_order_id().to_le_bytes());
        out.push(side_code(self.side));
        out.extend_from_slice(&fill.price.to_le_bytes());
        out.push(0);
        out.push(VENUE_CODES[venue_index(&self.venue)].1);
        out.extend_from_slice(&(cumulative as f64).to_le_bytes());
        match status {
            Some(status) => out.extend_from_slice(&[1, status]),
            None => out.push(0),
        }
        RecordExtension {
            position_effect: Some(effect(self.offset)),
            trading_day: Some(self.trading_day.clone()),
            trade_id: Some(fill.trade_id.clone()),
            last_filled_quantity: Some(fill.lots as f64),
            commission: fill.commission,
            client_order_text: Some(self.text()),
        }
        .encode(&mut out);
        out
    }
}

/// One internal-cross leg at the mid: buyer `side = Buy`, seller `Sell`.
#[allow(clippy::too_many_arguments)] // Mirrors every field of one Exec cross leg.
pub fn cross_leg(
    market: &str,
    trading_day: &str,
    quote_us: i64,
    index: usize,
    side: nav::Side,
    strategy: &str,
    lots: i64,
    price: f64,
    at_us: i64,
) -> Vec<u8> {
    let (venue, symbol) = market.split_once(':').expect("EXCHANGE:symbol");
    let leg = if side == nav::Side::Buy { "b" } else { "s" };
    uniform(Uniform {
        symbol,
        created_us: at_us,
        at_us,
        client_order_id: client_order_id(
            trading_day,
            venue,
            &format!("cross:{quote_us}"),
            &format!("{index}{leg}"),
        ),
        venue,
        order_type: INTERNAL_CROSS,
        side,
        price,
        amount_init: lots as f64,
        amount_update: lots as f64,
        status: FILLED,
        from_key: strategy,
        extension: RecordExtension {
            trading_day: Some(trading_day.into()),
            ..Default::default()
        },
    })
}

/// A primary execution store with exactly the two domestic column families.
pub fn open(path: &std::path::Path) -> DB {
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    DB::open_cf(&options, path, [ORDERS, TRADES]).expect("open domestic fixture store")
}

/// Appends a record under the 20-digit key for `at_us`.
pub fn put(db: &DB, cf: &str, at_us: i64, value: &[u8]) {
    db.put_cf(
        db.cf_handle(cf).expect("domestic column family"),
        key(at_us),
        value,
    )
    .expect("write domestic fixture record");
}

pub fn put_order(db: &DB, at_us: i64, value: &[u8]) {
    put(db, ORDERS, at_us, value)
}

pub fn put_trade(db: &DB, at_us: i64, value: &[u8]) {
    put(db, TRADES, at_us, value)
}
