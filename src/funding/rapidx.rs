//! RapidX factual statement API; no exchange order or Exec-store writes.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response};
use serde_json::Value;
use sha2::Sha256;

use super::{IncomeRow, ValidIncome};
use crate::config::SourceConfig;

const PAGE_SIZE: usize = 1000;
const PATH: &str = "/api/v1/trading/statement";

fn params(
    symbol: &str,
    from_ms: i64,
    through_ms: i64,
    page: usize,
) -> Result<BTreeMap<String, String>> {
    let quote = ["USDT", "USDC"]
        .into_iter()
        .find(|quote| symbol.ends_with(quote))
        .context("unsupported RapidX funding quote")?;
    let base = symbol
        .strip_suffix(quote)
        .filter(|base| {
            !base.is_empty()
                && base
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
        .context("invalid RapidX funding symbol")?;
    ensure!(
        from_ms > 0 && through_ms >= from_ms && (1..=100).contains(&page),
        "invalid RapidX statement range/page"
    );
    Ok(BTreeMap::from([
        ("exchange".into(), "BINANCE".into()),
        ("sym".into(), format!("BINANCE_PERP_{base}_{quote}")),
        ("statementType".into(), "FUNDING_FEE".into()),
        ("startTime".into(), from_ms.to_string()),
        ("endTime".into(), through_ms.to_string()),
        ("page".into(), page.to_string()),
        ("pageSize".into(), PAGE_SIZE.to_string()),
    ]))
}

fn signature(secret: &str, params: &BTreeMap<String, String>, nonce: &str) -> Result<String> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).context("invalid RapidX signing key")?;
    let mut raw = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    raw.push('&');
    raw.push_str(nonce);
    mac.update(raw.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

pub(super) async fn request_page(
    client: &Client,
    source: &SourceConfig,
    symbol: &str,
    from_ms: i64,
    through_ms: i64,
    page: usize,
) -> Result<Response> {
    let values = crate::market_rules::source_values(source)?;
    let required = |key| {
        values
            .get(key)
            .filter(|v| !v.trim().is_empty())
            .context("RapidX funding credentials missing")
    };
    let api_key = required("LTP_API_KEY")?;
    let secret = required("LTP_API_SECRET")?;
    let base = values
        .get("LTP_REST_URL")
        .map(String::as_str)
        .filter(|v| !v.is_empty())
        .unwrap_or("https://api.liquiditytech.com")
        .trim_end_matches('/');
    // Credentials must only go to the authoritative HTTPS API, without redirects.
    ensure!(
        base == "https://api.liquiditytech.com",
        "unsupported RapidX funding API origin"
    );
    let params = params(symbol, from_ms, through_ms, page)?;
    let ts = crate::kline::now_us();
    let nonce = (ts / 1_000_000).to_string();
    let signature = signature(secret, &params, &nonce)?;
    client
        .get(format!("{base}{PATH}"))
        .query(&params)
        .header("X-MBX-APIKEY", api_key)
        .header("nonce", nonce)
        .header("ts", ts.to_string())
        .header("signature", signature)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("RapidX funding statement request failed")
}

pub(super) fn parse_page(
    value: Value,
    source: &SourceConfig,
    symbol: &str,
    from: i64,
    through: i64,
    page: usize,
    previous: Option<(usize, usize)>,
) -> Result<(usize, usize, Vec<ValidIncome>)> {
    ensure!(
        matches!(value["code"].as_i64(), Some(200 | 200000)),
        "RapidX statement response code invalid"
    );
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .context("RapidX statement data missing")?;
    let number = |key| {
        data.get(key)
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .context("RapidX statement pagination missing")
    };
    let pages = number("pageNum")?;
    let total = number("totalSize")?;
    ensure!(
        number("page")? == page
            && number("pageSize")? == PAGE_SIZE
            && (1..=100).contains(&page)
            && pages <= 100
            && total <= PAGE_SIZE * 100
            && if total == 0 {
                page == 1 && pages <= 1
            } else {
                pages == total.div_ceil(PAGE_SIZE) && page <= pages
            },
        "RapidX statement pagination invalid"
    );
    ensure!(
        previous.is_none_or(|p| p == (pages, total)),
        "RapidX statement pagination changed; cursor retained"
    );
    let rows = data
        .get("list")
        .and_then(Value::as_array)
        .context("RapidX statement list missing")?;
    ensure!(
        rows.len() == total.saturating_sub((page - 1) * PAGE_SIZE).min(PAGE_SIZE),
        "RapidX statement page incomplete"
    );
    let values = crate::market_rules::source_values(source)?;
    let portfolio = values
        .get("LTP_PORTFOLIO_ID")
        .context("RapidX funding portfolio missing")?;
    ensure!(
        !portfolio.is_empty() && portfolio.bytes().all(|b| b.is_ascii_digit()),
        "invalid RapidX funding portfolio"
    );
    let expected_sym = params(symbol, (from + 999) / 1000, through / 1000, page)?["sym"].clone();
    let mut records = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        ensure!(
            row["portfolioId"].as_u64().map(|v| v.to_string()).as_ref() == Some(portfolio)
                && row["exchangeType"].as_str() == Some("BINANCE")
                && row["businessType"].as_str() == Some("PERP")
                && row["sym"].as_str() == Some(expected_sym.as_str()),
            "RapidX funding statement account/market scope mismatch"
        );
        let text = |key| {
            row.get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("RapidX funding statement field missing")
        };
        let mut record = IncomeRow {
            symbol: symbol.to_owned(),
            asset: text("coin")?,
            income: text("deltaAmount")?,
            time: row["createAt"]
                .as_i64()
                .context("RapidX funding time missing")?,
            income_type: text("statementType")?,
            tran_id: Value::String(text("statementId")?),
        }
        .validate(symbol, from, through)?;
        record.record.source_id = source.id.clone();
        ensure!(
            ids.insert(record.tran_id.clone()),
            "RapidX statement repeated an ID"
        );
        records.push(record);
    }
    Ok((pages, total, records))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, SourceConfig, Value) {
        let temp = tempfile::tempdir().unwrap();
        let env = temp.path().join("exec.env");
        std::fs::write(
            &env,
            "TRADE_ENGINE_EXEC_BACKEND=rapidx\nLTP_PORTFOLIO_ID=123\n",
        )
        .unwrap();
        let source: SourceConfig=toml::from_str(&format!("id='rx01'\naccount='rx01'\nvenue='binance-futures'\nrocksdb_path='/source'\nenv_path='{}'",env.display())).unwrap();
        let body = serde_json::json!({"code":200000,"data":{"page":1,"pageSize":1000,"pageNum":1,"totalSize":1,
            "list":[{"portfolioId":123,"exchangeType":"BINANCE","businessType":"PERP","sym":"BINANCE_PERP_BTC_USDT",
                "coin":"USDT","deltaAmount":"-0.0123456789012345678","createAt":2000,"statementType":"FUNDING_FEE","statementId":"tx"}]}});
        (temp, source, body)
    }
    #[test]
    fn statements_preserve_signed_income_identity_and_require_exact_account_market_time() {
        let (_temp, source, body) = fixture();
        let (_, _, rows) = parse_page(
            body.clone(),
            &source,
            "BTCUSDT",
            1_000_000,
            3_000_000,
            1,
            None,
        )
        .unwrap();
        assert_eq!(rows[0].income, "-0.0123456789012345678");
        assert_eq!(rows[0].tran_id, "tx");
        assert_eq!(rows[0].record.ts_us, 2_000_000);
        for (key, value) in [
            ("portfolioId", Value::from(456)),
            ("sym", Value::from("BINANCE_PERP_ETH_USDT")),
            ("businessType", Value::from("MARGIN")),
            ("statementType", Value::from("DEDUCT_INTEREST")),
            ("coin", Value::from("BTC")),
            ("createAt", Value::from(4000)),
            ("deltaAmount", Value::from("NaN")),
        ] {
            let mut wrong = body.clone();
            wrong["data"]["list"][0][key] = value;
            assert!(
                parse_page(wrong, &source, "BTCUSDT", 1_000_000, 3_000_000, 1, None).is_err(),
                "{key}"
            );
        }
    }
    #[test]
    fn incomplete_or_changed_pages_are_rejected_and_empty_history_is_valid() {
        let (_temp, source, body) = fixture();
        assert!(
            parse_page(
                body.clone(),
                &source,
                "BTCUSDT",
                1_000_000,
                3_000_000,
                1,
                Some((2, 1001))
            )
            .is_err()
        );
        for (pages, total) in [(2, 1001), (101, 100001), (1, 0)] {
            let mut wrong = body.clone();
            wrong["data"]["pageNum"] = Value::from(pages);
            wrong["data"]["totalSize"] = Value::from(total);
            assert!(parse_page(wrong, &source, "BTCUSDT", 1_000_000, 3_000_000, 1, None).is_err());
        }
        for pages in [0, 1] {
            let mut empty = body.clone();
            empty["data"]["pageNum"] = Value::from(pages);
            empty["data"]["totalSize"] = Value::from(0);
            empty["data"]["list"] = serde_json::json!([]);
            assert!(
                parse_page(empty, &source, "BTCUSDT", 1_000_000, 3_000_000, 1, None)
                    .unwrap()
                    .2
                    .is_empty()
            );
        }
    }
    #[test]
    fn request_scopes_only_funding_and_signature_uses_sorted_raw_parameters() {
        let params = params("BTCUSDT", 1000, 2000, 1).unwrap();
        assert_eq!(params["sym"], "BINANCE_PERP_BTC_USDT");
        assert_eq!(params["statementType"], "FUNDING_FEE");
        let raw = "endTime=2000&exchange=BINANCE&page=1&pageSize=1000&startTime=1000&statementType=FUNDING_FEE&sym=BINANCE_PERP_BTC_USDT&123";
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(raw.as_bytes());
        assert_eq!(
            signature("secret", &params, "123").unwrap(),
            hex::encode(mac.finalize().into_bytes())
        );
        assert!(super::params("BTC", 1000, 2000, 1).is_err());
    }
}
