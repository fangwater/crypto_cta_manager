use super::*;
use std::{
    os::unix::fs::MetadataExt,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_NAV_EVIDENCE_REVISION: AtomicU64 = AtomicU64::new(1);

#[derive(PartialEq, Eq)]
struct Epoch {
    source: String,
    adapter: String,
    device: u64,
    inode: u64,
    identity: Vec<u8>,
    families: [u32; 3],
}
impl Epoch {
    fn read(source: &Source, db: &DB) -> Result<Self> {
        use rocksdb::AsColumnFamilyRef;
        let metadata = std::fs::metadata(db.path())?;
        let mut families = [0; 3];
        for (i, name) in [EVENTS, ORDERS, FILLS].into_iter().enumerate() {
            let cf = db
                .cf_handle(name)
                .context("native observation column family")?;
            // Stable view retains each real handle for this read.
            families[i] =
                unsafe { librocksdb_sys::rocksdb_column_family_handle_get_id(cf.inner()) };
        }
        Ok(Self {
            source: source.id.clone(),
            adapter: source.adapter.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
            identity: std::fs::read(db.path().join("IDENTITY"))?,
            families,
        })
    }
}

#[derive(Default)]
pub struct Cache {
    epoch: Option<Epoch>,
    database_sequence: Option<u64>,
    version: Option<(u64, Option<String>, usize)>,
    projection: Option<projection::Projection>,
    snapshot: Option<Snapshot>,
}
impl Cache {
    pub fn read(
        &mut self,
        source: &Source,
        db: &DB,
        limit: usize,
        stale_secs: u64,
    ) -> Result<Snapshot> {
        ensure!((1..=1000).contains(&limit), "invalid row limit");
        let result = self.refresh(source, db, limit, stale_secs);
        if result.is_err() {
            *self = Self::default();
        }
        result
    }
    fn refresh(
        &mut self,
        source: &Source,
        db: &DB,
        limit: usize,
        stale_secs: u64,
    ) -> Result<Snapshot> {
        let epoch = Epoch::read(source, db)?;
        if self.epoch.as_ref() != Some(&epoch) {
            *self = Self::default();
            self.epoch = Some(epoch);
        }
        let replication = if source.sync.is_some() {
            Some(super::super::sync::replica_status(db, &source.id)?)
        } else {
            None
        };
        if replication
            .as_ref()
            .is_some_and(|s| !s.initialized || s.applying)
        {
            return Err(super::super::sync::ReplicaBusy.into());
        }
        let sequence = db.latest_sequence_number();
        ensure!(
            self.database_sequence.is_none_or(|old| old <= sequence),
            "native reader sequence moved backwards"
        );
        let version = (
            replication
                .as_ref()
                .map_or(sequence, |s| s.committed_cursor),
            replication.as_ref().and_then(|s| s.store_id.clone()),
            limit,
        );
        let changed = self.database_sequence.map(|old| {
            changes::since(
                db,
                old,
                self.projection.as_ref().and_then(|p| p.last_key.as_deref()),
            )
        });
        if self.version.as_ref() == Some(&version)
            && changed.is_some_and(|c| !c.events && !c.orders && !c.fills)
        {
            if let (Some(cached), Some(projection)) = (&self.snapshot, &self.projection) {
                let mut result = cached.clone();
                result.read_at_us = super::super::now_us();
                result.account = source.account.clone();
                result.replication = replication;
                result.replication_stale =
                    replication_stale(result.replication.as_ref(), result.read_at_us, stale_secs);
                projection.apply(&mut result, stale_secs);
                self.database_sequence = Some(sequence);
                return Ok(result);
            }
        }
        let previous = if changed.is_some_and(|c| !c.reset_events) {
            self.projection.as_ref()
        } else {
            None
        };
        let projection = projection::read(db, &source.adapter, previous)?;
        let old_rows = self
            .snapshot
            .as_ref()
            .filter(|_| self.version.as_ref().is_some_and(|v| v.2 == limit));
        let reuse_orders = changed.is_some_and(|c| !c.orders);
        let reuse_fills = changed.is_some_and(|c| !c.fills);
        let mut result = read_snapshot_with_projection(
            source,
            db,
            limit,
            stale_secs,
            &projection,
            old_rows,
            reuse_orders,
            reuse_fills,
        )?;
        // Fill rewrites and corrections to existing journal evidence can change
        // NAV even when the observed account sequence and row count are unchanged.
        result.nav_evidence_revision = self
            .snapshot
            .as_ref()
            .filter(|_| changed.is_some_and(|c| !c.fills && !c.reset_events))
            .map_or_else(
                || NEXT_NAV_EVIDENCE_REVISION.fetch_add(1, Ordering::Relaxed),
                |previous| previous.nav_evidence_revision,
            );
        self.database_sequence = Some(sequence);
        self.version = Some(version);
        self.projection = Some(projection);
        self.snapshot = Some(result.clone());
        Ok(result)
    }
}
