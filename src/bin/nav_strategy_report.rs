use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::nav::{
    self, NavTimelineRequest, SourcePositionSnapshots, SourceStrategyPositionSnapshots,
};
use crypto_cta_manager::postgres;

/// Rebuild the NAV timeline for one source and print per-strategy and
/// per-symbol PnL summaries, exactly as the /api/timeline endpoint serves.
#[derive(Debug, Parser)]
#[command(name = "nav_strategy_report")]
struct Args {
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    #[arg(long)]
    source: String,

    /// Window end timestamp in microseconds. Defaults to latest available data.
    #[arg(long)]
    end_ts_us: Option<i64>,

    /// Window start timestamp in microseconds.
    #[arg(long)]
    start_ts_us: Option<i64>,

    /// Restrict the report to these symbols (repeatable).
    #[arg(long = "symbol")]
    symbols: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = AppConfig::load(&args.config)?;
    let source = config
        .sources
        .iter()
        .find(|source| source.id == args.source)
        .with_context(|| format!("source {} is not configured", args.source))?;
    if !source.enabled {
        bail!("source {} is disabled", source.id);
    }

    let database_url = config.database_url()?;
    let pool = postgres::connect(&database_url, config.database.max_connections).await?;

    let mut snapshots = SourcePositionSnapshots::new();
    let mut strategy_snapshots = SourceStrategyPositionSnapshots::new();
    if let Some(snapshot) = postgres::load_latest_position_snapshot(&pool, &source.id).await? {
        snapshots.insert(source.id.clone(), snapshot);
    }
    if let Some(snapshot) =
        postgres::load_latest_strategy_position_snapshot(&pool, &source.id).await?
    {
        strategy_snapshots.insert(source.id.clone(), snapshot);
    }

    let histories = nav::load_nav_source_histories(&config, &[source.id.clone()])?;

    let end_ts_us = match args.end_ts_us {
        Some(end_ts_us) => end_ts_us,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_micros()
            .try_into()
            .context("current timestamp exceeds i64 microseconds")?,
    };
    let report = nav::rebuild_nav_timeline_from_histories_with_strategy_snapshots(
        &config,
        NavTimelineRequest {
            start_ts_us: args.start_ts_us,
            end_ts_us,
            selected_source_ids: vec![source.id.clone()],
            selected_symbols: args.symbols.clone(),
            max_points: 10_000,
        },
        &snapshots,
        &strategy_snapshots,
        &histories,
    )?;

    println!("valuation: {}", report.valuation);
    println!("window: {} .. {}", report.start_ts_us, report.end_ts_us);
    println!("available_strategies: {:?}", report.available_strategies);
    println!();
    println!(
        "{:<48} {:>14} {:>14} {:>14} {:>14} {:>14} {:>10}",
        "strategy", "realized", "floating", "nav_chg_bf", "fee", "net_pos_usdt", "symbols"
    );
    for timeline in &report.strategy_points {
        let t = &timeline.summary;
        println!(
            "{:<48} {:>14.4} {:>14.4} {:>14.4} {:>14.4} {:>14.4} {:>10}",
            timeline.strategy,
            t.realized_pnl_before_fee_quote,
            t.floating_pnl_quote,
            t.nav_change_before_fee_quote,
            t.estimated_trading_fee_quote,
            timeline.net_position_value_quote,
            timeline.symbol_count,
        );
    }
    println!();
    println!("account-level summary: {:?}", report.summary);
    println!();

    println!(
        "{:<20} {:>14} {:>14} {:>14} {:>14}",
        "symbol", "realized", "floating", "net_qty", "net_pos_usdt"
    );
    for symbol in &report.symbols {
        println!(
            "{:<20} {:>14.4} {:>14.4} {:>14.4} {:>14.4}",
            symbol.symbol,
            symbol.totals.realized_pnl_before_fee_quote,
            symbol.totals.floating_pnl_quote,
            symbol.net_quantity,
            symbol.net_position_value_quote,
        );
    }

    // Alignment check: sum of strategy summaries vs account summary.
    let mut nav_bf = 0.0;
    let mut realized = 0.0;
    let mut floating = 0.0;
    for timeline in &report.strategy_points {
        nav_bf += timeline.summary.nav_change_before_fee_quote;
        realized += timeline.summary.realized_pnl_before_fee_quote;
        floating += timeline.summary.floating_pnl_quote;
    }
    println!();
    println!("sum(strategy nav_chg_bf): {nav_bf:.4}");
    println!(
        "account nav_chg_bf:       {:.4}",
        report.summary.nav_change_before_fee_quote
    );
    println!("sum(strategy realized):   {realized:.4}");
    println!(
        "account realized:         {:.4}",
        report.summary.realized_pnl_before_fee_quote
    );
    println!("sum(strategy floating):   {floating:.4}");
    println!(
        "account floating:         {:.4}",
        report.summary.floating_pnl_quote
    );
    Ok(())
}
