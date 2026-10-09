//! One process-wide budget for all asset-management accounts using an egress.
//! Reservations use rolling windows, including unsuccessful requests. Exhaustion
//! returns before sending rather than sleeping while a funds transaction is held.
use std::collections::{BTreeMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode, header::HeaderMap};

#[derive(Debug)]
pub(crate) struct NotSent(pub String);
impl std::fmt::Display for NotSent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; request not sent, retry on a later poll", self.0)
    }
}
impl std::error::Error for NotSent {}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pool {
    Futures,
    Spot,
    SapiIp,
    SapiAccount,
}
impl Pool {
    fn limit(self) -> u32 {
        // Leave capacity for other retrieval clients on the same dedicated IP.
        match self {
            Self::Futures => 1200,
            Self::Spot => 4500,
            Self::SapiIp => 4500,
            Self::SapiAccount => 6000,
        }
    }
    fn header(self) -> &'static str {
        match self {
            Self::Futures | Self::Spot => "x-mbx-used-weight-1m",
            Self::SapiIp => "x-sapi-used-ip-weight-1m",
            Self::SapiAccount => "x-sapi-used-uid-weight-1m",
        }
    }
}

fn cost(path: &str) -> Result<(Pool, u32)> {
    use Pool::*;
    Ok(match path {
        "/api/v3/account" => (Spot, 20),
        "/api/v3/ticker/price" => (Spot, 2),
        "/fapi/v2/account" => (Futures, 5),
        "/fapi/v1/feeBurn" => (Futures, 1),
        "/fapi/v1/convert/exchangeInfo" => (Futures, 20),
        "/fapi/v1/convert/getQuote" | "/fapi/v1/convert/orderStatus" => (Futures, 50),
        "/fapi/v1/convert/acceptQuote" => (Futures, 200),
        "/sapi/v1/convert/exchangeInfo" => (SapiIp, 3000),
        "/sapi/v1/convert/getQuote" => (SapiAccount, 200),
        "/sapi/v1/convert/acceptQuote" => (SapiAccount, 500),
        "/sapi/v1/convert/orderStatus" => (SapiAccount, 100),
        "/sapi/v1/asset/transfer" => (SapiAccount, 900),
        "/sapi/v1/bfusd/quota" | "/sapi/v1/bfusd/subscribe" => (SapiIp, 150),
        "/sapi/v1/simple-earn/flexible/position"
        | "/sapi/v1/simple-earn/flexible/list"
        | "/sapi/v1/simple-earn/flexible/personalLeftQuota" => (SapiIp, 150),
        "/sapi/v1/simple-earn/flexible/subscribe" | "/sapi/v1/simple-earn/flexible/redeem" => {
            (SapiIp, 1)
        }
        _ => bail!("asset-management endpoint has no configured request weight: {path}"),
    })
}

#[derive(Default)]
struct Window {
    reservations: VecDeque<(Instant, u32)>,
    observed: Option<(Instant, u32)>,
}
impl Window {
    fn used(&mut self, now: Instant) -> u32 {
        let minute = Duration::from_secs(60);
        self.reservations
            .retain(|(at, _)| now.duration_since(*at) < minute);
        if self
            .observed
            .is_some_and(|(at, _)| now.duration_since(at) >= minute)
        {
            self.observed = None;
        }
        let local: u32 = self.reservations.iter().map(|(_, w)| w).sum();
        let external = self.observed.map_or(0, |(at, used)| {
            used.saturating_add(
                self.reservations
                    .iter()
                    .filter(|(t, _)| *t > at)
                    .map(|(_, w)| w)
                    .sum(),
            )
        });
        local.max(external)
    }
}
#[derive(Default)]
struct State {
    windows: BTreeMap<Pool, Window>,
    cooldown: Option<Instant>,
    last_earn_write: Option<Instant>,
    futures_quotes: VecDeque<Instant>,
}
impl State {
    fn ready(&self, now: Instant) -> Result<()> {
        if self.cooldown.is_some_and(|at| at > now) {
            return Err(
                NotSent("Binance egress is cooling down after rate limiting".into()).into(),
            );
        }
        Ok(())
    }
    fn reserve(&mut self, method: &Method, path: &str, now: Instant) -> Result<()> {
        self.ready(now)?;
        let (pool, weight) = cost(path)?;
        let is_quote = path == "/fapi/v1/convert/getQuote";
        self.futures_quotes
            .retain(|at| now.duration_since(*at) < Duration::from_secs(86400));
        if is_quote
            && (self.futures_quotes.len() >= 500
                || self
                    .futures_quotes
                    .iter()
                    .filter(|at| now.duration_since(**at) < Duration::from_secs(3600))
                    .count()
                    >= 360)
        {
            return Err(NotSent("USD-M Convert quote hour/day budget exhausted".into()).into());
        }
        let earn_write = *method == Method::POST && path.starts_with("/sapi/v1/simple-earn/");
        if earn_write
            && self
                .last_earn_write
                .is_some_and(|at| now.duration_since(at) < Duration::from_secs(3))
        {
            return Err(NotSent("Simple Earn write cooldown is active".into()).into());
        }
        // Keep room to accept a ten-second quote without a rate-limit wait.
        let headroom = match path {
            "/fapi/v1/convert/getQuote" => 200,
            "/sapi/v1/convert/getQuote" => 500,
            _ => 0,
        };
        let window = self.windows.entry(pool).or_default();
        if window
            .used(now)
            .saturating_add(weight)
            .saturating_add(headroom)
            > pool.limit()
        {
            return Err(
                NotSent("Binance egress rolling-minute weight budget exhausted".into()).into(),
            );
        }
        window.reservations.push_back((now, weight));
        if is_quote {
            self.futures_quotes.push_back(now);
        }
        if earn_write {
            self.last_earn_write = Some(now);
        }
        Ok(())
    }
    fn observe(
        &mut self,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        now: Instant,
    ) -> Result<()> {
        let (pool, _) = cost(path)?;
        if let Some(weight) = headers
            .get(pool.header())
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
        {
            let window = self.windows.entry(pool).or_default();
            // Out-of-order responses and exchange minute resets cannot reduce
            // the observed usage before our conservative sixty-second expiry.
            let old = window
                .observed
                .filter(|(at, _)| now.duration_since(*at) < Duration::from_secs(60))
                .map_or(0, |(_, w)| w);
            window.observed = Some((now, weight.max(old)));
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 418 {
            let default = if status.as_u16() == 418 { 180 } else { 60 };
            let seconds = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(default)
                .max(1);
            let until = now
                .checked_add(Duration::from_secs(seconds))
                .unwrap_or(now + Duration::from_secs(86400 * 3));
            self.cooldown = Some(self.cooldown.map_or(until, |old| old.max(until)));
        }
        Ok(())
    }
}

type Registry = BTreeMap<(IpAddr, String), Arc<Mutex<State>>>;
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct Limiter(Arc<Mutex<State>>);
impl Limiter {
    pub fn shared(ip: IpAddr, origin: &str) -> Result<Self> {
        let origin = reqwest::Url::parse(origin)?.origin().ascii_serialization();
        let mut registry = REGISTRY
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate-limit registry unavailable"))?;
        Ok(Self(registry.entry((ip, origin)).or_default().clone()))
    }
    pub fn reserve(&self, method: &Method, path: &str) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate limiter unavailable"))?
            .reserve(method, path, Instant::now())
    }
    pub fn ready(&self) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate limiter unavailable"))?
            .ready(Instant::now())
    }
    pub fn observe(&self, path: &str, response: &reqwest::Response) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate limiter unavailable"))?
            .observe(path, response.status(), response.headers(), Instant::now())
            .context("failed to observe Binance rate limits")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_discovery_has_a_shared_rolling_budget() {
        let ip = "127.0.0.2".parse().unwrap();
        let first = Limiter::shared(ip, "http://fixture-rate-limit/").unwrap();
        let second = Limiter::shared(ip, "http://fixture-rate-limit/another").unwrap();
        assert!(Arc::ptr_eq(&first.0, &second.0));
        let mut state = first.0.lock().unwrap();
        let now = Instant::now();
        state
            .reserve(&Method::GET, "/sapi/v1/convert/exchangeInfo", now)
            .unwrap();
        assert!(
            state
                .reserve(
                    &Method::GET,
                    "/sapi/v1/convert/exchangeInfo",
                    now + Duration::from_secs(59)
                )
                .is_err()
        );
        state
            .reserve(
                &Method::GET,
                "/sapi/v1/convert/exchangeInfo",
                now + Duration::from_secs(60),
            )
            .unwrap();
    }

    #[test]
    fn throttling_cools_every_pool_and_headers_account_for_external_usage() {
        let mut state = State::default();
        let now = Instant::now();
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "120".parse().unwrap());
        state
            .observe(
                "/api/v3/account",
                StatusCode::TOO_MANY_REQUESTS,
                &headers,
                now,
            )
            .unwrap();
        assert!(
            state
                .reserve(
                    &Method::GET,
                    "/sapi/v1/bfusd/quota",
                    now + Duration::from_secs(119)
                )
                .is_err()
        );
        state
            .reserve(
                &Method::GET,
                "/sapi/v1/bfusd/quota",
                now + Duration::from_secs(120),
            )
            .unwrap();
        headers.insert("x-mbx-used-weight-1m", "4500".parse().unwrap());
        state
            .observe("/api/v3/account", StatusCode::OK, &headers, now)
            .unwrap();
        assert!(state.reserve(&Method::GET, "/api/v3/account", now).is_err());
        state
            .reserve(
                &Method::GET,
                "/api/v3/account",
                now + Duration::from_secs(120),
            )
            .unwrap();
    }

    #[test]
    fn quotes_respect_long_windows_and_have_acceptance_headroom() {
        let mut state = State::default();
        let now = Instant::now();
        state
            .windows
            .entry(Pool::Futures)
            .or_default()
            .reservations
            .push_back((now, 1000));
        assert!(
            state
                .reserve(&Method::POST, "/fapi/v1/convert/getQuote", now)
                .is_err()
        );
        state.windows.clear();
        state.futures_quotes.extend(std::iter::repeat_n(now, 360));
        assert!(
            state
                .reserve(&Method::POST, "/fapi/v1/convert/getQuote", now)
                .is_err()
        );
        state
            .reserve(
                &Method::POST,
                "/fapi/v1/convert/getQuote",
                now + Duration::from_secs(3600),
            )
            .unwrap();
        state.futures_quotes.extend(std::iter::repeat_n(now, 139));
        assert!(
            state
                .reserve(
                    &Method::POST,
                    "/fapi/v1/convert/getQuote",
                    now + Duration::from_secs(3601)
                )
                .is_err()
        );
        state
            .reserve(
                &Method::POST,
                "/fapi/v1/convert/getQuote",
                now + Duration::from_secs(86401),
            )
            .unwrap();
    }
}
