//! Source-bound native Bybit/Bitget unified-account APIs.
use reqwest::Method;
use serde_json::{Value, json};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Venue {
    Bybit,
    Bitget,
}

struct AccountApi {
    venue: Venue,
    client: Client,
    key: String,
    secret: String,
    passphrase: Option<String>,
    base: String,
}

pub(super) fn supports(venue: &str) -> bool {
    matches!(venue, "bybit-futures" | "bitget-futures")
}

impl AccountApi {
    fn load(source: &SourceConfig) -> Result<Self> {
        let values = parse_env_file(&source.env_path())?;
        anyhow::ensure!(
            crate::market_rules::execution_backend(&source.venue, &values)? == "native",
            "UTA account APIs require the native backend"
        );
        let (venue, prefix, default) = match source.venue.as_str() {
            "bybit-futures" => (Venue::Bybit, "BYBIT", "https://api.bybit.com"),
            "bitget-futures" => (Venue::Bitget, "BITGET", "https://api.bitget.com"),
            _ => bail!("unsupported unified account venue"),
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .local_address(crate::bfusd_auto::account_fixed_ip(source)?)
            .build()
            .context("build account-bound unified account client")?;
        Ok(Self {
            venue,
            client,
            key: required_env(&values, &format!("{prefix}_API_KEY"))?,
            secret: required_env(&values, &format!("{prefix}_API_SECRET"))?,
            passphrase: if venue == Venue::Bitget {
                Some(required_env(&values, "BITGET_API_PASSPHRASE")?)
            } else {
                None
            },
            base: optional_env(&values, &format!("{prefix}_BASE_URL"))
                .unwrap_or_else(|| default.into())
                .trim_end_matches('/')
                .into(),
        })
    }

    fn name(&self) -> &'static str {
        match self.venue {
            Venue::Bybit => "bybit",
            Venue::Bitget => "bitget",
        }
    }

    fn mode_path(&self) -> &'static str {
        match self.venue {
            Venue::Bybit => "/v5/account/info",
            Venue::Bitget => "/api/v3/account/settings",
        }
    }

    fn symbol_params(&self, symbol: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "category".into(),
                if self.venue == Venue::Bybit {
                    "linear"
                } else {
                    "USDT-FUTURES"
                }
                .into(),
            ),
            ("symbol".into(), symbol.into()),
        ])
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        params: &BTreeMap<String, String>,
        body: Option<&Value>,
        timestamp: i64,
    ) -> Result<reqwest::RequestBuilder> {
        let query = encode_query(params);
        let path_query = if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{query}")
        };
        let raw_body = body
            .map(serde_json::to_string)
            .transpose()?
            .unwrap_or_default();
        let mut request = self
            .client
            .request(method.clone(), format!("{}{path_query}", self.base));
        match self.venue {
            Venue::Bybit => {
                let data = if method == Method::GET {
                    query.as_str()
                } else {
                    raw_body.as_str()
                };
                let signature =
                    sign_hmac_hex(&self.secret, &format!("{timestamp}{}5000{data}", self.key))?;
                request = request
                    .header("X-BAPI-API-KEY", &self.key)
                    .header("X-BAPI-TIMESTAMP", timestamp.to_string())
                    .header("X-BAPI-RECV-WINDOW", "5000")
                    .header("X-BAPI-SIGN", signature);
            }
            Venue::Bitget => {
                let signature = sign_hmac_base64(
                    &self.secret,
                    &format!("{timestamp}{}{path_query}{raw_body}", method.as_str()),
                )?;
                request = request
                    .header("ACCESS-KEY", &self.key)
                    .header("ACCESS-TIMESTAMP", timestamp.to_string())
                    .header("ACCESS-SIGN", signature)
                    .header(
                        "ACCESS-PASSPHRASE",
                        self.passphrase
                            .as_deref()
                            .context("Bitget passphrase missing")?,
                    );
            }
        }
        if body.is_some() {
            request = request
                .header("Content-Type", "application/json")
                .body(raw_body);
        }
        Ok(request)
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        params: BTreeMap<String, String>,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        let response = self
            .request(method, path, &params, body.as_ref(), now_ms())?
            .send()
            .await
            .map_err(|e| e.without_url())
            .with_context(|| format!("{} {path} request failed", self.name()))?;
        let status = response.status().as_u16();
        anyhow::ensure!(
            (200..300).contains(&status),
            "{} {path} HTTP {status}",
            self.name()
        );
        let value: Value = response
            .json()
            .await
            .map_err(|e| e.without_url())
            .context("invalid unified account response JSON")?;
        // Private responses and exchange error text can contain account identity.
        // Expose only the documented numeric code, never the raw body.
        let data = match self.venue {
            Venue::Bybit => {
                let code = value["retCode"]
                    .as_i64()
                    .context("Bybit response omitted retCode")?;
                anyhow::ensure!(
                    code == 0 || (path == "/v5/position/set-leverage" && code == 110043),
                    "Bybit {path} error code {code}"
                );
                value.get("result")
            }
            Venue::Bitget => {
                let code = value["code"]
                    .as_str()
                    .context("Bitget response omitted code")?;
                let numeric_code: u64 = code.parse().context("invalid Bitget response code")?;
                anyhow::ensure!(code == "00000", "Bitget {path} error code {numeric_code}");
                value.get("data")
            }
        }
        .context("unified account response omitted data")?;
        Ok((status, data.clone()))
    }

    async fn mode(&self) -> Result<(u16, Value)> {
        self.call(Method::GET, self.mode_path(), BTreeMap::new(), None)
            .await
    }

    async fn checked_mode(&self, fixed_leverage: bool) -> Result<(u16, Value)> {
        let (status, data) = self.mode().await?;
        anyhow::ensure!(
            mode_valid(self.venue, &data),
            "{} account mode does not satisfy native UTA Exec requirements",
            self.name()
        );
        if fixed_leverage && self.venue == Venue::Bybit {
            anyhow::ensure!(
                data["marginMode"].as_str() != Some("PORTFOLIO_MARGIN"),
                "Bybit portfolio margin has no fixed per-symbol leverage"
            );
        }
        Ok((status, data))
    }

    async fn leverage(&self, symbol: &str) -> Result<(i32, u16, &'static str)> {
        let (status, settings) = self.checked_mode(true).await?;
        match self.venue {
            Venue::Bybit => {
                let path = "/v5/position/list";
                let (status, data) = self
                    .call(Method::GET, path, self.symbol_params(symbol), None)
                    .await?;
                Ok((bybit_leverage(&data, symbol)?, status, path))
            }
            Venue::Bitget => Ok((
                bitget_leverage(&settings, symbol)?,
                status,
                self.mode_path(),
            )),
        }
    }
}

fn mode_valid(venue: Venue, data: &Value) -> bool {
    match venue {
        Venue::Bybit => {
            matches!(data["unifiedMarginStatus"].as_i64(), Some(3 | 4 | 5 | 6))
                && matches!(
                    data["marginMode"].as_str(),
                    Some("REGULAR_MARGIN" | "PORTFOLIO_MARGIN")
                )
        }
        Venue::Bitget => {
            matches!(data["accountMode"].as_str(), Some("unified" | "hybrid"))
                && matches!(
                    data["accountLevel"].as_str(),
                    Some("basic" | "advanced" | "delta")
                )
                && data["holdMode"].as_str() == Some("one_way_mode")
        }
    }
}

fn positive_leverage(value: &Value) -> Result<i32> {
    let leverage = value
        .as_str()
        .and_then(|v| v.parse::<f64>().ok())
        .context("exchange did not return fixed leverage")?;
    anyhow::ensure!(
        leverage.is_finite()
            && leverage >= 1.0
            && leverage <= i32::MAX as f64
            && leverage.fract() == 0.0,
        "exchange returned unsupported non-integer or invalid leverage"
    );
    Ok(leverage as i32)
}

fn bybit_leverage(data: &Value, symbol: &str) -> Result<i32> {
    anyhow::ensure!(
        data["category"].as_str() == Some("linear"),
        "Bybit position category mismatch"
    );
    let rows = data["list"]
        .as_array()
        .context("Bybit positions omitted list")?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0]["symbol"].as_str() == Some(symbol),
        "Bybit did not return one position configuration for {symbol}"
    );
    anyhow::ensure!(
        rows[0]["positionIdx"].as_i64() == Some(0),
        "Bybit Exec requires one-way position mode"
    );
    positive_leverage(&rows[0]["leverage"])
}

fn bitget_leverage(data: &Value, symbol: &str) -> Result<i32> {
    let rows = data["symbolConfigList"]
        .as_array()
        .context("Bitget settings omitted symbolConfigList")?;
    let rows: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["category"].as_str() == Some("USDT-FUTURES")
                && row["symbol"].as_str() == Some(symbol)
        })
        .collect();
    anyhow::ensure!(
        rows.len() == 1,
        "Bitget did not return one leverage configuration for {symbol}"
    );
    anyhow::ensure!(
        rows[0]["marginMode"].as_str() == Some("crossed"),
        "Bitget Exec requires cross margin for {symbol}"
    );
    positive_leverage(&rows[0]["leverage"])
}

pub(super) async fn has_required_mode(source: &SourceConfig) -> Result<bool> {
    let api = AccountApi::load(source)?;
    let (_, data) = api.mode().await?;
    Ok(mode_valid(api.venue, &data))
}

fn leverage_result(
    source: &SourceConfig,
    api: &AccountApi,
    symbol: &str,
    leverage: i32,
    status: u16,
    path: &str,
) -> SymbolContractLeverageResult {
    SymbolContractLeverageResult {
        source_id: source.id.clone(),
        symbol: symbol.into(),
        contract_leverage: leverage,
        exchange: api.name().into(),
        endpoint: path.into(),
        http_status: status,
        recorded_contract_leverage: None,
    }
}

pub(super) async fn get_leverage(
    source: &SourceConfig,
    symbol: &str,
) -> Result<SymbolContractLeverageResult> {
    let api = AccountApi::load(source)?;
    let (leverage, status, path) = api.leverage(symbol).await?;
    Ok(leverage_result(
        source, &api, symbol, leverage, status, path,
    ))
}

pub(super) async fn set_leverage(
    source: &SourceConfig,
    request: &SaveSymbolContractLeverageRequest,
) -> Result<SymbolContractLeverageResult> {
    let api = AccountApi::load(source)?;
    // Observe position/margin mode first; never silently change either mode.
    if api.venue == Venue::Bitget {
        let (_, settings) = api.checked_mode(true).await?;
        let rows = settings["symbolConfigList"]
            .as_array()
            .context("Bitget settings omitted symbolConfigList")?;
        // An untouched symbol has no override. Setting its cross leverage is
        // supported; an existing isolated/hedged configuration must be rejected.
        if rows.iter().any(|row| {
            row["symbol"].as_str() == Some(&request.symbol)
                && row["category"].as_str() == Some("USDT-FUTURES")
        }) {
            bitget_leverage(&settings, &request.symbol)?;
        }
    } else {
        api.leverage(&request.symbol).await?;
    }
    let leverage = request.contract_leverage.to_string();
    let (path, body) = match api.venue {
        Venue::Bybit => (
            "/v5/position/set-leverage",
            json!({"category":"linear","symbol":request.symbol,"buyLeverage":leverage,"sellLeverage":leverage}),
        ),
        Venue::Bitget => (
            "/api/v3/account/set-leverage",
            json!({"category":"USDT-FUTURES","symbol":request.symbol,"leverage":leverage,"marginMode":"crossed"}),
        ),
    };
    let (status, _) = api
        .call(Method::POST, path, BTreeMap::new(), Some(body))
        .await?;
    let (actual, _, _) = api.leverage(&request.symbol).await?;
    anyhow::ensure!(
        actual == request.contract_leverage,
        "leverage write was accepted but exchange readback differs; refresh before retrying"
    );
    Ok(leverage_result(
        source,
        &api,
        &request.symbol,
        actual,
        status,
        path,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Bytes,
        extract::{ConnectInfo, State},
        http::{HeaderMap, Uri},
        routing::any,
    };
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    #[derive(Clone)]
    struct MockState {
        venue: Venue,
        source_ip: std::net::IpAddr,
        leverage: Arc<Mutex<i32>>,
        posts: Arc<Mutex<usize>>,
        bad_mode: Arc<Mutex<bool>>,
    }

    async fn endpoint(
        State(state): State<MockState>,
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Json<Value> {
        assert_eq!(
            peer.ip(),
            state.source_ip,
            "private requests must bind the source IP"
        );
        let raw = std::str::from_utf8(&body).unwrap();
        match state.venue {
            Venue::Bybit => {
                assert_eq!(headers["X-BAPI-API-KEY"], "fixture-key");
                let timestamp = headers["X-BAPI-TIMESTAMP"].to_str().unwrap();
                let data = if method == Method::GET {
                    uri.query().unwrap_or("")
                } else {
                    raw
                };
                assert_eq!(
                    headers["X-BAPI-SIGN"],
                    sign_hmac_hex(
                        "fixture-secret",
                        &format!("{timestamp}fixture-key5000{data}")
                    )
                    .unwrap()
                );
            }
            Venue::Bitget => {
                assert_eq!(headers["ACCESS-KEY"], "fixture-key");
                assert_eq!(headers["ACCESS-PASSPHRASE"], "fixture-passphrase");
                let timestamp = headers["ACCESS-TIMESTAMP"].to_str().unwrap();
                assert_eq!(
                    headers["ACCESS-SIGN"],
                    sign_hmac_base64(
                        "fixture-secret",
                        &format!("{timestamp}{method}{}{raw}", uri.path_and_query().unwrap())
                    )
                    .unwrap()
                );
            }
        }
        let mut leverage = state.leverage.lock().unwrap();
        let bad_mode = *state.bad_mode.lock().unwrap();
        let data = match uri.path() {
            "/v5/account/info" => {
                json!({"unifiedMarginStatus":if bad_mode {1} else {6},"marginMode":"REGULAR_MARGIN"})
            }
            "/api/v3/account/settings" => {
                json!({"accountMode":if bad_mode {"upgrading"} else {"unified"},"accountLevel":"advanced","holdMode":"one_way_mode","symbolConfigList":[{"category":"USDT-FUTURES","symbol":"BTCUSDT","marginMode":"crossed","leverage":leverage.to_string()}]})
            }
            "/v5/position/list" => {
                assert_eq!(uri.query(), Some("category=linear&symbol=BTCUSDT"));
                json!({"category":"linear","list":[{"symbol":"BTCUSDT","positionIdx":0,"leverage":leverage.to_string()}]})
            }
            "/v5/position/set-leverage" | "/api/v3/account/set-leverage" => {
                assert_eq!(method, Method::POST);
                *state.posts.lock().unwrap() += 1;
                let value: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(value["symbol"], "BTCUSDT");
                if state.venue == Venue::Bybit {
                    assert_eq!(value["category"], "linear");
                    assert_eq!(value["buyLeverage"], value["sellLeverage"]);
                    *leverage = value["buyLeverage"].as_str().unwrap().parse().unwrap();
                } else {
                    assert_eq!(value["category"], "USDT-FUTURES");
                    assert_eq!(value["marginMode"], "crossed");
                    *leverage = value["leverage"].as_str().unwrap().parse().unwrap();
                }
                json!({})
            }
            "/v5/account/fee-rate" => {
                json!({"list":[{"symbol":"BTCUSDT","makerFeeRate":"-0.0001","takerFeeRate":"0.0005"}]})
            }
            "/api/v3/account/fee-rate" => {
                assert_eq!(uri.query(), Some("category=USDT-FUTURES&symbol=BTCUSDT"));
                json!({"makerFeeRate":"-0.0001","takerFeeRate":"0.0005"})
            }
            _ => panic!("unexpected request path"),
        };
        Json(match state.venue {
            Venue::Bybit => json!({"retCode":0,"result":data}),
            Venue::Bitget => json!({"code":"00000","data":data}),
        })
    }

    #[tokio::test]
    async fn native_uta_apis_sign_bind_verify_and_block_invalid_modes() {
        // UDP connect selects a local address without sending any datagram.
        // Account egress validation intentionally rejects loopback addresses.
        let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let source_ip = route.local_addr().unwrap().ip();
        for (venue, name, prefix) in [
            (Venue::Bybit, "bybit-futures", "BYBIT"),
            (Venue::Bitget, "bitget-futures", "BITGET"),
        ] {
            let state = MockState {
                venue,
                source_ip,
                leverage: Arc::new(Mutex::new(3)),
                posts: Arc::new(Mutex::new(0)),
                bad_mode: Arc::new(Mutex::new(false)),
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new()
                .fallback(any(endpoint))
                .with_state(state.clone());
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            });
            let dir = tempfile::tempdir().unwrap();
            let env = dir.path().join("env.sh");
            fs::write(&env, format!("{prefix}_API_KEY=fixture-key\n{prefix}_API_SECRET=fixture-secret\nBITGET_API_PASSPHRASE=fixture-passphrase\n{prefix}_BASE_URL={base}\n")).unwrap();
            fs::write(
                dir.path().join("trade_engine.toml"),
                format!("local_ips=['{source_ip}']\n"),
            )
            .unwrap();
            let source: SourceConfig = toml::from_str(&format!("id='fixture'\naccount='fixture'\nvenue='{name}'\nrocksdb_path='{}'\nenv_path='{}'\n", dir.path().join("data/persist_manager").display(), env.display())).unwrap();
            assert!(
                super::super::has_required_trading_account_mode(&source)
                    .await
                    .unwrap()
            );
            assert_eq!(
                super::super::get_symbol_contract_leverage(&source, "BTCUSDT")
                    .await
                    .unwrap()
                    .contract_leverage,
                3
            );
            let request = SaveSymbolContractLeverageRequest {
                symbol: "BTCUSDT".into(),
                contract_leverage: 7,
            };
            assert_eq!(
                super::super::set_symbol_contract_leverage(&source, &request)
                    .await
                    .unwrap()
                    .contract_leverage,
                7
            );
            assert_eq!(*state.posts.lock().unwrap(), 1);
            let fees = super::super::get_exchange_fee_rates(&source, "BTCUSDT")
                .await
                .unwrap();
            assert_eq!(fees.vip_tier, None);
            assert_eq!(
                (fees.maker_fee_rate, fees.taker_fee_rate),
                (-0.0001, 0.0005)
            );
            *state.bad_mode.lock().unwrap() = true;
            assert!(
                !super::super::has_required_trading_account_mode(&source)
                    .await
                    .unwrap()
            );
            assert!(
                super::super::set_symbol_contract_leverage(&source, &request)
                    .await
                    .is_err()
            );
            assert_eq!(*state.posts.lock().unwrap(), 1);
            fs::remove_file(dir.path().join("trade_engine.toml")).unwrap();
            assert!(
                super::super::get_exchange_fee_rates(&source, "BTCUSDT")
                    .await
                    .is_err()
            );
            server.abort();
        }
    }

    #[test]
    fn missing_or_unsafe_modes_and_ambiguous_leverage_fail_closed() {
        for value in [
            json!({}),
            json!({"unifiedMarginStatus":1,"marginMode":"REGULAR_MARGIN"}),
            json!({"unifiedMarginStatus":6,"marginMode":"ISOLATED_MARGIN"}),
        ] {
            assert!(!mode_valid(Venue::Bybit, &value));
        }
        assert!(mode_valid(
            Venue::Bybit,
            &json!({"unifiedMarginStatus":6,"marginMode":"PORTFOLIO_MARGIN"})
        ));
        for value in [
            json!({}),
            json!({"accountMode":"upgrading","accountLevel":"advanced","holdMode":"one_way_mode"}),
            json!({"accountMode":"unified","accountLevel":"advanced","holdMode":"hedge_mode"}),
        ] {
            assert!(!mode_valid(Venue::Bitget, &value));
        }
        let row = json!({"symbol":"BTCUSDT","positionIdx":1,"leverage":"5"});
        assert!(bybit_leverage(&json!({"category":"linear","list":[row]}), "BTCUSDT").is_err());
        assert!(positive_leverage(&json!("NaN")).is_err());
        assert!(positive_leverage(&json!("")).is_err());
        assert!(positive_leverage(&json!("0")).is_err());
        assert!(fee(&json!({"makerFeeRate":"NaN"}), "makerFeeRate").is_err());
        assert!(bybit_fee_row(&json!({"list":[{"symbol":"ETHUSDT"}]}), "BTCUSDT").is_err());
    }
}

fn fee(data: &Value, field: &str) -> Result<f64> {
    let value = data[field]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .context("missing or invalid account fee rate")?;
    anyhow::ensure!(
        value.is_finite() && value.abs() <= 1.0,
        "invalid account fee rate"
    );
    Ok(value)
}

fn bybit_fee_row<'a>(data: &'a Value, symbol: &str) -> Result<&'a Value> {
    let rows = data["list"]
        .as_array()
        .context("Bybit fee response omitted list")?;
    let rows: Vec<_> = rows
        .iter()
        .filter(|row| row["symbol"].as_str() == Some(symbol))
        .collect();
    anyhow::ensure!(
        rows.len() == 1,
        "Bybit fee response did not identify requested symbol"
    );
    Ok(rows[0])
}

pub(super) async fn get_fees(
    source: &SourceConfig,
    symbol: &str,
) -> Result<ExchangeFeeRatesResult> {
    let api = AccountApi::load(source)?;
    let (account_status, _) = api.checked_mode(false).await?;
    let path = match api.venue {
        Venue::Bybit => "/v5/account/fee-rate",
        Venue::Bitget => "/api/v3/account/fee-rate",
    };
    let (status, data) = api
        .call(Method::GET, path, api.symbol_params(symbol), None)
        .await?;
    let rates = if api.venue == Venue::Bybit {
        bybit_fee_row(&data, symbol)?
    } else {
        &data
    };
    Ok(ExchangeFeeRatesResult {
        source_id: source.id.clone(),
        symbol: symbol.into(),
        exchange: api.name().into(),
        // Neither endpoint reports VIP tier; do not infer it from fee schedules.
        vip_tier: None,
        maker_fee_rate: fee(rates, "makerFeeRate")?,
        taker_fee_rate: fee(rates, "takerFeeRate")?,
        account_endpoint: api.mode_path().into(),
        commission_endpoint: path.into(),
        account_http_status: account_status,
        commission_http_status: status,
    })
}
