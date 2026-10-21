#[path = "../../src/model.rs"]
mod model;
#[path = "../../src/rocks_source.rs"]
mod rocks_source;
use std::{collections::BTreeMap, path::Path};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let since: i64 = args[2].parse()?;
    let records = rocks_source::read_latest_column_families(
        Path::new(&args[1]),
        &[model::UNIFORM_ORDERS_CF, model::TRADE_UPDATES_CF, model::ORDER_UPDATES_CF],
        50_000,
    )?;
    let mut orders = BTreeMap::new();
    let mut fills = Vec::new();
    let mut newest_event = 0;
    let mut newest_fill = 0;
    let mut rejects = Vec::new();
    for raw in records.get(model::UNIFORM_ORDERS_CF).into_iter().flatten() {
        let e = model::decode_uniform_order(&raw.key, &raw.value)?;
        newest_event = newest_event.max(e.update_ts_us);
        if e.amount_update > 0.0 { newest_fill = newest_fill.max(e.update_ts_us); }
        if e.update_ts_us < since { continue; }
        let row = json!({"ts_us": e.update_ts_us, "created_us": e.create_ts_us, "client_order_id": e.client_order_id, "symbol": e.symbol, "venue": e.venue, "side": e.side, "type": e.order_type, "status": e.status, "price": e.price, "quantity": e.amount_initial, "fill_quantity": e.amount_update, "strategy": e.from_key_text, "liquidity": format!("{:?}",e.fill_liquidity)});
        orders.entry(e.client_order_id).or_insert_with(|| row.clone());
        if e.amount_update > 0.0 { fills.push(row.clone()); }
        if e.status.to_lowercase().contains("reject") { rejects.push(row); }
    }
    let mut trades = Vec::new();
    for raw in records.get(model::TRADE_UPDATES_CF).into_iter().flatten() {
        let e = model::decode_trade_update(&raw.key, &raw.value)?;
        if e.trade_ts_us < since { continue; }
        trades.push(json!({"ts_us":e.trade_ts_us,"client_order_id":e.client_order_id,"symbol":e.symbol,"price":e.price,"is_maker":e.is_maker,"cumulative_qty":e.cumulative_filled_quantity}));
    }
    println!("{}", json!({"since_us":since,"newest_event_us":newest_event,"newest_fill_us":newest_fill,"orders":orders.values().collect::<Vec<_>>(),"fills":fills,"rejections":rejects,"trades":trades,"records_scanned":records.iter().map(|(k,v)|(k.clone(),v.len())).collect::<BTreeMap<_,_>>()}));
    Ok(())
}
