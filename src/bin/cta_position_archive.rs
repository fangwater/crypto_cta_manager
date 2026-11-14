//! Operator-only streaming archive client and offline backup verifier.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crypto_cta_manager::position_archive::retention::{self, Command, Receipt};
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Read-only inspection; no owner socket or maintenance enablement needed.
    Inspect {
        #[arg(long)]
        db: PathBuf,
    },
    Status {
        #[arg(long)]
        socket: PathBuf,
    },
    Export {
        #[arg(long)]
        socket: PathBuf,
    },
    Commit {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        receipt: PathBuf,
    },
    Compact {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Local only: restore raw signals into an independent backup RocksDB.
    Verify {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        restore: PathBuf,
    },
}
fn main() -> Result<()> {
    let args = Args::parse();
    let (socket, command) = match args.command {
        Action::Inspect { db } => {
            inspect(&db)?;
            return Ok(());
        }
        Action::Status { socket } => (socket, Command::Status),
        Action::Export { socket } => (socket, Command::Export),
        Action::Compact { socket } => (socket, Command::Compact),
        Action::Commit { socket, receipt } => {
            // stdin avoids remote temporary receipt files.
            let value: Receipt = if receipt.as_os_str() == "-" {
                serde_json::from_reader(std::io::stdin())?
            } else {
                serde_json::from_reader(std::fs::File::open(receipt)?)?
            };
            (socket, Command::Commit { receipt: value })
        }
        Action::Verify { file, restore } => {
            let receipt = retention::verify_restore(
                std::io::BufReader::new(std::fs::File::open(file)?),
                &restore,
            )
            .context("archive restore validation")?;
            println!("{}", serde_json::to_string(&receipt)?);
            return Ok(());
        }
    };
    if !retention::client(&socket, &command, &mut std::io::stdout().lock())? {
        std::process::exit(3);
    }
    Ok(())
}

fn inspect(path: &std::path::Path) -> Result<()> {
    use crypto_cta_manager::manager_db::{POSITION_UPDATES_CF, decode_seq_key, encode_seq_key};
    use rocksdb::{DB, IteratorMode, Options, Range};
    let mut options = Options::default();
    options.set_max_open_files(128);
    let families = DB::list_cf(&options, path)?;
    anyhow::ensure!(
        families.iter().any(|f| f == POSITION_UPDATES_CF)
            && !families.iter().any(|f| f == "uniform_orders"),
        "expected Manager store, refuse Exec database"
    );
    let db = DB::open_cf_for_read_only(&options, path, families, false)?;
    let cf = db.cf_handle(POSITION_UPDATES_CF).unwrap();
    let boundary = |mode| -> Result<_> {
        db.iterator_cf(cf, mode)
            .next()
            .transpose()?
            .map(|(key, _)| decode_seq_key(&key).context("invalid archive key"))
            .transpose()
    };
    let before = retention::cutoff(crypto_cta_manager::kline::now_us());
    let start = encode_seq_key(1, 0)?;
    let end = encode_seq_key(before.0, before.1)?;
    println!(
        "{}",
        serde_json::json!({
            "identity": std::fs::read_to_string(path.join("IDENTITY"))?.trim(),
            "first": boundary(IteratorMode::Start)?, "last": boundary(IteratorMode::End)?,
            "eligible_before": before,
            "estimated_messages": db.property_int_value_cf(cf, "rocksdb.estimate-num-keys")?,
            "archive_sst_bytes": db.property_int_value_cf(cf, "rocksdb.live-sst-files-size")?,
            "eligible_sst_bytes_approx": db.get_approximate_sizes_cf(cf, &[Range::new(&start, &end)])[0]
        })
    );
    Ok(())
}
