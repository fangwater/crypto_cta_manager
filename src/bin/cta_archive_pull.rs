//! Dedicated powerleader executable; its only remote route is direct public SSH.
use anyhow::{Result, ensure};
use clap::Parser;
use crypto_cta_manager::position_archive::cold::pull::{self, PullConfig};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Pull verified expired cold RocksDB segments over public IP; default is preview only"
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    host: String,
    /// Download and independently verify one segment; otherwise list only.
    #[arg(long)]
    run: bool,
    /// Ask the source to remove the exact segment AFTER durable local verification.
    #[arg(long, requires = "run")]
    reclaim_source: bool,
    /// Repeat at the configured hourly-or-slower interval.
    #[arg(long, requires = "run")]
    watch: bool,
}
#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let c = PullConfig::load(&a.config)?;
    let h = c.host(&a.host)?.clone();
    ensure!(a.config.is_absolute(), "pull config must be absolute");
    if a.watch {
        pull::watch(c, a.host, a.reclaim_source).await
    } else {
        let r = pull::once(
            &c,
            &h,
            a.run,
            a.reclaim_source,
            crypto_cta_manager::kline::now_us(),
        )?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        Ok(())
    }
}
