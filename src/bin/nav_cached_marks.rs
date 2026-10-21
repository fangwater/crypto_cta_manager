//! Inspect existing Manager minute marks without fetching or modifying anything.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use rocksdb::{DB, Options};
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    manager_db: PathBuf,
    #[arg(long, required = true)]
    ts_us: Vec<i64>,
    #[arg(long = "symbol", required = true)]
    symbols: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut options = Options::default();
    options.set_max_open_files(256);
    let families = DB::list_cf(&options, &args.manager_db)?;
    let db = DB::open_cf_for_read_only(&options, &args.manager_db, families, false)?;
    let cf = db
        .cf_handle("klines_1m")
        .context("no minute candle cache")?;
    for symbol in &args.symbols {
        ensure!(symbol.len() < 256, "symbol too long");
        for &ts_us in &args.ts_us {
            let open_us = ts_us.div_euclid(60_000_000) * 60_000_000 - 60_000_000;
            let mut key = vec![symbol.len() as u8];
            key.extend_from_slice(symbol.as_bytes());
            key.extend_from_slice(&open_us.to_be_bytes());
            let close = match db.get_cf(cf, key)? {
                Some(value) => {
                    ensure!(value.len() == 56, "invalid cached candle size");
                    Some(f64::from_le_bytes(value[24..32].try_into()?))
                }
                None => None,
            };
            println!(
                "{}",
                json!({"symbol":symbol,"ts_us":ts_us,"open_ts_us":open_us,"close":close})
            );
        }
    }
    Ok(())
}
