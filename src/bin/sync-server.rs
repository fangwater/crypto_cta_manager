use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use crypto_cta_manager::native_futures::{config::Config, sync};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "CN futures read-only export and incremental local replication; no trading SDK or SQL"
)]
struct Args {
    #[arg(long, default_value = "config/cn-futures-manager.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Only validate paths and settings; no environment tokens or connections.
    Check,
    /// Cloud: serve read-only native records through a private export journal.
    Serve,
    /// Local: download changes into explicitly configured Manager replicas.
    Pull {
        #[arg(long)]
        once: bool,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let mode = crypto_cta_manager::runtime_mode::read(&args.config)?;
    ensure!(
        mode == crypto_cta_manager::runtime_mode::RuntimeMode::CnFutures,
        "sync-server currently supports mode = cn_futures; crypto keeps its existing synchronization path"
    );
    let config = Config::load(&args.config)?;
    match args.command {
        Command::Check => {
            println!("Native synchronization configuration valid; no connections or writes");
            Ok(())
        }
        Command::Pull { once } => sync::pull(config, once).await,
        Command::Serve => {
            let bind = config
                .sync_export
                .as_ref()
                .context("configure [sync_export]")?
                .bind;
            let service = sync::ExportService::new(config)?;
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind,"native read-only sync exporter listening");
            axum::serve(listener, service.router())
                .with_graceful_shutdown(sync::shutdown_signal())
                .await?;
            Ok(())
        }
    }
}
