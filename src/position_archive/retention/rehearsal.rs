//! Explicit diagnostics: read-only real-record sampling and isolated-copy tests.
//! No command in this module writes to a production Manager store.
use super::*;
use std::collections::BTreeMap;
use std::fs::File;

const MARKER: &str = "MANUAL_ARCHIVE_REHEARSAL";
const SAMPLE_LIMIT: usize = 128 * 1024 * 1024;

/// Pin one read-only secondary view while sampling bounded records across days.
/// The existing archive codec is used ONLY as a diagnostic dataset. Incomplete
/// samples cannot pass the live owner's complete-prefix receipt validation.
pub fn sample(primary: &Path, secondary: &Path, output: &mut impl Write) -> Result<()> {
    sample_with_format(primary, secondary, output, false)
}
pub fn sample_with_format(
    primary: &Path,
    secondary: &Path,
    output: &mut impl Write,
    legacy_json: bool,
) -> Result<()> {
    sample_with_options(primary, secondary, output, legacy_json, 0)
}
pub fn sample_with_options(
    primary: &Path,
    secondary: &Path,
    output: &mut impl Write,
    legacy_json: bool,
    tail_records: usize,
) -> Result<()> {
    ensure!(
        tail_records <= 10_000,
        "tail sample exceeds 10000-record limit"
    );
    use rocksdb::{DB, Options};
    let primary = primary.canonicalize()?;
    ensure!(
        secondary.is_absolute() && !secondary.exists(),
        "use a new absolute secondary path"
    );
    let parent = secondary
        .parent()
        .context("secondary parent missing")?
        .canonicalize()?;
    let secondary = parent.join(secondary.file_name().context("secondary name missing")?);
    ensure!(
        !secondary.starts_with(&primary) && !primary.starts_with(&secondary),
        "secondary overlaps source"
    );
    let mut options = Options::default();
    options.set_max_open_files(-1); // Pin source SST descriptors while the primary compacts.
    let families = DB::list_cf(&options, &primary)?;
    ensure!(
        families.iter().any(|f| f == POSITION_UPDATES_CF)
            && !families.iter().any(|f| matches!(
                f.as_str(),
                "uniform_orders" | "trade_updates" | "order_updates"
            )),
        "sampling requires a Manager signal store, never Exec"
    );
    // Never pin the large unrelated candle/order-analysis CFs just to sample
    // signals. Legacy sources need only targets; current sources also need the
    // observation references and their snapshot bodies.
    let selected = families
        .iter()
        .filter(|f| {
            matches!(f.as_str(), "default" | POSITION_UPDATES_CF)
                || (!legacy_json
                    && matches!(f.as_str(), storage::OBSERVATIONS_CF | storage::SNAPSHOTS_CF))
        })
        .collect::<Vec<_>>();
    let db = DB::open_cf_as_secondary(&options, &primary, &secondary, &selected)?;
    let cf = db.cf_handle(POSITION_UPDATES_CF).unwrap();
    if !legacy_json {
        ensure!(
            db.get(storage::FORMAT_KEY)?.as_deref() == Some(storage::FORMAT),
            "use --legacy-json explicitly for a legacy source"
        );
    }
    let storage = storage::Storage::default();
    let identity = std::fs::read_to_string(primary.join("IDENTITY"))?
        .trim()
        .to_owned();
    let state = RetentionState::decode(identity, db.get(META)?.as_deref())?;
    ensure!(
        state.floor == (1, 0),
        "rehearsal sampler requires the original unpruned source"
    );
    let first = db
        .iterator_cf(cf, IteratorMode::Start)
        .next()
        .transpose()?
        .context("no signals")?
        .0;
    let last = db
        .iterator_cf(cf, IteratorMode::End)
        .next()
        .transpose()?
        .context("no signals")?
        .0;
    let first = manager_db::decode_seq_key(&first).context("bad first key")?;
    let last = manager_db::decode_seq_key(&last).context("bad last key")?;
    let to = if last.1 == u32::MAX {
        (last.0.checked_add(1).context("overflow")?, 0)
    } else {
        (last.0, last.1 + 1)
    };
    let mut selected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut bytes = 0usize;
    let mut insert = |key: Box<[u8]>, value: Box<[u8]>| -> Result<()> {
        if !selected.contains_key(key.as_ref()) {
            let value = if legacy_json {
                super::super::migration::decode_json(&key, &value)?;
                value.into_vec()
            } else {
                codec::encode(&storage.complete_with(
                    &|name, key| {
                        let cf = db.cf_handle(name).context("missing sample CF")?;
                        Ok(db.get_cf(cf, key)?)
                    },
                    &key,
                    &value,
                )?)?
            };
            bytes = bytes
                .checked_add(key.len() + value.len())
                .context("sample overflow")?;
            ensure!(bytes <= SAMPLE_LIMIT, "sample exceeds 128 MiB cap");
            selected.insert(key.into_vec(), value);
        }
        Ok(())
    };
    if tail_records > 0 {
        for row in db.iterator_cf(cf, IteratorMode::End).take(tail_records) {
            let (key, value) = row?;
            insert(key, value)?;
        }
    } else {
        for mode in [IteratorMode::Start, IteratorMode::End] {
            for row in db.iterator_cf(cf, mode).take(64) {
                let (key, value) = row?;
                insert(key, value)?;
            }
        }
        const DAY: i64 = 86_400_000_000;
        for day in first.0.div_euclid(DAY)..=last.0.div_euclid(DAY) {
            let key = manager_db::encode_seq_key((day * DAY).max(1), 0)?;
            for row in db
                .iterator_cf(cf, IteratorMode::From(&key, Direction::Forward))
                .take(16)
            {
                let (key, value) = row?;
                if manager_db::decode_seq_key(&key)
                    .context("bad sample key")?
                    .0
                    .div_euclid(DAY)
                    != day
                {
                    break;
                }
                insert(key, value)?;
            }
        }
    }
    let header = state.header(to);
    let header_bytes = encode_header(&header)?;
    let mut hash = Sha256::new();
    hash.update(&header_bytes);
    let mut writer = GzEncoder::new(output, Compression::fast());
    writer.write_all(MAGIC)?;
    write_blob(&mut writer, &header_bytes)?;
    let mut raw_bytes = 0;
    for (key, value) in &selected {
        writer.write_all(key)?;
        write_blob(&mut writer, value)?;
        hash_frame(&mut hash, key, value);
        raw_bytes += 20 + value.len() as u64;
    }
    writer.write_all(&[0; 12])?;
    write_blob(&mut writer, &[])?;
    let receipt = Receipt {
        identity: header.identity,
        from: header.from,
        to: header.to,
        previous: header.previous,
        count: selected.len() as u64,
        raw_bytes,
        sha256: hex::encode(hash.finalize()),
    };
    write_blob(&mut writer, &serde_json::to_vec(&receipt)?)?;
    writer.finish()?.flush()?;
    eprintln!(
        "rehearsal sample only: {} records, {} raw bytes; no source writes",
        receipt.count, receipt.raw_bytes
    );
    Ok(())
}

fn marked(db: &Path) -> Result<()> {
    ensure!(db.is_absolute(), "rehearsal path must be absolute");
    let identity = std::fs::read_to_string(db.join("IDENTITY"))?;
    ensure!(
        std::fs::read_to_string(db.join(MARKER))? == identity,
        "not a marked isolated rehearsal copy"
    );
    Ok(())
}

/// Restore original sampled bytes, then create an independent writable checkpoint.
pub fn prepare(sample: &Path, root: &Path) -> Result<serde_json::Value> {
    prepare_with_format(sample, root, false)
}
pub fn prepare_with_format(
    sample: &Path,
    root: &Path,
    legacy_json: bool,
) -> Result<serde_json::Value> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        root.is_absolute() && !root.exists(),
        "rehearsal root must be a new absolute path"
    );
    std::fs::create_dir(root)?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    let reference = root.join("reference");
    let receipt = if legacy_json {
        let receipt = verify_json_restore(File::open(sample)?, &reference)?;
        super::super::migration::migrate_json(&reference, &root.join("original-json"), false)?;
        receipt
    } else {
        verify_restore(File::open(sample)?, &reference)?
    };
    let db = ManagerDb::open(&reference)?;
    let working = root.join("working");
    rocksdb::checkpoint::Checkpoint::new(db.db())?.create_checkpoint(&working)?;
    // RocksDB checkpoints do not copy IDENTITY; opening the isolated copy
    // creates its own identity before the safety marker is written.
    let _working_db = ManagerDb::open(&working)?;
    for path in [&reference, &working] {
        let identity = std::fs::read(path.join("IDENTITY"))?;
        let mut marker = File::create(path.join(MARKER))?;
        marker.write_all(&identity)?;
        marker.sync_all()?;
    }
    Ok(
        serde_json::json!({"sample_only":true,"receipt":receipt,"reference":reference,"working":working}),
    )
}

/// The same owner socket implementation; guard against any unmarked live store.
pub fn serve(db: &Path, socket: &Path) -> Result<()> {
    marked(db)?;
    ensure!(
        socket.is_absolute() && socket.starts_with(db.parent().context("copy parent missing")?),
        "socket must be inside the rehearsal root"
    );
    let archive = Arc::new(PositionArchive::open(ManagerDb::open(db)?)?);
    start_socket(archive, socket, |_| Ok(()))?;
    println!("isolated rehearsal owner ready");
    loop {
        std::thread::park();
    }
}

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(rmp_serde::to_vec(value)?)))
}

/// Compare retained original inputs and exact reconstructed boundary inventories.
/// This intentionally makes no assertion about full-production priced NAV.
pub fn proof(db: &Path, start: i64, end: i64) -> Result<serde_json::Value> {
    marked(db)?;
    ensure!(start < end, "invalid rehearsal window");
    let archive = PositionArchive::open(ManagerDb::open(db)?)?;
    let (floor, mut boundary) = archive.boundary();
    ensure!(floor.0 < start, "window crosses reclaimed boundary");
    let mut opening = None;
    let mut signal_hash = Sha256::new();
    let mut signals = 0;
    archive.visit_range(floor, (end, u32::MAX), |key, value| {
        let message = validate_message(key, value)?;
        if message.received_at_us >= start {
            if opening.is_none() {
                opening = Some(digest(&boundary)?);
            }
            hash_frame(&mut signal_hash, key, value);
            signals += 1;
        }
        boundary.ingest(&message)?;
        Ok(true)
    })?;
    let opening = opening.unwrap_or(digest(&boundary)?);
    Ok(
        serde_json::json!({"sample_only":true,"window_start_us":start,"window_end_us":end,
        "retained_signals":signals,"raw_signal_sha256":hex::encode(signal_hash.finalize()),
        "opening_inventory_sha256":opening,"closing_inventory_sha256":digest(&boundary)?,
        "source_bindings":boundary.latest.len(),"source_symbol_positions":boundary.cumulative.len()}),
    )
}
