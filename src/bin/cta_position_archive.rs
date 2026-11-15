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
    /// Back up an exclusively owned original DB, without converting its records.
    BackupJson {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        backup: PathBuf,
    },
    /// Read-only verification of an original checkpoint copied to powerleader.
    VerifyJsonBackup {
        #[arg(long)]
        db: PathBuf,
    },
    /// Read-only deployment compatibility check; never initializes or converts data.
    CheckStorage {
        #[arg(long)]
        db: PathBuf,
    },
    /// Explicit offline replacement; verifies a complete original DB backup first.
    MigrateJson {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        backup: PathBuf,
        /// Reclaim obsolete JSON SSTs only after the backup and full equality check.
        #[arg(long)]
        compact: bool,
    },
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
        /// Read JSON only for an explicitly selected legacy source.
        #[arg(long)]
        legacy_json: bool,
        /// Sample only a contiguous recent suffix (max 10000, still diagnostic only).
        #[arg(long, default_value_t = 0)]
        tail_records: usize,
    },
    /// Restore a sample into a NEW isolated test root, never a live store.
    RehearsalPrepare {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        legacy_json: bool,
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
    /// Restore original JSON backups offline; use migrate-json after the chain.
    VerifyJson {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        restore: PathBuf,
    },
}
fn main() -> Result<()> {
    let args = Args::parse();
    let (socket, command) = match args.command {
        Action::BackupJson { db, backup } => {
            println!(
                "{}",
                crypto_cta_manager::position_archive::migration::backup_json(&db, &backup)?
            );
            return Ok(());
        }
        Action::VerifyJsonBackup { db } => {
            println!(
                "{}",
                crypto_cta_manager::position_archive::migration::verify_json_backup(&db)?
            );
            return Ok(());
        }
        Action::CheckStorage { db } => {
            println!(
                "{}",
                crypto_cta_manager::position_archive::migration::check_storage(&db)?
            );
            return Ok(());
        }
        Action::MigrateJson {
            db,
            backup,
            compact,
        } => {
            println!(
                "{}",
                crypto_cta_manager::position_archive::migration::migrate_json(
                    &db, &backup, compact
                )?
            );
            return Ok(());
        }
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
        Action::RehearsalSample {
            db,
            secondary,
            legacy_json,
            tail_records,
        } => {
            retention::rehearsal::sample_with_options(
                &db,
                &secondary,
                &mut std::io::stdout().lock(),
                legacy_json,
                tail_records,
            )?;
            return Ok(());
        }
        Action::RehearsalPrepare {
            file,
            root,
            legacy_json,
        } => {
            println!(
                "{}",
                retention::rehearsal::prepare_with_format(&file, &root, legacy_json)?
            );
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
        Action::VerifyJson { file, restore } => {
            println!(
                "{}",
                serde_json::to_string(&retention::verify_json_restore(
                    std::fs::File::open(file)?,
                    &restore
                )?)?
            );
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
