use super::*;
use crate::order_config::TargetPosition;
use crate::position_archive::{
    ArchivedFactualPosition, ArchivedSourcePositions, published_account,
};
use crate::strategy_catalog::PositionStrategy;
use std::collections::BTreeMap;
const DAY: i64 = 86_400_000_000;
const NOW: i64 = 100 * DAY;

pub(super) fn records(
    start: i64,
    count: usize,
) -> (
    Vec<(Cursor, Vec<u8>)>,
    retention::Boundary,
    retention::Boundary,
) {
    let before = retention::Boundary::default();
    let mut after = before.clone();
    let mut rows = Vec::new();
    for i in 0..count {
        let ts = start + i as i64;
        let strategy = PositionStrategy {
            strategy_name: "alpha".into(),
            updated_at_us: ts,
            targets: BTreeMap::from([(
                "BTCUSDT".into(),
                TargetPosition::new(i as f64 + 0.25, 1).unwrap(),
            )]),
            symbol_order_strategy_overrides: BTreeMap::new(),
        };
        let mut m = PositionUpdateMsg::from_strategy(
            ts,
            0,
            strategy,
            vec![],
            vec![published_account("source-a", "alpha", 2.0)],
        );
        m.factual_positions = vec![ArchivedSourcePositions {
            source_id: "source-a".into(),
            snapshot_ts_ms: ts / 1000,
            position_ready: true,
            positions: vec![ArchivedFactualPosition {
                symbol: "BTCUSDT".into(),
                qty: -0.0,
                usdt: Some(1.2345678901234567),
            }],
        }];
        after.ingest(&m).unwrap();
        rows.push(((ts, 0), codec::encode(&m).unwrap()));
    }
    (rows, before, after)
}
fn root(temp: &tempfile::TempDir) -> ColdRoot {
    ColdRoot::create(&temp.path().join("cold"), "source-identity").unwrap()
}

#[test]
fn bounded_one_batch_roundtrip_confirm_retry_and_oldest_retirement() {
    let t = tempfile::tempdir().unwrap();
    let r = root(&t);
    let (rows, before, after) = records(10 * DAY, 3);
    let s = r
        .write_batch(&rows, &before, &after, BatchLimits::default())
        .unwrap();
    assert_eq!(s.count, 3);
    assert!(r.list(NOW, true).unwrap().is_empty());
    assert_eq!(
        r.write_batch(&rows, &before, &after, BatchLimits::default())
            .unwrap(),
        s
    );
    assert!(r.delete_verified(&s, NOW).is_err());
    r.confirm(&s).unwrap();
    assert_eq!(r.list(NOW, true).unwrap(), vec![s.clone()]);
    let mut seen = Vec::new();
    r.visit(&s, |key, bytes| {
        seen.push((key, bytes));
        assert!(r.delete_verified(&s, NOW).is_err());
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, rows);
    // Closed means another owner can acquire the DB without stopping Manager.
    {
        let _db = DB::open_cf(&options(false), r.path.join(&s.id), ["default", ROWS]).unwrap();
    }
    r.delete_verified(&s, NOW).unwrap();
    r.delete_verified(&s, NOW).unwrap();
    let partial = r.path.join(format!(".removing-{}", s.id));
    private_dir(&partial).unwrap();
    std::fs::write(
        partial.join("partially-removed-file"),
        b"partial crash remainder",
    )
    .unwrap();
    r.delete_verified(&s, NOW).unwrap();
    assert!(!partial.exists());
    assert!(!r.path.join(&s.id).exists());
    assert_eq!(
        json_file::<Segment>(&r.path.join("RETAINED_BOUNDARY.json")).unwrap(),
        s
    );
}

#[test]
fn invalid_order_count_bytes_day_or_boundary_never_produces_a_ready_segment() {
    let t = tempfile::tempdir().unwrap();
    let r = root(&t);
    let (rows, before, after) = records(10 * DAY, 3);
    assert!(
        r.write_batch(
            &rows,
            &before,
            &after,
            BatchLimits {
                max_records: 2,
                max_bytes: 1 << 20
            }
        )
        .is_err()
    );
    let mut reversed = rows.clone();
    reversed.reverse();
    assert!(
        r.write_batch(&reversed, &before, &after, BatchLimits::default())
            .is_err()
    );
    assert!(
        r.write_batch(&rows, &before, &before, BatchLimits::default())
            .is_err()
    );
    let (mut huge, b, a) = records(11 * DAY, 1);
    let mut m = codec::decode(&huge[0].1).unwrap();
    m.factual_positions[0].positions = (0..1000)
        .map(|i| ArchivedFactualPosition {
            symbol: format!("COIN{i}USDT"),
            qty: i as f64,
            usdt: None,
        })
        .collect();
    huge[0].1 = codec::encode(&m).unwrap();
    assert!(
        r.write_batch(
            &huge,
            &b,
            &a,
            BatchLimits {
                max_records: 1,
                max_bytes: 4096
            }
        )
        .is_err()
    );
    let (cross, b, a) = records(12 * DAY - 8 * 3_600_000_000 - 1, 2);
    assert!(
        r.write_batch(&cross, &b, &a, BatchLimits::default())
            .is_err()
    );
    assert!(r.list(NOW, false).unwrap().is_empty());
}

#[test]
fn source_deletion_preserves_33_days_and_cannot_jump_over_an_older_segment() {
    let t = tempfile::tempdir().unwrap();
    let r = root(&t);
    let (one, b, a) = records(10 * DAY, 1);
    let s1 = r.write_batch(&one, &b, &a, BatchLimits::default()).unwrap();
    r.confirm(&s1).unwrap();
    let (two, b, a) = records(11 * DAY, 1);
    let s2 = r.write_batch(&two, &b, &a, BatchLimits::default()).unwrap();
    r.confirm(&s2).unwrap();
    assert_eq!(
        r.list_after(NOW, Some(&s1.id), 1).unwrap(),
        vec![s2.clone()]
    );
    assert!(r.list_after(NOW, Some("../escape"), 1).is_err());
    assert!(r.delete_verified(&s2, NOW).is_err());
    assert!(r.delete_verified(&s1, 40 * DAY).is_err());
    r.delete_verified(&s1, NOW).unwrap();
    r.delete_verified(&s2, NOW).unwrap();
}

#[test]
fn independent_bundle_restore_detects_corruption_truncation_and_path_escape() {
    let t = tempfile::tempdir().unwrap();
    let r = root(&t);
    let (rows, b, a) = records(10 * DAY, 3);
    let s = r
        .write_batch(&rows, &b, &a, BatchLimits::default())
        .unwrap();
    r.confirm(&s).unwrap();
    let mut bytes = Vec::new();
    let bundle = wire::export(&r, &s.id, &mut bytes, NOW).unwrap();
    let d = t.path().join("restored");
    assert_eq!(
        wire::receive(&mut bytes.as_slice(), &d, &s, 16 * 1024 * 1024).unwrap(),
        bundle
    );
    assert_eq!(verify_segment(&d).unwrap(), s);
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(
        wire::receive(
            &mut corrupt.as_slice(),
            &t.path().join("corrupt"),
            &s,
            16 * 1024 * 1024
        )
        .is_err()
    );
    assert!(
        wire::receive(
            &mut &bytes[..bytes.len() - 1],
            &t.path().join("truncated"),
            &s,
            16 * 1024 * 1024
        )
        .is_err()
    );
    assert!(wire::receive(&mut bytes.as_slice(), &t.path().join("too-large"), &s, 4096).is_err());
    let header_len = u64::from_be_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let mut malicious: wire::Bundle = serde_json::from_slice(&bytes[24..24 + header_len]).unwrap();
    malicious.files[0].name = "../escape".into();
    let header = serde_json::to_vec(&malicious).unwrap();
    let mut frame = bytes[..16].to_vec();
    frame.extend_from_slice(&(header.len() as u64).to_be_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&bytes[24 + header_len..]);
    assert!(
        wire::receive(
            &mut frame.as_slice(),
            &t.path().join("unsafe"),
            &s,
            16 * 1024 * 1024
        )
        .is_err()
    );
    assert!(!t.path().join("escape").exists());
    assert!(r.path.join(&s.id).exists());
}

#[test]
fn cold_root_refuses_an_existing_foreign_store_before_writing_a_lock() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("exec");
    private_dir(&p).unwrap();
    std::fs::write(p.join("IDENTITY"), "foreign").unwrap();
    assert!(ColdRoot::create(&p, "source").is_err());
    assert!(!p.join(".maintenance.lock").exists());
}

#[test]
fn public_pull_has_no_private_or_proxy_fallback_and_enforces_hourly_cadence() {
    for ip in [
        "10.1.2.3",
        "172.16.30.42",
        "192.168.1.1",
        "127.0.0.1",
        "169.254.2.3",
        "100.64.0.1",
        "198.18.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "240.0.0.1",
        "::1",
        "source.example",
    ] {
        assert!(pull::validate_public_ipv4(ip).is_err(), "{ip}");
    }
    assert!(pull::validate_public_ipv4("8.8.8.8").is_ok());
    let c = pull::PullConfig {
        backup_root: PathBuf::from("/archive"),
        interval_secs: 3599,
        max_bundle_bytes: 1 << 20,
        operation_timeout_secs: 300,
        transfer_timeout_secs: 3600,
        hosts: vec![],
    };
    assert!(c.validate().is_err());
    assert_eq!(pull::shell_quote("a'$(id)"), "'a'\\''$(id)'");
}
