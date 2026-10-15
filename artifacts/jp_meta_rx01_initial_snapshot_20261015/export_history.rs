use std::io::{self, Write};

use crypto_cta_manager::{model, rocks_source};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("missing absolute RocksDB path")?;
    let family = std::env::args().nth(2).unwrap_or_else(|| model::UNIFORM_ORDERS_CF.into());
    assert!(family == model::UNIFORM_ORDERS_CF || family == "rapidx_executions");
    let records = rocks_source::read_all_column_families(
        std::path::Path::new(&path),
        &[family.as_str()],
    )?;
    let records = &records[family.as_str()];
    let mut output = io::BufWriter::new(io::stdout().lock());
    for record in records {
        if family == "rapidx_executions" {
            let stored: serde_json::Value = serde_json::from_slice(&record.value)?;
            serde_json::to_writer(&mut output, &json!({"exchange":stored["exchange"],"execution":stored["execution"]}))?;
            writeln!(output)?;
            continue;
        }
        let event = model::decode_uniform_order(&record.key, &record.value)?;
        serde_json::to_writer(
            &mut output,
            &json!({
                "record_key":event.record_key, "update_ts_us":event.update_ts_us,
                "create_ts_us":event.create_ts_us, "event_ts_us":event.event_ts_us,
                "symbol":event.symbol, "venue_code":event.venue_code,
                "side_code":event.side_code, "amount_update":event.amount_update,
                "price":event.price, "from_key_text":event.from_key_text,
                "client_order_id":event.client_order_id, "order_type_code":event.order_type_code,
            }),
        )?;
        writeln!(output)?;
    }
    Ok(())
}
