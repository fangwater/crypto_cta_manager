//! Poll an independent NAV Manager over HTTP. External accounts never become
//! Exec sources and share no paths, database connections or runtime stores.
use super::*;
use crate::auth::AuthUser;
use serde_json::json;
use sqlx::{PgPool, Row};
use tokio::sync::Notify;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub provider_id: String,
    pub url: String,
    pub detail_base_url: String,
    pub token_env: String,
    pub refresh_secs: u64,
    pub request_timeout_secs: u64,
    pub stale_secs: u64,
    /// Operator-defined visibility boundary, including administrators. When set,
    /// this explicit allowlist grants these users access to this provider only.
    pub viewer_usernames: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_id: "jp-meta-nav".into(),
            url: String::new(),
            detail_base_url: "/nav/#/strategies/".into(),
            token_env: "CRYPTO_NAV_OVERVIEW_TOKEN".into(),
            refresh_secs: 15,
            request_timeout_secs: 10,
            stale_secs: 90,
            viewer_usernames: vec![],
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        ensure!(
            !self.provider_id.is_empty()
                && self.provider_id.len() <= 64
                && self
                    .provider_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
            "invalid dashboard.nav_overview.provider_id"
        );
        let url = reqwest::Url::parse(&self.url).context("invalid dashboard.nav_overview.url")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "nav_overview URL must be an HTTP endpoint without credentials or query"
        );
        let local_detail = self.detail_base_url.starts_with('/')
            && !self.detail_base_url.starts_with("//")
            && !self.detail_base_url.contains(['?', '\\'])
            && self
                .detail_base_url
                .split_once('#')
                .is_none_or(|(_, route)| route.starts_with('/') && !route.contains('#'))
            && self.detail_base_url.ends_with('/');
        let remote_detail = reqwest::Url::parse(&self.detail_base_url).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && url.query() == Some("source=")
        });
        ensure!(local_detail || remote_detail, "invalid overview detail URL");
        ensure!(
            self.viewer_usernames
                .iter()
                .all(|name| !name.trim().is_empty() && name.len() <= 128),
            "invalid overview viewers"
        );
        ensure!(
            (5..=300).contains(&self.refresh_secs)
                && (1..=60).contains(&self.request_timeout_secs)
                && (self.refresh_secs * 2..=3600).contains(&self.stale_secs),
            "invalid nav_overview polling/freshness intervals"
        );
        ensure!(
            !self.token_env.is_empty()
                && self
                    .token_env
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "invalid nav_overview token environment name"
        );
        Ok(())
    }
    fn source_id(&self, upstream: &str) -> String {
        format!("external:{}:{upstream}", self.provider_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider_id: String,
    pub status: String,
    pub last_success_ms: Option<i64>,
}
#[derive(Clone, Deserialize)]
struct Upstream {
    accounts: Vec<OverviewRow>,
    dates: Vec<String>,
    generated_at_ms: i64,
    loading: bool,
}
#[derive(Clone)]
struct Received {
    report: Upstream,
    at: i64,
}
#[derive(Default)]
struct Cache {
    latest: Option<Received>,
    days: BTreeMap<String, Received>,
    pending: BTreeSet<String>,
    failed: BTreeSet<String>,
    latest_failed: bool,
    catalog: Vec<OverviewRow>,
}
#[derive(Clone)]
pub struct ExternalOverview {
    settings: Settings,
    cache: Arc<RwLock<Cache>>,
    notify: Arc<Notify>,
}

impl ExternalOverview {
    pub fn user_allowed(&self, user: &AuthUser) -> bool {
        self.settings.viewer_usernames.is_empty()
            || self.settings.viewer_usernames.contains(&user.username)
    }
    pub fn owns(&self, source: &str) -> bool {
        source.starts_with(&format!("external:{}:", self.settings.provider_id))
    }
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            cache: Arc::default(),
            notify: Arc::default(),
        }
    }
    pub async fn initialize(&self, pool: &PgPool) -> Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }
        let rows=sqlx::query("SELECT upstream_id,account_label,venue_label FROM cta_external_overview_accounts WHERE provider_id=$1 AND enabled ORDER BY source_id")
            .bind(&self.settings.provider_id).fetch_all(pool).await.context("load external overview catalog; apply explicit external overview schema maintenance first")?;
        self.cache.write().unwrap().catalog = rows
            .into_iter()
            .map(|row| {
                Ok(OverviewRow {
                    source_id: row.try_get("upstream_id")?,
                    account: row.try_get("account_label")?,
                    venue: row.try_get("venue_label")?,
                    ..Default::default()
                })
            })
            .collect::<Result<_>>()?;
        Ok(())
    }
    pub fn spawn(&self, pool: PgPool) -> Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }
        self.settings.validate()?;
        let token = std::env::var(&self.settings.token_env)
            .context("external overview token environment is missing")?;
        ensure!(
            (32..=256).contains(&token.len()),
            "external overview token must have 32-256 characters"
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(self.settings.request_timeout_secs))
            .build()?;
        let hub = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(hub.settings.refresh_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let tick = tokio::select! {_=ticker.tick()=>true,_=hub.notify.notified()=>false};
                if tick {
                    match hub.fetch(&client, &token, None).await {
                        Ok(report) => {
                            if !report.loading {
                                if let Err(error) = hub.register(&pool, &report.accounts).await {
                                    warn!(%error,"external account catalog refresh failed");
                                    hub.cache.write().unwrap().latest_failed = true;
                                    continue;
                                }
                            }
                            let mut cache = hub.cache.write().unwrap();
                            if !report.loading {
                                cache.catalog = report.accounts.clone();
                            }
                            // A provider restart must not replace the last good report with an empty warm-up.
                            if !report.loading || cache.latest.is_none() {
                                cache.latest = Some(Received {
                                    report,
                                    at: now_ms(),
                                });
                            }
                            cache.latest_failed = false;
                        }
                        Err(_) => {
                            hub.cache.write().unwrap().latest_failed = true;
                            warn!("NAV overview request failed; retaining the last good report");
                        }
                    }
                }
                let pending = std::mem::take(&mut hub.cache.write().unwrap().pending);
                for date in pending {
                    match hub.fetch(&client, &token, Some(&date)).await {
                        Ok(report) => {
                            let mut cache = hub.cache.write().unwrap();
                            cache.days.insert(
                                date.clone(),
                                Received {
                                    report,
                                    at: now_ms(),
                                },
                            );
                            cache.failed.remove(&date);
                        }
                        Err(_) => {
                            hub.cache.write().unwrap().failed.insert(date);
                        }
                    }
                }
                let floor = date_label(day_start(now_ms()) - 31 * DAY_MS);
                hub.cache
                    .write()
                    .unwrap()
                    .days
                    .retain(|day, _| day >= &floor);
            }
        });
        Ok(())
    }
    async fn fetch(
        &self,
        client: &reqwest::Client,
        token: &str,
        date: Option<&str>,
    ) -> Result<Upstream> {
        let mut request = client.get(&self.settings.url).bearer_auth(token);
        if let Some(date) = date {
            request = request.query(&[("date", date)]);
        }
        let response = request.send().await?.error_for_status()?;
        ensure!(
            response.content_length().is_none_or(|n| n <= 1_048_576),
            "external response too large"
        );
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 1_048_576,
                "external response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let report: Upstream = serde_json::from_slice(&bytes)?;
        validate_report(&report, now_ms(), date)?;
        Ok(report)
    }
    async fn register(&self, pool: &PgPool, accounts: &[OverviewRow]) -> Result<()> {
        let mut tx = pool.begin().await?;
        for row in accounts {
            sqlx::query("INSERT INTO cta_external_overview_accounts (source_id,provider_id,upstream_id,account_label,venue_label,enabled) VALUES ($1,$2,$3,$4,$5,true) ON CONFLICT (source_id) DO UPDATE SET account_label=EXCLUDED.account_label,venue_label=EXCLUDED.venue_label,enabled=true")
                .bind(self.settings.source_id(&row.source_id)).bind(&self.settings.provider_id).bind(&row.source_id).bind(&row.account).bind(&row.venue).execute(&mut *tx).await?;
        }
        let ids = accounts
            .iter()
            .map(|r| r.source_id.clone())
            .collect::<Vec<_>>();
        sqlx::query("UPDATE cta_external_overview_accounts SET enabled=false WHERE provider_id=$1 AND NOT (upstream_id=ANY($2))")
            .bind(&self.settings.provider_id).bind(ids).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn merge(
        &self,
        pool: &PgPool,
        user: &AuthUser,
        selected: Option<i64>,
        alerts: &OverviewAlertSettings,
        report: &mut OverviewResponse,
    ) -> Result<()> {
        if !self.settings.enabled || !self.user_allowed(user) {
            return Ok(());
        }
        let allowed = if user.is_admin() || !self.settings.viewer_usernames.is_empty() {
            None
        } else {
            Some(sqlx::query_scalar::<_,String>("SELECT v.source_id FROM cta_external_overview_viewers v JOIN cta_external_overview_accounts a USING(source_id) WHERE v.user_id=$1 AND a.provider_id=$2 AND a.enabled")
            .bind(user.user_id).bind(&self.settings.provider_id).fetch_all(pool).await?.into_iter().collect::<BTreeSet<_>>())
        };
        let time = now_ms();
        let historical = selected
            .filter(|day| *day < day_start(time))
            .map(date_label);
        let mut cache = self.cache.write().unwrap();
        let received = if let Some(date) = &historical {
            cache.days.get(date)
        } else {
            cache.latest.as_ref()
        }
        .cloned();
        let pending = historical.as_ref().is_some_and(|date| {
            cache
                .latest
                .as_ref()
                .is_some_and(|latest| latest.report.dates.contains(date))
                && received.as_ref().is_none_or(|r| time - r.at > 300_000)
        });
        if pending {
            if cache.pending.insert(historical.clone().unwrap()) {
                self.notify.notify_one();
            }
        }
        let failed = historical
            .as_ref()
            .map_or(cache.latest_failed, |d| cache.failed.contains(d));
        let freshness_ms = if historical.is_some() {
            300_000
        } else {
            self.settings.stale_secs as i64 * 1000
        };
        let stale = failed
            || received.as_ref().is_none_or(|r| {
                time - r.at > freshness_ms || time - r.report.generated_at_ms > freshness_ms
            });
        let rows = received
            .as_ref()
            .map(|r| r.report.accounts.as_slice())
            .unwrap_or(&cache.catalog);
        let mut visible = 0;
        for original in rows {
            let id = self.settings.source_id(&original.source_id);
            if allowed
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&id))
            {
                continue;
            }
            visible += 1;
            let mut row = if received.is_some() {
                original.clone()
            } else {
                OverviewRow {
                    source_id: original.source_id.clone(),
                    account: original.account.clone(),
                    venue: original.venue.clone(),
                    ..Default::default()
                }
            };
            row.source_id = id;
            if row.account_kind.is_empty() {
                row.account_kind = "fr".into();
            }
            row.detail_url = Some(format!(
                "{}{}",
                self.settings.detail_base_url, original.source_id
            ));
            if received.is_none() {
                row.collection_status =
                    Some(if pending { "loading" } else { "unavailable" }.into());
            } else if stale {
                row.collection_status = Some("stale".into());
            }
            row.leverage_alert = row
                .long_leverage
                .zip(alerts.long_leverage_limit)
                .zip(row.short_leverage.zip(alerts.short_leverage_limit))
                .map(|((a, b), (c, d))| a > b || c > d)
                // A priced subtotal can prove a breach, but cannot prove that
                // leverage is below the limit when other holdings lack prices.
                .filter(|breached| *breached || row.position_unpriced_assets.is_empty());
            row.timeout_alert = alerts
                .timeout_secs
                .map(|secs| stale || row.date_ms.is_none_or(|ts| time - ts > secs as i64 * 1000));
            if historical.is_some() && !stale {
                row.timeout_alert = alerts.timeout_secs.map(|_| false);
            }
            report.accounts.push(row);
        }
        if visible > 0 || user.is_admin() {
            if let Some(latest) = &cache.latest {
                report.dates.extend(latest.report.dates.clone());
            }
            report.dates.sort();
            report.dates.dedup();
            report.dates.reverse();
            report.external_status.push(ProviderStatus {
                provider_id: self.settings.provider_id.clone(),
                status: if received.is_none() {
                    if pending { "loading" } else { "unavailable" }
                } else if stale {
                    "stale"
                } else {
                    "live"
                }
                .into(),
                last_success_ms: received.as_ref().map(|r| r.at),
            });
        }
        Ok(())
    }
    pub async fn access(&self, pool: &PgPool) -> Result<serde_json::Value> {
        if !self.settings.enabled {
            return Ok(json!([]));
        }
        let rows=sqlx::query("SELECT a.source_id,a.account_label,a.venue_label,coalesce(array_agg(v.user_id) FILTER (WHERE v.user_id IS NOT NULL),'{}') AS viewers FROM cta_external_overview_accounts a LEFT JOIN cta_external_overview_viewers v USING(source_id) WHERE a.provider_id=$1 AND a.enabled GROUP BY a.source_id ORDER BY a.account_label")
            .bind(&self.settings.provider_id).fetch_all(pool).await?;
        rows.into_iter().map(|row|Ok(json!({"source_id":row.try_get::<String,_>("source_id")?,"account":row.try_get::<String,_>("account_label")?,"venue":row.try_get::<String,_>("venue_label")?,"viewers":row.try_get::<Vec<i64>,_>("viewers")?}))).collect::<Result<Vec<_>>>().map(|v|json!(v))
    }
    pub async fn set_viewers(&self, pool: &PgPool, source: &str, viewers: &[i64]) -> Result<()> {
        ensure!(self.settings.enabled, "external overview is disabled");
        let mut tx = pool.begin().await?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cta_external_overview_accounts WHERE source_id=$1 AND provider_id=$2 AND enabled)").bind(source).bind(&self.settings.provider_id).fetch_one(&mut *tx).await?;
        ensure!(exists, "external account not found");
        let viewers = viewers
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cta_users WHERE user_id=ANY($1) AND NOT disabled",
        )
        .bind(&viewers)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            count as usize == viewers.len(),
            "unknown or disabled viewer"
        );
        sqlx::query("DELETE FROM cta_external_overview_viewers WHERE source_id=$1")
            .bind(source)
            .execute(&mut *tx)
            .await?;
        for viewer in viewers {
            sqlx::query(
                "INSERT INTO cta_external_overview_viewers (source_id,user_id) VALUES ($1,$2)",
            )
            .bind(source)
            .bind(viewer)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

fn validate_report(report: &Upstream, time: i64, date: Option<&str>) -> Result<()> {
    ensure!(
        (0..=120_000).contains(&(time - report.generated_at_ms)),
        "external report time is stale or future"
    );
    ensure!(
        report.accounts.len() <= 1000 && report.dates.len() <= 64,
        "external report too large"
    );
    let mut ids = BTreeSet::new();
    for row in &report.accounts {
        ensure!(
            !row.source_id.is_empty()
                && row.source_id.len() <= 128
                && row
                    .source_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
                && ids.insert(&row.source_id),
            "invalid or duplicate external account identity"
        );
        ensure!(
            !row.account.is_empty()
                && row.account.len() <= 256
                && matches!(
                    row.venue.as_str(),
                    "binance" | "bybit" | "gate" | "bitget" | "okx" | "bybit-futures"
                ),
            "invalid external account metadata"
        );
        ensure!(
            matches!(row.account_kind.as_str(), "" | "fr" | "mm" | "cta"),
            "invalid external account kind"
        );
        ensure!(
            row.collection_status
                .as_deref()
                .is_some_and(|s| matches!(s, "live" | "stale" | "unavailable" | "historical")),
            "invalid external collection status"
        );
        ensure!(
            row.position_unpriced_assets.len() <= 1000
                && row.position_unpriced_assets.iter().all(|asset| {
                    !asset.trim().is_empty()
                        && asset.len() <= 128
                        && !asset.chars().any(char::is_control)
                }),
            "invalid unpriced exposure assets"
        );
        for value in [
            row.yesterday_equity,
            row.equity,
            row.today_profit,
            row.profit_percent,
            row.long_position,
            row.short_position,
            row.long_leverage,
            row.short_leverage,
            row.net_position,
            row.max_drawdown_15d_percent,
            row.margin,
            row.margin_percent,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(value.is_finite(), "non-finite external amount");
        }
        for value in [
            row.long_position,
            row.short_position,
            row.long_leverage,
            row.short_leverage,
            row.max_drawdown_15d_percent,
            row.margin,
            row.margin_percent,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(value >= 0.0, "negative external risk amount");
        }
        if let Some(date) = date {
            let end = selected_day(date)? + DAY_MS;
            ensure!(
                row.date_ms.is_none_or(|ts| ts <= end),
                "external historical row contains later data"
            );
        }
    }
    for date in &report.dates {
        selected_day(date)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn explicit_provider_viewers_exclude_admins_from_rows_dates_and_status() {
        let hub = ExternalOverview::new(Settings {
            enabled: true,
            viewer_usernames: vec!["lgq".into()],
            ..Default::default()
        });
        let time = now_ms();
        let mut upstream = report();
        upstream.generated_at_ms = time;
        upstream.dates = vec![date_label(day_start(time) - DAY_MS)];
        upstream.accounts[0].account_kind = "mm".into();
        upstream.accounts[0].source_id = "bybit_mm_alpha".into();
        hub.cache.write().unwrap().latest = Some(Received {
            report: upstream,
            at: time,
        });
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://localhost/unused_external_test")
            .unwrap();
        for (name, role, count) in [
            ("prc", "admin", 0),
            ("lgq", "user", 1),
            ("other", "admin", 0),
        ] {
            let user = AuthUser {
                user_id: 1,
                username: name.into(),
                role: role.into(),
            };
            let mut response = OverviewResponse {
                accounts: vec![],
                dates: vec![],
                generated_at_ms: time,
                alerts: Default::default(),
                loading: false,
                external_status: vec![],
            };
            hub.merge(&pool, &user, None, &Default::default(), &mut response)
                .await
                .unwrap();
            assert_eq!(response.accounts.len(), count);
            assert_eq!(response.dates.len(), count);
            assert_eq!(response.external_status.len(), count);
            if count > 0 {
                assert_eq!(response.accounts[0].account_kind, "mm");
            }
        }
    }

    fn report() -> Upstream {
        Upstream {
            accounts: vec![OverviewRow {
                source_id: "binance_fr_arb02".into(),
                account: "FR".into(),
                venue: "binance".into(),
                collection_status: Some("live".into()),
                equity: Some(100.0),
                ..Default::default()
            }],
            dates: vec![],
            generated_at_ms: 1000,
            loading: false,
        }
    }
    #[test]
    fn malformed_duplicate_stale_and_nonfinite_reports_are_rejected() {
        let mut r = report();
        assert!(validate_report(&r, 1000, None).is_ok());
        r.accounts.push(r.accounts[0].clone());
        assert!(validate_report(&r, 1000, None).is_err());
        r.accounts.pop();
        assert!(validate_report(&r, 200000, None).is_err());
        r.accounts[0].equity = Some(f64::NAN);
        assert!(validate_report(&r, 1000, None).is_err());
        r.accounts[0].equity = Some(100.0);
        r.accounts[0].source_id = "../account".into();
        assert!(validate_report(&r, 1000, None).is_err());
    }
    #[test]
    fn integration_settings_keep_identity_and_links_separate_from_exec() {
        let mut s = Settings {
            enabled: true,
            url: "http://127.0.0.1:4191/nav-api/integrations/fr-account-overview".into(),
            ..Default::default()
        };
        assert!(s.validate().is_ok());
        assert_eq!(s.source_id("same"), "external:jp-meta-nav:same");
        s.detail_base_url = "//other/".into();
        assert!(s.validate().is_err());
        s.detail_base_url = "/nav/#/strategies/".into();
        assert!(s.validate().is_ok());
        s.detail_base_url = "/nav/#https://other/".into();
        assert!(s.validate().is_err());
        s.detail_base_url = "/nav/#/strategies/".into();
        s.url = "http://user:secret@example.test/api".into();
        assert!(s.validate().is_err());
    }

    #[tokio::test]
    async fn partial_positions_keep_coverage_and_do_not_claim_safe_leverage() {
        let hub = ExternalOverview::new(Settings {
            enabled: true,
            ..Default::default()
        });
        let time = now_ms();
        let upstream: Upstream = serde_json::from_value(json!({
            "accounts": [{"source_id":"gate_fr_arb01","account":"Gate","venue":"gate",
                "collection_status":"live","equity":100.0,"long_position":200.0,
                "short_position":190.0,"long_leverage":2.0,"short_leverage":1.9,
                "net_position":10.0,"position_unpriced_assets":["SLVON","TSLAX"]}],
            "dates":[],"generated_at_ms":time,"loading":false
        }))
        .unwrap();
        validate_report(&upstream, time, None).unwrap();
        hub.cache.write().unwrap().latest = Some(Received {
            report: upstream,
            at: time,
        });
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://localhost/unused_external_test")
            .unwrap();
        let user = AuthUser {
            user_id: 1,
            username: "admin".into(),
            role: "admin".into(),
        };
        for (limit, expected_alert) in [(3.0, None), (1.0, Some(true))] {
            let alerts = OverviewAlertSettings {
                long_leverage_limit: Some(limit),
                short_leverage_limit: Some(limit),
                ..Default::default()
            };
            let mut response = OverviewResponse {
                accounts: vec![],
                dates: vec![],
                generated_at_ms: time,
                alerts: alerts.clone(),
                loading: false,
                external_status: vec![],
            };
            hub.merge(&pool, &user, None, &alerts, &mut response)
                .await
                .unwrap();
            let row = &response.accounts[0];
            assert_eq!(row.long_position, Some(200.0));
            assert_eq!(row.net_position, Some(10.0));
            assert_eq!(row.position_unpriced_assets, ["SLVON", "TSLAX"]);
            assert_eq!(row.leverage_alert, expected_alert);
            assert_eq!(row.equity, Some(100.0));
        }
    }

    #[tokio::test]
    async fn historical_misses_never_relabel_latest_and_outages_preserve_factual_rows() {
        let settings = Settings {
            enabled: true,
            ..Default::default()
        };
        let hub = ExternalOverview::new(settings);
        let time = now_ms();
        let yesterday = day_start(time) - DAY_MS;
        let mut upstream = report();
        upstream.generated_at_ms = time;
        upstream.dates = vec![date_label(yesterday)];
        hub.cache.write().unwrap().catalog = upstream.accounts.clone();
        hub.cache.write().unwrap().latest = Some(Received {
            report: upstream,
            at: time,
        });
        let user = AuthUser {
            user_id: 1,
            username: "admin".into(),
            role: "admin".into(),
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://localhost/unused_external_test")
            .unwrap();
        let empty = || OverviewResponse {
            accounts: vec![],
            dates: vec![],
            generated_at_ms: time,
            alerts: Default::default(),
            loading: false,
            external_status: vec![],
        };
        let mut historical = empty();
        hub.merge(
            &pool,
            &user,
            Some(yesterday),
            &Default::default(),
            &mut historical,
        )
        .await
        .unwrap();
        assert_eq!(historical.accounts[0].equity, None);
        assert_eq!(
            historical.accounts[0].collection_status.as_deref(),
            Some("loading")
        );
        assert!(
            hub.cache
                .read()
                .unwrap()
                .pending
                .contains(&date_label(yesterday))
        );
        let mut current = empty();
        hub.merge(&pool, &user, None, &Default::default(), &mut current)
            .await
            .unwrap();
        assert_eq!(current.accounts[0].equity, Some(100.0));
        assert_eq!(
            current.accounts[0].source_id,
            "external:jp-meta-nav:binance_fr_arb02"
        );
        assert_eq!(
            current.accounts[0].detail_url.as_deref(),
            Some("/nav/#/strategies/binance_fr_arb02")
        );
        hub.cache.write().unwrap().latest_failed = true;
        let mut stale = empty();
        hub.merge(&pool, &user, None, &Default::default(), &mut stale)
            .await
            .unwrap();
        assert_eq!(stale.accounts[0].equity, Some(100.0));
        assert_eq!(
            stale.accounts[0].collection_status.as_deref(),
            Some("stale")
        );
    }
}
