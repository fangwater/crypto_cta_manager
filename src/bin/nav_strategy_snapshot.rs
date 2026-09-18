use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crypto_cta_manager::config::{AppConfig, SourceConfig};
use crypto_cta_manager::model::{UNIFORM_ORDERS_CF, UniformOrderEvent, decode_uniform_order};
use crypto_cta_manager::postgres;
use crypto_cta_manager::rocks_source;
use crypto_cta_manager::snapshot::{StrategyPositionSnapshot, StrategySnapshotPosition};
use crypto_cta_manager::viz_snapshot::{SourceStrategyAllocation, VizSnapshotClient};

const UNALLOCATED_STRATEGY: &str = "__unallocated__";
const UNATTRIBUTED_STRATEGY: &str = "__unattributed__";
const EPSILON: f64 = 1e-10;
const INFERRED_POSITION_MIN_NOTIONAL: f64 = 1.0;
const REPLAY_TOLERANCE_NOTIONAL: f64 = 1e-6;
const MAX_INFERENCE_ITERATIONS: usize = 100;
type StrategySymbol = (String, String);

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

    /// Required for manual --position input. With --infer-from-fills, use this as the anchor.
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

    /// Infer the initial allocation as current Exec allocation minus persisted factual fills.
    #[arg(long)]
    infer_from_fills: bool,

    /// Exec venue code for Viz-backed modes, for example 1 for Binance Futures.
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
    let selected_modes = usize::from(args.from_exec_viz)
        + usize::from(args.infer_from_fills)
        + usize::from(!args.positions.is_empty());
    if selected_modes != 1 {
        bail!("select exactly one of --position, --from-exec-viz, or --infer-from-fills");
    }
    let snapshot = if args.from_exec_viz || args.infer_from_fills {
        let venue_code = args
            .venue_code
            .context("--venue-code is required with a Viz-backed mode")?;
        let origin = source
            .exec_viz_origin()
            .context("source has no exec_viz_url configured")?;
        let client = VizSnapshotClient::new(config.order_config.request_timeout_secs)?;
        if args.infer_from_fills {
            snapshot_inferred_from_fills(source, venue_code, args.snapshot_ts_us, origin, &client)
                .await?
        } else {
            if args.snapshot_ts_us.is_some() {
                bail!("--snapshot-ts-us cannot be combined with --from-exec-viz");
            }
            let allocation = client.load_strategy_allocation(&source.id, origin).await?;
            snapshot_from_exec_allocation(&source.id, venue_code, allocation)?
        }
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
    if args.infer_from_fills {
        if let Some(existing) =
            postgres::load_latest_strategy_position_snapshot(&pool, &source.id).await?
        {
            bail!(
                "source {} already has an immutable strategy snapshot at {}; refusing to replace it",
                source.id,
                existing.snapshot_ts_us
            );
        }
        if let Some(account_snapshot) =
            postgres::load_latest_position_snapshot(&pool, &source.id).await?
        {
            validate_matching_account_snapshot(&snapshot, &account_snapshot)?;
        }
    }
    let default_note = args
        .infer_from_fills
        .then_some("inferred from current Exec strategy allocation minus persisted factual fills");
    postgres::create_strategy_position_snapshot(
        &pool,
        &snapshot,
        args.note.as_deref().or(default_note),
    )
    .await?;

    serde_json::to_writer_pretty(std::io::stdout().lock(), &snapshot)
        .context("failed to write stored strategy snapshot JSON")?;
    println!();
    Ok(())
}

async fn snapshot_inferred_from_fills(
    source: &SourceConfig,
    venue_code: i16,
    requested_snapshot_ts_us: Option<i64>,
    origin: &str,
    client: &VizSnapshotClient,
) -> Result<StrategyPositionSnapshot> {
    const MAX_ATTEMPTS: usize = 3;

    for attempt in 1..=MAX_ATTEMPTS {
        let mut records =
            rocks_source::read_all_column_families(&source.rocksdb_path, &[UNIFORM_ORDERS_CF])?;
        let records = records
            .remove(UNIFORM_ORDERS_CF)
            .context("uniform_orders disappeared after the read-only RocksDB scan")?;
        let scanned_latest_key = records.last().map(|record| record.key.clone());
        let events = records
            .into_iter()
            .map(|record| {
                decode_uniform_order(&record.key, &record.value).with_context(|| {
                    format!(
                        "source {} contains an undecodable uniform order at key {:?}",
                        source.id,
                        String::from_utf8_lossy(&record.key)
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let allocation = client.load_strategy_allocation(&source.id, origin).await?;
        let latest = rocks_source::read_latest_column_families(
            &source.rocksdb_path,
            &[UNIFORM_ORDERS_CF],
            1,
        )?;
        let observed_latest_key = latest
            .get(UNIFORM_ORDERS_CF)
            .and_then(|records| records.first())
            .map(|record| record.key.clone());
        if scanned_latest_key == observed_latest_key {
            return infer_snapshot_from_history(
                &source.id,
                venue_code,
                requested_snapshot_ts_us,
                allocation,
                &events,
            );
        }
        if attempt == MAX_ATTEMPTS {
            bail!(
                "source {} uniform_orders changed during all {MAX_ATTEMPTS} inference attempts; retry when the account is between fills",
                source.id
            );
        }
    }
    unreachable!()
}

fn infer_snapshot_from_history(
    source_id: &str,
    venue_code: i16,
    requested_snapshot_ts_us: Option<i64>,
    allocation: SourceStrategyAllocation,
    events: &[UniformOrderEvent],
) -> Result<StrategyPositionSnapshot> {
    validate_allocation_header(source_id, venue_code, &allocation)?;

    let allocation_ts_us = allocation
        .snapshot_ts_ms
        .checked_mul(1_000)
        .context("Exec Viz allocation timestamp overflowed microseconds")?;
    if requested_snapshot_ts_us.is_some_and(|timestamp| timestamp >= allocation_ts_us) {
        bail!("--snapshot-ts-us must be earlier than the Exec Viz allocation timestamp");
    }

    let earliest_fill_ts_us = events
        .iter()
        .filter(|event| event.venue_code == venue_code && event.amount_update > 0.0)
        .map(fifo_ts_us)
        .min()
        .context("the selected venue has no factual fills to infer from")?;
    let snapshot_ts_us = requested_snapshot_ts_us.unwrap_or_else(|| {
        earliest_fill_ts_us
            .checked_sub(1)
            .unwrap_or(earliest_fill_ts_us)
    });
    if snapshot_ts_us <= 0 {
        bail!("inferred strategy snapshot timestamp must be positive");
    }

    let mut strategy_current = BTreeMap::<StrategySymbol, f64>::new();
    let mut account_current = BTreeMap::<String, f64>::new();
    for row in allocation.rows {
        validate_finite_quantity(row.current_qty, "Exec strategy current_qty", &row.symbol)?;
        if let Some(account_qty) = row.account_position_qty {
            validate_finite_quantity(account_qty, "Exec account_position_qty", &row.symbol)?;
            if let Some(previous) = account_current.insert(row.symbol.clone(), account_qty)
                && !same_quantity(previous, account_qty)
            {
                bail!(
                    "Exec Viz allocation has inconsistent account_position_qty for {}",
                    row.symbol
                );
            }
        }
        if let Some(strategy_name) = direct_strategy_name(&row.strategy_name) {
            *strategy_current
                .entry((strategy_name, row.symbol))
                .or_default() += row.current_qty;
        }
    }

    let mut direct_strategy_fills = BTreeMap::<StrategySymbol, f64>::new();
    let mut account_fills = BTreeMap::<String, f64>::new();
    let mut first_prices = BTreeMap::<String, (i64, String, f64)>::new();
    let mut fill_events = Vec::new();
    for event in events {
        if event.venue_code != venue_code || fifo_ts_us(event) <= snapshot_ts_us {
            continue;
        }
        if !event.amount_update.is_finite() || event.amount_update < 0.0 {
            bail!(
                "source {source_id} record {} has invalid amount_update {}",
                event.record_key,
                event.amount_update
            );
        }
        if event.amount_update == 0.0 {
            continue;
        }
        if !event.price.is_finite() || event.price <= 0.0 {
            bail!(
                "source {source_id} record {} has invalid fill price {}",
                event.record_key,
                event.price
            );
        }
        if event.symbol.trim().is_empty() {
            bail!(
                "source {source_id} record {} has an empty symbol",
                event.record_key
            );
        }
        let side = match event.side_code {
            1 => 1.0,
            2 => -1.0,
            value => bail!(
                "source {source_id} record {} has unsupported side code {value}",
                event.record_key
            ),
        };
        let signed_quantity = side * event.amount_update;
        fill_events.push(event);
        *account_fills.entry(event.symbol.clone()).or_default() += signed_quantity;
        let candidate = (fifo_ts_us(event), event.record_key.clone(), event.price);
        let first_price = first_prices
            .entry(event.symbol.clone())
            .or_insert_with(|| candidate.clone());
        if (candidate.0, candidate.1.as_str()) < (first_price.0, first_price.1.as_str()) {
            *first_price = candidate;
        }
        if !is_system_close_from_key(&event.from_key_text) {
            let strategy_name = strategy_from_fill(event);
            *direct_strategy_fills
                .entry((strategy_name, event.symbol.clone()))
                .or_default() += signed_quantity;
        }
    }
    fill_events.sort_by(|left, right| {
        fifo_ts_us(left)
            .cmp(&fifo_ts_us(right))
            .then_with(|| left.event_ts_us.cmp(&right.event_ts_us))
            .then_with(|| left.record_key.cmp(&right.record_key))
    });

    let mut account_initial = BTreeMap::<String, f64>::new();
    let mut symbols = account_current.keys().cloned().collect::<BTreeSet<_>>();
    symbols.extend(account_fills.keys().cloned());
    for symbol in &symbols {
        account_initial.insert(
            symbol.clone(),
            account_current.get(symbol).copied().unwrap_or_default()
                - account_fills.get(symbol).copied().unwrap_or_default(),
        );
    }

    let mut solve_keys = strategy_current.keys().cloned().collect::<BTreeSet<_>>();
    solve_keys.extend(direct_strategy_fills.keys().cloned());
    let mut strategy_initial = BTreeMap::<StrategySymbol, f64>::new();
    for key in &solve_keys {
        strategy_initial.insert(
            key.clone(),
            strategy_current.get(key).copied().unwrap_or_default()
                - direct_strategy_fills.get(key).copied().unwrap_or_default(),
        );
    }
    rebalance_unallocated(&mut strategy_initial, &account_initial, &symbols);

    let mut converged = false;
    for _ in 0..MAX_INFERENCE_ITERATIONS {
        let replayed = replay_strategy_quantities(&strategy_initial, &fill_events);
        let mut max_difference_notional = 0.0_f64;
        for key in &solve_keys {
            let difference = strategy_current.get(key).copied().unwrap_or_default()
                - replayed.get(key).copied().unwrap_or_default();
            if difference.abs() <= EPSILON {
                continue;
            }
            let price = reference_price(&first_prices, &key.1)?;
            max_difference_notional = max_difference_notional.max(difference.abs() * price);
            *strategy_initial.entry(key.clone()).or_default() += difference;
        }
        rebalance_unallocated(&mut strategy_initial, &account_initial, &symbols);
        if max_difference_notional <= REPLAY_TOLERANCE_NOTIONAL {
            converged = true;
            break;
        }
    }
    if !converged {
        bail!(
            "source {source_id} initial-position inference did not converge after {MAX_INFERENCE_ITERATIONS} replay iterations"
        );
    }

    let replayed = replay_strategy_quantities(&strategy_initial, &fill_events);
    validate_replayed_current(
        source_id,
        &strategy_current,
        &account_current,
        &replayed,
        &first_prices,
        &symbols,
    )?;

    let mut positions = Vec::new();
    for ((strategy_name, symbol), quantity) in strategy_initial {
        if quantity.abs() <= EPSILON {
            continue;
        }
        let reference_price = reference_price(&first_prices, &symbol)?;
        if quantity.abs() * reference_price < INFERRED_POSITION_MIN_NOTIONAL {
            continue;
        }
        if strategy_name == UNATTRIBUTED_STRATEGY {
            bail!(
                "source {source_id} has a material unattributed inferred position for {symbol}; automatic strategy snapshot is unsafe"
            );
        }
        positions.push(StrategySnapshotPosition {
            strategy_name,
            symbol,
            venue_code,
            quantity,
            reference_price,
        });
    }

    if positions.is_empty() {
        bail!(
            "source {source_id} has no inferred initial position at or above {INFERRED_POSITION_MIN_NOTIONAL} USDT; no strategy snapshot is needed"
        );
    }

    let snapshot = StrategyPositionSnapshot {
        source_id: source_id.to_string(),
        snapshot_ts_us,
        positions,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn validate_allocation_header(
    source_id: &str,
    venue_code: i16,
    allocation: &SourceStrategyAllocation,
) -> Result<()> {
    if !allocation.position_ready || allocation.snapshot_ts_ms <= 0 {
        bail!("Exec Viz allocation state is not ready");
    }
    if allocation.source_id != source_id {
        bail!("Exec Viz allocation source does not match requested source");
    }
    if !(0..=u8::MAX as i16).contains(&venue_code) {
        bail!("--venue-code must be between 0 and 255");
    }
    Ok(())
}

fn direct_strategy_name(value: &str) -> Option<String> {
    (!value.eq_ignore_ascii_case("SYSTEM_POSITION_CLOSE")
        && crypto_cta_manager::order_config::validate_strategy_name(value).is_ok())
    .then(|| value.to_string())
}

fn is_system_close_from_key(from_key: &str) -> bool {
    from_key
        .strip_prefix("batch_exec:")
        .is_some_and(|name| name.eq_ignore_ascii_case("SYSTEM_POSITION_CLOSE"))
}

fn strategy_from_fill(event: &UniformOrderEvent) -> String {
    event
        .from_key_text
        .strip_prefix("batch_exec:")
        .and_then(direct_strategy_name)
        .unwrap_or_else(|| UNATTRIBUTED_STRATEGY.to_string())
}

fn rebalance_unallocated(
    initial: &mut BTreeMap<StrategySymbol, f64>,
    account_initial: &BTreeMap<String, f64>,
    symbols: &BTreeSet<String>,
) {
    for symbol in symbols {
        let unallocated_key = (UNALLOCATED_STRATEGY.to_string(), symbol.clone());
        initial.remove(&unallocated_key);
        let attributed = initial
            .iter()
            .filter(|((_, candidate_symbol), _)| candidate_symbol == symbol)
            .map(|(_, quantity)| quantity)
            .sum::<f64>();
        let unallocated = account_initial.get(symbol).copied().unwrap_or_default() - attributed;
        if unallocated.abs() > EPSILON {
            initial.insert(unallocated_key, unallocated);
        }
    }
}

/// Replay net strategy quantities forward from an anchor. System-close fills
/// move only `__unallocated__`, matching the residual ledger that
/// `SYSTEM_POSITION_CLOSE` trades on the Exec side; named strategy quantities
/// change only through their own `batch_exec:<strategy>` fills.
fn replay_strategy_quantities(
    initial: &BTreeMap<StrategySymbol, f64>,
    fill_events: &[&UniformOrderEvent],
) -> BTreeMap<StrategySymbol, f64> {
    let mut states = initial
        .iter()
        .filter(|(_, quantity)| quantity.abs() > EPSILON)
        .map(|(key, quantity)| (key.clone(), *quantity))
        .collect::<BTreeMap<_, _>>();
    for event in fill_events {
        let side = if event.side_code == 1 { 1.0 } else { -1.0 };
        let strategy = if is_system_close_from_key(&event.from_key_text) {
            UNALLOCATED_STRATEGY.to_string()
        } else {
            strategy_from_fill(event)
        };
        *states.entry((strategy, event.symbol.clone())).or_default() += side * event.amount_update;
    }
    states
        .into_iter()
        .filter(|(_, quantity)| quantity.abs() > EPSILON)
        .collect()
}

fn validate_replayed_current(
    source_id: &str,
    strategy_current: &BTreeMap<StrategySymbol, f64>,
    account_current: &BTreeMap<String, f64>,
    replayed: &BTreeMap<StrategySymbol, f64>,
    first_prices: &BTreeMap<String, (i64, String, f64)>,
    symbols: &BTreeSet<String>,
) -> Result<()> {
    let mut keys = strategy_current.keys().cloned().collect::<BTreeSet<_>>();
    keys.extend(
        replayed
            .keys()
            .filter(|(strategy, _)| strategy != UNALLOCATED_STRATEGY)
            .cloned(),
    );
    for key in keys {
        let expected = strategy_current.get(&key).copied().unwrap_or_default();
        let actual = replayed.get(&key).copied().unwrap_or_default();
        if (expected - actual).abs() <= EPSILON {
            continue;
        }
        let difference_notional =
            (expected - actual).abs() * reference_price(first_prices, &key.1)?;
        if difference_notional > REPLAY_TOLERANCE_NOTIONAL {
            bail!(
                "source {source_id} inferred replay does not reach current strategy quantity for {} {}: expected {expected}, got {actual}",
                key.0,
                key.1
            );
        }
    }
    for symbol in symbols {
        let actual = replayed
            .iter()
            .filter(|((_, candidate_symbol), _)| candidate_symbol == symbol)
            .map(|(_, quantity)| quantity)
            .sum::<f64>();
        let expected = account_current.get(symbol).copied().unwrap_or_default();
        if (expected - actual).abs() <= EPSILON {
            continue;
        }
        let difference_notional =
            (expected - actual).abs() * reference_price(first_prices, symbol)?;
        if difference_notional > REPLAY_TOLERANCE_NOTIONAL {
            bail!(
                "source {source_id} inferred replay does not reach current account quantity for {symbol}: expected {expected}, got {actual}"
            );
        }
    }
    Ok(())
}

fn fifo_ts_us(event: &UniformOrderEvent) -> i64 {
    if event.update_ts_us > 0 {
        event.update_ts_us
    } else {
        event.event_ts_us
    }
}

fn reference_price(
    first_prices: &BTreeMap<String, (i64, String, f64)>,
    symbol: &str,
) -> Result<f64> {
    first_prices
        .get(symbol)
        .map(|(_, _, price)| *price)
        .with_context(|| format!("no post-anchor factual fill price is available for {symbol}"))
}

fn validate_finite_quantity(value: f64, field: &str, symbol: &str) -> Result<()> {
    if !value.is_finite() {
        bail!("{field} for {symbol} must be finite, got {value}");
    }
    Ok(())
}

fn validate_matching_account_snapshot(
    strategy_snapshot: &StrategyPositionSnapshot,
    account_snapshot: &crypto_cta_manager::snapshot::PositionSnapshot,
) -> Result<()> {
    if strategy_snapshot.snapshot_ts_us != account_snapshot.snapshot_ts_us {
        bail!(
            "source {} already has an account snapshot at {}; rerun inference with --snapshot-ts-us {}",
            strategy_snapshot.source_id,
            account_snapshot.snapshot_ts_us,
            account_snapshot.snapshot_ts_us
        );
    }
    let inferred = strategy_snapshot.account_snapshot()?;
    let inferred_quantities = inferred
        .positions
        .iter()
        .map(|position| {
            (
                (position.symbol.as_str(), position.venue_code),
                position.quantity,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let stored_quantities = account_snapshot
        .positions
        .iter()
        .map(|position| {
            (
                (position.symbol.as_str(), position.venue_code),
                position.quantity,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut keys = inferred_quantities.keys().copied().collect::<Vec<_>>();
    keys.extend(stored_quantities.keys().copied());
    keys.sort();
    keys.dedup();
    for key in keys {
        let inferred = inferred_quantities.get(&key).copied().unwrap_or_default();
        let stored = stored_quantities.get(&key).copied().unwrap_or_default();
        if !same_quantity(inferred, stored) {
            bail!(
                "inferred account quantity for {} venue {} ({inferred}) does not match stored account snapshot ({stored})",
                key.0,
                key.1
            );
        }
    }
    Ok(())
}

fn snapshot_from_exec_allocation(
    source_id: &str,
    venue_code: i16,
    allocation: SourceStrategyAllocation,
) -> Result<StrategyPositionSnapshot> {
    validate_allocation_header(source_id, venue_code, &allocation)?;

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

    fn fill(
        ts_us: i64,
        strategy_name: Option<&str>,
        side_code: i16,
        quantity: f64,
        price: f64,
    ) -> UniformOrderEvent {
        let from_key_text = strategy_name
            .map(|name| format!("batch_exec:{name}"))
            .unwrap_or_else(|| "other_source".to_string());
        UniformOrderEvent {
            record_key: format!("{ts_us:020}"),
            event_ts_us: ts_us,
            recv_ts_us: ts_us,
            symbol: "BTCUSDT".to_string(),
            create_ts_us: ts_us,
            update_ts_us: ts_us,
            signal_ts_us: ts_us,
            submit_ts_us: ts_us,
            local_ts_us: ts_us,
            market_ts_us: ts_us,
            client_order_id: ts_us,
            venue_code: 1,
            venue: "binance-futures".to_string(),
            order_type_code: 1,
            order_type: "LIMIT".to_string(),
            side_code,
            side: if side_code == 1 { "BUY" } else { "SELL" }.to_string(),
            price,
            price_offset: 0.0,
            amount_initial: quantity,
            amount_update: quantity,
            status_code: 3,
            status: "FILLED".to_string(),
            from_key: from_key_text.as_bytes().to_vec(),
            from_key_text,
            bbo_spread: String::new(),
            signal_open: None,
            signal_hedge: None,
            wire_payload: Vec::new(),
        }
    }

    fn allocation(rows: Vec<(&str, f64)>, account_qty: f64) -> SourceStrategyAllocation {
        SourceStrategyAllocation {
            source_id: "trade01".to_string(),
            snapshot_ts_ms: 1_000,
            position_ready: true,
            rows: rows
                .into_iter()
                .map(|(strategy_name, current_qty)| StrategyAllocationRow {
                    strategy_name: strategy_name.to_string(),
                    symbol: "BTCUSDT".to_string(),
                    current_qty,
                    current_usdt: Some(current_qty * 120.0),
                    account_position_qty: Some(account_qty),
                })
                .collect(),
        }
    }

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

    #[test]
    fn infers_initial_strategy_positions_and_account_residual_from_fills() {
        let snapshot = infer_snapshot_from_history(
            "trade01",
            1,
            None,
            allocation(
                vec![
                    ("cta_a", 1.8),
                    ("cta_b", -0.4),
                    ("SYSTEM_POSITION_CLOSE", 0.5),
                ],
                1.9,
            ),
            &[
                fill(100, Some("cta_a"), 1, 2.0, 100.0),
                fill(200, Some("cta_a"), 2, 0.5, 110.0),
                fill(300, Some("cta_b"), 2, 1.0, 105.0),
            ],
        )
        .unwrap();

        assert_eq!(snapshot.snapshot_ts_us, 99);
        assert_eq!(snapshot.positions.len(), 3);
        let quantities = snapshot
            .positions
            .iter()
            .map(|position| (position.strategy_name.as_str(), position.quantity))
            .collect::<BTreeMap<_, _>>();
        assert!((quantities["cta_a"] - 0.3).abs() < 1e-12);
        assert!((quantities["cta_b"] - 0.6).abs() < 1e-12);
        assert!((quantities[UNALLOCATED_STRATEGY] - 0.5).abs() < 1e-12);
        assert!(
            snapshot
                .positions
                .iter()
                .all(|position| { (position.reference_price - 100.0).abs() < 1e-12 })
        );
        let account = snapshot.account_snapshot().unwrap();
        assert!((account.positions[0].quantity - 1.4).abs() < 1e-12);
    }

    #[test]
    fn system_close_is_replayed_into_the_unallocated_anchor() {
        let snapshot = infer_snapshot_from_history(
            "trade01",
            1,
            None,
            allocation(vec![("CTA_B", 1.0)], 1.0),
            &[fill(100, Some("SYSTEM_POSITION_CLOSE"), 2, 2.0, 100.0)],
        )
        .unwrap();

        assert_eq!(snapshot.positions.len(), 2, "{:?}", snapshot.positions);
        let quantities = snapshot
            .positions
            .iter()
            .map(|position| (position.strategy_name.as_str(), position.quantity))
            .collect::<BTreeMap<_, _>>();
        assert!((quantities["CTA_B"] - 1.0).abs() < 1e-12);
        assert!((quantities[UNALLOCATED_STRATEGY] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn explicit_anchor_uses_only_later_fills_and_the_first_later_price() {
        let snapshot = infer_snapshot_from_history(
            "trade01",
            1,
            Some(150),
            allocation(vec![("cta_a", 1.0)], 1.0),
            &[
                fill(100, Some("cta_a"), 1, 9.0, 90.0),
                fill(200, Some("cta_a"), 2, 0.5, 110.0),
            ],
        )
        .unwrap();

        assert_eq!(snapshot.snapshot_ts_us, 150);
        assert_eq!(snapshot.positions.len(), 1);
        assert_eq!(snapshot.positions[0].strategy_name, "cta_a");
        assert!((snapshot.positions[0].quantity - 1.5).abs() < 1e-12);
        assert!((snapshot.positions[0].reference_price - 110.0).abs() < 1e-12);
    }

    #[test]
    fn ignores_sub_dollar_floating_point_remainders() {
        let error = infer_snapshot_from_history(
            "trade01",
            1,
            None,
            allocation(vec![("cta_a", 1.000_001)], 1.000_001),
            &[fill(100, Some("cta_a"), 1, 1.0, 100.0)],
        )
        .unwrap_err();

        assert!(error.to_string().contains("no strategy snapshot is needed"));
    }
}
