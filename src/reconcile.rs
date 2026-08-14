use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{AppConfig, SourceConfig};
use crate::model::{
    ORDER_UPDATES_CF, ORDER_UPDATES_UNMATCHED_CF, TRADE_UPDATES_CF, TRADE_UPDATES_UNMATCHED_CF,
    UNIFORM_ORDERS_CF, decode_order_update, decode_trade_update, decode_uniform_order,
};
use crate::rocks_source;

const RAW_COLUMN_FAMILIES: [&str; 4] = [
    TRADE_UPDATES_CF,
    TRADE_UPDATES_UNMATCHED_CF,
    ORDER_UPDATES_CF,
    ORDER_UPDATES_UNMATCHED_CF,
];

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QuantityComparison {
    pub buy_quantity: f64,
    pub sell_quantity: f64,
    pub net_quantity: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SymbolFillComparison {
    pub symbol: String,
    pub venue_code: i16,
    pub uniform_fill_event_count: u64,
    pub raw_positive_order_count: u64,
    pub raw_observation_count: u64,
    pub raw_orders_without_positive_uniform_fill: u64,
    pub uniform: QuantityComparison,
    pub raw_max_cumulative: QuantityComparison,
    pub raw_minus_uniform: QuantityComparison,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceFillReconciliation {
    pub source_id: String,
    pub account: String,
    pub configured_venue: String,
    pub rocksdb_path: String,
    pub column_family_record_counts: BTreeMap<String, u64>,
    pub uniform_event_count: u64,
    pub uniform_positive_fill_event_count: u64,
    pub raw_unique_order_count: u64,
    pub raw_positive_order_count: u64,
    pub first_uniform_fill_ts_us: Option<i64>,
    pub last_uniform_fill_ts_us: Option<i64>,
    pub first_raw_observation_ts_us: Option<i64>,
    pub last_raw_observation_ts_us: Option<i64>,
    pub symbols: Vec<SymbolFillComparison>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FillReconciliationReport {
    pub identity: &'static str,
    pub raw_quantity_rule: &'static str,
    pub source_count: usize,
    pub sources: Vec<SourceFillReconciliation>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum StableOrderId {
    Exchange(i64),
    Client(i64),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RawOrderKey {
    symbol: String,
    venue_code: i16,
    id: StableOrderId,
}

#[derive(Clone, Debug)]
struct RawObservation {
    symbol: String,
    venue_code: i16,
    order_id: i64,
    client_order_id: i64,
    side_code: i16,
    cumulative_quantity: f64,
    event_ts_us: i64,
}

#[derive(Clone, Debug)]
struct RawOrder {
    symbol: String,
    venue_code: i16,
    side_code: i16,
    max_cumulative_quantity: f64,
    observation_count: u64,
    client_order_ids: BTreeSet<i64>,
}

#[derive(Clone, Debug, Default)]
struct QuantityBuilder {
    buy_quantity: f64,
    sell_quantity: f64,
}

impl QuantityBuilder {
    fn add(&mut self, side_code: i16, quantity: f64) -> Result<()> {
        match side_code {
            1 => self.buy_quantity += quantity,
            2 => self.sell_quantity += quantity,
            value => bail!("unsupported side code {value}"),
        }
        if !self.buy_quantity.is_finite() || !self.sell_quantity.is_finite() {
            bail!("cumulative reconciliation quantity overflowed");
        }
        Ok(())
    }

    fn finish(self) -> QuantityComparison {
        QuantityComparison {
            buy_quantity: clean_zero(self.buy_quantity),
            sell_quantity: clean_zero(self.sell_quantity),
            net_quantity: clean_zero(self.buy_quantity - self.sell_quantity),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SymbolBuilder {
    uniform_fill_event_count: u64,
    raw_positive_order_count: u64,
    raw_observation_count: u64,
    raw_orders_without_positive_uniform_fill: u64,
    uniform: QuantityBuilder,
    raw: QuantityBuilder,
}

pub fn reconcile_from_rocksdb(
    config: &AppConfig,
    selected_source_ids: &[String],
) -> Result<FillReconciliationReport> {
    let selected = select_sources(config, selected_source_ids)?;
    let mut sources = Vec::with_capacity(selected.len());
    for source in selected {
        sources.push(reconcile_source(source)?);
    }
    Ok(FillReconciliationReport {
        identity: "venue+symbol+exchange_order_id (client_order_id fallback)",
        raw_quantity_rule: "maximum cumulative filled quantity observed per exchange order",
        source_count: sources.len(),
        sources,
    })
}

fn reconcile_source(source: &SourceConfig) -> Result<SourceFillReconciliation> {
    let mut requested = vec![UNIFORM_ORDERS_CF];
    requested.extend_from_slice(&RAW_COLUMN_FAMILIES);
    let mut records = rocks_source::read_all_column_families(&source.rocksdb_path, &requested)
        .with_context(|| format!("failed to read source {} RocksDB", source.id))?;
    let column_family_record_counts = records
        .iter()
        .map(|(name, values)| {
            u64::try_from(values.len())
                .map(|count| (name.clone(), count))
                .context("column family record count exceeds u64")
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    let uniform_records = records
        .remove(UNIFORM_ORDERS_CF)
        .context("uniform_orders records disappeared after scan")?;
    let uniform_event_count =
        u64::try_from(uniform_records.len()).context("uniform event count exceeds u64")?;
    let mut symbol_builders = BTreeMap::<(String, i16), SymbolBuilder>::new();
    let mut positive_uniform_clients = BTreeSet::<(String, i16, i64)>::new();
    let mut uniform_positive_fill_event_count = 0_u64;
    let mut first_uniform_fill_ts_us: Option<i64> = None;
    let mut last_uniform_fill_ts_us: Option<i64> = None;
    for record in uniform_records {
        let event = decode_uniform_order(&record.key, &record.value).with_context(|| {
            format!(
                "source {} contains an undecodable uniform order at key {:?}",
                source.id,
                String::from_utf8_lossy(&record.key)
            )
        })?;
        validate_quantity(
            event.amount_update,
            "uniform amount_update",
            &event.record_key,
        )?;
        if event.amount_update == 0.0 {
            continue;
        }
        let builder = symbol_builders
            .entry((event.symbol.clone(), event.venue_code))
            .or_default();
        builder.uniform.add(event.side_code, event.amount_update)?;
        builder.uniform_fill_event_count = builder
            .uniform_fill_event_count
            .checked_add(1)
            .context("uniform fill event count overflowed")?;
        uniform_positive_fill_event_count = uniform_positive_fill_event_count
            .checked_add(1)
            .context("uniform positive fill count overflowed")?;
        positive_uniform_clients.insert((event.symbol, event.venue_code, event.client_order_id));
        let ts_us = if event.update_ts_us > 0 {
            event.update_ts_us
        } else {
            event.event_ts_us
        };
        update_bounds(
            &mut first_uniform_fill_ts_us,
            &mut last_uniform_fill_ts_us,
            ts_us,
        );
    }

    let mut raw_orders = BTreeMap::<RawOrderKey, RawOrder>::new();
    let mut first_raw_observation_ts_us: Option<i64> = None;
    let mut last_raw_observation_ts_us: Option<i64> = None;
    for column_family in RAW_COLUMN_FAMILIES {
        let raw_records = records
            .remove(column_family)
            .with_context(|| format!("{column_family} records disappeared after scan"))?;
        for record in raw_records {
            let observation = if column_family == TRADE_UPDATES_CF
                || column_family == TRADE_UPDATES_UNMATCHED_CF
            {
                let event = decode_trade_update(&record.key, &record.value).with_context(|| {
                    format!(
                        "source {} contains an undecodable {} record at key {:?}",
                        source.id,
                        column_family,
                        String::from_utf8_lossy(&record.key)
                    )
                })?;
                RawObservation {
                    symbol: event.symbol,
                    venue_code: event.venue_code,
                    order_id: event.order_id,
                    client_order_id: event.client_order_id,
                    side_code: event.side_code,
                    cumulative_quantity: event.cumulative_filled_quantity,
                    event_ts_us: if event.trade_ts_us > 0 {
                        event.trade_ts_us
                    } else if event.event_ts_us > 0 {
                        event.event_ts_us
                    } else {
                        event.record_ts_us
                    },
                }
            } else {
                let event = decode_order_update(&record.key, &record.value).with_context(|| {
                    format!(
                        "source {} contains an undecodable {} record at key {:?}",
                        source.id,
                        column_family,
                        String::from_utf8_lossy(&record.key)
                    )
                })?;
                RawObservation {
                    symbol: event.symbol,
                    venue_code: event.venue_code,
                    order_id: event.order_id,
                    client_order_id: event.client_order_id,
                    side_code: event.side_code,
                    cumulative_quantity: event.cumulative_filled_quantity,
                    event_ts_us: if event.event_ts_us > 0 {
                        event.event_ts_us
                    } else {
                        event.record_ts_us
                    },
                }
            };
            update_bounds(
                &mut first_raw_observation_ts_us,
                &mut last_raw_observation_ts_us,
                observation.event_ts_us,
            );
            apply_raw_observation(&mut raw_orders, observation).with_context(|| {
                format!(
                    "invalid raw observation in source {} {column_family}",
                    source.id
                )
            })?;
        }
    }
    let mut raw_positive_order_count = 0_u64;
    for order in raw_orders.values() {
        if order.max_cumulative_quantity == 0.0 {
            continue;
        }
        raw_positive_order_count = raw_positive_order_count
            .checked_add(1)
            .context("raw positive order count overflowed")?;
        let builder = symbol_builders
            .entry((order.symbol.clone(), order.venue_code))
            .or_default();
        builder
            .raw
            .add(order.side_code, order.max_cumulative_quantity)?;
        builder.raw_positive_order_count = builder
            .raw_positive_order_count
            .checked_add(1)
            .context("raw symbol order count overflowed")?;
        builder.raw_observation_count = builder
            .raw_observation_count
            .checked_add(order.observation_count)
            .context("raw observation count overflowed")?;
        let represented = order.client_order_ids.iter().any(|client_order_id| {
            positive_uniform_clients.contains(&(
                order.symbol.clone(),
                order.venue_code,
                *client_order_id,
            ))
        });
        if !represented {
            builder.raw_orders_without_positive_uniform_fill = builder
                .raw_orders_without_positive_uniform_fill
                .checked_add(1)
                .context("raw missing-uniform order count overflowed")?;
        }
    }

    let symbols = symbol_builders
        .into_iter()
        .map(|((symbol, venue_code), builder)| {
            let uniform = builder.uniform.finish();
            let raw = builder.raw.finish();
            SymbolFillComparison {
                symbol,
                venue_code,
                uniform_fill_event_count: builder.uniform_fill_event_count,
                raw_positive_order_count: builder.raw_positive_order_count,
                raw_observation_count: builder.raw_observation_count,
                raw_orders_without_positive_uniform_fill: builder
                    .raw_orders_without_positive_uniform_fill,
                raw_minus_uniform: QuantityComparison {
                    buy_quantity: clean_zero(raw.buy_quantity - uniform.buy_quantity),
                    sell_quantity: clean_zero(raw.sell_quantity - uniform.sell_quantity),
                    net_quantity: clean_zero(raw.net_quantity - uniform.net_quantity),
                },
                uniform,
                raw_max_cumulative: raw,
            }
        })
        .collect();

    Ok(SourceFillReconciliation {
        source_id: source.id.clone(),
        account: source.account.clone(),
        configured_venue: source.venue.clone(),
        rocksdb_path: source.rocksdb_path.display().to_string(),
        column_family_record_counts,
        uniform_event_count,
        uniform_positive_fill_event_count,
        raw_unique_order_count: u64::try_from(raw_orders.len())
            .context("raw unique order count exceeds u64")?,
        raw_positive_order_count,
        first_uniform_fill_ts_us,
        last_uniform_fill_ts_us,
        first_raw_observation_ts_us,
        last_raw_observation_ts_us,
        symbols,
    })
}

fn apply_raw_observation(
    orders: &mut BTreeMap<RawOrderKey, RawOrder>,
    observation: RawObservation,
) -> Result<()> {
    validate_quantity(
        observation.cumulative_quantity,
        "raw cumulative filled quantity",
        &format!("{}:{}", observation.symbol, observation.order_id),
    )?;
    if observation.symbol.trim().is_empty() {
        bail!("raw update has an empty symbol");
    }
    if !matches!(observation.side_code, 1 | 2) {
        bail!(
            "raw update has unsupported side code {}",
            observation.side_code
        );
    }
    let stable_id = if observation.order_id > 0 {
        StableOrderId::Exchange(observation.order_id)
    } else if observation.client_order_id != 0 {
        StableOrderId::Client(observation.client_order_id)
    } else if observation.cumulative_quantity == 0.0 {
        return Ok(());
    } else {
        bail!("positive raw fill has neither exchange nor client order ID");
    };
    let key = RawOrderKey {
        symbol: observation.symbol.clone(),
        venue_code: observation.venue_code,
        id: stable_id,
    };
    let order = orders.entry(key).or_insert_with(|| RawOrder {
        symbol: observation.symbol,
        venue_code: observation.venue_code,
        side_code: observation.side_code,
        max_cumulative_quantity: 0.0,
        observation_count: 0,
        client_order_ids: BTreeSet::new(),
    });
    if order.side_code != observation.side_code {
        bail!(
            "one raw order changed side from {} to {}",
            order.side_code,
            observation.side_code
        );
    }
    order.max_cumulative_quantity = order
        .max_cumulative_quantity
        .max(observation.cumulative_quantity);
    order.observation_count = order
        .observation_count
        .checked_add(1)
        .context("raw observation count overflowed")?;
    if observation.client_order_id != 0 {
        order.client_order_ids.insert(observation.client_order_id);
    }
    Ok(())
}

fn validate_quantity(quantity: f64, field: &str, identity: &str) -> Result<()> {
    if quantity.is_finite() && quantity >= 0.0 {
        Ok(())
    } else {
        bail!("{field} for {identity} must be finite and nonnegative, got {quantity}")
    }
}

fn update_bounds(first: &mut Option<i64>, last: &mut Option<i64>, value: i64) {
    *first = Some(first.map_or(value, |current| current.min(value)));
    *last = Some(last.map_or(value, |current| current.max(value)));
}

fn select_sources<'a>(
    config: &'a AppConfig,
    selected_source_ids: &[String],
) -> Result<Vec<&'a SourceConfig>> {
    let requested = selected_source_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if requested.len() != selected_source_ids.len() {
        bail!("selected source IDs contain duplicates");
    }
    for source_id in &requested {
        let source = config
            .sources
            .iter()
            .find(|source| source.id == *source_id)
            .with_context(|| format!("selected source {} is not configured", source_id))?;
        if !source.enabled {
            bail!("selected source {} is disabled", source_id);
        }
    }
    let selected = config
        .sources
        .iter()
        .filter(|source| {
            source.enabled && (requested.is_empty() || requested.contains(source.id.as_str()))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no enabled sources selected for fill reconciliation");
    }
    Ok(selected)
}

fn clean_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        order_id: i64,
        client_order_id: i64,
        side_code: i16,
        cum: f64,
    ) -> RawObservation {
        RawObservation {
            symbol: "BTCUSDT".to_string(),
            venue_code: 1,
            order_id,
            client_order_id,
            side_code,
            cumulative_quantity: cum,
            event_ts_us: 1,
        }
    }

    #[test]
    fn repeated_updates_use_maximum_cumulative_quantity() {
        let mut orders = BTreeMap::new();
        apply_raw_observation(&mut orders, observation(10, 100, 1, 0.2)).unwrap();
        apply_raw_observation(&mut orders, observation(10, 100, 1, 0.5)).unwrap();
        apply_raw_observation(&mut orders, observation(10, 100, 1, 0.5)).unwrap();

        assert_eq!(orders.len(), 1);
        let order = orders.values().next().unwrap();
        assert_eq!(order.max_cumulative_quantity, 0.5);
        assert_eq!(order.observation_count, 3);
    }

    #[test]
    fn different_exchange_orders_are_not_merged_by_client_id() {
        let mut orders = BTreeMap::new();
        apply_raw_observation(&mut orders, observation(10, 100, 1, 0.2)).unwrap();
        apply_raw_observation(&mut orders, observation(11, 100, 1, 0.3)).unwrap();
        assert_eq!(orders.len(), 2);
    }
}
