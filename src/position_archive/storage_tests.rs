use super::*;
use prost::Message;

pub(super) fn message(ts: i64, quantity: f64) -> PositionUpdateMsg {
    let value = serde_json::json!({
        "msg_type":"position_update","schema_version":4,"received_at_us":ts,"seq":0,
        "strategy":{"strategy_name":"alpha","updated_at_us":ts,
            "targets":{"BTCUSDT":{"qty":quantity,"signal":-1},"ETHUSDT":{"qty":"clear","signal":0}},
            "symbol_order_strategy_overrides":{"BTCUSDT":"taker"}},
        "factual_positions":[{"source_id":"account-a","snapshot_ts_ms":ts/1000+37,"position_ready":true,
            "positions":[{"symbol":"BTCUSDT","qty":-0.0,"usdt":-0.0},{"symbol":"ETHUSDT","qty":1e-17}]}],
        "published_accounts":[{"source_id":"account-a","binding_name":"alpha-account","shares":0.5,"leverage":0.125,
            "theoretical_fee_rate":-0.0001,"taker_fee_rate":0.0,
            "theoretical_fee_rates_by_quote":{"USDT":-0.0,"USDC":0.0002},
            "taker_fee_rates_by_quote":{"USDT":0.0,"USDC":0.0004},
            "target_projection":{"rules_fetched_at_us":ts-1,
                "targets":{"BTCUSDT":{"qty":quantity,"signal":-1},"ETHUSDT":{"qty":"clear","signal":0}},
                "matched":{"BTCUSDT":{"symbol":"BTCUSDT","source_contract_multiplier":1.0,
                    "target_contract_multiplier":0.001,"quantity_multiplier":1000.0}},
                "filtered":{"UNKNOWNUSDT":"inactive"}}}]
    });
    serde_json::from_value(value).unwrap()
}
fn store(db: &ManagerDb, storage: &storage::Storage, message: &PositionUpdateMsg) {
    let mut batch = rocksdb::WriteBatch::default();
    let pending = storage.stage(db, message, &mut batch, true).unwrap();
    db.db().write(batch).unwrap();
    storage.published(pending);
}
fn exact(a: &impl Serialize, b: &impl Serialize) {
    assert_eq!(rmp_serde::to_vec(a).unwrap(), rmp_serde::to_vec(b).unwrap());
}
#[test]
fn snapshot_days_align_to_manual_shanghai_dates_and_do_not_overflow() {
    let cutoff = retention::date_boundary("2026-10-01").unwrap().0;
    assert!(storage::day_key(cutoff - 1) < storage::day_key(cutoff));
    assert_eq!(
        storage::day_key(cutoff),
        storage::day_key(cutoff + storage::DAY_US - 1)
    );
    assert!(storage::day_key(i64::MAX) > storage::day_key(cutoff));
    assert_eq!(storage::day_key(1), storage::day_key(0));
}
fn blob_count(db: &ManagerDb) -> usize {
    let cf = db.db().cf_handle(storage::SNAPSHOTS_CF).unwrap();
    db.db()
        .iterator_cf(&cf, IteratorMode::Start)
        .filter(|r| r.as_ref().unwrap().0[8] == 1)
        .count()
}

#[test]
fn compact_codec_preserves_double_bits_presence_clear_and_every_frozen_field() {
    let mut m = message(1_700_000_000_000_001, -0.0);
    m.seq = u32::MAX;
    let bytes = codec::encode(&m).unwrap();
    let decoded = codec::decode(&bytes).unwrap();
    exact(&m, &decoded);
    assert!(decoded.strategy.targets["ETHUSDT"].clear);
    assert_eq!(
        decoded.strategy.targets["BTCUSDT"].qty.to_bits(),
        (-0.0f64).to_bits()
    );
    assert_eq!(
        decoded.factual_positions[0].positions[0]
            .usdt
            .unwrap()
            .to_bits(),
        (-0.0f64).to_bits()
    );
    assert!(decoded.factual_positions[0].positions[1].usdt.is_none());
    let mut corrupt = codec::pb::Record::decode(bytes.as_slice()).unwrap();
    corrupt.strategy.as_mut().unwrap().targets[0].symbol = u32::MAX;
    assert!(codec::decode(&corrupt.encode_to_vec()).is_err());
    let mut corrupt = codec::pb::Record::decode(bytes.as_slice()).unwrap();
    corrupt.factual[0]
        .values
        .as_mut()
        .unwrap()
        .usdt_presence
        .push(0);
    assert!(codec::decode(&corrupt.encode_to_vec()).is_err());
}

#[test]
fn bounded_replacement_chains_deduplicate_and_cold_read_exactly_across_days_and_namespaces() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = ManagerDb::open(dir.path()).unwrap();
    storage::ensure_current(&db).unwrap();
    let writer = storage::Storage::default();
    let day = 1_700_000_000_000_000i64.div_euclid(storage::DAY_US) * storage::DAY_US;
    let mut expected = Vec::new();
    let mut m = message(day + 60_000_000, 0.1);
    m.factual_positions[0].positions = (0..96)
        .map(|i| ArchivedFactualPosition {
            symbol: format!("S{i:03}USDT"),
            qty: i as f64,
            usdt: None,
        })
        .collect();
    for i in 0..100 {
        m.received_at_us = day + 60_000_000 + i;
        m.strategy.updated_at_us = m.received_at_us;
        m.factual_positions[0].snapshot_ts_ms = m.received_at_us / 1000 + i;
        m.factual_positions[0].position_ready = i % 2 == 0;
        m.factual_positions[0].positions[0].qty = if i == 0 { -0.0 } else { i as f64 * 1e-17 };
        m.factual_positions[0].positions[1].usdt = if i % 3 == 0 { Some(-0.0) } else { None };
        store(&db, &writer, &m);
        expected.push(m.clone());
    }
    let before = blob_count(&db);
    m.received_at_us += 1;
    m.strategy.updated_at_us = m.received_at_us;
    m.factual_positions[0].snapshot_ts_ms += 1000;
    m.factual_positions[0].position_ready = !m.factual_positions[0].position_ready;
    store(&db, &writer, &m);
    expected.push(m.clone());
    assert_eq!(blob_count(&db), before);
    m.received_at_us = day + storage::DAY_US;
    m.strategy.updated_at_us = m.received_at_us;
    store(&db, &writer, &m);
    expected.push(m.clone());
    assert_eq!(blob_count(&db), before + 1);
    m.received_at_us += 1;
    m.factual_positions[0].source_id = "account-b".into();
    store(&db, &writer, &m);
    expected.push(m.clone());
    assert_eq!(blob_count(&db), before + 2);
    m.received_at_us += 1;
    m.strategy.strategy_name = "beta".into();
    store(&db, &writer, &m);
    expected.push(m.clone());
    assert_eq!(blob_count(&db), before + 3);
    let cf = db.db().cf_handle(storage::SNAPSHOTS_CF).unwrap();
    let mut anchors = 0;
    let mut deepest = 0;
    for row in db.db().iterator_cf(&cf, IteratorMode::Start) {
        let (key, value) = row.unwrap();
        if key[8] == 0 {
            continue;
        }
        let blob = codec::pb::SnapshotBlob::decode(value.as_ref()).unwrap();
        if blob.depth == 0 {
            anchors += 1;
        }
        deepest = deepest.max(blob.depth);
        assert!(blob.depth <= storage::MAX_DEPTH);
        if !blob.base.is_empty() {
            assert_eq!(&key[..8], &blob.base[..8]);
        }
    }
    assert!(anchors >= 7);
    assert_eq!(deepest, storage::MAX_DEPTH);
    drop(cf);
    drop(writer);
    drop(db);
    let archive = PositionArchive::open(ManagerDb::open(dir.path()).unwrap()).unwrap();
    exact(&expected, &archive.scan_from(day).unwrap());
}

#[test]
fn duplicate_namespace_observations_never_create_a_cycle_in_one_atomic_batch() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = ManagerDb::open(dir.path()).unwrap();
    storage::ensure_current(&db).unwrap();
    let writer = storage::Storage::default();
    let mut m = message(1_700_000_000_000_001, 0.1);
    let original = m.factual_positions[0].clone();
    let mut changed = original.clone();
    changed.positions[0].qty = 99.0;
    m.factual_positions = vec![original.clone(), changed, original];
    store(&db, &writer, &m);
    writer.clear_cache();
    let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let key = manager_db::encode_seq_key(m.received_at_us, 0).unwrap();
    exact(
        &m,
        &writer
            .complete(&db, &key, &db.db().get_cf(&cf, key).unwrap().unwrap())
            .unwrap(),
    );
}

#[test]
fn theory_scan_never_reads_snapshots_and_enrichment_keeps_target_bytes_frozen() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = ManagerDb::open(dir.path()).unwrap();
    let archive = PositionArchive::open(db.clone()).unwrap();
    let m = message(1_700_000_000_000_001, 0.12345678901234567);
    let saved = archive
        .append_target(&m.strategy, 10, m.published_accounts.clone())
        .unwrap();
    let key = manager_db::encode_seq_key(saved.received_at_us, saved.seq).unwrap();
    let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let before = db.db().get_cf(&cf, key).unwrap().unwrap();
    let revision = archive.target_changes_since(0, None);
    let source = &m.factual_positions[0];
    let positions = source
        .positions
        .iter()
        .map(|p| {
            (
                p.symbol.clone(),
                crate::viz_snapshot::FactualPosition {
                    symbol: p.symbol.clone(),
                    qty: p.qty,
                    usdt: p.usdt,
                },
            )
        })
        .collect();
    archive
        .enrich_factual_positions(
            saved.clone(),
            vec![SourceFactualPositions {
                source_id: source.source_id.clone(),
                snapshot_ts_ms: source.snapshot_ts_ms,
                position_ready: source.position_ready,
                positions,
            }],
        )
        .unwrap();
    assert_eq!(db.db().get_cf(&cf, key).unwrap().unwrap(), before);
    assert_eq!(archive.target_changes_since(0, None), revision);
    let full = archive.latest().unwrap().unwrap();
    assert_eq!(
        full.factual_positions[0].snapshot_ts_ms,
        source.snapshot_ts_ms
    );
    let snapshots = db.db().cf_handle(storage::SNAPSHOTS_CF).unwrap();
    let keys = db
        .db()
        .iterator_cf(&snapshots, IteratorMode::Start)
        .map(|r| r.unwrap().0)
        .collect::<Vec<_>>();
    for key in keys {
        db.db().delete_cf(&snapshots, key).unwrap();
    }
    archive.storage.clear_cache();
    let mut inputs = Vec::new();
    archive
        .visit_target_updates_through(i64::MAX, |m| {
            inputs.push(m);
            Ok(())
        })
        .unwrap();
    exact(&inputs, &vec![saved]);
    assert!(archive.latest().is_err());
}

#[test]
fn day_local_snapshot_reclamation_and_self_contained_backup_survive_cold_restart() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = ManagerDb::open(&dir.path().join("source")).unwrap();
    storage::ensure_current(&db).unwrap();
    let writer = storage::Storage::default();
    let now = crate::kline::now_us();
    let day = storage::DAY_US;
    let before = (now.div_euclid(day) - 40) * day;
    let mut messages = Vec::new();
    for ts in [before - day, before - day + 1, before, before + 1] {
        let mut m = message(ts, 0.25);
        m.factual_positions[0].positions[0].qty = ts as f64 * 1e-17;
        store(&db, &writer, &m);
        messages.push(m);
    }
    let archive = PositionArchive::open(db.clone()).unwrap();
    let mut bytes = Vec::new();
    archive.export_prefix(&mut bytes, (before, 0), now).unwrap();
    let destination = dir.path().join("backup");
    let receipt = retention::verify_restore(bytes.as_slice(), &destination).unwrap();
    let restored = PositionArchive::open(ManagerDb::open(&destination).unwrap()).unwrap();
    exact(&messages[..2].to_vec(), &restored.scan_from(1).unwrap());
    drop(restored);
    archive.commit_receipt(&receipt, now).unwrap();
    archive.compact_archived((before, 0)).unwrap();
    let cf = db.db().cf_handle(storage::SNAPSHOTS_CF).unwrap();
    assert!(
        db.db()
            .iterator_cf(&cf, IteratorMode::Start)
            .all(|r| r.unwrap().0[..8] >= storage::day_key(before)[..])
    );
    drop(cf);
    drop(archive);
    drop(writer);
    drop(db);
    let reopened =
        PositionArchive::open(ManagerDb::open(&dir.path().join("source")).unwrap()).unwrap();
    exact(
        &messages[2..].to_vec(),
        &reopened.scan_from(before).unwrap(),
    );
    reopened.commit_receipt(&receipt, now).unwrap();
}
