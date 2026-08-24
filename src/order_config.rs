use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderParameters {
    pub single_order_usdt: f64,
    pub orders_per_batch: u32,
    #[serde(default = "default_max_batch")]
    pub max_batch: u32,
    pub maker_price_anchor: String,
    pub tick_spacing: u32,
    pub batch_interval_ms: u32,
    pub maker_timeout_ms: u32,
    pub max_maker_requotes: u32,
    pub target_tolerance_usdt: f64,
}

impl Default for OrderParameters {
    fn default() -> Self {
        Self {
            single_order_usdt: 100.0,
            orders_per_batch: 3,
            max_batch: default_max_batch(),
            maker_price_anchor: "own_best".to_string(),
            tick_spacing: 1,
            batch_interval_ms: 500,
            maker_timeout_ms: 1_000,
            max_maker_requotes: 2,
            target_tolerance_usdt: 10.0,
        }
    }
}

impl OrderParameters {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !self.single_order_usdt.is_finite() || self.single_order_usdt <= 0.0 {
            return Err("single_order_usdt must be finite and greater than zero".to_string());
        }
        if self.orders_per_batch == 0 {
            return Err("orders_per_batch must be greater than zero".to_string());
        }
        if self.max_batch == 0 {
            return Err("max_batch must be greater than zero".to_string());
        }
        if !matches!(
            self.maker_price_anchor.as_str(),
            "own_best" | "opposite_best_plus_one_tick"
        ) {
            return Err("maker_price_anchor is invalid".to_string());
        }
        if self.maker_timeout_ms == 0 {
            return Err("maker_timeout_ms must be greater than zero".to_string());
        }
        if !self.target_tolerance_usdt.is_finite() || self.target_tolerance_usdt < 0.0 {
            return Err("target_tolerance_usdt must be finite and nonnegative".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderParameterOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_order_usdt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orders_per_batch: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_batch: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maker_price_anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tick_spacing: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_interval_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maker_timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_maker_requotes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_tolerance_usdt: Option<f64>,
}

impl OrderParameterOverrides {
    pub fn is_empty(&self) -> bool {
        self.single_order_usdt.is_none()
            && self.orders_per_batch.is_none()
            && self.max_batch.is_none()
            && self.maker_price_anchor.is_none()
            && self.tick_spacing.is_none()
            && self.batch_interval_ms.is_none()
            && self.maker_timeout_ms.is_none()
            && self.max_maker_requotes.is_none()
            && self.target_tolerance_usdt.is_none()
    }

    pub fn apply_to(&self, defaults: &OrderParameters) -> OrderParameters {
        OrderParameters {
            single_order_usdt: self.single_order_usdt.unwrap_or(defaults.single_order_usdt),
            orders_per_batch: self.orders_per_batch.unwrap_or(defaults.orders_per_batch),
            max_batch: self.max_batch.unwrap_or(defaults.max_batch),
            maker_price_anchor: self
                .maker_price_anchor
                .clone()
                .unwrap_or_else(|| defaults.maker_price_anchor.clone()),
            tick_spacing: self.tick_spacing.unwrap_or(defaults.tick_spacing),
            batch_interval_ms: self.batch_interval_ms.unwrap_or(defaults.batch_interval_ms),
            maker_timeout_ms: self.maker_timeout_ms.unwrap_or(defaults.maker_timeout_ms),
            max_maker_requotes: self
                .max_maker_requotes
                .unwrap_or(defaults.max_maker_requotes),
            target_tolerance_usdt: self
                .target_tolerance_usdt
                .unwrap_or(defaults.target_tolerance_usdt),
        }
    }

    pub fn from_templates(defaults: &OrderParameters, selected: &OrderParameters) -> Self {
        Self {
            single_order_usdt: (selected.single_order_usdt != defaults.single_order_usdt)
                .then_some(selected.single_order_usdt),
            orders_per_batch: (selected.orders_per_batch != defaults.orders_per_batch)
                .then_some(selected.orders_per_batch),
            max_batch: (selected.max_batch != defaults.max_batch).then_some(selected.max_batch),
            maker_price_anchor: (selected.maker_price_anchor != defaults.maker_price_anchor)
                .then(|| selected.maker_price_anchor.clone()),
            tick_spacing: (selected.tick_spacing != defaults.tick_spacing)
                .then_some(selected.tick_spacing),
            batch_interval_ms: (selected.batch_interval_ms != defaults.batch_interval_ms)
                .then_some(selected.batch_interval_ms),
            maker_timeout_ms: (selected.maker_timeout_ms != defaults.maker_timeout_ms)
                .then_some(selected.maker_timeout_ms),
            max_maker_requotes: (selected.max_maker_requotes != defaults.max_maker_requotes)
                .then_some(selected.max_maker_requotes),
            target_tolerance_usdt: (selected.target_tolerance_usdt
                != defaults.target_tolerance_usdt)
                .then_some(selected.target_tolerance_usdt),
        }
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        self.apply_to(&OrderParameters::default()).validate()
    }
}

pub fn validate_symbol_order_parameter_overrides(
    overrides: &BTreeMap<String, OrderParameterOverrides>,
) -> std::result::Result<(), String> {
    for (symbol, override_parameters) in overrides {
        if symbol.is_empty()
            || !symbol
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(format!("invalid symbol override: {symbol}"));
        }
        if override_parameters.is_empty() {
            return Err(format!(
                "symbol_overrides.{symbol} must override at least one parameter"
            ));
        }
        override_parameters
            .validate()
            .map_err(|error| format!("symbol_overrides.{symbol}.{error}"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderStrategyView {
    pub source_id: String,
    pub strategy_name: String,
    pub order_parameters: OrderParameters,
    pub symbol_overrides: BTreeMap<String, OrderParameterOverrides>,
    pub updated_at_us: Option<i64>,
    pub target_count: usize,
    pub nonzero_target_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveOrderParametersRequest {
    pub strategy_name: String,
    pub expected_updated_at_us: Option<i64>,
    pub order_parameters: OrderParameters,
}

#[derive(Debug, Deserialize)]
struct StrategyIndexResponse {
    strategies: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StrategyResponse {
    strategy_name: String,
    exists: bool,
    config: ExecConfigPayload,
}

pub const ALLOWED_TARGET_SIGNALS: [i32; 5] = [-2, -1, 0, 1, 2];

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TargetPosition {
    pub qty: f64,
    pub signal: i32,
}

impl TargetPosition {
    pub fn new(qty: f64, signal: i32) -> std::result::Result<Self, String> {
        if !qty.is_finite() {
            return Err("qty must be finite".to_string());
        }
        validate_target_signal(signal)?;
        Ok(Self { qty, signal })
    }
}

impl<'de> Deserialize<'de> for TargetPosition {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum TargetPositionDe {
            Qty(f64),
            Object(TargetPositionObject),
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TargetPositionObject {
            qty: f64,
            #[serde(default)]
            signal: Option<i32>,
        }

        match TargetPositionDe::deserialize(deserializer)? {
            TargetPositionDe::Qty(qty) => {
                TargetPosition::new(qty, 0).map_err(serde::de::Error::custom)
            }
            TargetPositionDe::Object(value) => {
                TargetPosition::new(value.qty, value.signal.unwrap_or(0))
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

pub fn validate_target_signal(signal: i32) -> std::result::Result<(), String> {
    if ALLOWED_TARGET_SIGNALS.contains(&signal) {
        Ok(())
    } else {
        Err(format!(
            "signal must be one of {}",
            ALLOWED_TARGET_SIGNALS
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[derive(Debug, Deserialize)]
struct ExecConfigPayload {
    single_order_usdt: f64,
    orders_per_batch: u32,
    #[serde(default = "default_max_batch")]
    max_batch: u32,
    maker_price_anchor: String,
    tick_spacing: u32,
    batch_interval_ms: u32,
    maker_timeout_ms: u32,
    max_maker_requotes: u32,
    target_tolerance_usdt: f64,
    #[serde(default)]
    targets: BTreeMap<String, TargetPosition>,
    #[serde(default)]
    symbol_overrides: BTreeMap<String, OrderParameterOverrides>,
    updated_at_us: Option<i64>,
}

impl ExecConfigPayload {
    fn order_parameters(&self) -> OrderParameters {
        OrderParameters {
            single_order_usdt: self.single_order_usdt,
            orders_per_batch: self.orders_per_batch,
            max_batch: self.max_batch,
            maker_price_anchor: self.maker_price_anchor.clone(),
            tick_spacing: self.tick_spacing,
            batch_interval_ms: self.batch_interval_ms,
            maker_timeout_ms: self.maker_timeout_ms,
            max_maker_requotes: self.max_maker_requotes,
            target_tolerance_usdt: self.target_tolerance_usdt,
        }
    }
}

const fn default_max_batch() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
struct SaveResponse {
    strategy_name: String,
    order_parameters: OrderParameters,
    #[serde(default)]
    symbol_overrides: BTreeMap<String, OrderParameterOverrides>,
    updated_at_us: i64,
}

#[derive(Debug, Deserialize)]
struct StrategyPublishResponse {
    strategy_name: String,
    config: Option<ExecConfigPayload>,
}

#[derive(Debug, Deserialize)]
struct UpstreamErrorResponse {
    error: Option<String>,
}

#[derive(Serialize)]
struct UpstreamSaveRequest<'a> {
    strategy_name: &'a str,
    expected_updated_at_us: Option<i64>,
    order_parameters: &'a OrderParameters,
}

#[derive(Debug)]
pub struct ExecConfigError {
    status: Option<StatusCode>,
    message: String,
}

impl ExecConfigError {
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }

    pub fn public_message(&self) -> &str {
        &self.message
    }

    fn transport(error: impl fmt::Display) -> Self {
        Self {
            status: None,
            message: format!("Exec Config request failed: {error}"),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: Some(StatusCode::BAD_GATEWAY),
            message: message.into(),
        }
    }
}

impl fmt::Display for ExecConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExecConfigError {}

#[derive(Clone)]
pub struct ExecConfigClient {
    http: Client,
}

impl ExecConfigClient {
    pub fn new(timeout_secs: u64) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .no_proxy()
            .build()
            .context("failed to build Exec Config HTTP client")?;
        Ok(Self { http })
    }

    pub async fn list_strategies(
        &self,
        base_url: &str,
    ) -> std::result::Result<Vec<String>, ExecConfigError> {
        let url = endpoint(base_url, "strategies")?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(ExecConfigError::transport)?;
        let payload: StrategyIndexResponse = decode_response(response).await?;
        for name in &payload.strategies {
            validate_strategy_name(name).map_err(ExecConfigError::invalid)?;
        }
        let mut strategies = payload.strategies;
        strategies.sort();
        strategies.dedup();
        Ok(strategies)
    }

    pub async fn load_strategy(
        &self,
        source_id: &str,
        base_url: &str,
        strategy_name: &str,
    ) -> std::result::Result<OrderStrategyView, ExecConfigError> {
        validate_strategy_name(strategy_name).map_err(ExecConfigError::invalid)?;
        let mut url = endpoint(base_url, "strategy")?;
        url.query_pairs_mut().append_pair("name", strategy_name);
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(ExecConfigError::transport)?;
        let payload: StrategyResponse = decode_response(response).await?;
        if !payload.exists {
            return Err(ExecConfigError {
                status: Some(StatusCode::NOT_FOUND),
                message: "strategy config was not found".to_string(),
            });
        }
        if payload.strategy_name != strategy_name {
            return Err(ExecConfigError::invalid(
                "Exec Config returned a different strategy_name",
            ));
        }
        let order_parameters = payload.config.order_parameters();
        order_parameters
            .validate()
            .map_err(ExecConfigError::invalid)?;
        let target_count = payload.config.targets.len();
        let nonzero_target_count = payload
            .config
            .targets
            .values()
            .filter(|target| target.qty.abs() > 0.0)
            .count();
        Ok(OrderStrategyView {
            source_id: source_id.to_string(),
            strategy_name: strategy_name.to_string(),
            order_parameters,
            symbol_overrides: payload.config.symbol_overrides,
            updated_at_us: payload.config.updated_at_us,
            target_count,
            nonzero_target_count,
        })
    }

    pub async fn save_order_parameters(
        &self,
        source_id: &str,
        base_url: &str,
        request: &SaveOrderParametersRequest,
    ) -> std::result::Result<OrderStrategyView, ExecConfigError> {
        validate_strategy_name(&request.strategy_name).map_err(ExecConfigError::invalid)?;
        request
            .order_parameters
            .validate()
            .map_err(ExecConfigError::invalid)?;
        if request
            .expected_updated_at_us
            .is_some_and(|value| value <= 0)
        {
            return Err(ExecConfigError::invalid(
                "expected_updated_at_us must be positive when present",
            ));
        }
        let url = endpoint(base_url, "order-parameters")?;
        let response = self
            .http
            .post(url)
            .json(&UpstreamSaveRequest {
                strategy_name: &request.strategy_name,
                expected_updated_at_us: request.expected_updated_at_us,
                order_parameters: &request.order_parameters,
            })
            .send()
            .await
            .map_err(ExecConfigError::transport)?;
        let payload: SaveResponse = decode_response(response).await?;
        if payload.strategy_name != request.strategy_name {
            return Err(ExecConfigError::invalid(
                "Exec Config returned a different strategy_name",
            ));
        }
        payload
            .order_parameters
            .validate()
            .map_err(ExecConfigError::invalid)?;
        Ok(OrderStrategyView {
            source_id: source_id.to_string(),
            strategy_name: payload.strategy_name,
            order_parameters: payload.order_parameters,
            symbol_overrides: payload.symbol_overrides,
            updated_at_us: Some(payload.updated_at_us),
            target_count: 0,
            nonzero_target_count: 0,
        })
    }

    pub async fn publish_strategy(
        &self,
        source_id: &str,
        base_url: &str,
        strategy_name: &str,
        order_parameters: &OrderParameters,
        symbol_overrides: &BTreeMap<String, OrderParameterOverrides>,
        targets: &BTreeMap<String, TargetPosition>,
    ) -> std::result::Result<OrderStrategyView, ExecConfigError> {
        validate_strategy_name(strategy_name).map_err(ExecConfigError::invalid)?;
        order_parameters
            .validate()
            .map_err(ExecConfigError::invalid)?;
        validate_symbol_order_parameter_overrides(symbol_overrides)
            .map_err(ExecConfigError::invalid)?;
        let url = endpoint(base_url, "strategy")?;
        let mut config = serde_json::json!({
            "single_order_usdt": order_parameters.single_order_usdt,
            "orders_per_batch": order_parameters.orders_per_batch,
            "max_batch": order_parameters.max_batch,
            "maker_price_anchor": order_parameters.maker_price_anchor,
            "tick_spacing": order_parameters.tick_spacing,
            "batch_interval_ms": order_parameters.batch_interval_ms,
            "maker_timeout_ms": order_parameters.maker_timeout_ms,
            "max_maker_requotes": order_parameters.max_maker_requotes,
            "target_tolerance_usdt": order_parameters.target_tolerance_usdt,
            "targets": targets,
        });
        if !symbol_overrides.is_empty() {
            config["symbol_overrides"] =
                serde_json::to_value(symbol_overrides).map_err(ExecConfigError::transport)?;
        }
        let response = self
            .http
            .post(url)
            .json(&serde_json::json!({
                "strategy_name": strategy_name,
                "config": config,
            }))
            .send()
            .await
            .map_err(ExecConfigError::transport)?;
        let payload: StrategyPublishResponse = decode_response(response).await?;
        if payload.strategy_name != strategy_name {
            return Err(ExecConfigError::invalid(
                "Exec Config returned a different strategy_name",
            ));
        }
        let published = payload
            .config
            .ok_or_else(|| ExecConfigError::invalid("Exec Config omitted published config"))?;
        let published_parameters = published.order_parameters();
        published_parameters
            .validate()
            .map_err(ExecConfigError::invalid)?;
        let target_count = published.targets.len();
        let nonzero_target_count = published
            .targets
            .values()
            .filter(|target| target.qty.abs() > 0.0)
            .count();
        Ok(OrderStrategyView {
            source_id: source_id.to_string(),
            strategy_name: payload.strategy_name,
            order_parameters: published_parameters,
            symbol_overrides: published.symbol_overrides,
            updated_at_us: published.updated_at_us,
            target_count,
            nonzero_target_count,
        })
    }
}

fn endpoint(base_url: &str, path: &str) -> std::result::Result<Url, ExecConfigError> {
    let mut base = Url::parse(base_url).map_err(ExecConfigError::transport)?;
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(&format!("api/{path}"))
        .map_err(ExecConfigError::transport)
}

async fn decode_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> std::result::Result<T, ExecConfigError> {
    let status = response.status();
    if !status.is_success() {
        let message = response
            .json::<UpstreamErrorResponse>()
            .await
            .ok()
            .and_then(|payload| payload.error)
            .unwrap_or_else(|| format!("Exec Config returned HTTP {status}"));
        return Err(ExecConfigError {
            status: Some(status),
            message,
        });
    }
    response.json().await.map_err(ExecConfigError::transport)
}

pub fn validate_strategy_name(name: &str) -> std::result::Result<(), String> {
    let valid_len = !name.is_empty() && name.len() <= 256;
    let mut bytes = name.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    let valid_rest =
        bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid_len || !valid_first || !valid_rest {
        return Err("strategy_name has an invalid format".to_string());
    }
    if matches!(
        name,
        "strategy_names" | "removed_strategy_names" | "SYSTEM_POSITION_CLOSE"
    ) {
        return Err("strategy_name is reserved".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_parameters() -> OrderParameters {
        OrderParameters {
            single_order_usdt: 100.0,
            orders_per_batch: 3,
            max_batch: 20,
            maker_price_anchor: "own_best".to_string(),
            tick_spacing: 1,
            batch_interval_ms: 500,
            maker_timeout_ms: 12_000,
            max_maker_requotes: 2,
            target_tolerance_usdt: 10.0,
        }
    }

    #[test]
    fn validates_order_parameters() {
        assert!(valid_parameters().validate().is_ok());
        let mut invalid = valid_parameters();
        invalid.orders_per_batch = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = valid_parameters();
        invalid.max_batch = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = valid_parameters();
        invalid.maker_price_anchor = "mid".to_string();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn symbol_overrides_replace_only_their_defined_fields() {
        let defaults = valid_parameters();
        let overrides = OrderParameterOverrides {
            single_order_usdt: Some(250.0),
            maker_price_anchor: Some("opposite_best_plus_one_tick".to_string()),
            ..Default::default()
        };
        let effective = overrides.apply_to(&defaults);
        assert_eq!(effective.single_order_usdt, 250.0);
        assert_eq!(effective.maker_price_anchor, "opposite_best_plus_one_tick");
        assert_eq!(effective.orders_per_batch, defaults.orders_per_batch);

        let map = BTreeMap::from([("BTCUSDT".to_string(), overrides)]);
        assert!(validate_symbol_order_parameter_overrides(&map).is_ok());
        let invalid_symbol = BTreeMap::from([("btc-usdt".to_string(), Default::default())]);
        assert!(validate_symbol_order_parameter_overrides(&invalid_symbol).is_err());
        let empty_override = BTreeMap::from([("BTCUSDT".to_string(), Default::default())]);
        assert!(validate_symbol_order_parameter_overrides(&empty_override).is_err());

        let selected = OrderParameters {
            single_order_usdt: 250.0,
            max_maker_requotes: 0,
            ..defaults.clone()
        };
        let derived = OrderParameterOverrides::from_templates(&defaults, &selected);
        assert_eq!(derived.single_order_usdt, Some(250.0));
        assert_eq!(derived.max_maker_requotes, Some(0));
        assert_eq!(derived.orders_per_batch, None);
        assert_eq!(derived.apply_to(&defaults), selected);
    }

    #[test]
    fn legacy_order_parameters_default_max_batch() {
        let mut payload = serde_json::to_value(valid_parameters()).unwrap();
        payload.as_object_mut().unwrap().remove("max_batch");
        let decoded: OrderParameters = serde_json::from_value(payload).unwrap();
        assert_eq!(decoded.max_batch, 20);
    }

    #[test]
    fn save_payload_rejects_targets() {
        let payload = serde_json::json!({
            "strategy_name": "cta_alpha",
            "expected_updated_at_us": 1,
            "order_parameters": {
                "single_order_usdt": 100.0,
                "orders_per_batch": 3,
                "max_batch": 20,
                "maker_price_anchor": "own_best",
                "tick_spacing": 1,
                "batch_interval_ms": 500,
                "maker_timeout_ms": 12000,
                "max_maker_requotes": 2,
                "target_tolerance_usdt": 10.0,
                "targets": {"BTCUSDT": 1.0}
            }
        });
        assert!(serde_json::from_value::<SaveOrderParametersRequest>(payload).is_err());
    }

    #[test]
    fn validates_strategy_names() {
        assert!(validate_strategy_name("CTA_SK_01.alpha").is_ok());
        assert!(validate_strategy_name("../strategy").is_err());
        assert!(validate_strategy_name("SYSTEM_POSITION_CLOSE").is_err());
    }

    #[test]
    fn target_position_accepts_legacy_qty_and_signal_object() {
        let legacy: TargetPosition = serde_json::from_value(serde_json::json!(-0.006)).unwrap();
        assert_eq!(
            legacy,
            TargetPosition {
                qty: -0.006,
                signal: 0
            }
        );
        let object: TargetPosition = serde_json::from_value(serde_json::json!({
            "qty": -0.54,
            "signal": -1
        }))
        .unwrap();
        assert_eq!(
            object,
            TargetPosition {
                qty: -0.54,
                signal: -1
            }
        );
        let omitted: TargetPosition =
            serde_json::from_value(serde_json::json!({ "qty": 1.0 })).unwrap();
        assert_eq!(omitted.signal, 0);
        assert!(
            serde_json::from_value::<TargetPosition>(serde_json::json!({
                "qty": 1.0,
                "signal": 3
            }))
            .is_err()
        );
    }
}
