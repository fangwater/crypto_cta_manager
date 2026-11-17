//! Operator-hosted Binance-shaped minute API. Volume is explicitly domestic
//! single-sided lots and turnover is CNY. No venue API or trading SDK fallback.
use super::{
    config::resolved,
    cost,
    reference::{self, Bar, MINUTE_US, Pack},
};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub api_url: String,
    pub cache_root: PathBuf,
    /// Explicit local venue:contract -> HTTP symbol mapping, preserving case.
    pub symbols: BTreeMap<String, String>,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        let url = Url::parse(&self.api_url).context("invalid domestic Kline API URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Kline URL requires HTTP(S), no embedded credentials/query/fragment"
        );
        resolved(&self.cache_root)?;
        ensure!(
            !self.symbols.is_empty(),
            "market_data.symbols must explicitly map domestic contracts"
        );
        let mut unique = BTreeSet::new();
        for (market, symbol) in &self.symbols {
            super::validate_market(market)?;
            ensure!(
                !symbol.is_empty()
                    && symbol.len() <= 128
                    && !symbol.chars().any(char::is_whitespace)
                    && unique.insert(symbol),
                "HTTP symbols must be nonempty, unique and contain no whitespace"
            );
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    feed_sha256: String,
    bars: Vec<Bar>,
}

pub struct Feed {
    settings: Settings,
    client: Client,
    permits: Semaphore,
    locks: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}
impl Feed {
    pub fn new(settings: Settings) -> Result<Self> {
        settings.validate()?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            settings,
            client,
            permits: Semaphore::new(4),
            locks: Mutex::new(BTreeMap::new()),
        })
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
        // A failed contract must not prevent other independent contracts loading.
        for ((venue, symbol), times) in markets {
            if let Err(error) = self.load_market(pack, &venue, &symbol, &times).await {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    async fn load_market(
        &self,
        pack: &mut Pack,
        venue: &str,
        symbol: &str,
        times: &BTreeSet<i64>,
    ) -> Result<()> {
        let market = format!("{venue}:{symbol}");
        let http_symbol = self
            .settings
            .symbols
            .get(&market)
            .context("missing explicit HTTP symbol mapping")?;
        let key = format!("{market}:{}", pack.trading_day);
        let lock = self
            .locks
            .lock()
            .map_err(|_| anyhow::anyhow!("market lock poisoned"))?
            .entry(key)
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        let root = resolved(&self.settings.cache_root)?;
        let path = root
            .join(venue)
            .join(symbol)
            .join(format!("{}.json", pack.trading_day));
        ensure!(
            resolved(&path)?.starts_with(&root),
            "market cache file escapes cache_root"
        );
        let fingerprint = hex::encode(Sha256::digest(format!(
            "{}\n{http_symbol}\nvolume_lots\nturnover_cny",
            self.settings.api_url
        )));
        let p = path.clone();
        let expected = fingerprint.clone();
        let mut cache = tokio::task::spawn_blocking(move || read_cache(&p, &expected)).await??;
        validate_bars(pack, venue, symbol, &cache.bars)?;
        let mut known = cache
            .bars
            .iter()
            .map(|b| Ok((reference::timestamp(&b.open_time)?, b.clone())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let missing: Vec<_> = times
            .iter()
            .filter(|t| !known.contains_key(t))
            .copied()
            .collect();
        // Overlay cached requested minutes before any HTTP error can occur.
        for t in times {
            if let Some(b) = known.get(t) {
                pack.bars.push(b.clone());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let p = path.clone();
        let _file_lock = tokio::task::spawn_blocking(move || CacheLock::acquire(&p)).await??;
        // Another process may have completed the same request before the lock.
        let p = path.clone();
        let expected = fingerprint.clone();
        let latest = tokio::task::spawn_blocking(move || read_cache(&p, &expected)).await??;
        validate_bars(pack, venue, symbol, &latest.bars)?;
        for b in latest.bars {
            let t = reference::timestamp(&b.open_time)?;
            if !known.contains_key(&t) && times.contains(&t) {
                pack.bars.push(b.clone());
            }
            known.insert(t, b);
        }
        let missing: Vec<_> = times
            .iter()
            .filter(|t| !known.contains_key(t))
            .copied()
            .collect();
        let mut begin = 0;
        while begin < missing.len() {
            let mut end = begin + 1;
            while end < missing.len()
                && end - begin < 1000
                && missing[end] == missing[end - 1] + MINUTE_US
            {
                end += 1;
            }
            let start = missing[begin];
            let last = missing[end - 1];
            let bars = self.fetch(venue, symbol, http_symbol, start, last).await?;
            validate_bars(pack, venue, symbol, &bars)?;
            for b in bars {
                let t = reference::timestamp(&b.open_time)?;
                if times.contains(&t) {
                    pack.bars.push(b.clone());
                }
                known.insert(t, b);
            }
            cache.bars = known.values().cloned().collect();
            let bytes = serde_json::to_vec(&cache)?;
            let p = path.clone();
            tokio::task::spawn_blocking(move || write_cache(&p, &bytes)).await??;
            begin = end;
        }
        Ok(())
    }

    async fn fetch(
        &self,
        venue: &str,
        symbol: &str,
        http_symbol: &str,
        start: i64,
        last: i64,
    ) -> Result<Vec<Bar>> {
        let _permit = self.permits.acquire().await?;
        let mut response = self
            .client
            .get(&self.settings.api_url)
            .query(&[
                ("symbol", http_symbol.to_owned()),
                ("interval", "1m".into()),
                ("startTime", (start / 1000).to_string()),
                ("endTime", ((last + MINUTE_US) / 1000 - 1).to_string()),
                ("limit", "1000".into()),
            ])
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                "Kline HTTP page exceeds 8 MiB"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).context("invalid Kline JSON")?;
        let rows = value
            .as_array()
            .context("Kline response must be a Binance-shaped array")?;
        ensure!(rows.len() <= 1000, "Kline page exceeds requested limit");
        let mut seen = BTreeSet::new();
        rows.iter()
            .map(|row| {
                let bar = parse_bar(row, venue, symbol)?;
                let t = reference::timestamp(&bar.open_time)?;
                ensure!(
                    t >= start && t <= last && seen.insert(t),
                    "Kline page contains duplicate or out-of-range minutes"
                );
                Ok(bar)
            })
            .collect()
    }
}

fn parse_bar(raw: &Value, venue: &str, symbol: &str) -> Result<Bar> {
    let row = raw.as_array().context("Kline row must be an array")?;
    ensure!(row.len() >= 8, "short Kline row");
    let open_ms = row[0]
        .as_i64()
        .context("missing integer Kline open timestamp")?;
    let close_ms = row[6]
        .as_i64()
        .context("missing integer Kline close timestamp")?;
    ensure!(
        (0..=9_000_000_000_000_i64 - 60_000).contains(&open_ms)
            && open_ms % 60_000 == 0
            && close_ms == open_ms + 59_999,
        "Kline must span exactly one whole minute in Unix milliseconds"
    );
    let number = |i: usize| -> Result<f64> {
        let v = if let Some(s) = row[i].as_str() {
            s.parse::<f64>().context("invalid Kline decimal")?
        } else {
            row[i].as_f64().context("invalid Kline number")?
        };
        ensure!(v.is_finite(), "nonfinite Kline value");
        Ok(v)
    };
    let open_time = chrono::DateTime::from_timestamp_millis(open_ms)
        .context("invalid Kline date")?
        .to_rfc3339();
    Ok(Bar {
        venue: venue.into(),
        symbol: symbol.into(),
        open_time,
        open: number(1)?,
        high: number(2)?,
        low: number(3)?,
        close: number(4)?,
        volume_lots: number(5)?,
        turnover: number(7)?,
    })
}

fn validate_bars(pack: &Pack, venue: &str, symbol: &str, bars: &[Bar]) -> Result<()> {
    ensure!(
        bars.iter().all(|b| b.venue == venue && b.symbol == symbol),
        "cached Kline market identity mismatch"
    );
    Pack {
        source_id: pack.source_id.clone(),
        trading_day: pack.trading_day.clone(),
        provenance: pack.provenance.clone(),
        contracts: pack
            .contracts
            .iter()
            .filter(|c| c.venue == venue && c.symbol == symbol)
            .cloned()
            .collect(),
        sessions: pack
            .sessions
            .iter()
            .filter(|s| s.venue == venue && s.symbol == symbol)
            .cloned()
            .collect(),
        bars: bars.to_vec(),
        signals: vec![],
        order_links: vec![],
    }
    .validate()
}
fn read_cache(path: &Path, fingerprint: &str) -> Result<Cache> {
    if !path.exists() {
        return Ok(Cache {
            feed_sha256: fingerprint.into(),
            bars: vec![],
        });
    }
    let mut bytes = vec![];
    fs::File::open(path)?
        .take(reference::MAX_PACK_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= reference::MAX_PACK_BYTES,
        "oversized market cache file"
    );
    let cache: Cache = serde_json::from_slice(&bytes).context("invalid market cache")?;
    ensure!(
        cache.feed_sha256 == fingerprint,
        "market cache belongs to a different feed or HTTP symbol; select a separate cache_root"
    );
    Ok(cache)
}
fn write_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("cache parent")?;
    let temporary = path.with_extension(format!(
        "pending-{}-{}",
        std::process::id(),
        super::now_us()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
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
struct CacheLock(PathBuf);
impl CacheLock {
    fn acquire(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().context("cache parent")?)?;
        let lock = path.with_extension("lock");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .context("market cache update in progress or stale cache .lock exists")?;
        Ok(Self(lock))
    }
}
impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests;
