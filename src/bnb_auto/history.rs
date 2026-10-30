//! Read-only recovery of identified wallet operations after their balance receipt
//! has been spent. Missing IDs or incomplete/non-final history never permit retry.
use super::{BinanceApi, Pending, now_ms};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) struct Resolution {
    pub succeeded: bool,
    pub record: Value,
}

pub(super) async fn resolve(api: &BinanceApi, pending: &Pending) -> Result<Option<Resolution>> {
    // Give normal balance propagation time before spending history-query weight.
    if now_ms().saturating_sub(pending.at_ms) < 30_000 {
        return Ok(None);
    }
    let (path, id_key, time_key, success_status) = match pending.path.as_str() {
        "/sapi/v1/asset/transfer" => (
            "/sapi/v1/asset/transfer",
            "tranId",
            "timestamp",
            "CONFIRMED",
        ),
        "/sapi/v1/simple-earn/flexible/redeem" => (
            "/sapi/v1/simple-earn/flexible/history/redemptionRecord",
            "redeemId",
            "time",
            "PAID",
        ),
        "/sapi/v1/simple-earn/flexible/subscribe" => (
            "/sapi/v1/simple-earn/flexible/history/subscriptionRecord",
            "purchaseId",
            "time",
            "SUCCESS",
        ),
        _ => return Ok(None),
    };
    let Some(id) = pending.response.as_ref().and_then(|v| integer(&v[id_key])) else {
        return Ok(None);
    };
    if id == 0 {
        return Ok(None);
    }
    // Allow exchange second precision and the signed request's clock tolerance.
    // Fix both boundaries to the original submission, even after a restart.
    let start = pending.at_ms.saturating_sub(60_000);
    let end = pending.at_ms.saturating_add(300_000);
    let mut params = vec![
        ("startTime", start.to_string()),
        ("endTime", end.min(now_ms()).to_string()),
        ("size", "100".into()),
    ];
    if id_key == "tranId" {
        params.push(("type", parameter(pending, "type")?.into()));
    } else {
        params.push(("asset", "BNB".into()));
        params.push(("productId", parameter(pending, "productId")?.into()));
        params.push((id_key, id.to_string()));
    }
    let amount = parameter(pending, "amount")?;
    let mut total = None;
    let mut ids = BTreeSet::new();
    let mut matched = None;
    for page in 1..=100 {
        let current = page.to_string();
        let mut borrowed: Vec<_> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        borrowed.push(("current", &current));
        let body = api.get(false, path, &borrowed).await?;
        let count = integer(&body["total"]).context("BNB operation history total missing")?;
        ensure!(
            total.is_none_or(|old| old == count),
            "BNB operation history changed during pagination"
        );
        total = Some(count);
        let rows = body["rows"]
            .as_array()
            .context("BNB operation history rows missing")?;
        ensure!(
            rows.len() <= 100,
            "BNB operation history page exceeds limit"
        );
        for row in rows {
            let row_id = integer(&row[id_key]).context("BNB operation history ID missing")?;
            ensure!(
                ids.insert(row_id),
                "BNB operation history contains duplicate IDs"
            );
            if row_id != id {
                continue;
            }
            ensure!(
                integer(&row[time_key]).is_some_and(|at| (start..=end).contains(&at)),
                "BNB operation history time differs from journal"
            );
            let asset = if id_key == "tranId" {
                parameter(pending, "asset")?
            } else {
                "BNB"
            };
            ensure!(
                row["asset"].as_str() == Some(asset),
                "BNB operation history asset differs from journal"
            );
            ensure!(
                row["amount"]
                    .as_str()
                    .is_some_and(|observed| same_decimal(observed, amount)),
                "BNB operation history amount differs from journal"
            );
            if id_key == "tranId" {
                ensure!(
                    row["type"].as_str() == Some(parameter(pending, "type")?),
                    "BNB operation history transfer direction differs from journal"
                );
            } else {
                // Binance documents projectId for redemptions, while live
                // responses use productId. Validate every supplied spelling.
                let mut product_seen = false;
                for key in ["productId", "projectId"] {
                    if let Some(product) = row.get(key) {
                        ensure!(
                            product.as_str() == Some(parameter(pending, "productId")?),
                            "BNB operation history product differs from journal"
                        );
                        product_seen = true;
                    }
                }
                ensure!(product_seen, "BNB operation history product missing");
                let account_key = if id_key == "redeemId" {
                    "destAccount"
                } else {
                    "sourceAccount"
                };
                ensure!(
                    row[account_key].as_str() == Some(parameter(pending, account_key)?),
                    "BNB operation history wallet differs from journal"
                );
            }
            matched = Some(row.clone());
        }
        ensure!(
            ids.len() as u64 <= count,
            "BNB operation history exceeds total"
        );
        if ids.len() as u64 == count {
            return Ok(matched.and_then(|record| {
                let status = record["status"].as_str()?;
                let succeeded = status == success_status;
                (succeeded || status == "FAILED").then_some(Resolution { succeeded, record })
            }));
        }
        ensure!(
            !rows.is_empty(),
            "BNB operation history pagination incomplete"
        );
    }
    anyhow::bail!("BNB operation history pagination exceeded limit")
}

fn parameter<'a>(pending: &'a Pending, name: &str) -> Result<&'a str> {
    pending
        .params
        .get(name)
        .map(String::as_str)
        .with_context(|| format!("BNB pending operation parameter {name} missing"))
}

fn integer(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

// Financial identity uses decimal digits, without an absolute float tolerance
// that could confuse adjacent satoshis in the tiny transfer that caused this bug.
fn same_decimal(left: &str, right: &str) -> bool {
    fn parts(value: &str) -> Option<(&str, &str)> {
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some((
            whole.trim_start_matches('0'),
            fraction.trim_end_matches('0'),
        ))
    }
    parts(left).is_some_and(|left| Some(left) == parts(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn financial_amount_matching_preserves_tiny_differences() {
        assert!(same_decimal("0.0000065700", "00.00000657"));
        assert!(same_decimal("001", "1.00000000"));
        assert!(!same_decimal("0.00000658", "0.00000657"));
        assert!(!same_decimal("NaN", "0"));
        assert!(!same_decimal("1e-8", "0.00000001"));
    }
}
