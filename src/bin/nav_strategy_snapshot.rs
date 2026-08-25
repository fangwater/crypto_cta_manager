use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_cta_manager::config::AppConfig;
use crypto_cta_manager::postgres;
use crypto_cta_manager::snapshot::{StrategyPositionSnapshot, StrategySnapshotPosition};
use crypto_cta_manager::viz_snapshot::{SourceStrategyAllocation, VizSnapshotClient};

const UNALLOCATED_STRATEGY: &str = "__unallocated__";
const EPSILON: f64 = 1e-10;

#[derive(Clone, Debug)]
struct PositionArg(StrategySnapshotPosition);

impl FromStr for PositionArg {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let fields = value.split(':').collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(
                "expected STRATEGY:SYMBOL:VENUE_CODE:SIGNED_QUANTITY:REFERENCE_PRICE".to_string(),
            );
        }
        Ok(Self(StrategySnapshotPosition {
            strategy_name: fields[0].to_string(),
            symbol: fields[1].to_string(),
            venue_code: fields[2]
                .parse()
                .map_err(|error| format!("invalid venue code: {error}"))?,
            quantity: fields[3]
                .parse()
                .map_err(|error| format!("invalid quantity: {error}"))?,
            reference_price: fields[4]
                .parse()
                .map_err(|error| format!("invalid reference price: {error}"))?,
        }))
    }
}

#[derive(Debug, Parser)]
#[command(name = "nav_strategy_snapshot")]
#[command(about = "Store an immutable CTA strategy-allocation snapshot in PostgreSQL")]
struct Args {
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,

    #[arg(long)]
    source: String,

    /// Required for manual --position input. Omitted with --from-exec-viz.
    #[arg(long)]
    snapshot_ts_us: Option<i64>,

    #[arg(
        long = "position",
        value_name = "STRATEGY:SYMBOL:VENUE:QTY:REFERENCE_PRICE"
    )]
    positions: Vec<PositionArg>,

    /// Read one complete Exec Viz allocation snapshot from the source's configured loopback URL.
    #[arg(long)]
    from_exec_viz: bool,

    /// Exec venue code for --from-exec-viz, for example 1 for Binance Futures.
    #[arg(long)]
    venue_code: Option<i16>,

    /// Validate and print the immutable snapshot without connecting to PostgreSQL.
    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    note: Option<String>,
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
    if args.from_exec_viz && !args.positions.is_empty() {
        bail!("--from-exec-viz cannot be combined with --position");
    }
    if !args.from_exec_viz && args.positions.is_empty() {
        bail!("provide at least one --position or use --from-exec-viz");
    }
    let snapshot = if args.from_exec_viz {
        let venue_code = args
            .venue_code
            .context("--venue-code is required with --from-exec-viz")?;
        let origin = source
            .exec_viz_origin()
            .context("source has no exec_viz_url configured")?;
        let client = VizSnapshotClient::new(config.order_config.request_timeout_secs)?;
        let allocation = client.load_strategy_allocation(&source.id, origin).await?;
        snapshot_from_exec_allocation(&source.id, venue_code, allocation)?
    } else {
        StrategyPositionSnapshot {
            source_id: args.source,
            snapshot_ts_us: args
                .snapshot_ts_us
                .context("--snapshot-ts-us is required with --position")?,
            positions: args.positions.into_iter().map(|value| value.0).collect(),
        }
    };
    snapshot.validate()?;

    if args.dry_run {
        serde_json::to_writer_pretty(std::io::stdout().lock(), &snapshot)
            .context("failed to write validated strategy snapshot JSON")?;
        println!();
        return Ok(());
    }

    let database_url = config.database_url()?;
    let pool = postgres::connect(&database_url, config.database.max_connections).await?;
    postgres::migrate(&pool).await?;
    postgres::register_sources(&pool, &config.sources).await?;
    postgres::create_strategy_position_snapshot(&pool, &snapshot, args.note.as_deref()).await?;

    serde_json::to_writer_pretty(std::io::stdout().lock(), &snapshot)
        .context("failed to write stored strategy snapshot JSON")?;
    println!();
    Ok(())
}

fn snapshot_from_exec_allocation(
    source_id: &str,
    venue_code: i16,
    allocation: SourceStrategyAllocation,
) -> Result<StrategyPositionSnapshot> {
    if !allocation.position_ready || allocation.snapshot_ts_ms <= 0 {
        bail!("Exec Viz allocation state is not ready");
    }
    if allocation.source_id != source_id {
        bail!("Exec Viz allocation source does not match requested source");
    }
    if !(0..=u8::MAX as i16).contains(&venue_code) {
        bail!("--venue-code must be between 0 and 255");
    }

    #[derive(Default)]
    struct SymbolAllocation {
        account_qty: Option<f64>,
        reference_price: Option<f64>,
        strategy_quantities: Vec<(String, f64)>,
    }

    let mut by_symbol = BTreeMap::<String, SymbolAllocation>::new();
    for row in allocation.rows {
        let entry = by_symbol.entry(row.symbol.clone()).or_default();
        if let Some(account_qty) = row.account_position_qty {
            if let Some(previous) = entry.account_qty
                && !same_quantity(previous, account_qty)
            {
                bail!(
                    "Exec Viz allocation has inconsistent account_position_qty for {}",
                    row.symbol
                );
            }
            entry.account_qty = Some(account_qty);
        }
        if row.current_qty.abs() > EPSILON {
            let mark = row
                .current_usdt
                .map(|usdt| usdt.abs() / row.current_qty.abs())
                .filter(|price| price.is_finite() && *price > 0.0)
                .with_context(|| {
                    format!(
                        "Exec Viz allocation has no usable current_usdt mark for {}",
                        row.symbol
                    )
                })?;
            if let Some(previous) = entry.reference_price
                && !same_price(previous, mark)
            {
                bail!(
                    "Exec Viz allocation has inconsistent current mark for {}",
                    row.symbol
                );
            }
            entry.reference_price = Some(mark);
            if crypto_cta_manager::order_config::validate_strategy_name(&row.strategy_name).is_ok()
            {
                entry
                    .strategy_quantities
                    .push((row.strategy_name, row.current_qty));
            }
        }
    }

    let mut positions = Vec::new();
    for (symbol, allocation) in by_symbol {
        let account_qty = allocation.account_qty.with_context(|| {
            format!("Exec Viz allocation has no account_position_qty for {symbol}")
        })?;
        let strategy_qty = allocation
            .strategy_quantities
            .iter()
            .map(|(_, quantity)| quantity)
            .sum::<f64>();
        let residual_qty = account_qty - strategy_qty;
        if account_qty.abs() <= EPSILON && strategy_qty.abs() <= EPSILON {
            continue;
        }
        let reference_price = allocation
            .reference_price
            .with_context(|| format!("Exec Viz allocation has no usable mark for {symbol}"))?;
        for (strategy_name, quantity) in allocation.strategy_quantities {
            positions.push(StrategySnapshotPosition {
                strategy_name,
                symbol: symbol.clone(),
                venue_code,
                quantity,
                reference_price,
            });
        }
        if residual_qty.abs() > EPSILON {
            positions.push(StrategySnapshotPosition {
                strategy_name: UNALLOCATED_STRATEGY.to_string(),
                symbol,
                venue_code,
                quantity: residual_qty,
                reference_price,
            });
        }
    }

    let snapshot_ts_us = allocation
        .snapshot_ts_ms
        .checked_mul(1_000)
        .context("Exec Viz allocation timestamp overflowed microseconds")?;
    let snapshot = StrategyPositionSnapshot {
        source_id: source_id.to_string(),
        snapshot_ts_us,
        positions,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn same_quantity(left: f64, right: f64) -> bool {
    (left - right).abs() <= left.abs().max(right.abs()).max(1.0) * EPSILON
}

fn same_price(left: f64, right: f64) -> bool {
    (left - right).abs() <= left.abs().max(right.abs()).max(1.0) * 1e-5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto_cta_manager::viz_snapshot::StrategyAllocationRow;

    #[test]
    fn exec_allocation_preserves_the_account_total_and_isolates_system_residual() {
        let snapshot = snapshot_from_exec_allocation(
            "trade01",
            1,
            SourceStrategyAllocation {
                source_id: "trade01".to_string(),
                snapshot_ts_ms: 123,
                position_ready: true,
                rows: vec![
                    StrategyAllocationRow {
                        strategy_name: "cta_a".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        current_qty: 0.8,
                        current_usdt: Some(80.0),
                        account_position_qty: Some(1.0),
                    },
                    StrategyAllocationRow {
                        strategy_name: "SYSTEM_POSITION_CLOSE".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        current_qty: 0.2,
                        current_usdt: Some(20.0),
                        account_position_qty: Some(1.0),
                    },
                ],
            },
        )
        .unwrap();

        assert_eq!(snapshot.snapshot_ts_us, 123_000);
        assert_eq!(snapshot.positions.len(), 2);
        assert!(snapshot.positions.iter().any(|position| {
            position.strategy_name == "cta_a" && (position.quantity - 0.8).abs() < 1e-12
        }));
        assert!(snapshot.positions.iter().any(|position| {
            position.strategy_name == UNALLOCATED_STRATEGY
                && (position.quantity - 0.2).abs() < 1e-12
        }));
        let account = snapshot.account_snapshot().unwrap();
        assert!((account.positions[0].quantity - 1.0).abs() < 1e-12);
    }
}
