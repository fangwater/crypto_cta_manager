//! Incremental observation: both logs are append-only, so each refresh decodes
//! only keys after the last one it has seen. A replaced store resets the cache.
use super::*;
use std::{collections::HashMap, os::unix::fs::MetadataExt};

#[derive(PartialEq, Eq)]
struct Epoch {
    source: String,
    device: u64,
    inode: u64,
    identity: Vec<u8>,
    families: [u32; 2],
}
impl Epoch {
    fn read(source: &Source, db: &DB) -> Result<Self> {
        use rocksdb::AsColumnFamilyRef;
        let metadata = std::fs::metadata(db.path())?;
        let mut families = [0; 2];
        for (i, name) in records::COLUMN_FAMILIES.into_iter().enumerate() {
            let cf = db
                .cf_handle(name)
                .context("native observation column family")?;
            // Stable view retains each real handle for this read.
            families[i] =
                unsafe { librocksdb_sys::rocksdb_column_family_handle_get_id(cf.inner()) };
        }
        Ok(Self {
            source: source.id.clone(),
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
    last_order_key: i64,
    last_trade_key: i64,
    lifecycle_records: usize,
    trades: usize,
    /// Factual lots per `client_order_id` across the whole trade log.
    filled: HashMap<i64, i64>,
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
            *self = Self {
                epoch: Some(epoch),
                ..Self::default()
            };
        }
        let replication = replication(source, db)?;
        if replication
            .as_ref()
            .is_some_and(|s| !s.initialized || s.applying)
        {
            return Err(super::super::sync::ReplicaBusy.into());
        }
        records::scan(db, ORDERS, self.last_order_key, |key, value| {
            if let Uniform::Order(_) = records::decode_uniform(key, value)? {
                self.lifecycle_records += 1;
            }
            self.last_order_key = records::key_us(key)?;
            Ok(true)
        })?;
        records::scan(db, TRADES, self.last_trade_key, |key, value| {
            let trade = records::decode_trade(key, value)?;
            let lots = self.filled.entry(trade.client_order_id).or_default();
            *lots = lots
                .checked_add(trade.lots)
                .context("filled lots overflow")?;
            self.trades += 1;
            self.last_trade_key = trade.key_us;
            Ok(true)
        })?;
        let orders = recent_orders(db, limit)?
            .into_iter()
            .map(|order| {
                let lots = self
                    .filled
                    .get(&order.client_order_id)
                    .copied()
                    .unwrap_or(0);
                super::super::state::observe(order, lots)
            })
            .collect::<Result<_>>()?;
        let read_at_us = super::super::now_us();
        let through_us = match &replication {
            Some(replica) => replica.source_captured_at_us,
            None => Some(read_at_us),
        };
        Ok(Snapshot {
            source_id: source.id.clone(),
            account: source.account.clone(),
            read_at_us,
            last_record_us: [self.last_order_key, self.last_trade_key]
                .into_iter()
                .filter(|key| *key > 0)
                .max(),
            through_us,
            orders,
            fills: recent_trades(db, limit)?,
            total_orders: self.lifecycle_records,
            total_fills: self.trades,
            replication_stale: replication_stale(replication.as_ref(), read_at_us, stale_secs),
            replication,
        })
    }
}
