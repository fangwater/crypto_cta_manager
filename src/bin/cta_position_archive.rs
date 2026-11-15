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
        /// Exclusive date, YYYY-MM-DD at 00:00 UTC+08:00.
        #[arg(long)]
        before: String,
    },
    Status {
        #[arg(long)]
        socket: PathBuf,
    },
    Export {
        #[arg(long)]
        socket: PathBuf,
        /// Exclusive date, YYYY-MM-DD at 00:00 UTC+08:00.
        #[arg(long)]
        before: String,
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
        /// Only compact a verified, reclaimed prefix before this date (UTC+08:00).
        #[arg(long)]
        before: String,
    },
    /// Isolated experiment only; does not change the live JSON storage format.
    RehearsalCodec {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        root: PathBuf,
    },
    /// Diagnostic only: sample immutable original records through a read-only secondary.
    RehearsalSample {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        secondary: PathBuf,
    },
    /// Restore a sample into a NEW isolated test root, never a live store.
    RehearsalPrepare {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        root: PathBuf,
    },
    /// Serve owner commands ONLY on a marked isolated rehearsal copy.
    RehearsalServe {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        socket: PathBuf,
    },
    RehearsalProof {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        start_us: i64,
        #[arg(long)]
        end_us: i64,
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
        Action::Inspect { db, before } => {
            let plan = retention::inspect(
                &db,
                retention::date_boundary(&before)?,
                crypto_cta_manager::kline::now_us(),
            )?;
            println!("{}", serde_json::to_string(&plan)?);
            return Ok(());
        }
        Action::Status { socket } => (socket, Command::Status),
        Action::Export { socket, before } => (
            socket,
            Command::Export {
                before_us: retention::date_boundary(&before)?.0,
            },
        ),
        Action::Compact { socket, before } => (
            socket,
            Command::Compact {
                before_us: retention::date_boundary(&before)?.0,
            },
        ),
        Action::Commit { socket, receipt } => {
            // stdin avoids remote temporary receipt files.
            let value: Receipt = if receipt.as_os_str() == "-" {
                serde_json::from_reader(std::io::stdin())?
            } else {
                serde_json::from_reader(std::fs::File::open(receipt)?)?
            };
            (socket, Command::Commit { receipt: value })
        }
        Action::RehearsalCodec { file, root } => {
            println!("{}", retention::codec_bench::benchmark(&file, &root)?);
            return Ok(());
        }
        Action::RehearsalSample { db, secondary } => {
            retention::rehearsal::sample(&db, &secondary, &mut std::io::stdout().lock())?;
            return Ok(());
        }
        Action::RehearsalPrepare { file, root } => {
            println!("{}", retention::rehearsal::prepare(&file, &root)?);
            return Ok(());
        }
        Action::RehearsalServe { db, socket } => {
            retention::rehearsal::serve(&db, &socket)?;
            return Ok(());
        }
        Action::RehearsalProof {
            db,
            start_us,
            end_us,
        } => {
            println!("{}", retention::rehearsal::proof(&db, start_us, end_us)?);
            return Ok(());
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
