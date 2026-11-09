//! Read-only margin facts from native USD-M or the RapidX monitor snapshot.
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
    pub initial_margin: Option<f64>,
    pub frozen_margin: Option<f64>,
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
        if source.venue != "binance-futures" {
            return Ok(None);
        }
        if source
            .account_ipc_service
            .as_deref()
            .is_some_and(|s| s.contains("rapidx"))
        {
            return read_rapidx_margin(source).await;
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
        initial_margin: Some(margin),
        frozen_margin: None,
    })
}

async fn read_rapidx_margin(source: &SourceConfig) -> Result<Option<MarginSnapshot>> {
    let Some(stream) = source
        .account_ipc_service
        .as_deref()
        .and_then(|s| s.rsplit('/').next())
        .and_then(|s| s.strip_suffix("_pm"))
    else {
        return Ok(None);
    };
    let Some(portfolio) = stream.strip_prefix("rapidx_binance_") else {
        return Ok(None);
    };
    ensure!(
        !portfolio.is_empty()
            && portfolio
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "invalid RapidX account identity"
    );
    let path = source
        .rocksdb_path
        .parent()
        .context("source has no data directory")?
        .join("rapidx_account")
        .join(stream)
        .join("latest_financial.json");
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read RapidX account-monitor snapshot"),
    };
    ensure!(
        bytes.len() <= 2 * 1024 * 1024,
        "RapidX account-monitor snapshot exceeds limit"
    );
    let value: Value =
        serde_json::from_slice(&bytes).context("invalid RapidX account-monitor snapshot")?;
    parse_rapidx_margin(&value, portfolio, super::now_ms())
}

fn parse_rapidx_margin(value: &Value, portfolio: &str, now: i64) -> Result<Option<MarginSnapshot>> {
    if value["recovery_ready"].as_bool() != Some(true) {
        return Ok(None);
    }
    let financial = &value["financial"];
    let account = &financial["raw_account"];
    ensure!(
        financial["portfolio"].as_str() == Some(portfolio)
            && financial["exchange"].as_str() == Some("BINANCE")
            && account["portfolioId"].as_str() == Some(portfolio)
            && account["exchangeType"].as_str() == Some("BINANCE"),
        "RapidX margin snapshot scope mismatch"
    );
    // Use this channel's factual observation, not another financial channel's
    // heartbeat or an exchange update timestamp that may stay unchanged.
    let ts_ms = financial["source_observed_ms"]["Accounts"]
        .as_i64()
        .context("missing RapidX margin observation timestamp")?;
    if ts_ms <= 0 || !(0..=super::SAMPLE_FRESH_MS).contains(&now.saturating_sub(ts_ms)) {
        return Ok(None);
    }
    let frozen = account["frozenMargin"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| account["frozenMargin"].as_f64())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .context("invalid RapidX frozen margin")?;
    Ok(Some(MarginSnapshot {
        ts_ms,
        initial_margin: None,
        frozen_margin: Some(frozen),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rapidx_snapshot(ts: i64, margin: Value) -> Value {
        serde_json::json!({"recovery_ready":true,"financial":{
            "portfolio":"123","exchange":"BINANCE",
            "source_observed_ms":{"Accounts":ts,"Assets":ts+10},
            "raw_account":{"portfolioId":"123","exchangeType":"BINANCE",
                "updateAt":"1","maintainMargin":"7","frozenMargin":margin}
        }})
    }

    #[test]
    fn rapidx_frozen_margin_requires_matching_scope_fresh_channel_and_ready_recovery() {
        for (value, expected) in [("42.5", 42.5), ("0", 0.0)] {
            let row = parse_rapidx_margin(&rapidx_snapshot(1000, Value::from(value)), "123", 1100)
                .unwrap()
                .unwrap();
            assert_eq!(row.initial_margin, None);
            assert_eq!(row.frozen_margin, Some(expected));
            assert_eq!(row.ts_ms, 1000);
        }
        let value = rapidx_snapshot(1000, Value::from("42.5"));
        assert!(parse_rapidx_margin(&value, "other", 1100).is_err());
        assert!(
            parse_rapidx_margin(&value, "123", 1000 + super::super::SAMPLE_FRESH_MS + 1)
                .unwrap()
                .is_none()
        );
        assert!(parse_rapidx_margin(&value, "123", 999).unwrap().is_none());
        let mut invalid = value.clone();
        invalid["recovery_ready"] = false.into();
        assert!(
            parse_rapidx_margin(&invalid, "123", 1100)
                .unwrap()
                .is_none()
        );
        let mut invalid = value.clone();
        invalid["financial"]["raw_account"]["exchangeType"] = "OKX".into();
        assert!(parse_rapidx_margin(&invalid, "123", 1100).is_err());
        for invalid in [
            Value::Null,
            Value::from("-1"),
            Value::from("NaN"),
            Value::from("Infinity"),
        ] {
            assert!(parse_rapidx_margin(&rapidx_snapshot(1000, invalid), "123", 1100).is_err());
        }
    }

    #[tokio::test]
    async fn rapidx_reads_only_its_canonical_monitor_snapshot_without_native_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let mut source: SourceConfig = toml::from_str(&format!(
            "id='one'\naccount='one'\nvenue='binance-futures'\nrocksdb_path='{}'\naccount_ipc_service='account_pubs/rapidx_binance_123_pm'\nenv_path='/must-not-read-credentials'",
            dir.path().join("data/persist_manager").display())).unwrap();
        let path = dir
            .path()
            .join("data/rapidx_account/rapidx_binance_123/latest_financial.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&rapidx_snapshot(
                super::super::now_ms(),
                Value::from("42.5"),
            ))
            .unwrap(),
        )
        .unwrap();
        let reader = MarginReader::default();
        assert_eq!(
            reader.get(&source).await.unwrap().unwrap().frozen_margin,
            Some(42.5)
        );
        assert!(reader.cache.lock().unwrap().is_empty());
        source.account_ipc_service = Some("account_pubs/rapidx_binance_other_pm".into());
        assert!(reader.get(&source).await.unwrap().is_none());
        source.account_ipc_service = Some("account_pubs/rapidx_binance_123_pm".into());
        let mut foreign = rapidx_snapshot(super::super::now_ms(), Value::from("42.5"));
        foreign["financial"]["portfolio"] = "other".into();
        std::fs::write(path, serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert!(reader.get(&source).await.is_err());
    }
    #[test]
    fn factual_margin_preserves_zero_and_rejects_missing_invalid_values() {
        let data = serde_json::json!({"updateTime": 0, "totalInitialMargin": "83613.10", "totalMaintMargin": "5", "availableBalance": "100"});
        let row = parse_margin(&data, 123).unwrap();
        assert_eq!(row.initial_margin, Some(83613.10));
        assert_eq!(row.frozen_margin, None);
        assert_eq!(row.ts_ms, 123);
        assert_eq!(
            parse_margin(&serde_json::json!({"totalInitialMargin":"0"}), 123)
                .unwrap()
                .initial_margin,
            Some(0.0)
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
