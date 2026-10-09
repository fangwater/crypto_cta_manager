use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::{postgres, web};
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

    /// Rebuild the cached dashboard at this interval. Defaults to dashboard.refresh_secs.
    #[arg(long)]
    refresh_secs: Option<u64>,

    /// Initialize a new, empty Manager database, register sources, and exit.
    #[arg(long)]
    init_db: bool,

    /// Read-only BNB balance/settings preview; does not start Manager or trading.
    #[arg(long, conflicts_with = "init_db")]
    bnb_preview_source: Option<String>,
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
    if let Some(source_id) = &args.bnb_preview_source {
        let status = crypto_cta_manager::bnb_auto::preview_source(&config, source_id).await?;
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    if args.init_db {
        let database_url = config.database_url()?;
        let pool = postgres::connect(&database_url, config.database.max_connections).await?;
        postgres::initialize(&pool).await?;
        postgres::register_sources(&pool, &config.sources).await?;
        tracing::info!(
            sources = config.sources.len(),
            "Manager schema initialization and source registration complete"
        );
        return Ok(());
    }
    let refresh_secs = args.refresh_secs.unwrap_or(config.dashboard.refresh_secs);
    web::serve(config, args.bind, refresh_secs).await
}
