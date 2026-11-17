//! Read source WAL through a secondary; never open or mutate the Exec primary.
use super::*;
use rocksdb::{AsColumnFamilyRef, WriteBatchIteratorCf};

const MAX_KEYS: usize = 100_000;
const MAX_KEY_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPERATIONS: usize = 1_000_000;

pub(super) fn column_family_ids(view: &DB) -> Result<[u32; 3]> {
    let mut ids = [0; 3];
    for cf in NativeCf::ALL {
        let handle = view
            .cf_handle(cf.name())
            .context("native WAL column family")?;
        // The handle is owned by this live secondary for the complete call.
        ids[cf.index()] =
            unsafe { librocksdb_sys::rocksdb_column_family_handle_get_id(handle.inner()) };
    }
    Ok(ids)
}

struct Changes {
    ids: [u32; 3],
    puts: Vec<(NativeCf, Vec<u8>, Vec<u8>)>,
    operations: usize,
    bytes: usize,
    unsupported: bool,
    deletion: bool,
}
impl Changes {
    fn cf(&self, id: u32) -> Option<NativeCf> {
        NativeCf::ALL
            .into_iter()
            .find(|cf| self.ids[cf.index()] == id)
    }
}
impl WriteBatchIteratorCf for Changes {
    fn put_cf(&mut self, id: u32, key: &[u8], value: &[u8]) {
        self.operations += 1;
        if let Some(cf) = self.cf(id) {
            self.bytes = self
                .bytes
                .saturating_add(key.len())
                .saturating_add(value.len());
            if self.bytes > MAX_RESPONSE_BYTES {
                self.unsupported = true;
                return;
            }
            self.puts.push((cf, key.to_vec(), value.to_vec()));
        }
    }
    fn delete_cf(&mut self, id: u32, _: &[u8]) {
        self.operations += 1;
        self.deletion |= self.cf(id).is_some();
    }
    fn merge_cf(&mut self, id: u32, _: &[u8], _: &[u8]) {
        self.operations += 1;
        self.unsupported |= self.cf(id).is_some();
    }
}

/// False requires a full audit, with all unpublished work discarded first.
/// The fixed secondary's last sequence is a complete WriteBatch boundary.
pub(super) fn capture(db: &DB, view: &DB, head: &Head, plan: &mut Capture) -> Result<bool> {
    let previous = head
        .source_sequence
        .context("missing native WAL checkpoint")?;
    let end = plan.source_sequence.context("missing native WAL cut")?;
    if previous == end {
        return Ok(true);
    }
    let Ok(mut wal) = view.get_updates_since(previous) else {
        return Ok(false);
    };
    let ids = plan
        .source_cf_ids
        .context("missing native WAL column families")?;
    let mut expected = previous
        .checked_add(1)
        .context("native source sequence exhausted")?;
    let mut seen: BTreeMap<Vec<u8>, [u8; 32]> = BTreeMap::new();
    let mut key_bytes = 0;
    let mut operations = 0;
    let mut previous_hash = hash_at(db, head.cursor)?;
    let mut batch = WriteBatch::default();
    let mut batch_bytes = 0;
    while expected <= end {
        let Some(Ok((sequence, source_batch))) = wal.next() else {
            return Ok(false);
        };
        if sequence != expected {
            return Ok(false);
        }
        let count = source_batch.len();
        if count == 0 {
            return Ok(false);
        }
        let next = sequence
            .checked_add(count as u64)
            .context("native WAL sequence overflow")?;
        if next - 1 > end {
            return Ok(false);
        }
        operations += count;
        if operations > MAX_OPERATIONS {
            return Ok(false);
        }
        let mut changes = Changes {
            ids,
            puts: vec![],
            operations: 0,
            bytes: 0,
            unsupported: false,
            deletion: false,
        };
        source_batch.iterate_cf(&mut changes);
        if changes.deletion {
            return conflict("source records disappeared; original history must be preserved");
        }
        // Unhandled range deletes/transactions cannot silently advance the cut.
        if changes.unsupported || changes.operations != count {
            return Ok(false);
        }
        for (cf, key, value) in changes.puts {
            let index = hash_key(cf, &key);
            let hash: [u8; 32] = Sha256::digest(&value).into();
            let old = if let Some(hash) = seen.get(&index) {
                Some(hash.to_vec())
            } else {
                db.get(&index)?
            };
            if old.as_deref().is_some_and(|old| old != hash) && cf == NativeCf::Events {
                return conflict("immutable native evidence was modified");
            }
            if !seen.contains_key(&index) {
                key_bytes += index.len();
                if seen.len() >= MAX_KEYS || key_bytes > MAX_KEY_BYTES {
                    return Ok(false);
                }
                if old.is_none() {
                    plan.counts[cf.index()] = plan.counts[cf.index()]
                        .checked_add(1)
                        .context("native record count overflow")?;
                }
            }
            seen.insert(index, hash);
            plan.until = plan
                .until
                .checked_add(1)
                .context("native sync sequence exhausted")?;
            let change = Change::new(plan.until, cf, &key, &value, &previous_hash)?;
            previous_hash = change.record_sha256.clone();
            let encoded = serde_json::to_vec(&change)?;
            batch_bytes += encoded.len();
            batch.put(seq_key(b'J', change.sequence), encoded);
            if batch_bytes >= 4 * 1024 * 1024 {
                write(db, batch)?;
                batch = WriteBatch::default();
                batch_bytes = 0;
            }
        }
        expected = next;
    }
    if wal.status().is_err() {
        return Ok(false);
    }
    write(db, batch)?;
    Ok(true)
}
