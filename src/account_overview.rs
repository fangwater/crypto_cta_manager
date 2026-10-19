//! Account observations for the monitoring table. These are exchange equity and
//! collateral facts, independent of reconstructed order NAV and strategy FIFO.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use chrono::NaiveDate;
use rocksdb::{Direction, IteratorMode};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::account_ipc::LiveEquityHub;
use crate::config::SourceConfig;
use crate::manager_db::ManagerDb;
use crate::viz_snapshot::VizSnapshotClient;

mod margin;

const CF: &str = "account_observations";
const DAY_MS: i64 = 86_400_000;
const SHANGHAI_MS: i64 = 8 * 3_600_000;
const SAMPLE_FRESH_MS: i64 = 45_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AccountObservation {
    pub source_id: String,
    pub observed_at_ms: i64,
    pub equity_at_ms: i64,
    pub equity: f64,
    pub long_position: Option<f64>,
    pub short_position: Option<f64>,
    pub position_at_ms: Option<i64>,
    pub initial_margin: Option<f64>,
    pub margin_at_ms: Option<i64>,
}

impl AccountObservation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.source_id.is_empty() && self.source_id.len() <= u16::MAX as usize,
            "invalid observation source"
        );
        ensure!(
            self.observed_at_ms > 0 && self.equity_at_ms > 0,
            "invalid observation timestamps"
        );
        ensure!(self.equity.is_finite(), "non-finite account equity");
        for value in [self.long_position, self.short_position, self.initial_margin]
            .into_iter()
            .flatten()
        {
            ensure!(
                value.is_finite() && value >= 0.0,
                "invalid account observation amount"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverviewAlertSettings {
    pub long_leverage_limit: Option<f64>,
    pub short_leverage_limit: Option<f64>,
    pub timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OverviewRow {
    pub source_id: String,
    pub account: String,
    pub venue: String,
    pub yesterday_equity: Option<f64>,
    pub equity: Option<f64>,
    pub today_profit: Option<f64>,
    pub profit_percent: Option<f64>,
    pub long_position: Option<f64>,
    pub short_position: Option<f64>,
    pub long_leverage: Option<f64>,
    pub short_leverage: Option<f64>,
    pub net_position: Option<f64>,
    pub max_drawdown_15d_percent: Option<f64>,
    pub margin: Option<f64>,
    pub margin_percent: Option<f64>,
    pub date_ms: Option<i64>,
    pub queried_at_ms: i64,
    pub leverage_alert: Option<bool>,
    pub timeout_alert: Option<bool>,
    pub history_start_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OverviewResponse {
    pub accounts: Vec<OverviewRow>,
    pub dates: Vec<String>,
    pub generated_at_ms: i64,
    pub alerts: OverviewAlertSettings,
    pub loading: bool,
}

#[derive(Default)]
struct State {
    latest: BTreeMap<String, AccountObservation>,
    history: BTreeMap<String, Vec<AccountObservation>>,
    loading: bool,
}

#[derive(Clone)]
pub struct AccountOverview {
    db: ManagerDb,
    state: Arc<RwLock<State>>,
    write_lock: Arc<Mutex<()>>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn day_start(ts_ms: i64) -> i64 {
    (ts_ms + SHANGHAI_MS).div_euclid(DAY_MS) * DAY_MS - SHANGHAI_MS
}

pub fn selected_day(date: &str) -> Result<i64> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").context("date must be YYYY-MM-DD")?;
    ensure!(
        day.format("%Y-%m-%d").to_string() == date,
        "date must be YYYY-MM-DD"
    );
    Ok(day
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
        - SHANGHAI_MS)
}

fn date_label(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms + SHANGHAI_MS)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn source_prefix(source: &str) -> Vec<u8> {
    let mut key = (source.len() as u16).to_be_bytes().to_vec();
    key.extend_from_slice(source.as_bytes());
    key
}

fn record_key(source: &str, timestamp: i64) -> Vec<u8> {
    let mut key = source_prefix(source);
    key.extend_from_slice(&timestamp.to_be_bytes());
    key
}

impl AccountOverview {
    pub fn open(db: ManagerDb) -> Result<Self> {
        db.ensure_column_family(CF)?;
        Ok(Self {
            db,
            write_lock: Arc::new(Mutex::new(())),
            state: Arc::new(RwLock::new(State {
                loading: true,
                ..Default::default()
            })),
        })
    }

    /// Persist explicit factual input. The source id must be chosen by the
    /// caller from the configured source catalog, never guessed from a label.
    fn append(&self, observation: &AccountObservation) -> Result<()> {
        observation.validate()?;
        let _write = self
            .write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("account observation write lock poisoned"))?;
        let cf = self
            .db
            .db()
            .cf_handle(CF)
            .context("missing account observations")?;
        let key = record_key(&observation.source_id, observation.observed_at_ms);
        let value = serde_json::to_vec(observation)?;
        if let Some(existing) = self.db.db().get_cf(&cf, &key)? {
            ensure!(
                existing == value,
                "conflicting immutable account observation"
            );
            return Ok(());
        }
        self.db.db().put_cf(&cf, &key, value)?;
        Ok(())
    }

    fn history(&self, source: &str, start: i64, end: i64) -> Result<Vec<AccountObservation>> {
        let cf = self
            .db
            .db()
            .cf_handle(CF)
            .context("missing account observations")?;
        let prefix = source_prefix(source);
        let key = record_key(source, start);
        let mut rows = Vec::new();
        for item in self
            .db
            .db()
            .iterator_cf(&cf, IteratorMode::From(&key, Direction::Forward))
        {
            let (key, value) = item?;
            if !key.starts_with(&prefix) {
                break;
            }
            let row: AccountObservation = serde_json::from_slice(&value)?;
            row.validate()?;
            ensure!(
                row.source_id == source && key.as_ref() == record_key(source, row.observed_at_ms),
                "account history identity mismatch"
            );
            if row.observed_at_ms > end {
                break;
            }
            rows.push(row);
        }
        Ok(rows)
    }

    pub fn spawn(&self, sources: Vec<SourceConfig>, live: LiveEquityHub, viz: VizSnapshotClient) {
        let hub = self.clone();
        let margin_reader = margin::MarginReader::default();
        tokio::spawn(async move {
            // Initialize each account once. Later samples increment the in-memory
            // window so browser polling never rescans weeks of RocksDB JSON.
            let initial = hub.clone();
            let source_ids = sources
                .iter()
                .filter(|source| source.enabled)
                .map(|source| source.id.clone())
                .collect::<Vec<_>>();
            let loaded = tokio::task::spawn_blocking(move || {
                let now = now_ms();
                for id in source_ids {
                    match initial.history(&id, day_start(now) - 30 * DAY_MS, now) {
                        Ok(rows) => {
                            if let Ok(mut state) = initial.state.write() {
                                state.history.insert(id, rows);
                            }
                        }
                        Err(error) => {
                            warn!(source_id = %id, "account equity history unavailable: {error}")
                        }
                    }
                }
            })
            .await;
            if let Err(error) = loaded {
                warn!("account history initialization failed: {error}");
            }
            let mut timer = tokio::time::interval(Duration::from_secs(15));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                for chunk in sources.chunks(4) {
                    let mut tasks = tokio::task::JoinSet::new();
                    for source in chunk.iter().filter(|source| source.enabled) {
                        let source = source.clone();
                        let live = live.clone();
                        let viz = viz.clone();
                        let margin_reader = margin_reader.clone();
                        tasks.spawn(async move {
                            let risk = if let Some(url) = &source.exec_viz_url {
                                viz.load_account_risk(url, &source.venue).await.ok()
                            } else {
                                None
                            };
                            let reading = live.get(&source.id)?;
                            let equity = reading.equity?;
                            let now = now_ms();
                            if equity.ts_ms <= 0
                                || equity.ts_ms > now
                                || !equity.equity_usdt.is_finite()
                            {
                                return None;
                            }
                            let ipc_margin = reading.initial_margin_ts_ms
                                .filter(|ts| (0..=SAMPLE_FRESH_MS).contains(&now.saturating_sub(*ts)))
                                .zip(reading.initial_margin_usd);
                            let margin = if let Some(value) = ipc_margin {
                                Some(value)
                            } else {
                                match margin_reader.get(&source).await {
                                    Ok(value) => value.map(|row| (row.ts_ms, row.initial_margin)),
                                    Err(_) => { warn!(source_id = %source.id, "read-only account margin query unavailable"); None }
                                }
                            };
                            let now = now_ms();
                            let fresh_risk = risk.filter(|risk| {
                                (0..=SAMPLE_FRESH_MS).contains(&now.saturating_sub(risk.ts_ms))
                            });
                            Some(AccountObservation {
                                source_id: source.id,
                                observed_at_ms: now,
                                equity_at_ms: equity.ts_ms,
                                equity: equity.equity_usdt,
                                long_position: fresh_risk.as_ref().map(|r| r.long_notional_usdt),
                                short_position: fresh_risk.as_ref().map(|r| r.short_notional_usdt),
                                position_at_ms: fresh_risk.map(|r| r.ts_ms),
                                initial_margin: margin.map(|(_, value)| value),
                                margin_at_ms: margin.map(|(ts, _)| ts),
                            })
                        });
                    }
                    while let Some(result) = tasks.join_next().await {
                        if let Ok(Some(row)) = result {
                            let hub = hub.clone();
                            let result = tokio::task::spawn_blocking(move || -> Result<()> {
                                if (0..=SAMPLE_FRESH_MS)
                                    .contains(&row.observed_at_ms.saturating_sub(row.equity_at_ms))
                                {
                                    hub.append(&row)?;
                                    let mut state = hub.state.write().map_err(|_| {
                                        anyhow::anyhow!("account overview lock poisoned")
                                    })?;
                                    let history =
                                        state.history.entry(row.source_id.clone()).or_default();
                                    // A clock adjustment must not scramble the factual sequence.
                                    let index = history.partition_point(|entry| {
                                        entry.observed_at_ms < row.observed_at_ms
                                    });
                                    if history.get(index) != Some(&row) {
                                        history.insert(index, row.clone());
                                    }
                                    let keep = history.partition_point(|entry| {
                                        entry.observed_at_ms
                                            < day_start(row.observed_at_ms) - 30 * DAY_MS
                                    });
                                    if keep > 0 {
                                        history.drain(..keep);
                                    }
                                }
                                hub.state
                                    .write()
                                    .map_err(|_| anyhow::anyhow!("account overview lock poisoned"))?
                                    .latest
                                    .insert(row.source_id.clone(), row);
                                Ok(())
                            })
                            .await;
                            if !matches!(result, Ok(Ok(()))) {
                                warn!("account overview sample failed: {result:?}");
                            }
                        }
                    }
                }
                if let Ok(mut state) = hub.state.write() {
                    state.loading = false;
                }
            }
        });
    }

    pub fn report(
        &self,
        sources: &[SourceConfig],
        selected: Option<i64>,
        alerts: &OverviewAlertSettings,
    ) -> Result<OverviewResponse> {
        let now = now_ms();
        let end = selected.map_or(now, |day| day + DAY_MS - 1);
        ensure!(
            end <= now || selected == Some(day_start(now)),
            "future date is unavailable"
        );
        let end = end.min(now);
        let state = self
            .state
            .read()
            .map_err(|_| anyhow::anyhow!("account overview lock poisoned"))?;
        let loading = state.loading;
        let mut dates = BTreeSet::new();
        let mut accounts = Vec::new();
        for source in sources {
            let history = state
                .history
                .get(&source.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for row in history {
                dates.insert(day_start(row.observed_at_ms));
            }
            let before_end = history.partition_point(|row| row.observed_at_ms <= end);
            let history = &history[..before_end];
            let current = if selected.is_none() {
                state.latest.get(&source.id).or_else(|| history.last())
            } else {
                history
                    .last()
                    .filter(|row| row.observed_at_ms >= day_start(end))
            };
            accounts.push(make_row(source, current, history, end, now, alerts));
        }
        Ok(OverviewResponse {
            accounts,
            dates: dates.into_iter().rev().map(date_label).collect(),
            generated_at_ms: now,
            alerts: alerts.clone(),
            loading,
        })
    }
}

fn make_row(
    source: &SourceConfig,
    current: Option<&AccountObservation>,
    history: &[AccountObservation],
    end: i64,
    queried_at: i64,
    alerts: &OverviewAlertSettings,
) -> OverviewRow {
    let start = day_start(end);
    let previous = history
        .iter()
        .rev()
        .find(|row| row.equity_at_ms < start && row.equity_at_ms >= start - DAY_MS);
    let yesterday = previous.map(|row| row.equity);
    let equity = current.map(|row| row.equity);
    let today_profit = current
        .filter(|row| row.equity_at_ms >= start)
        .zip(yesterday)
        .map(|(row, previous)| row.equity - previous);
    let ratio = |value: Option<f64>, denominator: Option<f64>| {
        value
            .zip(denominator)
            .filter(|(_, base)| *base > 0.0)
            .map(|(value, base)| value / base)
    };
    let fresh_equity = current
        .filter(|row| (0..=SAMPLE_FRESH_MS).contains(&end.saturating_sub(row.equity_at_ms)))
        .map(|row| row.equity);
    let fresh_position = current.filter(|row| {
        row.position_at_ms.is_some_and(|ts| {
            (0..=SAMPLE_FRESH_MS).contains(&row.observed_at_ms.saturating_sub(ts))
        })
    });
    let long = fresh_position.and_then(|row| row.long_position);
    let short = fresh_position.and_then(|row| row.short_position);
    let long_leverage = ratio(long, fresh_equity);
    let short_leverage = ratio(short, fresh_equity);
    let margin = current
        .filter(|row| {
            row.margin_at_ms.is_some_and(|ts| {
                (0..=SAMPLE_FRESH_MS).contains(&row.observed_at_ms.saturating_sub(ts))
            })
        })
        .and_then(|row| row.initial_margin);
    let leverage_alert = long_leverage
        .zip(alerts.long_leverage_limit)
        .zip(short_leverage.zip(alerts.short_leverage_limit))
        .map(|((long, long_limit), (short, short_limit))| long > long_limit || short > short_limit);
    let timeout_alert = alerts.timeout_secs.map(|seconds| {
        current.is_none_or(|row| {
            end.saturating_sub(row.equity_at_ms)
                > i64::try_from(seconds)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1000)
        })
    });
    let window_start = end - 15 * DAY_MS;
    let covered = history
        .first()
        .is_some_and(|row| row.equity_at_ms <= window_start);
    let mut observations = history
        .iter()
        .filter(|row| row.equity_at_ms >= window_start && row.equity_at_ms <= end)
        .map(|row| (row.equity_at_ms, row.equity))
        .collect::<BTreeMap<_, _>>();
    if let Some(row) =
        current.filter(|row| row.equity_at_ms >= window_start && row.equity_at_ms <= end)
    {
        observations.insert(row.equity_at_ms, row.equity);
    }
    let observed_days = observations
        .keys()
        .map(|ts| day_start(*ts))
        .collect::<BTreeSet<_>>();
    let complete_days =
        (0..=15).all(|day| observed_days.contains(&(day_start(end) - day * DAY_MS)));
    let max_drawdown = (covered && complete_days)
        .then(|| maximum_drawdown(observations.values().copied()))
        .flatten();
    OverviewRow {
        source_id: source.id.clone(),
        account: source.display_name().into(),
        venue: source.venue.clone(),
        yesterday_equity: yesterday,
        equity,
        today_profit,
        profit_percent: ratio(today_profit, yesterday).map(|value| value * 100.0),
        long_position: long,
        short_position: short,
        long_leverage,
        short_leverage,
        net_position: long.zip(short).map(|(long, short)| long - short),
        max_drawdown_15d_percent: max_drawdown.map(|value| value * 100.0),
        margin,
        margin_percent: ratio(margin, fresh_equity).map(|value| value * 100.0),
        date_ms: current.map(|row| row.equity_at_ms),
        queried_at_ms: queried_at,
        leverage_alert,
        timeout_alert,
        history_start_ms: history.first().map(|row| row.equity_at_ms),
    }
}

fn maximum_drawdown(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut peak: f64 = 0.0;
    let mut worst: f64 = 0.0;
    let mut seen = false;
    for value in values {
        if !value.is_finite() {
            return None;
        }
        peak = peak.max(value);
        if peak > 0.0 {
            worst = worst.max((peak - value) / peak);
            seen = true;
        }
    }
    seen.then_some(worst)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> SourceConfig {
        toml::from_str(
            r#"id = "one"
account = "example"
venue = "binance-futures"
rocksdb_path = "/unused"
"#,
        )
        .unwrap()
    }
    fn observation(ts: i64, equity: f64) -> AccountObservation {
        AccountObservation {
            source_id: "one".into(),
            observed_at_ms: ts,
            equity_at_ms: ts,
            equity,
            long_position: Some(412778.43),
            short_position: Some(5750.30),
            position_at_ms: Some(ts),
            initial_margin: Some(83613.10),
            margin_at_ms: Some(ts),
        }
    }
    #[test]
    fn daily_profit_and_risk_use_factual_equity_without_order_nav() {
        let now = selected_day("2026-10-19").unwrap() + 12 * 3_600_000;
        let yesterday = observation(day_start(now) - 1, 575109.94);
        let current = observation(now, 587316.20);
        let row = make_row(
            &source(),
            Some(&current),
            &[yesterday],
            now,
            now,
            &OverviewAlertSettings::default(),
        );
        assert!((row.today_profit.unwrap() - 12206.26).abs() < 1e-8);
        assert!((row.profit_percent.unwrap() - 2.1224220189969247).abs() < 1e-6);
        assert!((row.net_position.unwrap() - 407028.13).abs() < 1e-8);
        assert!((row.long_leverage.unwrap() - 412778.43 / 587316.20).abs() < 1e-12);
        assert!((row.margin_percent.unwrap() - 83613.10 / 587316.20 * 100.0).abs() < 1e-12);
        assert_eq!(row.leverage_alert, None);
        assert_eq!(row.timeout_alert, None);
        assert_eq!(row.max_drawdown_15d_percent, None);
    }
    #[test]
    fn missing_baselines_stale_equity_and_zero_margin_are_distinct() {
        let now = selected_day("2026-10-19").unwrap() + 12 * 3_600_000;
        let mut current = observation(now - 60_000, 100.0);
        current.initial_margin = Some(0.0);
        let settings = OverviewAlertSettings {
            long_leverage_limit: Some(2.0),
            short_leverage_limit: Some(2.0),
            timeout_secs: Some(45),
        };
        let row = make_row(&source(), Some(&current), &[], now, now, &settings);
        assert_eq!(row.yesterday_equity, None);
        assert_eq!(row.today_profit, None);
        assert_eq!(row.profit_percent, None);
        assert_eq!(row.long_leverage, None);
        assert_eq!(row.margin, Some(0.0));
        assert_eq!(row.timeout_alert, Some(true));
        let row = make_row(&source(), None, &[], now, now, &settings);
        assert_eq!(row.equity, None);
        assert_eq!(row.margin, None);
        assert_eq!(row.timeout_alert, Some(true));
    }
    #[test]
    fn drawdown_requires_fifteen_days_and_follows_time_order() {
        let now = selected_day("2026-10-19").unwrap();
        let mut history = (1..=15)
            .rev()
            .map(|day| observation(now - day * DAY_MS, 100.0))
            .collect::<Vec<_>>();
        history[13].equity = 120.0;
        history[14].equity = 90.0;
        let current = observation(now, 110.0);
        let row = make_row(
            &source(),
            Some(&current),
            &history,
            now,
            now,
            &OverviewAlertSettings::default(),
        );
        assert_eq!(row.max_drawdown_15d_percent, Some(25.0));
        let row = make_row(
            &source(),
            Some(&current),
            &history[1..],
            now,
            now,
            &OverviewAlertSettings::default(),
        );
        assert_eq!(row.max_drawdown_15d_percent, None);
    }
    #[test]
    fn historical_selection_preserves_dates_and_never_exposes_other_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = AccountOverview::open(ManagerDb::open(tmp.path()).unwrap()).unwrap();
        let now = now_ms();
        let today = day_start(now);
        let yesterday = observation(today - 1, 100.0);
        let current = observation(now, 110.0);
        {
            let mut state = hub.state.write().unwrap();
            state
                .history
                .insert("one".into(), vec![yesterday, current.clone()]);
            state.latest.insert("one".into(), current);
            state.history.insert(
                "hidden".into(),
                vec![observation(today - 2 * DAY_MS, 999.0)],
            );
        }
        let report = hub
            .report(
                &[source()],
                Some(today - DAY_MS),
                &OverviewAlertSettings::default(),
            )
            .unwrap();
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(report.accounts[0].equity, Some(100.0));
        assert_eq!(
            report.dates,
            vec![date_label(today), date_label(today - DAY_MS)]
        );
        let empty = hub
            .report(&[], None, &OverviewAlertSettings::default())
            .unwrap();
        assert!(empty.accounts.is_empty());
        assert!(empty.dates.is_empty());
        assert!(
            hub.report(
                &[source()],
                Some(today + DAY_MS),
                &OverviewAlertSettings::default()
            )
            .is_err()
        );
    }
    #[test]
    fn shanghai_date_boundaries_and_drawdown_order() {
        let start = selected_day("2026-10-19").unwrap();
        assert_eq!(day_start(start - 1), start - DAY_MS);
        assert_eq!(day_start(start + DAY_MS - 1), start);
        assert_eq!(date_label(start), "2026-10-19");
        assert!(selected_day("2026-2-1").is_err());
        assert_eq!(maximum_drawdown([100.0, 120.0, 90.0, 110.0]), Some(0.25));
        assert_eq!(maximum_drawdown([90.0, 100.0, 120.0]), Some(0.0));
        assert_eq!(maximum_drawdown([0.0, 0.0]), None);
    }
    #[test]
    fn observations_are_immutable_deduplicated_and_source_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let db = ManagerDb::open(tmp.path()).unwrap();
        let hub = AccountOverview::open(db.clone()).unwrap();
        let row = AccountObservation {
            source_id: "one".into(),
            observed_at_ms: 100,
            equity_at_ms: 100,
            equity: 100.0,
            long_position: Some(10.0),
            short_position: Some(2.0),
            position_at_ms: Some(100),
            initial_margin: Some(5.0),
            margin_at_ms: Some(100),
        };
        hub.append(&row).unwrap();
        hub.append(&row).unwrap();
        assert!(
            hub.append(&AccountObservation {
                equity: 200.0,
                ..row.clone()
            })
            .is_err()
        );
        hub.append(&AccountObservation {
            source_id: "two".into(),
            equity: 200.0,
            ..row.clone()
        })
        .unwrap();
        assert_eq!(hub.history("one", 0, 101).unwrap(), vec![row.clone()]);
        drop(hub);
        assert_eq!(
            AccountOverview::open(db)
                .unwrap()
                .history("one", 0, 101)
                .unwrap(),
            vec![row]
        );
    }
}
