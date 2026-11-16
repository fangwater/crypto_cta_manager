//! Split target/observation storage with day-local content addressing and bounded
//! snapshot replacement chains. No arithmetic quantity differencing is used.
use super::codec::pb;
use super::*;
use anyhow::ensure;
use prost::Message;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub const OBSERVATIONS_CF: &str = "position_observations";
pub const SNAPSHOTS_CF: &str = "position_snapshots";
pub(super) const FORMAT_KEY: &str = "position_storage";
pub(super) const FORMAT: &[u8] = b"protobuf";
pub(super) const MIGRATING_KEY: &str = "position_storage_migration";
pub(super) const DAY_US: i64 = 86_400_000_000;
pub(super) const MAX_DEPTH: u32 = 31;
const CACHE_ENTRIES: usize = 128;
const CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct Snapshot {
    key: Vec<u8>,
    namespace: [u8; 32],
    depth: u32,
    positions: Arc<Vec<ArchivedFactualPosition>>,
}
impl Snapshot {
    fn bytes(&self) -> usize {
        self.positions
            .iter()
            .map(|p| p.symbol.len() + std::mem::size_of::<ArchivedFactualPosition>())
            .sum::<usize>()
            + self.key.len()
    }
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<(Vec<u8>, Snapshot)>,
    bytes: usize,
}
impl Cache {
    fn get(&mut self, key: &[u8]) -> Option<Snapshot> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        let row = self.entries.remove(i).unwrap();
        let value = row.1.clone();
        self.entries.push_back(row);
        Some(value)
    }
    fn insert(&mut self, key: Vec<u8>, value: Snapshot) {
        if let Some(i) = self.entries.iter().position(|(k, _)| k == &key) {
            self.bytes -= self.entries.remove(i).unwrap().1.bytes();
        }
        let size = value.bytes();
        if size > CACHE_BYTES {
            return;
        }
        while self.entries.len() >= CACHE_ENTRIES || self.bytes + size > CACHE_BYTES {
            self.bytes -= self.entries.pop_front().unwrap().1.bytes();
        }
        self.bytes += size;
        self.entries.push_back((key, value));
    }
}
#[derive(Default)]
pub(super) struct Storage {
    heads: Mutex<Cache>,
    blobs: Mutex<Cache>,
}
pub(super) struct Pending(Vec<(Vec<u8>, Snapshot)>);

pub(super) fn families(db: &ManagerDb) -> Result<()> {
    ensure!(
        !db.column_families()?.iter().any(|f| matches!(
            f.as_str(),
            "uniform_orders" | "order_updates" | "trade_updates"
        )),
        "refuse Exec store"
    );
    db.ensure_column_family(OBSERVATIONS_CF)?;
    db.ensure_column_family(SNAPSHOTS_CF)?;
    Ok(())
}
pub(super) fn ensure_current(db: &ManagerDb) -> Result<()> {
    ensure!(
        super::online::load(db)?.is_none_or(|p| p.completed),
        "active hot conversion requires the explicit conversion owner"
    );
    ensure!(
        !db.column_families()?.iter().any(|f| matches!(
            f.as_str(),
            "uniform_orders" | "order_updates" | "trade_updates"
        )),
        "refuse Exec store"
    );
    ensure!(
        db.db().get(MIGRATING_KEY)?.is_none(),
        "signal migration is incomplete; resume cta_position_archive migrate-json before startup"
    );
    match db.db().get(FORMAT_KEY)? {
        Some(v) => ensure!(v == FORMAT, "unknown signal storage format"),
        None => {
            let cf = db
                .db()
                .cf_handle(POSITION_UPDATES_CF)
                .context("missing target CF")?;
            ensure!(
                db.db()
                    .iterator_cf(&cf, IteratorMode::Start)
                    .next()
                    .transpose()?
                    .is_none(),
                "legacy JSON archive requires explicit cta_position_archive migrate-json with a recoverable backup"
            );
            for name in [OBSERVATIONS_CF, SNAPSHOTS_CF] {
                if let Some(cf) = db.db().cf_handle(name) {
                    ensure!(
                        db.db()
                            .iterator_cf(&cf, IteratorMode::Start)
                            .next()
                            .transpose()?
                            .is_none(),
                        "unmarked store contains snapshot data; explicit recovery is required"
                    );
                }
            }
            let mut options = rocksdb::WriteOptions::default();
            options.set_sync(true);
            db.db().put_opt(FORMAT_KEY, FORMAT, &options)?;
        }
    }
    families(db)
}
pub(super) fn day_key(received_at_us: i64) -> [u8; 8] {
    // Shanghai day ordinal, avoiding overflow even at i64::MAX. This aligns
    // normal --before dates exactly with independently reclaimable blocks.
    let shift = 8 * 3_600_000_000;
    (received_at_us.div_euclid(DAY_US) + (received_at_us.rem_euclid(DAY_US) + shift) / DAY_US)
        .to_be_bytes()
}
fn namespace(source: &str, strategy: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in [source, strategy] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().into()
}
fn full(positions: &[ArchivedFactualPosition]) -> pb::SnapshotBlob {
    pb::SnapshotBlob {
        symbols: positions.iter().map(|p| p.symbol.clone()).collect(),
        values: Some(codec::pack_values(positions)),
        ..Default::default()
    }
}
fn content_key(
    day: [u8; 8],
    namespace: &[u8; 32],
    positions: &[ArchivedFactualPosition],
) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(namespace);
    hash.update(full(positions).encode_to_vec());
    let digest: [u8; 32] = hash.finalize().into();
    [day.as_slice(), &[1], digest.as_slice()].concat()
}
fn head_key(day: [u8; 8], namespace: &[u8; 32]) -> Vec<u8> {
    [day.as_slice(), &[0], namespace].concat()
}
fn same(a: &ArchivedFactualPosition, b: &ArchivedFactualPosition) -> bool {
    a.symbol == b.symbol
        && a.qty.to_bits() == b.qty.to_bits()
        && a.usdt.map(f64::to_bits) == b.usdt.map(f64::to_bits)
}

impl Storage {
    pub(super) fn clear_cache(&self) {
        *self.heads.lock().unwrap() = Cache::default();
        *self.blobs.lock().unwrap() = Cache::default();
    }
    fn load(&self, db: &ManagerDb, key: &[u8], ns: [u8; 32]) -> Result<Snapshot> {
        self.load_with(
            &|name, key| {
                let cf = db.db().cf_handle(name).context("missing signal CF")?;
                Ok(db.db().get_cf(&cf, key)?)
            },
            key,
            ns,
        )
    }
    fn load_with(
        &self,
        get: &impl Fn(&str, &[u8]) -> Result<Option<Vec<u8>>>,
        key: &[u8],
        ns: [u8; 32],
    ) -> Result<Snapshot> {
        ensure!(key.len() == 41 && key[8] == 1, "invalid snapshot key");
        let day: [u8; 8] = key[..8].try_into()?;
        let mut current = key.to_vec();
        let mut chain = Vec::new();
        let mut state = loop {
            if let Some(cached) = self.blobs.lock().unwrap().get(&current) {
                ensure!(cached.namespace == ns, "snapshot namespace mismatch");
                break Some(cached);
            }
            ensure!(
                chain.len() <= MAX_DEPTH as usize,
                "snapshot chain exceeds bound"
            );
            let bytes = get(SNAPSHOTS_CF, &current)?.context("snapshot reference is missing")?;
            let blob = pb::SnapshotBlob::decode(bytes.as_slice())?;
            ensure!(blob.depth <= MAX_DEPTH, "invalid snapshot depth");
            let base = blob.base.clone();
            chain.push((current, blob));
            if base.is_empty() {
                break None;
            }
            ensure!(
                base.len() == 41 && base[..8] == day && base[8] == 1,
                "snapshot parent crosses date block"
            );
            ensure!(
                !chain.iter().any(|(k, _)| k == &base),
                "cyclic snapshot reference"
            );
            current = base;
        };
        for (key, blob) in chain.into_iter().rev() {
            let values = codec::unpack_values(blob.values.context("missing snapshot values")?)?;
            let positions = if blob.base.is_empty() {
                ensure!(
                    blob.depth == 0
                        && blob.changed.is_empty()
                        && blob.symbols.len() == values.len(),
                    "invalid full snapshot"
                );
                blob.symbols
                    .into_iter()
                    .zip(values)
                    .map(|(symbol, (qty, usdt))| ArchivedFactualPosition { symbol, qty, usdt })
                    .collect()
            } else {
                let base = state.as_ref().context("missing decoded snapshot parent")?;
                ensure!(
                    blob.depth == base.depth + 1
                        && blob.symbols.is_empty()
                        && blob.changed.len() == values.len(),
                    "invalid snapshot replacement"
                );
                let mut positions = base.positions.as_ref().clone();
                let mut previous = None;
                for (slot, (qty, usdt)) in blob.changed.into_iter().zip(values) {
                    ensure!(
                        previous.is_none_or(|p| slot > p),
                        "duplicate/unordered snapshot slot"
                    );
                    previous = Some(slot);
                    let p = positions
                        .get_mut(slot as usize)
                        .context("snapshot slot outside baseline")?;
                    p.qty = qty;
                    p.usdt = usdt;
                }
                positions
            };
            ensure!(
                content_key(day, &ns, &positions) == key,
                "snapshot content/namespace hash mismatch"
            );
            let next = Snapshot {
                key: key.clone(),
                namespace: ns,
                depth: blob.depth,
                positions: Arc::new(positions),
            };
            self.blobs.lock().unwrap().insert(key, next.clone());
            state = Some(next);
        }
        state.context("snapshot chain is empty")
    }
    /// Caller serializes writers; all three CFs and head changes share its WAL batch.
    pub(super) fn stage(
        &self,
        db: &ManagerDb,
        message: &PositionUpdateMsg,
        batch: &mut rocksdb::WriteBatch,
        targets: bool,
    ) -> Result<Pending> {
        let key = manager_db::encode_seq_key(message.received_at_us, message.seq)?;
        if targets {
            let cf = db
                .db()
                .cf_handle(POSITION_UPDATES_CF)
                .context("missing target CF")?;
            batch.put_cf(&cf, key, codec::encode_targets(message)?);
        }
        self.stage_positions(
            db,
            key,
            &message.strategy.strategy_name,
            &message.factual_positions,
            batch,
        )
    }
    pub(super) fn stage_positions(
        &self,
        db: &ManagerDb,
        key: [u8; 12],
        strategy: &str,
        observed: &[ArchivedSourcePositions],
        batch: &mut rocksdb::WriteBatch,
    ) -> Result<Pending> {
        let cf = db
            .db()
            .cf_handle(SNAPSHOTS_CF)
            .context("missing snapshot CF")?;
        let observations = db
            .db()
            .cf_handle(OBSERVATIONS_CF)
            .context("missing observation CF")?;
        let day = day_key(
            manager_db::decode_seq_key(&key)
                .context("invalid observation key")?
                .0,
        );
        let mut pending = Vec::<(Vec<u8>, Snapshot)>::new();
        let mut refs = pb::Observations::default();
        for source in observed {
            let ns = namespace(&source.source_id, strategy);
            let hk = head_key(day, &ns);
            let ck = content_key(day, &ns, &source.positions);
            let head = pending
                .iter()
                .rev()
                .find(|(k, _)| k == &hk)
                .map(|(_, s)| s.clone())
                .or_else(|| self.heads.lock().unwrap().get(&hk));
            let head = match head {
                Some(s) => Some(s),
                None => db
                    .db()
                    .get_cf(&cf, &hk)?
                    .map(|k| self.load(db, &k, ns))
                    .transpose()?,
            };
            ensure!(
                head.as_ref().is_none_or(|s| s.key[..8] == day),
                "snapshot head crosses date block"
            );
            let staged = pending
                .iter()
                .rev()
                .find(|(_, s)| s.key == ck)
                .map(|(_, s)| s);
            let state = if let Some(s) = staged.or_else(|| head.as_ref().filter(|s| s.key == ck)) {
                s.clone()
            } else if db.db().get_cf(&cf, &ck)?.is_some() {
                self.load(db, &ck, ns)?
            } else {
                let mut blob = full(&source.positions);
                let full_size = blob.encoded_len();
                if let Some(base) = head.as_ref().filter(|b| {
                    b.depth < MAX_DEPTH
                        && b.positions.len() == source.positions.len()
                        && b.positions
                            .iter()
                            .zip(&source.positions)
                            .all(|(a, b)| a.symbol == b.symbol)
                }) {
                    let changed = base
                        .positions
                        .iter()
                        .zip(&source.positions)
                        .enumerate()
                        .filter(|(_, (a, b))| !same(a, b))
                        .map(|(i, _)| i as u32)
                        .collect::<Vec<_>>();
                    let delta = pb::SnapshotBlob {
                        base: base.key.clone(),
                        depth: base.depth + 1,
                        symbols: Vec::new(),
                        values: Some(codec::pack_values(
                            changed.iter().map(|&i| &source.positions[i as usize]),
                        )),
                        changed,
                    };
                    if delta.encoded_len() < full_size {
                        blob = delta;
                    }
                }
                batch.put_cf(&cf, &ck, blob.encode_to_vec());
                Snapshot {
                    key: ck.clone(),
                    namespace: ns,
                    depth: blob.depth,
                    positions: Arc::new(source.positions.clone()),
                }
            };
            batch.put_cf(&cf, &hk, &ck);
            refs.entries.push(pb::Observation {
                source_id: source.source_id.clone(),
                ts_ms: source.snapshot_ts_ms,
                ready: source.position_ready,
                blob: ck,
            });
            pending.push((hk, state));
        }
        if refs.entries.is_empty() {
            batch.delete_cf(&observations, key);
        } else {
            batch.put_cf(&observations, key, refs.encode_to_vec());
        }
        Ok(Pending(pending))
    }
    pub(super) fn published(&self, pending: Pending) {
        for (head, snapshot) in pending.0 {
            self.blobs
                .lock()
                .unwrap()
                .insert(snapshot.key.clone(), snapshot.clone());
            self.heads.lock().unwrap().insert(head, snapshot);
        }
    }
    pub(super) fn complete(
        &self,
        db: &ManagerDb,
        key: &[u8],
        target: &[u8],
    ) -> Result<PositionUpdateMsg> {
        self.complete_with(
            &|name, key| {
                let cf = db.db().cf_handle(name).context("missing signal CF")?;
                Ok(db.db().get_cf(&cf, key)?)
            },
            key,
            target,
        )
    }
    pub(super) fn complete_with(
        &self,
        get: &impl Fn(&str, &[u8]) -> Result<Option<Vec<u8>>>,
        key: &[u8],
        target: &[u8],
    ) -> Result<PositionUpdateMsg> {
        let mut message = codec::decode(target)?;
        ensure!(
            message.factual_positions.is_empty(),
            "live target record contains inline factual data"
        );
        ensure!(
            manager_db::decode_seq_key(key) == Some((message.received_at_us, message.seq)),
            "target key/message mismatch"
        );
        if let Some(bytes) = get(OBSERVATIONS_CF, key)? {
            for s in pb::Observations::decode(bytes.as_slice())?.entries {
                ensure!(
                    s.blob.len() == 41 && s.blob[..8] == day_key(message.received_at_us),
                    "observation crosses date block"
                );
                let snapshot = self.load_with(
                    get,
                    &s.blob,
                    namespace(&s.source_id, &message.strategy.strategy_name),
                )?;
                message.factual_positions.push(ArchivedSourcePositions {
                    source_id: s.source_id,
                    snapshot_ts_ms: s.ts_ms,
                    position_ready: s.ready,
                    positions: snapshot.positions.as_ref().clone(),
                });
            }
        }
        Ok(message)
    }
    /// Date-local parents make prefix reclamation a bounded atomic range delete.
    pub(super) fn delete_prefix(
        db: &ManagerDb,
        batch: &mut rocksdb::WriteBatch,
        from: (i64, u32),
        to: (i64, u32),
    ) -> Result<()> {
        for name in [POSITION_UPDATES_CF, OBSERVATIONS_CF] {
            let cf = db.db().cf_handle(name).context("missing signal CF")?;
            batch.delete_range_cf(
                &cf,
                manager_db::encode_seq_key(from.0, from.1)?,
                manager_db::encode_seq_key(to.0, to.1)?,
            );
        }
        let cf = db
            .db()
            .cf_handle(SNAPSHOTS_CF)
            .context("missing snapshot CF")?;
        if day_key(to.0) > day_key(0) {
            batch.delete_range_cf(&cf, day_key(0), day_key(to.0));
        }
        Ok(())
    }
}
