//! Replay and atomically append an audited historical ownership batch.
//! Factual fills and immutable opening/later correction rows are never edited.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use crypto_cta_manager::{config::AppConfig, nav, postgres};
use nav::allocation::{self, AllocationSnapshot, OwnershipObservation};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn open_minute_secondary(primary: &Path, secondary: &Path) -> Result<rocksdb::DB> {
    ensure!(
        !secondary.exists(),
        "use a new secondary diagnostic directory"
    );
    let mut options = rocksdb::Options::default();
    // Pin this immutable read view while the live primary compacts. Open only
    // candle/default files, not the much larger position-update archive.
    options.set_max_open_files(-1);
    Ok(rocksdb::DB::open_cf_as_secondary(
        &options,
        primary,
        secondary,
        ["default", "klines_1m"],
    )?)
}

/// A changed zero-quantity strategy can change whether a minute mark touches
/// arithmetic dust in a flat account lot. Permit only sub-nanounit valuation
/// roundoff; fills, quantities, realized FIFO, fees, volume and timestamps stay
/// exact. This is separate from the signed account quantity proof below.
fn account_roundoff(before: &serde_json::Value, after: &serde_json::Value) -> Result<f64> {
    fn compare(a: &serde_json::Value, b: &serde_json::Value, key: &str) -> Result<f64> {
        if a == b {
            return Ok(0.0);
        }
        match (a, b) {
            (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
                ensure!(a.len() == b.len(), "account field count changed");
                let mut maximum = 0.0f64;
                for (key, value) in a {
                    maximum = maximum.max(compare(
                        value,
                        b.get(key).context("account field missing")?,
                        key,
                    )?);
                }
                Ok(maximum)
            }
            (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
                ensure!(a.len() == b.len(), "account item count changed");
                let mut maximum = 0.0f64;
                for (a, b) in a.iter().zip(b) {
                    maximum = maximum.max(compare(a, b, key)?);
                }
                Ok(maximum)
            }
            (serde_json::Value::Number(a), serde_json::Value::Number(b))
                if matches!(
                    key,
                    "floating_pnl_quote"
                        | "nav_change_before_fee_quote"
                        | "nav_change_after_fee_quote"
                ) =>
            {
                let error = (a.as_f64().context("invalid account value")?
                    - b.as_f64().context("invalid account value")?)
                .abs();
                ensure!(
                    error.is_finite() && error < 1e-9,
                    "account valuation changed at {key}"
                );
                Ok(error)
            }
            _ => anyhow::bail!("account value changed at {key}"),
        }
    }
    compare(before, after, "")
}

fn read_minute(db: &rocksdb::DB, symbol: &str, venue: i16, open: i64) -> Result<Option<f64>> {
    ensure!(venue == 1, "unexpected observation venue");
    let cf = db.cf_handle("klines_1m").context("minute cache missing")?;
    let mut key = vec![u8::try_from(symbol.len())?];
    key.extend_from_slice(symbol.as_bytes());
    key.extend_from_slice(&open.to_be_bytes());
    let Some(value) = db.get_cf(cf, key)? else {
        return Ok(None);
    };
    ensure!(value.len() == 56, "invalid minute candle");
    Ok(Some(f64::from_le_bytes(value[24..32].try_into()?)))
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    source: String,
    #[arg(long, conflicts_with = "candidate")]
    observations: Option<PathBuf>,
    #[arg(long)]
    manager_db: Option<PathBuf>,
    /// New diagnostic directory for RocksDB secondary metadata, outside stores.
    #[arg(long)]
    manager_secondary: PathBuf,
    #[arg(long)]
    candidate: Option<PathBuf>,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    verification_json: Option<PathBuf>,
    /// Complete signed account observations bracketing the audited cutoff.
    #[arg(long)]
    account_proof: PathBuf,
    #[arg(long)]
    end_ts_us: i64,
    #[arg(long)]
    apply: bool,
    #[arg(long)]
    note: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.apply || args.candidate.is_some(),
        "--apply requires --candidate"
    );
    let mut config = AppConfig::load(&args.config)?;
    let secondary = args
        .manager_secondary
        .parent()
        .context("secondary parent missing")?
        .canonicalize()?
        .join(
            args.manager_secondary
                .file_name()
                .context("secondary name missing")?,
        );
    for source in &config.sources {
        let primary = source
            .rocksdb_path
            .canonicalize()
            .unwrap_or_else(|_| source.rocksdb_path.clone());
        ensure!(
            !secondary.starts_with(primary),
            "secondary cannot be inside an Exec store"
        );
    }
    config.sources.retain(|s| s.id == args.source && s.enabled);
    ensure!(
        config.sources.len() == 1 && config.sources[0].venue == "binance-futures",
        "select one enabled Binance linear source"
    );
    let pool = postgres::connect(&config.database_url()?, 2).await?;
    config = config.with_fee_rates(&postgres::load_fee_rates(&pool).await?);
    let retention_floor =
        crypto_cta_manager::kline::now_us() - i64::from(config.kline.retain_days) * 86_400_000_000;
    let db_path = args
        .manager_db
        .as_ref()
        .context("--manager-db required for strict minute verification")?;
    let db_path = db_path.canonicalize()?;
    ensure!(
        !secondary.starts_with(&db_path) && !db_path.starts_with(&secondary),
        "secondary must be separate from the Manager store"
    );
    let minute_db = open_minute_secondary(&db_path, &secondary)?;
    let original = allocation::load(&pool).await?;
    let existing = original.get(&args.source).cloned().unwrap_or_default();
    let captured_existing = serde_json::to_value(existing.as_ref())?;
    let mut snapshots = nav::SourcePositionSnapshots::new();
    if let Some(snapshot) = postgres::load_latest_position_snapshot(&pool, &args.source).await? {
        snapshots.insert(args.source.clone(), snapshot);
    }
    let mut anchors = nav::SourceStrategyPositionSnapshots::new();
    if let Some(anchor) =
        postgres::load_latest_strategy_position_snapshot(&pool, &args.source).await?
    {
        anchors.insert(args.source.clone(), anchor);
    }
    let mut histories = nav::load_nav_source_histories(&config, &[args.source.clone()])?;
    allocation::attach(&mut histories, &original);
    let cache = nav::NavTimelineCache::build(
        config.clone(),
        Arc::new(snapshots.clone()),
        Arc::new(anchors.clone()),
        Arc::new(histories.clone()),
    )?;
    let candidates = if let Some(path) = &args.candidate {
        serde_json::from_slice::<Vec<AllocationSnapshot>>(&std::fs::read(path)?)?
    } else {
        let observations: Vec<OwnershipObservation> = serde_json::from_slice(&std::fs::read(
            args.observations
                .context("--observations or --candidate required")?,
        )?)?;
        cache.prepare_historical_allocations(&args.source, &observations, |symbol, venue, ts| {
            let open = ts.div_euclid(60_000_000) * 60_000_000 - 60_000_000;
            read_minute(&minute_db, symbol, venue, open)?
                .with_context(|| format!("missing completed minute for {symbol} at {open}"))
        })?
    };
    ensure!(
        !candidates.is_empty() && candidates.iter().all(|a| a.source_id == args.source),
        "empty or cross-source historical batch"
    );
    ensure!(
        candidates
            .windows(2)
            .all(|w| w[0].snapshot_ts_us < w[1].snapshot_ts_us),
        "historical batch timestamps must increase"
    );
    ensure!(
        candidates.last().unwrap().snapshot_ts_us < args.end_ts_us,
        "verification end precedes batch"
    );
    for snapshot in &candidates {
        snapshot.validate()?;
    }
    let mut revised = original.clone();
    let list = Arc::make_mut(revised.entry(args.source.clone()).or_default());
    list.extend(candidates.iter().cloned());
    list.sort_by_key(|s| s.snapshot_ts_us);
    allocation::resolve(list)?;
    allocation::attach(&mut histories, &revised);
    let adjusted = nav::NavTimelineCache::build(
        config,
        Arc::new(snapshots),
        Arc::new(anchors),
        Arc::new(histories),
    )?;
    let earliest = cache
        .start_ts_us(&args.source, args.end_ts_us, true)
        .context("source history is empty")?;
    let first = candidates[0].snapshot_ts_us;
    let current_boundary = existing
        .last()
        .map(|a| a.snapshot_ts_us + 1)
        .unwrap_or(args.end_ts_us);
    let mut windows = Vec::new();
    let day = 86_400_000_000;
    let shanghai_day =
        (args.end_ts_us + 8 * 3_600_000_000).div_euclid(day) * day - 8 * 3_600_000_000;
    for (name, start, end) in [
        ("before_history", earliest, first - 1),
        ("history", earliest, args.end_ts_us),
        ("7d", earliest.max(args.end_ts_us - 7 * day), args.end_ts_us),
        ("1d", earliest.max(args.end_ts_us - day), args.end_ts_us),
        ("today", earliest.max(shanghai_day), args.end_ts_us),
        ("after_current", current_boundary, args.end_ts_us),
    ] {
        if start > end {
            continue;
        }
        for strategy in [false, true] {
            let request = nav::NavTimelineRequest {
                start_ts_us: Some(start),
                end_ts_us: end,
                selected_source_ids: vec![args.source.clone()],
                selected_symbols: vec![],
                max_points: 1000,
                tick_interval_us: None,
            };
            let strict = start.div_euclid(60_000_000) * 60_000_000 - 60_000_000 >= retention_floor;
            let before = if strict {
                cache.rebuild_from_minute_reader(request.clone(), strategy, |s, v, t| {
                    read_minute(&minute_db, s, v, t)
                })?
            } else {
                cache.rebuild(request.clone(), strategy)?
            };
            let after = if strict {
                adjusted.rebuild_from_minute_reader(request, strategy, |s, v, t| {
                    read_minute(&minute_db, s, v, t)
                })?
            } else {
                adjusted.rebuild(request, strategy)?
            };
            let account_evidence = |r: &nav::NavTimelineReport| json!({"summary":r.summary,"symbols":r.symbols,"points":r.points});
            let valuation_roundoff =
                account_roundoff(&account_evidence(&before), &account_evidence(&after))
                    .with_context(|| {
                        format!("historical batch changes account report in {name}")
                    })?;
            if name == "before_history" {
                ensure!(
                    before == after,
                    "historical batch changes earlier ownership"
                );
            }
            let sum: f64 = after
                .strategy_points
                .iter()
                .map(|s| s.summary.nav_change_after_fee_quote)
                .sum();
            ensure!(
                (sum - after.summary.nav_change_after_fee_quote).abs() < 1e-6,
                "strategy/account additivity failed in {name}"
            );
            let summarize = |r: &nav::NavTimelineReport| {
                r.strategy_points.iter().map(|s|json!({"strategy":s.strategy,"summary":s.summary,"gross_position_value_quote":s.gross_position_value_quote,"net_position_value_quote":s.net_position_value_quote})).collect::<Vec<_>>()
            };
            windows.push(json!({"window":name,"strategy_anchor":strategy,"valuation":after.valuation,"account_unchanged":true,"maximum_account_valuation_roundoff":valuation_roundoff,"account_summary":after.summary,"before_strategies":summarize(&before),"after_strategies":summarize(&after),"start_ts_us":start,"end_ts_us":end,"account_point_count":after.points.len(),"symbol_count":after.symbols.len()}));
        }
    }
    let before = cache.allocation_audit(&args.source, args.end_ts_us)?;
    let after = adjusted.allocation_audit(&args.source, args.end_ts_us)?;
    ensure!(
        before.account == after.account,
        "historical batch changed final account quantities"
    );
    let proof: serde_json::Value = serde_json::from_slice(&std::fs::read(&args.account_proof)?)?;
    ensure!(
        proof["source_id"] == args.source
            && proof["open_orders"].as_array().is_some_and(Vec::is_empty),
        "wrong source or unsettled signed account proof"
    );
    let proof_ms = proof["snapshot_ts_ms"]
        .as_i64()
        .context("account proof time missing")?;
    let begin_ms = proof["start_ms"]
        .as_i64()
        .context("account proof start missing")?;
    let finish_ms = proof["end_ms"]
        .as_i64()
        .context("account proof end missing")?;
    ensure!(
        begin_ms > 0
            && begin_ms <= proof_ms
            && proof_ms <= finish_ms
            && finish_ms - begin_ms <= 15000,
        "account proof must bracket its snapshot time"
    );
    let quantities = |field: &str| -> Result<std::collections::BTreeMap<String, (String, f64)>> {
        let mut result = std::collections::BTreeMap::new();
        for row in proof[field]
            .as_array()
            .context("complete account positions missing")?
        {
            ensure!(
                row["positionSide"] == "BOTH",
                "historical proof requires one-way positions"
            );
            let symbol = row["symbol"].as_str().context("account symbol missing")?;
            let decimal = row["positionAmt"]
                .as_str()
                .context("account quantity must retain its decimal string")?;
            let quantity: f64 = decimal.parse()?;
            ensure!(quantity.is_finite(), "nonfinite account quantity");
            ensure!(
                result
                    .insert(symbol.into(), (decimal.into(), quantity))
                    .is_none(),
                "duplicate account symbol"
            );
        }
        ensure!(!result.is_empty(), "account observations are empty");
        Ok(result)
    };
    let observed = quantities("positions_before")?;
    ensure!(
        observed == quantities("positions_after")?,
        "account quantities changed across proof"
    );
    let proof_before = cache.allocation_audit(&args.source, proof_ms * 1000)?;
    let proof_after = adjusted.allocation_audit(&args.source, proof_ms * 1000)?;
    ensure!(
        proof_before.account == proof_after.account,
        "historical repair changed observed account quantities"
    );
    let replay: std::collections::BTreeMap<_, _> = proof_after
        .account
        .iter()
        .map(|(s, v, q)| ((s.clone(), *v), *q))
        .collect();
    let mut maximum_error = 0.0f64;
    for ((symbol, venue), quantity) in &replay {
        ensure!(*venue == 1, "unexpected account venue");
        let factual = observed
            .get(symbol)
            .with_context(|| format!("account proof omits {symbol}; missing is not zero"))?
            .1;
        let error = (quantity - factual).abs();
        maximum_error = maximum_error.max(error);
        ensure!(
            error < 1e-8,
            "historical account replay differs from signed observation for {symbol}"
        );
    }
    for (symbol, (_, quantity)) in &observed {
        ensure!(
            (replay
                .get(&(symbol.clone(), 1))
                .copied()
                .unwrap_or_default()
                - quantity)
                .abs()
                < 1e-8,
            "signed account symbol differs from replay for {symbol}"
        );
    }
    if let Some(path) = &args.verification_json {
        std::fs::write(
            path,
            serde_json::to_vec(
                &json!({"before":before,"after":after,"windows":windows,"account_proof":{"snapshot_ts_ms":proof_ms,"observed_symbols":observed.len(),"replayed_markets":replay.len(),"maximum_error":maximum_error,"matched":true}}),
            )?,
        )?;
    }
    if let Some(path) = &args.output {
        std::fs::write(path, serde_json::to_vec_pretty(&candidates)?)?;
    }
    if args.apply {
        let note = args.note.context("--note required")?;
        ensure!(!note.trim().is_empty(), "audit note is empty");
        let mut transaction = pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '60s'")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(&args.source)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("LOCK TABLE cta_strategy_allocation_snapshots IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *transaction)
            .await?;
        // Detect another maintenance writer before the atomic append. No
        // originals are updated, and a duplicate timestamp aborts the batch.
        let latest = allocation::load(&pool).await?;
        ensure!(
            serde_json::to_value(
                latest
                    .get(&args.source)
                    .cloned()
                    .unwrap_or_default()
                    .as_ref()
            )? == captured_existing,
            "source allocations changed during preparation"
        );
        let inserted = sqlx::query("INSERT INTO cta_strategy_allocation_snapshots (source_id,snapshot_ts_us,positions,restatements,note) SELECT $1,r.snapshot_ts_us,r.positions,coalesce(r.restatements,'[]'::jsonb),$3 FROM jsonb_to_recordset($2::jsonb) AS r(snapshot_ts_us bigint,positions jsonb,restatements jsonb)")
            .bind(&args.source).bind(serde_json::to_value(&candidates)?).bind(&note).execute(&mut *transaction).await?;
        ensure!(
            inserted.rows_affected() == candidates.len() as u64,
            "incomplete historical append"
        );
        transaction.commit().await?;
    }
    println!(
        "{}",
        json!({"source_id":args.source,"inserted":args.apply,"records":candidates.len(),"positions":candidates.iter().map(|a| a.positions.len()).sum::<usize>(),"restatements":candidates.iter().map(|a| a.restatements.len()).sum::<usize>(),"first_ts_us":first,"last_ts_us":candidates.last().unwrap().snapshot_ts_us,"account_unchanged":true})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minute_secondary_survives_primary_compaction_without_opening_archive() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let secondary = dir.path().join("secondary");
        let mut options = rocksdb::Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db =
            rocksdb::DB::open_cf(&options, &primary, ["klines_1m", "position_updates"]).unwrap();
        let cf = db.cf_handle("klines_1m").unwrap();
        db.put_cf(cf, b"minute", b"original").unwrap();
        db.flush_cf(cf).unwrap();
        let view = open_minute_secondary(&primary, &secondary).unwrap();
        assert!(view.cf_handle("position_updates").is_none());
        db.put_cf(cf, b"minute", b"new candle").unwrap();
        db.flush_cf(cf).unwrap();
        db.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        assert_eq!(
            view.get_cf(view.cf_handle("klines_1m").unwrap(), b"minute")
                .unwrap()
                .as_deref(),
            Some(b"original".as_slice())
        );
        assert_eq!(
            db.get_cf(cf, b"minute").unwrap().as_deref(),
            Some(b"new candle".as_slice())
        );
        assert!(open_minute_secondary(&primary, &secondary).is_err());
    }

    #[test]
    fn valuation_roundoff_does_not_relax_factual_or_quantity_checks() {
        let before = json!({"net_quantity":1.0,"realized_pnl_before_fee_quote":9.497959999999983,"nav_change_before_fee_quote":9.49795999999998,"fill_count":123,"estimated_trading_fee_quote":1.18});
        let mut after = before.clone();
        after["nav_change_before_fee_quote"] = json!(9.497959999999983);
        assert!(account_roundoff(&before, &after).unwrap() < 1e-13);
        for (key, value) in [
            ("net_quantity", json!(1.000000000001)),
            ("realized_pnl_before_fee_quote", json!(9.49795999999998)),
            ("fill_count", json!(124)),
            ("estimated_trading_fee_quote", json!(1.180000000001)),
            ("nav_change_before_fee_quote", json!(9.4979601)),
        ] {
            let mut invalid = before.clone();
            invalid[key] = value;
            assert!(account_roundoff(&before, &invalid).is_err(), "{key}");
        }
    }
}
