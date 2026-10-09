use std::collections::BTreeMap;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::auth;
use crate::config::{AppConfig, SourceConfig, TreasuryConfig};
use crate::exchange_leverage::parse_env_file;
use crate::treasury_rate_limit::Limiter;

type HmacSha256 = Hmac<Sha256>;
const MIN_SPOT_BFUSD_TRANSFER: f64 = 0.0001;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub round_cap_usdt: f64,
    pub trigger_usdt: f64,
    pub paused: bool,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 3600,
            round_cap_usdt: 5000.0,
            trigger_usdt: 100.0,
            paused: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DiskSettings {
    token_hash: String,
    accounts: BTreeMap<String, AccountSettings>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountStatus {
    #[serde(flatten)]
    pub settings: AccountSettings,
    pub running: bool,
    pub last_result: Option<String>,
}

#[derive(Default)]
struct Runtime {
    disk: DiskSettings,
    next_due: BTreeMap<String, Instant>,
    last_result: BTreeMap<String, String>,
    running: Option<String>,
}

/// A shared round cursor; every request within a financial round keeps one IP.
#[derive(Clone)]
pub(crate) struct TreasuryEgress {
    settings: TreasuryConfig,
    rotation: Arc<Mutex<BTreeMap<String, usize>>>,
}
impl TreasuryEgress {
    pub(crate) fn new(settings: TreasuryConfig) -> Self {
        Self {
            settings,
            rotation: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub(crate) fn configured(&self) -> bool {
        self.settings.use_account_ip_rotation || self.settings.local_ip.is_some()
    }
    pub(crate) async fn select(&self, source: &SourceConfig) -> Result<IpAddr> {
        if self.settings.use_account_ip_rotation {
            let ips = load_rotation_ips(source)?;
            let mut rotation = self.rotation.lock().await;
            let next = rotation.entry(source.id.clone()).or_default();
            let ip = ips[*next % ips.len()];
            *next = next.wrapping_add(1);
            return Ok(ip);
        }
        let ip = self.settings.local_ip.context(
            "configure treasury.local_ip or explicitly enable treasury.use_account_ip_rotation",
        )?;
        validate_treasury_ip(source, ip)?;
        Ok(ip)
    }
}

#[derive(Clone)]
pub struct AutoEarnHub {
    path: PathBuf,
    runtime: Arc<Mutex<Runtime>>,
    gate: Arc<Mutex<()>>,
    egress: TreasuryEgress,
}

impl AutoEarnHub {
    pub(crate) fn new(
        config: &AppConfig,
        gate: Arc<Mutex<()>>,
        egress: TreasuryEgress,
    ) -> Result<Self> {
        let root = config
            .kline
            .rocksdb_path
            .parent()
            .context("Manager RocksDB path must have a parent")?;
        let path = root.join("config/bfusd-auto.json");
        let disk = if path.exists() {
            serde_json::from_slice(&fs::read(&path).context("failed to read BFUSD settings")?)
                .context("invalid BFUSD settings")?
        } else {
            DiskSettings::default()
        };
        Ok(Self {
            path,
            gate,
            egress,
            runtime: Arc::new(Mutex::new(Runtime {
                disk,
                ..Runtime::default()
            })),
        })
    }

    pub async fn status(&self, source_id: &str) -> AccountStatus {
        let state = self.runtime.lock().await;
        AccountStatus {
            settings: state
                .disk
                .accounts
                .get(source_id)
                .cloned()
                .unwrap_or_default(),
            running: state.running.as_deref() == Some(source_id),
            last_result: state.last_result.get(source_id).cloned(),
        }
    }

    pub async fn save(
        &self,
        source_id: &str,
        token: Option<&str>,
        mut settings: AccountSettings,
    ) -> Result<AccountStatus> {
        validate_settings(&settings)?;
        let mut state = self.runtime.lock().await;
        self.check_token(&state.disk, token)?;
        settings.paused = state
            .disk
            .accounts
            .get(source_id)
            .is_some_and(|old| old.paused);
        let old = state.disk.clone();
        state
            .disk
            .accounts
            .insert(source_id.to_owned(), settings.clone());
        if let Err(error) = self.persist(&state.disk) {
            state.disk = old;
            return Err(error);
        }
        state.next_due.insert(source_id.to_owned(), Instant::now());
        Ok(AccountStatus {
            settings,
            running: false,
            last_result: state.last_result.get(source_id).cloned(),
        })
    }

    pub async fn resume(&self, source_id: &str, token: Option<&str>) -> Result<AccountStatus> {
        let mut state = self.runtime.lock().await;
        self.check_token(&state.disk, token)?;
        let old = state.disk.clone();
        state
            .disk
            .accounts
            .entry(source_id.to_owned())
            .or_default()
            .paused = false;
        if let Err(error) = self.persist(&state.disk) {
            state.disk = old;
            return Err(error);
        }
        state.next_due.insert(source_id.to_owned(), Instant::now());
        state.last_result.remove(source_id);
        Ok(AccountStatus {
            settings: state.disk.accounts[source_id].clone(),
            running: false,
            last_result: None,
        })
    }

    pub async fn run(
        &self,
        source: &SourceConfig,
        token: Option<&str>,
        manual: bool,
    ) -> Result<String> {
        if manual {
            self.verify_token(token).await?;
        }
        let _funds = self.gate.lock().await;
        let mut state = self.runtime.lock().await;
        if manual {
            self.check_token(&state.disk, token)?;
        }
        let settings = state
            .disk
            .accounts
            .get(&source.id)
            .cloned()
            .unwrap_or_default();
        if !source.enabled || !settings.enabled || settings.paused {
            bail!("automatic BFUSD is disabled or paused for this account");
        }
        if !manual
            && state
                .next_due
                .get(&source.id)
                .is_some_and(|due| *due > Instant::now())
        {
            return Ok("not due".to_owned());
        }
        state.next_due.insert(
            source.id.clone(),
            Instant::now() + Duration::from_secs(settings.interval_secs),
        );
        state.running = Some(source.id.clone());
        let result = self.execute(source, &settings, &mut state).await;
        state.running = None;
        let summary = match result {
            Ok(message) => message,
            Err(error) => {
                let message = format!("{error:#}");
                error!(source_id = %source.id, error = %message, "automatic BFUSD round failed");
                state.last_result.insert(source.id.clone(), message.clone());
                return Err(error);
            }
        };
        info!(source_id = %source.id, result = %summary, "automatic BFUSD round finished");
        state.last_result.insert(source.id.clone(), summary.clone());
        Ok(summary)
    }

    pub fn spawn(&self, config: Vec<SourceConfig>) {
        let hub = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                for source in &config {
                    let active = {
                        let state = hub.runtime.lock().await;
                        state
                            .disk
                            .accounts
                            .get(&source.id)
                            .is_some_and(|s| s.enabled && !s.paused)
                            && state
                                .next_due
                                .get(&source.id)
                                .is_none_or(|due| *due <= Instant::now())
                    };
                    if active {
                        let _ = hub.run(source, None, false).await;
                    }
                }
            }
        });
    }

    async fn execute(
        &self,
        source: &SourceConfig,
        settings: &AccountSettings,
        state: &mut Runtime,
    ) -> Result<String> {
        if source.venue != "binance-futures" {
            bail!("automatic BFUSD requires a Binance futures source");
        }
        let env = parse_env_file(&source.env_path())?;
        let mode = env
            .get("BINANCE_ACCOUNT_MODE")
            .map(String::as_str)
            .unwrap_or_default();
        if !mode.eq_ignore_ascii_case("standard") && !mode.eq_ignore_ascii_case("std") {
            bail!("automatic BFUSD requires a Binance STANDARD account");
        }
        let ip = self.egress.select(source).await?;
        let api = BinanceApi::new(&env, ip)?;
        let account = api.get(true, "/fapi/v2/account", &[]).await?;
        if account.get("multiAssetsMargin").and_then(Value::as_bool) != Some(true) {
            bail!("Binance Multi-Assets Mode must be enabled for BFUSD collateral");
        }
        let swept = self.sweep_spot_bfusd(source, state, &api).await?;
        let subscribed = self
            .subscribe_futures_usdt(source, settings, state, &api, &account, ip)
            .await;
        match (swept, subscribed) {
            (Some(sweep), Ok(subscription)) => Ok(format!("{sweep}; {subscription}")),
            (Some(sweep), Err(error)) => Err(error.context(format!("{sweep}; USDT round failed"))),
            (None, result) => result,
        }
    }

    async fn sweep_spot_bfusd(
        &self,
        source: &SourceConfig,
        state: &mut Runtime,
        api: &BinanceApi,
    ) -> Result<Option<String>> {
        let spot = api.get(false, "/api/v3/account", &[]).await?;
        let Some(amount) = spot_bfusd_transfer_amount(&spot)? else {
            return Ok(None);
        };
        let transfer_permit = api.reserve_post(false, "/sapi/v1/asset/transfer")?;
        self.set_paused(state, &source.id, true)?;
        let transfer = api
            .post_reserved(
                transfer_permit,
                &[
                    ("type", "MAIN_UMFUTURE"),
                    ("asset", "BFUSD"),
                    ("amount", &amount),
                ],
            )
            .await?;
        let transfer_id = transfer
            .get("tranId")
            .and_then(Value::as_u64)
            .context("spot BFUSD transfer did not return tranId")?;
        self.set_paused(state, &source.id, false)?;
        Ok(Some(format!(
            "transferred {amount} spot BFUSD to futures (tranId {transfer_id})"
        )))
    }

    async fn subscribe_futures_usdt(
        &self,
        source: &SourceConfig,
        settings: &AccountSettings,
        state: &mut Runtime,
        api: &BinanceApi,
        account: &Value,
        ip: IpAddr,
    ) -> Result<String> {
        let usdt = account
            .get("assets")
            .and_then(Value::as_array)
            .and_then(|assets| {
                assets
                    .iter()
                    .find(|asset| asset.get("asset").and_then(Value::as_str) == Some("USDT"))
            })
            .context("USDT asset missing from Binance account")?;
        let wallet = finite_number(usdt, "walletBalance")?;
        let withdrawable = number(usdt, "maxWithdrawAmount")?;
        if wallet.min(withdrawable) <= settings.trigger_usdt {
            return Ok(format!(
                "skipped: USDT wallet {:.2}, max withdrawable {:.2} is below trigger",
                wallet, withdrawable
            ));
        }
        let margin = number(&account, "totalMarginBalance")?;
        let maintenance = number(&account, "totalMaintMargin")?;
        if margin - maintenance * 3.0 <= settings.trigger_usdt {
            return Ok("skipped: insufficient margin headroom".to_owned());
        }
        let quota = api.get(false, "/sapi/v1/bfusd/quota", &[]).await?;
        let left = quota
            .get("subscriptionQuota")
            .context("BFUSD subscription quota missing")?;
        let left = number(left, "leftQuota")?;
        let amount_cents = round_amount_cents(
            wallet,
            withdrawable,
            margin,
            maintenance,
            left,
            settings.round_cap_usdt,
        );
        if amount_cents < 100 || amount_cents as f64 / 100.0 <= settings.trigger_usdt {
            return Ok("skipped: transferable USDT or BFUSD quota below trigger".to_owned());
        }
        let amount_text = format!("{}.{:02}", amount_cents / 100, amount_cents % 100);

        // A crash or ambiguous response leaves the account stopped for reconciliation.
        let outbound = api.reserve_post(false, "/sapi/v1/asset/transfer")?;
        let purchase = api.reserve_post(false, "/sapi/v1/bfusd/subscribe")?;
        let inbound = api.reserve_post(false, "/sapi/v1/asset/transfer")?;
        self.set_paused(state, &source.id, true)?;
        api.post_reserved(
            outbound,
            &[
                ("type", "UMFUTURE_MAIN"),
                ("asset", "USDT"),
                ("amount", &amount_text),
            ],
        )
        .await?;
        let subscription = api
            .post_reserved(purchase, &[("asset", "USDT"), ("amount", &amount_text)])
            .await?;
        if subscription.get("success").and_then(Value::as_bool) != Some(true) {
            bail!("BFUSD subscription did not report success; reconcile before resuming");
        }
        let bfusd = subscription
            .get("bfusdAmount")
            .and_then(Value::as_str)
            .context("BFUSD subscription did not return bfusdAmount")?;
        let fee = subscription_fee(&amount_text, bfusd)?;
        api.post_reserved(
            inbound,
            &[
                ("type", "MAIN_UMFUTURE"),
                ("asset", "BFUSD"),
                ("amount", bfusd),
            ],
        )
        .await?;
        if fee > 0.0 {
            return Ok(format!(
                "subscribed {amount_text} USDT into {bfusd} BFUSD using {ip}; purchase fee {fee:.8} USDT detected, automatic BFUSD paused"
            ));
        }
        self.set_paused(state, &source.id, false)?;
        Ok(format!(
            "subscribed {amount_text} USDT into {bfusd} BFUSD using {ip}; purchase fee 0 USDT"
        ))
    }

    fn set_paused(&self, state: &mut Runtime, source_id: &str, paused: bool) -> Result<()> {
        let old = state.disk.clone();
        state
            .disk
            .accounts
            .entry(source_id.to_owned())
            .or_default()
            .paused = paused;
        if let Err(error) = self.persist(&state.disk) {
            state.disk = old;
            return Err(error);
        }
        Ok(())
    }

    pub async fn verify_token(&self, token: Option<&str>) -> Result<()> {
        self.check_token(&self.runtime.lock().await.disk, token)
    }

    fn check_token(&self, disk: &DiskSettings, token: Option<&str>) -> Result<()> {
        if disk.token_hash.is_empty() || !auth::publish_token_matches(token, &disk.token_hash) {
            bail!("valid automatic earn operation token required");
        }
        Ok(())
    }

    fn persist(&self, settings: &DiskSettings) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("BFUSD settings path has no parent")?;
        fs::create_dir_all(parent)?;
        let temp = self.path.with_extension(format!(
            "json.next.{}.{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let bytes = serde_json::to_vec_pretty(settings)?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        fs::write(&temp, &bytes)?;
        fs::rename(&temp, &self.path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

fn validate_settings(settings: &AccountSettings) -> Result<()> {
    if !(60..=86400).contains(&settings.interval_secs) {
        bail!("interval must be between 60 and 86400 seconds");
    }
    if !settings.round_cap_usdt.is_finite()
        || !(1.0..=1_000_000.0).contains(&settings.round_cap_usdt)
    {
        bail!("round cap must be between 1 and 1000000 USDT");
    }
    if !settings.trigger_usdt.is_finite() || !(0.0..=1_000_000.0).contains(&settings.trigger_usdt) {
        bail!("trigger must be between 0 and 1000000 USDT");
    }
    Ok(())
}

fn round_amount_cents(
    wallet: f64,
    withdrawable: f64,
    margin: f64,
    maintenance: f64,
    quota: f64,
    cap: f64,
) -> i64 {
    let margin_headroom = (margin - maintenance * 3.0).max(0.0);
    [wallet, withdrawable, margin_headroom, quota, cap]
        .into_iter()
        .fold(f64::INFINITY, f64::min)
        .mul_add(100.0, 0.0)
        .floor() as i64
}

fn load_rotation_ips(source: &SourceConfig) -> Result<Vec<IpAddr>> {
    let path = source
        .env_path()
        .parent()
        .context("source env file has no parent")?
        .join("trade_engine.toml");
    let value: toml::Value = fs::read_to_string(&path)
        .context("failed to read account IP configuration")?
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid trade_engine.toml"))?;
    let ips = value
        .get("local_ips")
        .and_then(toml::Value::as_array)
        .context("trade_engine.toml local_ips is missing")?;
    let ips = ips
        .iter()
        .map(|v| -> Result<IpAddr> {
            let ip: IpAddr = v
                .as_str()
                .context("local_ips entry is not a string")?
                .parse()
                .context("invalid local_ips entry")?;
            if ip.is_unspecified() || ip.is_loopback() {
                bail!("rotation requires explicit non-loopback local_ips");
            }
            Ok(ip)
        })
        .collect::<Result<Vec<_>>>()?;
    if ips.is_empty() {
        bail!("trade_engine.toml local_ips is empty");
    }
    Ok(ips)
}

pub(crate) fn validate_treasury_ip(source: &SourceConfig, ip: IpAddr) -> Result<()> {
    if ip.is_unspecified() || ip.is_loopback() {
        bail!("treasury.local_ip must be an explicit non-loopback source address");
    }
    if load_account_ips(source)?.contains(&ip) {
        bail!("treasury.local_ip must not share a trading local_ip");
    }
    if source.env_path().starts_with("/home/el01")
        && !["154.197.32.9", "154.197.32.10"].contains(&ip.to_string().as_str())
    {
        bail!("el01 treasury.local_ip must use a reserved special-operation address");
    }
    Ok(())
}

fn load_account_ips(source: &SourceConfig) -> Result<Vec<IpAddr>> {
    let path = source
        .env_path()
        .parent()
        .context("source env file has no parent")?
        .join("trade_engine.toml");
    let value: toml::Value = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {} trade_engine.toml", source.id))?
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid trade_engine.toml"))?;
    let ips = value
        .get("local_ips")
        .and_then(toml::Value::as_array)
        .context("trade_engine.toml local_ips is missing")?;
    let mut parsed: Vec<IpAddr> = ips
        .iter()
        .map(|ip| {
            let text = ip.as_str().context("local_ips entry is not a string")?;
            let parsed: IpAddr = text.parse().context("invalid local_ips entry")?;
            if parsed.is_unspecified() {
                bail!("local_ips must contain explicit source addresses");
            }
            Ok(parsed)
        })
        .collect::<Result<_>>()?;
    for field in [
        "primary_local_ip",
        "secondary_local_ip",
        "binance_um_whitelist_ip",
        "binance_um_ip_whitelist_ip",
    ] {
        if let Some(address) = value
            .get(field)
            .and_then(toml::Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            let address: IpAddr = address.trim().parse().context("invalid trading IP field")?;
            if address.is_unspecified() {
                bail!("trading IP fields must be explicit before enabling treasury");
            }
            if !parsed.contains(&address) {
                parsed.push(address);
            }
        }
    }
    if parsed.is_empty() {
        bail!("trade_engine.toml local_ips is empty");
    }
    Ok(parsed)
}

pub(crate) fn number(value: &Value, key: &str) -> Result<f64> {
    let amount = finite_number(value, key)?;
    if amount < 0.0 {
        bail!("Binance field {key} is invalid");
    }
    Ok(amount)
}

fn spot_bfusd_transfer_amount(account: &Value) -> Result<Option<String>> {
    let balances = account
        .get("balances")
        .and_then(Value::as_array)
        .context("Binance spot account balances missing")?;
    let Some(asset) = balances
        .iter()
        .find(|asset| asset.get("asset").and_then(Value::as_str) == Some("BFUSD"))
    else {
        return Ok(None);
    };
    if number(asset, "free")? < MIN_SPOT_BFUSD_TRANSFER {
        return Ok(None);
    }
    let amount = asset
        .get("free")
        .and_then(Value::as_str)
        .context("Binance spot BFUSD free balance is not a decimal string")?;
    Ok(Some(amount.to_owned()))
}

fn subscription_fee(spent_usdt: &str, received_bfusd: &str) -> Result<f64> {
    let spent = spent_usdt.parse::<f64>()?;
    let received = received_bfusd.parse::<f64>()?;
    if !spent.is_finite() || !received.is_finite() || received <= 0.0 {
        bail!("BFUSD subscription returned invalid bfusdAmount");
    }
    let difference = spent - received;
    if difference < -0.000000005 {
        bail!("BFUSD subscription returned more BFUSD than USDT spent");
    }
    Ok(if difference > 0.000000005 {
        difference
    } else {
        0.0
    })
}

pub(crate) fn finite_number(value: &Value, key: &str) -> Result<f64> {
    let field = value
        .get(key)
        .with_context(|| format!("Binance field {key} missing"))?;
    let amount = field
        .as_str()
        .map(str::parse::<f64>)
        .transpose()?
        .or_else(|| field.as_f64())
        .with_context(|| format!("Binance field {key} is not numeric"))?;
    if !amount.is_finite() {
        bail!("Binance field {key} is invalid");
    }
    Ok(amount)
}

pub(crate) struct BinanceApi {
    client: Client,
    key: String,
    secret: String,
    fapi: String,
    sapi: String,
    fapi_limit: Limiter,
    sapi_limit: Limiter,
}

pub(crate) struct PostPermit {
    futures: bool,
    path: String,
}

impl BinanceApi {
    pub(crate) fn new(env: &BTreeMap<String, String>, ip: IpAddr) -> Result<Self> {
        let required = |key: &str| -> Result<String> {
            env.get(key)
                .filter(|v| !v.is_empty())
                .cloned()
                .with_context(|| format!("{key} missing"))
        };
        let fapi = env
            .get("BINANCE_FAPI_URL")
            .cloned()
            .unwrap_or_else(|| "https://fapi.binance.com".to_owned());
        let sapi = env
            .get("BINANCE_SAPI_URL")
            .or_else(|| env.get("BINANCE_API_URL"))
            .cloned()
            .unwrap_or_else(|| "https://api.binance.com".to_owned());
        Ok(Self {
            client: Client::builder()
                .local_address(ip)
                .no_proxy()
                .timeout(Duration::from_secs(15))
                .build()?,
            key: required("BINANCE_API_KEY")?,
            secret: required("BINANCE_API_SECRET")?,
            fapi_limit: Limiter::shared(ip, &fapi)?,
            sapi_limit: Limiter::shared(ip, &sapi)?,
            fapi,
            sapi,
        })
    }

    pub(crate) async fn get(
        &self,
        futures: bool,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        self.request(Method::GET, futures, path, params).await
    }

    pub(crate) async fn post(
        &self,
        futures: bool,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        self.request(Method::POST, futures, path, params).await
    }

    fn limiter(&self, futures: bool) -> &Limiter {
        if futures {
            &self.fapi_limit
        } else {
            &self.sapi_limit
        }
    }

    pub(crate) fn reserve_post(&self, futures: bool, path: &str) -> Result<PostPermit> {
        self.limiter(futures).reserve(&Method::POST, path)?;
        Ok(PostPermit {
            futures,
            path: path.into(),
        })
    }

    pub(crate) async fn post_reserved(
        &self,
        permit: PostPermit,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        self.request_reserved(Method::POST, permit.futures, &permit.path, params)
            .await
    }

    pub(crate) async fn public_get(
        &self,
        futures: bool,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        self.limiter(futures).reserve(&Method::GET, path)?;
        let base = if futures { &self.fapi } else { &self.sapi };
        let url = Url::parse_with_params(&format!("{}{path}", base.trim_end_matches('/')), params)?;
        let response = self
            .client
            .get(url)
            .header("X-MBX-APIKEY", &self.key)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Binance {path} request failed"))?;
        self.limiter(futures).observe(path, &response)?;
        if !response.status().is_success() {
            bail!("Binance {path} HTTP {}", response.status());
        }
        response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Binance {path} returned invalid JSON"))
    }

    async fn request(
        &self,
        method: Method,
        futures: bool,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        self.limiter(futures).reserve(&method, path)?;
        self.request_reserved(method, futures, path, params).await
    }

    async fn request_reserved(
        &self,
        method: Method,
        futures: bool,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value> {
        self.limiter(futures).ready()?;
        let base = if futures { &self.fapi } else { &self.sapi };
        let mut url = Url::parse(&format!("{}{path}", base.trim_end_matches('/')))?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
            query.append_pair("recvWindow", "5000");
            query.append_pair(
                "timestamp",
                &SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_millis()
                    .to_string(),
            );
        }
        let payload = url
            .query()
            .context("missing signed request query")?
            .to_owned();
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())?;
        mac.update(payload.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());
        let request = if method == Method::GET {
            url.query_pairs_mut().append_pair("signature", &signature);
            self.client.request(method, url)
        } else {
            url.set_query(None);
            self.client
                .request(method, url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(format!("{payload}&signature={signature}"))
        };
        let response = request
            .header("X-MBX-APIKEY", &self.key)
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!("Binance {path} transport failed; outcome may be ambiguous")
            })?;
        self.limiter(futures).observe(path, &response)?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Binance {path} returned invalid JSON"))?;
        if !status.is_success() {
            let code = body.get("code").and_then(Value::as_i64).unwrap_or_default();
            let message = body
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("exchange rejected request");
            bail!("Binance {path} HTTP {status} code {code}: {message}");
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_limits_reject_non_finite_values() {
        let mut settings = AccountSettings::default();
        settings.round_cap_usdt = f64::NAN;
        assert!(validate_settings(&settings).is_err());
        settings.round_cap_usdt = 5000.0;
        settings.interval_secs = 59;
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn round_amount_respects_withdrawable_quota_cap_and_margin() {
        assert_eq!(
            round_amount_cents(800.0, 649.27, 50_000.0, 200.0, 9000.0, 5000.0),
            64927
        );
        assert_eq!(
            round_amount_cents(9000.0, 9000.0, 50_000.0, 200.0, 9000.0, 5000.0),
            500000
        );
        assert_eq!(
            round_amount_cents(9000.0, 9000.0, 50_000.0, 200.0, 350.0, 5000.0),
            35000
        );
        assert_eq!(
            round_amount_cents(9000.0, 9000.0, 650.0, 200.0, 9000.0, 5000.0),
            5000
        );
    }

    #[test]
    fn negative_usdt_wallet_balance_is_a_valid_skip_condition() {
        let asset = serde_json::json!({
            "walletBalance": "-0.25",
            "maxWithdrawAmount": "0.00",
        });
        let wallet = finite_number(&asset, "walletBalance").unwrap();
        let withdrawable = number(&asset, "maxWithdrawAmount").unwrap();
        assert_eq!(wallet, -0.25);
        assert!(wallet.min(withdrawable) <= AccountSettings::default().trigger_usdt);
        assert!(number(&asset, "walletBalance").is_err());
    }

    #[test]
    fn subscription_fee_detects_non_par_conversion() {
        assert_eq!(subscription_fee("5000.00", "5000.00000000").unwrap(), 0.0);
        assert!((subscription_fee("100.01", "99.91").unwrap() - 0.1).abs() < 1e-8);
        assert!(subscription_fee("100", "0").is_err());
        assert!(subscription_fee("100", "100.01").is_err());
    }

    #[test]
    fn spot_bfusd_sweep_uses_only_free_balance_with_exact_precision() {
        let spot = serde_json::json!({
            "balances": [
                {"asset": "USDT", "free": "100.00"},
                {"asset": "BFUSD", "free": "2.88457476", "locked": "50.00000000"}
            ]
        });
        assert_eq!(
            spot_bfusd_transfer_amount(&spot).unwrap(),
            Some("2.88457476".to_owned())
        );
        assert_eq!(
            spot_bfusd_transfer_amount(&serde_json::json!({
                "balances": [{"asset": "BFUSD", "free": "0.00009999"}]
            }))
            .unwrap(),
            None
        );
        assert!(
            spot_bfusd_transfer_amount(&serde_json::json!({"balances": []}))
                .unwrap()
                .is_none()
        );
        assert!(
            spot_bfusd_transfer_amount(&serde_json::json!({"balances": [{
                "asset": "BFUSD", "free": "invalid"
            }]}))
            .is_err()
        );
    }

    #[test]
    fn source_ips_follow_trade_engine_config() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("trade_engine.toml"),
            "local_ips = [\"172.31.35.228\", \"172.31.35.234\"]\n",
        )
        .unwrap();
        let source: SourceConfig = toml::from_str(&format!(
            "id = \"trade03\"\naccount = \"trade03\"\nvenue = \"binance-futures\"\nrocksdb_path = \"/tmp/data/persist_manager\"\nenv_path = \"{}/env.sh\"\n",
            dir.path().display()
        )).unwrap();
        let ips = load_account_ips(&source).unwrap();
        assert_eq!(ips.len(), 2);
        assert_eq!(ips[0].to_string(), "172.31.35.228");
        assert_eq!(ips[1].to_string(), "172.31.35.234");
        assert!(validate_treasury_ip(&source, "172.31.35.228".parse().unwrap()).is_err());
        assert!(validate_treasury_ip(&source, "154.197.32.9".parse().unwrap()).is_ok());
        fs::write(
            dir.path().join("trade_engine.toml"),
            "local_ips = [\"172.31.35.228\"]\nprimary_local_ip = \"154.197.32.9\"\n",
        )
        .unwrap();
        assert!(validate_treasury_ip(&source, "154.197.32.9".parse().unwrap()).is_err());
    }
    #[tokio::test]
    async fn exchange_rate_limit_is_shared_across_clients_before_another_request() {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let app = axum::Router::new().fallback(axum::routing::any(move || {
            let requests = requests.clone();
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "120")],
                    axum::Json(serde_json::json!({"code": -1003, "msg": "fixture rate limit"})),
                )
                    .into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let env = BTreeMap::from([
            ("BINANCE_API_KEY".into(), "fixture-key".into()),
            ("BINANCE_API_SECRET".into(), "fixture-secret".into()),
            ("BINANCE_FAPI_URL".into(), origin.clone()),
            ("BINANCE_SAPI_URL".into(), origin),
        ]);
        let first = BinanceApi::new(&env, "127.0.0.1".parse().unwrap()).unwrap();
        assert!(
            first
                .get(false, "/api/v3/account", &[])
                .await
                .unwrap_err()
                .to_string()
                .contains("429")
        );
        let second = BinanceApi::new(&env, "127.0.0.1".parse().unwrap()).unwrap();
        let blocked = second.get(true, "/fapi/v2/account", &[]).await.unwrap_err();
        assert!(
            blocked
                .downcast_ref::<crate::treasury_rate_limit::NotSent>()
                .is_some()
        );
        let blocked = second
            .public_get(false, "/sapi/v1/convert/exchangeInfo", &[])
            .await
            .unwrap_err();
        assert!(
            blocked
                .downcast_ref::<crate::treasury_rate_limit::NotSent>()
                .is_some()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn treasury_rounds_share_rotation_without_changing_account_ips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trade_engine.toml");
        let original = "local_ips = [\"172.31.35.228\", \"172.31.35.234\"]\nprimary_local_ip = \"172.31.35.229\"\n";
        fs::write(&path, original).unwrap();
        let source: SourceConfig = toml::from_str(&format!("id='trade03'\naccount='trade03'\nvenue='binance-futures'\nrocksdb_path='/tmp/data/persist_manager'\nenv_path='{}'",dir.path().join("env.sh").display())).unwrap();
        assert!(
            TreasuryEgress::new(TreasuryConfig::default())
                .select(&source)
                .await
                .is_err()
        );
        let earn = TreasuryEgress::new(TreasuryConfig {
            use_account_ip_rotation: true,
            ..Default::default()
        });
        let bnb = earn.clone();
        assert_eq!(
            earn.select(&source).await.unwrap().to_string(),
            "172.31.35.228"
        );
        assert_eq!(
            bnb.select(&source).await.unwrap().to_string(),
            "172.31.35.234"
        );
        assert_eq!(
            earn.select(&source).await.unwrap().to_string(),
            "172.31.35.228"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::write(&path, "local_ips = ['0.0.0.0']\n").unwrap();
        assert!(bnb.select(&source).await.is_err());
        fs::write(&path, "local_ips = []\n").unwrap();
        assert!(bnb.select(&source).await.is_err());
    }
}
