use super::*;
use crate::manager_db::POSITION_UPDATES_CF;
const DAY: i64 = 86_400_000_000;
const NOW: i64 = 100 * DAY;

fn setup(
    times: &[i64],
) -> (
    tempfile::TempDir,
    PositionArchive,
    Vec<PositionUpdateMsg>,
    cold::manager::Settings,
) {
    let root = tempfile::tempdir().unwrap();
    let db = ManagerDb::open(&root.path().join("hot")).unwrap();
    let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let mut expected = Vec::new();
    for (index, &ts) in times.iter().enumerate() {
        let original = super::super::storage_tests::message(ts, index as f64 * 1e-17);
        let json = serde_json::to_vec(&original).unwrap();
        let key = manager_db::encode_seq_key(ts, 0).unwrap();
        expected.push(migration::decode_json(&key, &json).unwrap());
        db.db().put_cf(&cf, key, json).unwrap();
    }
    db.db()
        .put(b"position_current:protected", b"current target")
        .unwrap();
    db.ensure_column_family("klines_1m").unwrap();
    db.db()
        .put_cf(
            &db.db().cf_handle("klines_1m").unwrap(),
            b"protected",
            b"candle",
        )
        .unwrap();
    drop(cf);
    let settings = cold::manager::Settings {
        root: root.path().join("cold"),
        hot_conversion: Some(Settings {
            max_records: 2,
            max_work_millis: 1000,
            ..Default::default()
        }),
        ..Default::default()
    };
    let archive = PositionArchive::open_with_cold(db, Some(settings.clone())).unwrap();
    (root, archive, expected, settings)
}
fn exact(a: &impl Serialize, b: &impl Serialize) {
    assert_eq!(rmp_serde::to_vec(a).unwrap(), rmp_serde::to_vec(b).unwrap());
}

#[test]
fn new_signals_are_pb_and_partial_conversion_restarts_with_exact_history() {
    let (root, archive, expected, settings) = setup(&[90 * DAY, 90 * DAY + 1, 90 * DAY + 2]);
    assert_eq!(archive.hot_format(), "converting_json");
    exact(&archive.scan_from(1).unwrap(), &expected);
    let p = archive.convert_hot_once(NOW).unwrap().unwrap();
    assert_eq!(p.converted_records, 2);
    assert!(!p.completed);
    assert!(p.last_batch_bytes <= settings.hot_conversion.as_ref().unwrap().max_bytes);
    exact(&archive.scan_from(1).unwrap(), &expected);
    let revision = archive.target_changes_since(0, None).0;
    assert_eq!(revision, 0);
    let mut new = expected[0].strategy.clone();
    new.updated_at_us = NOW;
    archive.append_target(&new, 7, vec![]).unwrap();
    let cf = archive.db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let bytes = archive
        .db
        .db()
        .get_cf(&cf, manager_db::encode_seq_key(NOW, 0).unwrap())
        .unwrap()
        .unwrap();
    assert!(!is_json(&bytes));
    drop(cf);
    drop(archive);
    assert!(PositionArchive::open(ManagerDb::open(&root.path().join("hot")).unwrap()).is_err());
    let archive = PositionArchive::open_with_cold(
        ManagerDb::open(&root.path().join("hot")).unwrap(),
        Some(settings),
    )
    .unwrap();
    let p = archive.convert_hot_once(NOW).unwrap().unwrap();
    assert!(p.completed);
    assert_eq!(p.converted_records, 3);
    exact(&archive.scan_from(1).unwrap()[..3].to_vec(), &expected);
    assert_eq!(archive.hot_format(), "protobuf");
    drop(archive);
    let archive =
        PositionArchive::open(ManagerDb::open(&root.path().join("hot")).unwrap()).unwrap();
    exact(&archive.scan_from(1).unwrap()[..3].to_vec(), &expected);
    assert!(
        archive
            .current_target(&new.strategy_name, 7)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        archive
            .db
            .db()
            .get_cf(
                &archive.db.db().cf_handle("klines_1m").unwrap(),
                b"protected"
            )
            .unwrap()
            .unwrap(),
        b"candle"
    );
}

#[test]
fn expired_json_is_moved_directly_to_pb_cold_and_recent_json_is_replaced_in_place() {
    let (_root, archive, expected, _) = setup(&[10 * DAY, 90 * DAY, 90 * DAY + 1]);
    assert!(archive.convert_hot_once(NOW).unwrap().is_none());
    assert_eq!(
        archive
            .hot_conversion_status()
            .unwrap()
            .unwrap()
            .converted_records,
        0
    );
    let segment = archive.archive_cold_once(NOW).unwrap().unwrap();
    assert_eq!(segment.count, 1);
    let cold = cold::ColdRoot::open(archive.cold.as_ref().unwrap().root.path()).unwrap();
    assert_eq!(cold.list_after(NOW, None, 10).unwrap().len(), 1);
    let p = archive.convert_hot_once(NOW).unwrap().unwrap();
    assert_eq!(p.converted_records, 2);
    assert!(p.completed);
    exact(
        &archive.scan_from(archive.retention_floor().0).unwrap(),
        &expected[1..].to_vec(),
    );
}

#[test]
fn readers_and_archive_writer_defer_conversion_without_moving_cursor() {
    let (_root, archive, _, _) = setup(&[90 * DAY]);
    let before = archive.hot_conversion_status().unwrap().unwrap();
    {
        let _reader = archive.retention_read();
        assert!(archive.convert_hot_once(NOW).unwrap().is_none());
    }
    archive.hold_writes(|| assert!(archive.convert_hot_once(NOW).unwrap().is_none()));
    exact(&before, &archive.hot_conversion_status().unwrap().unwrap());
    assert!(archive.convert_hot_once(NOW).unwrap().unwrap().completed);
}

#[test]
fn oversized_original_is_preserved_and_progress_cannot_skip_it() {
    let (_root, mut archive, _, _) = setup(&[90 * DAY]);
    archive
        .cold
        .as_mut()
        .unwrap()
        .settings
        .hot_conversion
        .as_mut()
        .unwrap()
        .max_bytes = 8192;
    let cf = archive.db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    let key = manager_db::encode_seq_key(90 * DAY, 0).unwrap();
    let mut msg = super::super::storage_tests::message(90 * DAY, 1.0);
    msg.factual_positions[0].positions[0].symbol = "X".repeat(16000);
    let value = serde_json::to_vec(&msg).unwrap();
    archive.db.db().put_cf(&cf, key, &value).unwrap();
    let before = archive.hot_conversion_status().unwrap().unwrap();
    assert!(archive.convert_hot_once(NOW).is_err());
    assert_eq!(archive.db.db().get_cf(&cf, key).unwrap().unwrap(), value);
    exact(&before, &archive.hot_conversion_status().unwrap().unwrap());
}

#[test]
fn deployment_gate_requires_transition_opt_in_and_accepts_completed_pb() {
    let (root, archive, _, settings) = setup(&[90 * DAY]);
    assert!(migration::check_storage(&root.path().join("hot")).is_err());
    assert!(migration::check_storage_with_cold(&root.path().join("hot"), &settings.root).is_err());
    assert_eq!(
        migration::check_storage_with_hot_conversion(&root.path().join("hot"), &settings.root)
            .unwrap()["storage_format"],
        "converting_json"
    );
    archive.convert_hot_once(NOW).unwrap();
    assert_eq!(
        migration::check_storage(&root.path().join("hot")).unwrap()["storage_format"],
        "protobuf"
    );
}

#[test]
fn target_only_pages_and_full_history_remain_exact_through_conversion() {
    let (_root, archive, expected, _) = setup(&[90 * DAY, 90 * DAY + 1, 90 * DAY + 2]);
    let targets = |archive: &PositionArchive| {
        let mut result = Vec::new();
        archive
            .visit_target_updates_through(NOW, |m| {
                result.push(m);
                Ok(())
            })
            .unwrap();
        result
    };
    let before = targets(&archive);
    archive.convert_hot_once(NOW).unwrap();
    exact(&before, &targets(&archive));
    let page: Vec<PositionUpdateMsg> =
        serde_json::from_slice(&archive.raw_json_page(None, 10).unwrap()).unwrap();
    exact(&page, &expected);
    archive.convert_hot_once(NOW).unwrap();
    exact(&before, &targets(&archive));
}

#[test]
fn visible_commit_then_reported_failure_resumes_without_skipping_or_reparsing_pb() {
    let (root, archive, expected, settings) = setup(&[90 * DAY, 90 * DAY + 1, 90 * DAY + 2]);
    assert!(
        archive
            .convert_hot_with(NOW, || anyhow::bail!("injected post-commit failure"))
            .is_err()
    );
    assert_eq!(
        archive
            .hot_conversion_status()
            .unwrap()
            .unwrap()
            .converted_records,
        2
    );
    exact(&archive.scan_from(1).unwrap(), &expected);
    drop(archive);
    let archive = PositionArchive::open_with_cold(
        ManagerDb::open(&root.path().join("hot")).unwrap(),
        Some(settings),
    )
    .unwrap();
    let progress = archive.convert_hot_once(NOW).unwrap().unwrap();
    assert_eq!(progress.converted_records, 3);
    assert!(progress.completed);
    exact(&archive.scan_from(1).unwrap(), &expected);
}

#[test]
fn delayed_enrichment_converts_json_without_reparsing_frozen_targets() {
    let (_root, archive, expected, _) = setup(&[90 * DAY, 90 * DAY + 1]);
    let incoming = vec![crate::viz_snapshot::SourceFactualPositions {
        source_id: "account-a".into(),
        snapshot_ts_ms: NOW / 1000,
        position_ready: true,
        positions: BTreeMap::from([(
            "BTCUSDT".into(),
            crate::viz_snapshot::FactualPosition {
                symbol: "BTCUSDT".into(),
                qty: -0.0,
                usdt: None,
            },
        )]),
    }];
    archive
        .enrich_factual_positions(expected[0].clone(), incoming.clone())
        .unwrap();
    let rows = archive.scan_from(1).unwrap();
    exact(&rows[0].strategy, &expected[0].strategy);
    exact(&rows[0].published_accounts, &expected[0].published_accounts);
    assert_eq!(
        rows[0].factual_positions[0].positions[0].qty.to_bits(),
        (-0.0f64).to_bits()
    );
    archive.convert_hot_once(NOW).unwrap();
    let before = archive.scan_from(1).unwrap();
    exact(&before, &rows);
    archive
        .enrich_factual_positions(expected[0].clone(), incoming)
        .unwrap();
    exact(&archive.scan_from(1).unwrap(), &rows);
}

#[test]
fn disk_reserve_refuses_conversion_without_touching_json_or_cursor() {
    let (_root, mut archive, expected, _) = setup(&[90 * DAY]);
    archive.cold.as_mut().unwrap().settings.min_free_bytes = u64::MAX;
    let before = archive.hot_conversion_status().unwrap().unwrap();
    assert!(archive.convert_hot_once(NOW).is_err());
    exact(&before, &archive.hot_conversion_status().unwrap().unwrap());
    exact(&archive.scan_from(1).unwrap(), &expected);
}

#[test]
fn hot_and_cold_share_one_minute_quota_across_owner_restarts() {
    let (root, archive, _, settings) = setup(&[90 * DAY, 90 * DAY + 1, 90 * DAY + 2]);
    archive.archive_cold_once(NOW).unwrap();
    let before = archive.hot_conversion_status().unwrap().unwrap();
    assert_eq!(before.converted_records, 2);
    drop(archive);
    let archive = PositionArchive::open_with_cold(
        ManagerDb::open(&root.path().join("hot")).unwrap(),
        Some(settings),
    )
    .unwrap();
    archive.archive_cold_once(NOW + 59_000_000).unwrap();
    exact(&archive.hot_conversion_status().unwrap().unwrap(), &before);
    archive.archive_cold_once(NOW + 60_000_000).unwrap();
    assert!(archive.hot_conversion_status().unwrap().unwrap().completed);
    assert_eq!(
        archive.cold_status().unwrap().last_action.as_deref(),
        Some("convert_hot_json")
    );
}

#[test]
fn pb_dictionary_header_cannot_be_misread_as_json_whitespace() {
    use prost::Message;
    let message = super::super::storage_tests::message(90 * DAY, -0.0);
    let mut record =
        codec::pb::Record::decode(codec::encode_targets(&message).unwrap().as_slice()).unwrap();
    // A valid dictionary can have an unused first string. Remap msg_type so
    // this changes only the byte prefix, never the decoded target semantics.
    assert_eq!(record.msg_type, 0);
    let name = record.strings[0].clone();
    record.strings[0] = "{abcdefg}".into();
    record.msg_type = record.strings.len() as u32;
    record.strings.push(name);
    let bytes = record.encode_to_vec();
    assert_eq!(&bytes[..3], &[10, 9, b'{']);
    assert!(!is_json(&bytes));
    let decoded = codec::decode_target(&bytes, false, true).unwrap();
    exact(&decoded.strategy, &message.strategy);
}
