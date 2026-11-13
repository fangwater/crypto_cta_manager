use anyhow::{Context, Result};
use clap::Parser;
use crypto_cta_manager::{
    native_futures::{config::Config, reader, web},
    postgres,
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    about = "Domestic futures account/strategy management and read-only execution observation; no SDK or order submission"
)]
struct Args {
    #[arg(long, default_value = "config/cn-futures-manager.toml")]
    config: PathBuf,
    #[arg(long, default_value = "127.0.0.1:18211")]
    bind: SocketAddr,
    /// Validate only; no database, SDK or network access.
    #[arg(long,conflicts_with_all=["snapshot","init_db"])]
    check_config: bool,
    /// Read execution RocksDB through separate secondaries and exit. No PostgreSQL required.
    #[arg(long, conflicts_with = "init_db")]
    snapshot: bool,
    /// Explicitly initialize a NEW empty Manager database, register sources and exit.
    #[arg(long)]
    init_db: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    if args.check_config {
        println!(
            "Domestic Manager configuration valid; {} sources; no connections started",
            config.sources.len()
        );
        return Ok(());
    }
    if args.snapshot {
        let mut observations = vec![];
        for source in config.sources.iter().filter(|s| s.enabled) {
            observations.push(match reader::read(source,&config.secondary_root.join(&source.id),config.recent_rows,config.stale_secs) {
                Ok(s)=>serde_json::to_value(s)?,
                Err(e)=>serde_json::json!({"source_id":source.id,"account":source.account,"status":"unavailable","error":format!("{e:#}")}),
            });
        }
        println!("{}", serde_json::to_string_pretty(&observations)?);
        return Ok(());
    }
    let url = std::env::var(&config.database.url_env)
        .context("domestic Manager database URL environment variable is not set")?;
    let pool = postgres::connect(&url, config.database.max_connections).await?;
    if args.init_db {
        postgres::initialize(&pool).await?;
        web::register_sources(&pool, &config).await?;
        return Ok(());
    }
    web::register_sources(&pool, &config).await?;
    web::serve(config, pool, args.bind).await
}
