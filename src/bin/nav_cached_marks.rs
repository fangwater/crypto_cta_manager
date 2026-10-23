//! Inspect existing Manager minute marks without fetching or modifying anything.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use rocksdb::{DB, Direction, IteratorMode, Options};
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    manager_db: PathBuf,
    #[arg(long, required_unless_present_any = ["archive_status", "cache_coverage"])]
    ts_us: Vec<i64>,
    #[arg(long = "symbol", required_unless_present_any = ["archive_status", "target_source", "cache_coverage"])]
    symbols: Vec<String>,
    #[arg(long)]
    archive_status: bool,
    /// Summarize durable candle coverage by symbol, optionally within two timestamps.
    #[arg(long)]
    cache_coverage: bool,
    #[arg(long)]
    target_source: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut options = Options::default();
    options.set_max_open_files(256);
    let families = DB::list_cf(&options, &args.manager_db)?;
    let db = DB::open_cf_for_read_only(&options, &args.manager_db, families, false)?;
    if args.archive_status {
        let cf = db
            .cf_handle("position_updates")
            .context("no target archive")?;
        let boundary = |mode| -> Result<Option<i64>> {
            db.iterator_cf(cf, mode)
                .next()
                .transpose()?
                .map(|(key, _)| {
                    ensure!(key.len() >= 8, "invalid target archive key");
                    Ok(i64::from_be_bytes(key[..8].try_into()?))
                })
                .transpose()
        };
        println!(
            "{}",
            json!({
                "estimated_messages": db.property_int_value_cf(cf, "rocksdb.estimate-num-keys")?,
                "first_received_at_us": boundary(IteratorMode::Start)?,
                "last_received_at_us": boundary(IteratorMode::End)?,
            })
        );
        return Ok(());
    }
    let cf = db
        .cf_handle("klines_1m")
        .context("no minute candle cache")?;
    if args.cache_coverage {
        ensure!(
            args.ts_us.is_empty() || args.ts_us.len() == 2,
            "coverage requires zero or two --ts-us values"
        );
        let mut coverage = std::collections::BTreeMap::<String, (usize, i64, i64)>::new();
        for entry in db.iterator_cf(cf, IteratorMode::Start) {
            let (key, _) = entry?;
            ensure!(
                !key.is_empty() && key.len() == key[0] as usize + 9,
                "invalid candle key"
            );
            let symbol = std::str::from_utf8(&key[1..key.len() - 8])?;
            if !args.symbols.is_empty() && !args.symbols.iter().any(|s| s == symbol) {
                continue;
            }
            let open = i64::from_be_bytes(key[key.len() - 8..].try_into()?);
            if !args.ts_us.is_empty() && (open < args.ts_us[0] || open >= args.ts_us[1]) {
                continue;
            }
            let row = coverage.entry(symbol.to_owned()).or_insert((0, open, open));
            row.0 += 1;
            row.2 = open;
        }
        for (symbol, (count, first, last)) in coverage {
            println!(
                "{}",
                json!({"symbol":symbol,"candles":count,"first_open_ts_us":first,"last_open_ts_us":last})
            );
        }
        return Ok(());
    }
    if let Some(source) = &args.target_source {
        ensure!(
            args.ts_us.len() == 2,
            "target scan requires start and end --ts-us"
        );
        let targets = db
            .cf_handle("position_updates")
            .context("no target archive")?;
        let start_key = args.ts_us[0].to_be_bytes();
        let mut missing = std::collections::BTreeSet::new();
        let mut count = 0;
        for entry in db.iterator_cf(targets, IteratorMode::From(&start_key, Direction::Forward)) {
            let (key, value) = entry?;
            let received = i64::from_be_bytes(key[..8].try_into()?);
            if received > args.ts_us[1] {
                break;
            }
            let message: serde_json::Value = serde_json::from_slice(&value)?;
            for account in message["published_accounts"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if account["source_id"].as_str() != Some(source) {
                    continue;
                }
                count += 1;
                let target_map = account["target_projection"]["targets"]
                    .as_object()
                    .or_else(|| message["strategy"]["targets"].as_object());
                for symbol in target_map.into_iter().flat_map(|map| map.keys()) {
                    let open = received.div_euclid(60_000_000) * 60_000_000;
                    let next = ((received + 60_000_000 - 1) / 60_000_000) * 60_000_000;
                    for minute in [open, next, next + 60_000_000, next + 120_000_000] {
                        let mut candle_key = vec![symbol.len() as u8];
                        candle_key.extend_from_slice(symbol.as_bytes());
                        candle_key.extend_from_slice(&minute.to_be_bytes());
                        if db.get_cf(cf, candle_key)?.is_none()
                            && missing.insert((symbol.clone(), minute))
                        {
                            println!(
                                "{}",
                                json!({"symbol":symbol,"missing_open_ts_us":minute,"received_at_us":received})
                            );
                        }
                    }
                }
            }
        }
        println!(
            "{}",
            json!({"target_messages":count,"missing_candles":missing.len()})
        );
        return Ok(());
    }
    for symbol in &args.symbols {
        ensure!(symbol.len() < 256, "symbol too long");
        for &ts_us in &args.ts_us {
            let open_us = ts_us.div_euclid(60_000_000) * 60_000_000 - 60_000_000;
            let mut key = vec![symbol.len() as u8];
            key.extend_from_slice(symbol.as_bytes());
            key.extend_from_slice(&open_us.to_be_bytes());
            let (open, close) = match db.get_cf(cf, key)? {
                Some(value) => {
                    ensure!(value.len() == 56, "invalid cached candle size");
                    (
                        Some(f64::from_le_bytes(value[0..8].try_into()?)),
                        Some(f64::from_le_bytes(value[24..32].try_into()?)),
                    )
                }
                None => (None, None),
            };
            println!(
                "{}",
                json!({"symbol":symbol,"ts_us":ts_us,"open_ts_us":open_us,"open":open,"close":close})
            );
        }
    }
    Ok(())
}
