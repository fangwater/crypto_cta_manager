use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::postgres;
use crypto_cta_manager::snapshot::{PositionSnapshot, SnapshotPosition};

#[derive(Clone, Debug)]
struct PositionArg(SnapshotPosition);

impl FromStr for PositionArg {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let fields = value.split(':').collect::<Vec<_>>();
        if !(3..=4).contains(&fields.len()) {
            return Err("expected SYMBOL:VENUE_CODE:SIGNED_QUANTITY[:REFERENCE_PRICE]".to_string());
        }
        let venue_code = fields[1]
            .parse::<i16>()
            .map_err(|error| format!("invalid venue code: {error}"))?;
        let quantity = fields[2]
            .parse::<f64>()
            .map_err(|error| format!("invalid quantity: {error}"))?;
        let reference_price = fields
            .get(3)
            .map(|raw| {
                raw.parse::<f64>()
                    .map_err(|error| format!("invalid reference price: {error}"))
            })
            .transpose()?;
        Ok(Self(SnapshotPosition {
            symbol: fields[0].to_string(),
            venue_code,
            quantity,
            reference_price,
        }))
    }
}

#[derive(Debug, Parser)]
#[command(name = "nav_snapshot")]
#[command(about = "Store an immutable CTA position snapshot in PostgreSQL")]
struct Args {
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    #[arg(long)]
    source: String,

    #[arg(long)]
    snapshot_ts_us: i64,

    #[arg(
        long = "position",
        value_name = "SYMBOL:VENUE:QTY[:REFERENCE_PRICE]",
        required = true
    )]
    positions: Vec<PositionArg>,

    #[arg(long)]
    note: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = AppConfig::load(&args.config)?;
    let configured_source = config
        .sources
        .iter()
        .find(|source| source.id == args.source)
        .with_context(|| format!("source {} is not configured", args.source))?;
    if !configured_source.enabled {
        bail!("source {} is disabled", args.source);
    }
    let snapshot = PositionSnapshot {
        source_id: args.source,
        snapshot_ts_us: args.snapshot_ts_us,
        positions: args.positions.into_iter().map(|value| value.0).collect(),
    };
    snapshot.validate()?;

    let database_url = config.database_url()?;
    let pool = postgres::connect(&database_url, config.database.max_connections).await?;
    postgres::migrate(&pool).await?;
    postgres::register_sources(&pool, &config.sources).await?;
    postgres::create_position_snapshot(&pool, &snapshot, args.note.as_deref()).await?;

    serde_json::to_writer_pretty(std::io::stdout().lock(), &snapshot)
        .context("failed to write stored snapshot JSON")?;
    println!();
    Ok(())
}
