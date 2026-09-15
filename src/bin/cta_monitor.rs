use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crypto_cta_manager::{config::AppConfig, monitor};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "cta_monitor")]
#[command(about = "Check CTA market, order, and position execution health")]
struct Args {
    /// Runtime configuration file.
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    /// Run one check and exit.
    #[arg(long)]
    once: bool,

    /// Print detected issues without reading the DingTalk webhook environment variable or sending.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let config = AppConfig::load(&args.config)?;
    monitor::run(config, args.once, args.dry_run).await
}
