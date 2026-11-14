//! Recover unpriced and missing lifecycle fills from durable, uniquely identified
//! RapidX executions. The original Exec records remain read-only audit facts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::NavEvent;
use crate::config::SourceConfig;
use crate::model::FillLiquidity;
use crate::rocks_source::{self, RawRocksRecord};

pub(super) mod liquidity_journal;
pub(super) mod store;

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

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Fill {
    transaction_id: String,
    order_id: String,
    side_code: i16,
    quantity: f64,
    price: f64,
    timestamp_us: i64,
    liquidity: Option<FillLiquidity>,
}

#[cfg(test)]
fn decode_fills(
    records: &[RawRocksRecord],
    portfolio: &str,
    exchange: &str,
    wanted: &BTreeSet<OrderKey>,
) -> Result<BTreeMap<OrderKey, BTreeMap<String, Fill>>> {
    decode_fills_with_roles(records, portfolio, exchange, wanted, &BTreeMap::new())
}

#[cfg(test)]
pub(super) fn decode_fills_with_roles(
    records: &[RawRocksRecord],
    portfolio: &str,
    exchange: &str,
    wanted: &BTreeSet<OrderKey>,
    roles: &BTreeMap<String, liquidity_journal::RoleEvidence>,
) -> Result<BTreeMap<OrderKey, BTreeMap<String, Fill>>> {
    let mut result = BTreeMap::<OrderKey, BTreeMap<String, Fill>>::new();
    let clients = wanted.iter().map(|key| key.2).collect::<BTreeSet<_>>();
    for record in records {
        let Some((key, execution, mut fill)) = decode_record(
            record,
            portfolio,
            exchange,
            |client| Ok(clients.contains(&client)),
            |key| Ok(wanted.contains(key)),
        )?
        else {
            continue;
        };
        fill.liquidity = roles
            .get(&execution.transaction_id)
            .map(|role| role.validate(&execution))
            .transpose()?;
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

fn decode_record(
    record: &RawRocksRecord,
    portfolio: &str,
    exchange: &str,
    wanted_client: impl Fn(i64) -> Result<bool>,
    wanted_order: impl Fn(&OrderKey) -> Result<bool>,
) -> Result<Option<(OrderKey, Execution, Fill)>> {
    let stored: StoredExecution =
        serde_json::from_slice(&record.value).context("decode stored execution JSON")?;
    if stored.portfolio != portfolio || stored.exchange != exchange {
        return Ok(None);
    }
    let e = stored.execution;
    let Ok(client_order_id) = e.client_order_id.parse::<i64>() else {
        return Ok(None);
    };
    if !wanted_client(client_order_id)? {
        return Ok(None);
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
    if !wanted_order(&key)? {
        return Ok(None);
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
        transaction_id: e.transaction_id.clone(),
        order_id: e.order_id.clone(),
        side_code,
        quantity,
        price,
        timestamp_us,
        liquidity: None,
    };
    Ok(Some((key, e, fill)))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::nav) fn event(price: f64, quantity: f64, receive_us: i64) -> Arc<NavEvent> {
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
            report_liquidity: None,
        })
    }

    pub(in crate::nav) fn record(
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

    #[test]
    fn factual_roles_enrich_priced_fills_without_changing_fifo_inputs() {
        let events = im::vector![event(100.0, 3.0, 200_000)];
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let mut fills = decode_fills(
            &[record("123", "MARGIN", "a", 3.0, 100.0, false)],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        let order = fills.get_mut(&("FIL".into(), 0, 7)).unwrap();
        order.get_mut("a").unwrap().liquidity = Some(FillLiquidity::Maker);
        let result = replace_orders(&events, &wanted, &fills).unwrap();
        let mut expected = (*events[0]).clone();
        expected.fill_liquidity = Some(FillLiquidity::Maker);
        assert_eq!(result, im::vector![Arc::new(expected)]);
        assert_eq!(replace_orders(&result, &wanted, &fills).unwrap(), result);
        // A partial observation is not proof that the unobserved remainder was Maker.
        fills
            .get_mut(&("FIL".into(), 0, 7))
            .unwrap()
            .get_mut("a")
            .unwrap()
            .quantity = 2.0;
        assert_eq!(replace_orders(&events, &wanted, &fills).unwrap(), events);
    }

    #[test]
    fn mixed_roles_require_fill_level_matches_and_never_change_notional() {
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let mut fills = decode_fills(
            &[
                record("123", "MARGIN", "a", 1.0, 100.0, false),
                record("123", "MARGIN", "b", 2.0, 100.0, false),
            ],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        let order = fills.get_mut(&("FIL".into(), 0, 7)).unwrap();
        order.get_mut("a").unwrap().liquidity = Some(FillLiquidity::Maker);
        order.get_mut("b").unwrap().liquidity = Some(FillLiquidity::Taker);
        let events = im::vector![event(100.0, 1.0, 100_000), event(100.0, 2.0, 110_000)];
        let result = replace_orders(&events, &wanted, &fills).unwrap();
        assert_eq!(result[0].fill_liquidity, Some(FillLiquidity::Maker));
        assert_eq!(result[1].fill_liquidity, Some(FillLiquidity::Taker));
        // One aggregate lifecycle delta can be split only when both factual
        // executions share its price and timestamp.
        fills
            .get_mut(&("FIL".into(), 0, 7))
            .unwrap()
            .get_mut("b")
            .unwrap()
            .timestamp_us = 100_000;
        let combined = im::vector![event(100.0, 3.0, 100_000)];
        let result = replace_orders(&combined, &wanted, &fills).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(
            result
                .iter()
                .map(|e| e.amount_update * e.price)
                .sum::<f64>(),
            300.0
        );
        assert_eq!(result[0].fill_liquidity, Some(FillLiquidity::Maker));
        assert_eq!(result[1].fill_liquidity, Some(FillLiquidity::Taker));
        assert!(result.iter().all(|e| e.update_ts_us == 100_000));
        let ambiguous = im::vector![event(100.0, 3.0, 200_000)];
        assert_eq!(
            replace_orders(&ambiguous, &wanted, &fills).unwrap(),
            ambiguous
        );
    }

    #[test]
    fn recovered_fill_does_not_inherit_another_fills_role() {
        let mut old = (*event(0.0, 3.0, 200_000)).clone();
        old.fill_liquidity = Some(FillLiquidity::Maker);
        let wanted = BTreeSet::from([("FIL".into(), 0, 7)]);
        let mut fills = decode_fills(
            &[
                record("123", "MARGIN", "a", 1.0, 100.0, false),
                record("123", "MARGIN", "b", 2.0, 103.0, false),
            ],
            "123",
            "BINANCE",
            &wanted,
        )
        .unwrap();
        fills
            .get_mut(&("FIL".into(), 0, 7))
            .unwrap()
            .get_mut("b")
            .unwrap()
            .liquidity = Some(FillLiquidity::Taker);
        let result = replace_orders(&im::vector![Arc::new(old)], &wanted, &fills).unwrap();
        assert_eq!(result[0].fill_liquidity, Some(FillLiquidity::Unknown));
        assert_eq!(result[1].fill_liquidity, Some(FillLiquidity::Taker));
    }
}

#[cfg(test)]
pub(super) fn replace_orders(
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
        if actual + tolerance < expected && !unpriced {
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
        }
        if (actual - expected).abs() <= tolerance && !unpriced {
            // Enrich existing priced lifecycle rows without changing factual
            // price, quantity, timestamp, attribution or FIFO ordering.
            result.extend(apply_roles(&original, order)?);
            continue;
        }
        for fill in order.values() {
            let mut event = (**template).clone();
            event.record_key = format!(
                "{:020}:execution:{}",
                template.event_ts_us, fill.transaction_id
            );
            event.update_ts_us = fill.timestamp_us;
            event.price = fill.price;
            event.amount_update = fill.quantity;
            // Never inherit an earlier lifecycle delta's role on a different
            // recovered execution of the same order.
            event.fill_liquidity = Some(fill.liquidity.unwrap_or(FillLiquidity::Unknown));
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

fn with_role(event: &NavEvent, role: Option<FillLiquidity>) -> Result<NavEvent> {
    let mut result = event.clone();
    if let Some(role) = role {
        ensure!(
            event
                .fill_liquidity
                .is_none_or(|old| old == FillLiquidity::Unknown || old == role),
            "RapidX lifecycle and execution fill roles conflict for order {}",
            event.client_order_id
        );
        result.fill_liquidity = Some(role);
    }
    Ok(result)
}

#[cfg(test)]
fn apply_roles(
    original: &[&Arc<NavEvent>],
    order: &BTreeMap<String, Fill>,
) -> Result<Vec<Arc<NavEvent>>> {
    let first_role = order.values().next().and_then(|f| f.liquidity);
    if first_role.is_some() && order.values().all(|f| f.liquidity == first_role) {
        return original
            .iter()
            .map(|e| {
                if e.amount_update > 0.0 {
                    Ok(Arc::new(with_role(e, first_role)?))
                } else {
                    Ok((*e).clone())
                }
            })
            .collect();
    }
    // Mixed-role orders need fill-level evidence. Match the same millisecond
    // and price, consuming each execution quantity at most once. An aggregate
    // lifecycle row with ambiguous prices/times remains explicitly unknown.
    let mut remaining = order.values().map(|f| (f, f.quantity)).collect::<Vec<_>>();
    let mut result = Vec::new();
    for event in original {
        if event.amount_update <= 0.0 {
            result.push((*event).clone());
            continue;
        }
        let tolerance = event.amount_update.abs() * 1e-12;
        let mut needed = event.amount_update;
        let mut parts = Vec::new();
        for (index, (fill, quantity)) in remaining.iter().enumerate() {
            if *quantity <= 0.0
                || fill.timestamp_us / 1_000 != event.update_ts_us / 1_000
                || (fill.price - event.price).abs() > event.price.abs() * 1e-12
            {
                continue;
            }
            let quantity = needed.min(*quantity);
            parts.push((index, quantity, fill.liquidity));
            needed -= quantity;
            if needed <= tolerance {
                break;
            }
        }
        if needed > tolerance {
            result.push((*event).clone());
            continue;
        }
        for (index, quantity, _) in &parts {
            remaining[*index].1 -= quantity;
        }
        // Coalesce identical roles so ordinary lifecycle fill counts stay
        // unchanged. Only an actually mixed Maker/Taker delta needs splitting.
        let role = parts.first().and_then(|p| p.2);
        if parts.iter().all(|p| p.2 == role) {
            result.push(Arc::new(with_role(event, role)?));
        } else {
            let mut used = 0.0;
            for (i, (_, quantity, role)) in parts.iter().enumerate() {
                let mut part = with_role(event, *role)?;
                part.amount_update = if i + 1 == parts.len() {
                    event.amount_update - used
                } else {
                    *quantity
                };
                used += part.amount_update;
                part.record_key = format!("{}:liquidity:{i:08}", event.record_key);
                result.push(Arc::new(part));
            }
        }
    }
    Ok(result)
}
