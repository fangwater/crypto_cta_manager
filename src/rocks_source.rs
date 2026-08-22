use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rocksdb::{DB, Direction, IteratorMode, Options};

use crate::model::UNIFORM_ORDERS_CF;

#[derive(Debug)]
pub struct RawRocksRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

pub fn read_all_column_families(
    path: &Path,
    requested_column_families: &[&str],
) -> Result<BTreeMap<String, Vec<RawRocksRecord>>> {
    if !path.is_dir() {
        bail!("RocksDB path is not a directory: {}", path.display());
    }

    let mut options = Options::default();
    options.create_if_missing(false);
    options.create_missing_column_families(false);
    let column_families = DB::list_cf(&options, path)
        .with_context(|| format!("failed to list column families in {}", path.display()))?;
    for requested in requested_column_families {
        if !column_families.iter().any(|name| name == requested) {
            bail!(
                "RocksDB {} has no {} column family",
                path.display(),
                requested
            );
        }
    }

    let db = DB::open_cf_for_read_only(&options, path, column_families, false)
        .with_context(|| format!("failed to open RocksDB {} read-only", path.display()))?;
    let mut result = BTreeMap::new();
    for requested in requested_column_families {
        let column_family = db
            .cf_handle(requested)
            .with_context(|| format!("{requested} column family disappeared after open"))?;
        let mut records = Vec::new();
        for item in db.iterator_cf(column_family, IteratorMode::Start) {
            let (key, value) =
                item.with_context(|| format!("failed while iterating {requested}"))?;
            records.push(RawRocksRecord {
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        result.insert((*requested).to_string(), records);
    }
    Ok(result)
}

pub fn read_available_column_families(
    path: &Path,
    requested_column_families: &[&str],
) -> Result<BTreeMap<String, Vec<RawRocksRecord>>> {
    if !path.is_dir() {
        bail!("RocksDB path is not a directory: {}", path.display());
    }

    let mut options = Options::default();
    options.create_if_missing(false);
    options.create_missing_column_families(false);
    let column_families = DB::list_cf(&options, path)
        .with_context(|| format!("failed to list column families in {}", path.display()))?;
    let db = DB::open_cf_for_read_only(&options, path, column_families.clone(), false)
        .with_context(|| format!("failed to open RocksDB {} read-only", path.display()))?;
    let mut result = BTreeMap::new();
    for requested in requested_column_families {
        if !column_families.iter().any(|name| name == requested) {
            continue;
        }
        let column_family = db
            .cf_handle(requested)
            .with_context(|| format!("{requested} column family disappeared after open"))?;
        let mut records = Vec::new();
        for item in db.iterator_cf(column_family, IteratorMode::Start) {
            let (key, value) =
                item.with_context(|| format!("failed while iterating {requested}"))?;
            records.push(RawRocksRecord {
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        result.insert((*requested).to_string(), records);
    }
    Ok(result)
}

pub fn read_uniform_orders(
    path: &Path,
    start_ts_us: i64,
    end_ts_us: i64,
) -> Result<Vec<RawRocksRecord>> {
    if start_ts_us < 0 || end_ts_us < 0 {
        bail!("RocksDB scan timestamps must not be negative");
    }
    if start_ts_us >= end_ts_us {
        return Ok(Vec::new());
    }
    if !path.is_dir() {
        bail!("RocksDB path is not a directory: {}", path.display());
    }

    let mut options = Options::default();
    options.create_if_missing(false);
    options.create_missing_column_families(false);
    let column_families = DB::list_cf(&options, path)
        .with_context(|| format!("failed to list column families in {}", path.display()))?;
    if !column_families.iter().any(|name| name == UNIFORM_ORDERS_CF) {
        bail!(
            "RocksDB {} has no {} column family",
            path.display(),
            UNIFORM_ORDERS_CF
        );
    }
    let db = DB::open_cf_for_read_only(&options, path, column_families, false)
        .with_context(|| format!("failed to open RocksDB {} read-only", path.display()))?;
    let column_family = db
        .cf_handle(UNIFORM_ORDERS_CF)
        .context("uniform_orders column family disappeared after open")?;

    let start_key = format_time_key(start_ts_us);
    let end_key = format_time_key(end_ts_us);
    let iterator = db.iterator_cf(
        column_family,
        IteratorMode::From(start_key.as_bytes(), Direction::Forward),
    );
    let mut records = Vec::new();
    for item in iterator {
        let (key, value) = item.context("failed while iterating uniform_orders")?;
        if key.as_ref() >= end_key.as_bytes() {
            break;
        }
        records.push(RawRocksRecord {
            key: key.to_vec(),
            value: value.to_vec(),
        });
    }
    Ok(records)
}

fn format_time_key(ts_us: i64) -> String {
    format!("{ts_us:020}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocksdb::{ColumnFamilyDescriptor, DBCompressionType};

    #[test]
    fn reads_half_open_time_range_while_primary_is_open() {
        let temp = tempfile::tempdir().unwrap();
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        options.set_compression_type(DBCompressionType::Lz4);
        let db = DB::open_cf_descriptors(
            &options,
            temp.path(),
            vec![ColumnFamilyDescriptor::new(
                UNIFORM_ORDERS_CF,
                Options::default(),
            )],
        )
        .unwrap();
        let cf = db.cf_handle(UNIFORM_ORDERS_CF).unwrap();
        db.put_cf(cf, b"00000000000000000100", b"a").unwrap();
        db.put_cf(cf, b"00000000000000000200", b"b").unwrap();
        db.put_cf(cf, b"00000000000000000300", b"c").unwrap();

        let records = read_uniform_orders(temp.path(), 100, 300).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].value, b"a");
        assert_eq!(records[1].value, b"b");
    }

    #[test]
    fn reads_multiple_column_families_from_one_read_only_open() {
        let temp = tempfile::tempdir().unwrap();
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db = DB::open_cf_descriptors(
            &options,
            temp.path(),
            vec![
                ColumnFamilyDescriptor::new(UNIFORM_ORDERS_CF, Options::default()),
                ColumnFamilyDescriptor::new("trade_updates", Options::default()),
            ],
        )
        .unwrap();
        db.put_cf(
            db.cf_handle(UNIFORM_ORDERS_CF).unwrap(),
            b"00000000000000000100",
            b"uniform",
        )
        .unwrap();
        db.put_cf(
            db.cf_handle("trade_updates").unwrap(),
            b"00000000000000000200",
            b"trade",
        )
        .unwrap();

        let records =
            read_all_column_families(temp.path(), &[UNIFORM_ORDERS_CF, "trade_updates"]).unwrap();
        assert_eq!(records[UNIFORM_ORDERS_CF][0].value, b"uniform");
        assert_eq!(records["trade_updates"][0].value, b"trade");
    }
}
