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
use crate::config::{AppConfig, SourceConfig};
use crate::exchange_leverage::parse_env_file;

type HmacSha256 = Hmac<Sha256>;

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
            interval_secs: 180,
            round_cap_usdt: 5000.0,
            trigger_usdt: 50.0,
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
    rotation: BTreeMap<String, usize>,
    running: Option<String>,
}

#[derive(Clone)]
pub struct AutoEarnHub {
    path: PathBuf,
    runtime: Arc<Mutex<Runtime>>,
}

impl AutoEarnHub {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let root = config
            .twap
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
        let ips = load_account_ips(source)?;
        let index = state.rotation.entry(source.id.clone()).or_default();
        let ip = ips[*index % ips.len()];
        *index += 1;
        let api = BinanceApi::new(&env, ip)?;
        let account = api.get(true, "/fapi/v2/account", &[]).await?;
        if account.get("multiAssetsMargin").and_then(Value::as_bool) != Some(true) {
            bail!("Binance Multi-Assets Mode must be enabled for BFUSD collateral");
        }
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
        if margin - maintenance * 3.0 < 1.0 {
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
        if amount_cents < 100 {
            return Ok("skipped: transferable USDT or BFUSD quota below 1 USDT".to_owned());
        }
        let amount_text = format!("{}.{:02}", amount_cents / 100, amount_cents % 100);

        // Persist the pause before the first mutation. A crash or ambiguous response
        // leaves the account stopped until an operator reconciles Binance balances.
        let old = state.disk.clone();
        state
            .disk
            .accounts
            .entry(source.id.clone())
            .or_default()
            .paused = true;
        if let Err(error) = self.persist(&state.disk) {
            state.disk = old;
            return Err(error);
        }
        api.post(
            false,
            "/sapi/v1/asset/transfer",
            &[
                ("type", "UMFUTURE_MAIN"),
                ("asset", "USDT"),
                ("amount", &amount_text),
            ],
        )
        .await?;
        let subscription = api
            .post(
                false,
                "/sapi/v1/bfusd/subscribe",
                &[("asset", "USDT"), ("amount", &amount_text)],
            )
            .await?;
        if subscription.get("success").and_then(Value::as_bool) != Some(true) {
            bail!("BFUSD subscription did not report success; reconcile before resuming");
        }
        let bfusd = subscription
            .get("bfusdAmount")
            .and_then(Value::as_str)
            .context("BFUSD subscription did not return bfusdAmount")?;
        if bfusd
            .parse::<f64>()
            .ok()
            .is_none_or(|amount| !amount.is_finite() || amount <= 0.0)
        {
            bail!("BFUSD subscription returned invalid bfusdAmount");
        }
        api.post(
            false,
            "/sapi/v1/asset/transfer",
            &[
                ("type", "MAIN_UMFUTURE"),
                ("asset", "BFUSD"),
                ("amount", bfusd),
            ],
        )
        .await?;
        state
            .disk
            .accounts
            .entry(source.id.clone())
            .or_default()
            .paused = false;
        if let Err(error) = self.persist(&state.disk) {
            state
                .disk
                .accounts
                .entry(source.id.clone())
                .or_default()
                .paused = true;
            return Err(error);
        }
        Ok(format!(
            "subscribed {amount_text} USDT into {bfusd} BFUSD using {ip}"
        ))
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

fn load_account_ips(source: &SourceConfig) -> Result<Vec<IpAddr>> {
    let path = source
        .env_path()
        .parent()
        .context("source env file has no parent")?
        .join("trade_engine.toml");
    let value: toml::Value = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {} trade_engine.toml", source.id))?
        .parse()
        .context("invalid trade_engine.toml")?;
    let ips = value
        .get("local_ips")
        .and_then(toml::Value::as_array)
        .context("trade_engine.toml local_ips is missing")?;
    let parsed: Vec<IpAddr> = ips
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
    if parsed.is_empty() {
        bail!("trade_engine.toml local_ips is empty");
    }
    Ok(parsed)
}

fn number(value: &Value, key: &str) -> Result<f64> {
    let amount = finite_number(value, key)?;
    if amount < 0.0 {
        bail!("Binance field {key} is invalid");
    }
    Ok(amount)
}

fn finite_number(value: &Value, key: &str) -> Result<f64> {
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

struct BinanceApi {
    client: Client,
    key: String,
    secret: String,
    fapi: String,
    sapi: String,
}

impl BinanceApi {
    fn new(env: &BTreeMap<String, String>, ip: IpAddr) -> Result<Self> {
        let required = |key: &str| -> Result<String> {
            env.get(key)
                .filter(|v| !v.is_empty())
                .cloned()
                .with_context(|| format!("{key} missing"))
        };
        Ok(Self {
            client: Client::builder()
                .local_address(ip)
                .no_proxy()
                .timeout(Duration::from_secs(15))
                .build()?,
            key: required("BINANCE_API_KEY")?,
            secret: required("BINANCE_API_SECRET")?,
            fapi: env
                .get("BINANCE_FAPI_URL")
                .cloned()
                .unwrap_or_else(|| "https://fapi.binance.com".to_owned()),
            sapi: env
                .get("BINANCE_SAPI_URL")
                .or_else(|| env.get("BINANCE_API_URL"))
                .cloned()
                .unwrap_or_else(|| "https://api.binance.com".to_owned()),
        })
    }

    async fn get(&self, futures: bool, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.request(Method::GET, futures, path, params).await
    }

    async fn post(&self, futures: bool, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.request(Method::POST, futures, path, params).await
    }

    async fn request(
        &self,
        method: Method,
        futures: bool,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value> {
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
    }
}
