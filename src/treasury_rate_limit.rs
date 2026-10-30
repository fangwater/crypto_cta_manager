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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pool {
    Futures,
    Spot,
    SapiIp,
    SapiAccount,
}
impl Pool {
    fn limit(self) -> u32 {
        // Include observed use by other clients and leave exchange headroom.
        match self {
            Self::Futures => 1800,
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

fn cost(method: &Method, path: &str) -> Result<(Pool, u32)> {
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
        "/sapi/v1/asset/transfer" if *method == Method::GET => (SapiIp, 1),
        "/sapi/v1/asset/transfer" => (SapiAccount, 900),
        "/sapi/v1/bfusd/quota" | "/sapi/v1/bfusd/subscribe" => (SapiIp, 150),
        "/sapi/v1/simple-earn/flexible/position"
        | "/sapi/v1/simple-earn/flexible/list"
        | "/sapi/v1/simple-earn/flexible/personalLeftQuota" => (SapiIp, 150),
        "/sapi/v1/simple-earn/flexible/history/redemptionRecord"
        | "/sapi/v1/simple-earn/flexible/history/subscriptionRecord" => (SapiIp, 150),
        "/sapi/v1/simple-earn/flexible/subscribe" | "/sapi/v1/simple-earn/flexible/redeem" => {
            (SapiIp, 1)
        }
        _ => bail!("asset-management endpoint has no configured request weight: {path}"),
    })
}

#[derive(Default)]
struct Window {
    reservations: VecDeque<(Instant, u32)>,
    observations: VecDeque<(Instant, u32)>,
}
impl Window {
    fn used(&mut self, now: Instant) -> u32 {
        let minute = Duration::from_secs(60);
        self.reservations
            .retain(|(at, _)| now.duration_since(*at) < minute);
        self.observations
            .retain(|(at, _)| now.duration_since(*at) < minute);
        let local: u32 = self.reservations.iter().map(|(_, w)| w).sum();
        let external = self
            .observations
            .iter()
            .map(|(at, used)| {
                used.saturating_add(
                    self.reservations
                        .iter()
                        .filter(|(t, _)| t > at)
                        .map(|(_, w)| w)
                        .sum(),
                )
            })
            .max()
            .unwrap_or(0);
        local.max(external)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capacity {
    pub remaining: u32,
    pub limit: u32,
}
impl Capacity {
    pub fn score(self) -> u32 {
        self.remaining.saturating_mul(10_000) / self.limit
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
        let (pool, weight) = cost(method, path)?;
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
        let used = window.used(now);
        if used.saturating_add(weight).saturating_add(headroom) > pool.limit() {
            return Err(NotSent(format!(
                "Binance egress rolling-minute weight budget exhausted: {method} {path}, pool={pool:?}, used={used}, weight={weight}, headroom={headroom}, limit={}",
                pool.limit()
            )).into());
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
        method: &Method,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        now: Instant,
    ) -> Result<()> {
        let (pool, _) = cost(method, path)?;
        if let Some(weight) = headers
            .get(pool.header())
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
        {
            let window = self.windows.entry(pool).or_default();
            // Each observation expires independently. Lower/out-of-order
            // responses cannot refresh an older high-water mark indefinitely.
            window
                .observations
                .retain(|(at, _)| now.duration_since(*at) < Duration::from_secs(60));
            window.observations.push_back((now, weight));
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
    /// Snapshot for round selection; the actual request must still reserve.
    pub fn capacity(&self, method: &Method, path: &str) -> Result<Capacity> {
        let now = Instant::now();
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate limiter unavailable"))?;
        state.ready(now)?;
        let (pool, weight) = cost(method, path)?;
        let used = state.windows.entry(pool).or_default().used(now);
        if used.saturating_add(weight) > pool.limit() {
            return Err(NotSent(format!(
                "Binance egress has insufficient round capacity: pool={pool:?}, used={used}, limit={}",
                pool.limit()
            )).into());
        }
        Ok(Capacity {
            remaining: pool.limit().saturating_sub(used),
            limit: pool.limit(),
        })
    }
    pub fn ready(&self) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate limiter unavailable"))?
            .ready(Instant::now())
    }
    pub fn observe(&self, method: &Method, path: &str, response: &reqwest::Response) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("treasury rate limiter unavailable"))?
            .observe(
                method,
                path,
                response.status(),
                response.headers(),
                Instant::now(),
            )
            .context("failed to observe Binance rate limits")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn futures_budget_is_1800_per_ip_and_capacity_does_not_reserve() {
        let first = Limiter::shared(
            "127.0.0.10".parse().unwrap(),
            "http://capacity-limit.fixture",
        )
        .unwrap();
        let second = Limiter::shared(
            "127.0.0.11".parse().unwrap(),
            "http://capacity-limit.fixture",
        )
        .unwrap();
        for _ in 0..20 {
            assert_eq!(
                first
                    .capacity(&Method::GET, "/fapi/v2/account")
                    .unwrap()
                    .remaining,
                1800
            );
        }
        for _ in 0..9 {
            first
                .reserve(&Method::POST, "/fapi/v1/convert/acceptQuote")
                .unwrap();
        }
        let error = first.reserve(&Method::GET, "/fapi/v2/account").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("pool=Futures"));
        assert!(message.contains("limit=1800"));
        assert!(first.capacity(&Method::GET, "/fapi/v2/account").is_err());
        second.reserve(&Method::GET, "/fapi/v2/account").unwrap();
    }

    #[test]
    fn lower_headers_cannot_extend_old_high_usage_past_its_own_expiry() {
        let mut state = State::default();
        let now = Instant::now();
        let mut headers = HeaderMap::new();
        for (seconds, used) in [(0, "1799"), (30, "10"), (59, "11")] {
            headers.insert("x-mbx-used-weight-1m", used.parse().unwrap());
            state
                .observe(
                    &Method::GET,
                    "/fapi/v2/account",
                    StatusCode::OK,
                    &headers,
                    now + Duration::from_secs(seconds),
                )
                .unwrap();
        }
        assert!(
            state
                .reserve(
                    &Method::GET,
                    "/fapi/v2/account",
                    now + Duration::from_secs(59)
                )
                .is_err()
        );
        state
            .reserve(
                &Method::GET,
                "/fapi/v2/account",
                now + Duration::from_secs(60),
            )
            .unwrap();
        let window = state.windows.get_mut(&Pool::Futures).unwrap();
        assert_eq!(window.used(now + Duration::from_secs(60)), 16);
    }

    #[test]
    fn observations_keep_local_reservations_after_the_response_in_the_total() {
        let mut state = State::default();
        let now = Instant::now();
        let mut headers = HeaderMap::new();
        headers.insert("x-mbx-used-weight-1m", "1500".parse().unwrap());
        state
            .observe(
                &Method::GET,
                "/fapi/v2/account",
                StatusCode::OK,
                &headers,
                now,
            )
            .unwrap();
        state
            .reserve(
                &Method::POST,
                "/fapi/v1/convert/acceptQuote",
                now + Duration::from_secs(1),
            )
            .unwrap();
        headers.insert("x-mbx-used-weight-1m", "10".parse().unwrap());
        state
            .observe(
                &Method::GET,
                "/fapi/v2/account",
                StatusCode::OK,
                &headers,
                now + Duration::from_secs(2),
            )
            .unwrap();
        let window = state.windows.get_mut(&Pool::Futures).unwrap();
        assert_eq!(window.used(now + Duration::from_secs(3)), 1700);
        assert_eq!(window.used(now + Duration::from_secs(60)), 200);
        assert_eq!(window.used(now + Duration::from_secs(61)), 10);
    }

    #[test]
    fn transfer_history_uses_ip_read_budget_without_consuming_write_budget() {
        let mut state = State::default();
        let now = Instant::now();
        state
            .windows
            .entry(Pool::SapiAccount)
            .or_default()
            .reservations
            .push_back((now, 6000));
        assert!(
            state
                .reserve(&Method::POST, "/sapi/v1/asset/transfer", now)
                .is_err()
        );
        state
            .reserve(&Method::GET, "/sapi/v1/asset/transfer", now)
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-sapi-used-ip-weight-1m", "4500".parse().unwrap());
        state
            .observe(
                &Method::GET,
                "/sapi/v1/asset/transfer",
                StatusCode::OK,
                &headers,
                now,
            )
            .unwrap();
        assert!(
            state
                .reserve(&Method::GET, "/sapi/v1/asset/transfer", now)
                .is_err()
        );
    }

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
                &Method::GET,
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
            .observe(
                &Method::GET,
                "/api/v3/account",
                StatusCode::OK,
                &headers,
                now,
            )
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
            .push_back((now, 1600));
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
