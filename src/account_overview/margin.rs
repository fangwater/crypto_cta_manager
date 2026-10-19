//! Read-only fill-in for native USD-M monitors that do not yet publish margin.
//! Reuse the fixed account egress and shared request budget, never rotate IPs.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::bfusd_auto::{BinanceApi, account_fixed_ip};
use crate::config::SourceConfig;
use crate::exchange_leverage::parse_env_file;

#[derive(Clone, Debug)]
pub(super) struct MarginSnapshot {
    pub ts_ms: i64,
    pub initial_margin: f64,
}

#[derive(Clone, Default)]
pub(super) struct MarginReader {
    cache: Arc<Mutex<BTreeMap<String, Attempt>>>,
}
struct Attempt {
    at: Instant,
    snapshot: Option<MarginSnapshot>,
}

impl MarginReader {
    pub async fn get(&self, source: &SourceConfig) -> Result<Option<MarginSnapshot>> {
        if source.venue != "binance-futures"
            || source
                .account_ipc_service
                .as_deref()
                .is_some_and(|service| service.contains("rapidx"))
        {
            return Ok(None);
        }
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| anyhow::anyhow!("margin cache lock poisoned"))?;
            if let Some(attempt) = cache.get(&source.id) {
                if attempt.at.elapsed() < Duration::from_secs(30) {
                    return Ok(attempt.snapshot.clone().filter(|snapshot| {
                        (0..=super::SAMPLE_FRESH_MS)
                            .contains(&super::now_ms().saturating_sub(snapshot.ts_ms))
                    }));
                }
            }
            // Reserve before network access. Failed requests also wait for the
            // next poll interval, rather than retrying at browser frequency.
            cache.insert(
                source.id.clone(),
                Attempt {
                    at: Instant::now(),
                    snapshot: None,
                },
            );
        }
        let env = parse_env_file(&source.env_path())?;
        let mode = env
            .get("BINANCE_ACCOUNT_MODE")
            .map(String::as_str)
            .unwrap_or_default();
        ensure!(
            mode.eq_ignore_ascii_case("standard") || mode.eq_ignore_ascii_case("std"),
            "margin read requires an explicit native STANDARD account"
        );
        // This reads the current trading config, resolves wildcard bindings
        // without packets, and refuses missing/unbound/default-proxy access.
        let ip = account_fixed_ip(source)?;
        let api = BinanceApi::new(&env, ip)?;
        let result = api.get(true, "/fapi/v2/account", &[]).await?;
        let snapshot = parse_margin(&result, super::now_ms())?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow::anyhow!("margin cache lock poisoned"))?;
        cache.insert(
            source.id.clone(),
            Attempt {
                at: Instant::now(),
                snapshot: Some(snapshot.clone()),
            },
        );
        Ok(Some(snapshot))
    }
}

fn parse_margin(value: &Value, received_at_ms: i64) -> Result<MarginSnapshot> {
    // The endpoint's updateTime is reserved, not an observation timestamp.
    let margin = value
        .get("totalInitialMargin")
        .and_then(|value| {
            value
                .as_str()
                .and_then(|v| v.parse::<f64>().ok())
                .or_else(|| value.as_f64())
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
        .context("invalid account initial margin")?;
    ensure!(received_at_ms > 0, "invalid margin receipt time");
    Ok(MarginSnapshot {
        ts_ms: received_at_ms,
        initial_margin: margin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn factual_margin_preserves_zero_and_rejects_missing_invalid_values() {
        let data = serde_json::json!({"updateTime": 0, "totalInitialMargin": "83613.10", "totalMaintMargin": "5", "availableBalance": "100"});
        let row = parse_margin(&data, 123).unwrap();
        assert_eq!(row.initial_margin, 83613.10);
        assert_eq!(row.ts_ms, 123);
        assert_eq!(
            parse_margin(&serde_json::json!({"totalInitialMargin":"0"}), 123)
                .unwrap()
                .initial_margin,
            0.0
        );
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"totalInitialMargin":"NaN"}),
            serde_json::json!({"totalInitialMargin":"-1"}),
            serde_json::json!({"totalInitialMargin":null}),
        ] {
            assert!(parse_margin(&bad, 123).is_err());
        }
    }
    #[tokio::test]
    async fn coin_and_rapidx_never_use_native_account_credentials_or_endpoint() {
        let mut source: SourceConfig = toml::from_str(
            "id='one'\naccount='test'\nvenue='binance-coin-futures'\nrocksdb_path='/missing'",
        )
        .unwrap();
        let reader = MarginReader::default();
        assert!(reader.get(&source).await.unwrap().is_none());
        source.venue = "binance-futures".into();
        source.account_ipc_service = Some("account_pubs/rapidx_binance_01".into());
        assert!(reader.get(&source).await.unwrap().is_none());
        assert!(reader.cache.lock().unwrap().is_empty());
    }
}
