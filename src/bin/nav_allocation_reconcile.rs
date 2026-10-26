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
    /// New diagnostic directory outside all Manager/Exec stores.
    #[arg(long)]
    manager_secondary: Option<PathBuf>,
    /// Validate and optionally insert this previously prepared audit record.
    #[arg(long)]
    candidate: Option<PathBuf>,
    #[arg(long)]
    apply: bool,
    #[arg(long)]
    note: Option<String>,
    #[arg(long)]
    verification_json: Option<PathBuf>,
    /// Save the complete source projection before checking observed ownership.
    #[arg(long)]
    audit_json: Option<PathBuf>,
    /// Same-source signed account observations bracketing the settled Viz time.
    #[arg(long)]
    account_json: Option<PathBuf>,
    /// Complete, unchanged Batch/Chase ownership ledgers around the same capture.
    #[arg(long)]
    ledger_json: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.apply || args.candidate.is_some(),
        "--apply requires --candidate"
    );
    ensure!(
        args.account_json.is_none() || args.candidate.is_none(),
        "--account-json is only used when preparing from --snapshot-json"
    );
    ensure!(
        args.account_json.is_some() == args.ledger_json.is_some(),
        "signed account coverage requires the matching complete ownership ledger"
    );
    let mut config = AppConfig::load(&args.config)?;
    config.sources.retain(|s| s.id == args.source && s.enabled);
    ensure!(
        config.sources.len() == 1 && config.sources[0].venue == "binance-futures",
        "select one enabled Binance linear source"
    );
    let pool = postgres::connect(&config.database_url()?, 2).await?;
    config = config.with_fee_rates(&postgres::load_fee_rates(&pool).await?);
    let mut strategy_snapshots = nav::SourceStrategyPositionSnapshots::new();
    if let Some(snapshot) =
        postgres::load_latest_strategy_position_snapshot(&pool, &args.source).await?
    {
        strategy_snapshots.insert(args.source.clone(), snapshot);
    }
    let strategy_allocation = strategy_snapshots.contains_key(&args.source);
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
        Arc::new(strategy_snapshots.clone()),
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
        if let Some(path) = &args.audit_json {
            std::fs::write(path, serde_json::to_vec_pretty(&audit)?)?;
        }
        let (mut after, mut accounts) = observed_allocations(&entry, strategy_allocation)?;
        if let Some(path) = &args.ledger_json {
            let proof: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            after = merge_ledger_ownership(
                &proof,
                &entry,
                &args.source,
                ts_us / 1000,
                &audit.strategies,
                after,
            )?;
            if !strategy_allocation {
                let keys: Vec<_> = after
                    .keys()
                    .filter(|(s, _, _)| s == "__unallocated__")
                    .cloned()
                    .collect();
                for key in keys {
                    let qty = after.remove(&key).unwrap();
                    after.insert(("system_position_close".into(), key.1, key.2), qty);
                }
            }
        }
        if let Some(path) = &args.account_json {
            let proof: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            accounts =
                verified_account_observations(&proof, &args.source, ts_us / 1000, &accounts)?;
            let residual_owner = if strategy_allocation {
                "__unallocated__"
            } else {
                "system_position_close"
            };
            for (owner, symbol, venue, quantity) in &audit.strategies {
                if owner.starts_with("__") && owner != "__unallocated__" {
                    after
                        .entry((owner.clone(), symbol.clone(), *venue))
                        .or_insert(*quantity);
                }
            }
            // Preserve the named Exec ownership exactly. The account residual
            // is derived from authoritative quantities, never a second ledger
            // rounded through the Viz account-position f32 representation.
            for (symbol, quantity) in &accounts {
                let named: f64 = after
                    .iter()
                    .filter(|((owner, market, _), _)| owner != residual_owner && market == symbol)
                    .map(|(_, q)| q)
                    .sum();
                after.insert((residual_owner.into(), symbol.clone(), 1), quantity - named);
            }
        }
        // Check coverage and quantities before looking up prices, so missing
        // candles cannot hide a missing-fill/account-observation failure.
        for (symbol, venue, quantity) in &audit.account {
            ensure!(*venue == 1, "unexpected account venue");
            let observed = accounts.get(symbol).copied().with_context(|| {
                format!(
                    "Exec snapshot has no account observation for {symbol}; missing is not zero"
                )
            })?;
            ensure!(
                (observed - quantity).abs() < 1e-8,
                "account projection does not match Exec for {symbol}: replay={quantity} Exec={observed}"
            );
        }
        let mut options = rocksdb::Options::default();
        options.set_max_open_files(-1);
        let db_path = args
            .manager_db
            .context("--manager-db required")?
            .canonicalize()?;
        let secondary = args
            .manager_secondary
            .context("--manager-secondary required")?;
        ensure!(
            secondary.is_absolute() && !secondary.exists(),
            "use a new absolute secondary path"
        );
        let parent = secondary
            .parent()
            .context("secondary parent missing")?
            .canonicalize()?;
        let secondary = parent.join(secondary.file_name().context("secondary name missing")?);
        ensure!(
            !secondary.starts_with(&db_path) && !db_path.starts_with(&secondary),
            "secondary overlaps Manager store"
        );
        for source in &config.sources {
            let exec = source.rocksdb_path.canonicalize()?;
            ensure!(
                !secondary.starts_with(&exec) && !exec.starts_with(&secondary),
                "secondary overlaps Exec store"
            );
        }
        let db = rocksdb::DB::open_cf_as_secondary(
            &options,
            &db_path,
            &secondary,
            ["default", "klines_1m"],
        )?;
        let cf = db.cf_handle("klines_1m").context("minute cache missing")?;
        let before: BTreeMap<_, _> = audit
            .strategies
            .iter()
            .map(|(s, m, v, q)| ((s.clone(), m.clone(), *v), *q))
            .collect();
        if strategy_allocation {
            // The residual is audited explicitly and applied by conservation.
            for symbol in accounts.keys() {
                after
                    .entry(("__unallocated__".into(), symbol.clone(), 1))
                    .or_default();
            }
        }
        let keys = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let changed_markets = keys
            .iter()
            .filter(|key| {
                (before.get(*key).copied().unwrap_or_default()
                    - after.get(*key).copied().unwrap_or_default())
                .abs()
                    >= 1e-12
            })
            .map(|(_, s, v)| (s.clone(), *v))
            .collect::<std::collections::BTreeSet<_>>();
        let mut marks = BTreeMap::new();
        let mut positions = Vec::new();
        for (strategy, symbol, venue_code) in keys {
            if !changed_markets.contains(&(symbol.clone(), venue_code)) {
                continue;
            }
            let key = (strategy.clone(), symbol.clone(), venue_code);
            let before_qty = before.get(&key).copied().unwrap_or_default();
            let after_qty = after.get(&key).copied().unwrap_or_default();
            if (before_qty - after_qty).abs() < 1e-12 && strategy != "__unallocated__" {
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
            restatements: Vec::new(),
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
        Arc::new(strategy_snapshots),
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
        .start_ts_us(&args.source, end, strategy_allocation)
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
        let before_report = cache.rebuild(request.clone(), strategy_allocation)?;
        let after_report = adjusted.rebuild(request, strategy_allocation)?;
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

// Strategy ownership is scoped by symbol and event venue; execution families
// are summed only after each distinct Viz row has been checked.
type ObservedPositions = BTreeMap<(String, String, i16), f64>;
fn merge_ledger_ownership(
    proof: &Value,
    entry: &Value,
    source: &str,
    ts_ms: i64,
    before: &[(String, String, i16, f64)],
    mut observed: ObservedPositions,
) -> Result<ObservedPositions> {
    ensure!(
        proof["source_id"] == source,
        "ownership ledger source mismatch"
    );
    let start = proof["start_ms"]
        .as_i64()
        .context("ownership capture start missing")?;
    let end = proof["end_ms"]
        .as_i64()
        .context("ownership capture end missing")?;
    ensure!(
        start > 0 && start <= ts_ms && ts_ms <= end && end - start <= 15_000,
        "ownership observations must bracket the settled Viz timestamp"
    );
    let mut rows = BTreeMap::new();
    for family in ["batch", "chase"] {
        ensure!(
            proof["keys"][family]
                == format!("{source}:binance-futures:{family}_exec_state:position_allocations"),
            "ownership ledger namespace mismatch"
        );
        let first = &proof["before"][family];
        let last = &proof["after"][family];
        if first.is_null() && last.is_null() {
            ensure!(
                entry["rows"]
                    .as_array()
                    .context("Viz ownership missing")?
                    .iter()
                    .all(|row| {
                        row["algorithm"]
                            != if family == "batch" {
                                "batch"
                            } else {
                                "chase_exec"
                            }
                    }),
                "absent ledger has active Viz ownership"
            );
            continue;
        }
        ensure!(
            first["positions"] == last["positions"],
            "ownership changed across capture"
        );
        for (ledger, limit) in [(first, ts_ms), (last, end)] {
            let updated = ledger["updated_at_us"]
                .as_i64()
                .context("ledger timestamp missing")?;
            ensure!(
                updated > 0 && updated <= limit * 1000,
                "ownership ledger has a future timestamp"
            );
            ensure!(
                ledger["inverse_notionals"].is_null()
                    || ledger["inverse_notionals"]
                        .as_object()
                        .is_some_and(|x| x.is_empty()),
                "linear ownership ledger contains inverse contracts"
            );
        }
        for (owner, positions) in first["positions"]
            .as_object()
            .context("complete ownership positions missing")?
        {
            ensure!(!owner.is_empty(), "empty ledger strategy");
            for (symbol, quantity) in positions
                .as_object()
                .context("invalid ownership market map")?
            {
                ensure!(
                    (symbol.ends_with("USDT") || symbol.ends_with("USDC"))
                        && crypto_cta_manager::order_config::validate_exec_symbol(symbol).is_ok(),
                    "unexpected ownership market"
                );
                let quantity = quantity.as_f64().context("ownership quantity missing")?;
                ensure!(quantity.is_finite(), "non-finite ownership quantity");
                rows.insert((family.to_owned(), owner.clone(), symbol.clone()), quantity);
            }
        }
    }
    for row in entry["rows"].as_array().context("Viz ownership missing")? {
        let key = (
            match row["algorithm"].as_str().context("Viz algorithm missing")? {
                "batch" => "batch",
                "chase_exec" => "chase",
                _ => anyhow::bail!("unknown Viz execution family"),
            }
            .to_owned(),
            row["strategy_name"]
                .as_str()
                .context("Viz owner missing")?
                .to_owned(),
            row["symbol"]
                .as_str()
                .context("Viz symbol missing")?
                .to_owned(),
        );
        let quantity = rows
            .get(&key)
            .context("complete ledger does not cover a Viz owner")?;
        ensure!(
            (quantity
                - row["current_qty"]
                    .as_f64()
                    .context("Viz quantity missing")?)
            .abs()
                < 1e-8,
            "Viz and durable ownership differ for {} / {}",
            key.1,
            key.2
        );
    }
    let managed: std::collections::BTreeSet<_> = rows.keys().map(|(_, _, s)| s.clone()).collect();
    let mut complete = ObservedPositions::new();
    for ((_, mut owner, symbol), quantity) in rows {
        if owner.eq_ignore_ascii_case("SYSTEM_POSITION_CLOSE") {
            owner = "__unallocated__".into();
        }
        // Apply the same numerical-zero convention as the Viz parser. The
        // full-source signed account comparison below keeps its strict 1e-8 gate.
        let quantity = if quantity.abs() < 1e-10 {
            0.0
        } else {
            quantity
        };
        *complete.entry((owner, symbol, 1)).or_default() += quantity;
    }
    observed.extend(complete);
    // A complete managed-market ledger includes retained zero positions for
    // retired targets. Markets absent from both ledgers are outside this
    // ownership observation; keep their historical owners, including genuine
    // opposing holdings in a flat account. An omitted market is never zero.
    for (owner, symbol, venue, quantity) in before {
        if owner != "__unallocated__" && !managed.contains(symbol) {
            observed.insert((owner.clone(), symbol.clone(), *venue), *quantity);
        }
    }
    Ok(observed)
}
fn verified_account_observations(
    proof: &Value,
    source: &str,
    ts_ms: i64,
    viz: &BTreeMap<String, f64>,
) -> Result<BTreeMap<String, f64>> {
    ensure!(
        proof["source_id"] == source,
        "account observation source mismatch"
    );
    let start = proof["start_ms"]
        .as_i64()
        .context("account observation start missing")?;
    let end = proof["end_ms"]
        .as_i64()
        .context("account observation end missing")?;
    ensure!(
        start > 0 && start <= ts_ms && ts_ms <= end && end - start <= 15_000,
        "account observations must closely bracket the Viz timestamp"
    );
    let parse = |name: &str| -> Result<BTreeMap<String, f64>> {
        let mut positions = BTreeMap::new();
        for row in proof[name]
            .as_array()
            .context("account position observations missing")?
        {
            ensure!(
                row["positionSide"] == "BOTH",
                "one-way account observations required"
            );
            let symbol = row["symbol"].as_str().context("account symbol missing")?;
            let quantity: f64 = row["positionAmt"]
                .as_str()
                .context("account decimal quantity missing")?
                .parse()?;
            ensure!(
                !symbol.is_empty() && quantity.is_finite(),
                "invalid account position"
            );
            ensure!(
                positions.insert(symbol.to_owned(), quantity).is_none(),
                "duplicate account position"
            );
        }
        ensure!(!positions.is_empty(), "empty account observation");
        Ok(positions)
    };
    let positions = parse("positions_before")?;
    ensure!(
        positions == parse("positions_after")?,
        "account quantities changed across Viz capture"
    );
    for (symbol, quantity) in viz {
        let exact = positions
            .get(symbol)
            .context("account observation does not cover Viz symbol")?;
        ensure!(
            (exact - quantity).abs() < 1e-10 || (f64::from(*exact as f32) - quantity).abs() < 1e-10,
            "Viz quantity is inconsistent with signed account observation for {symbol}"
        );
    }
    Ok(positions)
}
fn observed_allocations(
    entry: &Value,
    strategy_allocation: bool,
) -> Result<(ObservedPositions, BTreeMap<String, f64>)> {
    let mut after = BTreeMap::new();
    let mut accounts = BTreeMap::<String, f64>::new();
    let mut observed_rows = std::collections::BTreeSet::new();
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
            strategy = if strategy_allocation {
                "__unallocated__"
            } else {
                "system_position_close"
            }
            .into();
        }
        let qty = row["current_qty"]
            .as_f64()
            .context("strategy quantity missing")?;
        let qty = if qty.abs() < 1e-10 { 0.0 } else { qty };
        // Viz exposes BatchExec and ChaseExec ownership separately. NAV
        // groups both families under the same strategy, including their
        // independent SYSTEM_POSITION_CLOSE rows.
        ensure!(
            observed_rows.insert((
                strategy.clone(),
                symbol.clone(),
                row["algorithm"].as_str().unwrap_or_default().to_owned()
            )),
            "duplicate Exec allocation row for the same algorithm"
        );
        *after.entry((strategy, symbol.clone(), 1)).or_default() += qty;
        let account = row["account_position_qty"]
            .as_f64()
            .context("account quantity missing")?;
        if let Some(old) = accounts.insert(symbol, account) {
            ensure!((old - account).abs() < 1e-10, "inconsistent account rows");
        }
    }
    Ok((after, accounts))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ledger_proof() -> Value {
        let batch = json!({"updated_at_us":1_000_000,"inverse_notionals":{},
            "positions":{"alpha":{"BTCUSDT":1.0,"OLDUSDT":0.0}}});
        let chase = json!({"updated_at_us":1_000_000,"inverse_notionals":{},
            "positions":{"beta":{"BTCUSDT":-1.0}}});
        json!({"source_id":"source","start_ms":1000,"end_ms":2000,
            "keys":{"batch":"source:binance-futures:batch_exec_state:position_allocations",
                "chase":"source:binance-futures:chase_exec_state:position_allocations"},
            "before":{"batch":batch,"chase":chase},"after":{"batch":batch,"chase":chase}})
    }
    #[test]
    fn absent_unused_family_is_explicit_and_cannot_hide_viz_ownership() {
        let mut proof = ledger_proof();
        proof["before"]["chase"] = Value::Null;
        proof["after"]["chase"] = Value::Null;
        assert!(
            merge_ledger_ownership(
                &proof,
                &json!({"rows":[]}),
                "source",
                1100,
                &[],
                BTreeMap::new()
            )
            .is_ok()
        );
        assert!(
            merge_ledger_ownership(
                &proof,
                &json!({"rows":[{"algorithm":"chase_exec"}]}),
                "source",
                1100,
                &[],
                BTreeMap::new()
            )
            .is_err()
        );
        proof["after"]["chase"] = ledger_proof()["after"]["chase"].clone();
        assert!(
            merge_ledger_ownership(
                &proof,
                &json!({"rows":[]}),
                "source",
                1100,
                &[],
                BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn complete_ledger_keeps_opposing_positions_and_unobserved_legacy_ownership() {
        let before = vec![
            ("alpha".into(), "ZILUSDT".into(), 1, -42.0),
            ("__unattributed__".into(), "OTHERUSDT".into(), 1, 2.0),
        ];
        let result = merge_ledger_ownership(
            &ledger_proof(),
            &json!({"rows":[{"algorithm":"chase_exec","strategy_name":"beta",
                "symbol":"BTCUSDT","current_qty":-1.0}]}),
            "source",
            1500,
            &before,
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(result[&("alpha".into(), "BTCUSDT".into(), 1)], 1.0);
        assert_eq!(result[&("beta".into(), "BTCUSDT".into(), 1)], -1.0);
        assert_eq!(result[&("alpha".into(), "OLDUSDT".into(), 1)], 0.0);
        assert_eq!(result[&("alpha".into(), "ZILUSDT".into(), 1)], -42.0);
        assert_eq!(
            result[&("__unattributed__".into(), "OTHERUSDT".into(), 1)],
            2.0
        );
    }
    #[test]
    fn complete_ledger_refuses_changed_wrong_namespace_or_conflicting_viz_ownership() {
        let entry = json!({"rows":[{"algorithm":"batch","strategy_name":"alpha",
            "symbol":"BTCUSDT","current_qty":2.0}]});
        assert!(
            merge_ledger_ownership(
                &ledger_proof(),
                &entry,
                "source",
                1500,
                &[],
                BTreeMap::new()
            )
            .is_err()
        );
        let mut proof = ledger_proof();
        proof["after"]["batch"]["positions"]["alpha"]["BTCUSDT"] = json!(2.0);
        assert!(
            merge_ledger_ownership(
                &proof,
                &json!({"rows":[]}),
                "source",
                1500,
                &[],
                BTreeMap::new()
            )
            .is_err()
        );
        proof = ledger_proof();
        proof["keys"]["batch"] = json!("other:position_allocations");
        assert!(
            merge_ledger_ownership(
                &proof,
                &json!({"rows":[]}),
                "source",
                1500,
                &[],
                BTreeMap::new()
            )
            .is_err()
        );
    }
    fn account_proof() -> Value {
        let positions = json!([
            {"symbol":"BTCUSDT", "positionAmt":"0.1", "positionSide":"BOTH"},
            {"symbol":"OLDUSDT", "positionAmt":"0", "positionSide":"BOTH"}
        ]);
        json!({"source_id":"source", "start_ms":1000, "end_ms":2000,
            "positions_before":positions, "positions_after":positions})
    }
    #[test]
    fn signed_account_proof_keeps_exact_decimals_and_explicit_zero_coverage() {
        let viz = BTreeMap::from([("BTCUSDT".into(), f64::from(0.1f32))]);
        let quantities =
            verified_account_observations(&account_proof(), "source", 1500, &viz).unwrap();
        assert_eq!(quantities["BTCUSDT"], 0.1);
        assert_eq!(quantities.get("OLDUSDT"), Some(&0.0));
        assert!(!quantities.contains_key("MISSINGUSDT"));
        let mut rounded = account_proof();
        rounded["positions_before"][0]["positionAmt"] = json!("-109.8");
        rounded["positions_after"][0]["positionAmt"] = json!("-109.8");
        // Viz's shortest decimal JSON spelling can differ by one f64 ULP
        // from the exact f32-to-f64 conversion. The account check below still
        // uses the unrounded signed quantity and the existing 1e-8 gate.
        let quantities = verified_account_observations(
            &rounded,
            "source",
            1500,
            &BTreeMap::from([("BTCUSDT".into(), -109.8000030517578)]),
        )
        .unwrap();
        assert_eq!(quantities["BTCUSDT"], -109.8);
    }
    #[test]
    fn signed_account_proof_refuses_wrong_source_time_motion_and_coverage() {
        let proof = account_proof();
        let viz = BTreeMap::from([("BTCUSDT".into(), f64::from(0.1f32))]);
        assert!(verified_account_observations(&proof, "other", 1500, &viz).is_err());
        assert!(verified_account_observations(&proof, "source", 999, &viz).is_err());
        let mut changed = proof.clone();
        changed["positions_after"][0]["positionAmt"] = json!("0.2");
        assert!(verified_account_observations(&changed, "source", 1500, &viz).is_err());
        changed = proof.clone();
        changed["positions_before"][0]["positionSide"] = json!("LONG");
        assert!(verified_account_observations(&changed, "source", 1500, &viz).is_err());
        changed = proof.clone();
        changed["positions_before"]
            .as_array_mut()
            .unwrap()
            .push(proof["positions_before"][0].clone());
        assert!(verified_account_observations(&changed, "source", 1500, &viz).is_err());
        assert!(
            verified_account_observations(
                &proof,
                "source",
                1500,
                &BTreeMap::from([("MISSINGUSDT".into(), 0.0)])
            )
            .is_err()
        );
        assert!(
            verified_account_observations(
                &proof,
                "source",
                1500,
                &BTreeMap::from([("BTCUSDT".into(), 0.2)])
            )
            .is_err()
        );
    }
    fn row(algorithm: &str, qty: f64) -> Value {
        json!({"strategy_name":"SYSTEM_POSITION_CLOSE","symbol":"SOLUSDT",
            "algorithm":algorithm,"current_qty":qty,"account_position_qty":3.0,
            "active_batches":0,"live_order_qty":0.0})
    }
    #[test]
    fn separate_execution_families_sum_under_the_same_residual_owner() {
        let (owners, accounts) =
            observed_allocations(&json!({"rows":[row("TWAP",1.0),row("CHASE",2.0)]}), true)
                .unwrap();
        assert_eq!(owners.len(), 1);
        assert_eq!(
            owners[&("__unallocated__".into(), "SOLUSDT".into(), 1)],
            3.0
        );
        assert_eq!(accounts["SOLUSDT"], 3.0);
        assert!(
            observed_allocations(&json!({"rows":[row("TWAP",1.0),row("TWAP",2.0)]}), true).is_err()
        );
    }
    #[test]
    fn inconsistent_account_or_active_orders_are_rejected() {
        let mut second = row("CHASE", 2.0);
        second["account_position_qty"] = json!(4.0);
        assert!(observed_allocations(&json!({"rows":[row("TWAP",1.0),second]}), true).is_err());
        let mut active = row("TWAP", 1.0);
        active["active_batches"] = json!(1);
        assert!(observed_allocations(&json!({"rows":[active]}), true).is_err());
    }
}
