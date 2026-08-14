use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::reconcile;

#[derive(Debug, Parser)]
#[command(name = "fill_reconcile")]
#[command(about = "Compare uniform fill deltas with raw cumulative RocksDB updates")]
struct Args {
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    #[arg(long = "source")]
    source_ids: Vec<String>,

    #[arg(long)]
    compact: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = AppConfig::load(&args.config)?;
    let report = reconcile::reconcile_from_rocksdb(&config, &args.source_ids)?;

    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    if args.compact {
        serde_json::to_writer(&mut output, &report)
            .context("failed to write fill reconciliation JSON")?;
    } else {
        serde_json::to_writer_pretty(&mut output, &report)
            .context("failed to write fill reconciliation JSON")?;
    }
    writeln!(output).context("failed to finish fill reconciliation output")?;
    Ok(())
}
