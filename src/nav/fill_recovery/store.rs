//! Disposable on-disk evidence join. Only canonical positive fills reach FIFO.
//! Views retain an immutable generation while background evidence keeps arriving.
use super::*;
use crate::nav::{LiquidityByOrder, NavEvent, fifo_ts_us, joined_role};
use rocksdb::{BlockBasedOptions, Cache, DB, Direction, IteratorMode, Options};
use std::path::Path;
use std::sync::{Mutex, Weak};

type Id = [u8; 32];
const MAX_RECORD: usize = 8 * 1024 * 1024;
const MAX_DELTA: usize = 4096;

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(rmp_serde::to_vec_named(value)?)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(rmp_serde::from_slice(bytes)?)
}
fn key(tag: u8, suffix: &[u8]) -> Vec<u8> {
    [vec![tag], suffix.to_vec()].concat()
}
fn id(order: &OrderKey) -> Result<Id> {
    Ok(Sha256::digest(encode(order)?).into())
}
fn generation_key(tag: u8, id: &Id, generation: u64) -> Vec<u8> {
    [vec![tag], id.to_vec(), generation.to_be_bytes().to_vec()].concat()
}

#[derive(Serialize, Deserialize)]
struct Group {
    order: OrderKey,
    external: bool,
}
#[derive(Serialize, Deserialize)]
struct Evidence {
    execution: Execution,
    fill: Fill,
}
#[derive(Serialize, Deserialize)]
struct Match {
    fill: Fill,
    remaining: f64,
}

struct Disk {
    db: DB,
    directory: tempfile::TempDir,
    views: Mutex<Vec<Weak<View>>>,
}
impl std::fmt::Debug for Disk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FillEvidence")
            .field(&self.directory.path())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct View {
    disk: Arc<Disk>,
    generation: u64,
}
impl View {
    pub(crate) fn visit(&self, mut emit: impl FnMut(Arc<NavEvent>) -> Result<()>) -> Result<()> {
        for row in self.disk.db.prefix_iterator([b'O']) {
            let (key, _) = row?;
            if key.first() != Some(&b'O') {
                break;
            }
            let hash: Id = key[1..].try_into()?;
            if let Some(generation) = self.disk.at(&hash, self.generation)? {
                self.disk.events(&hash, generation, |e| emit(Arc::new(e)))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct State {
    disk: Arc<Disk>,
    portfolio: String,
    exchange: String,
    generation: u64,
    zero_rows: std::cell::Cell<usize>,
}

pub(crate) struct Update {
    pub view: Arc<View>,
    pub events: Vec<Arc<NavEvent>>,
    pub removed: BTreeSet<String>,
    pub changed_from: Option<i64>,
    pub requires_rebuild: bool,
}
impl Update {
    fn observe(&mut self, event: &NavEvent, retain: Option<i64>) {
        let ts = fifo_ts_us(event);
        self.changed_from = Some(self.changed_from.map_or(ts, |old| old.min(ts)));
        self.requires_rebuild |= retain.is_some_and(|from| ts < from);
    }
}

impl State {
    pub(crate) fn open(source: &SourceConfig, root: Option<&Path>) -> Result<Self> {
        let values = crate::market_rules::source_values(source)?;
        let portfolio = values
            .get("LTP_PORTFOLIO_ID")
            .context("missing execution portfolio scope")?
            .clone();
        ensure!(
            !portfolio.is_empty() && portfolio.bytes().all(|b| b.is_ascii_digit()),
            "invalid execution portfolio scope"
        );
        let exchange = match source.venue.as_str() {
            "binance-futures" | "binance-margin" => "BINANCE",
            "okex-futures" | "okex-margin" => "OKX",
            _ => anyhow::bail!("unsupported execution evidence venue"),
        }
        .to_owned();
        let root = root
            .map(Path::to_owned)
            .unwrap_or_else(|| std::env::temp_dir().join("crypto-cta-fill-evidence"));
        ensure!(
            !root.starts_with(&source.rocksdb_path),
            "analysis scratch cannot be inside Exec RocksDB"
        );
        std::fs::create_dir_all(&root)?;
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_dir()
                && let Some(pid) = name
                    .strip_prefix("evidence-")
                    .and_then(|s| s.split('-').next())
                    .and_then(|s| s.parse::<u32>().ok())
                && !Path::new("/proc").join(pid.to_string()).exists()
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        let directory = tempfile::Builder::new()
            .prefix(&format!("evidence-{}-", std::process::id()))
            .tempdir_in(root)?;
        let mut options = Options::default();
        options.create_if_missing(true);
        options.set_max_open_files(64);
        options.set_write_buffer_size(8 * 1024 * 1024);
        options.set_max_write_buffer_number(2);
        let mut table = BlockBasedOptions::default();
        table.set_block_cache(&Cache::new_lru_cache(8 * 1024 * 1024));
        options.set_block_based_table_factory(&table);
        let db = DB::open(&options, directory.path())?;
        Ok(Self {
            disk: Arc::new(Disk {
                db,
                directory,
                views: Mutex::default(),
            }),
            portfolio,
            exchange,
            generation: 0,
            zero_rows: Default::default(),
        })
    }

    pub(crate) fn ingest(&self, event: &NavEvent) -> Result<()> {
        ensure!(
            event.amount_update.is_finite() && event.amount_update >= 0.0,
            "invalid lifecycle fill quantity"
        );
        let external = event.client_order_id > 0
            && event.order_type_code != crate::model::INTERNAL_CROSS_ORDER_TYPE_CODE;
        let order = order_key(event);
        let hash = if external {
            id(&order)?
        } else {
            Sha256::digest(event.record_key.as_bytes()).into()
        };
        let k = [key(b'L', &hash), event.record_key.as_bytes().to_vec()].concat();
        let value = encode(event)?;
        ensure!(
            value.len() <= MAX_RECORD,
            "one lifecycle record exceeds evidence buffer"
        );
        let old = self.disk.db.get(&k)?;
        if old.as_deref() == Some(value.as_slice()) {
            return Ok(());
        }
        if old.is_none() && event.amount_update == 0.0 {
            self.zero_rows.set(self.zero_rows.get() + 1);
        }
        self.disk.db.put(k, value)?;
        self.disk
            .db
            .put(key(b'O', &hash), encode(&Group { order, external })?)?;
        if external {
            self.disk
                .db
                .put(key(b'A', &event.client_order_id.to_be_bytes()), [])?;
        }
        self.disk.db.put(key(b'D', &hash), [])?;
        Ok(())
    }

    pub(crate) fn zero_fill_records(&self) -> usize {
        self.zero_rows.get()
    }

    pub(crate) fn observe_report(&self, event: crate::model::TradeUpdateEvent) -> Result<()> {
        let order = (
            crate::exec_routing::target_symbol(&event.symbol, event.venue_code),
            event.venue_code,
            event.client_order_id,
        );
        let hash = id(&order)?;
        let known = self.disk.db.get(key(b'O', &hash))?.is_some();
        let k = key(b'T', &hash);
        let old = self.disk.db.get(&k)?;
        let mut roles = LiquidityByOrder::new();
        if let Some(old) = old.as_ref() {
            roles.insert(order.clone(), crate::nav::liquidity_from_code(old[0])?);
        }
        crate::nav::merge_liquidity_event(&mut roles, &mut Default::default(), event);
        if let Some(role) = roles.get(&order) {
            let value = [crate::nav::liquidity_code(*role)];
            if old.as_deref() != Some(value.as_slice()) {
                self.disk.db.put(k, value)?;
                if known {
                    self.disk.db.put(key(b'D', &hash), [])?;
                }
            }
        }
        Ok(())
    }

    fn durable_role(&self, role: liquidity::RoleEvidence) -> Result<()> {
        let k = key(b'J', role.execution.transaction_id.as_bytes());
        if let Some(old) = self.disk.db.get(&k)? {
            let old: liquidity::RoleEvidence = decode(&old)?;
            ensure!(
                old.validate(&role.execution)? == role.role,
                "conflicting RapidX durable fill roles"
            );
            return Ok(());
        }
        self.disk.db.put(k, encode(&role)?)?;
        // Role evidence can arrive without any new lifecycle or execution row.
        if let Some(hash) = self
            .disk
            .db
            .get(key(b'I', role.execution.transaction_id.as_bytes()))?
        {
            self.disk.db.put(key(b'D', &hash), [])?;
        }
        Ok(())
    }

    fn ingest_role(&self, raw_key: &[u8], value: &[u8]) -> Result<()> {
        let k = key(b'K', raw_key);
        if let Some(old) = self.disk.db.get(&k)? {
            ensure!(old.as_slice() == value, "durable RapidX role was changed");
            return Ok(());
        }
        if let Some(role) = liquidity::decode(raw_key, value, &self.portfolio, &self.exchange)? {
            self.durable_role(role)?;
        }
        self.disk.db.put(k, value)?;
        Ok(())
    }

    fn ingest_execution(&self, raw_key: &[u8], value: &[u8]) -> Result<()> {
        let marker = key(b'E', raw_key);
        let fingerprint: Id = Sha256::digest(value).into();
        if self.disk.db.get(&marker)?.as_deref() == Some(fingerprint.as_slice()) {
            return Ok(());
        }
        let record = RawRocksRecord {
            key: raw_key.to_vec(),
            value: value.to_vec(),
        };
        let Some((order, execution, fill)) = decode_record(
            &record,
            &self.portfolio,
            &self.exchange,
            |client| {
                Ok(self
                    .disk
                    .db
                    .get(key(b'A', &client.to_be_bytes()))?
                    .is_some())
            },
            |order| Ok(self.disk.db.get(key(b'O', &id(order)?))?.is_some()),
        )?
        else {
            return Ok(());
        };
        let hash = id(&order)?;
        let k = [key(b'X', &hash), fill.transaction_id.as_bytes().to_vec()].concat();
        if let Some(old) = self.disk.db.get(&k)? {
            let old: Evidence = decode(&old)?;
            ensure!(
                old.fill == fill,
                "conflicting WS/REST execution {}",
                fill.transaction_id
            );
        } else {
            self.disk.db.put(
                k,
                encode(&Evidence {
                    execution,
                    fill: fill.clone(),
                })?,
            )?;
            self.disk
                .db
                .put(key(b'I', fill.transaction_id.as_bytes()), hash)?;
            self.disk.db.put(key(b'D', &hash), [])?;
        }
        self.disk.db.put(marker, fingerprint)?;
        Ok(())
    }

    pub(crate) fn finish(
        &mut self,
        source: &SourceConfig,
        retain: Option<i64>,
        collect: bool,
    ) -> Result<Update> {
        // Exec execution keys are content hashes, not receive-time cursors.
        // Stream them and compare their durable identity/value on disk so a
        // newly inserted smaller key is never skipped.
        rocks_source::visit_column_families_from(
            &source.rocksdb_path,
            &[
                (ROLES_CF.to_owned(), None),
                (EXECUTIONS_CF.to_owned(), None),
            ],
            |cf, key, value| {
                if cf == ROLES_CF {
                    self.ingest_role(key, value)
                } else {
                    self.ingest_execution(key, value)
                }
            },
        )?;
        let generation = self
            .generation
            .checked_add(1)
            .context("fill generation overflow")?;
        let view = Arc::new(View {
            disk: self.disk.clone(),
            generation,
        });
        let mut update = Update {
            view: view.clone(),
            events: Vec::new(),
            removed: BTreeSet::new(),
            changed_from: None,
            requires_rebuild: false,
        };
        let mut delta_bytes = 0;
        for row in self.disk.db.prefix_iterator([b'D']) {
            let (k, _) = row?;
            if k.first() != Some(&b'D') {
                break;
            }
            let hash: Id = k[1..].try_into()?;
            self.disk.clear(&generation_key(b'P', &hash, generation))?;
            self.normalize(&hash, |event| {
                let p = [
                    generation_key(b'P', &hash, generation),
                    event.record_key.as_bytes().to_vec(),
                ]
                .concat();
                self.disk.db.put(p, encode(&event)?)?;
                Ok(())
            })?;
            self.disk
                .db
                .put(generation_key(b'R', &hash, generation), [])?;
            if collect {
                let before = self.disk.at(&hash, self.generation)?;
                self.disk.events(&hash, generation, |event| {
                    let old = before.map(|g| {
                        [
                            generation_key(b'P', &hash, g),
                            event.record_key.as_bytes().to_vec(),
                        ]
                        .concat()
                    });
                    if old
                        .as_ref()
                        .map(|key| self.disk.db.get(key))
                        .transpose()?
                        .flatten()
                        .as_deref()
                        != Some(encode(&event)?.as_slice())
                    {
                        update.observe(&event, retain);
                        delta_bytes += event.record_key.len() + encode(&event)?.len();
                        if !update.requires_rebuild {
                            update.events.push(Arc::new(event));
                        }
                    }
                    if update.events.len() > MAX_DELTA || delta_bytes > 4 * 1024 * 1024 {
                        update.requires_rebuild = true;
                        update.events.clear();
                        update.removed.clear();
                    }
                    Ok(())
                })?;
                if let Some(before) = before {
                    self.disk.events(&hash, before, |event| {
                        let new = [
                            generation_key(b'P', &hash, generation),
                            event.record_key.as_bytes().to_vec(),
                        ]
                        .concat();
                        if self.disk.db.get(new)?.as_deref() != Some(encode(&event)?.as_slice()) {
                            update.observe(&event, retain);
                            if !update.requires_rebuild {
                                update.removed.insert(event.record_key);
                            }
                        }
                        if update.removed.len() > MAX_DELTA {
                            update.requires_rebuild = true;
                            update.events.clear();
                            update.removed.clear();
                        }
                        Ok(())
                    })?;
                }
            }
            self.disk.prune(&hash, generation)?;
        }
        // Failed evidence keeps every dirty order pending and the old view live.
        self.disk.clear(&[b'D'])?;
        self.generation = generation;
        let mut views = self.disk.views.lock().unwrap();
        views.retain(|v| v.strong_count() > 0);
        views.push(Arc::downgrade(&view));
        Ok(update)
    }

    fn normalize(&self, hash: &Id, mut emit: impl FnMut(NavEvent) -> Result<()>) -> Result<()> {
        let group: Group = decode(
            &self
                .disk
                .db
                .get(key(b'O', hash))?
                .context("missing recovery order")?,
        )?;
        let mut template = None;
        let mut expected = 0.0;
        let mut unpriced = false;
        self.disk.originals(hash, |event| {
            if let Some(first) = &template {
                let first: &NavEvent = first;
                ensure!(
                    event.side_code == first.side_code
                        && event.from_key_text == first.from_key_text
                        && event.signal_ts_us == first.signal_ts_us,
                    "conflicting lifecycle order attribution"
                );
            } else {
                template = Some(event.clone());
            }
            expected += event.amount_update;
            unpriced |= needs_price(&event);
            Ok(())
        })?;
        let template = template.context("empty recovery order")?;
        ensure!(
            expected.is_finite() && expected >= 0.0,
            "invalid lifecycle fill quantity"
        );
        let mut actual = 0.0;
        let mut count = 0;
        let mut first_role = None;
        let mut same_role = true;
        let mut order_id = None;
        self.fills(hash, |_, fill| {
            actual += fill.quantity;
            ensure!(
                fill.side_code == template.side_code
                    && order_id.as_ref().is_none_or(|id| id == &fill.order_id)
                    && (template.create_ts_us <= 0
                        || fill.timestamp_us >= template.create_ts_us / 1000 * 1000),
                "execution order side/identity conflict"
            );
            order_id = Some(fill.order_id.clone());
            if count == 0 {
                first_role = fill.liquidity;
            } else {
                same_role &= first_role == fill.liquidity;
            }
            count += 1;
            Ok(())
        })?;
        let tolerance = expected.abs().max(actual.abs()) * 1e-12;
        let fallback = count == 0 || actual + tolerance < expected && !unpriced || !group.external;
        if fallback {
            ensure!(
                !unpriced,
                "waiting for complete execution prices for order {}",
                template.client_order_id
            );
            return self
                .disk
                .originals(hash, |event| self.emit_role(hash, event, &mut emit));
        }
        ensure!(
            actual.is_finite() && actual + tolerance >= expected,
            "execution quantity mismatch: lifecycle={expected} evidence={actual}"
        );
        if (actual - expected).abs() > tolerance || unpriced {
            return self.fills(hash, |_, fill| {
                let mut event = template.clone();
                event.record_key = format!(
                    "{:020}:execution:{}",
                    template.event_ts_us, fill.transaction_id
                );
                event.update_ts_us = fill.timestamp_us;
                event.price = fill.price;
                event.amount_update = fill.quantity;
                event.fill_liquidity = Some(fill.liquidity.unwrap_or(FillLiquidity::Unknown));
                self.emit_role(hash, event, &mut emit)
            });
        }
        if first_role.is_some() && same_role {
            return self.disk.originals(hash, |event| {
                let event = if event.amount_update > 0.0 {
                    with_role(&event, first_role)?
                } else {
                    event
                };
                self.emit_role(hash, event, &mut emit)
            });
        }
        // External matching preserves the existing priced lifecycle and exact
        // per-fill Maker/Taker split, even for one order larger than a batch.
        self.disk.clear(&key(b'M', hash))?;
        self.fills(hash, |_, fill| {
            let key = [
                key(b'M', hash),
                (fill.timestamp_us / 1000).to_be_bytes().to_vec(),
                fill.transaction_id.as_bytes().to_vec(),
            ]
            .concat();
            self.disk.db.put(
                key,
                encode(&Match {
                    remaining: fill.quantity,
                    fill,
                })?,
            )?;
            Ok(())
        })?;
        self.disk.originals(hash, |event| {
            if event.amount_update <= 0.0 {
                return self.emit_role(hash, event, &mut emit);
            }
            let tolerance = event.amount_update.abs() * 1e-12;
            let mut needed = event.amount_update;
            let mut parts = 0;
            let mut role = None;
            let mut same = true;
            self.matches(hash, &event, |_, fill, remaining| {
                if needed <= tolerance {
                    return Ok(false);
                }
                if !matches_fill(&event, &fill) || remaining <= 0.0 {
                    return Ok(true);
                }
                let quantity = needed.min(remaining);
                needed -= quantity;
                if parts == 0 {
                    role = fill.liquidity;
                } else {
                    same &= role == fill.liquidity;
                }
                parts += 1;
                Ok(needed > tolerance)
            })?;
            if needed > tolerance {
                return self.emit_role(hash, event, &mut emit);
            }
            let mut needed = event.amount_update;
            let mut used = 0.0;
            let mut index = 0;
            self.matches(hash, &event, |match_key, fill, remaining| {
                if needed <= tolerance {
                    return Ok(false);
                }
                if !matches_fill(&event, &fill) || remaining <= 0.0 {
                    return Ok(true);
                }
                let quantity = needed.min(remaining);
                needed -= quantity;
                if remaining - quantity <= 0.0 {
                    self.disk.db.delete(match_key)?;
                } else {
                    self.disk.db.put(
                        match_key,
                        encode(&Match {
                            fill: fill.clone(),
                            remaining: remaining - quantity,
                        })?,
                    )?;
                }
                if !same {
                    let mut part = with_role(&event, fill.liquidity)?;
                    part.amount_update = if index + 1 == parts {
                        event.amount_update - used
                    } else {
                        quantity
                    };
                    used += part.amount_update;
                    part.record_key = format!("{}:liquidity:{index:08}", event.record_key);
                    self.emit_role(hash, part, &mut emit)?;
                }
                index += 1;
                Ok(needed > tolerance)
            })?;
            if same {
                self.emit_role(hash, with_role(&event, role)?, &mut emit)?;
            }
            Ok(())
        })?;
        self.disk.clear(&key(b'M', hash))
    }

    fn matches(
        &self,
        hash: &Id,
        event: &NavEvent,
        mut visit: impl FnMut(&[u8], Fill, f64) -> Result<bool>,
    ) -> Result<()> {
        let prefix = [
            key(b'M', hash),
            (event.update_ts_us / 1000).to_be_bytes().to_vec(),
        ]
        .concat();
        for row in self.disk.db.prefix_iterator(&prefix) {
            let (key, value) = row?;
            if !key.starts_with(&prefix) {
                break;
            }
            let Match { fill, remaining } = decode(&value)?;
            if matches_fill(event, &fill) && !visit(&key, fill, remaining)? {
                break;
            }
        }
        Ok(())
    }

    fn emit_role(
        &self,
        hash: &Id,
        mut event: NavEvent,
        emit: &mut impl FnMut(NavEvent) -> Result<()>,
    ) -> Result<()> {
        if event.amount_update == 0.0 {
            return Ok(());
        }
        if event.fill_liquidity.is_none() {
            event.report_liquidity = self
                .disk
                .db
                .get(key(b'T', hash))?
                .map(|v| crate::nav::liquidity_from_code(v[0]).map(joined_role))
                .transpose()?;
        }
        emit(event)
    }
    fn fills(&self, hash: &Id, mut visit: impl FnMut(&[u8], Fill) -> Result<()>) -> Result<()> {
        let prefix = key(b'X', hash);
        for row in self.disk.db.prefix_iterator(&prefix) {
            let (k, v) = row?;
            if !k.starts_with(&prefix) {
                break;
            }
            let Evidence {
                execution,
                mut fill,
            } = decode(&v)?;
            fill.liquidity = self
                .disk
                .db
                .get(key(b'J', fill.transaction_id.as_bytes()))?
                .map(|v| decode::<liquidity::RoleEvidence>(&v)?.validate(&execution))
                .transpose()?;
            visit(&k, fill)?;
        }
        Ok(())
    }
}

fn matches_fill(event: &NavEvent, fill: &Fill) -> bool {
    fill.timestamp_us / 1000 == event.update_ts_us / 1000
        && (fill.price - event.price).abs() <= event.price.abs() * 1e-12
}

impl Disk {
    fn at(&self, hash: &Id, generation: u64) -> Result<Option<u64>> {
        let prefix = key(b'R', hash);
        let end = generation_key(b'R', hash, generation);
        let row = self
            .db
            .iterator(IteratorMode::From(&end, Direction::Reverse))
            .next()
            .transpose()?;
        row.filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| Ok(u64::from_be_bytes(k[33..].try_into()?)))
            .transpose()
    }
    fn originals(&self, hash: &Id, mut visit: impl FnMut(NavEvent) -> Result<()>) -> Result<()> {
        self.visit(&key(b'L', hash), |_, value| visit(decode(value)?))
    }
    fn events(
        &self,
        hash: &Id,
        generation: u64,
        mut visit: impl FnMut(NavEvent) -> Result<()>,
    ) -> Result<()> {
        self.visit(&generation_key(b'P', hash, generation), |_, value| {
            visit(decode(value)?)
        })
    }
    fn visit(
        &self,
        prefix: &[u8],
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<()>,
    ) -> Result<()> {
        for row in self.db.prefix_iterator(prefix) {
            let (k, v) = row?;
            if !k.starts_with(prefix) {
                break;
            }
            visit(&k, &v)?;
        }
        Ok(())
    }
    fn clear(&self, prefix: &[u8]) -> Result<()> {
        self.visit(prefix, |k, _| {
            self.db.delete(k)?;
            Ok(())
        })
    }
    fn prune(&self, hash: &Id, current: u64) -> Result<()> {
        let generations = {
            let mut views = self.views.lock().unwrap();
            views.retain(|v| v.strong_count() > 0);
            views
                .iter()
                .filter_map(|v| v.upgrade().map(|v| v.generation))
                .collect::<BTreeSet<_>>()
        };
        let mut keep = BTreeSet::from([current]);
        for generation in generations {
            if let Some(at) = self.at(hash, generation)? {
                keep.insert(at);
            }
        }
        self.visit(&key(b'R', hash), |key, _| {
            let generation = u64::from_be_bytes(key[33..].try_into()?);
            if !keep.contains(&generation) {
                self.clear(&generation_key(b'P', hash, generation))?;
                self.db.delete(key)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
