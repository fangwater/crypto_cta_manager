//! Closed-minute valuation of factual FIFO positions. Execution prices and fees
//! are never replaced. Missing prices reject the whole minute-priced curve.
use super::*;
use crate::kline::{KlineStore, MINUTE_US, first_complete_open, now_us, warm_ranges};
use std::sync::Mutex;

pub const VALUATION: &str = "quantity_fifo_closed_1m_window_delta";
type Market = (String, i16);
pub(super) type Requests = BTreeMap<Market, BTreeSet<i64>>;
type Prices = BTreeMap<Market, BTreeMap<i64, f64>>;

pub(super) enum MinuteMarks {
    Collect(Mutex<Requests>),
    Loaded { revision: u64, prices: Prices },
}

impl MinuteMarks {
    pub(super) fn revision(&self) -> Option<u64> {
        match self {
            Self::Loaded { revision, .. } => Some(*revision),
            Self::Collect(_) => None,
        }
    }
    pub(super) fn collecting(&self) -> bool {
        matches!(self, Self::Collect(_))
    }
    fn price(&self, market: &Market, ts: i64) -> Result<Option<f64>> {
        anyhow::ensure!(matches!(market.1, 0 | 1), "该市场尚未接入分钟估值价");
        let open = ts.div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
        match self {
            Self::Collect(requests) => {
                requests
                    .lock()
                    .unwrap()
                    .entry(market.clone())
                    .or_default()
                    .insert(open);
                Ok(None)
            }
            Self::Loaded { prices, .. } => prices
                .get(market)
                .and_then(|bars| bars.get(&open))
                .copied()
                .map(Some)
                .with_context(|| format!("缺少 {} 在 {ts} 的完整分钟估值价", market.0)),
        }
    }
}

pub(super) fn apply(
    prices: Option<&MinuteMarks>,
    runtimes: &mut [TimelineSourceState],
    symbols: &BTreeSet<String>,
    ts: i64,
) -> Result<()> {
    let Some(prices) = prices else { return Ok(()) };
    for runtime in runtimes {
        let open = |state: &VenueState| {
            state.fifo.long_quantity(1.0).abs() > 1e-12
                || state.fifo.short_quantity(1.0).abs() > 1e-12
        };
        // A flat account can still have offsetting strategy positions. They need
        // the same market mark, including the account's unallocated residual.
        let markets = runtime
            .states
            .iter()
            .filter(|((symbol, _), state)| symbols.contains(symbol) && open(state))
            .map(|(market, _)| market.clone())
            .chain(
                runtime
                    .strategy_states
                    .iter()
                    .filter(|((_, symbol, _), state)| symbols.contains(symbol) && open(state))
                    .map(|((_, symbol, venue), _)| (symbol.clone(), *venue)),
            )
            .collect::<BTreeSet<_>>();
        for market in markets {
            if let Some(price) = prices.price(&market, ts)? {
                runtime.latest_marks.insert(market, price);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct PricingUnavailable {
    pub code: &'static str,
    pub error: String,
    pub loading: bool,
    pub available_from_us: i64,
}
impl std::fmt::Display for PricingUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error)
    }
}
impl std::error::Error for PricingUnavailable {}

fn unavailable(store: &KlineStore, loading: bool, error: impl Into<String>) -> anyhow::Error {
    PricingUnavailable {
        code: "nav_pricing_unavailable",
        error: error.into(),
        loading,
        available_from_us: first_complete_open(store.cutoff_us(now_us())) + MINUTE_US,
    }
    .into()
}

fn validate(store: &KlineStore, start: i64, end: i64) -> Result<()> {
    if !store.enabled() {
        return Err(unavailable(
            store,
            false,
            "实际净值需要已启用的分钟 K 线估值",
        ));
    }
    let now = now_us();
    let baseline_open = start.div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
    if baseline_open < store.cutoff_us(now) || end > now || end < start {
        return Err(unavailable(
            store,
            false,
            "分钟估值仅支持 K 线保留范围，起点需要前一个完整分钟",
        ));
    }
    Ok(())
}

fn read_prices(
    store: &KlineStore,
    requests: &Requests,
) -> Result<(Prices, BTreeMap<String, (i64, i64)>)> {
    let read = crate::analysis::run(|| {
        requests
            .par_iter()
            .map(|(market, wanted)| {
                let opens = wanted.iter().copied().collect::<Vec<_>>();
                let values = store.read_minutes(&market.0, &opens)?;
                let mut prices = BTreeMap::new();
                let mut missing: Option<(i64, i64)> = None;
                for (open, candle) in opens.into_iter().zip(values) {
                    if let Some(candle) = candle {
                        prices.insert(open, candle.close);
                    } else {
                        let range = missing.get_or_insert((open, open + MINUTE_US));
                        range.1 = open + MINUTE_US;
                    }
                }
                Ok((market.clone(), prices, missing))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let mut prices = Prices::new();
    let mut missing = BTreeMap::new();
    for (market, bars, range) in read {
        if let Some(range) = range {
            missing.insert(market.0.clone(), range);
        }
        prices.insert(market, bars);
    }
    Ok((prices, missing))
}

async fn load_prices(
    store: Arc<KlineStore>,
    requests: Arc<Requests>,
    start: i64,
    end: i64,
) -> Result<MinuteMarks> {
    validate(&store, start, end)?;
    let read_store = store.clone();
    let (requests, (mut prices, missing)) = tokio::task::spawn_blocking(move || {
        let result = read_prices(&read_store, &requests)?;
        Ok::<_, anyhow::Error>((requests, result))
    })
    .await??;
    if !missing.is_empty() {
        let warm = warm_ranges(&store, missing).await;
        validate(&store, start, end)?;
        let read_store = store.clone();
        let (loaded, missing) =
            tokio::task::spawn_blocking(move || read_prices(&read_store, &requests)).await??;
        if !missing.is_empty() {
            let reason = if warm.pending {
                "实际持仓的分钟估值价正在后台补齐，完成后自动重算".into()
            } else if warm.errors.is_empty() {
                format!(
                    "实际净值缺少 {} 个币种的分钟估值价，请稍后重试",
                    missing.len()
                )
            } else {
                format!("实际净值分钟估值价暂不可用：{}", warm.errors.join("; "))
            };
            return Err(unavailable(&store, warm.pending, reason));
        }
        prices = loaded;
    }
    Ok(MinuteMarks::Loaded {
        revision: store.revision(),
        prices,
    })
}

impl NavTimelineCache {
    /// Interactive history must remain readable beyond candle retention. Replay
    /// the entire window with local latest-fill marks in that case, never mix
    /// stale fill prices with minute marks within one curve. FIFO realization,
    /// quantities and fees are unchanged; floating PnL is only a trade-price
    /// estimate, identified by `quantity_fifo_window_delta` in the response.
    /// Daily equity reconstruction continues to use strict `rebuild_priced`.
    pub async fn rebuild_available(
        self: &Arc<Self>,
        mut request: NavTimelineRequest,
        allocation: bool,
        store: Arc<KlineStore>,
    ) -> Result<Arc<NavTimelineReport>> {
        let sources = select_sources(&self.config, &request.selected_source_ids)?;
        let start = request.resolved_start_ts_us(
            sources
                .iter()
                .filter_map(|s| self.start_ts_us(&s.id, request.end_ts_us, allocation))
                .min(),
        );
        request.start_ts_us = Some(start);
        let baseline_open = start.div_euclid(MINUTE_US) * MINUTE_US - MINUTE_US;
        let now = now_us();
        if baseline_open < store.cutoff_us(now)
            && request.end_ts_us >= start
            && request.end_ts_us <= now
        {
            let cache = self.clone();
            return tokio::task::spawn_blocking(move || cache.rebuild(request, allocation)).await?;
        }
        self.rebuild_priced(request, allocation, store).await
    }

    fn strategy_with_prices(
        &self,
        request: StrategyPnlRequest,
        prices: Option<&MinuteMarks>,
    ) -> Result<StrategyPnlReport> {
        rebuild_strategy_pnl_with_prices(
            &self.config,
            request,
            &self.snapshots,
            &self.strategy_snapshots,
            &self.histories,
            prices,
        )
    }

    pub async fn rebuild_strategy_pnl_priced(
        self: &Arc<Self>,
        request: StrategyPnlRequest,
        store: Arc<KlineStore>,
    ) -> Result<StrategyPnlReport> {
        if !self.uses_minute_marks(std::slice::from_ref(&request.source_id)) {
            let cache = self.clone();
            return tokio::task::spawn_blocking(move || cache.rebuild_strategy_pnl(request))
                .await?;
        }
        validate(&store, request.start_ts_us, request.end_ts_us)?;
        let cache = self.clone();
        let planned = request.clone();
        let requests = tokio::task::spawn_blocking(move || {
            let collector = MinuteMarks::Collect(Mutex::new(Requests::new()));
            cache.strategy_with_prices(planned, Some(&collector))?;
            let MinuteMarks::Collect(requests) = collector else {
                unreachable!()
            };
            Ok::<_, anyhow::Error>(requests.into_inner().unwrap())
        })
        .await??;
        let prices = load_prices(
            store,
            Arc::new(requests),
            request.start_ts_us,
            request.end_ts_us,
        )
        .await?;
        let cache = self.clone();
        tokio::task::spawn_blocking(move || cache.strategy_with_prices(request, Some(&prices)))
            .await?
    }

    pub fn uses_minute_marks(&self, sources: &[String]) -> bool {
        self.config
            .sources
            .iter()
            .filter(|s| s.enabled && (sources.is_empty() || sources.contains(&s.id)))
            .all(|s| matches!(s.venue.as_str(), "binance-futures" | "binance-margin"))
    }

    /// Used by HTTP/Arrow and daily equity reconstruction. Old order-only
    /// diagnostics remain explicitly identifiable by their valuation string.
    pub async fn rebuild_priced(
        self: &Arc<Self>,
        mut request: NavTimelineRequest,
        allocation: bool,
        store: Arc<KlineStore>,
    ) -> Result<Arc<NavTimelineReport>> {
        if !self.uses_minute_marks(&request.selected_source_ids) {
            let cache = self.clone();
            return tokio::task::spawn_blocking(move || cache.rebuild(request, allocation)).await?;
        }
        let sources = select_sources(&self.config, &request.selected_source_ids)?;
        let start = request.resolved_start_ts_us(
            sources
                .iter()
                .filter_map(|s| self.start_ts_us(&s.id, request.end_ts_us, allocation))
                .min(),
        );
        request.start_ts_us = Some(start);
        validate(&store, start, request.end_ts_us)?;
        request.selected_source_ids.sort();
        request.selected_source_ids.dedup();
        request.selected_symbols.sort();
        request.selected_symbols.dedup();
        let key = TimelineCacheKey {
            start: request.start_ts_us,
            end: request.end_ts_us,
            sources: request.selected_source_ids.clone(),
            symbols: request.selected_symbols.clone(),
            max_points: request.max_points,
            strategy_allocation: allocation,
            minute_marks_revision: Some(store.revision()),
        };
        if let Some(report) = self.results.get(&key) {
            return Ok(report);
        }
        let cache = self.clone();
        let plan_request = request.clone();
        let requests = tokio::task::spawn_blocking(move || {
            let plan_key = TimelineCacheKey {
                minute_marks_revision: None,
                ..key
            };
            cache.mark_plans.get_or_compute(plan_key, || {
                let collector = MinuteMarks::Collect(Mutex::new(Requests::new()));
                cache.rebuild_with_prices(plan_request, allocation, Some(&collector))?;
                let MinuteMarks::Collect(requests) = collector else {
                    unreachable!()
                };
                Ok(requests.into_inner().unwrap())
            })
        })
        .await??;
        let prices = load_prices(store, requests, start, request.end_ts_us).await?;
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            cache.rebuild_with_prices(request, allocation, Some(&prices))
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::KlineConfig, manager_db::ManagerDb};

    fn fixture() -> (tempfile::TempDir, ManagerDb, Arc<KlineStore>) {
        let dir = tempfile::tempdir().unwrap();
        let trading = dir.path().join("trade.toml");
        std::fs::write(&trading, "local_ips=['198.51.100.4']").unwrap();
        let db = ManagerDb::open(&dir.path().join("db")).unwrap();
        let config = KlineConfig {
            enabled: true,
            local_ip: Some("198.51.100.50".parse().unwrap()),
            public_ip: Some("198.51.100.50".parse().unwrap()),
            trade_engine_configs: vec![trading],
            ..Default::default()
        };
        let store = Arc::new(KlineStore::from_db(db.clone(), config).unwrap());
        (dir, db, store)
    }

    fn candle(db: &ManagerDb, symbol: &str, open: i64, close: f64) {
        let mut key = vec![symbol.len() as u8];
        key.extend_from_slice(symbol.as_bytes());
        key.extend_from_slice(&open.to_be_bytes());
        let mut value = Vec::new();
        for number in [close, close, close, close, 0.0, 0.0] {
            value.extend_from_slice(&number.to_le_bytes());
        }
        value.extend_from_slice(&0u64.to_le_bytes());
        db.db()
            .put_cf(&db.db().cf_handle("klines_1m").unwrap(), key, value)
            .unwrap();
    }

    #[tokio::test]
    async fn clamped_short_history_uses_minute_marks_for_every_window() {
        let (_dir, db, store) = fixture();
        let end = now_us().div_euclid(MINUTE_US) * MINUTE_US;
        let first = end - 2 * MINUTE_US + 123;
        let mut cache =
            super::super::tests::minute_mark_fixture(vec![super::super::tests::event_at(
                first, first, "BTCUSDT", 1, 1, 100.0, 1.0,
            )]);
        cache.config.sources[0].venue = "binance-futures".into();
        let cache = Arc::new(cache);
        for open in [end - 3 * MINUTE_US, end - 2 * MINUTE_US, end - MINUTE_US] {
            candle(&db, "BTCUSDT", open, 120.0);
        }
        for days in [Some(1), Some(7), Some(30), Some(60), Some(90), None] {
            let request = NavTimelineRequest {
                start_ts_us: days.map(|days| end - days * crate::funding_rates::DAY_US),
                end_ts_us: end,
                selected_source_ids: vec!["trade01".into()],
                selected_symbols: vec![],
                max_points: 200,
            };
            for allocation in [false, true] {
                let report = cache
                    .rebuild_available(request.clone(), allocation, store.clone())
                    .await
                    .unwrap();
                assert_eq!(report.start_ts_us, first);
                assert_eq!(report.earliest_start_ts_us, first);
                assert_eq!(report.end_ts_us, end);
                assert_eq!(report.valuation, VALUATION);
                assert_eq!(report.summary.fill_count, 1);
                assert_eq!(report.summary.floating_pnl_quote, 20.0);
                assert_eq!(report.points.first().unwrap().totals, NavTotals::default());
                assert_eq!(report.points.last().unwrap().totals, report.summary);
                // Strict pricing also validates the effective range, including
                // ALL and raw starts older than candle retention.
                assert_eq!(
                    cache
                        .rebuild_priced(request.clone(), allocation, store.clone())
                        .await
                        .unwrap()
                        .as_ref(),
                    report.as_ref()
                );
            }
        }
        assert_eq!(store.status().requests, 0);
    }

    #[tokio::test]
    async fn completed_marks_use_cache_separate_cash_and_perps_and_never_read_future_close() {
        let (_dir, db, store) = fixture();
        let end = now_us().div_euclid(MINUTE_US) * MINUTE_US;
        let open = end - MINUTE_US;
        candle(&db, "BTC", open, 100.0);
        candle(&db, "BTCUSDT", open, 110.0);
        candle(&db, "BTCUSDT", end, 999.0); // unfinished minute must never be used
        let requests = Arc::new(BTreeMap::from([
            (("BTC".into(), 0), BTreeSet::from([open])),
            (("BTCUSDT".into(), 1), BTreeSet::from([open])),
        ]));
        let marks = load_prices(store.clone(), requests.clone(), end, end)
            .await
            .unwrap();
        assert_eq!(marks.price(&("BTC".into(), 0), end).unwrap(), Some(100.0));
        assert_eq!(
            marks.price(&("BTCUSDT".into(), 1), end + 1).unwrap(),
            Some(110.0)
        );
        load_prices(store.clone(), requests, end, end)
            .await
            .unwrap();
        assert_eq!(store.status().requests, 0);
        assert!(validate(&store, store.cutoff_us(now_us()), end).is_err());
        assert!(validate(&store, end, now_us() + MINUTE_US).is_err());
        assert_eq!(store.status().requests, 0);
    }

    #[test]
    fn missing_cached_marks_stay_missing_until_repaired() {
        let (_dir, db, store) = fixture();
        let open = now_us().div_euclid(MINUTE_US) * MINUTE_US - 2 * MINUTE_US;
        let requests = BTreeMap::from([(("BTCUSDT".into(), 1), BTreeSet::from([open]))]);
        let (prices, missing) = read_prices(&store, &requests).unwrap();
        assert!(prices[&("BTCUSDT".into(), 1)].is_empty());
        assert_eq!(missing["BTCUSDT"], (open, open + MINUTE_US));
        candle(&db, "BTCUSDT", open, 120.0);
        let (prices, missing) = read_prices(&store, &requests).unwrap();
        assert!(missing.is_empty());
        assert_eq!(prices[&("BTCUSDT".into(), 1)][&open], 120.0);
    }
}
