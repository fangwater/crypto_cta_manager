//! Bounded WAL inspection for invalidating disposable observation caches.
//! Tags are RocksDB 11.8.1's db/dbformat.h; unknown records force a full read.
use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct Changes {
    pub events: bool,
    pub orders: bool,
    pub fills: bool,
    pub reset_events: bool,
}
impl Changes {
    fn all() -> Self {
        Self {
            events: true,
            orders: true,
            fills: true,
            reset_events: true,
        }
    }
}

fn varint(input: &mut &[u8]) -> Result<u32> {
    let mut value = 0;
    for shift in (0..35).step_by(7) {
        let (&byte, rest) = input.split_first().context("truncated WAL varint")?;
        *input = rest;
        ensure!(shift != 28 || byte <= 15, "oversized WAL varint");
        value |= u32::from(byte & 127) << shift;
        if byte < 128 {
            return Ok(value);
        }
    }
    anyhow::bail!("invalid WAL varint")
}
fn bytes<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let n = varint(input)? as usize;
    ensure!(n <= input.len(), "truncated WAL slice");
    let (value, rest) = input.split_at(n);
    *input = rest;
    Ok(value)
}
fn visit(data: &[u8], mut action: impl FnMut(u32, &[u8], bool)) -> Result<usize> {
    ensure!(data.len() >= 12, "truncated WAL header");
    let declared = u32::from_le_bytes(data[8..12].try_into()?) as usize;
    let mut input = &data[12..];
    let mut count = 0;
    while !input.is_empty() {
        let tag = input[0];
        input = &input[1..];
        if tag == 3 {
            bytes(&mut input)?;
            continue;
        }
        let id = if matches!(tag, 4 | 5 | 6 | 8 | 14) {
            varint(&mut input)?
        } else {
            0
        };
        let key = bytes(&mut input)?;
        match tag {
            1 | 5 | 2 | 6 => {
                bytes(&mut input)?;
                action(id, key, matches!(tag, 2 | 6));
            }
            0 | 4 | 7 | 8 => action(id, key, true),
            14 | 15 => {
                bytes(&mut input)?;
                action(id, key, true);
            }
            _ => anyhow::bail!("unsupported WAL operation"),
        }
        count += 1;
    }
    ensure!(count == declared, "incomplete WAL operation coverage");
    Ok(count)
}

pub(super) fn since(db: &DB, previous: u64, last_event_key: Option<&[u8]>) -> Changes {
    use rocksdb::AsColumnFamilyRef;
    let end = db.latest_sequence_number();
    if end == previous {
        return Changes::default();
    }
    let read = || -> Result<Changes> {
        ensure!(end > previous, "source sequence moved backwards");
        let mut ids = [0; 3];
        for (i, name) in [EVENTS, ORDERS, FILLS].into_iter().enumerate() {
            let handle = db
                .cf_handle(name)
                .context("native observation column family")?;
            // Handle is owned by this stable secondary throughout the call.
            ids[i] = unsafe { librocksdb_sys::rocksdb_column_family_handle_get_id(handle.inner()) };
        }
        let mut wal = db.get_updates_since(previous)?;
        let mut expected = previous + 1;
        let mut changed = Changes::default();
        let mut total = 0;
        while expected <= end {
            let (seq, batch) = wal.next().context("missing native observation WAL")??;
            ensure!(seq == expected, "native observation WAL gap");
            let count = visit(batch.data(), |cf, key, destructive| {
                if cf == ids[0] {
                    changed.events = true;
                    changed.reset_events |=
                        destructive || last_event_key.is_some_and(|old| key <= old);
                }
                if cf == ids[1] {
                    changed.orders = true;
                }
                if cf == ids[2] {
                    changed.fills = true;
                }
            })?;
            ensure!(count > 0, "empty native observation WAL");
            total += count;
            ensure!(total <= 1_000_000, "native observation WAL budget exceeded");
            expected = seq
                .checked_add(count as u64)
                .context("native observation WAL overflow")?;
            ensure!(
                expected - 1 <= end,
                "native observation WAL crossed view boundary"
            );
        }
        wal.status()?;
        Ok(changed)
    };
    read().unwrap_or_else(|_| Changes::all())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secondary_wal_distinguishes_appends_old_evidence_and_fee_only_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("exec");
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db = DB::open_cf(&options, &primary, [EVENTS, ORDERS, FILLS]).unwrap();
        let source = Source {
            id: "wal-observation-test".into(),
            account: "test".into(),
            adapter: "ctp".into(),
            rocksdb_path: primary.clone(),
            enabled: true,
            sync: None,
        };
        let events = db.cf_handle(EVENTS).unwrap();
        let fills = db.cf_handle(FILLS).unwrap();
        db.put_cf(events, b"001", b"original").unwrap();
        let pool = crate::rocks_source::ReaderPool::new(&tmp.path().join("secondary"), &[&primary])
            .unwrap();
        let mut previous = db.latest_sequence_number();
        let read = |previous| {
            with_db(&pool, &source, |view| {
                Ok(since(view, previous, Some(b"001")))
            })
            .unwrap()
        };

        db.put_cf(events, b"002", b"append").unwrap();
        let change = read(previous);
        assert!(change.events && !change.reset_events && !change.orders && !change.fills);
        previous = db.latest_sequence_number();

        // The replica's staging cleanup must not hide a later factual update
        // in the same WriteBatch or force a full evidence scan.
        let mut batch = rocksdb::WriteBatch::default();
        batch.delete_range(b"stage", b"stagf");
        batch.put_cf(fills, b"trade", b"revised-fee");
        db.write(batch).unwrap();
        let change = read(previous);
        assert!(change.fills && !change.events && !change.orders && !change.reset_events);
        previous = db.latest_sequence_number();

        db.put_cf(events, b"001", b"changed-old-state").unwrap();
        let change = read(previous);
        assert!(change.events && change.reset_events && !change.orders && !change.fills);
    }

    #[test]
    fn parses_range_cleanup_and_later_puts_without_silently_stopping() {
        let mut batch = rocksdb::WriteBatch::default();
        batch.put(b"before", b"one");
        batch.delete_range(b"a", b"z");
        batch.put(b"after", b"two");
        let mut rows = vec![];
        assert_eq!(
            visit(batch.data(), |cf, key, bad| rows.push((
                cf,
                key.to_vec(),
                bad
            )))
            .unwrap(),
            3
        );
        assert_eq!(
            rows,
            vec![
                (0, b"before".to_vec(), false),
                (0, b"a".to_vec(), true),
                (0, b"after".to_vec(), false)
            ]
        );
        for n in 0..batch.data().len() {
            assert!(visit(&batch.data()[..n], |_, _, _| {}).is_err());
        }
    }
}
