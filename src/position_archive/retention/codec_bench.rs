//! Isolated size experiment; these protobuf types are NOT a live archive codec.
use super::*;
use prost::Message;

#[derive(Clone, PartialEq, Message)]
struct Number {
    #[prost(double, optional, tag = "1")]
    value: Option<f64>,
}
#[derive(Clone, PartialEq, Message)]
struct Target {
    #[prost(double, optional, tag = "1")]
    qty: Option<f64>,
    #[prost(sint32, tag = "2")]
    signal: i32,
    #[prost(bool, tag = "3")]
    clear: bool,
}
#[derive(Clone, PartialEq, Message)]
struct Strategy {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(btree_map = "string, message", tag = "2")]
    targets: BTreeMap<String, Target>,
    #[prost(btree_map = "string, string", tag = "3")]
    overrides: BTreeMap<String, String>,
    #[prost(sint64, tag = "4")]
    updated_at_us: i64,
}
#[derive(Clone, PartialEq, Message)]
struct Position {
    #[prost(string, tag = "1")]
    symbol: String,
    #[prost(double, optional, tag = "2")]
    qty: Option<f64>,
    #[prost(double, optional, tag = "3")]
    usdt: Option<f64>,
}
#[derive(Clone, PartialEq, Message)]
struct Snapshot {
    #[prost(string, tag = "1")]
    source_id: String,
    #[prost(sint64, tag = "2")]
    ts_ms: i64,
    #[prost(bool, tag = "3")]
    ready: bool,
    #[prost(message, repeated, tag = "4")]
    positions: Vec<Position>,
}
#[derive(Clone, PartialEq, Message)]
struct Matched {
    #[prost(string, tag = "1")]
    symbol: String,
    #[prost(double, optional, tag = "2")]
    source_multiplier: Option<f64>,
    #[prost(double, optional, tag = "3")]
    target_multiplier: Option<f64>,
    #[prost(double, optional, tag = "4")]
    quantity_multiplier: Option<f64>,
}
#[derive(Clone, PartialEq, Message)]
struct Projection {
    #[prost(sint64, tag = "1")]
    fetched_at_us: i64,
    #[prost(btree_map = "string, message", tag = "2")]
    targets: BTreeMap<String, Target>,
    #[prost(btree_map = "string, message", tag = "3")]
    matched: BTreeMap<String, Matched>,
    #[prost(btree_map = "string, string", tag = "4")]
    filtered: BTreeMap<String, String>,
}
#[derive(Clone, PartialEq, Message)]
struct Account {
    #[prost(string, tag = "1")]
    source_id: String,
    #[prost(string, tag = "2")]
    binding: String,
    #[prost(double, optional, tag = "3")]
    shares: Option<f64>,
    #[prost(message, optional, tag = "4")]
    projection: Option<Projection>,
    #[prost(double, optional, tag = "5")]
    fee: Option<f64>,
    #[prost(btree_map = "string, message", tag = "6")]
    quote_fees: BTreeMap<String, Number>,
    #[prost(double, optional, tag = "7")]
    taker_fee: Option<f64>,
    #[prost(btree_map = "string, message", tag = "8")]
    quote_taker_fees: BTreeMap<String, Number>,
    #[prost(double, optional, tag = "9")]
    legacy_leverage: Option<f64>,
}
#[derive(Clone, PartialEq, Message)]
struct Record {
    #[prost(string, tag = "1")]
    msg_type: String,
    // Preserve the input's existing JSON metadata; no new serving format version.
    #[prost(uint32, tag = "2")]
    original_schema_metadata: u32,
    #[prost(sint64, tag = "3")]
    received_at_us: i64,
    #[prost(uint32, tag = "4")]
    seq: u32,
    #[prost(message, optional, tag = "5")]
    strategy: Option<Strategy>,
    #[prost(message, repeated, tag = "6")]
    factual: Vec<Snapshot>,
    #[prost(message, repeated, tag = "7")]
    accounts: Vec<Account>,
}
fn pack_targets(
    v: &BTreeMap<String, crate::order_config::TargetPosition>,
) -> BTreeMap<String, Target> {
    v.iter()
        .map(|(k, t)| {
            (
                k.clone(),
                Target {
                    qty: Some(t.qty),
                    signal: t.signal,
                    clear: t.clear,
                },
            )
        })
        .collect()
}
fn unpack_targets(
    v: BTreeMap<String, Target>,
) -> BTreeMap<String, crate::order_config::TargetPosition> {
    v.into_iter()
        .map(|(k, t)| {
            (
                k,
                crate::order_config::TargetPosition {
                    qty: t.qty.unwrap_or(0.),
                    signal: t.signal,
                    clear: t.clear,
                },
            )
        })
        .collect()
}
fn pack_rates(v: &BTreeMap<String, f64>) -> BTreeMap<String, Number> {
    v.iter()
        .map(|(k, v)| (k.clone(), Number { value: Some(*v) }))
        .collect()
}
fn unpack_rates(v: BTreeMap<String, Number>) -> BTreeMap<String, f64> {
    v.into_iter()
        .map(|(k, v)| (k, v.value.unwrap_or(0.)))
        .collect()
}
impl From<&PositionUpdateMsg> for Record {
    fn from(m: &PositionUpdateMsg) -> Self {
        Self {
            msg_type: m.msg_type.clone(),
            original_schema_metadata: m.schema_version,
            received_at_us: m.received_at_us,
            seq: m.seq,
            strategy: Some(Strategy {
                name: m.strategy.strategy_name.clone(),
                targets: pack_targets(&m.strategy.targets),
                overrides: m.strategy.symbol_order_strategy_overrides.clone(),
                updated_at_us: m.strategy.updated_at_us,
            }),
            factual: m
                .factual_positions
                .iter()
                .map(|s| Snapshot {
                    source_id: s.source_id.clone(),
                    ts_ms: s.snapshot_ts_ms,
                    ready: s.position_ready,
                    positions: s
                        .positions
                        .iter()
                        .map(|p| Position {
                            symbol: p.symbol.clone(),
                            qty: Some(p.qty),
                            usdt: p.usdt,
                        })
                        .collect(),
                })
                .collect(),
            accounts: m
                .published_accounts
                .iter()
                .map(|a| Account {
                    source_id: a.source_id.clone(),
                    binding: a.binding_name.clone(),
                    shares: Some(a.shares),
                    fee: a.theoretical_fee_rate,
                    quote_fees: pack_rates(&a.theoretical_fee_rates_by_quote),
                    taker_fee: a.taker_fee_rate,
                    quote_taker_fees: pack_rates(&a.taker_fee_rates_by_quote),
                    legacy_leverage: a.legacy_leverage,
                    projection: a.target_projection.as_ref().map(|p| Projection {
                        fetched_at_us: p.rules_fetched_at_us,
                        targets: pack_targets(&p.targets),
                        filtered: p.filtered.clone(),
                        matched: p
                            .matched
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    Matched {
                                        symbol: v.symbol.clone(),
                                        source_multiplier: Some(v.source_contract_multiplier),
                                        target_multiplier: Some(v.target_contract_multiplier),
                                        quantity_multiplier: Some(v.quantity_multiplier),
                                    },
                                )
                            })
                            .collect(),
                    }),
                })
                .collect(),
        }
    }
}
impl TryFrom<Record> for PositionUpdateMsg {
    type Error = anyhow::Error;
    fn try_from(r: Record) -> Result<Self> {
        let s = r.strategy.context("protobuf strategy missing")?;
        Ok(Self {
            msg_type: r.msg_type,
            schema_version: r.original_schema_metadata,
            received_at_us: r.received_at_us,
            seq: r.seq,
            strategy: PositionStrategy {
                strategy_name: s.name,
                targets: unpack_targets(s.targets),
                symbol_order_strategy_overrides: s.overrides,
                updated_at_us: s.updated_at_us,
            },
            factual_positions: r
                .factual
                .into_iter()
                .map(|s| super::super::ArchivedSourcePositions {
                    source_id: s.source_id,
                    snapshot_ts_ms: s.ts_ms,
                    position_ready: s.ready,
                    positions: s
                        .positions
                        .into_iter()
                        .map(|p| super::super::ArchivedFactualPosition {
                            symbol: p.symbol,
                            qty: p.qty.unwrap_or(0.),
                            usdt: p.usdt,
                        })
                        .collect(),
                })
                .collect(),
            published_accounts: r
                .accounts
                .into_iter()
                .map(|a| super::super::ArchivedPublishedAccount {
                    source_id: a.source_id,
                    binding_name: a.binding,
                    shares: a.shares.unwrap_or(0.),
                    theoretical_fee_rate: a.fee,
                    theoretical_fee_rates_by_quote: unpack_rates(a.quote_fees),
                    taker_fee_rate: a.taker_fee,
                    taker_fee_rates_by_quote: unpack_rates(a.quote_taker_fees),
                    legacy_leverage: a.legacy_leverage,
                    target_projection: a.projection.map(|p| {
                        crate::target_proxy::TargetProjection {
                            rules_fetched_at_us: p.fetched_at_us,
                            targets: unpack_targets(p.targets),
                            filtered: p.filtered,
                            matched: p
                                .matched
                                .into_iter()
                                .map(|(k, v)| {
                                    (
                                        k,
                                        crate::target_proxy::MatchedContract {
                                            symbol: v.symbol,
                                            source_contract_multiplier: v
                                                .source_multiplier
                                                .unwrap_or(0.),
                                            target_contract_multiplier: v
                                                .target_multiplier
                                                .unwrap_or(0.),
                                            quantity_multiplier: v
                                                .quantity_multiplier
                                                .unwrap_or(0.),
                                        },
                                    )
                                })
                                .collect(),
                        }
                    }),
                })
                .collect(),
        })
    }
}

/// Compare actual LZ4 SST files with identical keys and DB options. No live DB.
pub fn benchmark(file: &Path, root: &Path) -> Result<serde_json::Value> {
    ensure!(
        root.is_absolute() && !root.exists(),
        "benchmark output must be a new absolute directory"
    );
    std::fs::create_dir(root)?;
    let original = ManagerDb::open(&root.join("original-json"))?;
    let canonical = ManagerDb::open(&root.join("canonical-json"))?;
    let protobuf = ManagerDb::open(&root.join("protobuf"))?;
    let dbs = [&original, &canonical, &protobuf];
    let mut batches: [rocksdb::WriteBatch; 3] = Default::default();
    let mut bytes = [0u64; 3];
    let mut count = 0u64;
    let mut reader = GzDecoder::new(std::io::BufReader::new(std::fs::File::open(file)?));
    let mut magic = [0; 16];
    reader.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "not a signal archive");
    let header = read_blob(&mut reader, MAX_HEADER)?;
    let mut hash = Sha256::new();
    hash.update(&header);
    loop {
        let mut key = [0; 12];
        reader.read_exact(&mut key)?;
        let value = read_blob(&mut reader, MAX_VALUE as usize)?;
        if key == [0; 12] {
            ensure!(value.is_empty(), "invalid terminator");
            break;
        }
        let msg = validate_message(&key, &value)?;
        let normalized = serde_json::to_vec(&msg)?;
        let encoded = Record::from(&msg).encode_to_vec();
        let decoded = PositionUpdateMsg::try_from(Record::decode(encoded.as_slice())?)?;
        ensure!(
            rmp_serde::to_vec(&msg)? == rmp_serde::to_vec(&decoded)?,
            "protobuf roundtrip changed typed values"
        );
        for (i, data) in [&value, &normalized, &encoded].into_iter().enumerate() {
            let cf = dbs[i].db().cf_handle(POSITION_UPDATES_CF).unwrap();
            batches[i].put_cf(&cf, key, data);
            bytes[i] += data.len() as u64;
        }
        hash_frame(&mut hash, &key, &value);
        count += 1;
    }
    let receipt: Receipt = serde_json::from_slice(&read_blob(&mut reader, 65536)?)?;
    ensure!(
        receipt.sha256 == hex::encode(hash.finalize()) && receipt.count == count,
        "sample checksum mismatch"
    );
    let mut extra = [0; 1];
    ensure!(
        reader.read(&mut extra)? == 0 && reader.into_inner().read(&mut extra)? == 0,
        "trailing archive data"
    );
    let mut sst = [0u64; 3];
    for (i, batch) in batches.into_iter().enumerate() {
        dbs[i].db().write(batch)?;
        let cf = dbs[i].db().cf_handle(POSITION_UPDATES_CF).unwrap();
        dbs[i].db().flush_cf(&cf)?;
        dbs[i]
            .db()
            .compact_range_cf(&cf, None::<&[u8]>, None::<&[u8]>);
        sst[i] = dbs[i]
            .db()
            .property_int_value_cf(&cf, "rocksdb.live-sst-files-size")?
            .context("missing SST size")?;
    }
    Ok(
        serde_json::json!({"sample_only":true,"records":count,"typed_roundtrip_equal":true,
        "raw_value_bytes":{"original_json":bytes[0],"canonical_json":bytes[1],"protobuf":bytes[2]},
        "lz4_signal_sst_bytes":{"original_json":sst[0],"canonical_json":sst[1],"protobuf":sst[2]},
        "protobuf_sst_reduction_vs_original":1.-sst[2] as f64/sst[0] as f64,
        "protobuf_sst_reduction_vs_canonical":1.-sst[2] as f64/sst[1] as f64}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protobuf_experiment_preserves_double_bits_clear_optional_fees_and_projection() {
        let msg:PositionUpdateMsg=serde_json::from_value(serde_json::json!({
            "msg_type":"position_update","schema_version":4,"received_at_us":1794750000000000_i64,"seq":4294967295_u32,
            "strategy":{"strategy_name":"test","updated_at_us":1794750000000000_i64,
                "targets":{"BTCUSDT":{"qty":-0.0,"signal":-1},"ETHUSDT":{"qty":"clear","signal":0}},
                "symbol_order_strategy_overrides":{"BTCUSDT":"other"}},
            "factual_positions":[{"source_id":"source","snapshot_ts_ms":1794750000000_i64,"position_ready":false,
                "positions":[{"symbol":"BTCUSDT","qty":-0.0,"usdt":-0.0},{"symbol":"ETHUSDT","qty":1e-17}]}],
            "published_accounts":[{"source_id":"source","binding_name":"binding","shares":-0.0,"leverage":0.125,
                "theoretical_fee_rate":-0.0001,"taker_fee_rate":0.0,
                "theoretical_fee_rates_by_quote":{"USDT":-0.0,"USDC":0.0002},
                "taker_fee_rates_by_quote":{"USDT":0.0,"USDC":0.0004},
                "target_projection":{"rules_fetched_at_us":1794750000000000_i64,
                    "targets":{"BTCUSDT":{"qty":1e-17,"signal":1}},
                    "matched":{"BTCUSDT":{"symbol":"BTCUSDT","source_contract_multiplier":1.0,
                        "target_contract_multiplier":0.001,"quantity_multiplier":1000.0}},
                    "filtered":{"UNKNOWNUSDT":"inactive"}}}]
        })).unwrap();
        let encoded = Record::from(&msg).encode_to_vec();
        let decoded =
            PositionUpdateMsg::try_from(Record::decode(encoded.as_slice()).unwrap()).unwrap();
        assert_eq!(
            rmp_serde::to_vec(&msg).unwrap(),
            rmp_serde::to_vec(&decoded).unwrap()
        );
        assert_eq!(
            decoded.factual_positions[0].positions[0].qty.to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(
            decoded.published_accounts[0].theoretical_fee_rates_by_quote["USDT"].to_bits(),
            (-0.0_f64).to_bits()
        );
    }
}
