use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use reqwest::{Client, Url};
use serde::Serialize;
use sha2::Sha256;

use crate::config::SourceConfig;
use crate::strategy_catalog::{
    SaveSymbolContractLeverageRequest, validate_contract_leverage, validate_contract_symbol,
};

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Serialize)]
pub struct SymbolContractLeverageResult {
    pub source_id: String,
    pub symbol: String,
    pub contract_leverage: i32,
    pub exchange: String,
    pub endpoint: String,
    pub http_status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_contract_leverage: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExchangeFeeRatesResult {
    pub source_id: String,
    pub symbol: String,
    pub exchange: String,
    pub vip_tier: i32,
    pub maker_fee_rate: f64,
    pub taker_fee_rate: f64,
    pub account_endpoint: String,
    pub commission_endpoint: String,
    pub account_http_status: u16,
    pub commission_http_status: u16,
}

#[derive(Debug, Clone)]
struct ExchangeCredentials {
    api_key: String,
    api_secret: String,
    account_mode: AccountMode,
    fapi_url: String,
    papi_url: String,
    okx_base_url: String,
    okx_passphrase: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountMode {
    Standard,
    Unified,
}

pub async fn set_symbol_contract_leverage(
    source: &SourceConfig,
    request: &SaveSymbolContractLeverageRequest,
) -> Result<SymbolContractLeverageResult> {
    validate_contract_symbol(&request.symbol).map_err(anyhow::Error::msg)?;
    validate_contract_leverage(request.contract_leverage).map_err(anyhow::Error::msg)?;
    let credentials = load_source_credentials(source)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .context("failed to build exchange leverage HTTP client")?;

    match source.venue.as_str() {
        "binance-futures" => {
            set_binance_symbol_leverage(&client, source, &credentials, request).await
        }
        "okex-futures" => set_okx_symbol_leverage(&client, source, &credentials, request).await,
        other => bail!(
            "source {} venue does not support set leverage: {other}",
            source.id
        ),
    }
}

pub async fn get_symbol_contract_leverage(
    source: &SourceConfig,
    symbol: &str,
) -> Result<SymbolContractLeverageResult> {
    let symbol = symbol.trim().to_ascii_uppercase();
    validate_contract_symbol(&symbol).map_err(anyhow::Error::msg)?;
    let credentials = load_source_credentials(source)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .context("failed to build exchange leverage HTTP client")?;

    match source.venue.as_str() {
        "binance-futures" => {
            get_binance_symbol_leverage(&client, source, &credentials, &symbol).await
        }
        "okex-futures" => get_okx_symbol_leverage(&client, source, &credentials, &symbol).await,
        other => bail!(
            "source {} venue does not support get leverage: {other}",
            source.id
        ),
    }
}

pub async fn get_exchange_fee_rates(
    source: &SourceConfig,
    symbol: &str,
) -> Result<ExchangeFeeRatesResult> {
    let credentials = load_source_credentials(source)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .context("failed to build exchange fee HTTP client")?;

    match source.venue.as_str() {
        "binance-futures" => {
            get_binance_exchange_fee_rates(&client, source, &credentials, symbol).await
        }
        "okex-futures" => get_okx_exchange_fee_rates(&client, source, &credentials, symbol).await,
        other => bail!(
            "source {} venue does not support exchange fee queries: {other}",
            source.id
        ),
    }
}

/// Operator-facing name of the account mode required before a non-zero publish.
pub fn required_trading_account_mode(venue: &str) -> &'static str {
    match venue {
        "okex-futures" => {
            "OKX unified account mode (acctLv 3 multi-currency margin or 4 portfolio margin)"
        }
        _ => "Binance USD-M Multi-Assets Mode",
    }
}

/// Returns whether the account satisfies the venue mode required before
/// Manager may publish a non-zero target. Binance USD-M requires Standard API
/// mode with Multi-Assets Mode enabled. OKX requires unified account mode:
/// multi-currency margin (`acctLv=3`) or portfolio margin (`acctLv=4`).
pub async fn has_required_trading_account_mode(source: &SourceConfig) -> Result<bool> {
    match source.venue.as_str() {
        "binance-futures" => has_binance_multi_assets_mode(source).await,
        "okex-futures" => has_okx_unified_account_mode(source).await,
        _ => Ok(true),
    }
}

async fn has_binance_multi_assets_mode(source: &SourceConfig) -> Result<bool> {
    let credentials = load_source_credentials(source)?;
    if credentials.account_mode != AccountMode::Standard {
        return Ok(false);
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .context("failed to build Binance account-mode HTTP client")?;
    // accountConfig exposes the same authoritative flag with one-sixth of the
    // request weight of the dedicated multiAssetsMargin endpoint.
    let path = "/fapi/v1/accountConfig";
    let (http_status, body) = binance_signed_get(
        &client,
        &credentials,
        &credentials.fapi_url,
        path,
        BTreeMap::new(),
    )
    .await?;
    if !(200..300).contains(&http_status) {
        bail!(
            "Binance account-mode query failed source={} status={} body={}",
            source.id,
            http_status,
            truncate(&body, 300)
        );
    }
    parse_binance_multi_assets_mode(&body)
}

async fn has_okx_unified_account_mode(source: &SourceConfig) -> Result<bool> {
    let credentials = load_source_credentials(source)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .context("failed to build OKX account-mode HTTP client")?;
    let path = "/api/v5/account/config";
    let (http_status, body) = okx_signed_get(&client, &credentials, path).await?;
    if !(200..300).contains(&http_status) {
        bail!(
            "OKX account-mode query failed source={} status={} body={}",
            source.id,
            http_status,
            truncate(&body, 300)
        );
    }
    let acct_lv = parse_okx_acct_lv(&body)?;
    // mkt_signal trade_engine::okex_precheck::MIN_ACCT_LV.
    // acctLv 1/2 are spot/futures mode and reject tdMode=cross.
    Ok(acct_lv >= 3)
}

async fn set_binance_symbol_leverage(
    client: &Client,
    source: &SourceConfig,
    credentials: &ExchangeCredentials,
    request: &SaveSymbolContractLeverageRequest,
) -> Result<SymbolContractLeverageResult> {
    let (base, path) = match credentials.account_mode {
        AccountMode::Standard => (credentials.fapi_url.as_str(), "/fapi/v1/leverage"),
        AccountMode::Unified => (credentials.papi_url.as_str(), "/papi/v1/um/leverage"),
    };
    let mut params = BTreeMap::new();
    params.insert(
        "leverage".to_string(),
        request.contract_leverage.to_string(),
    );
    params.insert("recvWindow".to_string(), "5000".to_string());
    params.insert("symbol".to_string(), request.symbol.clone());
    params.insert("timestamp".to_string(), now_ms().to_string());
    let query = encode_query(&params);
    let signature = sign_hmac_hex(&credentials.api_secret, &query)?;
    let url = format!(
        "{}{}?{}&signature={}",
        base.trim_end_matches('/'),
        path,
        query,
        signature
    );
    let response = client
        .post(url)
        .header("X-MBX-APIKEY", &credentials.api_key)
        .send()
        .await
        .context("Binance set leverage request failed")?;
    let http_status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&http_status) {
        bail!(
            "Binance set leverage failed source={} symbol={} leverage={} status={} body={}",
            source.id,
            request.symbol,
            request.contract_leverage,
            http_status,
            truncate(&body, 300)
        );
    }
    Ok(SymbolContractLeverageResult {
        source_id: source.id.clone(),
        symbol: request.symbol.clone(),
        contract_leverage: request.contract_leverage,
        exchange: "binance".to_string(),
        endpoint: path.to_string(),
        http_status,
        recorded_contract_leverage: None,
    })
}

async fn get_binance_symbol_leverage(
    client: &Client,
    source: &SourceConfig,
    credentials: &ExchangeCredentials,
    symbol: &str,
) -> Result<SymbolContractLeverageResult> {
    let (base, path) = match credentials.account_mode {
        AccountMode::Standard => (credentials.fapi_url.as_str(), "/fapi/v2/positionRisk"),
        AccountMode::Unified => (credentials.papi_url.as_str(), "/papi/v1/um/positionRisk"),
    };
    let mut params = BTreeMap::new();
    params.insert("recvWindow".to_string(), "5000".to_string());
    params.insert("symbol".to_string(), symbol.to_string());
    params.insert("timestamp".to_string(), now_ms().to_string());
    let query = encode_query(&params);
    let signature = sign_hmac_hex(&credentials.api_secret, &query)?;
    let url = format!(
        "{}{}?{}&signature={}",
        base.trim_end_matches('/'),
        path,
        query,
        signature
    );
    let response = client
        .get(url)
        .header("X-MBX-APIKEY", &credentials.api_key)
        .send()
        .await
        .context("Binance get leverage request failed")?;
    let http_status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&http_status) {
        bail!(
            "Binance get leverage failed source={} symbol={} status={} body={}",
            source.id,
            symbol,
            http_status,
            truncate(&body, 300)
        );
    }
    let contract_leverage = parse_binance_symbol_leverage(&body, symbol)?;
    Ok(SymbolContractLeverageResult {
        source_id: source.id.clone(),
        symbol: symbol.to_string(),
        contract_leverage,
        exchange: "binance".to_string(),
        endpoint: path.to_string(),
        http_status,
        recorded_contract_leverage: None,
    })
}

async fn get_binance_exchange_fee_rates(
    client: &Client,
    source: &SourceConfig,
    credentials: &ExchangeCredentials,
    symbol: &str,
) -> Result<ExchangeFeeRatesResult> {
    let (base, account_path, commission_path) = match credentials.account_mode {
        AccountMode::Standard => (
            credentials.fapi_url.as_str(),
            "/fapi/v1/accountConfig",
            "/fapi/v1/commissionRate",
        ),
        AccountMode::Unified => (
            credentials.papi_url.as_str(),
            "/papi/v1/um/account",
            "/papi/v1/um/commissionRate",
        ),
    };
    let (account_http_status, account_body) =
        binance_signed_get(client, credentials, base, account_path, BTreeMap::new()).await?;
    if !(200..300).contains(&account_http_status) {
        bail!(
            "Binance account fee tier query failed source={} status={} body={}",
            source.id,
            account_http_status,
            truncate(&account_body, 300)
        );
    }
    let vip_tier = parse_binance_fee_tier(&account_body)?;

    let mut commission_params = BTreeMap::new();
    commission_params.insert("symbol".to_string(), symbol.to_string());
    let (commission_http_status, commission_body) = binance_signed_get(
        client,
        credentials,
        base,
        commission_path,
        commission_params,
    )
    .await?;
    if !(200..300).contains(&commission_http_status) {
        bail!(
            "Binance commission-rate query failed source={} symbol={} status={} body={}",
            source.id,
            symbol,
            commission_http_status,
            truncate(&commission_body, 300)
        );
    }
    let (maker_fee_rate, taker_fee_rate) = parse_binance_commission_rates(&commission_body)?;

    Ok(ExchangeFeeRatesResult {
        source_id: source.id.clone(),
        symbol: symbol.to_string(),
        exchange: "binance".to_string(),
        vip_tier,
        maker_fee_rate,
        taker_fee_rate,
        account_endpoint: account_path.to_string(),
        commission_endpoint: commission_path.to_string(),
        account_http_status,
        commission_http_status,
    })
}

async fn get_okx_exchange_fee_rates(
    client: &Client,
    source: &SourceConfig,
    credentials: &ExchangeCredentials,
    symbol: &str,
) -> Result<ExchangeFeeRatesResult> {
    let symbol = symbol.trim().to_ascii_uppercase();
    let inst_id = okx_swap_inst_id(&symbol);
    let inst_family = inst_id
        .strip_suffix("-SWAP")
        .filter(|value| !value.is_empty())
        .context("OKX fee query could not derive instFamily")?;
    let instruments_path = format!("/api/v5/public/instruments?instType=SWAP&instId={inst_id}");
    let instruments_url = format!(
        "{}{instruments_path}",
        credentials.okx_base_url.trim_end_matches('/')
    );
    let instruments_response = client
        .get(instruments_url)
        .send()
        .await
        .context("OKX instruments request failed")?;
    let account_http_status = instruments_response.status().as_u16();
    let instruments_body = instruments_response.text().await.unwrap_or_default();
    if !(200..300).contains(&account_http_status) {
        bail!(
            "OKX instruments query failed source={} symbol={} status={} body={}",
            source.id,
            symbol,
            account_http_status,
            truncate(&instruments_body, 300)
        );
    }
    let group_id = parse_okx_instrument_group_id(&instruments_body, &inst_id)?;

    let commission_path = "/api/v5/account/trade-fee";
    let request_path = format!("{commission_path}?instType=SWAP&instFamily={inst_family}");
    let (commission_http_status, commission_body) =
        okx_signed_get(client, credentials, &request_path).await?;
    if !(200..300).contains(&commission_http_status) {
        bail!(
            "OKX trade-fee query failed source={} symbol={} status={} body={}",
            source.id,
            symbol,
            commission_http_status,
            truncate(&commission_body, 300)
        );
    }
    let (vip_tier, maker_fee_rate, taker_fee_rate) =
        parse_okx_trade_fee(&commission_body, group_id.as_deref())?;

    Ok(ExchangeFeeRatesResult {
        source_id: source.id.clone(),
        symbol,
        exchange: "okx".to_string(),
        vip_tier,
        maker_fee_rate,
        taker_fee_rate,
        account_endpoint: "/api/v5/public/instruments".to_string(),
        commission_endpoint: commission_path.to_string(),
        account_http_status,
        commission_http_status,
    })
}

async fn binance_signed_get(
    client: &Client,
    credentials: &ExchangeCredentials,
    base: &str,
    path: &str,
    mut params: BTreeMap<String, String>,
) -> Result<(u16, String)> {
    params.insert("recvWindow".to_string(), "5000".to_string());
    params.insert("timestamp".to_string(), now_ms().to_string());
    let query = encode_query(&params);
    let signature = sign_hmac_hex(&credentials.api_secret, &query)?;
    let url = format!(
        "{}{}?{}&signature={}",
        base.trim_end_matches('/'),
        path,
        query,
        signature
    );
    let response = client
        .get(url)
        .header("X-MBX-APIKEY", &credentials.api_key)
        .send()
        .await
        .with_context(|| format!("Binance GET {path} request failed"))?;
    let http_status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Ok((http_status, body))
}

async fn set_okx_symbol_leverage(
    client: &Client,
    source: &SourceConfig,
    credentials: &ExchangeCredentials,
    request: &SaveSymbolContractLeverageRequest,
) -> Result<SymbolContractLeverageResult> {
    let passphrase = credentials
        .okx_passphrase
        .as_deref()
        .filter(|value| !value.is_empty())
        .context("OKX_PASSPHRASE is missing in the Exec env file")?;
    let inst_id = okx_swap_inst_id(&request.symbol);
    let path = "/api/v5/account/set-leverage";
    let body = serde_json::json!({
        "instId": inst_id,
        "lever": request.contract_leverage.to_string(),
        "mgnMode": "cross",
    });
    let encoded = serde_json::to_string(&body).context("failed to encode OKX leverage payload")?;
    let timestamp = chrono_like_timestamp();
    let signature = sign_hmac_base64(
        &credentials.api_secret,
        &format!("{timestamp}POST{path}{encoded}"),
    )?;
    let url = format!("{}{path}", credentials.okx_base_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .header("OK-ACCESS-KEY", &credentials.api_key)
        .header("OK-ACCESS-SIGN", signature)
        .header("OK-ACCESS-TIMESTAMP", timestamp)
        .header("OK-ACCESS-PASSPHRASE", passphrase)
        .header("Content-Type", "application/json")
        .body(encoded)
        .send()
        .await
        .context("OKX set leverage request failed")?;
    let http_status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let ok = (200..300).contains(&http_status)
        && value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|code| code == "0");
    if !ok {
        bail!(
            "OKX set leverage failed source={} symbol={} leverage={} status={} body={}",
            source.id,
            request.symbol,
            request.contract_leverage,
            http_status,
            truncate(&text, 300)
        );
    }
    Ok(SymbolContractLeverageResult {
        source_id: source.id.clone(),
        symbol: request.symbol.clone(),
        contract_leverage: request.contract_leverage,
        exchange: "okx".to_string(),
        endpoint: path.to_string(),
        http_status,
        recorded_contract_leverage: None,
    })
}

async fn okx_signed_get(
    client: &Client,
    credentials: &ExchangeCredentials,
    request_path: &str,
) -> Result<(u16, String)> {
    let passphrase = credentials
        .okx_passphrase
        .as_deref()
        .filter(|value| !value.is_empty())
        .context("OKX_PASSPHRASE is missing in the Exec env file")?;
    let timestamp = chrono_like_timestamp();
    let signature = sign_hmac_base64(
        &credentials.api_secret,
        &format!("{timestamp}GET{request_path}"),
    )?;
    let url = format!(
        "{}{request_path}",
        credentials.okx_base_url.trim_end_matches('/')
    );
    let response = client
        .get(url)
        .header("OK-ACCESS-KEY", &credentials.api_key)
        .header("OK-ACCESS-SIGN", signature)
        .header("OK-ACCESS-TIMESTAMP", timestamp)
        .header("OK-ACCESS-PASSPHRASE", passphrase)
        .send()
        .await
        .with_context(|| format!("OKX GET {request_path} request failed"))?;
    let http_status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Ok((http_status, body))
}

async fn get_okx_symbol_leverage(
    client: &Client,
    source: &SourceConfig,
    credentials: &ExchangeCredentials,
    symbol: &str,
) -> Result<SymbolContractLeverageResult> {
    let inst_id = okx_swap_inst_id(symbol);
    let request_path = format!("/api/v5/account/leverage-info?instId={inst_id}&mgnMode=cross");
    let (http_status, text) = okx_signed_get(client, credentials, &request_path).await?;
    if !(200..300).contains(&http_status) {
        bail!(
            "OKX get leverage failed source={} symbol={} status={} body={}",
            source.id,
            symbol,
            http_status,
            truncate(&text, 300)
        );
    }
    let contract_leverage = parse_okx_symbol_leverage(&text, &inst_id)?;
    Ok(SymbolContractLeverageResult {
        source_id: source.id.clone(),
        symbol: symbol.to_string(),
        contract_leverage,
        exchange: "okx".to_string(),
        endpoint: "/api/v5/account/leverage-info".to_string(),
        http_status,
        recorded_contract_leverage: None,
    })
}

fn load_source_credentials(source: &SourceConfig) -> Result<ExchangeCredentials> {
    let env_path = source.env_path();
    let values = parse_env_file(&env_path).with_context(|| {
        format!(
            "failed to read Exec env file for {}: {}",
            source.id,
            env_path.display()
        )
    })?;
    if crate::market_rules::execution_backend(&source.venue, &values)? != "native" {
        bail!("native account APIs are disabled for RapidX sources; use the RapidX Exec adapter");
    }
    match source.venue.as_str() {
        "binance-futures" => {
            let api_key = required_env(&values, "BINANCE_API_KEY")?;
            let api_secret = required_env(&values, "BINANCE_API_SECRET")?;
            let account_mode = match required_env(&values, "BINANCE_ACCOUNT_MODE")?
                .to_ascii_lowercase()
                .as_str()
            {
                "standard" | "std" => AccountMode::Standard,
                "unified" | "pm" => AccountMode::Unified,
                other => bail!("unsupported BINANCE_ACCOUNT_MODE: {other}"),
            };
            Ok(ExchangeCredentials {
                api_key,
                api_secret,
                account_mode,
                fapi_url: optional_env(&values, "BINANCE_FAPI_URL")
                    .unwrap_or_else(|| "https://fapi.binance.com".to_string()),
                papi_url: optional_env(&values, "BINANCE_PAPI_URL")
                    .unwrap_or_else(|| "https://papi.binance.com".to_string()),
                okx_base_url: String::new(),
                okx_passphrase: None,
            })
        }
        "okex-futures" => Ok(ExchangeCredentials {
            api_key: required_env(&values, "OKX_API_KEY")?,
            api_secret: required_env(&values, "OKX_API_SECRET")?,
            account_mode: AccountMode::Standard,
            fapi_url: String::new(),
            papi_url: String::new(),
            okx_base_url: optional_env(&values, "OKX_BASE_URL")
                .unwrap_or_else(|| "https://www.okx.com".to_string()),
            okx_passphrase: Some(required_env(&values, "OKX_PASSPHRASE")?),
        }),
        other => bail!(
            "source {} venue does not support set leverage: {other}",
            source.id
        ),
    }
}

pub(crate) fn parse_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut values = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let assignment = trimmed
            .strip_prefix("export ")
            .map(str::trim)
            .unwrap_or(trimmed);
        let Some((key, value)) = assignment.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        values.insert(key.to_string(), unquote(value.trim()));
    }
    Ok(values)
}

fn required_env(values: &BTreeMap<String, String>, key: &str) -> Result<String> {
    optional_env(values, key).with_context(|| format!("{key} is missing in the Exec env file"))
}

fn optional_env(values: &BTreeMap<String, String>, key: &str) -> Option<String> {
    values
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn encode_query(params: &BTreeMap<String, String>) -> String {
    let mut url = Url::parse("http://localhost/").expect("static query URL must be valid");
    url.query_pairs_mut().extend_pairs(params.iter());
    url.query().unwrap_or_default().to_string()
}

fn sign_hmac_hex(secret: &str, payload: &str) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).context("invalid HMAC secret")?;
    mac.update(payload.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn sign_hmac_base64(secret: &str, payload: &str) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).context("invalid HMAC secret")?;
    mac.update(payload.as_bytes());
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        mac.finalize().into_bytes(),
    ))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn chrono_like_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    format_rfc3339_millis(secs, millis)
}

fn format_rfc3339_millis(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m, d)
}

fn parse_binance_symbol_leverage(body: &str, symbol: &str) -> Result<i32> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("Binance leverage response is not JSON")?;
    if let Some(code) = json_i64(value.get("code").unwrap_or(&serde_json::Value::Null)) {
        if code != 0 && code != 200 {
            bail!(
                "Binance leverage response error code={code} body={}",
                truncate(body, 300)
            );
        }
    }
    let rows = value
        .as_array()
        .or_else(|| value.get("positions").and_then(serde_json::Value::as_array))
        .context("Binance leverage response is not an array")?;
    for row in rows {
        let row_symbol = row
            .get("symbol")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !row_symbol.eq_ignore_ascii_case(symbol) {
            continue;
        }
        if let Some(leverage) = json_i32(row.get("leverage").unwrap_or(&serde_json::Value::Null)) {
            return Ok(leverage);
        }
    }
    bail!("exchange did not return leverage for {symbol}")
}

fn parse_binance_fee_tier(body: &str) -> Result<i32> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("Binance account response is not JSON")?;
    ensure_binance_success(&value, body, "account")?;
    let vip_tier = json_i32(value.get("feeTier").unwrap_or(&serde_json::Value::Null))
        .context("Binance account response is missing feeTier")?;
    if vip_tier < 0 {
        bail!("Binance account response has invalid feeTier={vip_tier}");
    }
    Ok(vip_tier)
}

fn parse_binance_multi_assets_mode(body: &str) -> Result<bool> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("Binance account-mode response is not JSON")?;
    ensure_binance_success(&value, body, "account-mode")?;
    let mode = value
        .get("multiAssetsMargin")
        .context("Binance account-mode response is missing multiAssetsMargin")?;
    mode.as_bool()
        .or_else(|| match mode.as_str() {
            Some(value) if value.eq_ignore_ascii_case("true") => Some(true),
            Some(value) if value.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        })
        .context("Binance account-mode response has invalid multiAssetsMargin")
}

fn parse_binance_commission_rates(body: &str) -> Result<(f64, f64)> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("Binance commission response is not JSON")?;
    ensure_binance_success(&value, body, "commission")?;
    let maker = json_f64(
        value
            .get("makerCommissionRate")
            .unwrap_or(&serde_json::Value::Null),
    )
    .context("Binance commission response is missing makerCommissionRate")?;
    let taker = json_f64(
        value
            .get("takerCommissionRate")
            .unwrap_or(&serde_json::Value::Null),
    )
    .context("Binance commission response is missing takerCommissionRate")?;
    Ok((maker, taker))
}

fn ensure_binance_success(
    value: &serde_json::Value,
    body: &str,
    response_name: &str,
) -> Result<()> {
    if let Some(code) = json_i64(value.get("code").unwrap_or(&serde_json::Value::Null)) {
        if code != 0 && code != 200 {
            bail!(
                "Binance {response_name} response error code={code} body={}",
                truncate(body, 300)
            );
        }
    }
    Ok(())
}

fn parse_okx_symbol_leverage(body: &str, inst_id: &str) -> Result<i32> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("OKX leverage response is not JSON")?;
    let code = value
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if code != "0" {
        bail!(
            "OKX leverage response error code={code} body={}",
            truncate(body, 300)
        );
    }
    let rows = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .context("OKX leverage response is missing data")?;
    for row in rows {
        let row_inst = row
            .get("instId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !row_inst.is_empty() && !row_inst.eq_ignore_ascii_case(inst_id) {
            continue;
        }
        if let Some(leverage) = json_i32(row.get("lever").unwrap_or(&serde_json::Value::Null)) {
            return Ok(leverage);
        }
    }
    bail!("exchange did not return leverage for {inst_id}")
}

fn parse_okx_acct_lv(body: &str) -> Result<i64> {
    let value = parse_okx_success(body, "account config")?;
    let acct_lv = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("acctLv"))
        .and_then(serde_json::Value::as_str)
        .context("OKX account config is missing data[0].acctLv")?;
    acct_lv
        .parse::<i64>()
        .with_context(|| format!("OKX account config acctLv is not an integer: {acct_lv}"))
}

fn parse_okx_instrument_group_id(body: &str, inst_id: &str) -> Result<Option<String>> {
    let value = parse_okx_success(body, "instruments")?;
    let rows = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .context("OKX instruments response is missing data")?;
    for row in rows {
        let row_inst = row
            .get("instId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !row_inst.eq_ignore_ascii_case(inst_id) {
            continue;
        }
        let group_id = row
            .get("groupId")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        return Ok(group_id);
    }
    bail!("OKX instruments did not include {inst_id}")
}

/// OKX trade-fee rates are negative when the account pays and positive for a
/// rebate. Manager stores the Binance commission convention: positive is a
/// cost and negative is a rebate.
fn parse_okx_trade_fee(body: &str, group_id: Option<&str>) -> Result<(i32, f64, f64)> {
    let value = parse_okx_success(body, "trade-fee")?;
    let row = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .context("OKX trade-fee response is missing data")?;
    let level = row
        .get("level")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let vip_tier = parse_okx_fee_level(level)?;
    let (maker, taker) = select_okx_usdt_swap_fee(row, group_id)?;
    Ok((vip_tier, -maker, -taker))
}

fn select_okx_usdt_swap_fee(row: &serde_json::Value, group_id: Option<&str>) -> Result<(f64, f64)> {
    if let Some(groups) = row.get("feeGroup").and_then(serde_json::Value::as_array) {
        if !groups.is_empty() {
            let group = if let Some(group_id) = group_id.filter(|value| !value.is_empty()) {
                groups
                    .iter()
                    .find(|group| {
                        group.get("groupId").and_then(serde_json::Value::as_str) == Some(group_id)
                    })
                    .with_context(|| {
                        format!("OKX trade-fee has no feeGroup for groupId={group_id}")
                    })?
            } else if groups.len() == 1 {
                &groups[0]
            } else {
                bail!("OKX trade-fee returned multiple fee groups without an instrument groupId");
            };
            return read_okx_maker_taker(group, "feeGroup");
        }
    }
    let maker_u = row
        .get("makerU")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    let taker_u = row
        .get("takerU")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    if !maker_u.is_empty() && !taker_u.is_empty() {
        return Ok((
            parse_okx_fee_number(maker_u, "makerU")?,
            parse_okx_fee_number(taker_u, "takerU")?,
        ));
    }
    bail!("OKX trade-fee did not include a USDT swap feeGroup or makerU/takerU")
}

fn read_okx_maker_taker(row: &serde_json::Value, label: &str) -> Result<(f64, f64)> {
    let maker = json_f64(row.get("maker").unwrap_or(&serde_json::Value::Null))
        .with_context(|| format!("OKX {label} is missing maker"))?;
    let taker = json_f64(row.get("taker").unwrap_or(&serde_json::Value::Null))
        .with_context(|| format!("OKX {label} is missing taker"))?;
    Ok((maker, taker))
}

fn parse_okx_fee_number(raw: &str, field: &str) -> Result<f64> {
    let value = raw
        .parse::<f64>()
        .with_context(|| format!("OKX {field} is not a decimal: {raw}"))?;
    if !value.is_finite() {
        bail!("OKX {field} is not finite: {raw}");
    }
    Ok(value)
}

fn parse_okx_fee_level(level: &str) -> Result<i32> {
    let digits: String = level.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        bail!("OKX trade-fee level is missing a numeric tier: {level}");
    }
    let tier = digits
        .parse::<i32>()
        .with_context(|| format!("OKX trade-fee level is not an integer: {level}"))?;
    if tier < 0 {
        bail!("OKX trade-fee level is negative: {level}");
    }
    Ok(tier)
}

fn parse_okx_success(body: &str, response_name: &str) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(body)
        .with_context(|| format!("OKX {response_name} response is not JSON"))?;
    let code = value
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if code != "0" {
        bail!(
            "OKX {response_name} response error code={code} body={}",
            truncate(body, 300)
        );
    }
    Ok(value)
}

fn json_i32(value: &serde_json::Value) -> Option<i32> {
    json_i64(value).and_then(|value| i32::try_from(value).ok())
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value);
    }
    if let Some(value) = value.as_u64() {
        return i64::try_from(value).ok();
    }
    if let Some(value) = value.as_f64() {
        if value.is_finite() && value.fract() == 0.0 {
            return Some(value as i64);
        }
    }
    value.as_str().and_then(|raw| raw.trim().parse().ok())
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    let value = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|raw| raw.trim().parse().ok()))?;
    value.is_finite().then_some(value)
}

fn okx_swap_inst_id(symbol: &str) -> String {
    let base = symbol
        .strip_suffix("USDT")
        .filter(|value| !value.is_empty())
        .unwrap_or(symbol);
    format!("{base}-USDT-SWAP")
}

fn truncate(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        value.to_string()
    } else {
        format!("{}...", &value[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_export_assignments_without_printing_secrets() {
        let parsed = {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("env.sh");
            fs::write(
                &path,
                "export BINANCE_API_KEY=abc\nexport BINANCE_ACCOUNT_MODE='STANDARD'\n# comment\n",
            )
            .unwrap();
            parse_env_file(&path).unwrap()
        };
        assert_eq!(
            parsed.get("BINANCE_API_KEY").map(String::as_str),
            Some("abc")
        );
        assert_eq!(
            parsed.get("BINANCE_ACCOUNT_MODE").map(String::as_str),
            Some("STANDARD")
        );
    }

    #[test]
    fn formats_okx_swap_symbol() {
        assert_eq!(okx_swap_inst_id("BTCUSDT"), "BTC-USDT-SWAP");
    }

    #[test]
    fn percent_encodes_unicode_binance_symbols_before_signing() {
        let query = encode_query(&BTreeMap::from([
            ("symbol".to_string(), "龙虾USDT".to_string()),
            ("timestamp".to_string(), "123".to_string()),
        ]));
        assert_eq!(query, "symbol=%E9%BE%99%E8%99%BEUSDT&timestamp=123");
    }

    #[test]
    fn parses_binance_position_risk_leverage() {
        let body = r#"[{"symbol":"BTCUSDT","leverage":"20","positionAmt":"0.0"}]"#;
        assert_eq!(parse_binance_symbol_leverage(body, "BTCUSDT").unwrap(), 20);
        assert!(parse_binance_symbol_leverage(body, "ETHUSDT").is_err());
    }

    #[test]
    fn parses_binance_fee_tier_and_commission_rates() {
        assert_eq!(parse_binance_fee_tier(r#"{"feeTier":3}"#).unwrap(), 3);
        assert_eq!(
            parse_binance_commission_rates(
                r#"{"symbol":"BTCUSDT","makerCommissionRate":"0.00020000","takerCommissionRate":"0.00050000"}"#,
            )
            .unwrap(),
            (0.0002, 0.0005)
        );
    }

    #[test]
    fn parses_binance_multi_assets_mode() {
        assert!(parse_binance_multi_assets_mode(r#"{"multiAssetsMargin":true}"#).unwrap());
        assert!(!parse_binance_multi_assets_mode(r#"{"multiAssetsMargin":"false"}"#).unwrap());
        assert!(parse_binance_multi_assets_mode(r#"{}"#).is_err());
    }

    #[test]
    fn rejects_missing_binance_fee_fields() {
        assert!(parse_binance_fee_tier(r#"{}"#).is_err());
        assert!(parse_binance_commission_rates(r#"{"makerCommissionRate":"0.0002"}"#).is_err());
    }

    #[test]
    fn parses_okx_leverage_info() {
        let body =
            r#"{"code":"0","data":[{"instId":"BTC-USDT-SWAP","mgnMode":"cross","lever":"5"}]}"#;
        assert_eq!(parse_okx_symbol_leverage(body, "BTC-USDT-SWAP").unwrap(), 5);
        assert!(parse_okx_symbol_leverage(body, "ETH-USDT-SWAP").is_err());
    }

    #[test]
    fn okx_unified_account_requires_acct_lv_at_least_three() {
        assert_eq!(
            parse_okx_acct_lv(r#"{"code":"0","data":[{"acctLv":"4"}]}"#).unwrap(),
            4
        );
        assert!(parse_okx_acct_lv(r#"{"code":"0","data":[{"acctLv":"3"}]}"#).unwrap() >= 3);
        assert!(parse_okx_acct_lv(r#"{"code":"0","data":[{"acctLv":"2"}]}"#).unwrap() < 3);
        assert!(parse_okx_acct_lv(r#"{"code":"50001","msg":"err"}"#).is_err());
    }

    #[test]
    fn parses_okx_usdt_swap_fee_group_into_manager_sign() {
        let instruments = r#"{"code":"0","data":[{"instId":"BTC-USDT-SWAP","groupId":"4"}]}"#;
        assert_eq!(
            parse_okx_instrument_group_id(instruments, "BTC-USDT-SWAP")
                .unwrap()
                .as_deref(),
            Some("4")
        );
        let body = r#"{"code":"0","data":[{"level":"Lv1","feeGroup":[
            {"groupId":"5","maker":"-0.0001","taker":"-0.0003"},
            {"groupId":"4","maker":"-0.0002","taker":"-0.0005"}
        ]}]}"#;
        assert_eq!(
            parse_okx_trade_fee(body, Some("4")).unwrap(),
            (1, 0.0002, 0.0005)
        );
        let rebate =
            r#"{"code":"0","data":[{"level":"Lv3","makerU":"0.00001","takerU":"-0.00015"}]}"#;
        assert_eq!(
            parse_okx_trade_fee(rebate, None).unwrap(),
            (3, -0.00001, 0.00015)
        );
        assert!(parse_okx_trade_fee(body, Some("9")).is_err());
    }
}
