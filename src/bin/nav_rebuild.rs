use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::{nav, postgres};

#[derive(Debug, Parser)]
#[command(name = "nav_rebuild")]
#[command(about = "Rebuild CTA PnL from a PostgreSQL position snapshot and RocksDB fills")]
struct Args {
    /// Runtime configuration file.
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    /// Include only this enabled source ID. Repeat to select multiple sources.
    #[arg(long = "source")]
    source_ids: Vec<String>,

    /// Ignore PostgreSQL position snapshots and rebuild only from RocksDB fills.
    #[arg(long)]
    no_position_snapshot: bool,

    /// Emit compact JSON instead of pretty-printed JSON.
    #[arg(long)]
    compact: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = AppConfig::load(&args.config)?;
    let report = if args.no_position_snapshot {
        nav::rebuild_nav_from_rocksdb(&config, &args.source_ids)?
    } else {
        let database_url = config.database_url()?;
        let pool = postgres::connect(&database_url, config.database.max_connections).await?;
        let requested = args
            .source_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let mut snapshots = nav::SourcePositionSnapshots::new();
        for source in config.sources.iter().filter(|source| {
            source.enabled && (requested.is_empty() || requested.contains(source.id.as_str()))
        }) {
            if let Some(snapshot) =
                postgres::load_latest_position_snapshot(&pool, &source.id).await?
            {
                snapshots.insert(source.id.clone(), snapshot);
            }
        }
        nav::rebuild_nav_from_rocksdb_with_snapshots(&config, &args.source_ids, &snapshots)?
    };

    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    if args.compact {
        serde_json::to_writer(&mut output, &report).context("failed to write NAV report JSON")?;
    } else {
        serde_json::to_writer_pretty(&mut output, &report)
            .context("failed to write NAV report JSON")?;
    }
    writeln!(output).context("failed to finish NAV report output")?;
    Ok(())
}
