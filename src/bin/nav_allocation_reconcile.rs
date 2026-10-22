//! Prepare/replay an allocation correction from an operator-captured Exec
//! snapshot and completed minute marks. Writing requires an explicit --apply.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use crypto_cta_manager::{config::AppConfig, nav, postgres};
use nav::allocation::{AllocationPosition, AllocationSnapshot};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    source: String,
    #[arg(long)]
    snapshot_json: Option<PathBuf>,
    /// Read-only Manager RocksDB holding the existing minute candles.
    #[arg(long)]
    manager_db: Option<PathBuf>,
    /// Validate and optionally insert this previously prepared audit record.
    #[arg(long)]
    candidate: Option<PathBuf>,
    #[arg(long)]
    apply: bool,
    #[arg(long)]
    note: Option<String>,
    #[arg(long)]
    verification_json: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.apply || args.candidate.is_some(),
        "--apply requires --candidate"
    );
    let mut config = AppConfig::load(&args.config)?;
    config.sources.retain(|s| s.id == args.source && s.enabled);
    ensure!(
        config.sources.len() == 1 && config.sources[0].venue == "binance-futures",
        "select one enabled Binance linear source"
    );
    let pool = postgres::connect(&config.database_url()?, 2).await?;
    config = config.with_fee_rates(&postgres::load_fee_rates(&pool).await?);
    ensure!(
        postgres::load_latest_strategy_position_snapshot(&pool, &args.source)
            .await?
            .is_none(),
        "source already has an initial strategy allocation; refusing ambiguous repair"
    );
    let mut snapshots = nav::SourcePositionSnapshots::new();
    if let Some(snapshot) = postgres::load_latest_position_snapshot(&pool, &args.source).await? {
        snapshots.insert(args.source.clone(), snapshot);
    }
    let mut histories = nav::load_nav_source_histories(&config, &[args.source.clone()])?;
    // Schema must be installed explicitly before the maintenance tool is used.
    nav::allocation::attach(&mut histories, &nav::allocation::load(&pool).await?);
    let cache = nav::NavTimelineCache::build(
        config.clone(),
        Arc::new(snapshots.clone()),
        Arc::default(),
        Arc::new(histories.clone()),
    )?;
    let candidate = if let Some(path) = &args.candidate {
        serde_json::from_slice::<AllocationSnapshot>(&std::fs::read(path)?)?
    } else {
        let value: Value = serde_json::from_slice(&std::fs::read(
            args.snapshot_json.context("--snapshot-json required")?,
        )?)?;
        let entry = value["entries"]
            .as_array()
            .context("snapshot entries missing")?
            .iter()
            .find(|e| e["channel"] == "exec_pre_trade_state")
            .context("Exec allocation missing")?["entry"]
            .clone();
        ensure!(
            entry["position_ready"] == true,
            "Exec positions are not ready"
        );
        let ts_us = entry["ts_ms"]
            .as_i64()
            .or(value["ts_ms"].as_i64())
            .context("snapshot time missing")?
            * 1000;
        let audit = cache.allocation_audit(&args.source, ts_us)?;
        let mut after = BTreeMap::new();
        let mut accounts = BTreeMap::<String, f64>::new();
        for row in entry["rows"]
            .as_array()
            .context("allocation rows missing")?
        {
            ensure!(
                row["active_batches"].as_u64().unwrap_or(0) == 0
                    && row["live_order_qty"].as_f64().unwrap_or(0.0).abs() < 1e-12,
                "capture a settled Exec snapshot without active orders"
            );
            let symbol = row["symbol"].as_str().context("symbol missing")?.to_owned();
            let mut strategy = row["strategy_name"]
                .as_str()
                .context("strategy missing")?
                .to_owned();
            if strategy.eq_ignore_ascii_case("SYSTEM_POSITION_CLOSE") {
                strategy = "system_position_close".into();
            }
            let qty = row["current_qty"]
                .as_f64()
                .context("strategy quantity missing")?;
            let qty = if qty.abs() < 1e-10 { 0.0 } else { qty };
            ensure!(
                after.insert((strategy, symbol.clone(), 1), qty).is_none(),
                "duplicate Exec allocation row"
            );
            let account = row["account_position_qty"]
                .as_f64()
                .context("account quantity missing")?;
            if let Some(old) = accounts.insert(symbol, account) {
                ensure!((old - account).abs() < 1e-10, "inconsistent account rows");
            }
        }
        let mut options = rocksdb::Options::default();
        options.set_max_open_files(128);
        let db_path = args.manager_db.context("--manager-db required")?;
        let families = rocksdb::DB::list_cf(&options, &db_path)?;
        let db = rocksdb::DB::open_cf_for_read_only(&options, &db_path, families, false)?;
        let cf = db.cf_handle("klines_1m").context("minute cache missing")?;
        let before: BTreeMap<_, _> = audit
            .strategies
            .iter()
            .map(|(s, m, v, q)| ((s.clone(), m.clone(), *v), *q))
            .collect();
        let keys = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut marks = BTreeMap::new();
        let mut positions = Vec::new();
        for (strategy, symbol, venue_code) in keys {
            let key = (strategy.clone(), symbol.clone(), venue_code);
            let before_qty = before.get(&key).copied().unwrap_or_default();
            let after_qty = after.get(&key).copied().unwrap_or_default();
            if (before_qty - after_qty).abs() < 1e-12 {
                continue;
            }
            ensure!(venue_code == 1, "unexpected event venue in linear source");
            let reference_price = if let Some(price) = marks.get(&symbol) {
                *price
            } else {
                let open = ts_us.div_euclid(60_000_000) * 60_000_000 - 60_000_000;
                let mut key = vec![u8::try_from(symbol.len())?];
                key.extend_from_slice(symbol.as_bytes());
                key.extend_from_slice(&open.to_be_bytes());
                let value = db.get_cf(cf, key)?.with_context(|| {
                    format!("missing closed minute mark for {symbol} at {open}")
                })?;
                ensure!(value.len() == 56, "invalid minute candle");
                let price = f64::from_le_bytes(value[24..32].try_into()?);
                marks.insert(symbol.clone(), price);
                price
            };
            positions.push(AllocationPosition {
                strategy,
                symbol,
                venue_code,
                before_qty,
                after_qty,
                reference_price,
            });
        }
        for (symbol, venue, quantity) in &audit.account {
            ensure!(*venue == 1, "unexpected account venue");
            let observed = accounts.get(symbol).copied().unwrap_or_default();
            ensure!(
                (observed - quantity).abs() < 1e-8,
                "account projection does not match Exec for {symbol}: replay={quantity} Exec={observed}"
            );
        }
        for (symbol, quantity) in &accounts {
            let reconstructed = audit
                .account
                .iter()
                .find(|(s, v, _)| s == symbol && *v == 1)
                .map_or(0.0, |(_, _, q)| *q);
            ensure!(
                (reconstructed - quantity).abs() < 1e-8,
                "Exec has unmatched account position for {symbol}"
            );
            let allocated: f64 = after
                .iter()
                .filter(|((_, s, v), _)| s == symbol && *v == 1)
                .map(|(_, q)| q)
                .sum();
            ensure!(
                (allocated - quantity).abs() < 1e-8,
                "Exec allocation does not add up for {symbol}"
            );
        }
        AllocationSnapshot {
            source_id: args.source.clone(),
            snapshot_ts_us: ts_us,
            positions,
        }
    };
    ensure!(
        candidate.source_id == args.source,
        "candidate source mismatch"
    );
    candidate.validate()?;
    let audit = cache.allocation_audit(&args.source, candidate.snapshot_ts_us)?;
    for p in &candidate.positions {
        let before = audit
            .strategies
            .iter()
            .find(|(s, m, v, _)| s == &p.strategy && m == &p.symbol && v == &p.venue_code)
            .map_or(0.0, |(_, _, _, q)| *q);
        ensure!(
            ((before - p.before_qty) * p.reference_price).abs() < 1e-6,
            "candidate no longer matches recovered historical projection"
        );
    }
    let mut revised = nav::allocation::load(&pool).await?;
    let list = revised.entry(args.source.clone()).or_default();
    Arc::make_mut(list).push(candidate.clone());
    nav::allocation::attach(&mut histories, &revised);
    let adjusted = nav::NavTimelineCache::build(
        config,
        Arc::new(snapshots),
        Arc::default(),
        Arc::new(histories),
    )?;
    let after = adjusted.allocation_audit(&args.source, candidate.snapshot_ts_us)?;
    ensure!(
        audit.account == after.account,
        "allocation changed account FIFO quantities"
    );
    let mut verification = Vec::new();
    let end = candidate.snapshot_ts_us + 1;
    let earliest = cache
        .start_ts_us(&args.source, end, false)
        .context("source history is empty")?;
    for (name, start) in [
        ("history", earliest),
        ("7d", earliest.max(end - 7 * 86_400_000_000)),
        ("1d", earliest.max(end - 86_400_000_000)),
        ("after", end),
    ] {
        let request = nav::NavTimelineRequest {
            start_ts_us: Some(start),
            end_ts_us: end,
            selected_source_ids: vec![args.source.clone()],
            selected_symbols: vec![],
            max_points: 3500,
        };
        let before_report = cache.rebuild(request.clone(), false)?;
        let after_report = adjusted.rebuild(request, false)?;
        ensure!(
            before_report.summary == after_report.summary
                && before_report.symbols == after_report.symbols
                && before_report.points == after_report.points,
            "allocation changes account report in {name}"
        );
        let strategy_sum: f64 = after_report
            .strategy_points
            .iter()
            .map(|s| s.summary.nav_change_after_fee_quote)
            .sum();
        ensure!(
            (strategy_sum - after_report.summary.nav_change_after_fee_quote).abs() < 1e-6,
            "strategy/account additivity failed"
        );
        verification.push(
            json!({"window":name,"account_unchanged":true,"report":after_report,
            "strategy_sum":strategy_sum}),
        );
    }
    if let Some(path) = &args.verification_json {
        std::fs::write(
            path,
            serde_json::to_vec(&json!({"before":audit,"after":after,"windows":verification}))?,
        )?;
    }
    if args.apply {
        nav::allocation::insert(
            &pool,
            &candidate,
            args.note.as_deref().context("--note required")?,
        )
        .await?;
        println!(
            "{}",
            json!({"inserted":true,"source_id":args.source,"snapshot_ts_us":candidate.snapshot_ts_us,"position_count":candidate.positions.len()})
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&candidate)?);
    }
    Ok(())
}
