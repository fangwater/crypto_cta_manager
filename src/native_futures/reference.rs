//! Operator-supplied analysis inputs. These are reference evidence, never fills
//! or execution instructions. One self-contained pack belongs to one source/day.
use super::config::{Config, resolved};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MINUTE_US: i64 = 60_000_000;
pub const MAX_PACK_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub source_id: String,
    pub trading_day: String,
    pub provenance: String,
    #[serde(default)]
    pub contracts: Vec<Contract>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub bars: Vec<Bar>,
    #[serde(default)]
    pub signals: Vec<Signal>,
    #[serde(default)]
    pub order_links: Vec<OrderLink>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub venue: String,
    pub symbol: String,
    pub contract_multiplier: f64,
    pub price_tick: f64,
    #[serde(default)]
    pub fees: BTreeMap<String, Fee>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Fee {
    pub per_lot: f64,
    pub notional_rate: f64,
}
impl Fee {
    pub fn amount(&self, lots: i64, price: f64, multiplier: f64) -> f64 {
        lots as f64 * (self.per_lot + price * multiplier * self.notional_rate)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub venue: String,
    pub symbol: String,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bar {
    pub venue: String,
    pub symbol: String,
    pub open_time: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Single-sided, incremental volume in lots, never cumulative tick volume.
    pub volume_lots: f64,
    /// Single-sided incremental CNY turnover, including the contract multiplier.
    pub turnover: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    Twap,
    Vwap,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub id: String,
    pub venue: String,
    pub symbol: String,
    /// Actual externally recorded reference time, with an explicit UTC offset.
    pub received_at: String,
    pub side: String,
    pub offset: String,
    pub lots: i64,
    pub model: Model,
    pub window_minutes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OrderLink {
    pub venue: String,
    pub session: String,
    pub order_ref: String,
    pub signal_id: String,
}

pub fn timestamp(value: &str) -> Result<i64> {
    let t = DateTime::parse_from_rfc3339(value)
        .context("timestamp requires RFC3339 with UTC offset")?;
    ensure!(
        t.timestamp_subsec_nanos() % 1000 == 0,
        "timestamp precision exceeds microseconds"
    );
    let us = t.timestamp_micros();
    ensure!(
        (0..=9_000_000_000_000_000).contains(&us),
        "timestamp outside supported range"
    );
    Ok(us)
}
pub fn valid_day(day: &str) -> Result<()> {
    ensure!(
        day.len() == 8 && day.bytes().all(|b| b.is_ascii_digit()),
        "trading_day must be YYYYMMDD"
    );
    NaiveDate::parse_from_str(day, "%Y%m%d").context("invalid trading_day")?;
    Ok(())
}
fn market(venue: &str, symbol: &str) -> Result<()> {
    super::validate_market(&format!("{venue}:{symbol}"))
}
fn offset(value: &str) -> bool {
    matches!(value, "open" | "close" | "close_today" | "close_yesterday")
}

impl Pack {
    pub fn load(path: &Path) -> Result<Self> {
        let file = fs::File::open(path).context("open reference pack")?;
        let mut bytes = Vec::new();
        file.take(MAX_PACK_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_PACK_BYTES,
            "reference pack exceeds 64 MiB"
        );
        let pack: Self = serde_json::from_slice(&bytes).context("decode reference pack")?;
        pack.validate()?;
        Ok(pack)
    }

    pub fn validate(&self) -> Result<()> {
        valid_day(&self.trading_day)?;
        ensure!(
            !self.source_id.is_empty()
                && self
                    .source_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "invalid reference source_id"
        );
        ensure!(
            !self.provenance.trim().is_empty(),
            "reference provenance is required"
        );
        let mut contracts = BTreeMap::new();
        for c in &self.contracts {
            market(&c.venue, &c.symbol)?;
            ensure!(
                c.contract_multiplier.is_finite()
                    && c.contract_multiplier > 0.0
                    && c.price_tick.is_finite()
                    && c.price_tick > 0.0,
                "invalid contract multiplier/tick"
            );
            ensure!(
                contracts.insert((&c.venue, &c.symbol), c).is_none(),
                "duplicate contract"
            );
            for (key, fee) in &c.fees {
                ensure!(
                    offset(key) && fee.per_lot.is_finite() && fee.notional_rate.is_finite(),
                    "invalid offset fee"
                );
            }
        }
        let mut sessions: BTreeMap<(&str, &str), Vec<(i64, i64)>> = BTreeMap::new();
        for s in &self.sessions {
            market(&s.venue, &s.symbol)?;
            let (start, end) = (timestamp(&s.start)?, timestamp(&s.end)?);
            ensure!(
                start < end
                    && end - start <= 24 * 60 * MINUTE_US
                    && start % MINUTE_US == 0
                    && end % MINUTE_US == 0,
                "sessions require ordered whole-minute boundaries within 24 hours"
            );
            sessions
                .entry((&s.venue, &s.symbol))
                .or_default()
                .push((start, end));
        }
        for windows in sessions.values_mut() {
            windows.sort_unstable();
            ensure!(
                windows.windows(2).all(|w| w[0].1 <= w[1].0),
                "overlapping trading sessions"
            );
            ensure!(
                windows.last().unwrap().1 - windows[0].0 <= 7 * 24 * 60 * MINUTE_US,
                "trading-day sessions span more than seven calendar days"
            );
        }
        let mut bars = BTreeSet::new();
        for b in &self.bars {
            market(&b.venue, &b.symbol)?;
            let open = timestamp(&b.open_time)?;
            ensure!(
                open % MINUTE_US == 0 && bars.insert((&b.venue, &b.symbol, open)),
                "unaligned or duplicate minute candle"
            );
            ensure!(
                [b.open, b.high, b.low, b.close]
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0)
                    && b.low <= b.open
                    && b.low <= b.close
                    && b.high >= b.open
                    && b.high >= b.close,
                "invalid candle OHLC"
            );
            ensure!(
                b.volume_lots.is_finite()
                    && b.volume_lots >= 0.0
                    && b.turnover.is_finite()
                    && b.turnover >= 0.0
                    && (b.volume_lots == 0.0) == (b.turnover == 0.0),
                "volume/turnover must be consistent nonnegative minute increments"
            );
            ensure!(
                sessions
                    .get(&(b.venue.as_str(), b.symbol.as_str()))
                    .is_some_and(|w| w.iter().any(|(s, e)| open >= *s && open + MINUTE_US <= *e)),
                "candle outside declared trading sessions"
            );
            if let Some(c) = contracts.get(&(&b.venue, &b.symbol)) {
                if b.volume_lots > 0.0 {
                    let p = b.turnover / b.volume_lots / c.contract_multiplier;
                    let tolerance = b.high.abs().max(1.0) * 1e-8;
                    ensure!(
                        p.is_finite() && p >= b.low - tolerance && p <= b.high + tolerance,
                        "minute VWAP outside OHLC: check single-sided volume, turnover and multiplier"
                    );
                }
                ensure!(
                    [b.open, b.high, b.low, b.close].iter().all(|p| {
                        let ticks = p / c.price_tick;
                        (ticks - ticks.round()).abs() <= ticks.abs().max(1.0) * 1e-9
                    }),
                    "OHLC does not respect price_tick"
                );
            }
        }
        let mut signals = BTreeMap::new();
        for s in &self.signals {
            market(&s.venue, &s.symbol)?;
            timestamp(&s.received_at)?;
            ensure!(
                !s.id.trim().is_empty()
                    && s.id.len() <= 128
                    && signals.insert(s.id.as_str(), s).is_none(),
                "invalid or duplicate signal id"
            );
            ensure!(
                matches!(s.side.as_str(), "buy" | "sell")
                    && offset(&s.offset)
                    && (1..=i32::MAX as i64).contains(&s.lots)
                    && (1..=60).contains(&s.window_minutes),
                "invalid signal side/offset/lots/window"
            );
        }
        let mut links = BTreeSet::new();
        for l in &self.order_links {
            ensure!(
                !l.session.is_empty()
                    && !l.order_ref.is_empty()
                    && links.insert((&l.venue, &l.session, &l.order_ref)),
                "missing or duplicate complete order reference"
            );
            let s = signals
                .get(l.signal_id.as_str())
                .context("order reference names an absent signal")?;
            ensure!(
                l.venue == s.venue,
                "order reference venue differs from signal"
            );
        }
        Ok(())
    }
}

/// Resolve descendants as well as the root; a source/day symlink must never
/// redirect reads, backups or atomic replacement into an execution database.
pub fn pack_path(config: &Config, source: &str, day: &str) -> Result<PathBuf> {
    config.validate()?;
    valid_day(day)?;
    ensure!(
        config.sources.iter().any(|s| s.enabled && s.id == source),
        "reference source is not enabled"
    );
    let root = resolved(
        config
            .reference_root
            .as_deref()
            .context("reference_root is not configured")?,
    )?;
    let path = root.join(source).join(format!("{day}.json"));
    ensure!(
        resolved(&path)?.starts_with(&root),
        "reference file escapes reference_root"
    );
    Ok(path)
}

pub fn load(config: &Config, source: &str, day: &str) -> Result<Option<Pack>> {
    let path = pack_path(config, source, day)?;
    if !path.exists() {
        return Ok(None);
    }
    let pack = Pack::load(&path)?;
    ensure!(
        pack.source_id == source && pack.trading_day == day,
        "reference pack identity differs from requested source/day"
    );
    Ok(Some(pack))
}

/// Install only Manager-owned input files. Existing revisions remain recoverable
/// by content hash; replacement requires an explicit maintenance flag.
pub fn install(config: &Config, pack: &Pack, replace: bool) -> Result<String> {
    pack.validate()?;
    let path = pack_path(config, &pack.source_id, &pack.trading_day)?;
    let parent = path.parent().context("reference parent")?;
    fs::create_dir_all(parent)?;
    let lock_path = path.with_extension("lock");
    let mut lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("reference import locked; another import or a stale .lock file exists")?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _lock_cleanup = Cleanup(lock_path);
    writeln!(lock, "{}", std::process::id())?;
    let bytes = serde_json::to_vec_pretty(pack)?;
    let hash = hex::encode(Sha256::digest(&bytes));
    let current = if path.exists() {
        Some(fs::read(&path)?)
    } else {
        None
    };
    if let Some(old) = &current {
        if *old == bytes {
            return Ok(hash);
        }
        ensure!(
            replace,
            "reference pack exists with different content; review and use --replace"
        );
    }
    let root = resolved(config.reference_root.as_deref().context("reference_root")?)?;
    let archive = parent.join("history").join(&pack.trading_day);
    ensure!(
        resolved(&archive)?.starts_with(&root),
        "reference history escapes reference_root"
    );
    fs::create_dir_all(&archive)?;
    for content in current.iter().chain(std::iter::once(&bytes)) {
        let revision = archive.join(format!("{}.json", hex::encode(Sha256::digest(content))));
        ensure!(
            resolved(&revision)?.starts_with(&root),
            "reference revision escapes reference_root"
        );
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&revision)
        {
            Ok(mut file) => {
                file.write_all(content)?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
                fs::read(&revision)? == *content,
                "reference history hash conflict"
            ),
            Err(e) => return Err(e.into()),
        }
    }
    fs::File::open(&archive)?.sync_all()?;
    let temporary = path.with_extension("pending");
    // create_new also protects a previous interrupted import's evidence.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let _temporary_cleanup = Cleanup(temporary.clone());
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(hash)
}
