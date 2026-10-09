//! Account BNB reserves. Financial writes are journaled before submission and
//! serialized with BFUSD management; the existing Exec owns hedge execution.
use crate::{
    bfusd_auto::{BinanceApi, TreasuryEgress, finite_number, number},
    config::{AppConfig, SourceConfig},
    exchange_leverage::parse_env_file,
    order_config::{OrderParameters, TargetPosition},
    redis_runtime::RedisRuntime,
    reload_notify::ReloadNotifyHub,
    viz_snapshot::VizSnapshotClient,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub const HEDGE_STRATEGY: &str = "SYSTEM_BNB_RESERVE";
pub const HEDGE_SYMBOL: &str = "BNBUSDC";
const LEGACY_HEDGE_SYMBOL: &str = "BNBUSDT";
const MIGRATION_STEP_BNB: f64 = 0.5;
const MIGRATION_INTERVAL_SECS: u64 = 60;

/// Deployment preflight: only reads exchange balances, without starting web,
/// Earn workers, database initialization or hedge publication.
pub async fn preview_source(config: &AppConfig, source_id: &str) -> Result<Status> {
    let source = config
        .sources
        .iter()
        .find(|s| s.id == source_id)
        .context("BNB preview source is not configured")?;
    let hub = BnbManager::new(
        config,
        Arc::new(Mutex::new(())),
        TreasuryEgress::new(config.treasury.clone()),
        RedisRuntime::connect(config.redis.clone())?,
        ReloadNotifyHub::spawn(),
    )?;
    hub.run(source, true).await?;
    Ok(hub.status(source_id).await)
}
const EPS: f64 = 0.00000001;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub dry_run: bool,
    /// Explicit operator-selected VIP requirement, never reduced from live VIP status.
    pub required_bnb: f64,
    pub refill_trigger_bnb: f64,
    pub refill_target_bnb: f64,
    pub futures_trigger_bnb: f64,
    pub futures_target_bnb: f64,
    pub futures_sweep_bnb: f64,
    pub earn_min_bnb: f64,
    pub hedge_tolerance_bnb: f64,
    pub hedge_min_interval_secs: u64,
    pub interval_secs: u64,
    pub max_conversion_usdt: f64,
    pub max_quote_deviation_bps: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: true,
            required_bnb: 5.,
            refill_trigger_bnb: 5.2,
            refill_target_bnb: 6.,
            futures_trigger_bnb: 1.2,
            futures_target_bnb: 1.5,
            futures_sweep_bnb: 1.8,
            earn_min_bnb: 0.1,
            hedge_tolerance_bnb: 0.02,
            hedge_min_interval_secs: 300,
            interval_secs: 60,
            max_conversion_usdt: 10_000.,
            max_quote_deviation_bps: 100.,
        }
    }
}
impl Settings {
    fn target(&self) -> f64 {
        self.refill_target_bnb
    }
    fn trigger(&self) -> f64 {
        self.refill_trigger_bnb
    }
    pub fn validate(&self) -> Result<()> {
        let values = [
            self.required_bnb,
            self.refill_trigger_bnb,
            self.refill_target_bnb,
            self.futures_trigger_bnb,
            self.futures_target_bnb,
            self.futures_sweep_bnb,
            self.earn_min_bnb,
            self.hedge_tolerance_bnb,
            self.max_conversion_usdt,
            self.max_quote_deviation_bps,
        ];
        ensure!(
            values.into_iter().all(|v| v.is_finite() && v > 0.),
            "BNB settings must be finite and positive"
        );
        ensure!(
            self.required_bnb < self.refill_trigger_bnb
                && self.refill_trigger_bnb < self.refill_target_bnb,
            "BNB thresholds must satisfy VIP requirement < trigger < target"
        );
        ensure!(
            1. < self.futures_trigger_bnb
                && self.futures_trigger_bnb < self.futures_target_bnb
                && self.futures_target_bnb < self.futures_sweep_bnb
                && self.futures_target_bnb < self.target(),
            "BNB futures thresholds must satisfy 1 < trigger < target < sweep and target < total target"
        );
        ensure!(
            (10..=3600).contains(&self.interval_secs),
            "BNB interval must be between 10 and 3600 seconds"
        );
        ensure!(
            (60..=3600).contains(&self.hedge_min_interval_secs),
            "BNB hedge minimum interval must be between 60 and 3600 seconds"
        );
        ensure!(
            self.max_quote_deviation_bps <= 500.
                && self.hedge_tolerance_bnb < self.refill_trigger_bnb - self.required_bnb
                && self.max_conversion_usdt <= 1_000_000.,
            "BNB quote, hedge or conversion limits are invalid"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Account {
    settings: Settings,
    refill_target: Option<f64>,
    route: Option<Route>,
    pending: Option<Pending>,
    #[serde(default)]
    audit: Vec<Audit>,
    #[serde(default)]
    last_hedge_publish_ms: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
enum Route {
    FuturesConvert,
    SpotConvert,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Audit {
    pub at_ms: u64,
    pub action: String,
    pub result: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub at_ms: u64,
    pub action: String,
    pub futures: bool,
    pub path: String,
    pub params: BTreeMap<String, String>,
    pub response: Option<Value>,
    before: Balances,
    /// The receiving wallet must reflect the operation before any next write.
    receive_field: String,
    receive_amount: f64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Balances {
    pub at_ms: u64,
    pub spot_bnb: f64,
    pub spot_free_bnb: f64,
    pub futures_bnb: f64,
    pub futures_available_bnb: f64,
    pub earn_bnb: f64,
    pub spot_bfusd: f64,
    pub futures_bfusd: f64,
    pub futures_available_bfusd: f64,
    pub margin_headroom_usdt: f64,
    pub bnb_price_usdt: f64,
    #[serde(default)]
    pub bnbusdc_position_qty: f64,
}
impl Balances {
    pub fn total(&self) -> f64 {
        self.spot_bnb + self.futures_bnb + self.earn_bnb
    }
    fn field(&self, field: &str) -> f64 {
        match field {
            "spot_bnb" => self.spot_bnb,
            "futures_bnb" => self.futures_bnb,
            "earn_bnb" => self.earn_bnb,
            "spot_bfusd" => self.spot_bfusd,
            "futures_bfusd" => self.futures_bfusd,
            "total" => self.total(),
            _ => f64::NAN,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub settings: Settings,
    pub balances: Option<Balances>,
    pub pending: Option<Pending>,
    pub refill_target: Option<f64>,
    pub last_result: Option<String>,
    pub hedge_qty: Option<f64>,
    pub hedge_error: Option<String>,
    pub hedge_symbol: &'static str,
    pub legacy_hedge_qty: Option<f64>,
    pub audit: Vec<Audit>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Disk {
    accounts: BTreeMap<String, Account>,
}
#[derive(Default)]
struct Runtime {
    disk: Disk,
    status: BTreeMap<String, Status>,
    next_due: BTreeMap<String, Instant>,
    pairs: BTreeMap<(String, bool, String), (Instant, Option<Value>)>,
}
#[derive(Clone)]
pub struct BnbManager {
    path: PathBuf,
    egress: TreasuryEgress,
    state: Arc<Mutex<Runtime>>,
    gate: Arc<Mutex<()>>,
    redis: RedisRuntime,
    notify: ReloadNotifyHub,
}
impl BnbManager {
    pub(crate) fn new(
        config: &AppConfig,
        gate: Arc<Mutex<()>>,
        egress: TreasuryEgress,
        redis: RedisRuntime,
        notify: ReloadNotifyHub,
    ) -> Result<Self> {
        let path = config
            .kline
            .rocksdb_path
            .parent()
            .context("Manager RocksDB has no parent")?
            .join("config/bnb-auto.json");
        let disk: Disk = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            Disk::default()
        };
        for a in disk.accounts.values() {
            a.settings.validate()?;
        }
        for source in disk.accounts.keys() {
            redis.reserve_bnb_symbol(source);
        }
        Ok(Self {
            path,
            egress,
            state: Arc::new(Mutex::new(Runtime {
                disk,
                ..Default::default()
            })),
            gate,
            redis,
            notify,
        })
    }
    pub async fn status(&self, source: &str) -> Status {
        let state = self.state.lock().await;
        let a = state.disk.accounts.get(source).cloned().unwrap_or_default();
        let mut status = state.status.get(source).cloned().unwrap_or(Status {
            settings: a.settings.clone(),
            balances: None,
            pending: None,
            refill_target: None,
            last_result: None,
            hedge_qty: None,
            hedge_error: None,
            hedge_symbol: HEDGE_SYMBOL,
            legacy_hedge_qty: None,
            audit: vec![],
        });
        status.settings = a.settings;
        status.pending = a.pending;
        status.refill_target = a.refill_target;
        status.audit = a.audit;
        status
    }
    pub async fn save(&self, source: &SourceConfig, settings: Settings) -> Result<Status> {
        settings.validate()?;
        ensure!(
            !settings.enabled || self.egress.configured(),
            "treasury egress is required for asset management"
        );
        let _funds = self.gate.lock().await;
        let mut s = self.state.lock().await;
        let mut a = s.disk.accounts.get(&source.id).cloned().unwrap_or_default();
        // Disabling freezes the hedge. Never silently close the reserve short.
        ensure!(
            a.pending.is_none() || settings == a.settings || !settings.enabled,
            "BNB pending operation must be reconciled before changing settings"
        );
        a.settings = settings;
        self.commit(&mut s, &source.id, a)?;
        self.redis.reserve_bnb_symbol(&source.id);
        s.next_due.remove(&source.id);
        drop(s);
        Ok(self.status(&source.id).await)
    }
    pub async fn acknowledge(&self, source: &SourceConfig, pending_at_ms: u64) -> Result<Status> {
        let _funds = self.gate.lock().await;
        let mut s = self.state.lock().await;
        let mut a = s.disk.accounts.get(&source.id).cloned().unwrap_or_default();
        let pending = a.pending.as_ref().context("BNB has no pending operation")?;
        ensure!(
            pending.at_ms == pending_at_ms,
            "BNB pending operation changed; reload before acknowledging"
        );
        let action = pending.action.clone();
        if pending.path.ends_with("/acceptQuote") {
            a.refill_target = None;
            a.route = None;
        }
        a.pending = None;
        add_audit(
            &mut a,
            &action,
            "operator confirmed exchange outcome and balances",
        );
        self.commit(&mut s, &source.id, a)?;
        drop(s);
        Ok(self.status(&source.id).await)
    }
    pub fn spawn(&self, sources: Vec<SourceConfig>) {
        let hub = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                for source in &sources {
                    let due = {
                        let s = hub.state.lock().await;
                        source.enabled
                            && s.disk
                                .accounts
                                .get(&source.id)
                                .is_some_and(|a| a.settings.enabled)
                            && s.next_due
                                .get(&source.id)
                                .is_none_or(|t| *t <= Instant::now())
                    };
                    if due {
                        let _ = hub.run(source, false).await;
                    }
                }
            }
        });
    }
    pub async fn run(&self, source: &SourceConfig, preview: bool) -> Result<String> {
        let _funds = self.gate.lock().await;
        self.run_locked(source, preview).await
    }
    async fn run_locked(&self, source: &SourceConfig, preview: bool) -> Result<String> {
        let mut state = self.state.lock().await;
        let mut a = state
            .disk
            .accounts
            .get(&source.id)
            .cloned()
            .unwrap_or_default();
        ensure!(preview || a.settings.enabled, "BNB management is disabled");
        state.next_due.insert(
            source.id.clone(),
            Instant::now() + Duration::from_secs(a.settings.interval_secs),
        );
        let dry = preview || a.settings.dry_run;
        let result = self.execute(source, &mut state, &mut a, dry).await;
        if result.is_ok() && (a.pending.is_some() || a.refill_target.is_some()) {
            state
                .next_due
                .insert(source.id.clone(), Instant::now() + Duration::from_secs(10));
        }
        let message = match &result {
            Ok(message) => message.clone(),
            Err(e) => format!("{e:#}"),
        };
        let status = state.status.entry(source.id.clone()).or_insert(Status {
            settings: a.settings.clone(),
            balances: None,
            pending: None,
            refill_target: None,
            last_result: None,
            hedge_qty: None,
            hedge_error: None,
            hedge_symbol: HEDGE_SYMBOL,
            legacy_hedge_qty: None,
            audit: vec![],
        });
        status.last_result = Some(message);
        if let Err(error) = &result {
            tracing::warn!(source_id=%source.id, error=%error, "BNB reserve round failed");
        }
        result
    }
    fn commit(&self, state: &mut Runtime, source: &str, account: Account) -> Result<()> {
        let old = state.disk.accounts.insert(source.into(), account);
        if let Err(error) = persist(&self.path, &state.disk) {
            if let Some(old) = old {
                state.disk.accounts.insert(source.into(), old);
            } else {
                state.disk.accounts.remove(source);
            }
            return Err(error);
        }
        Ok(())
    }
    async fn execute(
        &self,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        dry: bool,
    ) -> Result<String> {
        ensure!(
            source.enabled && source.venue == "binance-futures",
            "BNB management requires an enabled Binance USD-M source"
        );
        let ip = self.egress.select(source).await?;
        let env = parse_env_file(&source.env_path())?;
        ensure!(
            env.get("BINANCE_ACCOUNT_MODE").is_some_and(
                |v| v.eq_ignore_ascii_case("standard") || v.eq_ignore_ascii_case("std")
            ),
            "BNB management requires Binance STANDARD account mode"
        );
        let api = BinanceApi::new(&env, ip)?;
        self.execute_with_api(&api, source, state, a, dry).await
    }
    async fn execute_with_api(
        &self,
        api: &BinanceApi,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        dry: bool,
    ) -> Result<String> {
        let (b, positions) = load_balances(api).await?;
        let status = state.status.entry(source.id.clone()).or_insert(Status {
            settings: a.settings.clone(),
            balances: None,
            pending: None,
            refill_target: None,
            last_result: None,
            hedge_qty: None,
            hedge_error: None,
            hedge_symbol: HEDGE_SYMBOL,
            legacy_hedge_qty: None,
            audit: vec![],
        });
        status.balances = Some(b.clone());
        if a.pending.is_some() {
            if dry {
                return Ok(
                    "preview: an operation is pending; no financial writes or hedge updates".into(),
                );
            }
            if !self.reconcile(&api, state, source, a, &b).await? {
                return Ok(
                    "waiting for exchange confirmation/receiving balance; no repeat submission"
                        .into(),
                );
            }
        }
        if dry {
            return Ok(format!(
                "preview: total {:.8} BNB, trigger {:.8}, target {:.8}, buy {:.8}; futures {:.8} → {:.8}; hedge target {:.8}. No mutations or quote acceptance.",
                b.total(),
                a.settings.trigger(),
                a.settings.target(),
                refill_quantity(&a.settings, a.refill_target, b.total()),
                b.futures_bnb,
                a.settings.futures_target_bnb,
                -b.total()
            ));
        }
        // One confirmed funding action per round. Hedge runs even if funding fails,
        // but a hedge failure can never prevent a reserve purchase.
        let mut funding = self
            .manage_funds(&api, source, state, a, &b, &positions)
            .await;
        let mut hedge_balances = b.clone();
        if a.pending.as_ref().is_some_and(|p| p.response.is_some()) {
            if let Ok((fresh, _)) = load_balances(&api).await {
                state.status.get_mut(&source.id).unwrap().balances = Some(fresh.clone());
                if self
                    .reconcile(&api, state, source, a, &fresh)
                    .await
                    .unwrap_or(false)
                {
                    hedge_balances = fresh;
                    if funding.is_ok() {
                        funding = Ok("BNB operation confirmed; balances updated".into());
                    }
                }
            }
        }
        if a.pending.is_none() {
            let hedge = self.hedge(source, state, a, &hedge_balances).await;
            let status = state.status.get_mut(&source.id).unwrap();
            match hedge {
                Ok((qty, legacy_qty)) => {
                    status.hedge_qty = qty;
                    status.legacy_hedge_qty = legacy_qty;
                    status.hedge_error = None;
                }
                Err(e) => status.hedge_error = Some(format!("{e:#}")),
            }
        }
        funding
    }
    async fn manage_funds(
        &self,
        api: &BinanceApi,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        b: &Balances,
        positions: &[Value],
    ) -> Result<String> {
        let fee_refill = (a.settings.futures_target_bnb - b.futures_bnb - b.spot_free_bnb).max(0.);
        if a.refill_target.is_none()
            && b.futures_bnb <= a.settings.futures_trigger_bnb
            && b.total() - fee_refill <= a.settings.required_bnb
        {
            a.refill_target = Some(a.settings.target());
            self.commit(state, &source.id, a.clone())?;
        }
        let qty = refill_quantity(&a.settings, a.refill_target, b.total());
        if qty > EPS {
            if a.refill_target.is_none() {
                a.refill_target = Some(a.settings.target());
                self.commit(state, &source.id, a.clone())?;
            }
            if a.route.is_none() {
                a.route = Some(
                    if self
                        .pair(api, state, source, true, "BFUSD")
                        .await?
                        .is_some()
                    {
                        Route::FuturesConvert
                    } else if self
                        .pair(api, state, source, false, "BFUSD")
                        .await?
                        .is_some()
                    {
                        Route::SpotConvert
                    } else {
                        bail!("BFUSD/BNB direct Convert pair unavailable; BNB refill deferred")
                    },
                );
                self.commit(state, &source.id, a.clone())?;
            }
            return self.refill(api, source, state, a, b, qty).await;
        }
        if a.refill_target.is_some() {
            a.refill_target = None;
            a.route = None;
            self.commit(state, &source.id, a.clone())?;
        }
        let burn = api.get(true, "/fapi/v1/feeBurn", &[]).await?;
        if burn.get("feeBurn").and_then(Value::as_bool) != Some(true) {
            return self
                .submit(
                    api,
                    source,
                    state,
                    a,
                    b,
                    true,
                    "/fapi/v1/feeBurn",
                    vec![("feeBurn", "true".into())],
                    "enable BNB fee discount",
                    "",
                    0.,
                )
                .await;
        }
        if b.futures_bnb <= a.settings.futures_trigger_bnb {
            let amount = (a.settings.futures_target_bnb - b.futures_bnb).max(0.);
            if b.spot_free_bnb >= amount - EPS {
                return self
                    .transfer(
                        api,
                        source,
                        state,
                        a,
                        b,
                        "BNB",
                        "MAIN_UMFUTURE",
                        amount,
                        "futures_bnb",
                    )
                    .await;
            }
            let missing = amount - b.spot_free_bnb;
            let position = positions.iter().find(|p| {
                p.get("canRedeem").and_then(Value::as_bool) == Some(true)
                    && number(p, "totalAmount").unwrap_or(0.)
                        - number(p, "collateralAmount").unwrap_or(0.)
                        >= missing
            });
            let p = position.context("BNB fee reserve low, no redeemable flexible position")?;
            // Avoid taking the counted balance below the VIP floor while redeeming.
            ensure!(
                b.total() - missing > a.settings.required_bnb,
                "BNB redemption would cross VIP requirement; replenish reserve first"
            );
            return self
                .submit(
                    api,
                    source,
                    state,
                    a,
                    b,
                    false,
                    "/sapi/v1/simple-earn/flexible/redeem",
                    vec![
                        ("productId", string(p, "productId")?),
                        ("amount", decimal(missing)),
                        ("redeemAll", "false".into()),
                        ("destAccount", "SPOT".into()),
                    ],
                    "redeem BNB fee reserve",
                    "spot_bnb",
                    missing,
                )
                .await;
        }
        if b.futures_bnb >= a.settings.futures_sweep_bnb {
            let amount =
                (b.futures_bnb - a.settings.futures_target_bnb).min(b.futures_available_bnb);
            if amount >= a.settings.earn_min_bnb
                && b.margin_headroom_usdt >= amount * b.bnb_price_usdt
            {
                return self
                    .transfer(
                        api,
                        source,
                        state,
                        a,
                        b,
                        "BNB",
                        "UMFUTURE_MAIN",
                        amount,
                        "spot_bnb",
                    )
                    .await;
            }
        }
        if b.spot_free_bnb >= a.settings.earn_min_bnb {
            let products = pages(api, "/sapi/v1/simple-earn/flexible/list").await?;
            let product = products
                .iter()
                .find(|p| {
                    p.get("canPurchase").and_then(Value::as_bool) == Some(true)
                        && p.get("isSoldOut").and_then(Value::as_bool) == Some(false)
                })
                .context("BNB flexible product unavailable; funds remain in spot")?;
            let id = string(product, "productId")?;
            let quota = api
                .get(
                    false,
                    "/sapi/v1/simple-earn/flexible/personalLeftQuota",
                    &[("productId", &id)],
                )
                .await?;
            let amount = b.spot_free_bnb.min(number(&quota, "leftPersonalQuota")?);
            ensure!(
                amount
                    >= a.settings
                        .earn_min_bnb
                        .max(number(product, "minPurchaseAmount")?),
                "BNB flexible quota below subscription minimum"
            );
            return self
                .submit(
                    api,
                    source,
                    state,
                    a,
                    b,
                    false,
                    "/sapi/v1/simple-earn/flexible/subscribe",
                    vec![
                        ("productId", id),
                        ("amount", decimal(amount)),
                        ("autoSubscribe", "false".into()),
                        ("sourceAccount", "SPOT".into()),
                    ],
                    "subscribe surplus BNB",
                    "earn_bnb",
                    amount,
                )
                .await;
        }
        Ok("BNB reserve is within thresholds".into())
    }
    async fn pair(
        &self,
        api: &BinanceApi,
        state: &mut Runtime,
        source: &SourceConfig,
        futures: bool,
        from: &str,
    ) -> Result<Option<Value>> {
        let key = (source.id.clone(), futures, from.into());
        if let Some((at, pair)) = state.pairs.get(&key) {
            if at.elapsed() < Duration::from_secs(86400) {
                return Ok(pair.clone());
            }
        }
        let path = if futures {
            "/fapi/v1/convert/exchangeInfo"
        } else {
            "/sapi/v1/convert/exchangeInfo"
        };
        let response = api
            .public_get(futures, path, &[("fromAsset", from), ("toAsset", "BNB")])
            .await?;
        let rows = response.as_array().context("invalid Convert pair list")?;
        let pair = rows
            .iter()
            .find(|p| {
                p.get("fromAsset").and_then(Value::as_str) == Some(from)
                    && p.get("toAsset").and_then(Value::as_str) == Some("BNB")
            })
            .cloned();
        state.pairs.insert(key, (Instant::now(), pair.clone()));
        Ok(pair)
    }
    async fn refill(
        &self,
        api: &BinanceApi,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        b: &Balances,
        qty: f64,
    ) -> Result<String> {
        ensure!(
            qty * b.bnb_price_usdt <= a.settings.max_conversion_usdt,
            "BNB refill exceeds per-operation USDT limit; increase the configured limit"
        );
        let route = a.route.context("BNB funding route missing")?;
        let futures = route == Route::FuturesConvert;
        let from = "BFUSD";
        let pair = self.pair(api, state, source, futures, from).await?;
        let Some(pair) = pair else {
            if futures && self.pair(api, state, source, false, from).await?.is_some() {
                a.route = Some(Route::SpotConvert);
                self.commit(state, &source.id, a.clone())?;
                return Ok("BFUSD/BNB direct Convert route moved to Spot".into());
            }
            bail!("BFUSD/BNB direct Convert pair unavailable; BNB refill deferred");
        };
        ensure!(
            qty + EPS >= number(&pair, "toAssetMinAmount")?
                && qty <= number(&pair, "toAssetMaxAmount")? + EPS,
            "BNB refill is outside Convert pair limits"
        );
        let estimated_cost =
            qty * b.bnb_price_usdt * (1. + a.settings.max_quote_deviation_bps / 10_000.);
        if futures && b.futures_available_bfusd + EPS < qty * b.bnb_price_usdt {
            let amount = ceil8(estimated_cost - b.futures_available_bfusd);
            ensure!(
                amount <= b.spot_bfusd,
                "insufficient transferable BFUSD for futures conversion"
            );
            return self
                .transfer(
                    api,
                    source,
                    state,
                    a,
                    b,
                    "BFUSD",
                    "MAIN_UMFUTURE",
                    amount,
                    "futures_bfusd",
                )
                .await;
        }
        if route == Route::SpotConvert && b.spot_bfusd + EPS < qty * b.bnb_price_usdt {
            return self
                .transfer_bfusd(api, source, state, a, b, estimated_cost - b.spot_bfusd)
                .await;
        }
        let path = if futures {
            "/fapi/v1/convert/getQuote"
        } else {
            "/sapi/v1/convert/getQuote"
        };
        let qty_text = decimal(qty);
        let mut params = vec![
            ("fromAsset", from),
            ("toAsset", "BNB"),
            ("toAmount", qty_text.as_str()),
            ("validTime", "10s"),
        ];
        if !futures {
            params.push(("walletType", "SPOT"));
        }
        let quote = api.post(futures, path, &params).await?;
        let cost = validate_quote(&quote, qty, b.bnb_price_usdt, &a.settings)?;
        ensure!(
            cost >= number(&pair, "fromAssetMinAmount")?
                && cost <= number(&pair, "fromAssetMaxAmount")?,
            "BNB quote is outside source-asset Convert limits"
        );
        if futures {
            ensure!(
                cost <= b.futures_available_bfusd && cost <= b.margin_headroom_usdt,
                "BFUSD collateral is not safely available for conversion"
            );
        }
        let id = string(&quote, "quoteId")?;
        let path = if futures {
            "/fapi/v1/convert/acceptQuote"
        } else {
            "/sapi/v1/convert/acceptQuote"
        };
        self.submit(
            api,
            source,
            state,
            a,
            b,
            futures,
            path,
            vec![("quoteId", id)],
            &format!("convert {from} to BNB"),
            "total",
            number(&quote, "toAmount")?,
        )
        .await
    }
    async fn transfer_bfusd(
        &self,
        api: &BinanceApi,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        b: &Balances,
        amount: f64,
    ) -> Result<String> {
        let amount = ceil8(amount);
        ensure!(
            amount <= b.futures_available_bfusd && amount <= b.margin_headroom_usdt,
            "BFUSD balance exists but transferable margin is insufficient"
        );
        self.transfer(
            api,
            source,
            state,
            a,
            b,
            "BFUSD",
            "UMFUTURE_MAIN",
            amount,
            "spot_bfusd",
        )
        .await
    }
    async fn transfer(
        &self,
        api: &BinanceApi,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        b: &Balances,
        asset: &str,
        kind: &str,
        amount: f64,
        field: &str,
    ) -> Result<String> {
        self.submit(
            api,
            source,
            state,
            a,
            b,
            false,
            "/sapi/v1/asset/transfer",
            vec![
                ("type", kind.into()),
                ("asset", asset.into()),
                ("amount", decimal(amount)),
            ],
            &format!("transfer {asset} {kind}"),
            field,
            amount,
        )
        .await
    }
    async fn submit(
        &self,
        api: &BinanceApi,
        source: &SourceConfig,
        state: &mut Runtime,
        a: &mut Account,
        b: &Balances,
        futures: bool,
        path: &str,
        params: Vec<(&str, String)>,
        action: &str,
        field: &str,
        amount: f64,
    ) -> Result<String> {
        ensure!(a.pending.is_none(), "BNB operation already pending");
        let permit = api.reserve_post(futures, path)?;
        a.pending = Some(Pending {
            at_ms: now_ms(),
            action: action.into(),
            futures,
            path: path.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            response: None,
            before: b.clone(),
            receive_field: field.into(),
            receive_amount: amount,
        });
        self.commit(state, &source.id, a.clone())?;
        let borrowed: Vec<_> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let response = match api.post_reserved(permit, &borrowed).await {
            Ok(response) => response,
            Err(error)
                if error
                    .downcast_ref::<crate::treasury_rate_limit::NotSent>()
                    .is_some() =>
            {
                a.pending = None;
                self.commit(state, &source.id, a.clone())?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        a.pending.as_mut().unwrap().response = Some(response);
        self.commit(state, &source.id, a.clone())?;
        Ok(format!(
            "{action} submitted; awaiting confirmation and receiving balance"
        ))
    }
    async fn reconcile(
        &self,
        api: &BinanceApi,
        state: &mut Runtime,
        source: &SourceConfig,
        a: &mut Account,
        b: &Balances,
    ) -> Result<bool> {
        let p = a.pending.as_ref().unwrap();
        let response = if p.path.ends_with("/acceptQuote") {
            let id = p
                .params
                .get("quoteId")
                .context("pending Convert quote id missing")?;
            api.get(
                p.futures,
                if p.futures {
                    "/fapi/v1/convert/orderStatus"
                } else {
                    "/sapi/v1/convert/orderStatus"
                },
                &[("quoteId", id)],
            )
            .await?
        } else if p.path.ends_with("/feeBurn") {
            api.get(true, "/fapi/v1/feeBurn", &[]).await?
        } else {
            p.response.clone().context("BNB operation outcome unknown; verify exchange history and explicitly acknowledge before retrying")?
        };
        let success = if p.path.ends_with("/acceptQuote") {
            match response.get("orderStatus").and_then(Value::as_str) {
                Some("SUCCESS") => true,
                Some("FAIL") => {
                    let action = p.action.clone();
                    a.pending = None;
                    add_audit(
                        a,
                        &action,
                        "exchange confirmed failure; safe to retry on a later poll",
                    );
                    self.commit(state, &source.id, a.clone())?;
                    return Ok(false);
                }
                _ => false,
            }
        } else if p.path.ends_with("/feeBurn") {
            response.get("feeBurn").and_then(Value::as_bool) == Some(true)
        } else if p.path.ends_with("/asset/transfer") {
            response.get("tranId").is_some_and(|v| {
                v.as_u64().is_some_and(|id| id > 0) || v.as_str().is_some_and(|s| !s.is_empty())
            })
        } else {
            response.get("success").and_then(Value::as_bool) == Some(true)
        };
        let seen = p.receive_field.is_empty()
            || b.field(&p.receive_field) + EPS
                >= p.before.field(&p.receive_field) + p.receive_amount
                    - 0.001_f64.min(p.receive_amount * 0.01);
        if !success || !seen {
            return Ok(false);
        }
        let action = p.action.clone();
        if p.path.ends_with("/acceptQuote") {
            // The requested refill arrived. Small fees since that snapshot must
            // not turn a completed refill into endless sub-minimum purchases.
            a.refill_target = None;
            a.route = None;
        }
        a.pending = None;
        add_audit(a, &action, &response.to_string());
        self.commit(state, &source.id, a.clone())?;
        Ok(true)
    }
    async fn hedge(
        &self,
        source: &SourceConfig,
        state: &mut Runtime,
        account: &mut Account,
        b: &Balances,
    ) -> Result<(Option<f64>, Option<f64>)> {
        let snapshot = self.redis.load_exec_target_snapshot(source).await?;
        let current = snapshot.strategies.get(HEDGE_STRATEGY);
        if let Some(current) = current {
            ensure!(
                current.family == "batch_exec"
                    && !current.targets.is_empty()
                    && current.targets.iter().all(|(symbol, qty)| matches!(
                        symbol.as_str(),
                        HEDGE_SYMBOL | LEGACY_HEDGE_SYMBOL
                    ) && qty.is_finite()
                        && *qty <= EPS),
                "BNB reserved strategy has unexpected ownership/targets"
            );
        }
        ensure!(
            !snapshot
                .strategies
                .iter()
                .any(|(name, strategy)| name != HEDGE_STRATEGY
                    && strategy
                        .targets
                        .get(HEDGE_SYMBOL)
                        .is_some_and(|qty| qty.abs() > EPS)),
            "BNBUSDC is already used by a CTA strategy; reserve migration deferred"
        );
        let viz = VizSnapshotClient::new(5)?;
        let base = source
            .exec_viz_origin()
            .context("BNB hedge needs an Exec Viz endpoint")?;
        let live = viz.load_exec_state(&source.id, base).await?;
        ensure!(
            live.position_ready && now_ms().abs_diff(live.snapshot_ts_ms.max(0) as u64) < 30_000,
            "BNB hedge deferred: Exec snapshot is not ready or stale"
        );
        ensure!(
            !live.rows.iter().any(|r| r.symbol == HEDGE_SYMBOL
                && r.strategy_name != HEDGE_STRATEGY
                && (r.current_qty.is_some_and(|q| q.abs() > EPS)
                    || r.pending_qty.is_some_and(|q| q.abs() > EPS)
                    || r.live_order_qty.is_some_and(|q| q.abs() > EPS))),
            "BNBUSDC has other strategy holdings/orders; reserve migration deferred"
        );
        let row = |symbol: &str| {
            live.rows
                .iter()
                .find(|r| r.strategy_name == HEDGE_STRATEGY && r.symbol == symbol)
        };
        let actual = row(HEDGE_SYMBOL).and_then(|r| r.current_qty);
        let legacy_actual = row(LEGACY_HEDGE_SYMBOL).and_then(|r| r.current_qty);
        ensure!(
            current.is_some()
                || !live.rows.iter().any(|r| r.strategy_name == HEDGE_STRATEGY
                    && r.current_qty.is_some_and(|q| q.abs() > EPS)),
            "BNB reserved targets are missing while holdings remain; reconcile ownership before publishing"
        );
        ensure!(
            current.is_some_and(|c| c.targets.contains_key(HEDGE_SYMBOL))
                || b.bnbusdc_position_qty.abs() <= EPS,
            "BNBUSDC has an existing account position; reserve migration deferred"
        );
        let published_ms = current
            .map(|c| (c.updated_at_us.max(0) as u64) / 1000)
            .unwrap_or(0)
            .max(account.last_hedge_publish_ms);
        let old_targets = current.map(|c| c.targets.clone()).unwrap_or_default();
        // Advance a transfer only after both legs of the previous target settled.
        let settled = old_targets.iter().all(|(symbol, target)| {
            hedge_step_settled(*target, row(symbol), account.settings.hedge_tolerance_bnb)
        });
        let migrating = old_targets
            .get(LEGACY_HEDGE_SYMBOL)
            .is_some_and(|qty| qty.abs() > EPS)
            || legacy_actual.is_some_and(|qty| qty.abs() > account.settings.hedge_tolerance_bnb);
        let interval = if migrating {
            MIGRATION_INTERVAL_SECS
        } else {
            account.settings.hedge_min_interval_secs
        };
        if !settled || !hedge_due(now_ms(), published_ms, interval) {
            return Ok((actual, legacy_actual));
        }
        ensure!(
            (b.bnbusdc_position_qty - actual.unwrap_or(0.)).abs()
                <= account.settings.hedge_tolerance_bnb,
            "BNBUSDC account and reserved-strategy quantities differ; reconcile before adjusting"
        );
        let targets = next_hedge_targets(
            &old_targets,
            b.total(),
            account.settings.hedge_tolerance_bnb,
        );
        if old_targets == targets {
            return Ok((actual, legacy_actual));
        }
        // Persist the attempt before Redis. Redis's own target timestamp is also
        // consulted after restart/readback failure, preventing repeated reloads.
        account.last_hedge_publish_ms = now_ms();
        self.commit(state, &source.id, account.clone())?;
        let params = OrderParameters {
            single_order_usdt: 500.,
            orders_per_batch: 1,
            max_batch: 2,
            batch_interval_ms: 5_000,
            maker_timeout_ms: 10_000,
            max_maker_requotes: 1,
            target_tolerance_usdt: (account.settings.hedge_tolerance_bnb * b.bnb_price_usdt)
                .max(1.),
            ..Default::default()
        };
        let positions = targets
            .iter()
            .map(|(symbol, qty)| {
                (
                    symbol.clone(),
                    TargetPosition {
                        qty: *qty,
                        signal: 0,
                    },
                )
            })
            .collect();
        let published = self
            .redis
            .publish_bnb_hedge(source, &params, &positions)
            .await?;
        self.notify.notify(
            source,
            HEDGE_STRATEGY,
            published.updated_at_us.unwrap_or_default(),
        );
        add_audit(account, "hedge target", &serde_json::to_string(&targets)?);
        self.commit(state, &source.id, account.clone())?;
        Ok((actual, legacy_actual))
    }
}

fn hedge_step_settled(
    target: f64,
    row: Option<&crate::viz_snapshot::ExecStateRowSnapshot>,
    tolerance: f64,
) -> bool {
    row.is_some_and(|r| {
        r.execution_complete
            && r.position_allocated == Some(true)
            && r.current_qty
                .is_some_and(|q| (q - target).abs() <= tolerance)
            && r.target_qty.is_some_and(|q| (q - target).abs() <= EPS)
            // Exec keeps the unfilled target residual as pending_qty even
            // after completing within tolerance. Live orders must still be zero.
            && r.pending_qty.is_some_and(|q| q.abs() <= tolerance)
            && r.live_order_qty.is_some_and(|q| q.abs() <= EPS)
    })
}

fn hedge_due(now: u64, last: u64, interval_secs: u64) -> bool {
    last == 0 || now >= last.saturating_add(interval_secs.saturating_mul(1000))
}

fn next_hedge_targets(
    old: &BTreeMap<String, f64>,
    total: f64,
    tolerance: f64,
) -> BTreeMap<String, f64> {
    let legacy = old.get(LEGACY_HEDGE_SYMBOL).copied().unwrap_or(0.);
    if legacy < -EPS {
        // Keep the combined short at the reserve quantity; each leg changes by
        // at most half a BNB for a stable reserve. The next step waits for fills.
        let next_legacy = (legacy + MIGRATION_STEP_BNB).min(0.).max(-total);
        return BTreeMap::from([
            (LEGACY_HEDGE_SYMBOL.into(), next_legacy),
            (HEDGE_SYMBOL.into(), (-total - next_legacy).min(0.)),
        ]);
    }
    if old
        .get(HEDGE_SYMBOL)
        .is_some_and(|qty| (qty + total).abs() <= tolerance)
    {
        return old.clone();
    }
    let mut targets = BTreeMap::from([(HEDGE_SYMBOL.into(), -total)]);
    if old.contains_key(LEGACY_HEDGE_SYMBOL) {
        // Retain an explicit zero so restart cannot resurrect the old hedge.
        targets.insert(LEGACY_HEDGE_SYMBOL.into(), 0.);
    }
    targets
}

fn refill_quantity(settings: &Settings, latched: Option<f64>, total: f64) -> f64 {
    if let Some(target) = latched {
        (target.max(settings.target()) - total).max(0.)
    } else if total <= settings.trigger() + EPS {
        (settings.target() - total).max(0.)
    } else {
        0.
    }
}
fn validate_quote(q: &Value, requested: f64, price: f64, s: &Settings) -> Result<f64> {
    let qty = number(q, "toAmount")?;
    let cost = number(q, "fromAmount")?;
    ensure!(
        qty > 0. && (qty - requested).abs() <= EPS * 2.,
        "Convert quote does not match BNB refill quantity"
    );
    ensure!(
        cost > 0.
            && cost <= s.max_conversion_usdt
            && cost <= qty * price * (1. + s.max_quote_deviation_bps / 10_000.),
        "BNB Convert quote exceeds cost/slippage limits"
    );
    ensure!(
        q.get("validTimestamp")
            .and_then(Value::as_u64)
            .is_some_and(|t| t > now_ms() + 500),
        "BNB Convert quote expired"
    );
    Ok(cost)
}
fn decimal(v: f64) -> String {
    format!("{:.8}", (v * 1e8).floor() / 1e8)
}
fn ceil8(v: f64) -> f64 {
    (v * 1e8).ceil() / 1e8
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn string(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("Binance field {key} missing"))
}
fn add_audit(a: &mut Account, action: &str, result: &str) {
    a.audit.push(Audit {
        at_ms: now_ms(),
        action: action.into(),
        result: result.into(),
    });
    if a.audit.len() > 100 {
        a.audit.remove(0);
    }
}
fn persist(path: &std::path::Path, value: &Disk) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("BNB settings path has no parent")?;
    fs::create_dir_all(parent)?;
    let next = path.with_extension(format!("next.{}.{}", std::process::id(), now_ms()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&next)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    fs::rename(&next, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
async fn pages(api: &BinanceApi, path: &str) -> Result<Vec<Value>> {
    let mut all = vec![];
    for page in 1..=100 {
        let page = page.to_string();
        let body = api
            .get(
                false,
                path,
                &[("asset", "BNB"), ("current", &page), ("size", "100")],
            )
            .await?;
        let rows = body
            .get("rows")
            .and_then(Value::as_array)
            .context("BNB Earn rows missing")?;
        let total = body
            .get("total")
            .and_then(Value::as_u64)
            .context("BNB Earn total missing")? as usize;
        ensure!(
            rows.iter()
                .all(|r| r.get("asset").and_then(Value::as_str) == Some("BNB")),
            "BNB Earn response contains another asset"
        );
        all.extend(rows.iter().cloned());
        if all.len() >= total {
            return Ok(all);
        }
        ensure!(!rows.is_empty(), "BNB Earn pagination incomplete");
    }
    bail!("BNB Earn pagination exceeded limit")
}
async fn load_balances(api: &BinanceApi) -> Result<(Balances, Vec<Value>)> {
    let spot = api.get(false, "/api/v3/account", &[]).await?;
    let futures = api.get(true, "/fapi/v2/account", &[]).await?;
    let positions = pages(api, "/sapi/v1/simple-earn/flexible/position").await?;
    let price = api
        .public_get(false, "/api/v3/ticker/price", &[("symbol", "BNBUSDT")])
        .await?;
    let spot_rows = spot
        .get("balances")
        .and_then(Value::as_array)
        .context("spot balances missing")?;
    let future_rows = futures
        .get("assets")
        .and_then(Value::as_array)
        .context("futures assets missing")?;
    let asset = |rows: &[Value], name: &str, key: &str| -> Result<f64> {
        rows.iter()
            .find(|r| r.get("asset").and_then(Value::as_str) == Some(name))
            .map(|r| number(r, key))
            .unwrap_or(Ok(0.))
    };
    let mut earn = 0.;
    for p in &positions {
        earn += number(p, "totalAmount")?;
    }
    let b = Balances {
        at_ms: now_ms(),
        spot_free_bnb: asset(spot_rows, "BNB", "free")?,
        spot_bnb: asset(spot_rows, "BNB", "free")? + asset(spot_rows, "BNB", "locked")?,
        futures_bnb: asset(future_rows, "BNB", "walletBalance")?,
        futures_available_bnb: asset(future_rows, "BNB", "maxWithdrawAmount")?,
        earn_bnb: earn,
        spot_bfusd: asset(spot_rows, "BFUSD", "free")?,
        futures_bfusd: asset(future_rows, "BFUSD", "walletBalance")?,
        futures_available_bfusd: asset(future_rows, "BFUSD", "maxWithdrawAmount")?,
        margin_headroom_usdt: (finite_number(&futures, "totalMarginBalance")?
            - 3. * number(&futures, "totalMaintMargin")?)
        .max(0.),
        bnb_price_usdt: number(&price, "price")?,
        bnbusdc_position_qty: futures
            .get("positions")
            .and_then(Value::as_array)
            .context("BNB futures positions missing")?
            .iter()
            .filter(|p| p.get("symbol").and_then(Value::as_str) == Some(HEDGE_SYMBOL))
            .map(|p| finite_number(p, "positionAmt"))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum(),
    };
    ensure!(
        b.bnb_price_usdt > 0. && b.total().is_finite(),
        "BNB balances or price invalid"
    );
    Ok((b, positions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Request, State},
        response::{IntoResponse, Response},
        routing::any,
    };
    use serde_json::json;

    #[test]
    fn hedge_transfer_waits_for_both_legs_and_fresh_target_without_live_orders() {
        let row = crate::viz_snapshot::ExecStateRowSnapshot {
            strategy_name: HEDGE_STRATEGY.into(),
            symbol: HEDGE_SYMBOL.into(),
            position_allocated: Some(true),
            source_updated_at_ms: 1,
            current_qty: Some(-0.5),
            current_usdt: None,
            target_qty: Some(-0.5),
            pending_qty: Some(0.),
            live_order_qty: Some(0.),
            remaining_batches: 0,
            estimated_completion_ts_ms: 0,
            execution_complete: true,
            completion_reason: "target_reached".into(),
            account_position_qty: Some(-0.5),
        };
        assert!(hedge_step_settled(-0.5, Some(&row), 0.02));
        let dust = crate::viz_snapshot::ExecStateRowSnapshot {
            current_qty: Some(-0.509),
            pending_qty: Some(0.009),
            completion_reason: "target_tolerance".into(),
            ..row.clone()
        };
        assert!(hedge_step_settled(-0.5, Some(&dust), 0.02));
        assert!(!hedge_step_settled(
            -0.5,
            Some(&crate::viz_snapshot::ExecStateRowSnapshot {
                live_order_qty: Some(0.009),
                ..dust
            }),
            0.02
        ));
        assert!(!hedge_step_settled(-0.5, None, 0.02));
        for incomplete in [
            crate::viz_snapshot::ExecStateRowSnapshot {
                current_qty: Some(-0.1),
                ..row.clone()
            },
            crate::viz_snapshot::ExecStateRowSnapshot {
                target_qty: Some(-0.1),
                ..row.clone()
            },
            crate::viz_snapshot::ExecStateRowSnapshot {
                pending_qty: Some(-0.5),
                ..row.clone()
            },
            crate::viz_snapshot::ExecStateRowSnapshot {
                live_order_qty: Some(-0.5),
                ..row.clone()
            },
            crate::viz_snapshot::ExecStateRowSnapshot {
                position_allocated: None,
                ..row.clone()
            },
            crate::viz_snapshot::ExecStateRowSnapshot {
                execution_complete: false,
                ..row
            },
        ] {
            assert!(!hedge_step_settled(-0.5, Some(&incomplete), 0.02));
        }
    }

    #[test]
    fn hedge_migration_keeps_total_and_limits_each_transfer() {
        let mut old = BTreeMap::from([(LEGACY_HEDGE_SYMBOL.into(), -5.8)]);
        for _ in 0..12 {
            let next = next_hedge_targets(&old, 5.8, 0.02);
            assert!((next.values().sum::<f64>() + 5.8).abs() < EPS);
            assert!(next[LEGACY_HEDGE_SYMBOL] <= 0.);
            assert!(
                (next[LEGACY_HEDGE_SYMBOL] - old[LEGACY_HEDGE_SYMBOL]).abs()
                    <= MIGRATION_STEP_BNB + EPS
            );
            old = next;
        }
        assert_eq!(old[LEGACY_HEDGE_SYMBOL], 0.);
        assert!((old[HEDGE_SYMBOL] + 5.8).abs() < EPS);
        assert_eq!(next_hedge_targets(&old, 5.79, 0.02), old);
        assert!((next_hedge_targets(&old, 6., 0.02)[HEDGE_SYMBOL] + 6.).abs() < EPS);
    }

    #[test]
    fn hedge_cooldown_survives_clock_rollback_and_legacy_settings() {
        assert!(hedge_due(1, 0, 300));
        assert!(!hedge_due(299_999, 1, 300));
        assert!(!hedge_due(1, 300_000, 300));
        assert!(hedge_due(300_001, 1, 300));
        let settings: Settings = serde_json::from_value(json!({"interval_secs":60})).unwrap();
        assert_eq!(settings.hedge_min_interval_secs, 300);
        let invalid = Settings {
            hedge_min_interval_secs: 59,
            ..settings
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn thresholds_are_independently_configurable_and_hysteretic() {
        let s = Settings::default();
        assert!(s.validate().is_ok());
        assert_eq!(refill_quantity(&s, None, 5.21), 0.);
        assert!((refill_quantity(&s, None, 5.2) - 0.8).abs() < EPS);
        assert!((refill_quantity(&s, Some(6.), 5.7) - 0.3).abs() < EPS);
        let custom = Settings {
            required_bnb: 25.,
            refill_trigger_bnb: 25.4,
            refill_target_bnb: 27.,
            ..s.clone()
        };
        assert!(custom.validate().is_ok());
        assert!((refill_quantity(&custom, None, 25.4) - 1.6).abs() < EPS);
        for bad in [
            Settings {
                refill_trigger_bnb: 5.,
                ..s.clone()
            },
            Settings {
                refill_target_bnb: 5.1,
                ..s.clone()
            },
            Settings {
                refill_trigger_bnb: f64::NAN,
                ..s.clone()
            },
            Settings {
                futures_trigger_bnb: 0.9,
                ..s
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
    #[test]
    fn quotes_require_exact_quantity_freshness_and_cost_bound() {
        let settings = Settings::default();
        let quote = json!({"toAmount":"0.8","fromAmount":"480","validTimestamp":now_ms()+10_000});
        assert_eq!(validate_quote(&quote, 0.8, 600., &settings).unwrap(), 480.);
        assert!(validate_quote(&quote, 0.9, 600., &settings).is_err());
        assert!(validate_quote(&quote, 0.8, 500., &settings).is_err());
        let mut expired = quote;
        expired["validTimestamp"] = json!(1);
        assert!(validate_quote(&expired, 0.8, 600., &settings).is_err());
    }
    #[test]
    fn system_hedge_name_cannot_be_published_or_deleted_through_catalog() {
        assert!(crate::order_config::validate_strategy_name(HEDGE_STRATEGY).is_err());
        assert!(crate::order_config::validate_runtime_strategy_name(HEDGE_STRATEGY).is_ok());
    }

    struct Exchange {
        balances: Balances,
        futures_pair: bool,
        spot_pair: bool,
        accepted: usize,
        writes: Vec<String>,
        quote_qty: f64,
        quote_from: String,
        lose_response: bool,
        settle: bool,
    }
    impl Default for Exchange {
        fn default() -> Self {
            Self {
                balances: Balances {
                    at_ms: now_ms(),
                    futures_bnb: 1.5,
                    futures_available_bnb: 1.5,
                    earn_bnb: 3.7,
                    futures_bfusd: 10_000.,
                    futures_available_bfusd: 10_000.,
                    margin_headroom_usdt: 50_000.,
                    bnb_price_usdt: 600.,
                    ..Default::default()
                },
                futures_pair: true,
                spot_pair: false,
                accepted: 0,
                writes: vec![],
                quote_qty: 0.,
                quote_from: String::new(),
                lose_response: false,
                settle: true,
            }
        }
    }
    async fn exchange(State(exchange): State<Arc<Mutex<Exchange>>>, request: Request) -> Response {
        let path = request.uri().path().to_string();
        let query = request.uri().query().unwrap_or("").to_string();
        let method = request.method().clone();
        let body = axum::body::to_bytes(request.into_body(), 16_384)
            .await
            .unwrap();
        let encoded = if method == reqwest::Method::GET {
            query
        } else {
            String::from_utf8(body.to_vec()).unwrap()
        };
        let url = reqwest::Url::parse(&format!("http://fixture/?{encoded}")).unwrap();
        let params: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let mut e = exchange.lock().await;
        let num = |key: &str| {
            params
                .get(key)
                .map(|v| v.parse::<f64>().unwrap())
                .unwrap_or(0.)
        };
        let response = if path.ends_with("/exchangeInfo") {
            let from = params["fromAsset"].clone();
            assert_eq!(from, "BFUSD");
            if (path.starts_with("/fapi") && e.futures_pair)
                || (path.starts_with("/sapi") && e.spot_pair)
            {
                json!([{"fromAsset":from,"toAsset":"BNB","toAssetMinAmount":"0.01","toAssetMaxAmount":"1000","fromAssetMinAmount":"1","fromAssetMaxAmount":"1000000"}])
            } else {
                json!([])
            }
        } else if path.ends_with("/getQuote") {
            e.quote_qty = num("toAmount");
            e.quote_from = params["fromAsset"].clone();
            json!({"quoteId":"fixture-quote","toAmount":decimal(e.quote_qty),"fromAmount":decimal(e.quote_qty*600.),"validTimestamp":now_ms()+10_000})
        } else if path.ends_with("/acceptQuote") {
            e.accepted += 1;
            e.writes.push(path.clone());
            let qty = e.quote_qty;
            if path.starts_with("/fapi") {
                e.balances.futures_bnb += qty;
                e.balances.futures_bfusd -= qty * 600.;
                e.balances.futures_available_bfusd -= qty * 600.;
            } else {
                e.balances.spot_bnb += qty;
                e.balances.spot_free_bnb += qty;
                assert_eq!(e.quote_from, "BFUSD");
                e.balances.spot_bfusd -= qty * 600.;
            }
            if e.lose_response {
                return "interrupted response".into_response();
            }
            json!({"orderId":"fixture-order","orderStatus":"PROCESS"})
        } else if path.ends_with("/orderStatus") {
            json!({"orderStatus":if e.settle {"SUCCESS"} else {"PROCESS"},"orderId":"fixture-order"})
        } else if path.ends_with("/asset/transfer") {
            e.writes.push(path.clone());
            let amount = num("amount");
            if params["asset"] == "BFUSD" {
                let signed = if params["type"] == "MAIN_UMFUTURE" {
                    -amount
                } else {
                    amount
                };
                e.balances.futures_bfusd -= signed;
                e.balances.futures_available_bfusd -= signed;
                e.balances.spot_bfusd += signed;
            } else if params["type"] == "MAIN_UMFUTURE" {
                e.balances.spot_bnb -= amount;
                e.balances.spot_free_bnb -= amount;
                e.balances.futures_bnb += amount;
            } else {
                e.balances.futures_bnb -= amount;
                e.balances.spot_bnb += amount;
                e.balances.spot_free_bnb += amount;
            }
            json!({"tranId":1234})
        } else if path == "/api/v3/account" {
            let b = &e.balances;
            json!({"balances":[{"asset":"BNB","free":decimal(b.spot_free_bnb),"locked":"0"},{"asset":"BFUSD","free":decimal(b.spot_bfusd)},{"asset":"USDT","free":"777"}]})
        } else if path == "/fapi/v2/account" {
            let b = &e.balances;
            json!({"totalMarginBalance":"50000","totalMaintMargin":"0","positions":[{"symbol":"BNBUSDC","positionAmt":"0"}],"assets":[{"asset":"BNB","walletBalance":decimal(b.futures_bnb),"maxWithdrawAmount":decimal(b.futures_available_bnb)},{"asset":"BFUSD","walletBalance":decimal(b.futures_bfusd),"maxWithdrawAmount":decimal(b.futures_available_bfusd)}]})
        } else if path.ends_with("/flexible/position") {
            json!({"rows":[{"asset":"BNB","totalAmount":decimal(e.balances.earn_bnb),"productId":"BNB001","canRedeem":true,"collateralAmount":"0"}],"total":1})
        } else if path.ends_with("/ticker/price") {
            json!({"price":"600"})
        } else if path.ends_with("/feeBurn") {
            json!({"feeBurn":true})
        } else if path.ends_with("/flexible/redeem") {
            e.writes.push(path.clone());
            let amount = num("amount");
            e.balances.earn_bnb -= amount;
            e.balances.spot_bnb += amount;
            e.balances.spot_free_bnb += amount;
            json!({"success":true,"redeemId":321})
        } else {
            panic!("unexpected fixture endpoint: {path}")
        };
        Json(response).into_response()
    }
    struct Fixture {
        _dir: tempfile::TempDir,
        task: tokio::task::JoinHandle<()>,
        hub: BnbManager,
        config: AppConfig,
        source: SourceConfig,
        api: BinanceApi,
        exchange: Arc<Mutex<Exchange>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Fixture {
        async fn new(exchange_state: Exchange) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config:AppConfig=toml::from_str(&format!("[database]\n[kline]\nrocksdb_path='{}'\n[[sources]]\nid='fixture'\naccount='fixture'\nvenue='binance-futures'\nrocksdb_path='{}'\n",dir.path().join("db").display(),dir.path().join("exec/data/persist_manager").display())).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let exchange = Arc::new(Mutex::new(exchange_state));
            let router = Router::new()
                .fallback(any(super::tests::exchange))
                .with_state(exchange.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let env = BTreeMap::from([
                ("BINANCE_API_KEY".into(), "fixture-not-a-key".into()),
                ("BINANCE_API_SECRET".into(), "fixture-not-a-secret".into()),
                ("BINANCE_FAPI_URL".into(), format!("http://{address}")),
                ("BINANCE_SAPI_URL".into(), format!("http://{address}")),
            ]);
            let api = BinanceApi::new(&env, "127.0.0.1".parse().unwrap()).unwrap();
            let hub = BnbManager::new(
                &config,
                Arc::new(Mutex::new(())),
                TreasuryEgress::new(config.treasury.clone()),
                RedisRuntime::connect(config.redis.clone()).unwrap(),
                ReloadNotifyHub::spawn(),
            )
            .unwrap();
            let source = config.sources[0].clone();
            Self {
                _dir: dir,
                task,
                hub,
                config,
                source,
                api,
                exchange,
            }
        }
        fn account() -> Account {
            Account {
                settings: Settings {
                    enabled: true,
                    dry_run: false,
                    ..Default::default()
                },
                ..Default::default()
            }
        }
        async fn balances(&self) -> Balances {
            self.exchange.lock().await.balances.clone()
        }
    }
    #[tokio::test]
    async fn direct_conversion_is_reconciled_after_lost_response_and_restart() {
        let f = Fixture::new(Exchange {
            lose_response: true,
            settle: false,
            ..Default::default()
        })
        .await;
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        let b = f.balances().await;
        assert!(
            f.hub
                .manage_funds(&f.api, &f.source, &mut state, &mut a, &b, &[])
                .await
                .is_err()
        );
        assert_eq!(f.exchange.lock().await.accepted, 1);
        assert!(a.pending.is_some());
        drop(state);
        let restarted = BnbManager::new(
            &f.config,
            Arc::new(Mutex::new(())),
            TreasuryEgress::new(f.config.treasury.clone()),
            RedisRuntime::connect(f.config.redis.clone()).unwrap(),
            ReloadNotifyHub::spawn(),
        )
        .unwrap();
        let mut state = restarted.state.lock().await;
        let mut restored = state.disk.accounts[&f.source.id].clone();
        assert!(restored.pending.as_ref().unwrap().response.is_none());
        assert!(
            !restarted
                .reconcile(
                    &f.api,
                    &mut state,
                    &f.source,
                    &mut restored,
                    &f.balances().await
                )
                .await
                .unwrap()
        );
        // Funds arrived, but the quote is still processing. Never submit again.
        assert_eq!(f.exchange.lock().await.accepted, 1);
        f.exchange.lock().await.settle = true;
        let mut received = f.balances().await;
        received.futures_bnb -= 0.0001; // fee after purchase
        assert!(
            restarted
                .reconcile(&f.api, &mut state, &f.source, &mut restored, &received)
                .await
                .unwrap()
        );
        assert!(restored.pending.is_none());
        assert!(restored.refill_target.is_none());
        assert_eq!(
            refill_quantity(&restored.settings, restored.refill_target, received.total()),
            0.
        );
        assert_eq!(f.exchange.lock().await.accepted, 1);
    }
    #[tokio::test]
    async fn futures_direct_route_can_use_existing_spot_bfusd() {
        let mut exchange = Exchange::default();
        exchange.balances.futures_available_bfusd = 0.;
        exchange.balances.futures_bfusd = 0.;
        exchange.balances.spot_bfusd = 1000.;
        let f = Fixture::new(exchange).await;
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        for _ in 0..2 {
            f.hub
                .manage_funds(
                    &f.api,
                    &f.source,
                    &mut state,
                    &mut a,
                    &f.balances().await,
                    &[],
                )
                .await
                .unwrap();
            assert!(
                f.hub
                    .reconcile(&f.api, &mut state, &f.source, &mut a, &f.balances().await)
                    .await
                    .unwrap()
            );
        }
        let e = f.exchange.lock().await;
        assert_eq!(e.accepted, 1);
        assert!((e.balances.total() - 6.).abs() < EPS * 2.);
        assert_eq!(
            e.writes,
            vec!["/sapi/v1/asset/transfer", "/fapi/v1/convert/acceptQuote"]
        );
    }

    #[tokio::test]
    async fn unsupported_direct_pair_never_redeems_or_uses_usdt() {
        let f = Fixture::new(Exchange {
            futures_pair: false,
            ..Default::default()
        })
        .await;
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        for _ in 0..2 {
            let error = f
                .hub
                .manage_funds(
                    &f.api,
                    &f.source,
                    &mut state,
                    &mut a,
                    &f.balances().await,
                    &[],
                )
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("direct Convert pair unavailable")
            );
            assert!(a.pending.is_none());
        }
        let e = f.exchange.lock().await;
        assert_eq!(e.accepted, 0);
        assert!(e.writes.is_empty());
        assert_eq!(e.balances.futures_bfusd, 10_000.);
    }

    #[tokio::test]
    async fn spot_direct_conversion_uses_only_bfusd() {
        let f = Fixture::new(Exchange {
            futures_pair: false,
            spot_pair: true,
            ..Default::default()
        })
        .await;
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        for _ in 0..2 {
            f.hub
                .manage_funds(
                    &f.api,
                    &f.source,
                    &mut state,
                    &mut a,
                    &f.balances().await,
                    &[],
                )
                .await
                .unwrap();
            assert!(
                f.hub
                    .reconcile(&f.api, &mut state, &f.source, &mut a, &f.balances().await)
                    .await
                    .unwrap()
            );
        }
        let e = f.exchange.lock().await;
        assert_eq!(e.accepted, 1);
        assert_eq!(e.quote_from, "BFUSD");
        assert!((e.balances.total() - 6.).abs() < EPS * 2.);
        assert_eq!(
            e.writes,
            vec!["/sapi/v1/asset/transfer", "/sapi/v1/convert/acceptQuote"]
        );
    }
    #[tokio::test]
    async fn receiving_balance_must_arrive_and_unknown_transfer_never_repeats() {
        let f = Fixture::new(Exchange::default()).await;
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        let b = f.balances().await;
        f.hub
            .transfer_bfusd(&f.api, &f.source, &mut state, &mut a, &b, 100.)
            .await
            .unwrap();
        assert!(
            !f.hub
                .reconcile(&f.api, &mut state, &f.source, &mut a, &b)
                .await
                .unwrap()
        );
        a.pending.as_mut().unwrap().response = None;
        assert!(
            f.hub
                .reconcile(&f.api, &mut state, &f.source, &mut a, &f.balances().await)
                .await
                .is_err()
        );
        assert!(
            f.hub
                .transfer_bfusd(&f.api, &f.source, &mut state, &mut a, &b, 100.)
                .await
                .is_err()
        );
        assert_eq!(f.exchange.lock().await.writes.len(), 1);
    }
    #[tokio::test]
    async fn fee_wallet_refill_moves_existing_bnb_without_buying() {
        let f = Fixture::new(Exchange {
            balances: Balances {
                futures_bnb: 1.1,
                earn_bnb: 4.9,
                bnb_price_usdt: 600.,
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        let positions = vec![
            json!({"asset":"BNB","productId":"BNB001","totalAmount":"4.9","collateralAmount":"0","canRedeem":true}),
        ];
        let b = f.balances().await;
        f.hub
            .manage_funds(&f.api, &f.source, &mut state, &mut a, &b, &positions)
            .await
            .unwrap();
        f.hub
            .reconcile(&f.api, &mut state, &f.source, &mut a, &f.balances().await)
            .await
            .unwrap();
        f.hub
            .manage_funds(
                &f.api,
                &f.source,
                &mut state,
                &mut a,
                &f.balances().await,
                &positions,
            )
            .await
            .unwrap();
        let e = f.exchange.lock().await;
        assert_eq!(e.accepted, 0);
        assert!((e.balances.futures_bnb - 1.5).abs() < EPS);
        assert!((e.balances.total() - 6.).abs() < EPS);
    }
    #[tokio::test]
    async fn dry_run_defaults_are_read_only() {
        let f = Fixture::new(Exchange::default()).await;
        assert!(Settings::default().dry_run);
        assert!(!Settings::default().enabled);
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        let result = f
            .hub
            .execute_with_api(&f.api, &f.source, &mut state, &mut a, true)
            .await
            .unwrap();
        assert!(result.contains("buy 0.80000000"));
        assert!(a.pending.is_none() && a.refill_target.is_none());
        assert!(!f.hub.path.exists());
        assert!(f.exchange.lock().await.writes.is_empty());
    }
    #[tokio::test]
    async fn bnb_runs_independently_of_bfusd_pause() {
        let f = Fixture::new(Exchange {
            settle: false,
            ..Default::default()
        })
        .await;
        let path = f.hub.path.with_file_name("bfusd-auto.json");
        let original = br#"{"accounts":{"fixture":{"enabled":true,"paused":true}}}"#;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, original).unwrap();
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        f.hub
            .execute_with_api(&f.api, &f.source, &mut state, &mut a, false)
            .await
            .unwrap();
        assert_eq!(f.exchange.lock().await.accepted, 1);
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[tokio::test]
    async fn local_budget_exhaustion_never_creates_an_unknown_transaction() {
        let f = Fixture::new(Exchange::default()).await;
        for _ in 0..24 {
            f.api
                .get(
                    true,
                    "/fapi/v1/convert/orderStatus",
                    &[("quoteId", "fixture-quote")],
                )
                .await
                .unwrap();
        }
        let mut state = f.hub.state.lock().await;
        let mut a = Fixture::account();
        let error = f
            .hub
            .submit(
                &f.api,
                &f.source,
                &mut state,
                &mut a,
                &f.balances().await,
                true,
                "/fapi/v1/convert/acceptQuote",
                vec![("quoteId", "fixture-quote".into())],
                "convert BFUSD to BNB",
                "total",
                0.8,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .downcast_ref::<crate::treasury_rate_limit::NotSent>()
                .is_some()
        );
        assert!(a.pending.is_none());
        assert!(!f.hub.path.exists());
        assert!(f.exchange.lock().await.writes.is_empty());
    }
}
