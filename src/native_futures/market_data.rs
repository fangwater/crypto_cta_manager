//! Native market-replay client. Finality comes from the provider's watermark,
//! never from the Manager's clock alone. This module has no execution SDK.
use super::{
    config::resolved,
    cost,
    reference::{self, Bar, MINUTE_US, Pack},
};
use anyhow::{Context, Result, ensure};
use cn_market_proto::{
    Exchange, GetMinuteBarsRequest, GetMinuteBarsResponse,
    cn_futures_market_client::CnFuturesMarketClient, get_minute_bars_request::Range,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::Semaphore;
use tonic::transport::{Channel, Endpoint};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub grpc_url: String,
    pub cache_root: PathBuf,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.grpc_url).context("invalid native market gRPC URL")?;
        ensure!(
            url.scheme() == "http"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "market_data.grpc_url requires an HTTP origin without credentials, path or query"
        );
        resolved(&self.cache_root)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDay {
    pub venue: String,
    /// Original Exec identity; canonicalization is only used for API validation.
    pub symbol: String,
    pub trading_day: String,
    pub contract_multiplier: f64,
    pub price_tick: f64,
    pub price_tick_source: String,
    pub session_snapshot_as_of: String,
    pub live_through_ms: i64,
    pub archived: bool,
    pub pending_bar_count: usize,
    pub unpriced_empty_minutes: usize,
    /// Only provider-final, locally closed minutes. Missing bars stay missing.
    pub bars: Vec<Bar>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    feed_sha256: String,
    day: MarketDay,
}

type MarketLocks = BTreeMap<String, Weak<tokio::sync::Mutex<()>>>;
pub struct Feed {
    settings: Settings,
    channel: Channel,
    permits: Semaphore,
    admission: Semaphore,
    locks: Mutex<MarketLocks>,
}
impl Feed {
    pub fn new(settings: Settings) -> Result<Self> {
        settings.validate()?;
        // Tonic connects directly to the configured endpoint; no environment
        // HTTP proxy, venue REST API or default data source is consulted.
        let channel = Endpoint::from_shared(settings.grpc_url.clone())?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .connect_lazy();
        Ok(Self {
            settings,
            channel,
            permits: Semaphore::new(4),
            admission: Semaphore::new(64),
            locks: Mutex::default(),
        })
    }

    pub async fn day(
        &self,
        venue: &str,
        symbol: &str,
        day: &str,
        now_us: i64,
    ) -> Result<MarketDay> {
        self.load(venue, symbol, day, now_us, None).await
    }

    pub async fn augment(&self, pack: &mut Pack, now_us: i64) -> Result<()> {
        let needed = cost::required_minutes(pack, now_us)?;
        let existing = pack
            .bars
            .iter()
            .map(|b| {
                Ok((
                    b.venue.clone(),
                    b.symbol.clone(),
                    reference::timestamp(&b.open_time)?,
                ))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let mut markets: BTreeMap<(String, String), BTreeSet<i64>> = BTreeMap::new();
        for (venue, symbol, time) in needed.difference(&existing) {
            markets
                .entry((venue.clone(), symbol.clone()))
                .or_default()
                .insert(*time);
        }
        let mut first_error = None;
        for ((venue, symbol), times) in markets {
            let result = async {
                let day = self
                    .load(&venue, &symbol, &pack.trading_day, now_us, Some(&times))
                    .await?;
                let contract = pack
                    .contracts
                    .iter()
                    .find(|c| c.venue == venue && c.symbol == symbol)
                    .context("reference pack is missing the native market contract")?;
                ensure!(
                    contract.contract_multiplier == day.contract_multiplier
                        && contract.price_tick == day.price_tick,
                    "market-replay multiplier/tick conflicts with the maintained reference pack"
                );
                let bars: Vec<_> = day
                    .bars
                    .into_iter()
                    .filter(|b| {
                        reference::timestamp(&b.open_time).is_ok_and(|t| times.contains(&t))
                    })
                    .collect();
                let mut candidate = pack.clone();
                candidate.bars.extend(bars.clone());
                candidate.validate()?;
                pack.bars.extend(bars);
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn load(
        &self,
        venue: &str,
        symbol: &str,
        day: &str,
        now_us: i64,
        required: Option<&BTreeSet<i64>>,
    ) -> Result<MarketDay> {
        super::validate_market(&format!("{venue}:{symbol}"))?;
        reference::valid_day(day)?;
        let _admission = self
            .admission
            .try_acquire()
            .context("native market query capacity exhausted")?;
        let key = format!("{venue}:{symbol}:{day}");
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .map_err(|_| anyhow::anyhow!("native market lock poisoned"))?;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        };
        let _guard = lock.lock().await;
        let root = resolved(&self.settings.cache_root)?;
        let path = root.join(venue).join(symbol).join(format!("{day}.json"));
        ensure!(
            resolved(&path)?.starts_with(&root),
            "native market cache escapes cache_root"
        );
        let p = path.clone();
        let fingerprint = hex::encode(Sha256::digest(self.settings.grpc_url.as_bytes()));
        let expected = fingerprint.clone();
        let cached = tokio::task::spawn_blocking(move || read_cache(&p, &expected)).await??;
        if let Some(cached) = cached {
            validate_cached(&cached, venue, symbol, day, now_us)?;
            let minutes = cached
                .bars
                .iter()
                .map(|b| reference::timestamp(&b.open_time))
                .collect::<Result<BTreeSet<_>>>()?;
            if required.is_none_or(|r| r.is_subset(&minutes)) {
                return Ok(cached);
            }
        }
        let _permit = self.permits.acquire().await?;
        let mut client = CnFuturesMarketClient::new(self.channel.clone())
            .max_decoding_message_size(8 * 1024 * 1024);
        let response = client
            .get_minute_bars(GetMinuteBarsRequest {
                exchange: exchange(venue)? as i32,
                contract: symbol.to_owned(),
                range: Some(Range::TradingDay(day.parse()?)),
                include_empty_minutes: true,
            })
            .await
            .context("query native market-replay minutes")?
            .into_inner();
        let market = convert(response, venue, symbol, day, now_us)?;
        // An in-progress day is queried again even if its latest returned bar
        // is closed locally. Missing/omitted minutes are never negative-cached.
        if market.archived && market.pending_bar_count == 0 && !market.bars.is_empty() {
            let cache = Cache {
                feed_sha256: fingerprint,
                day: market.clone(),
            };
            tokio::task::spawn_blocking(move || write_cache(&path, &cache)).await??;
        }
        Ok(market)
    }
}

fn exchange(venue: &str) -> Result<Exchange> {
    Ok(match venue {
        "SHFE" => Exchange::Shfe,
        "INE" => Exchange::Ine,
        "DCE" => Exchange::Dce,
        "CZCE" => Exchange::Czce,
        "CFFEX" => Exchange::Cffex,
        "GFEX" => Exchange::Gfex,
        _ => anyhow::bail!("unsupported native venue"),
    })
}

fn convert(
    response: GetMinuteBarsResponse,
    venue: &str,
    symbol: &str,
    day: &str,
    now_us: i64,
) -> Result<MarketDay> {
    let contract = response
        .contract
        .context("market-replay omitted contract metadata")?;
    let state = response
        .data_state
        .context("market-replay omitted data state")?;
    ensure!(
        contract.exchange == exchange(venue)? as i32
            && (symbol.eq_ignore_ascii_case(&contract.native_code)
                || symbol.eq_ignore_ascii_case(&contract.canonical_code)),
        "market-replay returned a different venue or contract"
    );
    ensure!(
        contract.volume_multiple.is_finite()
            && contract.volume_multiple > 0.0
            && contract.price_tick.is_finite()
            && contract.price_tick > 0.0,
        "native market requires a verified positive multiplier and tick"
    );
    ensure!(
        state.source == "baseline_data_1min" && !state.session_snapshot_as_of.is_empty(),
        "unrecognized native market provenance"
    );
    ensure!(response.bars.len() <= 1500, "oversized native trading day");
    let trading_day: u32 = day.parse()?;
    let archived =
        state.latest_trading_day >= trading_day && response.bars.iter().all(|b| !b.provisional);
    let mut result = MarketDay {
        venue: venue.into(),
        symbol: symbol.into(),
        trading_day: day.into(),
        contract_multiplier: contract.volume_multiple,
        price_tick: contract.price_tick,
        price_tick_source: contract.price_tick_source,
        session_snapshot_as_of: state.session_snapshot_as_of,
        live_through_ms: state.live_through_ms,
        archived,
        pending_bar_count: 0,
        unpriced_empty_minutes: 0,
        bars: vec![],
    };
    let mut previous = None;
    for b in response.bars {
        ensure!(
            b.trading_day == trading_day
                && b.open_time_ms >= 0
                && b.open_time_ms % 60_000 == 0
                && previous.is_none_or(|t| b.open_time_ms > t),
            "unordered, duplicate or wrong-day native minute"
        );
        previous = Some(b.open_time_ms);
        let end_ms = b
            .open_time_ms
            .checked_add(60_000)
            .context("native minute timestamp overflow")?;
        let provider_final = if b.provisional {
            state.live_trading_day == trading_day && end_ms <= state.live_through_ms
        } else {
            state.latest_trading_day >= trading_day
        };
        if !provider_final || end_ms > now_us / 1000 {
            result.pending_bar_count += 1;
            continue;
        }
        if b.volume_lots == 0.0
            && b.turnover_cny == 0.0
            && [b.open, b.high, b.low, b.close]
                .iter()
                .any(|p| !p.is_finite())
        {
            result.unpriced_empty_minutes += 1;
            continue;
        }
        let open_time = chrono::DateTime::from_timestamp_millis(b.open_time_ms)
            .context("invalid native minute timestamp")?
            .to_rfc3339();
        let bar = Bar {
            venue: venue.into(),
            symbol: symbol.into(),
            open_time,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume_lots: b.volume_lots,
            turnover: b.turnover_cny,
        };
        validate_bar(&bar, result.contract_multiplier, result.price_tick)?;
        if bar.volume_lots > 0.0 {
            let vwap = bar.turnover / bar.volume_lots / result.contract_multiplier;
            ensure!(
                b.vwap.is_finite() && (b.vwap - vwap).abs() <= vwap.abs().max(1.0) * 1e-8,
                "native VWAP conflicts with lots, turnover and multiplier"
            );
        }
        result.bars.push(bar);
    }
    Ok(result)
}
fn validate_bar(b: &Bar, multiplier: f64, tick: f64) -> Result<()> {
    ensure!(
        [b.open, b.high, b.low, b.close]
            .iter()
            .all(|p| p.is_finite() && *p > 0.0)
            && b.low <= b.open
            && b.low <= b.close
            && b.high >= b.open
            && b.high >= b.close,
        "invalid native candle OHLC"
    );
    ensure!(
        b.volume_lots.is_finite()
            && b.volume_lots >= 0.0
            && b.turnover.is_finite()
            && b.turnover >= 0.0
            && (b.volume_lots == 0.0) == (b.turnover == 0.0),
        "invalid native lots/turnover units"
    );
    ensure!(
        [b.open, b.high, b.low, b.close].iter().all(|p| {
            let n = p / tick;
            (n - n.round()).abs() <= n.abs().max(1.0) * 1e-9
        }),
        "native candle price outside tick grid"
    );
    if b.volume_lots > 0.0 {
        let p = b.turnover / b.volume_lots / multiplier;
        let tolerance = b.high.abs().max(1.0) * 1e-8;
        ensure!(
            p.is_finite() && p >= b.low - tolerance && p <= b.high + tolerance,
            "native VWAP outside OHLC"
        );
    }
    Ok(())
}
fn validate_cached(
    cached: &MarketDay,
    venue: &str,
    symbol: &str,
    day: &str,
    now_us: i64,
) -> Result<()> {
    ensure!(
        cached.venue == venue
            && cached.symbol == symbol
            && cached.trading_day == day
            && cached.archived
            && cached.pending_bar_count == 0
            && cached.bars.len() <= 1500
            && cached.contract_multiplier.is_finite()
            && cached.contract_multiplier > 0.0
            && cached.price_tick.is_finite()
            && cached.price_tick > 0.0,
        "invalid native market cache identity or metadata"
    );
    let mut previous = None;
    for bar in &cached.bars {
        ensure!(
            bar.venue == venue && bar.symbol == symbol,
            "cached bar changed native identity"
        );
        let t = reference::timestamp(&bar.open_time)?;
        ensure!(
            t % MINUTE_US == 0 && t + MINUTE_US <= now_us && previous.is_none_or(|old| t > old),
            "invalid cached minute time"
        );
        previous = Some(t);
        validate_bar(bar, cached.contract_multiplier, cached.price_tick)?;
    }
    Ok(())
}
fn read_cache(path: &Path, fingerprint: &str) -> Result<Option<MarketDay>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut bytes = vec![];
    fs::File::open(path)?
        .take(reference::MAX_PACK_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= reference::MAX_PACK_BYTES,
        "oversized native market cache"
    );
    let cached: Cache = serde_json::from_slice(&bytes).context("invalid native market cache")?;
    ensure!(
        cached.feed_sha256 == fingerprint,
        "native market cache belongs to another provider"
    );
    Ok(Some(cached.day))
}
fn write_cache(path: &Path, cache: &Cache) -> Result<()> {
    let parent = path.parent().context("native cache parent")?;
    fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "native market cache write already in progress"
    );
    let temporary = path.with_extension(format!(
        "pending-{}-{}",
        std::process::id(),
        super::now_us()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(cache)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests;
