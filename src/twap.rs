use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use iceoryx2::prelude::*;
use iceoryx2::service::ipc;
use rocksdb::{CompactOptions, Direction, IteratorMode, WriteBatch};
use sqlx::postgres::PgPool;
use tracing::{info, warn};

use crate::config::TwapConfig;
use crate::manager_db::{self, DEFAULT_CF, ManagerDb, POSITION_UPDATES_CF};
use crate::strategy_catalog;

pub const ASK_BID_SPREAD_MSG_TYPE: u32 = 1015;
pub const SPREAD_PAYLOAD_BYTES: usize = 128;
pub const TWAP_BAR_VALUE_BYTES: usize = 21;
const BBO_MAX_AGE_US: i64 = 2_000_000;
const SAMPLE_INTERVAL_US: i64 = 1_000_000;
const SPREAD_HISTORY_SIZE: usize = 100;
const SPREAD_MAX_SUBSCRIBERS: usize = 64;
const SPREAD_SUBSCRIBER_BUFFER: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwapBar {
    pub end_ts_us: i64,
    pub twap: f64,
    pub sample_count: u32,
    pub first_ts_us: i64,
}

impl TwapBar {
    pub fn encode_value(self) -> [u8; TWAP_BAR_VALUE_BYTES] {
        let mut bytes = [0u8; TWAP_BAR_VALUE_BYTES];
        bytes[0] = 1;
        bytes[1..9].copy_from_slice(&self.twap.to_le_bytes());
        bytes[9..13].copy_from_slice(&self.sample_count.to_le_bytes());
        bytes[13..21].copy_from_slice(&self.first_ts_us.to_le_bytes());
        bytes
    }

    pub fn decode(end_ts_us: i64, bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TWAP_BAR_VALUE_BYTES || bytes.first().copied()? != 1 {
            return None;
        }
        Some(Self {
            end_ts_us,
            twap: f64::from_le_bytes(bytes.get(1..9)?.try_into().ok()?),
            sample_count: u32::from_le_bytes(bytes.get(9..13)?.try_into().ok()?),
            first_ts_us: i64::from_le_bytes(bytes.get(13..21)?.try_into().ok()?),
        })
    }
}

pub fn encode_bar_key(end_ts_us: i64) -> [u8; 8] {
    manager_db::encode_ts_key(end_ts_us)
}

pub fn column_family_name(symbol: &str, venue: &str) -> Result<String> {
    let symbol = normalize_symbol(symbol);
    let venue = venue.trim();
    if symbol.is_empty() {
        bail!("TWAP symbol must not be empty");
    }
    if venue.is_empty() {
        bail!("TWAP venue must not be empty");
    }
    if symbol.contains(':') || venue.contains(':') {
        bail!("TWAP column family must not contain ':': {symbol}:{venue}");
    }
    Ok(format!("{symbol}:{venue}"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct AskBidQuote {
    pub symbol: String,
    pub ts_us: i64,
    pub mid: f64,
}

pub fn parse_ask_bid_spread(payload: &[u8]) -> Option<AskBidQuote> {
    if payload.len() < 8 {
        return None;
    }
    let msg_type = u32::from_le_bytes(payload.get(0..4)?.try_into().ok()?);
    if msg_type != ASK_BID_SPREAD_MSG_TYPE {
        return None;
    }
    let symbol_len = u32::from_le_bytes(payload.get(4..8)?.try_into().ok()?) as usize;
    let symbol_end = 8usize.checked_add(symbol_len)?;
    let numbers_end = symbol_end.checked_add(40)?;
    if payload.len() < numbers_end {
        return None;
    }
    let symbol = std::str::from_utf8(payload.get(8..symbol_end)?).ok()?;
    let ts_us = i64::from_le_bytes(payload.get(symbol_end..symbol_end + 8)?.try_into().ok()?);
    let bid = f64::from_le_bytes(
        payload
            .get(symbol_end + 8..symbol_end + 16)?
            .try_into()
            .ok()?,
    );
    let ask = f64::from_le_bytes(
        payload
            .get(symbol_end + 24..symbol_end + 32)?
            .try_into()
            .ok()?,
    );
    if !bid.is_finite() || !ask.is_finite() || bid <= 0.0 || ask <= 0.0 || ask < bid {
        return None;
    }
    Some(AskBidQuote {
        symbol: normalize_symbol(symbol),
        ts_us,
        mid: (bid + ask) * 0.5,
    })
}

pub fn encode_ask_bid_spread(
    symbol: &str,
    ts_us: i64,
    bid: f64,
    bid_qty: f64,
    ask: f64,
    ask_qty: f64,
) -> [u8; SPREAD_PAYLOAD_BYTES] {
    let mut payload = [0u8; SPREAD_PAYLOAD_BYTES];
    payload[0..4].copy_from_slice(&ASK_BID_SPREAD_MSG_TYPE.to_le_bytes());
    payload[4..8].copy_from_slice(&(symbol.len() as u32).to_le_bytes());
    payload[8..8 + symbol.len()].copy_from_slice(symbol.as_bytes());
    let numbers = 8 + symbol.len();
    payload[numbers..numbers + 8].copy_from_slice(&ts_us.to_le_bytes());
    payload[numbers + 8..numbers + 16].copy_from_slice(&bid.to_le_bytes());
    payload[numbers + 16..numbers + 24].copy_from_slice(&bid_qty.to_le_bytes());
    payload[numbers + 24..numbers + 32].copy_from_slice(&ask.to_le_bytes());
    payload[numbers + 32..numbers + 40].copy_from_slice(&ask_qty.to_le_bytes());
    payload
}

#[derive(Debug, Default)]
struct Accumulator {
    sum: f64,
    count: u32,
    first_ts_us: i64,
}

impl Accumulator {
    fn push(&mut self, quote: &AskBidQuote) {
        if !quote.mid.is_finite() {
            return;
        }
        self.sum += quote.mid;
        self.count = self.count.saturating_add(1);
        if self.first_ts_us == 0 || quote.ts_us < self.first_ts_us {
            self.first_ts_us = quote.ts_us;
        }
    }

    fn finish(&self, end_ts_us: i64) -> Option<TwapBar> {
        if self.count == 0 || !self.sum.is_finite() {
            return None;
        }
        Some(TwapBar {
            end_ts_us,
            twap: self.sum / f64::from(self.count),
            sample_count: self.count,
            first_ts_us: self.first_ts_us,
        })
    }
}

pub struct TwapStore {
    db: ManagerDb,
    retain_us: i64,
}

impl TwapStore {
    pub fn open(path: &Path, retain_days: u32) -> Result<Self> {
        Self::from_db(ManagerDb::open(path)?, retain_days)
    }

    pub fn from_db(db: ManagerDb, retain_days: u32) -> Result<Self> {
        if retain_days == 0 {
            bail!("TWAP retain_days must be greater than zero");
        }
        Ok(Self {
            db,
            retain_us: i64::from(retain_days)
                .saturating_mul(24)
                .saturating_mul(3_600)
                .saturating_mul(1_000_000),
        })
    }

    pub fn ensure_column_family(&self, name: &str) -> Result<()> {
        self.db.ensure_column_family(name)
    }

    pub fn append_bar(&self, cf_name: &str, bar: TwapBar) -> Result<()> {
        if bar.end_ts_us <= 0 || !bar.twap.is_finite() || bar.sample_count == 0 {
            bail!("invalid TWAP bar for {cf_name}");
        }
        self.ensure_column_family(cf_name)?;
        let handle =
            self.db.db().cf_handle(cf_name).with_context(|| {
                format!("TWAP column family {cf_name} disappeared after create")
            })?;
        self.db
            .db()
            .put_cf(&handle, encode_bar_key(bar.end_ts_us), bar.encode_value())
            .with_context(|| format!("failed to append TWAP bar {cf_name}"))?;
        Ok(())
    }

    pub fn compact_older_than(&self, now_ts_us: i64) -> Result<usize> {
        if now_ts_us <= self.retain_us {
            return Ok(0);
        }
        let cutoff = now_ts_us - self.retain_us;
        let cutoff_key = encode_bar_key(cutoff);
        let names = self.db.column_families()?;
        let mut deleted = 0usize;
        for name in names {
            if name == DEFAULT_CF || name == POSITION_UPDATES_CF {
                continue;
            }
            let Some(handle) = self.db.db().cf_handle(&name) else {
                continue;
            };
            let mut batch = WriteBatch::default();
            let iter = self
                .db
                .db()
                .iterator_cf(&handle, IteratorMode::From(&cutoff_key, Direction::Reverse));
            for item in iter {
                let (key, _) = item.with_context(|| format!("failed to iterate TWAP {name}"))?;
                if key.as_ref() >= cutoff_key.as_slice() {
                    continue;
                }
                batch.delete_cf(&handle, &key);
                deleted += 1;
            }
            if !batch.is_empty() {
                self.db
                    .db()
                    .write(batch)
                    .with_context(|| format!("failed to delete expired TWAP bars {name}"))?;
            }
            let mut compact = CompactOptions::default();
            compact.set_exclusive_manual_compaction(false);
            self.db.db().compact_range_cf_opt(
                &handle,
                None::<&[u8]>,
                Some(cutoff_key.as_slice()),
                &compact,
            );
        }
        Ok(deleted)
    }

    pub fn scan_bars(
        &self,
        symbol: &str,
        venue: &str,
        start_end_ts_us: i64,
        end_end_ts_us: i64,
    ) -> Result<Vec<TwapBar>> {
        if start_end_ts_us < 0 || end_end_ts_us < 0 {
            bail!("TWAP scan timestamps must not be negative");
        }
        if start_end_ts_us >= end_end_ts_us {
            return Ok(Vec::new());
        }
        let cf_name = column_family_name(symbol, venue)?;
        let Some(handle) = self.db.db().cf_handle(&cf_name) else {
            return Ok(Vec::new());
        };
        let start_key = encode_bar_key(start_end_ts_us.max(1));
        let end_key = encode_bar_key(end_end_ts_us);
        let mut out = Vec::new();
        let iter = self
            .db
            .db()
            .iterator_cf(&handle, IteratorMode::From(&start_key, Direction::Forward));
        for item in iter {
            let (key, value) = item.with_context(|| format!("failed to iterate TWAP {cf_name}"))?;
            if key.as_ref() >= end_key.as_slice() {
                break;
            }
            let Some(end_ts_us) = decode_bar_key(&key) else {
                continue;
            };
            if let Some(bar) = TwapBar::decode(end_ts_us, &value) {
                out.push(bar);
            }
        }
        Ok(out)
    }
}

pub fn decode_bar_key(bytes: &[u8]) -> Option<i64> {
    if bytes.len() != 8 {
        return None;
    }
    Some(i64::from_be_bytes(bytes.try_into().ok()?))
}

#[derive(Clone)]
struct SharedSymbols {
    inner: Arc<Mutex<HashSet<String>>>,
}

impl SharedSymbols {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn replace(&self, symbols: HashSet<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = symbols;
        }
    }

    fn contains(&self, symbol: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .is_some_and(|guard| guard.contains(symbol))
    }
}

pub fn spawn_with_db(pool: PgPool, config: TwapConfig, db: ManagerDb) {
    if !config.enabled {
        info!("CTA Manager TWAP recorder disabled");
        return;
    }

    let symbols = SharedSymbols::new();
    let catalog_symbols = symbols.clone();
    let catalog_secs = config.catalog_reload_secs.max(1);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(catalog_secs));
        loop {
            interval.tick().await;
            match load_configured_symbols(&pool).await {
                Ok(next) => {
                    let count = next.len();
                    catalog_symbols.replace(next);
                    info!(count, "CTA Manager TWAP catalog symbols refreshed");
                }
                Err(error) => {
                    warn!(error = %error, "CTA Manager TWAP catalog reload failed");
                }
            }
        }
    });

    thread::Builder::new()
        .name("cta-twap-recorder".into())
        .spawn(move || {
            if let Err(error) = run_recorder(config, db, symbols) {
                warn!(error = %error, "CTA Manager TWAP recorder stopped");
            }
        })
        .expect("failed to spawn TWAP recorder");
}

fn run_recorder(config: TwapConfig, db: ManagerDb, symbols: SharedSymbols) -> Result<()> {
    let store = TwapStore::from_db(db, config.retain_days)?;
    info!(
        path = %config.rocksdb_path.display(),
        venue = %config.venue,
        interval_ms = config.interval_ms,
        retain_days = config.retain_days,
        "CTA Manager TWAP RocksDB opened"
    );

    let node_name = "cta_web_twap";
    let node = NodeBuilder::new()
        .name(&NodeName::new(node_name)?)
        .create::<ipc::Service>()
        .with_context(|| format!("failed to create iceoryx node {node_name}"))?;
    let service_name = format!("spread_pbs/{}/ask_bid_spread", config.venue);
    info!(service_name, "opening TWAP BBO IPC");
    let service = loop {
        match node
            .service_builder(&ServiceName::new(&service_name)?)
            .publish_subscribe::<[u8; SPREAD_PAYLOAD_BYTES]>()
            .max_publishers(1)
            .max_subscribers(SPREAD_MAX_SUBSCRIBERS)
            .history_size(SPREAD_HISTORY_SIZE)
            .subscriber_max_buffer_size(SPREAD_SUBSCRIBER_BUFFER)
            .open()
        {
            Ok(service) => break service,
            Err(error) => {
                warn!(
                    service_name,
                    error = ?error,
                    "waiting for spread_pbs BBO IPC"
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let subscriber = service
        .subscriber_builder()
        .buffer_size(SPREAD_SUBSCRIBER_BUFFER)
        .create()
        .with_context(|| format!("failed to subscribe to {service_name}"))?;
    info!(service_name, "CTA Manager TWAP BBO subscribed");

    let interval_us = i64::from(config.interval_ms).saturating_mul(1_000);
    let sample_every_us = SAMPLE_INTERVAL_US.min(interval_us).max(1);
    let mut latest: HashMap<String, AskBidQuote> = HashMap::new();
    let mut open: HashMap<String, Accumulator> = HashMap::new();
    let mut current_bar_end = aligned_bar_end(unix_now_us(), interval_us);
    let mut next_sample_at = unix_now_us();
    let mut last_compact = Instant::now();

    loop {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                if let Some(quote) = parse_ask_bid_spread(sample.payload()) {
                    if symbols.contains(&quote.symbol) {
                        latest.insert(quote.symbol.clone(), quote);
                    }
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => bail!("TWAP BBO receive failed: {error}"),
        }

        let now_us = unix_now_us();
        if now_us >= current_bar_end {
            flush_bar(&store, &mut open, &config.venue, current_bar_end)?;
            current_bar_end = aligned_bar_end(now_us, interval_us);
            if current_bar_end <= now_us {
                current_bar_end = now_us.saturating_add(interval_us);
            }
        }
        if now_us >= next_sample_at {
            sample_current_bar(&mut open, &latest, now_us);
            next_sample_at = now_us.saturating_add(sample_every_us);
        }
        if last_compact.elapsed() >= Duration::from_secs(config.compact_interval_secs.max(60)) {
            match store.compact_older_than(now_us) {
                Ok(deleted) if deleted > 0 => {
                    info!(deleted, "CTA Manager TWAP expired bars compacted");
                }
                Ok(_) => {}
                Err(error) => warn!(error = %error, "CTA Manager TWAP compaction failed"),
            }
            last_compact = Instant::now();
        }
    }
}

fn sample_current_bar(
    open: &mut HashMap<String, Accumulator>,
    latest: &HashMap<String, AskBidQuote>,
    now_us: i64,
) {
    for (symbol, quote) in latest {
        if quote.ts_us <= 0 || now_us.saturating_sub(quote.ts_us) > BBO_MAX_AGE_US {
            continue;
        }
        open.entry(symbol.clone()).or_default().push(quote);
    }
}

fn flush_bar(
    store: &TwapStore,
    open: &mut HashMap<String, Accumulator>,
    venue: &str,
    bar_end: i64,
) -> Result<()> {
    for (symbol, acc) in open.drain() {
        let Some(bar) = acc.finish(bar_end) else {
            continue;
        };
        let cf = column_family_name(&symbol, venue)?;
        store.append_bar(&cf, bar)?;
    }
    Ok(())
}

async fn load_configured_symbols(pool: &PgPool) -> Result<HashSet<String>> {
    let strategies = strategy_catalog::list_position_strategies(pool).await?;
    let mut symbols = BTreeSet::new();
    for strategy in strategies {
        for symbol in strategy.targets.keys() {
            let normalized = normalize_symbol(symbol);
            if !normalized.is_empty() {
                symbols.insert(normalized);
            }
        }
    }
    Ok(symbols.into_iter().collect())
}

fn aligned_bar_end(now_us: i64, interval_us: i64) -> i64 {
    if now_us <= 0 || interval_us <= 0 {
        return 0;
    }
    let completed = now_us / interval_us;
    completed.saturating_add(1).saturating_mul(interval_us)
}

fn unix_now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn normalize_symbol(raw: &str) -> String {
    raw.chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_and_encodes_ask_bid_spread() {
        let payload =
            encode_ask_bid_spread("btcusdt", 1_700_000_000_000_001, 100.0, 1.0, 101.0, 2.0);
        let quote = parse_ask_bid_spread(&payload).expect("quote");
        assert_eq!(quote.symbol, "BTCUSDT");
        assert_eq!(quote.ts_us, 1_700_000_000_000_001);
        assert!((quote.mid - 100.5).abs() < 1e-12);
    }

    #[test]
    fn compact_bar_round_trips() {
        let bar = TwapBar {
            end_ts_us: 1_700_000_005_000_000,
            twap: 100.25,
            sample_count: 12,
            first_ts_us: 1_700_000_000_123_456,
        };
        let decoded = TwapBar::decode(bar.end_ts_us, &bar.encode_value()).expect("bar");
        assert_eq!(decoded, bar);
        assert_eq!(bar.encode_value().len(), TWAP_BAR_VALUE_BYTES);
    }

    #[test]
    fn scans_bars_in_half_open_end_ts_range() {
        let dir = TempDir::new().unwrap();
        let store = TwapStore::open(dir.path(), 1).unwrap();
        let cf = column_family_name("BTCUSDT", "binance-futures").unwrap();
        for (end_ts_us, twap) in [(5_000_000, 100.0), (10_000_000, 101.0), (15_000_000, 102.0)] {
            store
                .append_bar(
                    &cf,
                    TwapBar {
                        end_ts_us,
                        twap,
                        sample_count: 1,
                        first_ts_us: end_ts_us - 5_000_000,
                    },
                )
                .unwrap();
        }
        let scanned = store
            .scan_bars("BTCUSDT", "binance-futures", 5_000_000, 15_000_000)
            .unwrap();
        assert_eq!(scanned.len(), 2);
        assert_eq!(scanned[0].end_ts_us, 5_000_000);
        assert_eq!(scanned[1].end_ts_us, 10_000_000);
    }

    #[test]
    fn appends_bars_and_deletes_expired_history() {
        let dir = TempDir::new().unwrap();
        let store = TwapStore::open(dir.path(), 1).unwrap();
        let cf = column_family_name("BTCUSDT", "binance-futures").unwrap();
        store
            .append_bar(
                &cf,
                TwapBar {
                    end_ts_us: 1_000_000,
                    twap: 90.0,
                    sample_count: 1,
                    first_ts_us: 900_000,
                },
            )
            .unwrap();
        store
            .append_bar(
                &cf,
                TwapBar {
                    end_ts_us: 90_000_000_000_000,
                    twap: 100.0,
                    sample_count: 2,
                    first_ts_us: 89_999_000_000_000,
                },
            )
            .unwrap();

        let deleted = store
            .compact_older_than(86_400_000_000_000 + 2_000_000)
            .unwrap();
        assert_eq!(deleted, 1);

        let handle = store.db.db().cf_handle(&cf).unwrap();
        let remaining: Vec<_> = store
            .db
            .db()
            .iterator_cf(&handle, IteratorMode::Start)
            .map(|item| item.unwrap())
            .collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0.as_ref(), encode_bar_key(90_000_000_000_000));
        let kept = TwapBar::decode(90_000_000_000_000, &remaining[0].1).unwrap();
        assert!((kept.twap - 100.0).abs() < 1e-12);
    }

    #[test]
    fn compaction_skips_position_update_messages() {
        use crate::order_config::TargetPosition;
        use crate::position_archive::PositionArchive;
        use crate::strategy_catalog::PositionStrategy;

        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let archive = PositionArchive::open(db.clone()).unwrap();
        archive
            .append(
                1_000_000,
                &PositionStrategy {
                    strategy_name: "cta_a".into(),
                    targets: std::collections::BTreeMap::from([(
                        "BTCUSDT".into(),
                        TargetPosition {
                            qty: 0.1,
                            signal: 0,
                        },
                    )]),
                    updated_at_us: 1_000_000,
                },
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        let store = TwapStore::from_db(db, 1).unwrap();
        let cf = column_family_name("BTCUSDT", "binance-futures").unwrap();
        store
            .append_bar(
                &cf,
                TwapBar {
                    end_ts_us: 1_000_000,
                    twap: 90.0,
                    sample_count: 1,
                    first_ts_us: 900_000,
                },
            )
            .unwrap();
        let deleted = store
            .compact_older_than(86_400_000_000_000 + 2_000_000)
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(archive.latest().unwrap().is_some());
    }

    #[test]
    fn samples_only_fresh_quotes_into_a_time_weighted_bar() {
        let mut open = HashMap::new();
        let mut latest = HashMap::new();
        latest.insert(
            "BTCUSDT".into(),
            AskBidQuote {
                symbol: "BTCUSDT".into(),
                ts_us: 10_000_000,
                mid: 100.0,
            },
        );
        sample_current_bar(&mut open, &latest, 10_500_000);
        latest.insert(
            "BTCUSDT".into(),
            AskBidQuote {
                symbol: "BTCUSDT".into(),
                ts_us: 11_000_000,
                mid: 102.0,
            },
        );
        sample_current_bar(&mut open, &latest, 11_500_000);
        latest.insert(
            "ETHUSDT".into(),
            AskBidQuote {
                symbol: "ETHUSDT".into(),
                ts_us: 1_000_000,
                mid: 50.0,
            },
        );
        sample_current_bar(&mut open, &latest, 12_000_000);

        let bar = open["BTCUSDT"].finish(15_000_000).unwrap();
        assert_eq!(bar.sample_count, 3);
        assert!((bar.twap - (100.0 + 102.0 + 102.0) / 3.0).abs() < 1e-12);
        assert!(!open.contains_key("ETHUSDT"));
    }
}
