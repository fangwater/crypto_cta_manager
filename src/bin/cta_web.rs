use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::web;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "cta_web")]
#[command(about = "Serve the CTA NAV dashboard API")]
struct Args {
    /// Runtime configuration file.
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    /// Loopback address used by the user-managed reverse proxy.
    #[arg(long, default_value = "127.0.0.1:18201")]
    bind: SocketAddr,

    /// Rebuild the cached dashboard at this interval. Defaults to ingestion.poll_interval_secs.
    #[arg(long)]
    refresh_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("crypto_cta_manager=info,tower_http=info")),
        )
        .init();

    let args = Args::parse();
    let config = AppConfig::load(&args.config)?;
    let refresh_secs = args
        .refresh_secs
        .unwrap_or(config.ingestion.poll_interval_secs);
    web::serve(config, args.bind, refresh_secs).await
}
