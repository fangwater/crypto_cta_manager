use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_cta_manager::model::{
    ORDER_UPDATES_UNMATCHED_CF, TRADE_UPDATES_UNMATCHED_CF, decode_order_update,
    decode_trade_update,
};
use crypto_cta_manager::rocks_source::read_latest_column_families;

/// Dump the newest records from the unmatched order/trade update column
/// families of an Exec persist_manager RocksDB. Read-only; the live
/// persist_manager keeps running.
#[derive(Debug, Parser)]
struct Args {
    /// Exec persist_manager RocksDB directory.
    #[arg(long)]
    rocksdb: PathBuf,
    /// Number of newest records to print per column family.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.rocksdb.is_dir() {
        bail!(
            "rocksdb path is not a directory: {}",
            args.rocksdb.display()
        );
    }
    let records = read_latest_column_families(
        &args.rocksdb,
        &[ORDER_UPDATES_UNMATCHED_CF, TRADE_UPDATES_UNMATCHED_CF],
        args.limit,
    )
    .with_context(|| format!("failed to read {}", args.rocksdb.display()))?;

    for (cf, rows) in &records {
        println!("== {cf} ({} newest) ==", rows.len());
        for row in rows {
            if cf == ORDER_UPDATES_UNMATCHED_CF {
                match decode_order_update(&row.key, &row.value) {
                    Ok(event) => println!(
                        "ts={} sym={} ord={} cli={} cli_str={:?} side={} type={} tif={} px={} qty={} filled={} status={}({}) x={}({}) venue={}",
                        event.record_key,
                        event.symbol,
                        event.order_id,
                        event.client_order_id,
                        event.client_order_id_text,
                        event.side_code,
                        event.order_type_code,
                        event.time_in_force_code,
                        event.price,
                        event.quantity,
                        event.cumulative_filled_quantity,
                        event.status_code,
                        event.raw_status,
                        event.execution_type_code,
                        event.raw_execution_type,
                        event.venue_code,
                    ),
                    Err(error) => println!(
                        "key={:?} decode failed: {error:#} ({} bytes)",
                        String::from_utf8_lossy(&row.key),
                        row.value.len()
                    ),
                }
            } else {
                match decode_trade_update(&row.key, &row.value) {
                    Ok(event) => println!(
                        "ts={} sym={} ord={} cli={} side={} px={} maker={} filled={} status={:?} venue={}",
                        event.record_key,
                        event.symbol,
                        event.order_id,
                        event.client_order_id,
                        event.side_code,
                        event.price,
                        event.is_maker,
                        event.cumulative_filled_quantity,
                        event.status_code,
                        event.venue_code,
                    ),
                    Err(error) => println!(
                        "key={:?} decode failed: {error:#} ({} bytes)",
                        String::from_utf8_lossy(&row.key),
                        row.value.len()
                    ),
                }
            }
        }
    }
    Ok(())
}
