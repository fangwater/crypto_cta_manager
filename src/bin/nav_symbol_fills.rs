use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use crypto_cta_manager::model::{UNIFORM_ORDERS_CF, decode_uniform_order};
use crypto_cta_manager::nav::strategy_from_from_key;
use crypto_cta_manager::rocks_source;

/// Dump signed fill quantities per symbol/strategy from an Exec persist_manager
/// RocksDB, split around a snapshot anchor timestamp. Read-only diagnostic.
#[derive(Debug, Parser)]
#[command(name = "nav_symbol_fills")]
struct Args {
    #[arg(long)]
    rocksdb_path: PathBuf,

    /// Restrict output to these symbols. Empty means all symbols.
    #[arg(long = "symbol")]
    symbols: Vec<String>,

    /// Snapshot anchor in microseconds. Fills at or before it are "pre-anchor".
    #[arg(long)]
    anchor_ts_us: Option<i64>,

    /// Print every fill row instead of only the summary.
    #[arg(long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut records =
        rocks_source::read_available_column_families(&args.rocksdb_path, &[UNIFORM_ORDERS_CF])?;
    let records = records
        .remove(UNIFORM_ORDERS_CF)
        .context("uniform_orders column family missing")?;

    let mut events = Vec::new();
    for record in records {
        let event = decode_uniform_order(&record.key, &record.value)?;
        if event.amount_update <= 0.0 {
            continue;
        }
        if !args.symbols.is_empty() && !args.symbols.iter().any(|s| s == &event.symbol) {
            continue;
        }
        events.push(event);
    }
    let fifo_ts = |e: &crypto_cta_manager::model::UniformOrderEvent| {
        if e.update_ts_us > 0 {
            e.update_ts_us
        } else {
            e.event_ts_us
        }
    };
    events.sort_by(|a, b| {
        fifo_ts(a)
            .cmp(&fifo_ts(b))
            .then_with(|| a.event_ts_us.cmp(&b.event_ts_us))
            .then_with(|| a.record_key.cmp(&b.record_key))
    });

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    if args.verbose {
        for event in &events {
            let ts = fifo_ts(event);
            let signed = if event.side_code == 1 {
                event.amount_update
            } else {
                -event.amount_update
            };
            writeln!(
                out,
                "{ts}\t{}\t{signed}\t{}\t{}\t{}",
                event.symbol, event.price, event.from_key_text, event.record_key,
            )?;
        }
    }

    // symbol -> strategy -> (pre_qty, post_qty, post_fill_count)
    let mut summary = BTreeMap::<String, BTreeMap<String, (f64, f64, u64)>>::new();
    for event in &events {
        let ts = fifo_ts(event);
        let signed = match event.side_code {
            1 => event.amount_update,
            2 => -event.amount_update,
            _ => continue,
        };
        let strategy = strategy_from_from_key(&event.from_key_text);
        let entry = summary
            .entry(event.symbol.clone())
            .or_default()
            .entry(strategy)
            .or_default();
        match args.anchor_ts_us {
            Some(anchor) if ts <= anchor => entry.0 += signed,
            _ => {
                entry.1 += signed;
                entry.2 += 1;
            }
        }
    }

    writeln!(
        out,
        "symbol\tstrategy\tpre_anchor_qty\tpost_anchor_qty\tpost_fills"
    )?;
    for (symbol, strategies) in &summary {
        for (strategy, (pre, post, count)) in strategies {
            writeln!(out, "{symbol}\t{strategy}\t{pre}\t{post}\t{count}")?;
        }
    }
    Ok(())
}
