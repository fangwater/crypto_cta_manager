//! Explicit, offline adoption of a previously verified unchanged source cut.
//! This only changes disposable export metadata, never Exec or replica records.
use super::*;
use serde_json::Value;
use std::time::UNIX_EPOCH;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct FileStamp {
    size: u64,
    mtime_ns: u64,
}
#[derive(Deserialize)]
struct Baseline {
    execution_store_files: BTreeMap<String, FileStamp>,
}

fn stamps(path: &FsPath) -> Result<BTreeMap<String, FileStamp>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        ensure!(
            metadata.is_file(),
            "source checkpoint adoption requires a flat unchanged store"
        );
        files.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF8 source filename"))?,
            FileStamp {
                size: metadata.len(),
                mtime_ns: metadata
                    .modified()?
                    .duration_since(UNIX_EPOCH)?
                    .as_nanos()
                    .try_into()
                    .context("source mtime outside supported range")?,
            },
        );
    }
    Ok(files)
}

/// Requires a source-file baseline recorded BEFORE the already verified cut,
/// plus the receiver's complete committed cursor and SHA-256 checkpoint.
/// The exporter must be stopped: open_journal enforces its exclusive DB lock.
pub fn adopt_source_checkpoint(
    config: &Config,
    source_id: &str,
    source_before: &FsPath,
    cursor: u64,
    sha256: &str,
) -> Result<Value> {
    config.validate()?;
    ensure!(
        config
            .sync_export
            .as_ref()
            .is_some_and(|s| s.source_ids.iter().any(|id| id == source_id)),
        "source is not exported"
    );
    let source = config
        .sources
        .iter()
        .find(|s| s.id == source_id && s.enabled && s.sync.is_none())
        .context("source is not a local export")?;
    let before: Baseline = serde_json::from_reader(std::fs::File::open(source_before)?)?;
    ensure!(
        !before.execution_store_files.is_empty()
            && stamps(&source.rocksdb_path)? == before.execution_store_files,
        "source files changed since the pre-cut baseline; perform a full audit instead"
    );
    let journal = open_journal(config, source_id)?;
    ensure!(
        get::<Capture>(&journal.db, PENDING)?.is_none(),
        "export has an unfinished capture"
    );
    let mut head: Head = get(&journal.db, HEAD)?.context("no published export cut")?;
    let baseline_time = source_before
        .metadata()?
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_micros();
    ensure!(
        head.captured_at_us > 0 && baseline_time < head.captured_at_us as u128,
        "source baseline must predate the verified cut"
    );
    ensure!(
        cursor > 0 && head.cursor == cursor && hash_at(&journal.db, cursor)? == sha256,
        "receiver checkpoint does not match the published export cut"
    );
    let view = reader::open_secondary(
        source,
        &config
            .secondary_root
            .join(format!("checkpoint-{source_id}")),
    )?;
    let sequence = view.latest_sequence_number();
    let ids = wal::column_family_ids(&view)?;
    ensure!(
        head.source_sequence.is_none_or(|old| old == sequence)
            && head.source_cf_ids.is_none_or(|old| old == ids),
        "a different source checkpoint is already present"
    );
    ensure!(
        stamps(&source.rocksdb_path)? == before.execution_store_files,
        "source changed during checkpoint adoption"
    );
    let mut cut: Capture =
        get(&journal.db, &seq_key(b'B', cursor))?.context("missing verified cut")?;
    ensure!(
        cut.ready && cut.until == cursor && cut.counts == head.counts,
        "published cut is inconsistent"
    );
    head.source_sequence = Some(sequence);
    head.source_cf_ids = Some(ids);
    cut.source_sequence = Some(sequence);
    cut.source_cf_ids = Some(ids);
    let mut batch = WriteBatch::default();
    batch.put(HEAD, serde_json::to_vec(&head)?);
    batch.put(seq_key(b'B', cursor), serde_json::to_vec(&cut)?);
    write(&journal.db, batch)?;
    Ok(
        serde_json::json!({"source_id":source_id,"cursor":cursor,"source_sequence":sequence,"source_cf_ids":ids}),
    )
}
