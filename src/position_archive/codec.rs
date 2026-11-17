//! One binary archive contract. Legacy JSON parsing lives only in the explicit
//! importer and explicitly marked online conversion; normal PB readers never
//! guess a stored format.
use super::*;
use anyhow::ensure;
use prost::Message;

/// Target-only reading keeps large factual JSON payloads out of theory scans.
/// Transitional decoding must be authorized by a validated conversion marker.
pub fn decode_target(
    value: &[u8],
    legacy_json: bool,
    converting_json: bool,
) -> Result<PositionUpdateMsg> {
    #[derive(Deserialize)]
    struct JsonTarget {
        msg_type: String,
        schema_version: u32,
        received_at_us: i64,
        seq: u32,
        strategy: PositionStrategy,
        #[serde(default)]
        published_accounts: Vec<ArchivedPublishedAccount>,
    }
    let message = if legacy_json || (converting_json && super::online::is_json(value)) {
        let m: JsonTarget = serde_json::from_slice(value)?;
        PositionUpdateMsg {
            msg_type: m.msg_type,
            schema_version: m.schema_version,
            received_at_us: m.received_at_us,
            seq: m.seq,
            strategy: m.strategy,
            published_accounts: m.published_accounts,
            factual_positions: vec![],
        }
    } else {
        decode(value)?
    };
    ensure!(
        message.msg_type == MSG_TYPE && message.factual_positions.is_empty(),
        "invalid archived target"
    );
    message.strategy.validate().map_err(anyhow::Error::msg)?;
    Ok(message)
}

pub(super) mod pb {
    include!(concat!(env!("OUT_DIR"), "/cta.archive.rs"));
}

#[derive(Default)]
struct Strings {
    ids: BTreeMap<String, u32>,
    values: Vec<String>,
}
impl Strings {
    fn id(&mut self, s: &str) -> u32 {
        *self.ids.entry(s.to_owned()).or_insert_with(|| {
            let id = self.values.len() as u32;
            self.values.push(s.to_owned());
            id
        })
    }
    fn targets(
        &mut self,
        targets: &BTreeMap<String, crate::order_config::TargetPosition>,
    ) -> Vec<pb::Target> {
        targets
            .iter()
            .map(|(s, t)| pb::Target {
                symbol: self.id(s),
                qty: Some(t.qty),
                signal: t.signal,
                clear: t.clear,
            })
            .collect()
    }
    fn pairs(&mut self, pairs: &BTreeMap<String, String>) -> Vec<pb::StringPair> {
        pairs
            .iter()
            .map(|(k, v)| pb::StringPair {
                key: self.id(k),
                value: self.id(v),
            })
            .collect()
    }
    fn rates(&mut self, rates: &BTreeMap<String, f64>) -> Vec<pb::Rate> {
        rates
            .iter()
            .map(|(k, v)| pb::Rate {
                quote: self.id(k),
                value: Some(*v),
            })
            .collect()
    }
}
fn string(strings: &[String], id: u32) -> Result<String> {
    strings
        .get(id as usize)
        .cloned()
        .context("archive string ID outside dictionary")
}
fn required(value: Option<f64>) -> Result<f64> {
    let v = value.context("missing required archive number")?;
    ensure!(v.is_finite(), "non-finite archive number");
    Ok(v)
}
fn optional(value: Option<f64>) -> Result<Option<f64>> {
    value.map(|v| required(Some(v))).transpose()
}
fn unique<K: Ord, V>(entries: impl IntoIterator<Item = Result<(K, V)>>) -> Result<BTreeMap<K, V>> {
    let mut out = BTreeMap::new();
    for entry in entries {
        let (k, v) = entry?;
        ensure!(
            out.insert(k, v).is_none(),
            "duplicate archive dictionary entry"
        );
    }
    Ok(out)
}
fn targets(
    strings: &[String],
    values: Vec<pb::Target>,
) -> Result<BTreeMap<String, crate::order_config::TargetPosition>> {
    unique(values.into_iter().map(|v| {
        let mut target = crate::order_config::TargetPosition::new(required(v.qty)?, v.signal)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            !v.clear || target.qty == 0.0,
            "clear target has nonzero quantity"
        );
        target.clear = v.clear;
        Ok((string(strings, v.symbol)?, target))
    }))
}
fn pairs(strings: &[String], values: Vec<pb::StringPair>) -> Result<BTreeMap<String, String>> {
    unique(
        values
            .into_iter()
            .map(|v| Ok((string(strings, v.key)?, string(strings, v.value)?))),
    )
}
fn rates(strings: &[String], values: Vec<pb::Rate>) -> Result<BTreeMap<String, f64>> {
    unique(
        values
            .into_iter()
            .map(|v| Ok((string(strings, v.quote)?, required(v.value)?))),
    )
}

pub(super) fn pack_values<'a>(
    positions: impl IntoIterator<Item = &'a ArchivedFactualPosition>,
) -> pb::PositionValues {
    let mut out = pb::PositionValues::default();
    for p in positions {
        let index = out.quantities.len();
        out.quantities.push(p.qty);
        if index % 8 == 0 {
            out.usdt_presence.push(0);
        }
        if let Some(v) = p.usdt {
            out.usdt_presence[index / 8] |= 1 << (index % 8);
            out.usdt.push(v);
        }
    }
    out
}
pub(super) fn unpack_values(values: pb::PositionValues) -> Result<Vec<(f64, Option<f64>)>> {
    let count = values.quantities.len();
    ensure!(
        values.usdt_presence.len() == count.div_ceil(8),
        "invalid position presence bitmap length"
    );
    if count % 8 != 0 {
        ensure!(
            values.usdt_presence.last().unwrap() >> (count % 8) == 0,
            "unused position bitmap bits set"
        );
    }
    let mut usdt = values.usdt.into_iter();
    let mut out = Vec::with_capacity(count);
    for (i, q) in values.quantities.into_iter().enumerate() {
        let quote = if values.usdt_presence[i / 8] & (1 << (i % 8)) != 0 {
            Some(required(usdt.next())?)
        } else {
            None
        };
        out.push((required(Some(q))?, quote));
    }
    ensure!(usdt.next().is_none(), "extra position quote values");
    Ok(out)
}

/// A self-contained binary message for exports, or just targets when factual is empty.
pub fn encode(message: &PositionUpdateMsg) -> Result<Vec<u8>> {
    encode_inner(message, true)
}
pub(super) fn encode_targets(message: &PositionUpdateMsg) -> Result<Vec<u8>> {
    encode_inner(message, false)
}
fn encode_inner(message: &PositionUpdateMsg, include_factual: bool) -> Result<Vec<u8>> {
    let mut dict = Strings::default();
    let msg_type = dict.id(&message.msg_type);
    let strategy = pb::Strategy {
        name: dict.id(&message.strategy.strategy_name),
        updated_at_us: message.strategy.updated_at_us,
        targets: dict.targets(&message.strategy.targets),
        overrides: dict.pairs(&message.strategy.symbol_order_strategy_overrides),
    };
    let accounts = message
        .published_accounts
        .iter()
        .map(|a| pb::Account {
            source_id: dict.id(&a.source_id),
            binding: dict.id(&a.binding_name),
            shares: Some(a.shares),
            fee: a.theoretical_fee_rate,
            quote_fees: dict.rates(&a.theoretical_fee_rates_by_quote),
            taker_fee: a.taker_fee_rate,
            quote_taker_fees: dict.rates(&a.taker_fee_rates_by_quote),
            legacy_leverage: a.legacy_leverage,
            projection: a.target_projection.as_ref().map(|p| pb::Projection {
                fetched_at_us: p.rules_fetched_at_us,
                targets: dict.targets(&p.targets),
                matched: p
                    .matched
                    .iter()
                    .map(|(k, v)| pb::Matched {
                        key: dict.id(k),
                        symbol: dict.id(&v.symbol),
                        source_multiplier: Some(v.source_contract_multiplier),
                        target_multiplier: Some(v.target_contract_multiplier),
                        quantity_multiplier: Some(v.quantity_multiplier),
                    })
                    .collect(),
                filtered: dict.pairs(&p.filtered),
            }),
        })
        .collect();
    let factual = message
        .factual_positions
        .iter()
        .filter(|_| include_factual)
        .map(|s| pb::InlineSnapshot {
            source_id: dict.id(&s.source_id),
            ts_ms: s.snapshot_ts_ms,
            ready: s.position_ready,
            symbols: s.positions.iter().map(|p| dict.id(&p.symbol)).collect(),
            values: Some(pack_values(&s.positions)),
        })
        .collect();
    Ok(pb::Record {
        strings: dict.values,
        msg_type,
        original_schema_metadata: message.schema_version,
        received_at_us: message.received_at_us,
        seq: message.seq,
        strategy: Some(strategy),
        accounts,
        factual,
    }
    .encode_to_vec())
}

pub fn decode(bytes: &[u8]) -> Result<PositionUpdateMsg> {
    let r = pb::Record::decode(bytes).context("decode Manager Protobuf signal")?;
    ensure!(r.received_at_us > 0, "invalid archive timestamp");
    let dict = &r.strings;
    let msg_type = string(dict, r.msg_type)?;
    ensure!(msg_type == MSG_TYPE, "invalid archive message type");
    let strategy = r.strategy.context("missing archive strategy")?;
    let strategy = PositionStrategy {
        strategy_name: string(dict, strategy.name)?,
        updated_at_us: strategy.updated_at_us,
        targets: targets(dict, strategy.targets)?,
        symbol_order_strategy_overrides: pairs(dict, strategy.overrides)?,
    };
    strategy.validate().map_err(anyhow::Error::msg)?;
    let accounts = r
        .accounts
        .into_iter()
        .map(|a| -> Result<_> {
            Ok(ArchivedPublishedAccount {
                source_id: string(dict, a.source_id)?,
                binding_name: string(dict, a.binding)?,
                shares: required(a.shares)?,
                theoretical_fee_rate: optional(a.fee)?,
                theoretical_fee_rates_by_quote: rates(dict, a.quote_fees)?,
                taker_fee_rate: optional(a.taker_fee)?,
                taker_fee_rates_by_quote: rates(dict, a.quote_taker_fees)?,
                legacy_leverage: optional(a.legacy_leverage)?,
                target_projection: a
                    .projection
                    .map(|p| -> Result<_> {
                        Ok(crate::target_proxy::TargetProjection {
                            rules_fetched_at_us: p.fetched_at_us,
                            targets: targets(dict, p.targets)?,
                            filtered: pairs(dict, p.filtered)?,
                            matched: unique(p.matched.into_iter().map(|m| {
                                Ok((
                                    string(dict, m.key)?,
                                    crate::target_proxy::MatchedContract {
                                        symbol: string(dict, m.symbol)?,
                                        source_contract_multiplier: required(m.source_multiplier)?,
                                        target_contract_multiplier: required(m.target_multiplier)?,
                                        quantity_multiplier: required(m.quantity_multiplier)?,
                                    },
                                ))
                            }))?,
                        })
                    })
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let factual = r
        .factual
        .into_iter()
        .map(|s| -> Result<_> {
            let values = unpack_values(s.values.context("missing position values")?)?;
            ensure!(
                values.len() == s.symbols.len(),
                "position symbol/value count mismatch"
            );
            Ok(ArchivedSourcePositions {
                source_id: string(dict, s.source_id)?,
                snapshot_ts_ms: s.ts_ms,
                position_ready: s.ready,
                positions: s
                    .symbols
                    .into_iter()
                    .zip(values)
                    .map(|(id, (qty, usdt))| {
                        Ok(ArchivedFactualPosition {
                            symbol: string(dict, id)?,
                            qty,
                            usdt,
                        })
                    })
                    .collect::<Result<_>>()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PositionUpdateMsg {
        msg_type,
        schema_version: r.original_schema_metadata,
        received_at_us: r.received_at_us,
        seq: r.seq,
        strategy,
        published_accounts: accounts,
        factual_positions: factual,
    })
}
