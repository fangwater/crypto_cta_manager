use super::*;
use crate::native_futures::{
    nav::{Offset, Side},
    records::fixture,
};

fn snapshot(tmp: &std::path::Path) -> Result<reader::Snapshot> {
    let primary = tmp.join("exec");
    let db = fixture::open(&primary);
    let order = fixture::Order::new(
        "SHFE:cu2612",
        "20261117",
        "yida:20261117:group:1",
        "7",
        Side::Buy,
        Offset::Open,
        1,
        100.0,
    );
    let now = crate::native_futures::now_us();
    fixture::put_order(&db, now, &order.lifecycle(fixture::NEW, "exec", now, now));
    let source = Source {
        id: "account".into(),
        account: "test".into(),
        adapter: "yida".into(),
        rocksdb_path: primary.clone(),
        enabled: true,
        sync: None,
    };
    let pool = crate::rocks_source::ReaderPool::new(&tmp.join("secondary"), &[&primary])?;
    let mut cache = reader::Cache::default();
    reader::with_db(&pool, &source, |db| cache.read(&source, db, 10, 30))
}

#[test]
fn coverage_cut_follows_records_and_replication_state() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let observed = snapshot(tmp.path())?;
    let now = observed.read_at_us;
    let sample = observation(Sample::default(), Ok(observed.clone()), now);
    assert_eq!(sample.collection(), "live");
    let cut = sample.nav_cut.context("local coverage")?;
    assert_eq!(cut.at_us, observed.through_us.context("through")?);
    assert!(!cut.historical);
    assert!(sample.position().is_none());
    assert!(
        sample
            .position_reason(None)
            .is_some_and(|reason| reason.contains("起始快照"))
    );

    let mut stale = observed.clone();
    stale.replication = Some(crate::native_futures::sync::ReplicaStatus {
        initialized: true,
        source_captured_at_us: Some(now - 1),
        last_synced_at_us: Some(now - 60_000_000),
        ..Default::default()
    });
    stale.through_us = Some(now - 1);
    stale.replication_stale = true;
    let delayed = observation(sample.clone(), Ok(stale), now);
    assert_eq!(delayed.collection(), "stale");
    let historical = delayed.nav_cut.context("replica coverage")?;
    assert!(historical.historical && historical.at_us == now - 1);
    assert!(!historical.accepts(cut));

    let mut failed = observed.clone();
    failed.replication = Some(crate::native_futures::sync::ReplicaStatus {
        last_error: Some("test synchronization failure".into()),
        ..Default::default()
    });
    let failed = observation(sample, Ok(failed), now);
    assert!(failed.nav_cut.is_none() && failed.nav.is_none());
    assert_eq!(failed.collection(), "stale");
    Ok(())
}

#[test]
fn holdings_and_cached_nav_are_current_only_for_the_same_cut() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let observed = snapshot(tmp.path())?;
    let now = observed.read_at_us;
    let mut sample = observation(Sample::default(), Ok(observed.clone()), now);
    let cut = sample.nav_cut.context("coverage")?;
    sample.holdings = Some(Holdings {
        cut,
        anchor_us: now - 1,
        rows: BTreeMap::from([(("SHFE".into(), "cu2612".into()), (1, 0))]),
        notionals: Some((500.0, 0.0)),
    });
    let position = sample.position().context("holdings")?;
    assert_eq!((position.status(), position.count()), ("live", 1));

    let later = Sample {
        nav_cut: Some(NavCut {
            at_us: cut.at_us + 1,
            ..cut
        }),
        ..sample.clone()
    };
    assert_eq!(
        later.position().context("previous holdings")?.status(),
        "historical"
    );

    let saved = CachedNav {
        cut,
        anchor_fingerprint: "initial-anchor".into(),
        report: None,
        retry_at_us: Some(now + NAV_RETRY_US),
    };
    assert!(saved.reusable(cut, "initial-anchor", now));
    assert!(!saved.reusable(cut, "new-anchor", now));
    assert!(!saved.reusable(cut, "initial-anchor", now + NAV_RETRY_US));
    assert!(!saved.reusable(
        NavCut {
            historical: true,
            ..cut
        },
        "initial-anchor",
        now
    ));
    assert!(!saved.reusable(
        NavCut {
            at_us: cut.at_us + 120_000_000,
            ..cut
        },
        "initial-anchor",
        now
    ));
    Ok(())
}
