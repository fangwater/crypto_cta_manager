use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crypto_cta_manager::{config::AppConfig, ingest};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "crypto_cta_manager")]
#[command(about = "Ingest local CTA Exec order events into PostgreSQL")]
struct Args {
    /// Runtime configuration file.
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    /// Poll every enabled source once and exit.
    #[arg(long)]
    once: bool,

    /// Apply PostgreSQL migrations, register configured sources, and exit.
    #[arg(long)]
    migrate_only: bool,
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
    ingest::run(config, args.once, args.migrate_only).await
}
