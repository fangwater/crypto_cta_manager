//! Disposable public USD-M funding-rate cache and estimated funding output.
//! No exchange credentials, database writes or Exec runtime dependencies.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::config::FundingRateConfig;
use crate::funding::FundingNavPoint;
use crate::nav::{NavTimelinePoint, NavTimelineReport};

pub const DAY_US: i64 = 86_400_000_000;
pub const RETENTION_US: i64 = 30 * DAY_US;
const OVERLAP_US: i64 = 3_600_000_000;
// WAF rejected even a one-request/second cold load on a separate jp-meta IP.
// Stay well below the documented shared 500 / 5 min / IP endpoint limit.
const BUDGET: usize = 90;
// The rolling quota alone permits a cold-cache burst large enough to hit WAF.
// Public funding is a slow historical feed, so spread reservations across time.
const REQUEST_INTERVAL: Duration = Duration::from_secs(3);
const WAF_COOLDOWN_SECS: u64 = 300;

pub fn supported_symbol(symbol: &str) -> bool {
    let Ok((venue, symbol)) = crate::market_data::split_cache_symbol(symbol) else {
        return false;
    };
    if !matches!(venue, "binance-futures" | "okex-futures") {
        return false;
    }
    (symbol.ends_with("USDT") || symbol.ends_with("USDC"))
        && symbol.len() > 4
        && symbol.len() <= 80
        && symbol.chars().all(char::is_alphanumeric)
        && symbol == symbol.to_uppercase()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FundingRate {
    pub venue_code: i16,
    pub symbol: String,
    pub ts_us: i64,
    pub rate: f64,
    pub mark_price: Option<f64>,
    pub rate_type: String,
}

impl FundingRate {
    /// Regular settlements use the scheduled hour, without the publication's
    /// millisecond drift or any inferred exchange/account posting delay.
    pub fn estimate_ts_us(&self) -> i64 {
        if self.rate_type == "Regular" {
            self.ts_us.div_euclid(3_600_000_000) * 3_600_000_000
        } else {
            self.ts_us
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateRow {
    symbol: String,
    funding_time: i64,
    funding_rate: String,
    mark_price: String,
    #[serde(default = "regular_rate")]
    rate_type: String,
}
fn regular_rate() -> String {
    "Regular".into()
}

fn parse_page(
    rows: Vec<RateRow>,
    symbol: &str,
    from_ms: i64,
    through_ms: i64,
) -> Result<Vec<FundingRate>> {
    ensure!(rows.len() <= 1000, "public funding page exceeds limit");
    let mut previous = None;
    let mut keys = BTreeSet::new();
    rows.into_iter()
        .map(|row| {
            ensure!(row.symbol == symbol, "public funding symbol mismatch");
            ensure!(
                (from_ms..=through_ms).contains(&row.funding_time),
                "public funding timestamp outside request"
            );
            ensure!(
                previous.is_none_or(|ts| ts <= row.funding_time),
                "public funding history is not ordered"
            );
            previous = Some(row.funding_time);
            ensure!(
                matches!(row.rate_type.as_str(), "Regular" | "Special"),
                "unknown public funding rate type"
            );
            ensure!(
                keys.insert((row.funding_time, row.rate_type.clone())),
                "duplicate public funding settlement"
            );
            let rate: f64 = row
                .funding_rate
                .parse()
                .context("invalid public funding rate")?;
            let mark_price: f64 = row
                .mark_price
                .parse()
                .context("invalid funding settlement mark")?;
            ensure!(
                rate.is_finite() && mark_price.is_finite() && mark_price > 0.0,
                "invalid funding rate or settlement mark"
            );
            Ok(FundingRate {
                venue_code: 1,
                symbol: row.symbol,
                ts_us: row
                    .funding_time
                    .checked_mul(1000)
                    .context("funding timestamp overflow")?,
                rate,
                mark_price: Some(mark_price),
                rate_type: row.rate_type,
            })
        })
        .collect()
}

#[derive(Default)]
struct SymbolCache {
    rates: Arc<Vec<FundingRate>>,
    from_us: Option<i64>,
    through_us: Option<i64>,
    requested_us: i64,
    requested_from_us: Option<i64>,
    attempted_us: i64,
    loading: bool,
    error: Option<String>,
}
#[derive(Default)]
struct Budget {
    requests: VecDeque<Instant>,
    cooldown: Option<Instant>,
    next_request: Option<Instant>,
    waf_rejections: u32,
}

#[derive(Clone, Copy)]
struct PriorityWindow {
    from: i64,
    through: i64,
    expires: i64,
}

#[derive(Clone)]
pub struct FundingRateCache {
    config: FundingRateConfig,
    client: reqwest::Client,
    endpoint: String,
    okex_endpoint: String,
    okex_marks_endpoint: String,
    symbols: Arc<RwLock<BTreeMap<String, SymbolCache>>>,
    budget: Arc<Mutex<Budget>>,
    concurrency: Arc<Semaphore>,
    priority_windows: Arc<RwLock<BTreeMap<String, PriorityWindow>>>,
}

#[derive(Clone, Debug, Default)]
pub struct RateCoverage {
    /// Complete symbol coverage captured under the same lock as the rate vectors.
    pub complete_symbols: BTreeSet<String>,
    pub rates: BTreeMap<String, Arc<Vec<FundingRate>>>,
    pub loading: bool,
    pub unavailable_reason: Option<String>,
    pub synced_through_ts_us: Option<i64>,
}

impl FundingRateCache {
    pub fn new(config: FundingRateConfig) -> Result<Self> {
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20));
        if let Some(ip) = config.local_ip {
            client = client.local_address(ip);
        }
        Ok(Self {
            config,
            client: client
                .build()
                .context("cannot build public funding client")?,
            endpoint: "https://fapi.binance.com/fapi/v1/fundingRate".into(),
            okex_endpoint: "https://www.okx.com/api/v5/public/funding-rate-history".into(),
            okex_marks_endpoint: "https://www.okx.com/api/v5/market/history-mark-price-candles"
                .into(),
            symbols: Default::default(),
            budget: Default::default(),
            concurrency: Arc::new(Semaphore::new(8)),
            priority_windows: Default::default(),
        })
    }

    pub fn spawn_refresh(&self) {
        if !self.config.enabled {
            return;
        }
        let cache = self.clone();
        tokio::spawn(async move {
            loop {
                let now = crate::kline::now_us();
                let mut symbols = cache.config.default_symbols.clone();
                {
                    let mut entries = cache.symbols.write().unwrap();
                    entries.retain(|symbol, entry| {
                        entry.loading
                            || entry.requested_us >= now - RETENTION_US
                            || symbols.contains(symbol)
                    });
                    symbols.extend(entries.keys().cloned());
                }
                symbols.sort();
                symbols.dedup();
                cache.schedule(&symbols, now, None);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
    }

    fn schedule(&self, symbols: &[String], now: i64, requested: Option<i64>) {
        if !self.config.enabled {
            return;
        }
        let mut entries = self.symbols.write().unwrap();
        for symbol in symbols.iter().filter(|s| supported_symbol(s)) {
            let entry = entries.entry(symbol.clone()).or_default();
            if let Some(from) = requested {
                entry.requested_us = now;
                entry.requested_from_us =
                    Some(entry.requested_from_us.map_or(from, |old| old.min(from)));
            }
            // OKX needs a separate historical mark request per settlement.
            // Load the requested window first instead of thirty days of marks.
            let requested_window =
                self.config.daily_estimates_enabled || symbol.starts_with("okex-futures:");
            let retained_from = now - RETENTION_US - 60_000_000;
            let minimum = if requested_window {
                let requested_from = entry.requested_from_us.unwrap_or(retained_from);
                if self.config.daily_estimates_enabled {
                    requested_from.max(1)
                } else {
                    requested_from.max(retained_from)
                }
            } else {
                retained_from
            };
            let needs_earlier = requested_window && entry.from_us.is_some_and(|ts| minimum < ts);
            let refresh_us = self.config.refresh_secs as i64 * 1_000_000;
            let retry_us = if entry.error.is_some() {
                60_000_000
            } else {
                refresh_us
            };
            if entry.loading
                || (entry.attempted_us > 0
                    && now - entry.attempted_us < retry_us
                    && !(needs_earlier && entry.error.is_none()))
            {
                continue;
            }
            entry.loading = true;
            entry.attempted_us = now;
            let mut from = if needs_earlier {
                minimum
            } else {
                entry
                    .through_us
                    .map_or(minimum, |ts| (ts - OVERLAP_US).max(minimum))
            }
            .div_euclid(1000)
                * 1000;
            // Publish recent settlements before a cumulative multi-month load
            // occupies the shared fetch slots. Older coverage extends next.
            if self.config.daily_estimates_enabled
                && entry.requested_from_us.is_some()
                && entry.from_us.is_none()
            {
                from = from.max((now - DAY_US).div_euclid(1000) * 1000);
            }
            // A missing historical mark must remain retryable after the normal
            // incremental overlap has moved past its settlement.
            if let Some(missing) = entry
                .rates
                .iter()
                .filter(|r| r.mark_price.is_none() && r.ts_us >= minimum)
                .map(|r| r.ts_us)
                .min()
            {
                from = from.min(missing);
            }
            let cache = self.clone();
            let symbol = symbol.clone();
            tokio::spawn(async move {
                let result = cache.fetch(&symbol, from, now).await;
                let mut entries = cache.symbols.write().unwrap();
                let entry = entries
                    .get_mut(&symbol)
                    .expect("loading cache entry retained");
                entry.loading = false;
                match result {
                    Ok(rows) => {
                        let mut merged: BTreeMap<_, _> = entry
                            .rates
                            .iter()
                            .filter(|r| {
                                (cache.config.daily_estimates_enabled
                                    || r.ts_us >= now - RETENTION_US - 60_000_000)
                                    && r.ts_us < from
                            })
                            .map(|r| ((r.ts_us, r.rate_type.clone()), r.clone()))
                            .collect();
                        merged.extend(
                            rows.into_iter()
                                .map(|r| ((r.ts_us, r.rate_type.clone()), r)),
                        );
                        entry.rates = Arc::new(merged.into_values().collect());
                        let earliest = entry.from_us.unwrap_or(from).min(from);
                        entry.from_us = Some(if cache.config.daily_estimates_enabled {
                            earliest
                        } else {
                            earliest.max(now - RETENTION_US - 60_000_000)
                        });
                        entry.through_us = Some(now / 1000 * 1000);
                        entry.error = None;
                    }
                    Err(error) => {
                        tracing::warn!(%symbol, %error, "public funding history refresh failed");
                        entry.error = Some(error.to_string());
                    }
                }
            });
        }
    }

    /// Fast, nonblocking read. Concurrent queries share one in-flight load per symbol.
    pub fn request_priority(&self, symbols: &[String], from: i64, through: i64) -> RateCoverage {
        let now = crate::kline::now_us();
        if self.config.enabled && from <= through && through <= now + 1_000_000 {
            let mut windows = self.priority_windows.write().unwrap();
            windows.retain(|_, window| window.expires > now);
            for symbol in symbols.iter().filter(|s| supported_symbol(s)) {
                windows.insert(
                    symbol.clone(),
                    PriorityWindow {
                        from,
                        through,
                        expires: now + 120_000_000,
                    },
                );
            }
        }
        self.request(symbols, from, through)
    }

    pub fn request(&self, symbols: &[String], from: i64, through: i64) -> RateCoverage {
        let now = crate::kline::now_us();
        // Millisecond API requests and a moving wall clock allow a small boundary tolerance.
        if !self.config.enabled || through > now + 1_000_000 || through < from {
            return RateCoverage {
                unavailable_reason: Some(
                    "公开资金费估算须启用 funding_rates，且查询区间不能晚于当前时间".into(),
                ),
                ..Default::default()
            };
        }
        let retained_from = now - RETENTION_US - 60_000_000;
        // Only request the retained overlap; older local orders still determine
        // settlement quantities. A wholly expired query never triggers REST.
        if self.config.daily_estimates_enabled || through >= retained_from {
            self.schedule(symbols, now, Some(from));
        }
        let entries = self.symbols.read().unwrap();
        let mut view = RateCoverage::default();
        if !self.config.daily_estimates_enabled && from < retained_from {
            view.unavailable_reason = Some(
                "公开费率仅保留最近 30 天；仅汇总可用时段及币种的估算 Funding，缺失部分未知".into(),
            );
        }
        for symbol in symbols {
            let Some(entry) = entries.get(symbol) else {
                view.unavailable_reason = Some(format!("资金费不支持合约 {symbol}"));
                continue;
            };
            let covers_window = entry.from_us.is_some_and(|ts| ts <= from)
                && entry.through_us.is_some_and(|ts| ts >= through - 1000);
            let missing_marks = entry
                .rates
                .iter()
                .any(|r| r.ts_us >= from && r.ts_us <= through && r.mark_price.is_none());
            // An older cumulative backfill must not keep a fully covered
            // interactive window in a loading state.
            view.loading |= entry.loading && (!covers_window || missing_marks);
            if let Some(error) = &entry.error {
                view.unavailable_reason = Some(format!("{symbol} 资金费历史暂不可用：{error}"));
            }
            if !covers_window {
                view.unavailable_reason
                    .get_or_insert_with(|| "公开资金费历史覆盖不完整；仅汇总已加载部分".into());
            } else if missing_marks {
                view.unavailable_reason.get_or_insert_with(|| {
                    format!("{symbol} 部分结算缺少标记价格；仅计入可估算部分")
                });
            } else if entry.error.is_none() {
                view.complete_symbols.insert(symbol.clone());
            }
            if entry.from_us.is_some_and(|ts| ts <= through)
                && entry.through_us.is_some_and(|ts| ts >= from)
            {
                view.rates.insert(symbol.clone(), entry.rates.clone());
                let end = entry.through_us.unwrap().min(through);
                view.synced_through_ts_us =
                    Some(view.synced_through_ts_us.map_or(end, |old| old.min(end)));
            }
        }
        view
    }

    /// Used by the read-only reconciliation command; the service uses request().
    pub async fn wait_for(
        &self,
        symbols: &[String],
        from: i64,
        through: i64,
    ) -> Result<RateCoverage> {
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            let coverage = self.request(symbols, from, through);
            if !coverage.loading {
                ensure!(
                    coverage.unavailable_reason.is_none(),
                    "{}",
                    coverage.unavailable_reason.unwrap_or_default()
                );
                return Ok(coverage);
            }
            ensure!(
                Instant::now() < deadline,
                "public funding history wait timed out"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn pending_priority(&self, symbol: Option<&str>) -> (bool, bool) {
        let now = crate::kline::now_us();
        let windows = self.priority_windows.read().unwrap();
        let entries = self.symbols.read().unwrap();
        let mut pending = false;
        let mut own = false;
        for (key, window) in windows.iter().filter(|(_, w)| w.expires > now) {
            if entries.get(key).is_some_and(|entry| {
                entry.loading
                    && (entry.from_us.is_none_or(|ts| ts > window.from)
                        || entry.through_us.is_none_or(|ts| ts < window.through - 1000)
                        || entry.rates.iter().any(|r| {
                            r.ts_us >= window.from
                                && r.ts_us <= window.through
                                && r.mark_price.is_none()
                        }))
            }) {
                pending = true;
                own |= symbol == Some(key.as_str());
            }
        }
        (pending, own)
    }

    async fn reserve(&self, symbol: Option<&str>) {
        loop {
            let (pending, own) = self.pending_priority(symbol);
            if pending && !own {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let delay = {
                let mut budget = self.budget.lock().await;
                let now = Instant::now();
                while budget
                    .requests
                    .front()
                    .is_some_and(|ts| now.duration_since(*ts) >= Duration::from_secs(300))
                {
                    budget.requests.pop_front();
                }
                if let Some(until) = budget.cooldown.filter(|ts| *ts > now) {
                    until - now
                } else if budget.requests.len() >= BUDGET {
                    budget.requests[0] + Duration::from_secs(300) - now
                } else if let Some(until) = budget.next_request.filter(|ts| *ts > now) {
                    until - now
                } else {
                    budget.requests.push_back(now);
                    budget.next_request = Some(now + REQUEST_INTERVAL);
                    return;
                }
            };
            tokio::time::sleep(delay).await;
        }
    }

    async fn observe_response(&self, status: reqwest::StatusCode, retry_after: Option<u64>) {
        let mut budget = self.budget.lock().await;
        let now = Instant::now();
        let cooling_down = budget.cooldown.is_some_and(|until| until > now);
        if status.is_success() {
            // An older in-flight success must not reset a newly imposed cooldown.
            if !cooling_down {
                budget.waf_rejections = 0;
            }
        } else if matches!(status.as_u16(), 403 | 418 | 429) {
            let seconds = if status.as_u16() == 403 {
                // Multiple in-flight failures are one rejection episode. Escalate
                // only if the first request after a cooldown is rejected again.
                if !cooling_down {
                    budget.waf_rejections = budget.waf_rejections.saturating_add(1);
                }
                let backoff = (WAF_COOLDOWN_SECS
                    * (1_u64 << budget.waf_rejections.saturating_sub(1).min(4)))
                .min(3600);
                retry_after.unwrap_or(backoff).max(backoff)
            } else {
                retry_after.unwrap_or(300)
            }
            .clamp(1, 86400);
            let until = now + Duration::from_secs(seconds);
            budget.cooldown = Some(budget.cooldown.map_or(until, |old| old.max(until)));
        }
    }

    async fn fetch(&self, symbol: &str, from: i64, through: i64) -> Result<Vec<FundingRate>> {
        let (venue, asset) = crate::market_data::split_cache_symbol(symbol)?;
        if venue == "okex-futures" {
            return self.fetch_okex(asset, from, through).await;
        }
        let mut from_ms = from.div_euclid(1000);
        let through_ms = through.div_euclid(1000);
        let mut rates = Vec::new();
        // Thirty days of hourly settlements fit in a single 1000-row request.
        // Keep pagination for additional settlement types and future interval changes.
        for _ in 0..32 {
            // Yield fetch capacity between historical pages so one long
            // backfill cannot monopolize a slot until its entire history ends.
            self.reserve(Some(symbol)).await;
            let _permit = self.concurrency.acquire().await?;
            let response = self
                .client
                .get(&self.endpoint)
                .query(&[
                    ("symbol", symbol.to_string()),
                    ("startTime", from_ms.to_string()),
                    ("endTime", through_ms.to_string()),
                    ("limit", "1000".into()),
                ])
                .send()
                .await
                .map_err(reqwest::Error::without_url)
                .context("public funding request failed")?;
            self.observe_response(
                response.status(),
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok()),
            )
            .await;
            ensure!(
                response.status().is_success(),
                "public funding REST returned HTTP {}",
                response.status().as_u16()
            );
            let rows: Vec<RateRow> = response
                .json()
                .await
                .map_err(reqwest::Error::without_url)
                .context("invalid public funding response")?;
            let page = parse_page(rows, symbol, from_ms, through_ms)?;
            let full = page.len() == 1000;
            let last = page.last().map(|r| r.ts_us / 1000);
            rates.extend(page);
            if !full || last.is_none_or(|ts| ts >= through_ms) {
                return Ok(rates);
            }
            from_ms = last.unwrap() + 1;
        }
        anyhow::bail!("public funding pagination incomplete")
    }

    async fn okex_get(
        &self,
        symbol: &str,
        endpoint: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value> {
        self.reserve(Some(symbol)).await;
        let _permit = self.concurrency.acquire().await?;
        let response = self
            .client
            .get(endpoint)
            .query(query)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        self.observe_response(
            response.status(),
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok()),
        )
        .await;
        ensure!(
            response.status().is_success(),
            "OKX public funding HTTP {}",
            response.status().as_u16()
        );
        let body: serde_json::Value = response.json().await.map_err(reqwest::Error::without_url)?;
        if body["code"].as_str() == Some("50011") {
            self.observe_response(reqwest::StatusCode::TOO_MANY_REQUESTS, Some(60))
                .await;
        }
        ensure!(
            body["code"].as_str() == Some("0"),
            "OKX public funding error: {}",
            body["code"]
        );
        Ok(body)
    }

    async fn fetch_okex(&self, symbol: &str, from: i64, through: i64) -> Result<Vec<FundingRate>> {
        let inst = crate::kline::okex_instrument("okex-futures", symbol)?;
        let mut cursor = through / 1000 + 1;
        let from_ms = from / 1000;
        let mut settlements = BTreeMap::new();
        let mut completed = false;
        // OKX exposes up to three months. Never claim older query coverage.
        ensure!(
            from >= crate::kline::now_us() - 89 * DAY_US,
            "OKX public funding history is limited to the recent three months"
        );
        for _ in 0..32 {
            let body = self
                .okex_get(
                    &format!("okex-futures:{symbol}"),
                    &self.okex_endpoint,
                    &[
                        ("instId", inst.clone()),
                        ("after", cursor.to_string()),
                        ("before", (from_ms - 1).to_string()),
                        ("limit", "100".into()),
                    ],
                )
                .await?;
            let rows = body["data"]
                .as_array()
                .context("missing OKX funding history")?;
            ensure!(rows.len() <= 100, "oversized OKX funding page");
            let mut oldest = cursor;
            for row in rows {
                ensure!(
                    row["instId"].as_str() == Some(&inst),
                    "OKX funding instrument mismatch"
                );
                let ts = row["fundingTime"]
                    .as_str()
                    .context("missing OKX funding time")?
                    .parse::<i64>()?;
                ensure!(
                    ts < cursor && ts >= from_ms,
                    "OKX funding timestamp outside requested range"
                );
                oldest = oldest.min(ts);
                let rate = row["realizedRate"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| row["fundingRate"].as_str())
                    .context("missing OKX funding rate")?
                    .parse::<f64>()?;
                ensure!(
                    rate.is_finite() && settlements.insert(ts, rate).is_none(),
                    "invalid or duplicate OKX funding settlement"
                );
            }
            if rows.len() < 100 || oldest <= from_ms {
                completed = true;
                break;
            }
            ensure!(oldest < cursor, "OKX funding pagination made no progress");
            cursor = oldest;
        }
        ensure!(completed, "OKX funding pagination incomplete");
        let mut result = Vec::new();
        for (ts, rate) in settlements {
            let cached = self
                .symbols
                .read()
                .unwrap()
                .get(&format!("okex-futures:{symbol}"))
                .and_then(|s| {
                    s.rates
                        .iter()
                        .find(|r| r.ts_us == ts * 1000 && r.rate == rate && r.mark_price.is_some())
                        .cloned()
                });
            if let Some(cached) = cached {
                result.push(cached);
                continue;
            }
            let body = self
                .okex_get(
                    &format!("okex-futures:{symbol}"),
                    &self.okex_marks_endpoint,
                    &[
                        ("instId", inst.clone()),
                        ("bar", "1m".into()),
                        ("after", ts.to_string()),
                        ("before", (ts - 60_001).to_string()),
                        ("limit", "1".into()),
                    ],
                )
                .await?;
            let mark_price = parse_okex_settlement_mark(&body, ts).ok();
            result.push(FundingRate {
                venue_code: 3,
                symbol: symbol.into(),
                ts_us: ts
                    .checked_mul(1000)
                    .context("OKX funding timestamp overflow")?,
                rate,
                mark_price,
                rate_type: "OKX_closed_1m_mark_estimate".into(),
            });
        }
        Ok(result)
    }
}

fn parse_okex_settlement_mark(body: &serde_json::Value, ts_ms: i64) -> Result<f64> {
    let rows = body["data"]
        .as_array()
        .context("missing OKX historical mark candles")?;
    ensure!(rows.len() == 1, "missing OKX settlement mark minute");
    let row = rows[0].as_array().context("invalid OKX mark candle")?;
    ensure!(
        row.len() == 6
            && row[5].as_str() == Some("1")
            && row[0].as_str().and_then(|s| s.parse::<i64>().ok()) == Some(ts_ms - 60_000),
        "unconfirmed or misaligned OKX settlement mark minute"
    );
    let mark = row[4]
        .as_str()
        .context("missing OKX mark close")?
        .parse::<f64>()?;
    ensure!(
        mark.is_finite() && mark > 0.0,
        "invalid OKX historical mark"
    );
    Ok(mark)
}

#[derive(Clone, Debug, Serialize)]
pub struct FundingEstimateRecord {
    pub venue_code: i16,
    pub source_id: String,
    pub symbol: String,
    pub asset: String,
    pub ts_us: i64,
    pub rate_type: String,
    pub rate: f64,
    pub mark_price: f64,
    pub quantity: f64,
    pub income_quote: f64,
    pub strategies: BTreeMap<String, f64>,
    pub boundary_fill_count: usize,
}

#[derive(Default)]
pub struct FundingEstimateRecords {
    pub records: Vec<FundingEstimateRecord>,
    pub skipped_settlements: usize,
    pub skipped_by_symbol: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StrategyFundingEstimate {
    pub strategy: String,
    pub income_quote: f64,
    pub income_by_asset: BTreeMap<String, f64>,
    pub total_nav_before_fee_quote: f64,
    pub total_nav_after_fee_quote: f64,
    pub points: Vec<FundingNavPoint>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FundingEstimate {
    pub reconciliation: Option<FundingReconciliation>,
    pub loading: bool,
    pub unavailable_reason: Option<String>,
    pub synced_through_ts_us: Option<i64>,
    pub income_quote: Option<f64>,
    pub income_by_asset: BTreeMap<String, f64>,
    pub total_nav_before_fee_quote: Option<f64>,
    pub total_nav_after_fee_quote: Option<f64>,
    pub settlement_count: usize,
    pub boundary_fill_count: usize,
    pub points: Vec<FundingNavPoint>,
    pub strategies: Vec<StrategyFundingEstimate>,
}

pub fn estimate_timeline(
    report: &NavTimelineReport,
    coverage: &RateCoverage,
    measured: &FundingEstimateRecords,
) -> FundingEstimate {
    let records = &measured.records;
    let mut result = FundingEstimate {
        loading: coverage.loading,
        unavailable_reason: coverage.unavailable_reason.clone(),
        synced_through_ts_us: coverage.synced_through_ts_us,
        ..Default::default()
    };
    if measured.skipped_settlements > 0 {
        let reason = result.unavailable_reason.get_or_insert_default();
        if !reason.is_empty() {
            reason.push_str("；");
        }
        reason.push_str(&format!(
            "{} 次结算无法还原持仓或金额，已排除，不计入合计",
            measured.skipped_settlements
        ));
        if records.is_empty() {
            return result;
        }
    }
    // A reason can describe partial coverage. Only absent coverage is null;
    // an observed overlap with no settlements is a factual zero for that part.
    if coverage.unavailable_reason.is_some() && coverage.rates.is_empty() {
        return result;
    }
    let income = records.iter().map(|r| r.income_quote).sum();
    result.income_quote = Some(income);
    result.total_nav_before_fee_quote = Some(report.summary.nav_change_before_fee_quote + income);
    result.total_nav_after_fee_quote = Some(report.summary.nav_change_after_fee_quote + income);
    result.settlement_count = records.len();
    result.boundary_fill_count = records.iter().map(|r| r.boundary_fill_count).sum();
    for record in records {
        *result
            .income_by_asset
            .entry(record.asset.clone())
            .or_default() += record.income_quote;
    }
    result.points = estimate_points(
        &report.points,
        report.start_ts_us,
        report.end_ts_us,
        records.iter().map(|r| (r.ts_us, r.income_quote)),
    );
    for strategy in &report.strategy_points {
        let income = records
            .iter()
            .map(|r| r.strategies.get(&strategy.strategy).copied().unwrap_or(0.0))
            .sum();
        let mut assets = BTreeMap::<String, f64>::new();
        for r in records {
            *assets.entry(r.asset.clone()).or_default() +=
                r.strategies.get(&strategy.strategy).copied().unwrap_or(0.0);
        }
        result.strategies.push(StrategyFundingEstimate {
            strategy: strategy.strategy.clone(),
            income_quote: income,
            income_by_asset: assets,
            total_nav_before_fee_quote: strategy.summary.nav_change_before_fee_quote + income,
            total_nav_after_fee_quote: strategy.summary.nav_change_after_fee_quote + income,
            points: estimate_points(
                &strategy.points,
                report.start_ts_us,
                report.end_ts_us,
                records.iter().map(|r| {
                    (
                        r.ts_us,
                        r.strategies.get(&strategy.strategy).copied().unwrap_or(0.0),
                    )
                }),
            ),
        });
    }
    result
}

fn estimate_points(
    points: &[NavTimelinePoint],
    start: i64,
    end: i64,
    records: impl Iterator<Item = (i64, f64)>,
) -> Vec<FundingNavPoint> {
    let mut amounts: BTreeMap<i64, f64> = BTreeMap::new();
    for (ts, income) in records {
        *amounts.entry(ts).or_default() += income;
    }
    let mut amounts = amounts.into_iter().peekable();
    let mut income = 0.0;
    points
        .iter()
        .map(|point| {
            while amounts.peek().is_some_and(|(ts, _)| *ts <= point.ts_us) {
                income += amounts.next().unwrap().1;
            }
            let value = if point.ts_us == start && end > start {
                0.0
            } else {
                income
            };
            FundingNavPoint {
                ts_us: point.ts_us,
                funding_income_quote: value,
                total_nav_before_fee_quote: point.totals.nav_change_before_fee_quote + value,
                total_nav_after_fee_quote: point.totals.nav_change_after_fee_quote + value,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Query, State},
        routing::get,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn okex_rates_use_realized_rates_and_pre_settlement_closed_mark_minutes() {
        let hour = crate::kline::now_us().div_euclid(3_600_000_000) * 3_600_000;
        let marks = Arc::new(AtomicUsize::new(0));
        let counter = marks.clone();
        let app = Router::new()
            .route("/rates", get(move |Query(q): Query<BTreeMap<String, String>>| async move {
                assert_eq!(q["instId"], "BTC-USDT-SWAP");
                Json(serde_json::json!({"code":"0", "data":[
                    {"instId":"BTC-USDT-SWAP", "fundingTime":hour.to_string(), "fundingRate":"0.1", "realizedRate":"0.001"},
                    {"instId":"BTC-USDT-SWAP", "fundingTime":(hour-3_600_000).to_string(), "fundingRate":"-0.002", "realizedRate":""}
                ]}))
            }))
            .route("/marks", get(move |Query(q): Query<BTreeMap<String, String>>| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let ts = q["after"].parse::<i64>().unwrap();
                    assert_eq!(q["before"].parse::<i64>().unwrap(), ts - 60_001);
                    Json(serde_json::json!({"code":"0", "data":[[(ts-60_000).to_string(), "99", "102", "98", "100", "1"]]}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        cache.okex_endpoint = format!("http://{address}/rates");
        cache.okex_marks_endpoint = format!("http://{address}/marks");
        let rows = cache
            .fetch(
                "okex-futures:BTCUSDT",
                (hour - 3_600_000) * 1000,
                hour * 1000,
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rate, -0.002);
        assert_eq!(rows[1].rate, 0.001);
        assert!(rows.iter().all(|r| r.venue_code == 3
            && r.mark_price == Some(100.0)
            && r.rate_type == "OKX_closed_1m_mark_estimate"));
        assert_eq!(marks.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn okex_cold_query_loads_requested_marks_and_extends_older_coverage_on_demand() {
        let now = crate::kline::now_us().div_euclid(1000) * 1000;
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests = observed.clone();
        let marks = Arc::new(AtomicUsize::new(0));
        let counter = marks.clone();
        let app = Router::new()
            .route("/rates", get(move |Query(q): Query<BTreeMap<String, String>>| {
                let requests = requests.clone();
                async move {
                    let from = q["before"].parse::<i64>().unwrap() + 1;
                    let through = q["after"].parse::<i64>().unwrap() - 1;
                    requests.lock().unwrap().push(from);
                    let rows = [8, 16, 32].into_iter().map(|hours| now / 1000 - hours * 3_600_000)
                        .filter(|ts| (from..=through).contains(ts))
                        .map(|ts| serde_json::json!({"instId":"BTC-USDT-SWAP", "fundingTime":ts.to_string(), "realizedRate":"0.001"}))
                        .collect::<Vec<_>>();
                    Json(serde_json::json!({"code":"0", "data":rows}))
                }
            }))
            .route("/marks", get(move |Query(q): Query<BTreeMap<String, String>>| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let ts = q["after"].parse::<i64>().unwrap();
                    Json(serde_json::json!({"code":"0", "data":[[(ts-60_000).to_string(), "99", "102", "98", "100", "1"]]}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        cache.okex_endpoint = format!("http://{address}/rates");
        cache.okex_marks_endpoint = format!("http://{address}/marks");
        let symbols = vec!["okex-futures:BTCUSDT".into()];
        let from = now - DAY_US;
        for _ in 0..10 {
            assert!(cache.request(&symbols, from, now).loading);
        }
        let first = cache.wait_for(&symbols, from, now).await.unwrap();
        assert_eq!(first.rates[&symbols[0]].len(), 2);
        assert_eq!(*observed.lock().unwrap(), vec![from / 1000]);
        assert_eq!(marks.load(Ordering::SeqCst), 2);
        let older = now - 2 * DAY_US;
        assert!(cache.request(&symbols, older, now).loading);
        let expanded = cache.wait_for(&symbols, older, now).await.unwrap();
        assert_eq!(expanded.rates[&symbols[0]].len(), 3);
        assert_eq!(*observed.lock().unwrap(), vec![from / 1000, older / 1000]);
        assert_eq!(
            marks.load(Ordering::SeqCst),
            3,
            "previously loaded marks are reused"
        );
        assert!(!cache.request(&symbols, from, now).loading);
        server.abort();
    }

    #[tokio::test]
    async fn cumulative_cold_load_publishes_recent_window_before_older_backfill_finishes() {
        let now = crate::kline::now_us().div_euclid(1000) * 1000;
        let recent = now - DAY_US + 60_000_000;
        let old = now - 10 * DAY_US;
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = requests.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = release.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/rates",
                    get(move |Query(q): Query<BTreeMap<String, String>>| {
                        let observed = observed.clone();
                        let gate = gate.clone();
                        async move {
                            let from = q["startTime"].parse::<i64>().unwrap();
                            observed.lock().unwrap().push(from);
                            if from < (now - 2 * DAY_US) / 1000 {
                                gate.notified().await;
                            }
                            Json(serde_json::json!([]))
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut config = FundingRateConfig::default();
        config.daily_estimates_enabled = true;
        let mut cache = FundingRateCache::new(config).unwrap();
        cache.endpoint = format!("http://{address}/rates");
        let symbols = vec!["TRBUSDT".into()];
        assert!(cache.request(&symbols, old, now).loading);
        let recent_view = tokio::time::timeout(
            Duration::from_secs(5),
            cache.wait_for(&symbols, recent, now),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(recent_view.complete_symbols.contains("TRBUSDT"));
        assert!(!recent_view.loading);
        let cold_from = cache.symbols.read().unwrap()["TRBUSDT"].from_us.unwrap();
        assert!((now - DAY_US..=recent).contains(&cold_from));
        let old_view = cache.request(&symbols, old, now);
        assert!(old_view.loading);
        assert!(old_view.unavailable_reason.is_some());
        assert!(!old_view.complete_symbols.contains("TRBUSDT"));
        assert!(cache.symbols.read().unwrap()["TRBUSDT"].loading);
        let recent_view = cache.request(&symbols, recent, now);
        assert!(
            !recent_view.loading,
            "unrelated older work cannot block the recent window"
        );
        assert!(recent_view.unavailable_reason.is_none());
        assert!(recent_view.complete_symbols.contains("TRBUSDT"));
        // Let the single older request finish after observing its query bounds.
        tokio::time::timeout(Duration::from_secs(5), async {
            while requests.lock().unwrap().len() < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        release.notify_one();
        let complete = cache.wait_for(&symbols, old, now).await.unwrap();
        assert!(!complete.loading);
        assert!(complete.unavailable_reason.is_none());
        assert_eq!(
            *requests.lock().unwrap(),
            vec![cold_from / 1000, old / 1000]
        );
        server.abort();
    }

    #[tokio::test]
    async fn managed_window_gets_budget_before_background_and_releases_priority_when_covered() {
        let cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        let now = crate::kline::now_us();
        let key = "okex-futures:TRBUSDT".to_string();
        cache.symbols.write().unwrap().insert(
            key.clone(),
            SymbolCache {
                loading: true,
                ..Default::default()
            },
        );
        assert!(
            cache
                .request_priority(&[key.clone()], now - DAY_US, now)
                .loading
        );
        let background_cache = cache.clone();
        let mut background = tokio::spawn(async move {
            background_cache.reserve(Some("BTCUSDT")).await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut background)
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_millis(100), cache.reserve(Some(&key)))
            .await
            .unwrap();
        assert_eq!(cache.budget.lock().await.requests.len(), 1);
        // The symbol can still be loading earlier history. The selected window
        // has arrived, so it no longer blocks unrelated accounts' requests.
        {
            let mut entries = cache.symbols.write().unwrap();
            let entry = entries.get_mut(&key).unwrap();
            entry.from_us = Some(now - DAY_US);
            entry.through_us = Some(now);
        }
        let covered = cache.request_priority(&[key], now - DAY_US, now);
        assert!(!covered.loading);
        assert!(covered.unavailable_reason.is_none());
        cache.budget.lock().await.next_request = Some(Instant::now());
        tokio::time::timeout(Duration::from_millis(500), background)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.budget.lock().await.requests.len(), 2);
    }

    #[tokio::test]
    async fn concurrent_symbols_share_request_pacing() {
        let cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        let reserve = || async {
            cache.reserve(None).await;
            Instant::now()
        };
        let (a, b, c) = tokio::join!(reserve(), reserve(), reserve());
        let mut times = [a, b, c];
        times.sort();
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= REQUEST_INTERVAL);
        }
    }

    #[tokio::test]
    async fn waf_rejection_blocks_other_symbols_and_recovers_without_a_retry_burst() {
        use axum::http::StatusCode;
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/rates",
                    get(move || {
                        let count = requests.clone();
                        async move {
                            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                                (StatusCode::FORBIDDEN, "blocked")
                            } else {
                                (StatusCode::OK, "[]")
                            }
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        cache.endpoint = format!("http://{address}/rates");
        assert!(
            cache
                .fetch("BTCUSDT", 0, DAY_US)
                .await
                .unwrap_err()
                .to_string()
                .contains("403")
        );
        let first_cooldown = cache.budget.lock().await.cooldown.unwrap();
        assert!(first_cooldown > Instant::now() + Duration::from_secs(299));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                cache.fetch("ETHUSDT", 0, DAY_US)
            )
            .await
            .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // In-flight failures share one episode; late successes do not reset it.
        cache.observe_response(StatusCode::FORBIDDEN, None).await;
        cache.observe_response(StatusCode::OK, None).await;
        assert_eq!(cache.budget.lock().await.waf_rejections, 1);
        // Model expiry, then another rejection: double the backoff, preserving
        // a longer server-provided Retry-After if present.
        cache.budget.lock().await.cooldown = Some(Instant::now());
        cache
            .observe_response(StatusCode::FORBIDDEN, Some(900))
            .await;
        assert_eq!(cache.budget.lock().await.waf_rejections, 2);
        assert!(
            cache.budget.lock().await.cooldown.unwrap() > Instant::now() + Duration::from_secs(899)
        );
        {
            let mut budget = cache.budget.lock().await;
            budget.cooldown = Some(Instant::now());
            budget.next_request = Some(Instant::now());
        }
        assert!(cache.fetch("ETHUSDT", 0, DAY_US).await.unwrap().is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(cache.budget.lock().await.waf_rejections, 0);
        server.abort();
    }

    #[test]
    fn validates_public_settlements_and_preserves_negative_rates_and_usdc() {
        let row = |symbol: &str, time, mark: &str| RateRow {
            symbol: symbol.into(),
            funding_time: time,
            funding_rate: "-0.0001".into(),
            mark_price: mark.into(),
            rate_type: "Regular".into(),
        };
        let rates = parse_page(vec![row("BTCUSDC", 1000, "65000")], "BTCUSDC", 1000, 2000).unwrap();
        assert_eq!(rates[0].rate, -0.0001);
        assert_eq!(rates[0].ts_us, 1_000_000);
        assert!(parse_page(vec![row("BTCUSDT", 1000, "65000")], "BTCUSDC", 1000, 2000).is_err());
        assert!(parse_page(vec![row("BTCUSDC", 999, "65000")], "BTCUSDC", 1000, 2000).is_err());
        assert!(parse_page(vec![row("BTCUSDC", 1000, "0")], "BTCUSDC", 1000, 2000).is_err());
        assert!(parse_page(vec![row("BTCUSDC", 1000, "NaN")], "BTCUSDC", 1000, 2000).is_err());
        assert!(
            parse_page(
                vec![row("BTCUSDC", 1001, "1"), row("BTCUSDC", 1000, "1")],
                "BTCUSDC",
                1000,
                2000
            )
            .is_err()
        );
        assert!(
            parse_page(
                vec![row("BTCUSDC", 1000, "1"), row("BTCUSDC", 1000, "1")],
                "BTCUSDC",
                1000,
                2000
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn caches_hourly_month_in_one_thousand_limit_request_and_coalesces_queries() {
        let count = Arc::new(AtomicUsize::new(0));
        async fn handler(
            State(count): State<Arc<AtomicUsize>>,
            Query(query): Query<BTreeMap<String, String>>,
        ) -> Json<serde_json::Value> {
            assert_eq!(query["limit"], "1000");
            assert_eq!(query["symbol"], "BTCUSDC");
            let from: i64 = query["startTime"].parse().unwrap();
            let through: i64 = query["endTime"].parse().unwrap();
            assert!(through - from >= 30 * 86_400_000);
            count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let rows: Vec<_> = (1..=720)
                .map(|n| {
                    serde_json::json!({"symbol":"BTCUSDC","fundingTime":from+n*3_600_000,
                "fundingRate":"0.0001","markPrice":"100"})
                })
                .collect();
            Json(serde_json::json!(rows))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/rates", get(handler))
                    .with_state(count.clone()),
            )
            .await
            .unwrap();
            count
        });
        let mut cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        cache.endpoint = format!("http://{address}/rates");
        let now = crate::kline::now_us();
        let from = now - RETENTION_US + 1_000_000;
        let symbols = vec!["BTCUSDC".into()];
        for _ in 0..10 {
            assert!(cache.request(&symbols, from, now).loading);
        }
        let coverage = cache.wait_for(&symbols, from, now).await.unwrap();
        assert_eq!(coverage.rates["BTCUSDC"].len(), 720);
        for _ in 0..10 {
            assert!(!cache.request(&symbols, from, now).loading);
        }
        assert_eq!(cache.budget.lock().await.requests.len(), 1);
        for days in [60, 90] {
            let partial = cache.request(&symbols, now - days * DAY_US, now);
            assert!(partial.unavailable_reason.is_some());
            assert_eq!(partial.rates["BTCUSDC"].len(), 720);
        }
        assert_eq!(cache.budget.lock().await.requests.len(), 1);
        let old = cache.request(
            &["ETHUSDT".into()],
            now - 60 * DAY_US,
            now - RETENTION_US - DAY_US,
        );
        assert!(old.unavailable_reason.is_some());
        assert!(!cache.symbols.read().unwrap().contains_key("ETHUSDT"));
        server.abort();
    }

    #[test]
    fn stale_rate_coverage_keeps_known_settlements_and_marks_unknown_tail_partial() {
        let cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        let now = crate::kline::now_us();
        let through = now - 2 * DAY_US;
        let symbols = vec!["BTCUSDT".into()];
        cache.symbols.write().unwrap().insert(
            "BTCUSDT".into(),
            SymbolCache {
                from_us: Some(now - 30 * DAY_US),
                through_us: Some(through),
                attempted_us: now, // No REST is needed for this cached-coverage test.
                rates: Arc::new(vec![FundingRate {
                    venue_code: 1,
                    symbol: "BTCUSDT".into(),
                    ts_us: through,
                    rate: 0.001,
                    mark_price: Some(100.0),
                    rate_type: "Regular".into(),
                }]),
                ..Default::default()
            },
        );
        let complete = cache.request(&symbols, now - 7 * DAY_US, through);
        assert!(complete.unavailable_reason.is_none());
        let partial = cache.request(&symbols, now - 7 * DAY_US, now);
        assert!(partial.unavailable_reason.is_some());
        assert_eq!(partial.rates["BTCUSDT"].len(), 1);
        assert_eq!(partial.synced_through_ts_us, Some(through));
        let absent = cache.request(&symbols, through + 1, now);
        assert!(absent.unavailable_reason.is_some());
        assert!(absent.rates.is_empty());
    }

    #[test]
    fn boundary_income_is_kept_after_zero_baseline_and_in_zero_duration_windows() {
        let point = |ts| NavTimelinePoint {
            ts_us: ts,
            ..Default::default()
        };
        let points = estimate_points(
            &[point(100), point(200)],
            100,
            200,
            [(100, -2.0), (200, 1.0)].into_iter(),
        );
        assert_eq!(points[0].funding_income_quote, 0.0);
        assert_eq!(points[1].funding_income_quote, -1.0);
        let one = estimate_points(&[point(100)], 100, 100, [(100, -2.0)].into_iter());
        assert_eq!(one[0].funding_income_quote, -2.0);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FundingReconciliation {
    pub compared_settlements: usize,
    pub actual_record_count: usize,
    pub unobserved_settlements: usize,
    pub unmatched_actual_records: usize,
    pub estimated_income_quote: f64,
    pub actual_income_quote: f64,
    pub difference_quote: f64,
    pub absolute_error_quote: f64,
    pub max_error_quote: f64,
    pub mismatch_count: usize,
}

/// Compare only observed symbol ranges. Absence before a symbol's collection
/// start (or before posting has had time to finish) is unknown, never zero.
pub fn reconcile_estimates(
    records: &[FundingEstimateRecord],
    actual: &[crate::funding::FundingRecord],
    observations: &[crate::funding::FundingSymbolStatus],
) -> FundingReconciliation {
    let mut result = FundingReconciliation::default();
    let observations: BTreeMap<_, _> = observations
        .iter()
        .map(|s| ((s.source_id.as_str(), s.symbol.as_str()), s))
        .collect();
    let mut settlements = BTreeMap::<(&str, &str, i64), f64>::new();
    let mut by_symbol = BTreeMap::<(&str, &str, &str), BTreeSet<i64>>::new();
    for r in records {
        *settlements
            .entry((&r.source_id, &r.symbol, r.ts_us))
            .or_default() += r.income_quote;
        by_symbol
            .entry((&r.source_id, &r.symbol, &r.asset))
            .or_default()
            .insert(r.ts_us);
    }
    let mut matched = BTreeMap::<(&str, &str, i64), (usize, f64)>::new();
    for row in actual {
        let nearest = by_symbol
            .get(&(
                row.source_id.as_str(),
                row.symbol.as_str(),
                row.asset.as_str(),
            ))
            .and_then(|times| {
                times
                    .range(..=row.ts_us)
                    .next_back()
                    .into_iter()
                    .chain(times.range(row.ts_us..).next())
                    .min_by_key(|ts| (**ts - row.ts_us).abs())
            })
            .filter(|ts| (**ts - row.ts_us).abs() <= 900_000_000);
        if let Some(ts) = nearest {
            let item = matched
                .entry((&row.source_id, &row.symbol, *ts))
                .or_default();
            item.0 += 1;
            item.1 += row.income_quote;
        } else {
            result.unmatched_actual_records += 1;
        }
    }
    for ((source, symbol, ts), estimate) in settlements {
        let seen = matched.get(&(source, symbol, ts));
        let observed = seen.is_some()
            || observations.get(&(source, symbol)).is_some_and(|state| {
                state.start_ts_us <= ts
                    && state
                        .synced_through_ts_us
                        .is_some_and(|end| end >= ts.saturating_add(900_000_000))
            });
        if !observed {
            if estimate.abs() > 1e-10 {
                result.unobserved_settlements += 1;
            }
            continue;
        }
        let (count, income) = seen.copied().unwrap_or_default();
        if count == 0 && estimate.abs() < 1e-10 {
            continue;
        }
        result.compared_settlements += 1;
        result.actual_record_count += count;
        result.estimated_income_quote += estimate;
        result.actual_income_quote += income;
        let error = estimate - income;
        result.absolute_error_quote += error.abs();
        result.max_error_quote = result.max_error_quote.max(error.abs());
        if error.abs() > 1e-7_f64.max(income.abs() * 1e-6) {
            result.mismatch_count += 1;
        }
    }
    result.difference_quote = result.estimated_income_quote - result.actual_income_quote;
    result
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use crate::funding::{FundingRecord, FundingSymbolStatus};

    #[test]
    fn reconciliation_separates_observation_gaps_posting_delays_and_source_identity() {
        let record = |source: &str, symbol: &str, income| FundingEstimateRecord {
            venue_code: 1,
            source_id: source.into(),
            symbol: symbol.into(),
            asset: "USDT".into(),
            ts_us: 1_000_000_000,
            rate_type: "Regular".into(),
            rate: 0.0001,
            mark_price: 100.0,
            quantity: 1.0,
            income_quote: income,
            strategies: BTreeMap::new(),
            boundary_fill_count: 0,
        };
        let records = vec![
            record("a", "BTCUSDT", -2.0),
            record("a", "ETHUSDT", 1.0),
            record("b", "BTCUSDT", -3.0),
            record("a", "SOLUSDT", -1.0),
        ];
        let actual = vec![FundingRecord {
            source_id: "a".into(),
            venue: "binance-futures".into(),
            symbol: "BTCUSDT".into(),
            asset: "USDT".into(),
            ts_us: 1_250_000_000,
            income_quote: -2.0,
        }];
        let observed = |source: &str, symbol: &str, start, end| FundingSymbolStatus {
            source_id: source.into(),
            symbol: symbol.into(),
            start_ts_us: start,
            synced_through_ts_us: Some(end),
        };
        let observations = vec![
            observed("a", "BTCUSDT", 0, 2_000_000_000),
            observed("a", "ETHUSDT", 1_100_000_000, 2_000_000_000),
            observed("b", "BTCUSDT", 0, 2_000_000_000),
            observed("a", "SOLUSDT", 0, 1_100_000_000),
        ];
        let result = reconcile_estimates(&records, &actual, &observations);
        assert_eq!(result.compared_settlements, 2);
        assert_eq!(result.actual_record_count, 1);
        assert_eq!(result.unobserved_settlements, 2);
        assert_eq!(result.mismatch_count, 1);
        assert_eq!(result.difference_quote, -3.0);
        assert_eq!(result.absolute_error_quote, 3.0);
    }

    #[tokio::test]
    async fn failed_refresh_retains_cache_and_applies_shared_retry_after_without_zero_estimates() {
        use axum::{Router, http::StatusCode, routing::get};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/rates",
                    get(|| async {
                        (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "30")],
                            "busy",
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut cache = FundingRateCache::new(FundingRateConfig::default()).unwrap();
        cache.endpoint = format!("http://{address}/rates");
        let now = crate::kline::now_us();
        let rate = FundingRate {
            venue_code: 1,
            symbol: "BTCUSDT".into(),
            ts_us: now - 3_600_000_000,
            rate: 0.0001,
            mark_price: Some(100.0),
            rate_type: "Regular".into(),
        };
        cache.symbols.write().unwrap().insert(
            "BTCUSDT".into(),
            SymbolCache {
                rates: Arc::new(vec![rate.clone()]),
                from_us: Some(now - RETENTION_US),
                through_us: Some(now - 3_600_000_000),
                ..Default::default()
            },
        );
        assert!(
            cache
                .wait_for(&["BTCUSDT".into()], now - DAY_US, now)
                .await
                .is_err()
        );
        let entry = cache.symbols.read().unwrap();
        assert_eq!(*entry["BTCUSDT"].rates, vec![rate]);
        assert!(!entry["BTCUSDT"].loading);
        assert!(entry["BTCUSDT"].error.is_some());
        drop(entry);
        assert!(cache.budget.lock().await.cooldown.unwrap() > Instant::now());
        let view = cache.request(&["BTCUSDT".into()], now - DAY_US, now);
        assert!(!view.loading);
        assert!(view.unavailable_reason.is_some());
        server.abort();
    }
}
