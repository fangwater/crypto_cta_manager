//! Recover unpriced and missing lifecycle fills from durable, uniquely identified
//! RapidX executions. The original Exec records remain read-only audit facts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::info;

use super::NavEvent;
use crate::config::SourceConfig;
use crate::rocks_source::{self, RawRocksRecord};

const EXECUTIONS_CF: &str = "rapidx_executions";
type OrderKey = (String, i16, i64);

pub(super) fn needs_price(event: &NavEvent) -> bool {
    event.amount_update > 0.0 && (!event.price.is_finite() || event.price <= 0.0)
}

fn order_key(event: &NavEvent) -> OrderKey {
    (
        event.symbol.clone(),
        event.venue_code,
        event.client_order_id,
    )
}

#[derive(Deserialize)]
struct StoredExecution {
    portfolio: String,
    exchange: String,
    execution: Execution,
}

#[derive(Deserialize)]
struct Execution {
    transaction_id: String,
    order_id: String,
    client_order_id: String,
    symbol: String,
    side: String,
    quantity: String,
    price: String,
    timestamp_ms: i64,
    rest: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Fill {
    transaction_id: String,
    order_id: String,
    side_code: i16,
    quantity: f64,
    price: f64,
    timestamp_us: i64,
}

/// Join only executions with an order already recorded by this Exec source.
/// Historical executions without a matching order keep their original identity.
pub(super) fn resolve(
    source: &SourceConfig,
    events: &im::Vector<Arc<NavEvent>>,
) -> Result<Option<im::Vector<Arc<NavEvent>>>> {
    if events.is_empty() {
        return Ok(None);
    }
    let unpriced = events.iter().any(|e| needs_price(e));
    let values = match crate::market_rules::source_values(source) {
        Ok(values) => values,
        Err(_) if !unpriced => return Ok(None),
        Err(error) => return Err(error),
    };
    let rapidx = matches!(
        crate::market_rules::execution_backend(&source.venue, &values),
        Ok("ltp")
    );
    if !rapidx {
        ensure!(
            !unpriced,
            "source {} has an unpriced fill without RapidX execution evidence",
            source.id
        );
        return Ok(None);
    }
    let wanted = events
        .iter()
        .filter(|e| e.order_type_code != crate::model::INTERNAL_CROSS_ORDER_TYPE_CODE)
        .filter(|e| e.client_order_id > 0)
        .map(|e| order_key(e))
        .collect::<BTreeSet<_>>();
    if wanted.is_empty() {
        ensure!(
            !unpriced,
            "unpriced fill has no matching external order identity"
        );
        return Ok(None);
    }
    let portfolio = values
        .get("LTP_PORTFOLIO_ID")
        .context("missing execution portfolio scope")?;
    ensure!(
        !portfolio.is_empty() && portfolio.bytes().all(|b| b.is_ascii_digit()),
        "invalid execution portfolio scope"
    );
    let exchange = match source.venue.as_str() {
        "binance-futures" | "binance-margin" => "BINANCE",
        "okex-futures" | "okex-margin" => "OKX",
        _ => anyhow::bail!(
            "unsupported execution evidence venue for source {}",
            source.id
        ),
    };
    let mut records =
        rocks_source::read_available_column_families(&source.rocksdb_path, &[EXECUTIONS_CF])?;
    let Some(records) = records.remove(EXECUTIONS_CF) else {
        ensure!(
            !unpriced,
            "source {} is waiting for durable execution prices: missing {EXECUTIONS_CF}",
            source.id
        );
        return Ok(None);
    };
    let fills = decode_fills(&records, portfolio, exchange, &wanted)
        .with_context(|| format!("source {} execution price evidence is invalid", source.id))?;
    let resolved = replace_orders(events, &wanted, &fills).with_context(|| {
        format!(
            "source {} is waiting for complete execution prices",
            source.id
        )
    })?;
    if resolved == *events {
        return Ok(None);
    }
    let recovered_orders = resolved
        .iter()
        .filter(|e| e.record_key.contains(":execution:"))
        .map(|e| order_key(e))
        .collect::<BTreeSet<_>>()
        .len();
    info!(
        source_id = source.id,
        recovered_orders, "recovered NAV fills from durable execution evidence"
    );
    Ok(Some(resolved))
}

fn decode_fills(
    records: &[RawRocksRecord],
    portfolio: &str,
    exchange: &str,
    wanted: &BTreeSet<OrderKey>,
) -> Result<BTreeMap<OrderKey, BTreeMap<String, Fill>>> {
    let mut result = BTreeMap::<OrderKey, BTreeMap<String, Fill>>::new();
    let wanted_clients = wanted.iter().map(|key| key.2).collect::<BTreeSet<_>>();
    for record in records {
        let stored: StoredExecution =
            serde_json::from_slice(&record.value).context("decode stored execution JSON")?;
        if stored.portfolio != portfolio || stored.exchange != exchange {
            continue;
        }
        let e = stored.execution;
        let Ok(client_order_id) = e.client_order_id.parse::<i64>() else {
            continue;
        };
        if !wanted_clients.contains(&client_order_id) {
            continue;
        }
        let parts = e.symbol.split('_').collect::<Vec<_>>();
        ensure!(
            parts.len() == 4
                && parts[0] == exchange
                && matches!(parts[3], "USDT" | "USDC")
                && !parts[2].is_empty(),
            "invalid execution symbol scope"
        );
        let venue_code = match (exchange, parts[1]) {
            ("BINANCE", "SPOT" | "MARGIN") => 0,
            ("BINANCE", "PERP") => 1,
            ("OKX", "SPOT" | "MARGIN") => 2,
            ("OKX", "PERP") => 3,
            _ => anyhow::bail!("unsupported execution market"),
        };
        let symbol = if matches!(venue_code, 0 | 2) {
            ensure!(parts[3] == "USDT", "cash execution requires USDT quote");
            parts[2].to_owned()
        } else {
            format!("{}{}", parts[2], parts[3])
        };
        let key = (symbol, venue_code, client_order_id);
        if !wanted.contains(&key) {
            continue;
        }

        // WS and REST have distinct storage keys but describe the same trade.
        // Validate the existing single storage contract before deduplicating.
        let mut hash = Sha256::new();
        hash.update(b"mkt_signal/rapidx_execution/stable_key");
        for part in [portfolio, exchange, &e.transaction_id] {
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part.as_bytes());
        }
        hash.update([u8::from(e.rest)]);
        let expected_key: [u8; 32] = hash.finalize().into();
        ensure!(
            record.key.as_slice() == expected_key,
            "execution storage identity mismatch"
        );
        let quantity: f64 = e.quantity.parse().context("invalid execution quantity")?;
        let price: f64 = e.price.parse().context("invalid execution price")?;
        ensure!(
            quantity.is_finite() && quantity > 0.0 && price.is_finite() && price > 0.0,
            "execution quantity and price must be finite and positive"
        );
        ensure!(
            !e.transaction_id.is_empty() && !e.order_id.is_empty() && client_order_id > 0,
            "execution identity is empty or invalid"
        );
        let timestamp_us = e
            .timestamp_ms
            .checked_mul(1_000)
            .filter(|t| *t > 0)
            .context("invalid execution timestamp")?;
        let side_code = match e.side.as_str() {
            "BUY" => 1,
            "SELL" => 2,
            _ => anyhow::bail!("invalid execution side"),
        };
        let fill = Fill {
            transaction_id: e.transaction_id,
            order_id: e.order_id,
            side_code,
            quantity,
            price,
            timestamp_us,
        };
        let order = result.entry(key).or_default();
        if let Some(old) = order.get(&fill.transaction_id) {
            ensure!(
                old == &fill,
                "conflicting WS/REST execution {}",
                fill.transaction_id
            );
        } else {
            order.insert(fill.transaction_id.clone(), fill);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(price: f64, quantity: f64, receive_us: i64) -> Arc<NavEvent> {
        Arc::new(NavEvent {
            record_key: format!("{receive_us:020}"),
            event_ts_us: receive_us,
            create_ts_us: 1,
            update_ts_us: receive_us,
            signal_ts_us: 1,
            symbol: "FIL".into(),
            client_order_id: 7,
            venue_code: 0,
            venue: "BinanceMargin".into(),
            order_type_code: 3,
            side_code: 2,
            price,
            amount_update: quantity,
            from_key_text: "batch_exec:alpha".into(),
            fill_liquidity: Some(crate::model::FillLiquidity::Unknown),
        })
    }

    pub(super) fn record(
        portfolio: &str,
        market: &str,
        tx: &str,
        qty: f64,
        price: f64,
        rest: bool,
    ) -> RawRocksRecord {
        let row = serde_json::json!({"portfolio":portfolio,"exchange":"BINANCE","execution":{
            "transaction_id":tx,"order_id":"88","client_order_id":"7",
            "symbol":format!("BINANCE_{market}_FIL_USDT"),"side":"SELL",
            "quantity":qty.to_string(),"price":price.to_string(),
            "timestamp_ms":if tx == "a" {100} else {110},"rest":rest
        }});
        let mut hash = Sha256::new();
        hash.update(b"mkt_signal/rapidx_execution/stable_key");
        for value in [portfolio, "BINANCE", tx] {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        hash.update([u8::from(rest)]);
        RawRocksRecord {
            key: hash.finalize().to_vec(),
            value: serde_json::to_vec(&row).unwrap(),
        }
    }

    #[test]
    fn uses_each_real_price_once_and_preserves_execution_time_and_attribution() {
        let events = im::vector![event(0.0, 3.0, 200_000)];
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let records = vec![
            record("123", "MARGIN", "a", 1.0, 100.0, false),
            record("123", "MARGIN", "a", 1.0, 100.0, true),
            record("123", "MARGIN", "b", 2.0, 103.0, true),
            // Identical client/transaction IDs in another portfolio or market
            // cannot provide coverage or duplicate this account's cash fill.
            record("456", "MARGIN", "a", 99.0, 999.0, false),
            record("123", "PERP", "a", 99.0, 999.0, false),
        ];
        let fills = decode_fills(&records, "123", "BINANCE", &wanted).unwrap();
        let result = replace_orders(&events, &wanted, &fills).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.iter().map(|e| e.amount_update).sum::<f64>(), 3.0);
        assert_eq!(
            result
                .iter()
                .map(|e| e.price * e.amount_update)
                .sum::<f64>(),
            306.0
        );
        assert_eq!(
            result.iter().map(|e| e.update_ts_us).collect::<Vec<_>>(),
            [100_000, 110_000]
        );
        assert!(
            result
                .iter()
                .all(|e| e.from_key_text == "batch_exec:alpha" && e.signal_ts_us == 1)
        );
        assert_eq!(events[0].price, 0.0);
        // The anchor remains stable when another terminal delta requires a
        // later replacement of an already recovered order.
        assert_eq!(replace_orders(&result, &wanted, &fills).unwrap(), result);
    }

    #[test]
    fn replaces_previously_priced_partial_fills_without_adding_quantity() {
        let events = im::vector![event(100.0, 1.0, 90_000), event(0.0, 2.0, 200_000)];
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let fills = decode_fills(
            &[
                record("123", "MARGIN", "a", 1.0, 100.0, false),
                record("123", "MARGIN", "b", 2.0, 103.0, false),
            ],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        let result = replace_orders(&events, &wanted, &fills).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.iter().map(|e| e.amount_update).sum::<f64>(), 3.0);
        assert_eq!(
            result
                .iter()
                .map(|e| e.price * e.amount_update)
                .sum::<f64>(),
            306.0
        );
    }

    #[test]
    fn incomplete_or_conflicting_evidence_never_consumes_a_lifecycle_delta() {
        let events = im::vector![event(0.0, 3.0, 200_000)];
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let partial = decode_fills(
            &[record("123", "MARGIN", "a", 1.0, 100.0, false)],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        assert!(
            replace_orders(&events, &wanted, &partial)
                .unwrap_err()
                .to_string()
                .contains("quantity mismatch")
        );
        let conflict = [
            record("123", "MARGIN", "a", 3.0, 100.0, false),
            record("123", "MARGIN", "a", 3.0, 101.0, true),
        ];
        assert!(
            decode_fills(&conflict, "123", "BINANCE", &wanted)
                .unwrap_err()
                .to_string()
                .contains("conflicting WS/REST")
        );
        let foreign = decode_fills(
            &[record("456", "MARGIN", "a", 3.0, 100.0, false)],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        assert!(replace_orders(&events, &wanted, &foreign).is_err());
        let mut wrong_key = record("123", "MARGIN", "a", 3.0, 100.0, false);
        wrong_key.key[0] ^= 1;
        assert!(decode_fills(&[wrong_key], "123", "BINANCE", &wanted).is_err());
    }

    #[test]
    fn zero_fill_lifecycle_recovers_only_matching_deduplicated_executions() {
        let events = im::vector![event(0.0, 0.0, 90_000)];
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let fills = decode_fills(
            &[
                record("123", "MARGIN", "a", 1.0, 100.0, false),
                record("123", "MARGIN", "a", 1.0, 100.0, true),
                record("123", "MARGIN", "b", 2.0, 103.0, true),
                record("123", "PERP", "x", 99.0, 999.0, true),
                record("456", "MARGIN", "x", 99.0, 999.0, true),
            ],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        let result = replace_orders(&events, &wanted, &fills).unwrap();
        assert_eq!(result.iter().filter(|e| e.amount_update > 0.0).count(), 2);
        assert_eq!(result.iter().map(|e| e.amount_update).sum::<f64>(), 3.0);
        assert!(
            result
                .iter()
                .all(|e| e.from_key_text == events[0].from_key_text)
        );
        assert_eq!(events[0].amount_update, 0.0);
        assert_eq!(
            replace_orders(&events, &wanted, &BTreeMap::new()).unwrap(),
            events
        );
    }

    #[test]
    fn missing_partial_is_recovered_and_late_lifecycle_does_not_duplicate_it() {
        let partial = im::vector![event(100.0, 1.0, 90_000)];
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let fills = decode_fills(
            &[
                record("123", "MARGIN", "a", 1.0, 100.0, false),
                record("123", "MARGIN", "b", 2.0, 103.0, true),
            ],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        let recovered = replace_orders(&partial, &wanted, &fills).unwrap();
        assert_eq!(recovered.iter().map(|e| e.amount_update).sum::<f64>(), 3.0);
        let mut complete = partial.clone();
        complete.push_back(event(103.0, 2.0, 110_000));
        let late = replace_orders(&complete, &wanted, &fills).unwrap();
        assert_eq!(late, complete);
        assert_eq!(late.iter().map(|e| e.amount_update).sum::<f64>(), 3.0);
    }

    #[test]
    fn execution_before_recorded_order_creation_is_not_reassigned() {
        let mut lifecycle = (*event(0.0, 0.0, 200_000)).clone();
        lifecycle.create_ts_us = 150_000;
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let fills = decode_fills(
            &[record("123", "MARGIN", "a", 1.0, 100.0, true)],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        assert!(replace_orders(&im::vector![Arc::new(lifecycle)], &wanted, &fills).is_err());
    }
}

fn replace_orders(
    events: &im::Vector<Arc<NavEvent>>,
    wanted: &BTreeSet<OrderKey>,
    fills: &BTreeMap<OrderKey, BTreeMap<String, Fill>>,
) -> Result<im::Vector<Arc<NavEvent>>> {
    let mut replaced = BTreeMap::<OrderKey, Vec<&Arc<NavEvent>>>::new();
    let mut result = im::Vector::new();
    for event in events {
        let key = order_key(event);
        if wanted.contains(&key) {
            replaced.entry(key).or_default().push(event);
        } else {
            result.push_back(event.clone());
        }
    }
    for (key, mut original) in replaced {
        original.sort_by(|a, b| a.record_key.cmp(&b.record_key));
        let template = original[0];
        let expected = original.iter().map(|e| e.amount_update).sum::<f64>();
        ensure!(
            expected.is_finite() && expected >= 0.0,
            "invalid lifecycle fill quantity"
        );
        ensure!(
            original.iter().all(|e| e.side_code == template.side_code
                && e.from_key_text == template.from_key_text
                && e.signal_ts_us == template.signal_ts_us),
            "conflicting lifecycle order attribution for {}",
            template.client_order_id
        );
        let unpriced = original.iter().any(|e| needs_price(e));
        let Some(order) = fills.get(&key) else {
            ensure!(
                !unpriced,
                "no executions for {} venue {} order {}",
                key.0,
                key.1,
                key.2
            );
            result.extend(original.into_iter().cloned());
            continue;
        };
        let actual = order.values().map(|f| f.quantity).sum::<f64>();
        let tolerance = expected.abs().max(actual.abs()) * 1e-12;
        // A priced lifecycle can arrive before its complete execution ledger.
        // Keep that factual lifecycle until the evidence catches up. Unpriced
        // deltas still require complete evidence before advancing the reader.
        if actual + tolerance < expected && !unpriced
            || (actual - expected).abs() <= tolerance && !unpriced
        {
            result.extend(original.into_iter().cloned());
            continue;
        }
        ensure!(
            actual.is_finite() && actual + tolerance >= expected,
            "execution quantity mismatch for {} venue {} order {}: lifecycle={} evidence={}",
            key.0,
            key.1,
            key.2,
            expected,
            actual
        );
        let order_id = &order
            .values()
            .next()
            .context("empty execution order")?
            .order_id;
        for fill in order.values() {
            ensure!(
                fill.side_code == template.side_code
                    && fill.order_id == *order_id
                    && (template.create_ts_us <= 0
                        || fill.timestamp_us >= template.create_ts_us / 1_000 * 1_000),
                "execution order side/identity conflict for {}",
                key.2
            );
            let mut event = (**template).clone();
            event.record_key = format!(
                "{:020}:execution:{}",
                template.event_ts_us, fill.transaction_id
            );
            event.update_ts_us = fill.timestamp_us;
            event.price = fill.price;
            event.amount_update = fill.quantity;
            result.push_back(Arc::new(event));
        }
        result.extend(
            original
                .into_iter()
                .filter(|e| e.amount_update == 0.0)
                .cloned(),
        );
    }
    result.sort_by(|a, b| a.record_key.cmp(&b.record_key));
    Ok(result)
}
