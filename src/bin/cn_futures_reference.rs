//! Offline input maintenance and read-only cost reconstruction. Optional custom
//! native market-replay gRPC; no SQL, exchange account API or trading SDK.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crypto_cta_manager::native_futures::{
    config::Config,
    cost,
    market_data::Feed,
    now_us, reader,
    reference::{self, Pack},
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Maintain domestic futures reference evidence and compare execution costs using native market-replay gRPC; no SQL or trading SDK"
)]
struct Args {
    #[arg(long, default_value = "config/cn-futures-manager.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a pack without opening any execution database or writing files.
    Validate {
        #[arg(long)]
        input: PathBuf,
    },
    /// Atomically install a validated source/day pack in reference_root.
    Import {
        #[arg(long)]
        input: PathBuf,
        /// Explicitly replace a reviewed pack; preserve old and new revisions by hash.
        #[arg(long)]
        replace: bool,
    },
    /// Read the complete trading day's factual fills through a separate secondary.
    Report {
        #[arg(long)]
        source: String,
        #[arg(long)]
        trading_day: String,
    },
    /// Query provider-final native minute bars without opening an execution store.
    Market {
        #[arg(long)]
        venue: String,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        trading_day: String,
    },
    /// Read an investor-specific fee observation known at the requested time.
    Rates {
        #[arg(long)]
        source: String,
        #[arg(long)]
        venue: String,
        #[arg(long)]
        symbol: String,
        #[arg(long)]
        as_of: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Command::Validate { input } = &args.command {
        let pack = Pack::load(input)?;
        println!(
            "Reference valid: source={} trading_day={} bars={} signals={}; no connections or writes",
            pack.source_id,
            pack.trading_day,
            pack.bars.len(),
            pack.signals.len()
        );
        return Ok(());
    }
    let config = Config::load(&args.config)?;
    match args.command {
        Command::Validate { .. } => unreachable!(),
        Command::Rates {
            source,
            venue,
            symbol,
            as_of,
        } => {
            let settings = config
                .reference_service
                .context("reference_service is not configured")?;
            let feed = crypto_cta_manager::native_futures::reference_data::Feed::new(settings)?;
            let rates = feed
                .rates(&source, &venue, &symbol, reference::timestamp(&as_of)?)
                .await?;
            println!("{}", serde_json::to_string_pretty(&rates)?);
        }
        Command::Market {
            venue,
            symbol,
            trading_day,
        } => {
            let settings = config
                .market_data
                .context("market_data is not configured")?;
            let day = Feed::new(settings)?
                .day(&venue, &symbol, &trading_day, now_us())
                .await?;
            println!("{}", serde_json::to_string_pretty(&day)?);
        }
        Command::Import { input, replace } => {
            let pack = Pack::load(&input)?;
            let hash = reference::install(&config, &pack, replace)?;
            println!(
                "Reference installed: source={} trading_day={} sha256={hash}",
                pack.source_id, pack.trading_day
            );
        }
        Command::Report {
            source,
            trading_day,
        } => {
            let mut pack = if config.reference_root.is_some() {
                reference::load(&config, &source, &trading_day)?
            } else {
                None
            };
            let source_config = config
                .sources
                .iter()
                .find(|s| s.enabled && s.id == source)
                .context("source is not enabled")?;
            let facts = reader::read_cost_facts(
                source_config,
                &config.secondary_root.join(&source),
                &trading_day,
            )?;
            let mut market_error = None;
            if let (Some(settings), Some(pack)) = (&config.market_data, &mut pack) {
                if let Err(e) = Feed::new(settings.clone())?.augment(pack, now_us()).await {
                    eprintln!("Market data unavailable: {e:#}");
                    market_error = Some("行情接口或缓存暂不可用；保留已覆盖参考与事实成交".into());
                }
            }
            let mut report = cost::build(&source, &trading_day, pack.as_ref(), facts, now_us())?;
            report.market_data_error = market_error;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}
