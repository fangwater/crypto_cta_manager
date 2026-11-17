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
    /// Operator-controlled finite batch; default one segment. Does not start a timer.
    #[arg(long, requires = "run", conflicts_with = "watch", value_parser = clap::value_parser!(u32).range(1..=4096))]
    max_segments: Option<u32>,
}
#[tokio::main]
async fn main() -> Result<()> {
    let a = Args::parse();
    let c = PullConfig::load(&a.config)?;
    let h = c.host(&a.host)?.clone();
    ensure!(a.config.is_absolute(), "pull config must be absolute");
    if a.watch {
        pull::watch(c, a.host, a.reclaim_source).await
    } else if a.max_segments.is_some_and(|n| n > 1) {
        let mut results = Vec::new();
        for _ in 0..a.max_segments.unwrap() {
            let r = pull::operator_once(
                &c,
                &h,
                true,
                a.reclaim_source,
                crypto_cta_manager::kline::now_us(),
            )?;
            if r.segment.is_none() {
                break;
            }
            results.push(r);
        }
        println!("{}", serde_json::to_string_pretty(&results)?);
        Ok(())
    } else {
        let r = pull::operator_once(
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
