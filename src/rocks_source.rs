use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rocksdb::{DB, Direction, IteratorMode, Options, ReadOptions};

use crate::model::UNIFORM_ORDERS_CF;

mod secondary;
pub(crate) use secondary::ReaderPool;
pub use secondary::{ReaderSession, configure as configure_source_readers};

#[derive(Debug)]
pub struct RawRocksRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

fn read_only_options() -> Options {
    let mut options = Options::default();
    options.create_if_missing(false);
    options.create_missing_column_families(false);
    options
}

pub fn read_all_column_families(
    path: &Path,
    requested_column_families: &[&str],
) -> Result<BTreeMap<String, Vec<RawRocksRecord>>> {
    if !path.is_dir() {
        bail!("RocksDB path is not a directory: {}", path.display());
    }

    let options = read_only_options();
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

    let options = read_only_options();
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

#[derive(Debug)]
pub struct ColumnFamilyScan {
    pub records: Vec<RawRocksRecord>,
    /// The newest key observed during the scan; `None` when the column family
    /// yielded no records.
    pub last_key: Option<Vec<u8>>,
}

/// Read records from each available requested column family in one read-only
/// RocksDB open, starting at the optional inclusive start key for each column
/// family (`None` scans from the beginning). The live persist_manager remains
/// writable by Exec.
pub fn read_column_families_from(
    path: &Path,
    requests: &[(String, Option<Vec<u8>>)],
) -> Result<BTreeMap<String, ColumnFamilyScan>> {
    if !path.is_dir() {
        bail!("RocksDB path is not a directory: {}", path.display());
    }

    let options = read_only_options();
    let column_families = DB::list_cf(&options, path)
        .with_context(|| format!("failed to list column families in {}", path.display()))?;
    let db = DB::open_cf_for_read_only(&options, path, column_families.clone(), false)
        .with_context(|| format!("failed to open RocksDB {} read-only", path.display()))?;
    let mut result = BTreeMap::new();
    for (requested, start_key) in requests {
        if !column_families.iter().any(|name| name == requested) {
            continue;
        }
        let column_family = db
            .cf_handle(requested)
            .with_context(|| format!("{requested} column family disappeared after open"))?;
        let mode = match start_key {
            Some(start_key) => IteratorMode::From(start_key.as_slice(), Direction::Forward),
            None => IteratorMode::Start,
        };
        let mut records = Vec::new();
        let mut last_key = None;
        for item in db.iterator_cf(column_family, mode) {
            let (key, value) =
                item.with_context(|| format!("failed while iterating {requested}"))?;
            last_key = Some(key.to_vec());
            records.push(RawRocksRecord {
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        result.insert(requested.clone(), ColumnFamilyScan { records, last_key });
    }
    Ok(result)
}

/// Read the newest records from each available requested column family in one
/// read-only RocksDB open. The live persist_manager remains writable by Exec.
pub fn read_latest_column_families(
    path: &Path,
    requested_column_families: &[&str],
    limit: usize,
) -> Result<BTreeMap<String, Vec<RawRocksRecord>>> {
    if limit == 0 {
        return Ok(BTreeMap::new());
    }
    if !path.is_dir() {
        bail!("RocksDB path is not a directory: {}", path.display());
    }

    let options = read_only_options();
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
        for item in db.iterator_cf(column_family, IteratorMode::End).take(limit) {
            let (key, value) =
                item.with_context(|| format!("failed while iterating newest {requested}"))?;
            records.push(RawRocksRecord {
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        result.insert((*requested).to_string(), records);
    }
    Ok(result)
}

pub(crate) fn format_time_key(ts_us: i64) -> String {
    format!("{ts_us:020}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UNIFORM_ORDERS_CF;
    use rocksdb::ColumnFamilyDescriptor;

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

    #[test]
    fn reads_newest_records_in_reverse_key_order() {
        let temp = tempfile::tempdir().unwrap();
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
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
        for (key, value) in [
            (b"001".as_slice(), b"a".as_slice()),
            (b"002", b"b"),
            (b"003", b"c"),
        ] {
            db.put_cf(cf, key, value).unwrap();
        }
        drop(db);

        let records = read_latest_column_families(temp.path(), &[UNIFORM_ORDERS_CF], 2).unwrap();
        let records = &records[UNIFORM_ORDERS_CF];
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].value, b"c");
        assert_eq!(records[1].value, b"b");
    }
}

/// Visit records through a reusable, incrementally caught-up secondary. The
/// selected CFs share one stable view for this scan; Exec remains the sole writer.
pub fn visit_column_families_from(
    path: &Path,
    requests: &[(String, Option<Vec<u8>>)],
    visit: impl FnMut(&str, &[u8], &[u8]) -> Result<()>,
) -> Result<BTreeMap<String, Option<Vec<u8>>>> {
    let requested = requests
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    secondary::read(path, &requested, |db| visit_from(db, requests, None, visit))
}

#[derive(Default)]
pub(crate) struct UniformScanOptions<'a> {
    pub limits: Option<&'a BTreeMap<String, Option<Vec<u8>>>>,
    pub revision: Option<u64>,
    pub legacy_fills: bool,
}

pub(crate) struct UniformScan {
    pub heads: BTreeMap<String, Option<Vec<u8>>>,
    pub revision: u64,
    pub legacy_fills: bool,
}

/// The writer advances the revision only when existing uniform records change.
/// Read it in the same secondary view as the records and any native legacy reports.
pub(crate) fn visit_uniform_history(
    path: &Path,
    requests: &[(String, Option<Vec<u8>>)],
    options: UniformScanOptions<'_>,
    mut visit: impl FnMut(&str, &[u8], &[u8]) -> Result<bool>,
) -> Result<Option<UniformScan>> {
    let (uniform, reports) = requests.split_at(1);
    debug_assert_eq!(uniform[0].0, UNIFORM_ORDERS_CF);
    let requested = requests
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    secondary::read(path, &requested, |db| {
        let revision = db
            .get(b"uniform_orders_revision")?
            .map(|value| {
                Ok::<_, anyhow::Error>(u64::from_be_bytes(
                    value
                        .as_slice()
                        .try_into()
                        .context("invalid uniform history revision")?,
                ))
            })
            .transpose()?
            .unwrap_or_default();
        if options
            .revision
            .is_some_and(|expected| expected != revision)
        {
            return Ok(None);
        }
        let mut legacy_fills = options.legacy_fills;
        let mut heads = visit_from(db, uniform, options.limits, |cf, key, value| {
            legacy_fills |= visit(cf, key, value)?;
            Ok(())
        })?;
        if legacy_fills {
            heads.extend(visit_from(
                db,
                reports,
                options.limits,
                |cf, key, value| {
                    visit(cf, key, value)?;
                    Ok(())
                },
            )?);
        }
        Ok(Some(UniformScan {
            heads,
            revision,
            legacy_fills,
        }))
    })
}

fn visit_from(
    db: &DB,
    requests: &[(String, Option<Vec<u8>>)],
    limits: Option<&BTreeMap<String, Option<Vec<u8>>>>,
    mut visit: impl FnMut(&str, &[u8], &[u8]) -> Result<()>,
) -> Result<BTreeMap<String, Option<Vec<u8>>>> {
    let mut heads = BTreeMap::new();
    for (name, start) in requests {
        let Some(cf) = db.cf_handle(name) else {
            continue;
        };
        let mut options = ReadOptions::default();
        if let Some(limits) = limits {
            let Some(Some(end)) = limits.get(name) else {
                heads.insert(name.clone(), None);
                continue;
            };
            // The default comparator is bytewise. Appending NUL gives the
            // smallest key strictly after the inclusive generation head.
            let mut upper_bound = end.clone();
            upper_bound.push(0);
            options.set_iterate_upper_bound(upper_bound);
            options.set_readahead_size(256 * 1024);
        }
        let mut iterator = db.raw_iterator_cf_opt(cf, options);
        match start {
            Some(key) => iterator.seek(key),
            None => iterator.seek_to_first(),
        }
        let mut head = None::<Vec<u8>>;
        while let Some((key, value)) = iterator.item() {
            visit(name, key, value)?;
            let last = head.get_or_insert_with(Vec::new);
            last.clear();
            last.extend_from_slice(key);
            iterator.next();
        }
        iterator.status()?;
        heads.insert(name.clone(), head);
    }
    Ok(heads)
}
