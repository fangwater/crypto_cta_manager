//! Read-only reconciliation against previously collected factual funding income.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use crypto_cta_manager::{
    config::AppConfig,
    funding_rates::{DAY_US, FundingRateCache},
    nav, postgres,
};
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "config/cta-manager.toml")]
    config: PathBuf,
    #[arg(long = "source", required = true)]
    sources: Vec<String>,
    #[arg(long, default_value_t = 30)]
    days: u32,
    #[arg(long)]
    local_ip: Option<std::net::IpAddr>,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!((1..=30).contains(&args.days), "days must be 1–30");
    let mut config = AppConfig::load(&args.config)?;
    config
        .sources
        .retain(|source| source.enabled && args.sources.contains(&source.id));
    ensure!(
        config.sources.len() == args.sources.len(),
        "source missing or disabled"
    );
    let pool = postgres::connect(&config.database_url()?, 2).await?;
    config = config.with_fee_rates(&postgres::load_fee_rates(&pool).await?);
    let mut snapshots = nav::SourcePositionSnapshots::new();
    let mut allocations = nav::SourceStrategyPositionSnapshots::new();
    for source in &config.sources {
        if let Some(snapshot) = postgres::load_latest_position_snapshot(&pool, &source.id).await? {
            snapshots.insert(source.id.clone(), snapshot);
        }
        if let Some(snapshot) =
            postgres::load_latest_strategy_position_snapshot(&pool, &source.id).await?
        {
            allocations.insert(source.id.clone(), snapshot);
        }
    }
    let mut histories = nav::load_nav_source_histories(&config, &args.sources)?;
    nav::allocation::attach(&mut histories, &nav::allocation::load(&pool).await?);
    let mut rate_config = config.funding_rates.clone();
    if args.local_ip.is_some() {
        rate_config.local_ip = args.local_ip;
    }
    let rates = FundingRateCache::new(rate_config)?;
    let cache = nav::NavTimelineCache::build(
        config.clone(),
        Arc::new(snapshots),
        Arc::new(allocations),
        Arc::new(histories),
    )?;
    let now = crypto_cta_manager::kline::now_us();
    let mut reports = Vec::<Value>::new();
    for source in &config.sources {
        let cursor: Option<i64> = sqlx::query_scalar(
            "SELECT min(synced_through_ts_us) FROM cta_funding_symbols WHERE source_id=$1",
        )
        .bind(&source.id)
        .fetch_one(&pool)
        .await?;
        let end = cursor.unwrap_or(now).min(now);
        let observations = sqlx::query("SELECT symbol,start_ts_us,synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1")
            .bind(&source.id).fetch_all(&pool).await?.into_iter().map(|row| crypto_cta_manager::funding::FundingSymbolStatus {
                source_id: source.id.clone(), symbol: row.get("symbol"), start_ts_us: row.get("start_ts_us"), synced_through_ts_us: row.get("synced_through_ts_us"),
            }).collect::<Vec<_>>();
        for allocation in [false, true] {
            let full = cache.rebuild(
                nav::NavTimelineRequest {
                    start_ts_us: None,
                    end_ts_us: end,
                    selected_source_ids: vec![source.id.clone()],
                    selected_symbols: Vec::new(),
                    max_points: 200,
                },
                allocation,
            )?;
            let start =
                (now - i64::from(args.days) * DAY_US + 1_000_000).max(full.earliest_start_ts_us);
            let report = cache.rebuild(
                nav::NavTimelineRequest {
                    start_ts_us: Some(start),
                    end_ts_us: end,
                    selected_source_ids: vec![source.id.clone()],
                    selected_symbols: Vec::new(),
                    max_points: 3000,
                },
                allocation,
            )?;
            let symbols = cache.funding_symbols(&report, allocation);
            let coverage = rates
                .wait_for(&symbols, report.start_ts_us, report.end_ts_us)
                .await?;
            let records = cache.estimate_funding_records(&report, allocation, &coverage.rates)?;
            let rows = sqlx::query("SELECT symbol,asset,ts_us,income::text AS income FROM cta_funding_income WHERE source_id=$1 AND ts_us >= $2 AND ts_us <= $3 ORDER BY ts_us,symbol")
                .bind(&source.id).bind(report.start_ts_us).bind(report.end_ts_us).fetch_all(&pool).await?;
            let actual_records = rows
                .iter()
                .map(|row| {
                    Ok(crypto_cta_manager::funding::FundingRecord {
                        source_id: source.id.clone(),
                        venue: "binance-futures".into(),
                        symbol: row.try_get("symbol")?,
                        asset: row.try_get("asset")?,
                        ts_us: row.try_get("ts_us")?,
                        income_quote: row.try_get::<String, _>("income")?.parse()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let checked = crypto_cta_manager::funding_rates::reconcile_estimates(
                &records,
                &actual_records,
                &observations,
            );
            // Reconcile by public settlement timestamp. Ledger posting timestamps
            // can differ; retain offsets and every unmatched record as evidence.
            let mut actual = BTreeMap::<(String, i64), f64>::new();
            let mut offsets = Vec::new();
            let mut unmatched = Vec::new();
            for row in &rows {
                let symbol: String = row.try_get("symbol")?;
                let asset: String = row.try_get("asset")?;
                let ts: i64 = row.try_get("ts_us")?;
                let income: f64 = row.try_get::<String, _>("income")?.parse()?;
                let nearest = records
                    .iter()
                    .filter(|r| r.symbol == symbol && r.asset == asset)
                    .min_by_key(|r| (r.ts_us - ts).abs())
                    .filter(|r| (r.ts_us - ts).abs() <= 900_000_000);
                if let Some(rate) = nearest {
                    *actual.entry((symbol, rate.ts_us)).or_default() += income;
                    offsets.push(ts - rate.ts_us);
                } else {
                    unmatched
                        .push(json!({"symbol":symbol,"asset":asset,"ts_us":ts,"income":income}));
                }
            }
            let mut estimated = BTreeMap::<(String, i64), f64>::new();
            for r in &records {
                *estimated.entry((r.symbol.clone(), r.ts_us)).or_default() += r.income_quote;
            }
            let mut comparisons = Vec::new();
            let mut mismatch_count = 0;
            let mut absolute_error = 0.0;
            let mut max_error: f64 = 0.0;
            for ((symbol, ts), estimate) in &estimated {
                let observed = actual.get(&(symbol.clone(), *ts)).copied().unwrap_or(0.0);
                let error = estimate - observed;
                absolute_error += error.abs();
                max_error = max_error.max(error.abs());
                let matched = error.abs() <= 1e-7_f64.max(observed.abs() * 1e-6);
                if !matched {
                    mismatch_count += 1;
                }
                if observed != 0.0 || estimate.abs() > 1e-10 {
                    let record = records
                        .iter()
                        .find(|r| &r.symbol == symbol && r.ts_us == *ts)
                        .context("missing estimate")?;
                    let within_observation = actual.contains_key(&(symbol.clone(), *ts))
                        || observations.iter().any(|state| {
                            state.symbol == *symbol
                                && state.start_ts_us <= *ts
                                && state
                                    .synced_through_ts_us
                                    .is_some_and(|end| end >= ts.saturating_add(900_000_000))
                        });
                    let boundary_fills =
                        cache.funding_boundary_fills(&source.id, allocation, symbol, *ts);
                    comparisons.push(json!({"observed":within_observation,"symbol":symbol,"ts_us":ts,"actual":observed,"estimated":estimate,"error":error,"matched":matched,"boundary_fills":boundary_fills,
                        "quantity":record.quantity,"rate":record.rate,"mark_price":record.mark_price,"boundary_fill_count":record.boundary_fill_count}));
                }
            }
            let estimate_sum: f64 = records.iter().map(|r| r.income_quote).sum();
            let actual_sum: f64 = rows
                .iter()
                .map(|r| r.get::<String, _>("income").parse::<f64>().unwrap())
                .sum();
            let strategy_sum: f64 = records.iter().flat_map(|r| r.strategies.values()).sum();
            let output = json!({"source_id":source.id,"allocation":allocation,"start_ts_us":report.start_ts_us,"end_ts_us":report.end_ts_us,
                "factual_synced_through_ts_us":cursor,"symbols":symbols,"actual_record_count":rows.len(),"estimated_income":estimate_sum,"actual_income":actual_sum,
                "unscoped_difference_quote":estimate_sum-actual_sum,"unscoped_absolute_difference_quote":absolute_error,"unscoped_max_difference_quote":max_error,"unscoped_mismatch_count":mismatch_count,
                "strategy_additivity_error":strategy_sum-estimate_sum,"ledger_time_offsets_us":offsets,"unmatched_actual":unmatched,
                "comparisons":comparisons,"estimates":records,"reconciliation":checked,"observations":observations});
            eprintln!(
                "{} allocation={} actual_rows={} actual={:.10} estimate={:.10} abs_error={:.10} mismatches={}",
                source.id,
                allocation,
                rows.len(),
                checked.actual_income_quote,
                checked.estimated_income_quote,
                checked.absolute_error_quote,
                checked.mismatch_count
            );
            reports.push(output);
        }
    }
    let output = serde_json::to_vec_pretty(&json!({"generated_at_us":now,"reports":reports}))?;
    if let Some(path) = args.output {
        std::fs::write(path, output)?;
    } else {
        println!("{}", String::from_utf8(output)?);
    }
    Ok(())
}
