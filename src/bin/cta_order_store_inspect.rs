//! Read-only Exec metadata inventory; no payloads, credentials or trading writes.
use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use rocksdb::{DB, IteratorMode, Options};
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    db: PathBuf,
    #[arg(long, value_enum, default_value_t=Store::Exec)]
    store: Store,
    /// A NEW directory outside the source store, for diagnostic RocksDB logs only.
    #[arg(long)]
    log_dir: PathBuf,
    /// Probe first/last keys for all CFs; default probes only queue history.
    #[arg(long)]
    probe_all: bool,
}
#[derive(Clone, Copy, ValueEnum)]
enum Store {
    Exec,
    Manager,
}
fn main() -> Result<()> {
    let a = Args::parse();
    crypto_cta_manager::position_archive::cold::manager::lower_priority();
    ensure!(
        a.db.is_absolute() && a.db.is_dir(),
        "use an existing absolute Exec store path"
    );
    let source = a.db.canonicalize()?;
    ensure!(source.join("IDENTITY").is_file(), "source identity missing");
    ensure!(
        a.log_dir.is_absolute() && !a.log_dir.exists(),
        "use a NEW absolute diagnostic log directory"
    );
    let parent = a
        .log_dir
        .parent()
        .context("missing diagnostic parent")?
        .canonicalize()?;
    let log_dir = parent.join(a.log_dir.file_name().context("missing diagnostic name")?);
    ensure!(
        !log_dir.starts_with(&source) && !source.starts_with(&log_dir),
        "diagnostic logs overlap source"
    );
    std::fs::create_dir(&log_dir)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700))?;
    let mut opts = Options::default();
    opts.create_if_missing(false);
    opts.create_missing_column_families(false);
    opts.set_max_open_files(128);
    opts.set_db_log_dir(&log_dir);
    let names = DB::list_cf(&opts, &a.db)?;
    match a.store {
        Store::Exec => ensure!(
            names.iter().any(|n| n == "uniform_orders"),
            "not an Exec order store"
        ),
        Store::Manager => ensure!(
            names.iter().any(|n| n == "position_updates")
                && !names.iter().any(|n| matches!(
                    n.as_str(),
                    "uniform_orders" | "order_updates" | "trade_updates"
                )),
            "not a Manager store"
        ),
    }
    let db = DB::open_cf_for_read_only(&opts, &a.db, &names, false)?;
    let files = db.live_files()?;
    let mut rows = Vec::new();
    for name in names {
        let cf = db.cf_handle(&name).context("column family disappeared")?;
        let probe = a.probe_all || name == "order_queue_positions";
        let mut first = None;
        let mut last = None;
        let mut probe_error = None;
        if probe {
            let result = (|| -> Result<_> {
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
                Ok((first, last))
            })();
            match result {
                Ok((a, b)) => {
                    first = a;
                    last = b;
                }
                Err(error) => probe_error = Some(error.to_string()),
            }
        }
        let metadata_files = files
            .iter()
            .filter(|f| f.column_family_name == name)
            .collect::<Vec<_>>();
        let has_visible_record = (probe && probe_error.is_none()).then_some(first.is_some());
        rows.push(serde_json::json!({"column_family":name,"estimated_keys":db.property_int_value_cf(cf,"rocksdb.estimate-num-keys")?,"live_sst_bytes":db.property_int_value_cf(cf,"rocksdb.live-sst-files-size")?,"sst_bytes_by_metadata":metadata_files.iter().map(|f|f.size as u64).sum::<u64>(),"sst_files":metadata_files.len(),"probe_performed":probe,"probe_error":probe_error,"has_visible_record":has_visible_record,"first_key_hex":first,"last_key_hex":last}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"read_only":true,"identity":std::fs::read_to_string(a.db.join("IDENTITY"))?.trim(),"column_families":rows})
        )?
    );
    Ok(())
}
