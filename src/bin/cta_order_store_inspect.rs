//! Read-only Exec metadata inventory; no payloads, credentials or trading writes.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use rocksdb::{DB, IteratorMode, Options};
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    db: PathBuf,
}
fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(
        a.db.is_absolute() && a.db.is_dir(),
        "use an existing absolute Exec store path"
    );
    let mut opts = Options::default();
    opts.create_if_missing(false);
    opts.create_missing_column_families(false);
    opts.set_max_open_files(128);
    let names = DB::list_cf(&opts, &a.db)?;
    ensure!(
        names.iter().any(|n| n == "uniform_orders"),
        "not an Exec order store"
    );
    let db = DB::open_cf_for_read_only(&opts, &a.db, &names, false)?;
    let mut rows = Vec::new();
    for name in names {
        let cf = db.cf_handle(&name).context("column family disappeared")?;
        let first = db
            .iterator_cf(cf, IteratorMode::Start)
            .next()
            .transpose()?
            .map(|(k, _)| hex::encode(k));
        let last = db
            .iterator_cf(cf, IteratorMode::End)
            .next()
            .transpose()?
            .map(|(k, _)| hex::encode(k));
        rows.push(serde_json::json!({"column_family":name,"estimated_keys":db.property_int_value_cf(cf,"rocksdb.estimate-num-keys")?,"live_sst_bytes":db.property_int_value_cf(cf,"rocksdb.live-sst-files-size")?,"first_key_hex":first,"last_key_hex":last}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"read_only":true,"identity":std::fs::read_to_string(a.db.join("IDENTITY"))?.trim(),"column_families":rows})
        )?
    );
    Ok(())
}
