//! Opt-in native Binance / RapidX funding income. Only Manager PostgreSQL is written.
mod rapidx;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;

use crate::config::{AppConfig, SourceConfig};
use crate::nav::{NavTimelineReport, NavTotals};

const DAY_US: i64 = 86_400_000_000;
const VENUE: &str = "binance-futures";

#[derive(Clone, Debug, Serialize)]
pub struct FundingSettings {
    pub enabled: bool,
    pub refresh_secs: u64,
    pub verified_at_us: Option<i64>,
    pub verified_local_ip: Option<String>,
    pub verified_public_ip: Option<String>,
}

impl Default for FundingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            refresh_secs: 300,
            verified_at_us: None,
            verified_local_ip: None,
            verified_public_ip: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveFundingSettings {
    pub enabled: bool,
    pub refresh_secs: u64,
    pub enable_token: Option<String>,
}

#[derive(Serialize)]
pub struct FundingSettingsResponse {
    pub source_id: String,
    #[serde(flatten)]
    pub settings: FundingSettings,
    pub local_ip: Option<std::net::IpAddr>,
    pub public_ip: Option<std::net::IpAddr>,
    pub unavailable_reason: Option<String>,
    pub summary: FundingSummary,
}

#[derive(Clone, Debug, Serialize)]
pub struct FundingRecord {
    pub source_id: String,
    pub venue: String,
    pub symbol: String,
    pub asset: String,
    pub ts_us: i64,
    pub income_quote: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct FundingSymbolStatus {
    pub source_id: String,
    pub symbol: String,
    pub start_ts_us: i64,
    pub synced_through_ts_us: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FundingSummary {
    pub enabled: bool,
    pub refresh_secs: u64,
    pub status: String,
    pub start_ts_us: Option<i64>,
    pub synced_through_ts_us: Option<i64>,
    pub last_success_ts_us: Option<i64>,
    pub last_error: Option<String>,
    pub record_count: usize,
    pub income_quote: f64,
    pub income_by_asset: BTreeMap<String, f64>,
    pub symbols: Vec<FundingSymbolStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FundingNavPoint {
    pub ts_us: i64,
    pub funding_income_quote: f64,
    pub total_nav_before_fee_quote: f64,
    pub total_nav_after_fee_quote: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FundingTimeline {
    pub summary: FundingSummary,
    pub total_nav_before_fee_quote: f64,
    pub total_nav_after_fee_quote: f64,
    pub points: Vec<FundingNavPoint>,
    pub records: Vec<FundingRecord>,
    pub records_sampled: bool,
}

#[derive(Default)]
struct AccountFunding {
    summary: FundingSummary,
    records: Arc<Vec<FundingRecord>>,
}

/// Shared, conservative funding-only budget: four requests (120 weight) per
/// rolling minute. Retry-After applies to every account in this hub.
#[derive(Default)]
struct RequestBudget {
    reservations: VecDeque<Instant>,
    cooldown: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct FundingHub {
    accounts: Arc<RwLock<BTreeMap<String, AccountFunding>>>,
    budget: Arc<Mutex<RequestBudget>>,
    rapidx_budget: Arc<Mutex<RequestBudget>>,
    controls: Arc<RwLock<BTreeMap<String, watch::Sender<FundingSettings>>>>,
}

pub fn enable_token_matches(provided: Option<&str>, expected: Option<&str>) -> bool {
    expected
        .filter(|value| !value.trim().is_empty())
        .is_some_and(|expected| {
            crate::auth::publish_token_matches(provided, &crate::auth::publish_token_hash(expected))
        })
}

impl FundingHub {
    pub fn new(config: &AppConfig) -> Self {
        let hub = Self::default();
        for source in config.sources.iter().filter(|s| s.enabled) {
            hub.accounts.write().unwrap().insert(
                source.id.clone(),
                AccountFunding {
                    summary: FundingSummary {
                        refresh_secs: 300,
                        status: "disabled".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            hub.controls.write().unwrap().insert(
                source.id.clone(),
                watch::channel(FundingSettings::default()).0,
            );
        }
        hub
    }

    pub fn settings(&self, source_id: &str) -> FundingSettings {
        self.controls
            .read()
            .unwrap()
            .get(source_id)
            .map(|sender| sender.borrow().clone())
            .unwrap_or_default()
    }

    pub fn subscribe(&self, source_id: &str) -> watch::Receiver<FundingSettings> {
        self.controls.read().unwrap()[source_id].subscribe()
    }

    fn apply_settings(&self, source_id: &str, settings: FundingSettings) {
        if let Some(account) = self.accounts.write().unwrap().get_mut(source_id) {
            account.summary.enabled = settings.enabled;
            account.summary.refresh_secs = settings.refresh_secs;
            account.summary.last_error = None;
            account.summary.status = if settings.enabled {
                "loading"
            } else {
                "disabled"
            }
            .into();
        }
        if let Some(sender) = self.controls.read().unwrap().get(source_id) {
            sender.send_replace(settings);
        }
    }

    pub async fn load(&self, pool: &PgPool, config: &AppConfig) -> Result<()> {
        for source in config.sources.iter().filter(|source| source.enabled) {
            let row = sqlx::query("SELECT funding_enabled,funding_refresh_secs,funding_verified_at_us,funding_verified_local_ip,funding_verified_public_ip FROM cta_order_sources WHERE source_id=$1")
                .bind(&source.id).fetch_one(pool).await.context("funding settings unavailable; apply explicit database maintenance")?;
            self.apply_settings(
                &source.id,
                FundingSettings {
                    enabled: row.try_get("funding_enabled")?,
                    refresh_secs: row.try_get::<i32, _>("funding_refresh_secs")? as u64,
                    verified_at_us: row.try_get("funding_verified_at_us")?,
                    verified_local_ip: row.try_get("funding_verified_local_ip")?,
                    verified_public_ip: row.try_get("funding_verified_public_ip")?,
                },
            );
            let start: Option<i64> = sqlx::query_scalar(
                "SELECT start_ts_us FROM cta_funding_accounts WHERE source_id=$1",
            )
            .bind(&source.id)
            .fetch_optional(pool)
            .await?;
            if let Some(start) = start {
                self.reload(pool, source, start, false).await?;
            }
        }
        Ok(())
    }

    pub fn configuration(
        &self,
        config: &AppConfig,
        source: &SourceConfig,
    ) -> FundingSettingsResponse {
        let unavailable_reason = if source.venue != VENUE {
            Some("目前只支持 Binance USD-M funding".into())
        } else {
            validate_funding_egress(config, source)
                .err()
                .map(|error| error.to_string())
        };
        FundingSettingsResponse {
            source_id: source.id.clone(),
            settings: self.settings(&source.id),
            local_ip: config.funding.local_ip,
            public_ip: config.funding.public_ip,
            unavailable_reason,
            summary: self.summary(&source.id, i64::MIN, i64::MAX, &[]),
        }
    }

    pub async fn save_settings(
        &self,
        pool: &PgPool,
        source_id: &str,
        settings: FundingSettings,
    ) -> Result<()> {
        ensure!(
            (60..=86400).contains(&settings.refresh_secs),
            "funding interval must be 60–86400 seconds"
        );
        ensure!(
            !settings.enabled
                || (settings.verified_at_us.is_some()
                    && settings.verified_local_ip.is_some()
                    && settings.verified_public_ip.is_some()),
            "funding requires successful fixed-IP and authenticated REST verification"
        );
        let result = sqlx::query("UPDATE cta_order_sources SET funding_enabled=$2,funding_refresh_secs=$3,funding_verified_at_us=$4,funding_verified_local_ip=$5,funding_verified_public_ip=$6,updated_at=now() WHERE source_id=$1")
            .bind(source_id).bind(settings.enabled).bind(settings.refresh_secs as i32)
            .bind(settings.verified_at_us).bind(&settings.verified_local_ip).bind(&settings.verified_public_ip)
            .execute(pool).await.context("cannot save funding settings")?;
        ensure!(
            result.rows_affected() == 1,
            "funding account is unavailable"
        );
        self.apply_settings(source_id, settings);
        Ok(())
    }

    pub async fn verify_enable(&self, config: &AppConfig, source: &SourceConfig) -> Result<()> {
        ensure!(
            source.enabled && source.venue == VENUE,
            "funding supports enabled Binance USD-M sources only"
        );
        let client = funding_http_client(config, source).await?;
        let now = crate::kline::now_us();
        self.verify_account(&client, source, now).await
    }

    async fn verify_account(&self, client: &Client, source: &SourceConfig, now: i64) -> Result<()> {
        if is_rapidx(source)? {
            self.reserve_rapidx().await;
            let response = rapidx::request_page(
                client,
                source,
                "BTCUSDT",
                (now - DAY_US) / 1000,
                now / 1000,
                1,
            )
            .await?;
            self.check_response(&response).await?;
            let value = response
                .json()
                .await
                .map_err(reqwest::Error::without_url)
                .context("funding account test returned invalid RapidX statement data")?;
            rapidx::parse_page(value, source, "BTCUSDT", now - DAY_US, now, 1, None)?;
            return Ok(());
        }
        self.reserve().await;
        let response = crate::exchange_leverage::binance_funding_page(
            client,
            source,
            "BTCUSDT",
            (now - DAY_US) / 1000,
            now / 1000,
            1,
        )
        .await
        .context("funding account authentication test failed")?;
        self.check_response(&response).await?;
        let _: Vec<IncomeRow> = response
            .json()
            .await
            .map_err(reqwest::Error::without_url)
            .context("funding account test returned invalid income data")?;
        Ok(())
    }

    async fn check_response(&self, response: &reqwest::Response) -> Result<()> {
        let status = response.status();
        if matches!(status.as_u16(), 429 | 418) {
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.parse::<u64>().ok())
                .unwrap_or(60)
                .clamp(1, 86400);
            let mut budget = self.budget.lock().await;
            let until = Instant::now() + Duration::from_secs(retry);
            budget.cooldown = Some(budget.cooldown.map_or(until, |old| old.max(until)));
            let mut rapidx = self.rapidx_budget.lock().await;
            rapidx.cooldown = Some(rapidx.cooldown.map_or(until, |old| old.max(until)));
        }
        ensure!(
            status.is_success(),
            "funding REST returned HTTP {}",
            status.as_u16()
        );
        Ok(())
    }

    async fn reserve(&self) {
        Self::reserve_budget(&self.budget, 4).await;
    }

    async fn reserve_rapidx(&self) {
        // Shared across the opted-in portfolios; leave headroom below RapidX's
        // 100 requests/minute statement limit for Exec's own statement polling.
        Self::reserve_budget(&self.rapidx_budget, 40).await;
    }

    async fn reserve_budget(shared: &Mutex<RequestBudget>, limit: usize) {
        loop {
            let delay = {
                let mut budget = shared.lock().await;
                let now = Instant::now();
                while budget
                    .reservations
                    .front()
                    .is_some_and(|ts| now.duration_since(*ts) >= Duration::from_secs(60))
                {
                    budget.reservations.pop_front();
                }
                if let Some(until) = budget.cooldown.filter(|ts| *ts > now) {
                    until - now
                } else if budget.reservations.len() >= limit {
                    budget.reservations[0] + Duration::from_secs(60) - now
                } else {
                    budget.reservations.push_back(now);
                    return;
                }
            };
            tokio::time::sleep(delay).await;
        }
    }

    pub fn failed(&self, source_id: &str, error: &anyhow::Error) {
        if let Some(account) = self.accounts.write().unwrap().get_mut(source_id) {
            account.summary.status = "error".into();
            // The top-level contexts never contain exchange bodies or signed URLs.
            account.summary.last_error = Some(error.to_string());
        }
    }

    /// One bounded seven-day increment per known symbol. Returns true while
    /// historical cursors need more increments. Persisted symbols stay active
    /// after positions close or contracts delist.
    pub async fn poll(
        &self,
        pool: &PgPool,
        config: &AppConfig,
        source: &SourceConfig,
        discovered: &BTreeMap<String, i64>,
    ) -> Result<bool> {
        ensure!(
            source.enabled && self.settings(&source.id).enabled,
            "funding collection is disabled"
        );
        let settings = self.settings(&source.id);
        ensure!(
            settings.verified_at_us.is_some()
                && settings.verified_local_ip == config.funding.local_ip.map(|ip| ip.to_string())
                && settings.verified_public_ip == config.funding.public_ip.map(|ip| ip.to_string()),
            "funding fixed-IP configuration changed; pause and re-enable after testing"
        );
        let now = crate::kline::now_us();
        let Some(start) = reconcile_order_history(pool, source, discovered).await? else {
            if let Some(account) = self.accounts.write().unwrap().get_mut(&source.id) {
                account.summary.status = "waiting_for_symbols".into();
            }
            return Ok(false);
        };
        self.reload(pool, source, start, false).await?;
        let client = funding_http_client(config, source).await?;
        let rows = sqlx::query("SELECT symbol,start_ts_us,synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1 AND venue=$2 ORDER BY synced_through_ts_us NULLS FIRST,symbol")
            .bind(&source.id).bind(VENUE).fetch_all(pool).await.context("cannot read funding cursors")?;
        let mut failures = Vec::new();
        for row in rows {
            let symbol: String = row.try_get("symbol")?;
            let symbol_start: i64 = row.try_get("start_ts_us")?;
            let cursor: Option<i64> = row.try_get("synced_through_ts_us")?;
            let (from, through) = sync_window(symbol_start, cursor, now);
            if through < from {
                continue;
            }
            let result = self
                .sync_symbol(pool, &client, source, &symbol, from, through)
                .await;
            if let Err(error) = result {
                failures.push(error.to_string());
            } else {
                // Long backfills must expose newly saved income and per-symbol
                // progress before the entire account pass finishes.
                self.reload(pool, source, start, false).await?;
            }
        }
        self.reload(pool, source, start, failures.is_empty())
            .await?;
        ensure!(
            failures.is_empty(),
            "funding symbol sync failed: {}",
            failures.join("; ")
        );
        Ok(self.accounts.read().unwrap()[&source.id].summary.status == "backfilling")
    }

    async fn sync_symbol(
        &self,
        pool: &PgPool,
        client: &Client,
        source: &SourceConfig,
        symbol: &str,
        from: i64,
        through: i64,
    ) -> Result<()> {
        if is_rapidx(source)? {
            return self
                .sync_rapidx_symbol(pool, client, source, symbol, from, through)
                .await;
        }
        // Binance only retains three months. Refuse to label an expired range as
        // successfully observed. 89 days is conservative across calendar months.
        ensure!(
            from >= crate::kline::now_us() - 89 * DAY_US,
            "funding cursor is outside Binance income retention; explicit observation-start maintenance is required"
        );
        let mut records = Vec::new();
        let mut ids = BTreeSet::new();
        for page in 1..=100 {
            self.reserve().await;
            let response = crate::exchange_leverage::binance_funding_page(
                client,
                source,
                symbol,
                (from + 999) / 1000,
                through / 1000,
                page,
            )
            .await
            .context("funding REST request failed")?;
            self.check_response(&response).await?;
            let rows: Vec<IncomeRow> = response
                .json()
                .await
                .map_err(reqwest::Error::without_url)
                .context("invalid funding REST response")?;
            let count = rows.len();
            ensure!(count <= 1000, "funding REST page exceeded its limit");
            for row in rows {
                let mut record = row.validate(symbol, from, through)?;
                record.record.source_id = source.id.clone();
                ensure!(
                    ids.insert(record.tran_id.clone()),
                    "funding pagination repeated a transaction; cursor retained"
                );
                records.push(record);
            }
            if count < 1000 {
                return persist_chunk(pool, &source.id, symbol, through, &records).await;
            }
        }
        anyhow::bail!("funding pagination limit reached; cursor retained")
    }

    async fn sync_rapidx_symbol(
        &self,
        pool: &PgPool,
        client: &Client,
        source: &SourceConfig,
        symbol: &str,
        from: i64,
        through: i64,
    ) -> Result<()> {
        ensure!(
            from >= crate::kline::now_us() - 89 * DAY_US,
            "funding cursor is outside RapidX statement retention; cursor retained"
        );
        let mut records = Vec::new();
        let mut ids = BTreeSet::new();
        let mut pagination = None;
        for page in 1..=100 {
            self.reserve_rapidx().await;
            let response = rapidx::request_page(
                client,
                source,
                symbol,
                (from + 999) / 1000,
                through / 1000,
                page,
            )
            .await?;
            self.check_response(&response).await?;
            let value = response
                .json()
                .await
                .map_err(reqwest::Error::without_url)
                .context("invalid RapidX funding response")?;
            let (pages, total, rows) =
                rapidx::parse_page(value, source, symbol, from, through, page, pagination)?;
            pagination = Some((pages, total));
            for record in rows {
                ensure!(
                    ids.insert(record.tran_id.clone()),
                    "RapidX funding repeated a statement ID; cursor retained"
                );
                records.push(record);
            }
            if page >= pages {
                ensure!(
                    records.len() == total,
                    "RapidX funding total changed; cursor retained"
                );
                return persist_chunk(pool, &source.id, symbol, through, &records).await;
            }
        }
        anyhow::bail!("RapidX funding pagination limit reached; cursor retained")
    }

    async fn reload(
        &self,
        pool: &PgPool,
        source: &SourceConfig,
        start: i64,
        success: bool,
    ) -> Result<()> {
        // Both rows and progress come from the same committed ledger view.
        let mut transaction = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await?;
        let symbols = sqlx::query("SELECT symbol,start_ts_us,synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1 AND venue=$2 ORDER BY symbol")
            .bind(&source.id).bind(VENUE).fetch_all(&mut *transaction).await?;
        let symbols = symbols
            .into_iter()
            .map(|row| {
                Ok(FundingSymbolStatus {
                    source_id: source.id.clone(),
                    symbol: row.try_get("symbol")?,
                    start_ts_us: row.try_get("start_ts_us")?,
                    synced_through_ts_us: row.try_get("synced_through_ts_us")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let rows = sqlx::query("SELECT symbol,asset,ts_us,income::double precision AS income_quote FROM cta_funding_income WHERE source_id=$1 AND venue=$2 ORDER BY ts_us,tran_id")
            .bind(&source.id).bind(VENUE).fetch_all(&mut *transaction).await?;
        let records = rows
            .into_iter()
            .map(|row| {
                Ok(FundingRecord {
                    source_id: source.id.clone(),
                    venue: VENUE.into(),
                    symbol: row.try_get("symbol")?,
                    asset: row.try_get("asset")?,
                    ts_us: row.try_get("ts_us")?,
                    income_quote: row.try_get("income_quote")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        transaction.commit().await?;
        let coverage = symbols
            .iter()
            .map(|s| s.synced_through_ts_us)
            .collect::<Option<Vec<_>>>()
            .and_then(|values| values.into_iter().min());
        let mut accounts = self.accounts.write().unwrap();
        let account = accounts
            .get_mut(&source.id)
            .context("funding account is disabled")?;
        account.records = Arc::new(records);
        account.summary.start_ts_us = Some(start);
        account.summary.synced_through_ts_us = coverage;
        account.summary.symbols = symbols;
        if !account.summary.enabled {
            account.summary.status = "disabled".into();
        } else if success {
            let finished_at = crate::kline::now_us();
            account.summary.last_success_ts_us = Some(finished_at);
            account.summary.last_error = None;
            account.summary.status = if account.summary.symbols.is_empty() {
                "waiting_for_symbols"
            } else if coverage.is_some_and(|ts| {
                ts >= finished_at - ((account.summary.refresh_secs + 60) as i64) * 1_000_000
            }) {
                "ok"
            } else {
                "backfilling"
            }
            .into();
        } else {
            account.summary.status = if account.summary.symbols.is_empty() {
                "waiting_for_symbols"
            } else {
                "backfilling"
            }
            .into();
        }
        Ok(())
    }

    pub fn summary(
        &self,
        source_id: &str,
        start: i64,
        end: i64,
        symbols: &[String],
    ) -> FundingSummary {
        let accounts = self.accounts.read().unwrap();
        let Some(account) = accounts.get(source_id) else {
            return FundingSummary {
                status: "disabled".into(),
                ..Default::default()
            };
        };
        let mut summary = account.summary.clone();
        for row in account.records.iter().filter(|r| {
            r.ts_us >= start
                && r.ts_us <= end
                && (symbols.is_empty() || symbols.contains(&r.symbol))
        }) {
            summary.record_count += 1;
            summary.income_quote += row.income_quote;
            *summary
                .income_by_asset
                .entry(row.asset.clone())
                .or_default() += row.income_quote;
        }
        summary
    }

    /// Funding is account-owned and never attributed to a named CTA strategy.
    pub fn timeline(&self, report: &NavTimelineReport) -> FundingTimeline {
        let mut summary = FundingSummary {
            status: "disabled".into(),
            ..Default::default()
        };
        let mut records = Vec::new();
        for source_id in &report.selected_source_ids {
            let item = self.summary(
                source_id,
                report.start_ts_us,
                report.end_ts_us,
                &report.selected_symbols,
            );
            if (item.enabled || item.start_ts_us.is_some()) && summary.start_ts_us.is_none() {
                summary = item;
            } else if item.enabled || item.start_ts_us.is_some() {
                summary.enabled |= item.enabled;
                summary.start_ts_us = summary
                    .start_ts_us
                    .zip(item.start_ts_us)
                    .map(|(a, b)| a.min(b));
                summary.synced_through_ts_us = summary
                    .synced_through_ts_us
                    .zip(item.synced_through_ts_us)
                    .map(|(a, b)| a.min(b));
                summary.last_success_ts_us = summary
                    .last_success_ts_us
                    .zip(item.last_success_ts_us)
                    .map(|(a, b)| a.min(b));
                summary.symbols.extend(item.symbols);
                if item.status == "error" || (summary.status != "error" && item.status != "ok") {
                    summary.status = item.status;
                }
                if let Some(error) = item.last_error {
                    summary.last_error = Some(error);
                }
            }
            let accounts = self.accounts.read().unwrap();
            if let Some(account) = accounts.get(source_id) {
                records.extend(
                    account
                        .records
                        .iter()
                        .filter(|r| {
                            r.ts_us >= report.start_ts_us
                                && r.ts_us <= report.end_ts_us
                                && report.selected_symbols.contains(&r.symbol)
                        })
                        .cloned(),
                );
            }
        }
        // HTTP NAV browsers select one account. The backend also supports an
        // explicitly scoped multi-source report; its sum uses the filtered records.
        summary.income_quote = records.iter().map(|r| r.income_quote).sum();
        summary.record_count = records.len();
        summary.income_by_asset.clear();
        for record in &records {
            *summary
                .income_by_asset
                .entry(record.asset.clone())
                .or_default() += record.income_quote;
        }
        records.sort_by_key(|r| r.ts_us);
        let points = merge_funding_points(report, &records);
        FundingTimeline {
            total_nav_before_fee_quote: report.summary.nav_change_before_fee_quote
                + summary.income_quote,
            total_nav_after_fee_quote: report.summary.nav_change_after_fee_quote
                + summary.income_quote,
            summary,
            points,
            records: records.iter().rev().take(100).cloned().collect(),
            records_sampled: records.len() > 100,
        }
    }
}

/// Account starts follow the earliest local order, independent of snapshots
/// and feature activation. Earlier history reopens only affected symbol cursors;
/// existing income is retained and REST replay is deduplicated by transaction ID.
async fn reconcile_order_history(
    pool: &PgPool,
    source: &SourceConfig,
    discovered: &BTreeMap<String, i64>,
) -> Result<Option<i64>> {
    let requested = discovered.values().copied().filter(|ts| *ts > 0).min();
    let mut transaction = pool.begin().await?;
    if let Some(requested) = requested {
        sqlx::query("INSERT INTO cta_funding_accounts(source_id,start_ts_us) VALUES($1,$2) ON CONFLICT(source_id) DO UPDATE SET start_ts_us=EXCLUDED.start_ts_us WHERE EXCLUDED.start_ts_us < cta_funding_accounts.start_ts_us")
            .bind(&source.id).bind(requested).execute(&mut *transaction).await
            .context("funding tables unavailable; apply explicit database maintenance")?;
    }
    let start: Option<i64> = sqlx::query_scalar(
        "SELECT start_ts_us FROM cta_funding_accounts WHERE source_id=$1 FOR UPDATE",
    )
    .bind(&source.id)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some(start) = start {
        for (symbol, first_order) in discovered.iter().filter(|(_, ts)| **ts > 0) {
            sqlx::query("INSERT INTO cta_funding_symbols(source_id,venue,symbol,start_ts_us) VALUES($1,$2,$3,$4) ON CONFLICT(source_id,venue,symbol) DO UPDATE SET start_ts_us=EXCLUDED.start_ts_us,synced_through_ts_us=NULL WHERE EXCLUDED.start_ts_us < cta_funding_symbols.start_ts_us")
                .bind(&source.id).bind(VENUE).bind(symbol).bind(start.max(*first_order))
                .execute(&mut *transaction).await.context("cannot register funding order history")?;
        }
    }
    transaction.commit().await?;
    Ok(start)
}

fn sync_window(start: i64, cursor: Option<i64>, now: i64) -> (i64, i64) {
    // Replay the latest 24 hours for exchange publication lag. Older backfills
    // continue without replay so every poll advances a complete bounded chunk.
    let from = cursor.map_or(start, |ts| {
        if ts >= now - 2 * DAY_US {
            start.max(ts - DAY_US)
        } else {
            ts + 1000
        }
    });
    let from = ((from + 999) / 1000) * 1000;
    let through = (from + 7 * DAY_US - 1000).min((now / 1000 - 60_000) * 1000);
    (from, through)
}

fn validate_funding_egress(config: &AppConfig, source: &SourceConfig) -> Result<()> {
    let local = config
        .funding
        .local_ip
        .context("funding.local_ip is required")?;
    let public = config
        .funding
        .public_ip
        .context("funding.public_ip is required")?;
    if is_rapidx(source)? {
        ensure!(
            config
                .funding
                .rapidx_trading_ip_sources
                .contains(&source.id),
            "RapidX funding requires an explicit source trading-IP exception"
        );
        ensure!(
            source.enabled && source.venue == VENUE,
            "unsupported RapidX funding source"
        );
        ensure!(
            local == crate::bfusd_auto::account_fixed_ip(source)?,
            "RapidX funding IP must equal this account's current fixed trading IP"
        );
        ensure!(
            !public.is_unspecified() && !public.is_loopback(),
            "invalid RapidX funding public IP"
        );
        return Ok(());
    }
    ensure!(
        !config
            .funding
            .rapidx_trading_ip_sources
            .contains(&source.id),
        "funding trading-IP exception cannot be used by a native account"
    );
    crate::bfusd_auto::validate_treasury_ip(source, local)
        .context("funding IP conflicts with account trading configuration")?;
    let mut egress = config.clone();
    egress.kline.enabled = true;
    egress.kline.local_ip = Some(local);
    egress.kline.public_ip = Some(public);
    egress.kline.trade_engine_configs = config.funding.trade_engine_configs.clone();
    egress.kline.forbidden_public_ips = config.funding.forbidden_public_ips.clone();
    crate::kline::validate_host_configs(&egress)
        .context("funding egress exclusions are incomplete or conflict with trading IPs")?;
    Ok(())
}

fn is_rapidx(source: &SourceConfig) -> Result<bool> {
    let values = crate::market_rules::source_values(source)?;
    Ok(crate::market_rules::execution_backend(&source.venue, &values)? == "ltp")
}

async fn funding_http_client(config: &AppConfig, source: &SourceConfig) -> Result<Client> {
    validate_funding_egress(config, source)?;
    let local = config
        .funding
        .local_ip
        .context("funding.local_ip is required")?;
    let public = config
        .funding
        .public_ip
        .context("funding.public_ip is required")?;
    let client = bound_http_client(local)?;
    verify_public_ip(&client, public, "https://api.ipify.org").await?;
    Ok(client)
}

async fn verify_public_ip(client: &Client, public: std::net::IpAddr, endpoint: &str) -> Result<()> {
    let observed = client
        .get(endpoint)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("cannot verify fixed funding public IP")?
        .error_for_status()
        .map_err(reqwest::Error::without_url)
        .context("funding egress verification failed")?
        .text()
        .await
        .context("cannot read funding egress verification")?;
    ensure!(
        observed.trim().parse::<std::net::IpAddr>().ok() == Some(public),
        "funding public IP verification mismatch; REST disabled"
    );
    Ok(())
}

fn bound_http_client(local: std::net::IpAddr) -> Result<Client> {
    Client::builder()
        .local_address(local)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .context("cannot build fixed-IP funding HTTP client")
}

#[derive(Deserialize)]
struct IncomeRow {
    symbol: String,
    asset: String,
    income: String,
    time: i64,
    #[serde(rename = "incomeType")]
    income_type: String,
    #[serde(rename = "tranId")]
    tran_id: serde_json::Value,
}

struct ValidIncome {
    record: FundingRecord,
    income: String,
    tran_id: String,
}

impl IncomeRow {
    fn validate(self, symbol: &str, start: i64, end: i64) -> Result<ValidIncome> {
        let income_quote: f64 = self
            .income
            .parse()
            .context("funding income is not a decimal")?;
        ensure!(income_quote.is_finite(), "funding income is non-finite");
        ensure!(
            self.income_type == "FUNDING_FEE" && self.symbol == symbol,
            "funding REST returned the wrong income type or symbol"
        );
        ensure!(
            matches!(self.asset.as_str(), "USDT" | "USDC") && symbol.ends_with(&self.asset),
            "funding income currency cannot be added to this symbol's quote PnL"
        );
        let ts_us = self
            .time
            .checked_mul(1000)
            .context("funding timestamp overflow")?;
        ensure!(
            ts_us >= start && ts_us <= end,
            "funding REST returned a timestamp outside the requested range"
        );
        let tran_id = self
            .tran_id
            .as_str()
            .map(str::to_owned)
            .or_else(|| self.tran_id.as_i64().map(|v| v.to_string()))
            .context("funding transaction ID missing")?;
        ensure!(!tran_id.is_empty(), "funding transaction ID is empty");
        Ok(ValidIncome {
            record: FundingRecord {
                source_id: String::new(),
                venue: VENUE.into(),
                symbol: self.symbol,
                asset: self.asset,
                ts_us,
                income_quote,
            },
            income: self.income,
            tran_id,
        })
    }
}

async fn persist_chunk(
    pool: &PgPool,
    source_id: &str,
    symbol: &str,
    through: i64,
    records: &[ValidIncome],
) -> Result<()> {
    let mut transaction = pool
        .begin()
        .await
        .context("cannot begin funding ledger write")?;
    for item in records {
        sqlx::query("INSERT INTO cta_funding_income(source_id,venue,tran_id,symbol,asset,ts_us,income) VALUES($1,$2,$3,$4,$5,$6,$7::text::numeric) ON CONFLICT DO NOTHING")
            .bind(source_id).bind(VENUE).bind(&item.tran_id).bind(symbol).bind(&item.record.asset)
            .bind(item.record.ts_us).bind(&item.income).execute(&mut *transaction).await.context("cannot store funding income")?;
    }
    sqlx::query("UPDATE cta_funding_symbols SET synced_through_ts_us=GREATEST(COALESCE(synced_through_ts_us,0),$4) WHERE source_id=$1 AND venue=$2 AND symbol=$3")
        .bind(source_id).bind(VENUE).bind(symbol).bind(through).execute(&mut *transaction).await.context("cannot advance funding cursor")?;
    transaction
        .commit()
        .await
        .context("cannot commit funding ledger and cursor")
}

fn merge_funding_points(
    report: &NavTimelineReport,
    records: &[FundingRecord],
) -> Vec<FundingNavPoint> {
    // Use the factual sampling timestamps. Never infer a factual FIFO value at
    // an omitted fill timestamp from a downsampled predecessor.
    let times: BTreeSet<i64> = report.points.iter().map(|p| p.ts_us).collect();
    let mut actual = NavTotals::default();
    let mut actual_index = 0;
    let mut record_index = 0;
    let mut income = 0.0;
    times
        .into_iter()
        .map(|ts_us| {
            while actual_index < report.points.len() && report.points[actual_index].ts_us <= ts_us {
                actual = report.points[actual_index].totals;
                actual_index += 1;
            }
            while record_index < records.len() && records[record_index].ts_us <= ts_us {
                income += records[record_index].income_quote;
                record_index += 1;
            }
            let baseline = ts_us == report.start_ts_us && report.end_ts_us > report.start_ts_us;
            let funding_income_quote = if baseline { 0.0 } else { income };
            FundingNavPoint {
                ts_us,
                funding_income_quote,
                total_nav_before_fee_quote: actual.nav_change_before_fee_quote
                    + funding_income_quote,
                total_nav_after_fee_quote: actual.nav_change_after_fee_quote + funding_income_quote,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enabling_requires_a_configured_matching_token() {
        let expected = Some("fixture-enable-token");
        assert!(!enable_token_matches(None, expected));
        assert!(!enable_token_matches(Some(""), expected));
        assert!(!enable_token_matches(Some("wrong"), expected));
        assert!(!enable_token_matches(Some("fixture-enable-token"), None));
        assert!(!enable_token_matches(
            Some("fixture-enable-token"),
            Some(" ")
        ));
        assert!(enable_token_matches(Some("fixture-enable-token"), expected));
    }

    #[tokio::test]
    async fn public_ip_test_requires_bound_egress_matching_response_and_http_success() -> Result<()>
    {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let server = std::thread::spawn(move || {
            for status in ["200 OK", "200 OK", "503 Unavailable"] {
                let (mut stream, peer) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                assert_eq!(peer.ip(), "127.0.0.2".parse::<std::net::IpAddr>().unwrap());
                let mut bytes = [0; 2048];
                stream.read(&mut bytes).unwrap();
                let body = "198.18.0.3";
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = bound_http_client("127.0.0.2".parse()?)?;
        let endpoint = format!("http://{address}/");
        verify_public_ip(&client, "198.18.0.3".parse()?, &endpoint).await?;
        assert!(
            verify_public_ip(&client, "198.18.0.4".parse()?, &endpoint)
                .await
                .is_err()
        );
        assert!(
            verify_public_ip(&client, "198.18.0.3".parse()?, &endpoint)
                .await
                .is_err()
        );
        server.join().unwrap();
        Ok(())
    }

    #[test]
    fn incremental_windows_advance_and_replay_only_recent_tail() {
        let now = 20 * DAY_US;
        assert_eq!(
            sync_window(DAY_US, Some(8 * DAY_US), now).0,
            8 * DAY_US + 1000
        );
        assert_eq!(
            sync_window(DAY_US, Some(now - 60_000_000), now).0,
            now - DAY_US - 60_000_000
        );
        assert_eq!(sync_window(now, None, now).0, now);
        assert!(sync_window(now, None, now).1 < now);
    }
    #[test]
    fn funding_preserves_signed_decimal_and_rejects_unrelated_income() {
        let value = serde_json::json!({"symbol":"BTCUSDT","asset":"USDT","income":"-0.0123456789012345","time":123,"incomeType":"FUNDING_FEE","tranId":987});
        let parsed: IncomeRow = serde_json::from_value(value.clone()).unwrap();
        let item = parsed.validate("BTCUSDT", 123000, 123000).unwrap();
        assert!(item.record.income_quote < 0.0);
        assert_eq!(item.income, "-0.0123456789012345");
        assert_eq!(item.tran_id, "987");
        for (key, bad) in [
            ("incomeType", "COMMISSION"),
            ("asset", "BTC"),
            ("symbol", "ETHUSDT"),
            ("income", "NaN"),
        ] {
            let mut invalid = value.clone();
            invalid[key] = bad.into();
            assert!(
                serde_json::from_value::<IncomeRow>(invalid)
                    .unwrap()
                    .validate("BTCUSDT", 123000, 123000)
                    .is_err()
            );
        }
    }

    #[test]
    fn funding_requires_fixed_egress_separate_from_every_trading_ip() {
        let directory = tempfile::tempdir().unwrap();
        let engine = directory.path().join("trade_engine.toml");
        std::fs::write(
            &engine,
            "local_ips = ['198.18.0.1']\nsecondary_local_ip = '198.18.0.2'\n",
        )
        .unwrap();
        let mut config =
            AppConfig::load(std::path::Path::new("config/cta-manager.example.toml")).unwrap();
        let mut source = config.sources.remove(0);
        source.env_path = Some(directory.path().join("env.sh"));
        std::fs::write(source.env_path(), "TRADE_ENGINE_EXEC_BACKEND=native\n").unwrap();
        source.rocksdb_path = directory.path().join("data/persist_manager");
        config.funding.local_ip = Some("198.18.0.3".parse().unwrap());
        config.funding.public_ip = config.funding.local_ip;
        config.funding.trade_engine_configs = vec![engine.clone()];
        config.sources = vec![source.clone()];
        validate_funding_egress(&config, &source).unwrap();
        for ip in ["0.0.0.0", "127.0.0.1", "198.18.0.1", "198.18.0.2"] {
            config.funding.local_ip = Some(ip.parse().unwrap());
            assert!(
                validate_funding_egress(&config, &source).is_err(),
                "accepted {ip}"
            );
        }
        config.funding.local_ip = Some("198.18.0.3".parse().unwrap());
        config.funding.public_ip = Some("198.18.0.2".parse().unwrap());
        assert!(validate_funding_egress(&config, &source).is_err());
        config.funding.public_ip = config.funding.local_ip;
        config.funding.forbidden_public_ips = vec![config.funding.public_ip.unwrap()];
        assert!(validate_funding_egress(&config, &source).is_err());
        config.funding.forbidden_public_ips.clear();
        config.funding.trade_engine_configs.clear();
        assert!(validate_funding_egress(&config, &source).is_err());
        config.funding.trade_engine_configs = vec![engine.clone()];
        std::fs::write(engine, "local_ips = ['10.0.0.1']").unwrap();
        assert!(validate_funding_egress(&config, &source).is_err());
    }

    #[test]
    fn rapidx_trading_ip_exception_requires_source_opt_in_backend_and_current_binding() {
        let directory = tempfile::tempdir().unwrap();
        let engine = directory.path().join("trade_engine.toml");
        std::fs::write(&engine, "local_ips=['198.18.0.1']\n").unwrap();
        let mut config =
            AppConfig::load(std::path::Path::new("config/cta-manager.example.toml")).unwrap();
        let mut source = config.sources.remove(0);
        source.env_path = Some(directory.path().join("exec.env"));
        source.rocksdb_path = directory.path().join("data/persist_manager");
        std::fs::write(
            source.env_path(),
            "TRADE_ENGINE_EXEC_BACKEND=rapidx\nLTP_PORTFOLIO_ID=123\n",
        )
        .unwrap();
        config.sources = vec![source.clone()];
        config.funding.local_ip = Some("198.18.0.1".parse().unwrap());
        config.funding.public_ip = Some("198.18.0.9".parse().unwrap());
        config.funding.trade_engine_configs = vec![engine.clone()];
        assert!(validate_funding_egress(&config, &source).is_err());
        config.funding.rapidx_trading_ip_sources = vec![source.id.clone()];
        config.validate().unwrap();
        validate_funding_egress(&config, &source).unwrap();
        std::fs::write(&engine, "local_ips=['198.18.0.2']\n").unwrap();
        assert!(validate_funding_egress(&config, &source).is_err());
        std::fs::write(&engine, "local_ips=['198.18.0.1']\n").unwrap();
        std::fs::write(source.env_path(), "TRADE_ENGINE_EXEC_BACKEND=native\n").unwrap();
        assert!(validate_funding_egress(&config, &source).is_err());
        config
            .funding
            .rapidx_trading_ip_sources
            .push("other-source".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn funding_boundaries_and_account_symbol_filters_preserve_order_nav() {
        let actual = NavTotals {
            nav_change_before_fee_quote: 10.0,
            nav_change_after_fee_quote: 8.0,
            ..Default::default()
        };
        let mut report = NavTimelineReport {
            valuation: "test",
            earliest_start_ts_us: 1000,
            start_ts_us: 1000,
            end_ts_us: 3000,
            selected_source_ids: vec!["a".into()],
            available_symbols: vec!["BTCUSDT".into()],
            selected_symbols: vec!["BTCUSDT".into()],
            available_strategies: vec![],
            summary: actual,
            symbols: vec![],
            points: vec![
                crate::nav::NavTimelinePoint {
                    ts_us: 1000,
                    ..Default::default()
                },
                crate::nav::NavTimelinePoint {
                    ts_us: 3000,
                    totals: actual,
                    ..Default::default()
                },
            ],
            symbol_points: vec![],
            strategy_points: vec![],
            sampled: true,
        };
        let record = |symbol: &str, ts_us, income_quote| FundingRecord {
            source_id: "a".into(),
            venue: VENUE.into(),
            symbol: symbol.into(),
            asset: "USDT".into(),
            ts_us,
            income_quote,
        };
        let hub = FundingHub::default();
        hub.accounts.write().unwrap().insert(
            "a".into(),
            AccountFunding {
                summary: FundingSummary {
                    enabled: true,
                    start_ts_us: Some(1000),
                    status: "ok".into(),
                    ..Default::default()
                },
                records: Arc::new(vec![
                    record("BTCUSDT", 999, 100.0),
                    record("BTCUSDT", 1000, -2.0),
                    record("ETHUSDT", 2000, 999.0),
                    record("BTCUSDT", 3000, 3.0),
                    record("BTCUSDT", 3001, 100.0),
                ]),
            },
        );
        hub.accounts.write().unwrap().insert(
            "b".into(),
            AccountFunding {
                records: Arc::new(vec![record("BTCUSDT", 2000, 1000.0)]),
                ..Default::default()
            },
        );
        let timeline = hub.timeline(&report);
        assert_eq!(timeline.summary.income_quote, 1.0);
        assert_eq!(timeline.summary.record_count, 2);
        assert_eq!(timeline.points[0].funding_income_quote, 0.0);
        assert_eq!(timeline.points[1].funding_income_quote, 1.0);
        assert_eq!(timeline.total_nav_before_fee_quote, 11.0);
        assert_eq!(timeline.total_nav_after_fee_quote, 9.0);
        {
            let mut accounts = hub.accounts.write().unwrap();
            let account = accounts.get_mut("a").unwrap();
            account.summary.enabled = false;
            account.summary.status = "disabled".into();
        }
        let paused = hub.timeline(&report);
        assert!(!paused.summary.enabled);
        assert_eq!(paused.summary.start_ts_us, Some(1000));
        assert_eq!(
            paused.total_nav_after_fee_quote,
            timeline.total_nav_after_fee_quote
        );
        assert_eq!(paused.points.len(), timeline.points.len());
        assert_eq!(report.summary, actual);
        report.end_ts_us = 1000;
        report.points.truncate(1);
        report.points[0].totals = actual;
        let terminal = hub.timeline(&report);
        assert_eq!(terminal.points[0].funding_income_quote, -2.0);
        assert_eq!(terminal.total_nav_after_fee_quote, 6.0);
    }

    #[tokio::test]
    #[ignore = "requires an isolated local PostgreSQL 16 test cluster"]
    async fn funding_ledger_retries_preserve_decimal_identity_and_atomic_cursor() -> Result<()> {
        use sqlx::AssertSqlSafe;
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        let socket = std::env::var("CTA_SCHEMA_TEST_SOCKET")?;
        ensure!(
            socket.starts_with("/tmp/cta-schema-test-")
                && std::path::Path::new(&socket)
                    .join(".s.PGSQL.15433")
                    .exists(),
            "only an isolated test PostgreSQL socket is allowed"
        );
        let options = PgConnectOptions::new()
            .host(&socket)
            .port(15433)
            .username(&std::env::var("USER")?)
            .database("postgres");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let name = format!(
            "cta_funding_test_{}_{}",
            std::process::id(),
            crate::kline::now_us()
        );
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.database(&name))
            .await?;
        crate::postgres::initialize(&pool).await?;
        let mut config = AppConfig::load(std::path::Path::new("config/cta-manager.example.toml"))?;
        config.sources.truncate(1);
        crate::postgres::register_sources(&pool, &config.sources).await?;
        let source = &config.sources[0];
        sqlx::query("INSERT INTO cta_funding_accounts VALUES($1,1000)")
            .bind(&source.id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO cta_funding_symbols(source_id,venue,symbol,start_ts_us) VALUES($1,$2,'BTCUSDT',1000)")
            .bind(&source.id).bind(VENUE).execute(&pool).await?;
        let item = || ValidIncome {
            tran_id: "large-stable-id".into(),
            income: "-0.0123456789012345".into(),
            record: FundingRecord {
                source_id: source.id.clone(),
                venue: VENUE.into(),
                symbol: "BTCUSDT".into(),
                asset: "USDT".into(),
                ts_us: 2000,
                income_quote: -0.0123456789012345,
            },
        };
        persist_chunk(&pool, &source.id, "BTCUSDT", 3000, &[item()]).await?;
        persist_chunk(&pool, &source.id, "BTCUSDT", 2000, &[item()]).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cta_funding_income")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT income::text FROM cta_funding_income")
                .fetch_one(&pool)
                .await?,
            "-0.0123456789012345"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT synced_through_ts_us FROM cta_funding_symbols")
                .fetch_one(&pool)
                .await?,
            3000
        );
        // A failed row and the proposed cursor advance must roll back together.
        let mut invalid = item();
        invalid.tran_id = "bad-asset".into();
        invalid.record.asset = "BTC".into();
        assert!(
            persist_chunk(&pool, &source.id, "BTCUSDT", 4000, &[invalid])
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT synced_through_ts_us FROM cta_funding_symbols")
                .fetch_one(&pool)
                .await?,
            3000
        );
        let hub = FundingHub::new(&config);
        hub.reload(&pool, source, 1000, true).await?;
        let restored = hub.summary(&source.id, 1000, 3000, &[]);
        assert_eq!(restored.record_count, 1);
        assert_eq!(restored.income_quote, -0.0123456789012345);
        assert!(!hub.summary("other-source", 1000, 3000, &[]).enabled);
        hub.failed(&source.id, &anyhow::anyhow!("fixture outage"));
        assert_eq!(
            hub.summary(&source.id, 1000, 3000, &[]).income_quote,
            restored.income_quote
        );
        let disabled = hub.settings(&source.id);
        assert!(!disabled.enabled);
        let mut unverified = disabled.clone();
        unverified.enabled = true;
        assert!(
            hub.save_settings(&pool, &source.id, unverified)
                .await
                .is_err()
        );
        assert!(
            !sqlx::query_scalar::<_, bool>(
                "SELECT funding_enabled FROM cta_order_sources WHERE source_id=$1"
            )
            .bind(&source.id)
            .fetch_one(&pool)
            .await?
        );
        let mut changes = hub.subscribe(&source.id);
        let mut verified = disabled.clone();
        verified.enabled = true;
        verified.refresh_secs = 600;
        verified.verified_at_us = Some(crate::kline::now_us());
        verified.verified_local_ip = Some("198.18.0.3".into());
        verified.verified_public_ip = verified.verified_local_ip.clone();
        hub.save_settings(&pool, &source.id, verified.clone())
            .await?;
        changes.changed().await?;
        assert_eq!(changes.borrow_and_update().refresh_secs, 600);
        let reloaded = FundingHub::new(&config);
        reloaded.load(&pool, &config).await?;
        assert!(reloaded.settings(&source.id).enabled);
        assert_eq!(reloaded.settings(&source.id).refresh_secs, 600);
        let mut invalid_interval = verified.clone();
        invalid_interval.refresh_secs = 59;
        assert!(
            hub.save_settings(&pool, &source.id, invalid_interval)
                .await
                .is_err()
        );
        verified.enabled = false;
        verified.refresh_secs = 900;
        hub.save_settings(&pool, &source.id, verified).await?;
        changes.changed().await?;
        assert!(!changes.borrow_and_update().enabled);
        assert_eq!(
            hub.summary(&source.id, 1000, 3000, &[]).income_quote,
            restored.income_quote
        );
        let paused = FundingHub::new(&config);
        paused.load(&pool, &config).await?;
        assert!(!paused.settings(&source.id).enabled);
        assert_eq!(paused.settings(&source.id).refresh_secs, 900);
        assert_eq!(
            paused.summary(&source.id, 1000, 3000, &[]).income_quote,
            restored.income_quote
        );
        // Exercise authenticated STANDARD/PM REST through a mock with an
        // explicitly bound source IP; repeat reads must not double-count income.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let from = (crate::kline::now_us() / 1000) * 1000;
        let through = from + 5000;
        let rest_body = serde_json::json!([{"symbol":"BTCUSDT","asset":"USDT","income":"-0.5",
            "incomeType":"FUNDING_FEE","tranId":"rest-id","time":from/1000+1}])
        .to_string();
        let server = std::thread::spawn(move || {
            for index in 0..5 {
                let (mut stream, peer) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                assert_eq!(peer.ip(), "127.0.0.2".parse::<std::net::IpAddr>().unwrap());
                let mut request = Vec::new();
                let mut bytes = [0; 2048];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let size = stream.read(&mut bytes).unwrap();
                    assert!(size > 0);
                    request.extend_from_slice(&bytes[..size]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.contains(if index != 2 {
                    "/fapi/v1/income?"
                } else {
                    "/papi/v1/um/income?"
                }));
                assert!(
                    request.contains("incomeType=FUNDING_FEE")
                        && request.contains("symbol=BTCUSDT")
                        && request.contains("page=1")
                );
                assert!(
                    request.contains("signature=")
                        && request.to_lowercase().contains("x-mbx-apikey: fixture-key")
                );
                let (status, body, extra) = if index < 2 {
                    ("200 OK", rest_body.as_str(), "")
                } else if index == 2 {
                    ("429 Too Many Requests", "{}", "Retry-After: 5\r\n")
                } else if index == 3 {
                    ("401 Unauthorized", "{}", "")
                } else {
                    ("200 OK", "[]", "")
                };
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let directory = tempfile::tempdir()?;
        let env = directory.path().join("env.sh");
        let env_body = |mode: &str| {
            format!(
                "BINANCE_API_KEY=fixture-key\nBINANCE_API_SECRET=fixture-secret\nBINANCE_ACCOUNT_MODE={mode}\nBINANCE_FAPI_URL=http://{address}\nBINANCE_PAPI_URL=http://{address}\n"
            )
        };
        std::fs::write(&env, env_body("standard"))?;
        let mut rest_source = source.clone();
        rest_source.env_path = Some(env.clone());
        let client = bound_http_client("127.0.0.2".parse()?)?;
        hub.sync_symbol(&pool, &client, &rest_source, "BTCUSDT", from, through)
            .await?;
        hub.sync_symbol(&pool, &client, &rest_source, "BTCUSDT", from, through)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cta_funding_income")
                .fetch_one(&pool)
                .await?,
            2
        );
        std::fs::write(&env, env_body("unified"))?;
        assert!(
            hub.sync_symbol(
                &pool,
                &client,
                &rest_source,
                "BTCUSDT",
                from,
                through + 1000
            )
            .await
            .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT synced_through_ts_us FROM cta_funding_symbols")
                .fetch_one(&pool)
                .await?,
            through
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), hub.reserve())
                .await
                .is_err()
        );
        *hub.budget.lock().await = RequestBudget::default();
        std::fs::write(&env, env_body("standard"))?;
        assert!(
            hub.verify_account(&client, &rest_source, crate::kline::now_us())
                .await
                .is_err()
        );
        assert!(!hub.settings(&source.id).enabled);
        hub.verify_account(&client, &rest_source, crate::kline::now_us())
            .await?;
        assert!(!hub.settings(&source.id).enabled); // Testing itself never toggles collection.
        server.join().unwrap();
        // Enabling on an existing account must backfill the earliest local
        // order, and repair an activation-based start without deleting income.
        let mut history_source = source.clone();
        history_source.id = "funding-history-fixture".into();
        crate::postgres::register_sources(&pool, std::slice::from_ref(&history_source)).await?;
        assert_eq!(
            reconcile_order_history(&pool, &history_source, &BTreeMap::new()).await?,
            None
        );
        sqlx::query("INSERT INTO cta_funding_accounts VALUES($1,5000)")
            .bind(&history_source.id)
            .execute(&pool)
            .await?;
        for symbol in ["BTCUSDT", "ETHUSDT"] {
            sqlx::query("INSERT INTO cta_funding_symbols(source_id,venue,symbol,start_ts_us,synced_through_ts_us) VALUES($1,$2,$3,5000,8000)")
                .bind(&history_source.id).bind(VENUE).bind(symbol).execute(&pool).await?;
        }
        let history_item = || {
            let mut row = item();
            row.record.source_id = history_source.id.clone();
            row.record.ts_us = 6000;
            row
        };
        persist_chunk(
            &pool,
            &history_source.id,
            "BTCUSDT",
            8000,
            &[history_item()],
        )
        .await?;
        let mut earliest = BTreeMap::from([("BTCUSDT".into(), 1000), ("ETHUSDT".into(), 3000)]);
        assert_eq!(
            reconcile_order_history(&pool, &history_source, &earliest).await?,
            Some(1000)
        );
        let symbol_states = sqlx::query("SELECT symbol,start_ts_us,synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1 ORDER BY symbol")
            .bind(&history_source.id).fetch_all(&pool).await?;
        assert_eq!(symbol_states[0].get::<i64, _>("start_ts_us"), 1000);
        assert_eq!(symbol_states[1].get::<i64, _>("start_ts_us"), 3000);
        assert!(
            symbol_states
                .iter()
                .all(|row| row.get::<Option<i64>, _>("synced_through_ts_us").is_none())
        );
        persist_chunk(
            &pool,
            &history_source.id,
            "BTCUSDT",
            7000,
            &[history_item()],
        )
        .await?;
        persist_chunk(&pool, &history_source.id, "ETHUSDT", 7000, &[]).await?;
        assert_eq!(
            reconcile_order_history(&pool, &history_source, &earliest).await?,
            Some(1000)
        );
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1 AND symbol='BTCUSDT'")
            .bind(&history_source.id).fetch_one(&pool).await?, 7000);
        earliest.insert("BTCUSDT".into(), 500);
        assert_eq!(
            reconcile_order_history(&pool, &history_source, &earliest).await?,
            Some(500)
        );
        assert!(sqlx::query_scalar::<_,Option<i64>>("SELECT synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1 AND symbol='BTCUSDT'")
            .bind(&history_source.id).fetch_one(&pool).await?.is_none());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT synced_through_ts_us FROM cta_funding_symbols WHERE source_id=$1 AND symbol='ETHUSDT'")
            .bind(&history_source.id).fetch_one(&pool).await?, 7000);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM cta_funding_income WHERE source_id=$1"
            )
            .bind(&history_source.id)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT start_ts_us FROM cta_funding_accounts WHERE source_id=$1"
            )
            .bind(&source.id)
            .fetch_one(&pool)
            .await?,
            1000
        );
        let mut history_config = config.clone();
        history_config.sources = vec![history_source.clone()];
        let history_hub = FundingHub::new(&history_config);
        history_hub
            .reload(&pool, &history_source, 500, false)
            .await?;
        let partial = history_hub.summary(&history_source.id, 500, 9000, &[]);
        assert_eq!(partial.status, "disabled");
        assert_eq!(partial.record_count, 1);
        assert_eq!(partial.income_quote, -0.0123456789012345);
        sqlx::raw_sql("ALTER TABLE cta_order_sources DROP COLUMN funding_enabled, DROP COLUMN funding_refresh_secs, DROP COLUMN funding_verified_at_us, DROP COLUMN funding_verified_local_ip, DROP COLUMN funding_verified_public_ip;").execute(&pool).await?;
        sqlx::raw_sql(include_str!("../scripts/apply_funding_settings_schema.sql"))
            .execute(&pool)
            .await?;
        assert!(
            !sqlx::query_scalar::<_, bool>(
                "SELECT funding_enabled FROM cta_order_sources WHERE source_id=$1"
            )
            .bind(&source.id)
            .fetch_one(&pool)
            .await?
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cta_funding_income")
                .fetch_one(&pool)
                .await?,
            3
        );
        // Explicit maintenance also works on the previous schema, atomically.
        sqlx::raw_sql("DROP TABLE cta_funding_income; DROP TABLE cta_funding_symbols; DROP TABLE cta_funding_accounts;")
            .execute(&pool).await?;
        sqlx::raw_sql(include_str!("../scripts/apply_funding_schema.sql"))
            .execute(&pool)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cta_funding_income")
                .fetch_one(&pool)
                .await?,
            0
        );
        pool.close().await;
        sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}")))
            .execute(&admin)
            .await?;
        admin.close().await;
        Ok(())
    }
}
