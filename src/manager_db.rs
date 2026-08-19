use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rocksdb::{DBCompressionType, DBWithThreadMode, MultiThreaded, Options};

pub type ManagerRocksDb = DBWithThreadMode<MultiThreaded>;

pub const DEFAULT_CF: &str = "default";
pub const POSITION_UPDATES_CF: &str = "position_updates";

#[derive(Clone)]
pub struct ManagerDb {
    db: Arc<ManagerRocksDb>,
    path: PathBuf,
}

impl ManagerDb {
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create Manager RocksDB {}", path.display()))?;

        let db_opts = db_options();
        let mut existing = match ManagerRocksDb::list_cf(&db_opts, path) {
            Ok(names) if !names.is_empty() => names,
            _ => vec![DEFAULT_CF.to_string()],
        };
        if !existing.iter().any(|name| name == POSITION_UPDATES_CF) {
            existing.push(POSITION_UPDATES_CF.to_string());
        }
        let cf_opts = existing
            .iter()
            .map(|name| (name.as_str(), cf_options()))
            .collect::<Vec<_>>();
        let db = ManagerRocksDb::open_cf_with_opts(&db_opts, path, cf_opts)
            .with_context(|| format!("failed to open Manager RocksDB {}", path.display()))?;
        Ok(Self {
            db: Arc::new(db),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn db(&self) -> &ManagerRocksDb {
        self.db.as_ref()
    }

    pub fn ensure_column_family(&self, name: &str) -> Result<()> {
        if self.db.cf_handle(name).is_some() {
            return Ok(());
        }
        self.db
            .create_cf(name, &cf_options())
            .with_context(|| format!("failed to create Manager column family {name}"))?;
        Ok(())
    }

    pub fn column_families(&self) -> Result<Vec<String>> {
        ManagerRocksDb::list_cf(&db_options(), &self.path).with_context(|| {
            format!(
                "failed to list Manager column families {}",
                self.path.display()
            )
        })
    }
}

pub fn encode_ts_key(ts_us: i64) -> [u8; 8] {
    ts_us.to_be_bytes()
}

pub fn encode_seq_key(received_at_us: i64, seq: u32) -> Result<[u8; 12]> {
    if received_at_us <= 0 {
        bail!("Manager RocksDB sequence key timestamp must be positive");
    }
    let mut bytes = [0u8; 12];
    bytes[0..8].copy_from_slice(&received_at_us.to_be_bytes());
    bytes[8..12].copy_from_slice(&seq.to_be_bytes());
    Ok(bytes)
}

pub fn decode_seq_key(bytes: &[u8]) -> Option<(i64, u32)> {
    if bytes.len() != 12 {
        return None;
    }
    Some((
        i64::from_be_bytes(bytes.get(0..8)?.try_into().ok()?),
        u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?),
    ))
}

pub fn db_options() -> Options {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    opts.set_compression_type(DBCompressionType::Lz4);
    opts
}

pub fn cf_options() -> Options {
    let mut opts = Options::default();
    opts.set_compression_type(DBCompressionType::Lz4);
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sequence_keys_stay_ordered_within_the_same_microsecond() {
        let first = encode_seq_key(1_700_000_000_000_001, 0).unwrap();
        let second = encode_seq_key(1_700_000_000_000_001, 1).unwrap();
        let later = encode_seq_key(1_700_000_000_000_002, 0).unwrap();
        assert!(first.as_slice() < second.as_slice());
        assert!(second.as_slice() < later.as_slice());
        assert_eq!(decode_seq_key(&second), Some((1_700_000_000_000_001, 1)));
    }

    #[test]
    fn opens_existing_column_families_and_creates_position_updates() {
        let dir = TempDir::new().unwrap();
        let first = ManagerDb::open(dir.path()).unwrap();
        first
            .ensure_column_family("BTCUSDT:binance-futures")
            .unwrap();
        drop(first);

        let second = ManagerDb::open(dir.path()).unwrap();
        let names = second.column_families().unwrap();
        assert!(names.iter().any(|name| name == POSITION_UPDATES_CF));
        assert!(names.iter().any(|name| name == "BTCUSDT:binance-futures"));
    }
}
