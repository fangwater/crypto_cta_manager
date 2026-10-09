//! Manager-owned, closed one-minute Binance USD-M candles. Cache misses only.
use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, StatusCode};
use rocksdb::{IteratorMode, ReadOptions, WriteBatch};
use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, OnceCell, Semaphore};
use tracing::warn;

use crate::config::KlineConfig;
use crate::manager_db::ManagerDb;

pub const MINUTE_US: i64 = 60_000_000;
pub const DAY_US: i64 = 86_400_000_000;
pub const CANDLES_CF: &str = "klines_1m";
pub const VALUE_BYTES: usize = 56;
const PAGE_LIMIT: usize = 499;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinutePrices {
    pub prices: [Option<f64>; 5],
    pub zero_volume_skipped_sample_count: usize,
    pub all_zero_volume_fallback: bool,
}
impl MinutePrices {
    pub fn priced_sample_count(&self) -> usize {
        self.prices.iter().flatten().count()
    }
    pub fn average(&self) -> Option<f64> {
        let count = self.priced_sample_count();
        (count > 0).then(|| self.prices.iter().flatten().sum::<f64>() / count as f64)
    }
    pub fn first_price(&self) -> Option<f64> {
        self.prices.iter().flatten().copied().next()
    }
}

#[derive(Default)]
struct MinuteCache {
    candles: BTreeMap<i64, Kline>,
    prices: BTreeMap<i64, MinutePrices>,
}

impl MinuteCache {
    fn insert(&mut self, candle: Kline) {
        if self
            .candles
            .get(&candle.open_ts_us)
            .is_some_and(|old| *old != candle)
        {
            for index in 0..5 {
                self.prices.remove(&(candle.open_ts_us - index * MINUTE_US));
            }
        }
        self.candles.insert(candle.open_ts_us, candle);
    }
    fn prune(&mut self, cutoff: i64) {
        self.candles = self.candles.split_off(&cutoff);
        self.prices = self.prices.split_off(&cutoff);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kline {
    pub open_ts_us: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub base_volume: f64,
    pub quote_volume: f64,
    pub trades: u64,
}

impl Kline {
    pub fn end_ts_us(&self) -> i64 {
        self.open_ts_us + MINUTE_US
    }
    pub fn vwap(&self) -> Option<f64> {
        let price = self.quote_volume / self.base_volume;
        (self.base_volume > 0.0 && self.quote_volume > 0.0 && price.is_finite() && price > 0.0)
            .then_some(price)
    }
    fn encode(&self) -> [u8; VALUE_BYTES] {
        let mut value = [0; VALUE_BYTES];
        for (index, number) in [
            self.open,
            self.high,
            self.low,
            self.close,
            self.base_volume,
            self.quote_volume,
        ]
        .iter()
        .enumerate()
        {
            value[index * 8..index * 8 + 8].copy_from_slice(&number.to_le_bytes());
        }
        value[48..56].copy_from_slice(&self.trades.to_le_bytes());
        value
    }
    fn decode(open_ts_us: i64, value: &[u8]) -> Result<Self> {
        ensure!(
            value.len() == VALUE_BYTES,
            "invalid minute candle cache value"
        );
        let number =
            |index: usize| f64::from_le_bytes(value[index * 8..index * 8 + 8].try_into().unwrap());
        let candle = Self {
            open_ts_us,
            open: number(0),
            high: number(1),
            low: number(2),
            close: number(3),
            base_volume: number(4),
            quote_volume: number(5),
            trades: u64::from_le_bytes(value[48..56].try_into().unwrap()),
        };
        candle.validate()?;
        Ok(candle)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.open_ts_us > 0 && self.open_ts_us % MINUTE_US == 0,
            "unaligned candle timestamp"
        );
        ensure!(
            [self.open, self.high, self.low, self.close]
                .iter()
                .all(|v| v.is_finite() && *v > 0.0)
                && [self.base_volume, self.quote_volume]
                    .iter()
                    .all(|v| v.is_finite() && *v >= 0.0)
                && self.high >= self.open.max(self.close)
                && self.low <= self.open.min(self.close)
                && self.high >= self.low,
            "invalid candle prices or volume"
        );
        Ok(())
    }
}

fn prefix(symbol: &str) -> Result<Vec<u8>> {
    ensure!(
        symbol.len() < 256 && crate::order_config::validate_exec_symbol(symbol).is_ok(),
        "invalid Binance symbol"
    );
    let mut key = vec![symbol.len() as u8];
    key.extend_from_slice(symbol.as_bytes());
    Ok(key)
}
fn key(symbol: &str, open_ts_us: i64) -> Result<Vec<u8>> {
    let mut key = prefix(symbol)?;
    key.extend_from_slice(&open_ts_us.to_be_bytes());
    Ok(key)
}
pub fn first_complete_open(received_at_us: i64) -> i64 {
    received_at_us
        .saturating_add(MINUTE_US - 1)
        .div_euclid(MINUTE_US)
        * MINUTE_US
}
pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CacheStatus {
    pub enabled: bool,
    pub retained_days: u32,
    pub requests: u64,
    pub fetched_candles: u64,
    pub cache_hits: u64,
    pub active_backfills: usize,
    pub last_error: Option<String>,
    pub disk_range_reads: u64,
    pub disk_batch_reads: u64,
    pub memory_candle_hits: u64,
    pub pricing_cache_hits: u64,
}
struct WeightGate {
    reservations: VecDeque<(tokio::time::Instant, u32)>,
    used: u32,
    blocked_until: tokio::time::Instant,
}
impl WeightGate {
    /// Allow concurrent requests within the budget, while enforcing it across
    /// every rolling minute (including Binance's server-directed cooldown).
    fn reserve(
        &mut self,
        now: tokio::time::Instant,
        weight: u32,
        budget: u32,
    ) -> Option<tokio::time::Instant> {
        assert!(weight <= budget);
        let window = Duration::from_secs(60);
        while let Some(&(at, expired)) = self.reservations.front() {
            if at + window > now {
                break;
            }
            self.reservations.pop_front();
            self.used -= expired;
        }
        if self.blocked_until > now {
            return Some(self.blocked_until);
        }
        if self.used + weight <= budget {
            self.reservations.push_back((now, weight));
            self.used += weight;
            return None;
        }
        let mut remaining = self.used;
        for &(at, reserved) in &self.reservations {
            remaining -= reserved;
            if remaining + weight <= budget {
                return Some(at + window);
            }
        }
        unreachable!("expiring the current reservations must release enough weight")
    }
}
struct Inner {
    db: ManagerDb,
    config: KlineConfig,
    client: OnceCell<Client>,
    egress_retry_at: AsyncMutex<tokio::time::Instant>,
    markets: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
    minutes: Mutex<BTreeMap<String, Arc<Mutex<MinuteCache>>>>,
    candle_revision: AtomicU64,
    permits: Semaphore,
    weight: AsyncMutex<WeightGate>,
    status: Mutex<CacheStatus>,
    // Fixed official origins in production; overridden only by in-module tests.
    api_url: String,
    ip_check_url: String,
}
#[derive(Clone)]
pub struct KlineStore {
    inner: Arc<Inner>,
}

struct BackfillGuard(KlineStore);
impl Drop for BackfillGuard {
    fn drop(&mut self) {
        self.0.inner.status.lock().unwrap().active_backfills -= 1;
    }
}

impl KlineStore {
    pub fn from_db(db: ManagerDb, config: KlineConfig) -> Result<Self> {
        config.validate()?;
        if config.enabled {
            check_trading_ips(&config)?;
        }
        db.ensure_column_family(CANDLES_CF)?;
        let status = CacheStatus {
            enabled: config.enabled,
            retained_days: config.retain_days,
            ..CacheStatus::default()
        };
        let instant = tokio::time::Instant::now();
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                permits: Semaphore::new(config.concurrency),
                config,
                client: OnceCell::new(),
                egress_retry_at: AsyncMutex::new(instant),
                markets: Mutex::new(BTreeMap::new()),
                minutes: Mutex::new(BTreeMap::new()),
                candle_revision: AtomicU64::new(0),
                weight: AsyncMutex::new(WeightGate {
                    reservations: VecDeque::new(),
                    used: 0,
                    blocked_until: instant,
                }),
                status: Mutex::new(status),
                api_url: "https://fapi.binance.com/fapi/v1/klines".into(),
                ip_check_url: "https://api.ipify.org".into(),
            }),
        })
    }
    pub fn enabled(&self) -> bool {
        self.inner.config.enabled
    }
    pub fn cutoff_us(&self, now: i64) -> i64 {
        now.saturating_sub(i64::from(self.inner.config.retain_days) * DAY_US)
    }
    pub fn status(&self) -> CacheStatus {
        self.inner.status.lock().unwrap().clone()
    }
    pub fn revision(&self) -> u64 {
        self.inner.candle_revision.load(Ordering::Acquire)
    }
    pub fn validate_range(&self, start: i64, end: i64, now: i64) -> Result<()> {
        ensure!(
            start >= self.cutoff_us(now) && end >= start && end <= now,
            "理论数据仅支持最近 {} 天，不能查询更早或未来的区间",
            self.inner.config.retain_days
        );
        Ok(())
    }
    pub fn get(&self, symbol: &str, open: i64) -> Result<Option<Kline>> {
        if open < self.cutoff_us(now_us()) || open + MINUTE_US > now_us() {
            return Ok(None);
        }
        self.read_minutes(symbol, &[open])
            .map(|mut bars| bars.pop().flatten())
    }
    fn minute_cache(&self, symbol: &str) -> Arc<Mutex<MinuteCache>> {
        self.inner
            .minutes
            .lock()
            .unwrap()
            .entry(symbol.to_string())
            .or_default()
            .clone()
    }

    /// Sparse reads deduplicate keys and use RocksDB's batched MultiGet. Missing
    /// minutes are never cached, so later writes/queries can repair coverage.
    pub fn read_minutes(&self, symbol: &str, opens: &[i64]) -> Result<Vec<Option<Kline>>> {
        prefix(symbol)?;
        let now = now_us();
        let cutoff = self.cutoff_us(now);
        let cache = self.minute_cache(symbol);
        let mut cache = cache.lock().unwrap();
        let wanted = opens
            .iter()
            .copied()
            .filter(|open| *open >= cutoff && *open + MINUTE_US <= now)
            .collect::<std::collections::BTreeSet<_>>();
        let missing = wanted
            .iter()
            .filter(|open| !cache.candles.contains_key(open))
            .copied()
            .collect::<Vec<_>>();
        self.inner.status.lock().unwrap().memory_candle_hits +=
            (wanted.len() - missing.len()) as u64;
        if !missing.is_empty() {
            let handle = self
                .inner
                .db
                .db()
                .cf_handle(CANDLES_CF)
                .context("minute cache disappeared")?;
            for chunk in missing.chunks(4096) {
                let keys = chunk
                    .iter()
                    .map(|open| key(symbol, *open))
                    .collect::<Result<Vec<_>>>()?;
                let values = self
                    .inner
                    .db
                    .db()
                    .batched_multi_get_cf(&handle, &keys, true);
                self.inner.status.lock().unwrap().disk_batch_reads += 1;
                for (open, value) in chunk.iter().zip(values) {
                    if let Some(value) = value? {
                        cache.insert(Kline::decode(*open, &value)?);
                    }
                }
            }
        }
        Ok(opens
            .iter()
            .map(|open| {
                (*open >= cutoff && *open + MINUTE_US <= now)
                    .then(|| cache.candles.get(open).copied())
                    .flatten()
            })
            .collect())
    }

    /// Fully closed immutable five-minute schedules are shared across accounts.
    pub fn window_prices(&self, symbol: &str, open: i64) -> Result<Option<MinutePrices>> {
        if open < self.cutoff_us(now_us()) || open + 5 * MINUTE_US > now_us() {
            return Ok(None);
        }
        let cache = self.minute_cache(symbol);
        if let Some(priced) = cache.lock().unwrap().prices.get(&open).copied() {
            self.inner.status.lock().unwrap().pricing_cache_hits += 1;
            return Ok(Some(priced));
        }
        let opens = std::array::from_fn::<_, 5, _>(|index| open + index as i64 * MINUTE_US);
        self.read_minutes(symbol, &opens)?;
        let mut cache = cache.lock().unwrap();
        let mut prices = [None; 5];
        let mut closes = [0.0; 5];
        let mut skipped = 0;
        for (index, ts) in opens.iter().enumerate() {
            let Some(candle) = cache.candles.get(ts) else {
                return Ok(None);
            };
            closes[index] = candle.close;
            if candle.base_volume == 0.0 && candle.quote_volume == 0.0 {
                skipped += 1;
            } else {
                let Some(price) = candle.vwap() else {
                    return Ok(None);
                };
                prices[index] = Some(price);
            }
        }
        let all_empty = skipped == 5;
        if all_empty {
            prices = closes.map(Some);
            skipped = 0;
        }
        let priced = MinutePrices {
            prices,
            zero_volume_skipped_sample_count: skipped,
            all_zero_volume_fallback: all_empty,
        };
        cache.prices.insert(open, priced);
        Ok(Some(priced))
    }
    pub fn scan(&self, symbol: &str, start: i64, end: i64) -> Result<Vec<Kline>> {
        let start = first_complete_open(start.max(self.cutoff_us(now_us())));
        let end = end.min(now_us().div_euclid(MINUTE_US) * MINUTE_US);
        if start >= end {
            return Ok(Vec::new());
        }
        prefix(symbol)?;
        let cache = self.minute_cache(symbol);
        let mut cache = cache.lock().unwrap();
        let present = cache
            .candles
            .range(start..end)
            .map(|(_, bar)| *bar)
            .collect::<Vec<_>>();
        self.inner.status.lock().unwrap().memory_candle_hits += present.len() as u64;
        if present.len() == ((end - start) / MINUTE_US) as usize {
            return Ok(present);
        }
        let missing = (start..end)
            .step_by(MINUTE_US as usize)
            .filter(|open| !cache.candles.contains_key(open))
            .collect::<Vec<_>>();
        if missing.len() <= 64 {
            drop(cache);
            self.read_minutes(symbol, &missing)?;
            return Ok(self
                .minute_cache(symbol)
                .lock()
                .unwrap()
                .candles
                .range(start..end)
                .map(|(_, bar)| *bar)
                .collect());
        }
        let start_key = key(symbol, missing[0])?;
        let end_key = key(symbol, missing.last().unwrap() + MINUTE_US)?;
        let handle = self
            .inner
            .db
            .db()
            .cf_handle(CANDLES_CF)
            .context("minute cache disappeared")?;
        let mut options = ReadOptions::default();
        options.set_iterate_upper_bound(end_key.clone());
        options.set_readahead_size(256 * 1024);
        self.inner.status.lock().unwrap().disk_range_reads += 1;
        for item in self.inner.db.db().iterator_cf_opt(
            &handle,
            options,
            rocksdb::IteratorMode::From(&start_key, rocksdb::Direction::Forward),
        ) {
            let (key, value) = item?;
            let prefix_len = symbol.len() + 1;
            ensure!(key.len() == prefix_len + 8, "invalid minute cache key");
            let ts = i64::from_be_bytes(key[prefix_len..].try_into().unwrap());
            cache.insert(Kline::decode(ts, &value)?);
        }
        Ok(cache
            .candles
            .range(start..end)
            .map(|(_, bar)| *bar)
            .collect())
    }
    async fn client(&self) -> Result<&Client> {
        self.inner
            .client
            .get_or_try_init(|| async {
                check_trading_ips(&self.inner.config)?;
                let mut retry = self.inner.egress_retry_at.lock().await;
                ensure!(
                    *retry <= tokio::time::Instant::now(),
                    "Kline 公网出口核验尚未通过，五分钟后重试"
                );
                *retry = tokio::time::Instant::now() + Duration::from_secs(300);
                let client = Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .local_address(
                        self.inner
                            .config
                            .local_ip
                            .context("missing Kline local_ip")?,
                    )
                    .timeout(Duration::from_secs(self.inner.config.request_timeout_secs))
                    .build()?;
                let response = client
                    .get(&self.inner.ip_check_url)
                    .send()
                    .await?
                    .error_for_status()?;
                let public: IpAddr = response
                    .text()
                    .await?
                    .trim()
                    .parse()
                    .context("invalid egress IP response")?;
                ensure!(
                    Some(public) == self.inner.config.public_ip
                        && !self.inner.config.forbidden_public_ips.contains(&public),
                    "Kline 公网出口核验失败，停止行情请求"
                );
                *retry = tokio::time::Instant::now();
                Ok(client)
            })
            .await
    }
    async fn reserve_weight(&self, weight: u32) {
        loop {
            let ready = {
                let mut gate = self.inner.weight.lock().await;
                gate.reserve(
                    tokio::time::Instant::now(),
                    weight,
                    self.inner.config.weight_per_minute,
                )
            };
            match ready {
                Some(ready) => tokio::time::sleep_until(ready).await,
                None => return,
            }
        }
    }
    /// Missing requested data backfills backwards in 24h blocks. A market mutex
    /// serializes overlapping misses; cached minutes are never requested again.
    async fn scan_async(&self, symbol: &str, start: i64, end: i64) -> Result<Vec<Kline>> {
        let store = self.clone();
        let symbol = symbol.to_owned();
        tokio::task::spawn_blocking(move || {
            crate::analysis::run(|| store.scan(&symbol, start, end))
        })
        .await
        .context("minute cache read task failed")?
    }

    pub async fn ensure_range(&self, symbol: &str, start: i64, end: i64) -> Result<()> {
        ensure!(self.enabled(), "分钟 K 线分析未启用");
        prefix(symbol)?;
        let now = now_us();
        ensure!(
            start >= self.cutoff_us(now),
            "Kline backfill cannot exceed retention"
        );
        let start = first_complete_open(start);
        let end = end.min(now.div_euclid(MINUTE_US) * MINUTE_US);
        if start >= end {
            return Ok(());
        }
        // Cached queries must not wait behind a larger historical backfill.
        if self.scan_async(symbol, start, end).await?.len() == ((end - start) / MINUTE_US) as usize
        {
            self.inner.status.lock().unwrap().cache_hits += 1;
            return Ok(());
        }
        let lock = self
            .inner
            .markets
            .lock()
            .unwrap()
            .entry(symbol.to_string())
            .or_default()
            .clone();
        let _market = lock.lock().await;
        self.inner.status.lock().unwrap().active_backfills += 1;
        let _progress = BackfillGuard(self.clone());
        // Check the requested range first: a hit must not trigger a 24h expansion.
        if self.scan_async(symbol, start, end).await?.len() == ((end - start) / MINUTE_US) as usize
        {
            self.inner.status.lock().unwrap().cache_hits += 1;
            return Ok(());
        }
        let mut block_end = end;
        while block_end > start {
            let block_start =
                first_complete_open((block_end - DAY_US).max(self.cutoff_us(now_us())));
            self.fill_missing(symbol, block_start, block_end).await?;
            block_end = block_start;
        }
        Ok(())
    }
    async fn fill_missing(&self, symbol: &str, start: i64, end: i64) -> Result<()> {
        let existing = self
            .scan_async(symbol, start, end)
            .await?
            .into_iter()
            .map(|bar| bar.open_ts_us)
            .collect::<std::collections::BTreeSet<_>>();
        let mut cursor = start;
        while cursor < end {
            if existing.contains(&cursor) {
                cursor += MINUTE_US;
                continue;
            }
            let first = cursor;
            cursor += MINUTE_US;
            while cursor < end
                && (cursor - first) / MINUTE_US < PAGE_LIMIT as i64
                && !existing.contains(&cursor)
            {
                cursor += MINUTE_US;
            }
            let result = self.fetch_page(symbol, first, cursor).await;
            match result {
                Ok(candles) => {
                    self.save_page(symbol, first, cursor, &candles)?;
                    self.inner.status.lock().unwrap().last_error = None;
                }
                Err(error) => {
                    self.inner.status.lock().unwrap().last_error = Some(error.to_string());
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    async fn fetch_page(&self, symbol: &str, start: i64, end: i64) -> Result<Vec<Kline>> {
        check_trading_ips(&self.inner.config)?;
        let client = self.client().await?;
        let count = ((end - start) / MINUTE_US) as usize;
        let weight = if count < 100 { 1 } else { 2 };
        let _permit = self.inner.permits.acquire().await?;
        self.reserve_weight(weight).await;
        self.inner.status.lock().unwrap().requests += 1;
        let response = client
            .get(&self.inner.api_url)
            .query(&[
                ("symbol", symbol.to_string()),
                ("interval", "1m".into()),
                ("startTime", (start / 1000).to_string()),
                ("endTime", (end / 1000 - 1).to_string()),
                ("limit", count.to_string()),
            ])
            .send()
            .await?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS || response.status().as_u16() == 418 {
            let seconds = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(120)
                .max(1);
            let mut gate = self.inner.weight.lock().await;
            gate.blocked_until = gate
                .blocked_until
                .max(tokio::time::Instant::now() + Duration::from_secs(seconds));
            bail!("Binance Kline 限流，{} 秒后重试", seconds);
        }
        let raw: Vec<serde_json::Value> = response.error_for_status()?.json().await?;
        let mut candles = Vec::new();
        for row in raw {
            let candle = parse_candle(&row)?;
            ensure!(
                candle.open_ts_us >= start
                    && candle.end_ts_us() <= end
                    && candle.end_ts_us() <= now_us(),
                "Binance returned a candle outside the closed requested range"
            );
            candles.push(candle);
        }
        candles.sort_by_key(|c| c.open_ts_us);
        ensure!(
            candles
                .windows(2)
                .all(|pair| pair[0].open_ts_us < pair[1].open_ts_us),
            "duplicate Binance candle"
        );
        Ok(candles)
    }
    pub(crate) fn save_page(
        &self,
        symbol: &str,
        start: i64,
        end: i64,
        candles: &[Kline],
    ) -> Result<()> {
        let data = self
            .inner
            .db
            .db()
            .cf_handle(CANDLES_CF)
            .context("minute cache disappeared")?;
        let cutoff = self.cutoff_us(now_us());
        let mut batch = WriteBatch::default();
        for candle in candles {
            candle.validate()?;
            ensure!(
                candle.open_ts_us >= start && candle.end_ts_us() <= end,
                "candle outside cache page range"
            );
            if candle.open_ts_us >= cutoff {
                batch.put_cf(&data, key(symbol, candle.open_ts_us)?, candle.encode());
            }
        }
        self.inner.db.db().write(batch)?;
        let cache = self.minute_cache(symbol);
        let mut cache = cache.lock().unwrap();
        for candle in candles.iter().filter(|bar| bar.open_ts_us >= cutoff) {
            cache.insert(*candle);
        }
        if !candles.is_empty() {
            self.inner.candle_revision.fetch_add(1, Ordering::Release);
        }
        self.inner.status.lock().unwrap().fetched_candles += candles.len() as u64;
        Ok(())
    }
    pub fn prune(&self) -> Result<usize> {
        let cutoff = self.cutoff_us(now_us());
        let mut removed = 0;
        let handle = self
            .inner
            .db
            .db()
            .cf_handle(CANDLES_CF)
            .context("Kline cache disappeared")?;
        let mut batch = WriteBatch::default();
        for item in self.inner.db.db().iterator_cf(&handle, IteratorMode::Start) {
            let (key, _) = item?;
            ensure!(
                key.len() >= 9 && key.len() == key[0] as usize + 9,
                "invalid minute cache key"
            );
            let ts = i64::from_be_bytes(key[key.len() - 8..].try_into().unwrap());
            if ts < cutoff {
                batch.delete_cf(&handle, &key);
                removed += 1;
            }
            if batch.len() >= 4096 {
                self.inner.db.db().write(batch)?;
                batch = WriteBatch::default();
            }
        }
        if !batch.is_empty() {
            self.inner.db.db().write(batch)?;
        }
        self.inner
            .db
            .db()
            .compact_range_cf(&handle, None::<&[u8]>, None::<&[u8]>);
        for cache in self.inner.minutes.lock().unwrap().values() {
            cache.lock().unwrap().prune(cutoff);
        }
        if removed > 0 {
            self.inner.candle_revision.fetch_add(1, Ordering::Release);
        }
        Ok(removed)
    }
    pub fn spawn_defaults(&self) {
        if !self.enabled() {
            return;
        }
        let store = self.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(store.inner.config.refresh_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let end = now_us().div_euclid(MINUTE_US) * MINUTE_US;
                let mut jobs = tokio::task::JoinSet::new();
                for symbol in &store.inner.config.default_symbols {
                    let store = store.clone();
                    let symbol = symbol.clone();
                    jobs.spawn(async move {
                        store
                            .ensure_range(
                                &symbol,
                                (end - DAY_US).max(store.cutoff_us(now_us())),
                                end,
                            )
                            .await
                    });
                }
                while let Some(result) = jobs.join_next().await {
                    if let Err(error) = result.unwrap_or_else(|error| Err(error.into())) {
                        warn!(%error, "default Kline refresh failed");
                    }
                }
            }
        });
        let store = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(
                store.inner.config.compact_interval_secs,
            ));
            loop {
                interval.tick().await;
                let store = store.clone();
                match tokio::task::spawn_blocking(move || store.prune()).await {
                    Ok(Ok(_)) => {}
                    result => warn!(?result, "Kline retention cleanup failed"),
                }
            }
        });
    }
}

fn parse_candle(raw: &serde_json::Value) -> Result<Kline> {
    let row = raw.as_array().context("invalid Binance candle row")?;
    ensure!(row.len() >= 9, "short Binance candle row");
    let number = |index: usize| -> Result<f64> {
        row[index]
            .as_str()
            .context("invalid candle decimal")?
            .parse()
            .context("invalid candle number")
    };
    let open_ms = row[0].as_i64().context("invalid candle open time")?;
    let open_ts_us = open_ms
        .checked_mul(1000)
        .context("candle timestamp overflow")?;
    let candle = Kline {
        open_ts_us,
        open: number(1)?,
        high: number(2)?,
        low: number(3)?,
        close: number(4)?,
        base_volume: number(5)?,
        quote_volume: number(7)?,
        trades: row[8].as_u64().context("invalid trade count")?,
    };
    candle.validate()?;
    ensure!(
        row[6].as_i64() == Some(open_ms + 59_999),
        "Binance candle must span exactly one minute"
    );
    Ok(candle)
}

fn check_trading_ips(config: &KlineConfig) -> Result<()> {
    check_trading_ips_with_route(config, default_route_local_ip)
}

pub(crate) fn default_route_local_ip(unspecified: IpAddr) -> Result<IpAddr> {
    // UDP connect selects a route without sending a packet or using a trading API.
    let destination: IpAddr = if unspecified.is_ipv4() {
        "192.0.2.1".parse().unwrap()
    } else {
        "2001:db8::1".parse().unwrap()
    };
    let socket = UdpSocket::bind((unspecified, 0))?;
    socket.connect((destination, 443))?;
    let actual = socket.local_addr()?.ip();
    ensure!(
        !actual.is_unspecified(),
        "cannot resolve the trading default-route address"
    );
    Ok(actual)
}

fn check_trading_ips_with_route(
    config: &KlineConfig,
    resolve_default: impl Fn(IpAddr) -> Result<IpAddr>,
) -> Result<()> {
    let local = config.local_ip.context("missing Kline local IP")?;
    let public = config.public_ip.context("missing Kline public IP")?;
    for path in &config.trade_engine_configs {
        // Deliberately read no credential/env files and print no TOML contents.
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read IP exclusion config {}", path.display()))?;
        let table: toml::Value = toml::from_str(&content)
            .map_err(|_| anyhow::anyhow!("invalid trade engine IP config: {}", path.display()))?;
        let mut ips = Vec::new();
        if let Some(array) = table.get("local_ips") {
            for value in array
                .as_array()
                .context("trade engine local_ips must be an array")?
            {
                ips.push(
                    value
                        .as_str()
                        .context("invalid trading IP")?
                        .trim()
                        .parse::<IpAddr>()?,
                );
            }
        }
        for field in [
            "primary_local_ip",
            "secondary_local_ip",
            "binance_um_whitelist_ip",
            "binance_um_ip_whitelist_ip",
        ] {
            if let Some(value) = table.get(field) {
                if let Some(value) = value.as_str().filter(|v| !v.trim().is_empty()) {
                    ips.push(value.trim().parse::<IpAddr>()?);
                }
            }
        }
        ensure!(
            !ips.is_empty(),
            "trade engine IP config must list trading addresses"
        );
        let uses_default_route = ips.iter().any(IpAddr::is_unspecified);
        for ip in &mut ips {
            if ip.is_unspecified() {
                *ip = resolve_default(*ip).context("resolve unbound trading egress")?;
                ensure!(
                    !ip.is_unspecified(),
                    "cannot resolve the trading default-route address"
                );
            }
        }
        if ips
            .iter()
            .any(|ip| matches!(ip, IpAddr::V4(address) if address.is_private()) || matches!(ip, IpAddr::V6(address) if address.is_unique_local() || address.is_unicast_link_local()))
            || uses_default_route
        {
            ensure!(
                !config.forbidden_public_ips.is_empty(),
                "NAT or default-route trading IPs require kline.forbidden_public_ips"
            );
        }
        ensure!(
            !ips.contains(&local) && !ips.contains(&public),
            "Kline 出口与 trade engine 下单 IP 冲突，停止行情请求"
        );
    }
    ensure!(
        !config.forbidden_public_ips.contains(&public),
        "Kline uses a trading public IP"
    );
    Ok(())
}

pub fn validate_host_configs(config: &crate::config::AppConfig) -> Result<()> {
    if !config.kline.enabled {
        return Ok(());
    }
    for source in config.sources.iter().filter(|source| source.enabled) {
        // Config path discovery is read-only and independent of Exec libraries.
        if let Some(root) = source.rocksdb_path.parent().and_then(|p| p.parent()) {
            for filename in ["trade_engine.toml", "trade engine.toml"] {
                let path = root.join(filename);
                if path.is_file() {
                    let actual = std::fs::canonicalize(&path)?;
                    ensure!(
                        config
                            .kline
                            .trade_engine_configs
                            .iter()
                            .any(|configured| std::fs::canonicalize(configured).ok().as_ref()
                                == Some(&actual)),
                        "kline.trade_engine_configs omits an enabled account's trading config: {}",
                        path.display()
                    );
                }
            }
        }
    }
    check_trading_ips(&config.kline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Query, State},
        routing::get,
    };
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct MockState {
        calls: Arc<Mutex<Vec<(i64, i64)>>>,
        symbols: Arc<Mutex<Vec<String>>>,
        omit: Option<i64>,
    }
    async fn mock_candles(
        State(state): State<MockState>,
        Query(params): Query<BTreeMap<String, String>>,
    ) -> Json<Vec<serde_json::Value>> {
        assert_eq!(params["interval"], "1m");
        let start = params["startTime"].parse::<i64>().unwrap() * 1000;
        let end = (params["endTime"].parse::<i64>().unwrap() + 1) * 1000;
        let limit = params["limit"].parse::<usize>().unwrap();
        state.calls.lock().unwrap().push((start, end));
        state.symbols.lock().unwrap().push(params["symbol"].clone());
        let mut result = Vec::new();
        let mut ts = start;
        while ts < end && result.len() < limit {
            if Some(ts) != state.omit {
                result.push(serde_json::json!([
                    ts / 1000,
                    "100",
                    "105",
                    "99",
                    "102",
                    "2",
                    ts / 1000 + 59_999,
                    "202",
                    7,
                    "1",
                    "100",
                    "0"
                ]));
            }
            ts += MINUTE_US;
        }
        Json(result)
    }
    async fn fixture(
        omit: Option<i64>,
    ) -> (TempDir, KlineStore, MockState, tokio::task::JoinHandle<()>) {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("trade_engine.toml");
        std::fs::write(&config_path, "local_ips = [\"10.11.12.13\"]").unwrap();
        let config = KlineConfig {
            trade_engine_configs: vec![config_path],
            local_ip: Some("127.0.0.1".parse().unwrap()),
            public_ip: Some("198.51.100.50".parse().unwrap()),
            forbidden_public_ips: vec!["198.51.100.4".parse().unwrap()],
            weight_per_minute: 1200,
            ..Default::default()
        };
        let mut store =
            KlineStore::from_db(ManagerDb::open(&dir.path().join("db")).unwrap(), config).unwrap();
        let state = MockState {
            omit,
            ..Default::default()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/klines", get(mock_candles))
            .route("/ip", get(|| async { "198.51.100.50" }))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // The test server uses loopback. Production validation rejects loopback.
        let inner = Arc::get_mut(&mut store.inner).unwrap();
        inner.config.enabled = true;
        inner.api_url = format!("http://{address}/klines");
        inner.ip_check_url = format!("http://{address}/ip");
        (dir, store, state, server)
    }
    fn candle(open_ts_us: i64) -> Kline {
        Kline {
            open_ts_us,
            open: 100.0,
            high: 105.0,
            low: 99.0,
            close: 102.0,
            base_volume: 2.0,
            quote_volume: 202.0,
            trades: 7,
        }
    }

    #[test]
    fn request_weight_allows_a_burst_but_never_exceeds_a_rolling_minute() {
        let now = tokio::time::Instant::now();
        let mut gate = WeightGate {
            reservations: VecDeque::new(),
            used: 0,
            blocked_until: now,
        };
        for _ in 0..120 {
            assert_eq!(gate.reserve(now, 1, 120), None);
        }
        let release = now + Duration::from_secs(60);
        assert_eq!(gate.reserve(now, 1, 120), Some(release));
        assert_eq!(
            gate.reserve(release - Duration::from_nanos(1), 2, 120),
            Some(release)
        );
        assert_eq!(gate.used, 120, "waiting must not spend weight");
        assert_eq!(gate.reserve(release, 2, 120), None);
        assert_eq!(gate.used, 2);
        assert_eq!(gate.reservations.len(), 1);
    }

    #[test]
    fn weighted_requests_wait_until_enough_of_the_window_expires() {
        let now = tokio::time::Instant::now();
        let mut gate = WeightGate {
            reservations: VecDeque::new(),
            used: 0,
            blocked_until: now,
        };
        assert_eq!(gate.reserve(now, 2, 6), None);
        assert_eq!(gate.reserve(now + Duration::from_secs(10), 1, 6), None);
        assert_eq!(gate.reserve(now + Duration::from_secs(20), 3, 6), None);
        assert_eq!(
            gate.reserve(now + Duration::from_secs(30), 3, 6),
            Some(now + Duration::from_secs(70))
        );
        assert_eq!(
            gate.reserve(now + Duration::from_secs(60), 3, 6),
            Some(now + Duration::from_secs(70))
        );
        assert_eq!(gate.reserve(now + Duration::from_secs(70), 3, 6), None);
        assert_eq!(gate.used, 6);
        gate.blocked_until = now + Duration::from_secs(150);
        assert_eq!(
            gate.reserve(now + Duration::from_secs(140), 1, 6),
            Some(now + Duration::from_secs(150))
        );
        assert_eq!(gate.used, 0);
        assert_eq!(gate.reserve(now + Duration::from_secs(150), 1, 6), None);
    }

    #[tokio::test]
    async fn fresh_multi_symbol_queries_do_not_serialize_unused_request_budget() {
        let (_dir, mut store, _state, server) = fixture(None).await;
        Arc::get_mut(&mut store.inner)
            .unwrap()
            .config
            .weight_per_minute = 120;
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..83 {
            let store = store.clone();
            jobs.spawn(async move { store.reserve_weight(1).await });
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(result) = jobs.join_next().await {
                result.unwrap();
            }
        })
        .await
        .expect("83 requests within budget must not be spread over 41 seconds");
        assert_eq!(store.inner.weight.lock().await.used, 83);
        server.abort();
    }

    #[test]
    fn sparse_minutes_use_one_deduplicated_batch_and_cache_repair_is_visible() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let start = first_complete_open(now_us() - DAY_US);
        let store = KlineStore::from_db(db.clone(), Default::default()).unwrap();
        let handle = db.db().cf_handle(CANDLES_CF).unwrap();
        for index in [0, 2, 3, 4] {
            let row = candle(start + index * MINUTE_US);
            db.db()
                .put_cf(
                    &handle,
                    key("BTCUSDT", row.open_ts_us).unwrap(),
                    row.encode(),
                )
                .unwrap();
        }
        let opens = [start, start + 2 * MINUTE_US, start, start + MINUTE_US];
        let rows = store.read_minutes("BTCUSDT", &opens).unwrap();
        assert_eq!(rows[0], rows[2]);
        assert!(rows[3].is_none());
        assert_eq!(store.status().disk_batch_reads, 1);
        store.read_minutes("BTCUSDT", &opens[..3]).unwrap();
        assert_eq!(store.status().disk_batch_reads, 1);
        assert!(store.window_prices("BTCUSDT", start).unwrap().is_none());
        let repaired = candle(start + MINUTE_US);
        store
            .save_page("BTCUSDT", start, start + 5 * MINUTE_US, &[repaired])
            .unwrap();
        let first = store.window_prices("BTCUSDT", start).unwrap().unwrap();
        assert_eq!(store.window_prices("BTCUSDT", start).unwrap(), Some(first));
        assert_eq!(store.status().pricing_cache_hits, 1);
        let mut corrected = candle(start);
        corrected.base_volume = 0.0;
        corrected.quote_volume = 0.0;
        store
            .save_page("BTCUSDT", start, start + 5 * MINUTE_US, &[corrected])
            .unwrap();
        let changed = store.window_prices("BTCUSDT", start).unwrap().unwrap();
        assert_eq!(changed.zero_volume_skipped_sample_count, 1);
        assert_ne!(changed, first);
    }

    #[test]
    fn dense_ranges_scan_once_and_later_queries_reuse_memory() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let store = KlineStore::from_db(db.clone(), Default::default()).unwrap();
        let start = first_complete_open(now_us() - DAY_US);
        let handle = db.db().cf_handle(CANDLES_CF).unwrap();
        for index in 0..120 {
            let row = candle(start + index * MINUTE_US);
            db.db()
                .put_cf(
                    &handle,
                    key("BTCUSDT", row.open_ts_us).unwrap(),
                    row.encode(),
                )
                .unwrap();
        }
        assert_eq!(
            store
                .scan("BTCUSDT", start, start + 120 * MINUTE_US)
                .unwrap()
                .len(),
            120
        );
        assert_eq!(store.status().disk_range_reads, 1);
        assert_eq!(
            store
                .scan("BTCUSDT", start + MINUTE_US, start + 119 * MINUTE_US)
                .unwrap()
                .len(),
            118
        );
        assert_eq!(store.status().disk_range_reads, 1);
        assert_eq!(store.status().disk_batch_reads, 0);
    }
    #[test]
    fn vwap_uses_quote_over_base_not_ohlc_average_and_zero_volume_has_no_price() {
        let mut row = candle(first_complete_open(now_us() - DAY_US));
        assert_eq!(row.vwap(), Some(101.0));
        assert_eq!(Kline::decode(row.open_ts_us, &row.encode()).unwrap(), row);
        row.base_volume = 0.0;
        row.quote_volume = 0.0;
        assert_eq!(row.vwap(), None);
        row.quote_volume = 1.0;
        assert_eq!(row.vwap(), None);
    }
    #[test]
    fn signal_schedule_excludes_the_partial_arrival_minute() {
        assert_eq!(first_complete_open(120 * MINUTE_US), 120 * MINUTE_US);
        assert_eq!(first_complete_open(120 * MINUTE_US + 1), 121 * MINUTE_US);
        assert_eq!(first_complete_open(121 * MINUTE_US - 1), 121 * MINUTE_US);
    }
    #[test]
    fn rejects_trading_binding_and_public_ip_conflicts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("trade_engine.toml");
        std::fs::write(
            &path,
            "local_ips = [\"10.1.1.1\"]\nbinance_um_whitelist_ip = \"10.1.1.2\"",
        )
        .unwrap();
        let mut config = KlineConfig {
            local_ip: Some("10.1.1.2".parse().unwrap()),
            public_ip: Some("198.51.100.4".parse().unwrap()),
            trade_engine_configs: vec![path],
            forbidden_public_ips: vec!["198.51.100.99".parse().unwrap()],
            ..Default::default()
        };
        assert!(check_trading_ips(&config).is_err());
        config.local_ip = Some("10.1.1.3".parse().unwrap());
        assert!(check_trading_ips(&config).is_ok());
        config.forbidden_public_ips.push(config.public_ip.unwrap());
        assert!(check_trading_ips(&config).is_err());
    }
    #[test]
    fn unbound_trading_ips_exclude_the_resolved_route_and_require_public_exclusions() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("trade_engine.toml");
        std::fs::write(&path, "local_ips = [\"0.0.0.0\", \"0.0.0.0\"]").unwrap();
        let mut config = KlineConfig {
            local_ip: Some("10.1.1.2".parse().unwrap()),
            public_ip: Some("198.51.100.4".parse().unwrap()),
            trade_engine_configs: vec![path],
            forbidden_public_ips: vec!["198.51.100.99".parse().unwrap()],
            ..Default::default()
        };
        let route = |_: IpAddr| Ok("10.1.1.1".parse::<IpAddr>().unwrap());
        assert!(check_trading_ips_with_route(&config, route).is_ok());
        config.local_ip = Some("10.1.1.1".parse().unwrap());
        assert!(check_trading_ips_with_route(&config, route).is_err());
        config.local_ip = Some("10.1.1.2".parse().unwrap());
        config.forbidden_public_ips.clear();
        assert!(check_trading_ips_with_route(&config, route).is_err());
        config.forbidden_public_ips.push(config.public_ip.unwrap());
        assert!(check_trading_ips_with_route(&config, route).is_err());
        assert!(check_trading_ips_with_route(&config, |_| bail!("no route")).is_err());
    }
    #[tokio::test]
    async fn cached_minutes_do_not_wait_for_a_different_range_backfill() {
        let (_dir, store, state, server) = fixture(None).await;
        let end = now_us().div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
        let start = end - MINUTE_US;
        store
            .save_page("ETHUSDT", start, end, &[candle(start)])
            .unwrap();
        let lock = store
            .inner
            .markets
            .lock()
            .unwrap()
            .entry("ETHUSDT".into())
            .or_default()
            .clone();
        let _backfill = lock.lock().await;
        tokio::time::timeout(
            Duration::from_millis(100),
            store.ensure_range("ETHUSDT", start, end),
        )
        .await
        .expect("cached range blocked behind an unrelated backfill")
        .unwrap();
        assert!(state.calls.lock().unwrap().is_empty());
        assert_eq!(store.status().cache_hits, 1);
        server.abort();
    }
    #[tokio::test]
    async fn unicode_symbol_backfill_is_24h_and_concurrent_queries_or_restart_do_not_refetch() {
        let (dir, store, state, server) = fixture(None).await;
        let end = now_us().div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
        let start = end - 10 * MINUTE_US;
        let symbol = "龙虾USDT";
        let (a, b, c) = tokio::join!(
            store.ensure_range(symbol, start, end),
            store.ensure_range(symbol, start, end),
            store.ensure_range(symbol, start, end)
        );
        a.unwrap();
        b.unwrap();
        c.unwrap();
        assert_eq!(state.calls.lock().unwrap().len(), 3);
        assert!(
            state
                .symbols
                .lock()
                .unwrap()
                .iter()
                .all(|value| value == symbol)
        );
        assert_eq!(store.scan(symbol, end - DAY_US, end).unwrap().len(), 1440);
        let handle = store.inner.db.db().cf_handle(CANDLES_CF).unwrap();
        store
            .inner
            .db
            .db()
            .delete_cf(&handle, key(symbol, start).unwrap())
            .unwrap();
        // Simulate a durable gap after the decoded symbol cache is evicted.
        // Production writes/prunes keep both layers coherent; direct DB edits do not.
        store.inner.minutes.lock().unwrap().remove(symbol);
        store.ensure_range(symbol, start, end).await.unwrap();
        assert_eq!(
            *state.calls.lock().unwrap().last().unwrap(),
            (start, start + MINUTE_US)
        );
        assert_eq!(state.calls.lock().unwrap().len(), 4);
        // Reopen the durable cache with networking disabled: all prices survive.
        drop(handle);
        drop(store);
        let config = KlineConfig::default();
        let reopened =
            KlineStore::from_db(ManagerDb::open(&dir.path().join("db")).unwrap(), config).unwrap();
        assert_eq!(reopened.scan(symbol, start, end).unwrap().len(), 10);
        assert_eq!(state.calls.lock().unwrap().len(), 4);
        server.abort();
    }
    #[tokio::test]
    async fn empty_minutes_stay_missing_and_later_queries_only_retry_the_gap() {
        let end = now_us().div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
        let missing = end - 2 * MINUTE_US;
        let (_dir, store, state, server) = fixture(Some(missing)).await;
        store
            .ensure_range("ETHUSDT", end - 5 * MINUTE_US, end)
            .await
            .unwrap();
        assert!(store.get("ETHUSDT", missing).unwrap().is_none());
        let before = state.calls.lock().unwrap().len();
        store
            .ensure_range("ETHUSDT", end - 5 * MINUTE_US, end)
            .await
            .unwrap();
        assert_eq!(state.calls.lock().unwrap().len(), before + 1);
        assert_eq!(
            *state.calls.lock().unwrap().last().unwrap(),
            (missing, missing + MINUTE_US)
        );
        assert!(store.get("ETHUSDT", missing).unwrap().is_none());
        assert_eq!(
            store
                .scan("ETHUSDT", end - 5 * MINUTE_US, end)
                .unwrap()
                .len(),
            4
        );
        server.abort();
    }
    #[test]
    fn retention_only_deletes_kline_data_and_rejects_old_or_future_queries() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let store = KlineStore::from_db(db.clone(), KlineConfig::default()).unwrap();
        let now = now_us();
        let recent = first_complete_open(now - DAY_US);
        let expired = first_complete_open(now - 31 * DAY_US);
        let handle = db.db().cf_handle(CANDLES_CF).unwrap();
        for ts in [recent, expired] {
            db.db()
                .put_cf(&handle, key("BTCUSDT", ts).unwrap(), candle(ts).encode())
                .unwrap();
        }
        let archive = db
            .db()
            .cf_handle(crate::manager_db::POSITION_UPDATES_CF)
            .unwrap();
        db.db().put_cf(&archive, b"sentinel", b"unchanged").unwrap();
        store.prune().unwrap();
        assert_eq!(
            store.get("BTCUSDT", recent).unwrap().unwrap().vwap(),
            Some(101.0)
        );
        assert!(
            db.db()
                .get_cf(&handle, key("BTCUSDT", expired).unwrap())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.db().get_cf(&archive, b"sentinel").unwrap().unwrap(),
            b"unchanged"
        );
        assert!(store.validate_range(now - 31 * DAY_US, now, now).is_err());
        assert!(store.validate_range(now - DAY_US, now + 1, now).is_err());
        assert!(
            KlineConfig {
                retain_days: 31,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    #[ignore = "offline storage capacity measurement; 144000 synthetic candles"]
    fn measure_100_symbols_one_day_storage() {
        let dir = TempDir::new().unwrap();
        let db = ManagerDb::open(dir.path()).unwrap();
        let store = KlineStore::from_db(db.clone(), KlineConfig::default()).unwrap();
        let handle = db.db().cf_handle(CANDLES_CF).unwrap();
        let start = now_us().div_euclid(MINUTE_US) * MINUTE_US - DAY_US - MINUTE_US;
        let mut rng = 123456789u64;
        let mut batch = WriteBatch::default();
        let mut raw = 0;
        for symbol_id in 0..100 {
            let symbol = format!("TEST{symbol_id:03}USDT");
            let mut price = (symbol_id as f64 + 1.0) * 13.37;
            for index in 0..1440 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let random = (rng as f64) / (u64::MAX as f64);
                let close = price * (0.999 + random * 0.002);
                let volume = 10.0 + random * 12345.0;
                let row = Kline {
                    open_ts_us: start + index * MINUTE_US,
                    open: price,
                    close,
                    high: price.max(close) * 1.0001,
                    low: price.min(close) * 0.9999,
                    base_volume: volume,
                    quote_volume: volume * (price + close) * 0.5,
                    trades: (random * 20000.0) as u64,
                };
                let key = key(&symbol, row.open_ts_us).unwrap();
                raw += key.len() + VALUE_BYTES;
                batch.put_cf(&handle, key, row.encode());
                price = close;
            }
        }
        db.db().write(batch).unwrap();
        db.db().flush_cf(&handle).unwrap();
        db.db()
            .compact_range_cf(&handle, None::<&[u8]>, None::<&[u8]>);
        let bytes = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sst"))
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum::<u64>();
        assert!(bytes > 0);
        println!(
            "100 symbols x 1440 candles: raw={} bytes, compacted SST={} bytes",
            raw, bytes
        );
        drop(store);
    }
}
