//! Account NAV inputs from the Exec trade log: an explicit opening inventory
//! cut at the log tip, then every factual fill after that key. Never derive
//! trade or strategy identity from order type, labels or a nearby timestamp.
use super::*;
use crate::native_futures::{
    config::Source,
    records::{self, TRADES},
};
use rocksdb::DB;

pub use crate::native_futures::records::generation;

pub struct Candidate {
    pub anchor: Anchor,
    pub inventory: Vec<Inventory>,
}

/// Cuts the trade log at its current tip for an operator-stated inventory.
/// Pricing happens separately in [`super::service::price_anchor`].
pub fn candidate(
    source: &Source,
    db: &DB,
    trading_day: String,
    inventory: Vec<Inventory>,
    note: String,
) -> Result<Candidate> {
    reference::valid_day(&trading_day)?;
    ensure!(inventory.len() <= 512, "起始持仓行数超过上限");
    let mut sides = BTreeSet::new();
    let mut held = vec![];
    for position in inventory {
        super::super::validate_market(&format!("{}:{}", position.venue, position.symbol))?;
        ensure!(
            position.today >= 0
                && position.yesterday >= 0
                && sides.insert((
                    position.venue.clone(),
                    position.symbol.clone(),
                    position.side
                )),
            "起始持仓须为非负今昨仓，且每个合约方向只列一次"
        );
        if position
            .today
            .checked_add(position.yesterday)
            .context("native position overflow")?
            > 0
        {
            held.push(position);
        }
    }
    let anchor = Anchor {
        source_id: source.id.clone(),
        snapshot_us: crate::native_futures::now_us(),
        trading_day,
        trades_key: records::tip(db, TRADES)?,
        store_id: generation(source, db)?,
        positions: vec![],
        note,
    };
    anchor.validate()?;
    Ok(Candidate {
        anchor,
        inventory: held,
    })
}

/// The anchor belongs to this source and store generation, and its cut exists.
pub fn validate_anchor(source: &Source, db: &DB, anchor: &Anchor) -> Result<()> {
    anchor.validate()?;
    ensure!(
        anchor.source_id == source.id
            && generation(source, db)? == anchor.store_id
            && anchor.trades_key <= records::tip(db, TRADES)?,
        "起始快照属于其他账户或执行库世代"
    );
    Ok(())
}

pub fn fill(source_id: &str, trade: records::Trade) -> Fill {
    Fill {
        key_us: trade.key_us,
        source_id: source_id.into(),
        venue: trade.venue,
        symbol: trade.symbol,
        trading_day: trade.trading_day,
        trade_id: trade.trade_id,
        side: trade.side,
        offset: trade.offset,
        lots: trade.lots,
        price: trade.price,
        received_us: trade.received_us,
        commission_cny: trade.commission_cny,
    }
}

/// Factual fills after the anchor cut received no later than `end_us`.
pub fn fills(source: &Source, db: &DB, anchor: &Anchor, end_us: i64) -> Result<Vec<Fill>> {
    validate_anchor(source, db, anchor)?;
    let stop = end_us.saturating_add(records::KEY_SLACK_US);
    let mut fills = vec![];
    records::scan(db, TRADES, anchor.trades_key, |key, value| {
        if records::key_us(key)? > stop {
            return Ok(false);
        }
        let trade = records::decode_trade(key, value)?;
        if trade.received_us <= end_us {
            ensure!(
                fills.len() < 250_000,
                "native NAV retained fill budget exceeded"
            );
            fills.push(fill(&source.id, trade));
        }
        Ok(true)
    })?;
    Ok(fills)
}
