//! No SDK, PostgreSQL, external endpoint or execution process is used.
use super::{store::*, *};
use crate::{
    native_futures::{config::Config, reader},
    runtime_mode::RuntimeMode,
};
use rocksdb::{DB, Options, WriteBatch};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const TEST_TOKEN: &str = "isolated-native-sync-test-token-0001";

#[tokio::test]
async fn maximum_configured_export_pages_are_accepted_and_durable_across_collector_restart() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, mut local) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = MAX_PAGE_RECORDS;
    cloud.sync_export.as_mut().unwrap().batch_bytes = 8 * 1024 * 1024;
    cloud.validate().unwrap();
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let mut batch = WriteBatch::default();
    for n in 0..MAX_PAGE_RECORDS + 17 {
        batch.put_cf(
            db.cf_handle(NativeCf::Events.name()).unwrap(),
            (n as u64).to_be_bytes(),
            b"{\"type\":\"bounded-sync-fixture\"}",
        );
    }
    db.write(batch).unwrap();
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    assert_eq!(first.changes.len(), MAX_PAGE_RECORDS);
    assert!(first.next < first.until);
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    let mut oversized = first.clone();
    oversized.changes.push(first.changes[0].clone());
    assert!(replica.accept(&oversized).is_err());
    assert_eq!(replica.status().unwrap().received_cursor, 0);
    replica.accept(&first).unwrap();
    replica.apply().unwrap();
    assert!(!replica.status().unwrap().initialized);
    drop(replica);
    let (url, server) = serve(cloud).await;
    local.sources[0].sync.as_mut().unwrap().url = url;
    let replica = Arc::new(Replica::open(&local, &local.sources[0]).unwrap());
    let state = replica.sync_once(&client(), TEST_TOKEN).await.unwrap();
    assert_eq!(state.committed_cursor, first.until);
    assert!(state.last_error.is_none());
    assert_eq!(all_bytes(&replica.db), all_bytes(&db));
    assert_eq!(
        replica
            .sync_once(&client(), TEST_TOKEN)
            .await
            .unwrap()
            .committed_cursor,
        state.committed_cursor
    );
    server.abort();
}

#[test]
fn live_secondary_can_read_original_column_family_wal_batches() {
    use rocksdb::{AsColumnFamilyRef, WriteBatchIteratorCf};
    struct Writes(Vec<(u32, Vec<u8>, Vec<u8>)>);
    impl WriteBatchIteratorCf for Writes {
        fn put_cf(&mut self, id: u32, key: &[u8], value: &[u8]) {
            self.0.push((id, key.to_vec(), value.to_vec()));
        }
        fn delete_cf(&mut self, _: u32, _: &[u8]) {
            panic!("unexpected delete in fixture");
        }
        fn merge_cf(&mut self, _: u32, _: &[u8], _: &[u8]) {
            panic!("unexpected merge in fixture");
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let (cloud, _) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let old = db.latest_sequence_number();
    let handle = db.cf_handle(NativeCf::Fills.name()).unwrap();
    // The C API reads the live handle's declared ID; no storage ID is guessed.
    let cf_id = unsafe { librocksdb_sys::rocksdb_column_family_handle_get_id(handle.inner()) };
    let mut batch = WriteBatch::default();
    batch.put_cf(handle, b"late-trade", b"original-value");
    db.write(batch).unwrap();
    let view =
        reader::open_secondary(&cloud.sources[0], &cloud.secondary_root.join("wal")).unwrap();
    assert_eq!(view.latest_sequence_number(), db.latest_sequence_number());
    let mut wal = view.get_updates_since(old).unwrap();
    let (seq, batch) = wal.next().unwrap().unwrap();
    assert_eq!(seq, old + 1);
    let mut writes = Writes(vec![]);
    batch.iterate_cf(&mut writes);
    assert_eq!(
        writes.0,
        vec![(cf_id, b"late-trade".to_vec(), b"original-value".to_vec())]
    );
    assert!(wal.next().is_none());
    wal.status().unwrap();
}

#[test]
fn wal_incremental_cuts_preserve_repeated_mutations_and_resume_from_exact_source_sequence() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, local) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = 1000;
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    replica.accept(&first).unwrap();
    replica.apply().unwrap();
    // A new mutable key changes twice, including returning to an older value.
    // Every original mutation must survive, with one final row in its CF.
    for value in [b"first".as_slice(), b"second", b"first"] {
        db.put_cf(
            db.cf_handle(NativeCf::Orders.name()).unwrap(),
            b"new-order",
            value,
        )
        .unwrap();
    }
    let next = super::export::page(
        &cloud,
        &cloud.sources[0].id,
        with_hash(
            request(first.until, None, Some(first.store_id.clone())),
            Some(first.until_sha256.clone()),
        ),
    )
    .unwrap();
    assert_eq!(next.changes.len(), 3);
    replica.accept(&next).unwrap();
    replica.apply().unwrap();
    assert_eq!(all_bytes(&replica.db), all_bytes(&db));
    let journal = open_owned(
        &cloud
            .sync_export
            .as_ref()
            .unwrap()
            .journal_root
            .join(&cloud.sources[0].id),
        "cn_futures_sync_export",
        &cloud.sources[0].id,
        &[],
    )
    .unwrap();
    let head: Value = get(&journal, b"meta/head").unwrap().unwrap();
    assert_eq!(
        head["source_sequence"].as_u64(),
        Some(db.latest_sequence_number())
    );
    assert_eq!(head["counts"], json!([1, 2, 1]));
    drop(journal);
    let unchanged = super::export::page(
        &cloud,
        &cloud.sources[0].id,
        with_hash(
            request(next.until, None, Some(next.store_id.clone())),
            Some(next.until_sha256.clone()),
        ),
    )
    .unwrap();
    assert!(unchanged.changes.is_empty());
    assert_eq!(unchanged.until, next.until);
}

#[test]
fn wal_catches_old_evidence_conflicts_even_when_new_events_are_appended() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, _) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let mut changed = fixture()["state"].clone();
    changed["projection_error"] = json!("rewritten");
    let mut batch = WriteBatch::default();
    batch.put_cf(
        db.cf_handle(NativeCf::Events.name()).unwrap(),
        b"evidence\x00",
        serde_json::to_vec(&changed).unwrap(),
    );
    batch.put_cf(
        db.cf_handle(NativeCf::Events.name()).unwrap(),
        b"evidence\x01",
        b"new event",
    );
    db.write(batch).unwrap();
    let result = super::export::page(
        &cloud,
        &cloud.sources[0].id,
        with_hash(
            request(first.until, None, Some(first.store_id)),
            Some(first.until_sha256),
        ),
    );
    assert!(result.unwrap_err().to_string().contains("immutable"));
    let journal = open_owned(
        &cloud
            .sync_export
            .as_ref()
            .unwrap()
            .journal_root
            .join(&cloud.sources[0].id),
        "cn_futures_sync_export",
        &cloud.sources[0].id,
        &[],
    )
    .unwrap();
    let head: Value = get(&journal, b"meta/head").unwrap().unwrap();
    assert_eq!(head["cursor"], 3);
}

#[test]
fn a_source_sequence_gap_audits_the_fixed_view_instead_of_skipping_history() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, _) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let path = cloud
        .sync_export
        .as_ref()
        .unwrap()
        .journal_root
        .join(&cloud.sources[0].id);
    let journal = open_owned(&path, "cn_futures_sync_export", &cloud.sources[0].id, &[]).unwrap();
    let mut head: Value = get(&journal, b"meta/head").unwrap().unwrap();
    head["source_sequence"] = json!(1);
    let mut batch = WriteBatch::default();
    batch.put(b"meta/head", serde_json::to_vec(&head).unwrap());
    write(&journal, batch).unwrap();
    drop(journal);
    db.put_cf(
        db.cf_handle(NativeCf::Fills.name()).unwrap(),
        b"another-fill",
        b"durable-original",
    )
    .unwrap();
    let next = super::export::page(
        &cloud,
        &cloud.sources[0].id,
        with_hash(
            request(first.until, None, Some(first.store_id)),
            Some(first.until_sha256),
        ),
    )
    .unwrap();
    assert_eq!(next.changes.len(), 1);
    assert_eq!(
        next.changes[0].decode().unwrap(),
        (b"another-fill".to_vec(), b"durable-original".to_vec())
    );
}

#[test]
fn unchanged_verified_cut_adoption_requires_earlier_source_baseline_and_receiver_hash() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, _) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let stamps:serde_json::Map<String,Value>=std::fs::read_dir(&cloud.sources[0].rocksdb_path).unwrap().map(|entry|{
        let e=entry.unwrap();let m=e.metadata().unwrap();
        (e.file_name().into_string().unwrap(),json!({"size":m.len(),"mtime_ns":m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64}))
    }).collect();
    let baseline = temp.path().join("before.json");
    std::fs::write(
        &baseline,
        serde_json::to_vec(&json!({"execution_store_files":stamps})).unwrap(),
    )
    .unwrap();
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let path = cloud
        .sync_export
        .as_ref()
        .unwrap()
        .journal_root
        .join(&cloud.sources[0].id);
    let journal = open_owned(&path, "cn_futures_sync_export", &cloud.sources[0].id, &[]).unwrap();
    let mut head: Value = get(&journal, b"meta/head").unwrap().unwrap();
    let mut cut: Value = get(&journal, &seq_key(b'B', first.until)).unwrap().unwrap();
    head["source_sequence"] = Value::Null;
    head["source_cf_ids"] = Value::Null;
    cut["source_sequence"] = Value::Null;
    cut["source_cf_ids"] = Value::Null;
    let mut batch = WriteBatch::default();
    batch.put(b"meta/head", serde_json::to_vec(&head).unwrap());
    batch.put(
        seq_key(b'B', first.until),
        serde_json::to_vec(&cut).unwrap(),
    );
    write(&journal, batch).unwrap();
    drop(journal);
    assert!(
        super::adopt_source_checkpoint(
            &cloud,
            &cloud.sources[0].id,
            &baseline,
            first.until,
            &GENESIS
        )
        .is_err()
    );
    let adopted = super::adopt_source_checkpoint(
        &cloud,
        &cloud.sources[0].id,
        &baseline,
        first.until,
        &first.until_sha256,
    )
    .unwrap();
    assert_eq!(
        adopted["source_sequence"].as_u64(),
        Some(db.latest_sequence_number())
    );
    db.put_cf(
        db.cf_handle(NativeCf::Fills.name()).unwrap(),
        b"changed",
        b"new value",
    )
    .unwrap();
    assert!(
        super::adopt_source_checkpoint(
            &cloud,
            &cloud.sources[0].id,
            &baseline,
            first.until,
            &first.until_sha256
        )
        .is_err()
    );
}

#[test]
fn wal_rejects_new_event_conflicts_and_column_family_replacement_without_advancing() {
    for replace_cf in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (cloud, _) = configs(temp.path());
        let mut db = primary(&cloud.sources[0].rocksdb_path);
        seed(&db);
        let first =
            super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
        if replace_cf {
            db.drop_cf(NativeCf::Fills.name()).unwrap();
            db.create_cf(NativeCf::Fills.name(), &Options::default())
                .unwrap();
        } else {
            let mut batch = WriteBatch::default();
            let cf = db.cf_handle(NativeCf::Events.name()).unwrap();
            batch.put_cf(cf, b"new-event", b"first evidence");
            batch.put_cf(cf, b"new-event", b"conflicting evidence");
            db.write(batch).unwrap();
        }
        let result = super::export::page(
            &cloud,
            &cloud.sources[0].id,
            with_hash(
                request(first.until, None, Some(first.store_id)),
                Some(first.until_sha256),
            ),
        );
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains(if replace_cf {
                "column families"
            } else {
                "immutable"
            }),
            "{error}"
        );
        let journal = open_owned(
            &cloud
                .sync_export
                .as_ref()
                .unwrap()
                .journal_root
                .join(&cloud.sources[0].id),
            "cn_futures_sync_export",
            &cloud.sources[0].id,
            &[],
        )
        .unwrap();
        let head: Value = get(&journal, b"meta/head").unwrap().unwrap();
        assert_eq!(head["cursor"], 3);
    }
}
fn configs(root: &Path) -> (Config, Config) {
    let mut cloud: Config = toml::from_str(include_str!(
        "../../../config/cn-futures-manager.example.toml"
    ))
    .unwrap();
    cloud.secondary_root = root.join("cloud-secondaries");
    cloud.reference_root = None;
    cloud.market_data = None;
    cloud.sources[0].rocksdb_path = root.join("execution");
    cloud.sync_export = Some(ExportSettings {
        bind: "127.0.0.1:0".parse().unwrap(),
        token_env: "NATIVE_SYNC_TEST_TOKEN".into(),
        journal_root: root.join("export-journal"),
        source_ids: vec![cloud.sources[0].id.clone()],
        batch_records: 1,
        batch_bytes: 4096,
    });
    let mut local = cloud.clone();
    local.sync_export = None;
    local.secondary_root = root.join("local-secondaries");
    local.sync = Some(PullSettings {
        enabled: true,
        replica_root: root.join("replicas"),
        poll_secs: 1,
        timeout_secs: 2,
        concurrent_sources: 4,
    });
    local.sources[0].rocksdb_path = root.join("replicas").join(&local.sources[0].id);
    local.sources[0].sync = Some(Remote {
        url: "http://127.0.0.1:18212".into(),
        token_env: "NATIVE_SYNC_TEST_TOKEN".into(),
    });
    cloud.validate().unwrap();
    local.validate().unwrap();
    (cloud, local)
}
fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/yida_native_records.json"
    ))
    .unwrap()
}
fn primary(path: &Path) -> DB {
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    DB::open_cf(&options, path, NativeCf::ALL.map(NativeCf::name)).unwrap()
}
fn seed(db: &DB) {
    let f = fixture();
    let mut fill = f["fill"].clone();
    fill["commission"] = Value::Null;
    let mut state = f["state"].clone();
    state["received_us"] = json!(crate::native_futures::now_us());
    let mut batch = WriteBatch::default();
    for (cf, key, value) in [
        (NativeCf::Events, &b"evidence\x00"[..], &state),
        (NativeCf::Orders, &b"order\x00"[..], &f["order"]),
        (NativeCf::Fills, &b"trade\x00"[..], &fill),
    ] {
        batch.put_cf(
            db.cf_handle(cf.name()).unwrap(),
            key,
            serde_json::to_vec(value).unwrap(),
        );
    }
    db.write(batch).unwrap();
}
fn request(after: u64, until: Option<u64>, store_id: Option<String>) -> Request {
    Request {
        mode: RuntimeMode::CnFutures,
        after,
        after_sha256: GENESIS.into(),
        until,
        store_id,
    }
}
fn with_hash(mut request: Request, hash: Option<String>) -> Request {
    request.after_sha256 = hash.unwrap_or_else(|| GENESIS.into());
    request
}
fn all_bytes(db: &DB) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    NativeCf::ALL
        .into_iter()
        .flat_map(|cf| {
            db.iterator_cf(
                db.cf_handle(cf.name()).unwrap(),
                rocksdb::IteratorMode::Start,
            )
            .map(move |row| {
                let (k, v) = row.unwrap();
                (cf.name().into(), k.to_vec(), v.to_vec())
            })
        })
        .collect()
}
async fn serve(config: Config) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = ExportService::with_token(config, TEST_TOKEN)
        .unwrap()
        .router();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, task)
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap()
}

#[tokio::test]
async fn ctp_http_sync_and_restart_preserve_unknown_fees_and_connection_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, mut local) = configs(temp.path());
    cloud.sources[0].adapter = "ctp".into();
    local.sources[0].adapter = "ctp".into();
    let db = primary(&cloud.sources[0].rocksdb_path);
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/ctp_native_records.json"
    ))
    .unwrap();
    fixture["state"]["received_us"] = json!(crate::native_futures::now_us());
    let mut batch = WriteBatch::default();
    for (cf, key, value) in [
        (NativeCf::Events, &b"evidence\x00"[..], &fixture["state"]),
        (NativeCf::Orders, &b"order\x00"[..], &fixture["order"]),
        (NativeCf::Fills, &b"trade\x00"[..], &fixture["fill"]),
    ] {
        batch.put_cf(
            db.cf_handle(cf.name()).unwrap(),
            key,
            serde_json::to_vec(value).unwrap(),
        );
    }
    db.write(batch).unwrap();
    let original = all_bytes(&db);
    let (url, server) = serve(cloud).await;
    local.sources[0].sync.as_mut().unwrap().url = url;
    let replica = Arc::new(Replica::open(&local, &local.sources[0]).unwrap());
    let state = replica.sync_once(&client(), TEST_TOKEN).await.unwrap();
    assert_eq!(state.committed_cursor, 3);
    assert_eq!(all_bytes(&db), original);
    assert_eq!(all_bytes(&replica.db), original);
    let snapshot =
        reader::read(&local.sources[0], &local.secondary_root.join("ctp"), 10, 30).unwrap();
    assert_eq!(snapshot.status, "observing");
    assert_eq!(snapshot.orders[0].record, fixture["order"]);
    assert_eq!(snapshot.fills[0], fixture["fill"]);
    assert!(snapshot.fills[0]["commission"].is_null());
    let disconnected = json!({"sequence":101,"received_us":crate::native_futures::now_us(),"projection_error":null,"value":{"type":"ctp_event","event":{"kind":2,"error_code":4097}}});
    db.put_cf(
        db.cf_handle(NativeCf::Events.name()).unwrap(),
        b"evidence\x01",
        serde_json::to_vec(&disconnected).unwrap(),
    )
    .unwrap();
    let state = replica.sync_once(&client(), TEST_TOKEN).await.unwrap();
    assert_eq!(state.committed_cursor, 4);
    assert_eq!(all_bytes(&replica.db), all_bytes(&db));
    drop(replica);
    let replica = Arc::new(Replica::open(&local, &local.sources[0]).unwrap());
    let resumed = replica.sync_once(&client(), TEST_TOKEN).await.unwrap();
    assert_eq!(resumed.committed_cursor, 4);
    assert_eq!(resumed.store_id, state.store_id);
    let snapshot =
        reader::read(&local.sources[0], &local.secondary_root.join("ctp"), 10, 30).unwrap();
    assert_eq!(snapshot.status, "disconnected");
    assert_eq!(snapshot.fills[0], fixture["fill"]);
    server.abort();
}

#[tokio::test]
async fn http_bootstrap_incremental_old_order_and_fee_updates_preserve_source_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, mut local) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let before = all_bytes(&db);
    let (url, server) = serve(cloud.clone()).await;
    local.sources[0].sync.as_mut().unwrap().url = url;
    let replica = Arc::new(Replica::open(&local, &local.sources[0]).unwrap());
    assert!(
        reader::read(
            &local.sources[0],
            &local.secondary_root.join("initial"),
            10,
            30
        )
        .unwrap_err()
        .downcast_ref::<ReplicaBusy>()
        .is_some()
    );
    let state = replica.sync_once(&client(), TEST_TOKEN).await.unwrap();
    assert!(state.initialized);
    assert_eq!(state.committed_cursor, 3);
    assert_eq!(
        all_bytes(&db),
        before,
        "export must not modify any execution record"
    );
    assert_eq!(
        all_bytes(&replica.db),
        before,
        "keys, JSON bytes and native identities must be copied exactly"
    );
    let snapshot = reader::read(
        &local.sources[0],
        &local.secondary_root.join(&local.sources[0].id),
        10,
        30,
    )
    .unwrap();
    assert_eq!(snapshot.total_fills, 1);
    assert_eq!(snapshot.total_orders, 1);
    assert_eq!(snapshot.replication.unwrap().committed_cursor, 3);
    let unchanged: Page = client()
        .post(format!(
            "{}/api/native/sync/{}",
            local.sources[0].sync.as_ref().unwrap().url,
            cloud.sources[0].id
        ))
        .bearer_auth(TEST_TOKEN)
        .json(&with_hash(
            request(3, None, state.store_id.clone()),
            state.committed_sha256.clone(),
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(unchanged.changes.is_empty());
    assert_eq!(unchanged.until, 3);
    let mut f = fixture();
    f["order"]["order"]["limit_price"] = json!(115000);
    f["fill"]["commission"] = json!(26.785);
    let old_sequence = f["fill"]["evidence_seq"].clone();
    let mut batch = WriteBatch::default();
    batch.put_cf(
        db.cf_handle(NativeCf::Orders.name()).unwrap(),
        b"order\x00",
        serde_json::to_vec(&f["order"]).unwrap(),
    );
    batch.put_cf(
        db.cf_handle(NativeCf::Fills.name()).unwrap(),
        b"trade\x00",
        serde_json::to_vec(&f["fill"]).unwrap(),
    );
    db.write(batch).unwrap();
    let changed = all_bytes(&db);
    let state = replica.sync_once(&client(), TEST_TOKEN).await.unwrap();
    assert_eq!(state.committed_cursor, 5);
    assert_eq!(all_bytes(&replica.db), changed);
    assert_eq!(all_bytes(&db), changed);
    let snapshot = reader::read(
        &local.sources[0],
        &local.secondary_root.join(&local.sources[0].id),
        10,
        30,
    )
    .unwrap();
    assert_eq!(snapshot.fills[0]["commission"], json!(26.785));
    assert_eq!(snapshot.fills[0]["evidence_seq"], old_sequence);
    assert_eq!(
        snapshot.orders[0].record["order"]["limit_price"],
        json!(115000)
    );
    server.abort();
}

#[test]
fn frozen_pages_and_resume_never_publish_an_incomplete_cut() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, local) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let source = &cloud.sources[0];
    let first = super::export::page(&cloud, &source.id, request(0, None, None)).unwrap();
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    replica.accept(&first).unwrap();
    assert_eq!(replica.status().unwrap().received_cursor, 1);
    assert!(!replica.status().unwrap().initialized);
    let mut changed = fixture()["order"].clone();
    changed["updated_us"] = json!(crate::native_futures::now_us());
    db.put_cf(
        db.cf_handle(NativeCf::Orders.name()).unwrap(),
        b"order\x00",
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    drop(replica);
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    let mut after = replica.status().unwrap().received_cursor;
    while after < first.until {
        let page = super::export::page(
            &cloud,
            &source.id,
            with_hash(
                request(after, Some(first.until), Some(first.store_id.clone())),
                replica.status().unwrap().received_sha256,
            ),
        )
        .unwrap();
        after = page.next;
        replica.accept(&page).unwrap();
        replica.apply().unwrap();
    }
    let copied: Value = serde_json::from_slice(
        &replica
            .db
            .get_cf(
                replica.db.cf_handle(NativeCf::Orders.name()).unwrap(),
                b"order\x00",
            )
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_ne!(
        copied["updated_us"], changed["updated_us"],
        "a frozen cut must not mix later source updates into earlier pages"
    );
    assert!(replica.status().unwrap().initialized);
    let next = super::export::page(
        &cloud,
        &source.id,
        with_hash(
            request(after, None, Some(first.store_id)),
            replica.status().unwrap().committed_sha256,
        ),
    )
    .unwrap();
    assert_eq!(next.changes.len(), 1);
    replica.accept(&next).unwrap();
    replica.apply().unwrap();
    assert_eq!(all_bytes(&replica.db), all_bytes(&db));
}

#[test]
fn applying_crash_recovers_idempotently_and_reader_barrier_rejects_partial_state() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, local) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = 500;
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let page = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    replica.accept(&page).unwrap();
    let mut state = replica.status().unwrap();
    state.applying = true;
    let change = &page.changes[0];
    let (key, value) = change.decode().unwrap();
    let mut batch = WriteBatch::default();
    batch.put(STATE, serde_json::to_vec(&state).unwrap());
    batch.put_cf(
        replica.db.cf_handle(change.column_family.name()).unwrap(),
        key,
        value,
    );
    write(&replica.db, batch).unwrap();
    drop(replica);
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    assert!(
        reader::read(
            &local.sources[0],
            &local.secondary_root.join("reader"),
            10,
            30
        )
        .unwrap_err()
        .downcast_ref::<ReplicaBusy>()
        .is_some()
    );
    replica.apply().unwrap();
    assert_eq!(all_bytes(&replica.db), all_bytes(&db));
    assert!(replica.status().unwrap().initialized);
    let guard = writer_lock(&replica.path).unwrap();
    assert!(
        reader::read(
            &local.sources[0],
            &local.secondary_root.join("reader"),
            10,
            30
        )
        .unwrap_err()
        .downcast_ref::<ReplicaBusy>()
        .is_some()
    );
    drop(guard);
    assert!(
        reader::read(
            &local.sources[0],
            &local.secondary_root.join("reader"),
            10,
            30
        )
        .is_ok()
    );
}

#[test]
fn wrong_mode_account_hash_sequence_and_store_identity_fail_without_advancing() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, local) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = 500;
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let page = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    let mut bad = page.clone();
    bad.mode = RuntimeMode::Crypto;
    assert!(replica.accept(&bad).is_err());
    bad = page.clone();
    bad.source_id = "another_account".into();
    assert!(replica.accept(&bad).is_err());
    bad = page.clone();
    bad.changes[0].value_sha256 = "bad".into();
    assert!(replica.accept(&bad).is_err());
    bad = page.clone();
    bad.changes[0].sequence = 2;
    assert!(replica.accept(&bad).is_err());
    assert_eq!(replica.status().unwrap().received_cursor, 0);
    assert!(all_bytes(&replica.db).is_empty());
    replica.accept(&page).unwrap();
    replica.apply().unwrap();
    bad = page.clone();
    bad.after = page.until;
    bad.next = page.until;
    bad.changes.clear();
    bad.store_id = "a".repeat(64);
    assert!(replica.accept(&bad).is_err());
    let mut bad_request = request(page.until, None, Some(page.store_id));
    bad_request.mode = RuntimeMode::Crypto;
    assert!(super::export::page(&cloud, &cloud.sources[0].id, bad_request).is_err());
    assert_eq!(replica.status().unwrap().committed_cursor, 3);
}

#[tokio::test]
async fn multiple_sources_keep_independent_cursors_and_a_failed_source_cannot_block_another() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, mut local) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let mut other = cloud.sources[0].clone();
    other.id = "independent_live02".into();
    other.rocksdb_path = temp.path().join("missing-execution");
    cloud
        .sync_export
        .as_mut()
        .unwrap()
        .source_ids
        .push(other.id.clone());
    cloud.sources.push(other);
    let (url, server) = serve(cloud).await;
    local.sources[0].sync.as_mut().unwrap().url = url.clone();
    let mut other = local.sources[0].clone();
    other.id = "independent_live02".into();
    other.rocksdb_path = local.sync.as_ref().unwrap().replica_root.join(&other.id);
    local.sources.push(other);
    let good = Arc::new(Replica::open(&local, &local.sources[0]).unwrap());
    let bad = Arc::new(Replica::open(&local, &local.sources[1]).unwrap());
    let c = client();
    let (a, b) = tokio::join!(
        good.sync_once(&c, TEST_TOKEN),
        bad.sync_once(&c, TEST_TOKEN)
    );
    assert!(a.is_ok());
    assert!(b.is_err());
    assert_eq!(good.status().unwrap().committed_cursor, 3);
    assert_eq!(bad.status().unwrap().committed_cursor, 0);
    assert!(bad.status().unwrap().last_error.is_some());
    assert_eq!(all_bytes(&good.db), all_bytes(&db));
    let recovered = primary(&temp.path().join("missing-execution"));
    seed(&recovered);
    let mut fill = fixture()["fill"].clone();
    fill["commission"] = json!(123.0);
    recovered
        .put_cf(
            recovered.cf_handle(NativeCf::Fills.name()).unwrap(),
            b"trade\x00",
            serde_json::to_vec(&fill).unwrap(),
        )
        .unwrap();
    bad.sync_once(&c, TEST_TOKEN).await.unwrap();
    assert_eq!(all_bytes(&bad.db), all_bytes(&recovered));
    assert_eq!(all_bytes(&good.db), all_bytes(&db));
    assert_ne!(
        good.status().unwrap().store_id,
        bad.status().unwrap().store_id
    );
    server.abort();
}

#[tokio::test]
async fn token_required_and_remote_store_replacement_preserves_existing_local_history() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, mut local) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let (url, server) = serve(cloud.clone()).await;
    let c = client();
    let endpoint = format!("{url}/api/native/sync/{}", cloud.sources[0].id);
    let response = c
        .post(&endpoint)
        .json(&request(0, None, None))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!cloud.sync_export.as_ref().unwrap().journal_root.exists());
    local.sources[0].sync.as_mut().unwrap().url = url;
    let replica = Arc::new(Replica::open(&local, &local.sources[0]).unwrap());
    replica.sync_once(&c, TEST_TOKEN).await.unwrap();
    let original = all_bytes(&replica.db);
    let cursor = replica.status().unwrap().committed_cursor;
    drop(db);
    std::fs::rename(
        &cloud.sources[0].rocksdb_path,
        temp.path().join("preserved-original-store"),
    )
    .unwrap();
    let fresh = primary(&cloud.sources[0].rocksdb_path);
    seed(&fresh);
    assert!(replica.sync_once(&c, TEST_TOKEN).await.is_err());
    assert_eq!(replica.status().unwrap().committed_cursor, cursor);
    assert_eq!(all_bytes(&replica.db), original);
    server.abort();
}

#[test]
fn source_deletion_and_unknown_local_database_are_never_accepted_silently() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, local) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    db.delete_cf(db.cf_handle(NativeCf::Fills.name()).unwrap(), b"trade\x00")
        .unwrap();
    assert!(
        super::export::page(
            &cloud,
            &cloud.sources[0].id,
            with_hash(
                request(first.until, None, Some(first.store_id)),
                Some(first.until_sha256)
            )
        )
        .is_err()
    );
    std::fs::create_dir_all(&local.sync.as_ref().unwrap().replica_root).unwrap();
    let unmarked = primary(&local.sources[0].rocksdb_path);
    seed(&unmarked);
    let before = all_bytes(&unmarked);
    drop(unmarked);
    assert!(Replica::open(&local, &local.sources[0]).is_err());
    let options = Options::default();
    let readonly = DB::open_cf_for_read_only(
        &options,
        &local.sources[0].rocksdb_path,
        NativeCf::ALL.map(NativeCf::name),
        false,
    )
    .unwrap();
    assert_eq!(all_bytes(&readonly), before);
}

#[test]
fn toml_modes_and_replica_storage_boundaries_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, mut local) = configs(temp.path());
    local.mode = RuntimeMode::Crypto;
    assert!(local.validate().is_err());
    local.mode = RuntimeMode::CnFutures;
    local.sources[0].rocksdb_path = cloud.sources[0].rocksdb_path.clone();
    assert!(local.validate().is_err());
    local = configs(temp.path()).1;
    local.sync.as_mut().unwrap().replica_root = local.secondary_root.clone();
    assert!(local.validate().is_err());
    let old = include_str!("../../../config/cta-manager.example.toml").replacen(
        "mode = \"crypto\"",
        "",
        1,
    );
    let crypto: crate::config::AppConfig = toml::from_str(&old).unwrap();
    assert_eq!(crypto.mode, RuntimeMode::Crypto);
    let mut crypto = crypto;
    crypto.mode = RuntimeMode::CnFutures;
    assert!(crypto.validate().is_err());
}

#[test]
fn interrupted_export_capture_is_discarded_or_completed_before_serving_a_cut() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, _) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = 500;
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let id = &cloud.sources[0].id;
    let first = super::export::page(&cloud, id, request(0, None, None)).unwrap();
    let journal_path = cloud.sync_export.as_ref().unwrap().journal_root.join(id);
    let mut revised = fixture()["order"].clone();
    revised["updated_us"] = json!(crate::native_futures::now_us());
    let raw = serde_json::to_vec(&revised).unwrap();
    let change = Change::new(4, NativeCf::Orders, b"order\x00", &raw, &first.until_sha256).unwrap();
    for ready in [false, true] {
        let journal = open_owned(&journal_path, "cn_futures_sync_export", id, &[]).unwrap();
        let mut batch = WriteBatch::default();
        batch.put(seq_key(b'J', 4), serde_json::to_vec(&change).unwrap());
        batch.put(b"meta/pending",serde_json::to_vec(&serde_json::json!({"after":3,"until":4,"counts":[1,1,1],"captured_at_us":crate::native_futures::now_us(),"ready":ready})).unwrap());
        write(&journal, batch).unwrap();
        drop(journal);
        let until = if ready { 4 } else { 3 };
        let page = super::export::page(
            &cloud,
            id,
            with_hash(
                request(3, Some(until), Some(first.store_id.clone())),
                Some(first.until_sha256.clone()),
            ),
        )
        .unwrap();
        let journal = open_owned(&journal_path, "cn_futures_sync_export", id, &[]).unwrap();
        if ready {
            assert_eq!(page.changes.len(), 1);
            assert_eq!(page.next, 4);
            assert_eq!(
                journal
                    .get(hash_key(NativeCf::Orders, b"order\x00"))
                    .unwrap()
                    .unwrap(),
                Sha256::digest(&raw).to_vec()
            );
        } else {
            assert!(page.changes.is_empty());
            assert!(journal.get(seq_key(b'J', 4)).unwrap().is_none());
        }
        assert!(journal.get(b"meta/pending").unwrap().is_none());
    }
}

#[test]
fn journal_generation_and_checkpoint_hash_detect_reset_and_rewritten_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, local) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = 500;
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let initial =
        super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    replica.accept(&initial).unwrap();
    replica.apply().unwrap();
    let state = replica.status().unwrap();
    let old = all_bytes(&replica.db);
    let journal_path = cloud
        .sync_export
        .as_ref()
        .unwrap()
        .journal_root
        .join(&cloud.sources[0].id);
    let journal = open_owned(
        &journal_path,
        "cn_futures_sync_export",
        &cloud.sources[0].id,
        &[],
    )
    .unwrap();
    let previous: Change = get(&journal, &seq_key(b'J', 2)).unwrap().unwrap();
    let mut fill = fixture()["fill"].clone();
    fill["commission"] = json!(1);
    let replacement = Change::new(
        3,
        NativeCf::Fills,
        b"trade\x00",
        &serde_json::to_vec(&fill).unwrap(),
        &previous.record_sha256,
    )
    .unwrap();
    let mut batch = WriteBatch::default();
    batch.put(seq_key(b'J', 3), serde_json::to_vec(&replacement).unwrap());
    write(&journal, batch).unwrap();
    drop(journal);
    assert!(
        super::export::page(
            &cloud,
            &cloud.sources[0].id,
            with_hash(
                request(state.committed_cursor, None, state.store_id.clone()),
                state.committed_sha256.clone()
            )
        )
        .is_err()
    );
    assert_eq!(
        replica.status().unwrap().committed_cursor,
        state.committed_cursor
    );
    assert_eq!(all_bytes(&replica.db), old);
    std::fs::rename(&journal_path, temp.path().join("preserved-export-journal")).unwrap();
    let fresh = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    assert_ne!(Some(fresh.store_id.clone()), state.store_id);
    assert!(replica.accept(&fresh).is_err());
    assert_eq!(all_bytes(&replica.db), old);
}

#[test]
fn gzip_pages_are_bounded_after_decompression_and_corrupt_streams_are_rejected() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let (cloud, _) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let page = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let raw = serde_json::to_vec(&page).unwrap();
    let compress = |bytes: &[u8]| {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    };
    let decoded = super::collector::decode_page(compress(&raw), true).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), raw);
    assert!(super::collector::decode_page(b"invalid gzip".to_vec(), true).is_err());
    assert!(
        super::collector::decode_page(compress(&vec![b'a'; MAX_RESPONSE_BYTES + 1]), true).is_err()
    );
}

#[tokio::test]
async fn http_pages_reuse_one_live_journal_and_negotiate_compression() {
    let temp = tempfile::tempdir().unwrap();
    let (cloud, _) = configs(temp.path());
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let (url, server) = serve(cloud.clone()).await;
    let c = client();
    let response = c
        .post(format!("{url}/api/native/sync/{}", cloud.sources[0].id))
        .bearer_auth(TEST_TOKEN)
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .json(&request(0, None, None))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_ENCODING],
        "gzip"
    );
    let page =
        super::collector::decode_page(response.bytes().await.unwrap().to_vec(), true).unwrap();
    let journal_path = cloud
        .sync_export
        .as_ref()
        .unwrap()
        .journal_root
        .join(&cloud.sources[0].id);
    assert!(
        open_owned(
            &journal_path,
            "cn_futures_sync_export",
            &cloud.sources[0].id,
            &[]
        )
        .is_err(),
        "exporter must keep its journal open between pages"
    );
    let next: Page = c
        .post(format!("{url}/api/native/sync/{}", cloud.sources[0].id))
        .bearer_auth(TEST_TOKEN)
        .json(&with_hash(
            request(page.next, Some(page.until), Some(page.store_id)),
            Some(page.next_sha256),
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(next.after, page.next);
    assert_eq!(next.changes.len(), 1);
    let identity = journal_path.join("IDENTITY");
    let original = std::fs::read(&identity).unwrap();
    std::fs::write(&identity, b"replacement-journal").unwrap();
    let replaced = c
        .post(format!("{url}/api/native/sync/{}", cloud.sources[0].id))
        .bearer_auth(TEST_TOKEN)
        .json(&request(0, None, None))
        .send()
        .await
        .unwrap();
    std::fs::write(&identity, original).unwrap();
    assert_eq!(replaced.status(), reqwest::StatusCode::CONFLICT);
    server.abort();
}

#[test]
fn cached_replica_queries_release_apply_barriers_and_read_later_committed_fees() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cloud, local) = configs(temp.path());
    cloud.sync_export.as_mut().unwrap().batch_records = 1000;
    let db = primary(&cloud.sources[0].rocksdb_path);
    seed(&db);
    let first = super::export::page(&cloud, &cloud.sources[0].id, request(0, None, None)).unwrap();
    let replica = Replica::open(&local, &local.sources[0]).unwrap();
    replica.accept(&first).unwrap();
    replica.apply().unwrap();
    let pool = crate::rocks_source::ReaderPool::new(
        &local.secondary_root,
        &[&local.sources[0].rocksdb_path],
    )
    .unwrap();
    let mut cache = reader::Cache::default();
    let mut read = || {
        reader::with_db(&pool, &local.sources[0], |db| {
            cache.read(&local.sources[0], db, 10, 30)
        })
        .unwrap()
    };
    assert!(read().fills[0]["commission"].is_null());
    let mut fill = fixture()["fill"].clone();
    fill["commission"] = json!(42.0);
    db.put_cf(
        db.cf_handle(NativeCf::Fills.name()).unwrap(),
        b"trade\x00",
        serde_json::to_vec(&fill).unwrap(),
    )
    .unwrap();
    let next = super::export::page(
        &cloud,
        &cloud.sources[0].id,
        with_hash(
            request(first.until, None, Some(first.store_id)),
            Some(first.until_sha256),
        ),
    )
    .unwrap();
    replica.accept(&next).unwrap();
    replica.apply().unwrap();
    assert_eq!(read().fills[0]["commission"], 42.0);
}
