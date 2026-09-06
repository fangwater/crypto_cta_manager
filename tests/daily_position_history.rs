use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crypto_cta_manager::config::{AppConfig, SourceConfig};
use crypto_cta_manager::model::UNIFORM_ORDERS_CF;
use crypto_cta_manager::nav::SourcePositionSnapshots;
use crypto_cta_manager::position_history::{DailyPositionHistory, PositionHistoryResponse};
use crypto_cta_manager::postgres;
use crypto_cta_manager::snapshot::{PositionSnapshot, SnapshotPosition};
use rocksdb::{ColumnFamilyDescriptor, DB, Options};
use sqlx::{
    AssertSqlSafe,
    postgres::{PgPool, PgPoolOptions},
};
use tempfile::TempDir;

const DAY_US: i64 = 86_400_000_000;
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

struct TestContext {
    pool: PgPool,
    source: SourceConfig,
    rocks: TempDir,
}

impl TestContext {
    async fn new() -> Self {
        let url = std::env::var("CTA_POSITION_HISTORY_TEST_DATABASE_URL")
            .expect("CTA_POSITION_HISTORY_TEST_DATABASE_URL must name the isolated test database");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect position-history test database");
        postgres::migrate(&pool)
            .await
            .expect("migrate test database");

        let rocks = tempfile::tempdir().expect("create temporary RocksDB");
        ensure_uniform_orders(rocks.path());
        let serial = NEXT_SOURCE.fetch_add(1, Ordering::Relaxed);
        let source_id = format!("daily_history_test_{}_{}", std::process::id(), serial);
        let config = config_for(&source_id, rocks.path());
        let source = config.sources[0].clone();
        postgres::register_sources(&pool, &config.sources)
            .await
            .expect("register isolated source");
        Self {
            pool,
            source,
            rocks,
        }
    }

    fn config(&self) -> AppConfig {
        config_from_source(&self.source)
    }

    fn write(&self, event: Event) {
        write_event(self.rocks.path(), event);
    }

    async fn load(
        &self,
        service: &DailyPositionHistory,
        snapshots: SourcePositionSnapshots,
        start_us: i64,
        end_us: i64,
        source_ids: Vec<String>,
        generated_at_us: i64,
    ) -> PositionHistoryResponse {
        service
            .load(
                &self.pool,
                &self.config(),
                &snapshots,
                start_us / 1_000,
                end_us / 1_000,
                source_ids,
                Vec::new(),
                Some(4_000),
                generated_at_us,
            )
            .await
            .expect("load daily position history")
    }
}

fn write_event(path: &Path, event: Event) {
    ensure_uniform_orders(path);
    let options = Options::default();
    let db = DB::open_cf_descriptors(
        &options,
        path,
        vec![ColumnFamilyDescriptor::new(
            UNIFORM_ORDERS_CF,
            Options::default(),
        )],
    )
    .expect("open temporary uniform_orders");
    let cf = db.cf_handle(UNIFORM_ORDERS_CF).expect("uniform_orders CF");
    let key = format!("{:020}", event.recv_ts_us);
    db.put_cf(cf, key.as_bytes(), encode_uniform_order(&event))
        .expect("write uniform order");
}

fn ensure_uniform_orders(path: &Path) {
    let mut options = Options::default();
    options.create_if_missing(true);
    options.create_missing_column_families(true);
    let _db = DB::open_cf_descriptors(
        &options,
        path,
        vec![ColumnFamilyDescriptor::new(
            UNIFORM_ORDERS_CF,
            Options::default(),
        )],
    )
    .expect("open temporary uniform_orders");
}

fn corrupt_record(path: &Path, recv_ts_us: i64) {
    let options = Options::default();
    let db = DB::open_cf_descriptors(
        &options,
        path,
        vec![ColumnFamilyDescriptor::new(
            UNIFORM_ORDERS_CF,
            Options::default(),
        )],
    )
    .expect("open temporary uniform_orders for corruption");
    let cf = db.cf_handle(UNIFORM_ORDERS_CF).expect("uniform_orders CF");
    let key = format!("{:020}", recv_ts_us);
    db.put_cf(cf, key.as_bytes(), b"corrupt")
        .expect("replace old record with corrupt payload");
}

#[derive(Clone, Copy)]
struct Event {
    recv_ts_us: i64,
    update_ts_us: i64,
    side_code: u8,
    price: f64,
    quantity: f64,
}

fn event(recv_ts_us: i64, update_ts_us: i64, side_code: u8, price: f64, quantity: f64) -> Event {
    Event {
        recv_ts_us,
        update_ts_us,
        side_code,
        price,
        quantity,
    }
}

fn config_for(source_id: &str, rocks_path: &Path) -> AppConfig {
    let path = rocks_path.display();
    let text = format!(
        r#"
            [database]
            url_env = "CTA_POSITION_HISTORY_TEST_DATABASE_URL"
            max_connections = 4

            [ingestion]
            poll_interval_secs = 60
            safety_lag_secs = 0
            overlap_secs = 300

            [[sources]]
            id = "{source_id}"
            account = "{source_id}"
            venue = "binance-futures"
            rocksdb_path = "{path}"
            enabled = true
            estimated_fee_rate = 0.0004
            gateway_prefix = "/daily_history_test"
        "#,
    );
    toml::from_str(&text).expect("parse test manager config")
}

fn config_from_source(source: &SourceConfig) -> AppConfig {
    config_for(&source.id, &source.rocksdb_path)
}

fn encode_uniform_order(event: &Event) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&event.recv_ts_us.to_le_bytes());
    out.extend_from_slice(&(7_u16).to_le_bytes());
    out.extend_from_slice(b"BTCUSDT");
    // create, update, signal, submit, local, market, client-order-id.
    for value in [
        event.update_ts_us,
        event.update_ts_us,
        0,
        0,
        event.update_ts_us,
        event.update_ts_us,
        1,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&[1, 1, event.side_code]); // Binance futures, LIMIT, side.
    for value in [event.price, 0.0, event.quantity, event.quantity] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.push(3); // FILLED
    out.extend_from_slice(&0_u32.to_le_bytes());
    out
}

fn snapshots(
    source: &SourceConfig,
    ts_us: i64,
    quantity: f64,
    price: Option<f64>,
) -> SourcePositionSnapshots {
    BTreeMap::from([(
        source.id.clone(),
        PositionSnapshot {
            source_id: source.id.clone(),
            snapshot_ts_us: ts_us,
            positions: vec![SnapshotPosition {
                symbol: "BTCUSDT".into(),
                venue_code: 1,
                quantity,
                reference_price: price,
            }],
        },
    )])
}

fn quantity_at(response: &PositionHistoryResponse, ts_us: i64) -> Option<f64> {
    response
        .symbol_points
        .iter()
        .find(|point| point.ts_ms == ts_us / 1_000 && point.symbol == "BTCUSDT")
        .and_then(|point| point.quantity)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn bootstrap_reconstructs_known_multi_day_quantities() {
    let context = TestContext::new().await;
    context.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 2.0));
    context.write(event(
        DAY_US + DAY_US / 2,
        DAY_US + DAY_US / 2,
        2,
        110.0,
        0.5,
    ));
    context.write(event(
        2 * DAY_US + DAY_US / 2,
        2 * DAY_US + DAY_US / 2,
        1,
        120.0,
        1.0,
    ));

    let response = context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            DAY_US / 4,
            2 * DAY_US + 3 * DAY_US / 4,
            vec![context.source.id.clone()],
            3 * DAY_US,
        )
        .await;

    assert_eq!(quantity_at(&response, DAY_US / 4), None);
    assert_eq!(quantity_at(&response, DAY_US + DAY_US / 4), Some(2.0));
    assert_eq!(quantity_at(&response, 2 * DAY_US + DAY_US / 4), Some(1.5));
    assert_eq!(
        quantity_at(&response, 2 * DAY_US + 3 * DAY_US / 4),
        Some(2.5)
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rollover_survives_restart_and_includes_exact_midnight() {
    let context = TestContext::new().await;
    context.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    let service = DailyPositionHistory::default();
    context
        .load(
            &service,
            BTreeMap::new(),
            0,
            DAY_US,
            vec![context.source.id.clone()],
            DAY_US + 1,
        )
        .await;

    context.write(event(DAY_US + 10, DAY_US, 1, 101.0, 2.0));
    let response = context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            DAY_US,
            DAY_US + 1_000,
            vec![context.source.id.clone()],
            DAY_US + 2_000,
        )
        .await;

    assert_eq!(quantity_at(&response, DAY_US), Some(3.0));
    assert_eq!(quantity_at(&response, DAY_US + 1_000), Some(3.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn incremental_overlap_does_not_apply_a_fill_twice() {
    let context = TestContext::new().await;
    context.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    let service = DailyPositionHistory::default();
    let first = context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US / 2,
            DAY_US / 2 + 1_000,
            vec![context.source.id.clone()],
            DAY_US,
        )
        .await;
    let second = context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US / 2,
            DAY_US / 2 + 1_000,
            vec![context.source.id.clone()],
            DAY_US + 1_000,
        )
        .await;

    assert_eq!(quantity_at(&first, DAY_US / 2 + 1_000), Some(1.0));
    assert_eq!(quantity_at(&second, DAY_US / 2 + 1_000), Some(1.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn late_old_fill_with_new_receive_key_repairs_prior_day_checkpoint() {
    let context = TestContext::new().await;
    context.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    let service = DailyPositionHistory::default();
    context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US,
            2 * DAY_US,
            vec![context.source.id.clone()],
            3 * DAY_US,
        )
        .await;

    // Its receive key is new, but the factual fill belongs to the preceding day.
    context.write(event(3 * DAY_US + 1, 3 * DAY_US / 4, 1, 99.0, 2.0));
    let response = context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US,
            DAY_US + 1_000,
            vec![context.source.id.clone()],
            3 * DAY_US + 2_000,
        )
        .await;

    assert_eq!(quantity_at(&response, DAY_US), Some(3.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn source_scope_preserves_pre_anchor_unknown_mark() {
    let context = TestContext::new().await;
    let anchor = 100 * DAY_US + DAY_US / 2;
    let response = context
        .load(
            &DailyPositionHistory::default(),
            snapshots(&context.source, anchor, 2.0, None),
            0,
            anchor + 1_000,
            Vec::new(),
            anchor + 2_000,
        )
        .await;

    assert_eq!(
        response.selected_source_ids,
        vec![context.source.id.clone()]
    );
    assert_eq!(quantity_at(&response, anchor - 1_000), None);
    let point = response
        .symbol_points
        .iter()
        .find(|point| point.ts_ms == (anchor + 1_000) / 1_000)
        .expect("anchor symbol point");
    assert_eq!(point.quantity, Some(2.0));
    assert_eq!(point.valuation_price, None);
    assert_eq!(point.valuation_source, "unavailable");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_path_does_not_advance_cursor_past_future_history() {
    let context = TestContext::new().await;
    let missing = context.rocks.path().join("not-created-yet");
    let mut source = context.source.clone();
    source.rocksdb_path = missing.clone();
    let config = config_from_source(&source);
    postgres::register_sources(&context.pool, &config.sources)
        .await
        .expect("update source path to missing directory");
    let service = DailyPositionHistory::default();
    service
        .load(
            &context.pool,
            &config,
            &BTreeMap::new(),
            DAY_US / 1_000,
            (DAY_US + 1_000) / 1_000,
            vec![source.id.clone()],
            Vec::new(),
            Some(100),
            2 * DAY_US,
        )
        .await
        .expect("missing source is an empty source");

    write_event(&missing, event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    let response = service
        .load(
            &context.pool,
            &config,
            &BTreeMap::new(),
            DAY_US / 1_000,
            (DAY_US + 1_000) / 1_000,
            vec![source.id.clone()],
            Vec::new(),
            Some(4_000),
            3 * DAY_US,
        )
        .await
        .expect("history is rebuilt after the source directory appears");
    assert_eq!(quantity_at(&response, DAY_US), Some(1.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn failed_cursor_advance_rolls_back_late_checkpoint_patch() {
    let context = TestContext::new().await;
    context.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    let service = DailyPositionHistory::default();
    let initial_cutoff = 3 * DAY_US;
    context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US,
            DAY_US + 1_000,
            vec![context.source.id.clone()],
            initial_cutoff,
        )
        .await;

    context.write(event(3 * DAY_US + 1, DAY_US / 4, 1, 99.0, 2.0));
    let suffix = NEXT_SOURCE.fetch_add(1, Ordering::Relaxed);
    let constraint = format!("daily_history_cursor_{}_{}", std::process::id(), suffix);
    let add_constraint = format!(
        "ALTER TABLE cta_position_history_sources ADD CONSTRAINT {constraint} \
         CHECK (source_id <> '{}' OR scanned_recv_ts_us = {initial_cutoff})",
        context.source.id,
    );
    // Both interpolated values are generated by this test from decimal process
    // identifiers and the locally generated `[a-z0-9_]` source identifier.
    sqlx::query(AssertSqlSafe(add_constraint))
        .execute(&context.pool)
        .await
        .expect("install source-only cursor constraint");

    let failed = service
        .load(
            &context.pool,
            &context.config(),
            &BTreeMap::new(),
            DAY_US / 1_000,
            (DAY_US + 1_000) / 1_000,
            vec![context.source.id.clone()],
            Vec::new(),
            Some(4_000),
            3 * DAY_US + 2_000,
        )
        .await;
    let drop_constraint =
        format!("ALTER TABLE cta_position_history_sources DROP CONSTRAINT {constraint}");
    sqlx::query(AssertSqlSafe(drop_constraint))
        .execute(&context.pool)
        .await
        .expect("remove source-only cursor constraint");
    assert!(
        failed.is_err(),
        "cursor advancement constraint must fail sync"
    );

    let response = context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US,
            DAY_US + 1_000,
            vec![context.source.id.clone()],
            3 * DAY_US + 2_000,
        )
        .await;
    assert_eq!(quantity_at(&response, DAY_US), Some(3.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn corrupt_source_does_not_cancel_healthy_source_sync() {
    let healthy = TestContext::new().await;
    let corrupt = TestContext::new().await;
    healthy.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    corrupt_record(corrupt.rocks.path(), DAY_US / 2);
    let mut config = healthy.config();
    config.sources.push(corrupt.source.clone());
    let cutoff_us = 2 * DAY_US;
    let result = DailyPositionHistory::default()
        .load(
            &healthy.pool,
            &config,
            &BTreeMap::new(),
            DAY_US / 1_000,
            (DAY_US + 1_000) / 1_000,
            vec![healthy.source.id.clone(), corrupt.source.id.clone()],
            Vec::new(),
            Some(4_000),
            cutoff_us,
        )
        .await;
    assert!(result.is_err(), "the corrupt source must surface an error");
    let scanned_recv_ts_us: i64 = sqlx::query_scalar(
        "SELECT scanned_recv_ts_us FROM cta_position_history_sources WHERE source_id = $1",
    )
    .bind(&healthy.source.id)
    .fetch_one(&healthy.pool)
    .await
    .expect("healthy source cursor committed despite corrupt peer");
    assert_eq!(scanned_recv_ts_us, cutoff_us);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn restart_recent_window_avoids_corrupt_ancient_rocks_prefix() {
    let context = TestContext::new().await;
    let ancient_recv = DAY_US / 2;
    context.write(event(ancient_recv, ancient_recv, 1, 100.0, 1.0));
    context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            7 * DAY_US,
            7 * DAY_US + 1_000,
            vec![context.source.id.clone()],
            10 * DAY_US,
        )
        .await;

    corrupt_record(context.rocks.path(), ancient_recv);
    let response = context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            7 * DAY_US,
            7 * DAY_US + 1_000,
            vec![context.source.id.clone()],
            10 * DAY_US + 1_000,
        )
        .await;

    assert_eq!(quantity_at(&response, 7 * DAY_US), Some(1.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn restart_old_window_does_not_scan_raw_records_after_requested_end() {
    let context = TestContext::new().await;
    context.write(event(DAY_US / 2, DAY_US / 2, 1, 100.0, 1.0));
    context.write(event(
        5 * DAY_US + DAY_US / 2,
        5 * DAY_US + DAY_US / 2,
        2,
        110.0,
        0.5,
    ));
    let corrupt_recv = 9 * DAY_US + DAY_US / 2;
    context.write(event(corrupt_recv, corrupt_recv, 1, 120.0, 3.0));
    context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            3 * DAY_US,
            6 * DAY_US + 1_000,
            vec![context.source.id.clone()],
            10 * DAY_US,
        )
        .await;

    corrupt_record(context.rocks.path(), corrupt_recv);
    let response = context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            3 * DAY_US,
            6 * DAY_US + 1_000,
            vec![context.source.id.clone()],
            10 * DAY_US + 1_000,
        )
        .await;

    assert_eq!(quantity_at(&response, 6 * DAY_US + 1_000), Some(0.5));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn empty_existing_rocks_source_later_accepts_its_first_fill() {
    let context = TestContext::new().await;
    let service = DailyPositionHistory::default();
    context
        .load(
            &service,
            BTreeMap::new(),
            0,
            1_000,
            vec![context.source.id.clone()],
            DAY_US,
        )
        .await;

    context.write(event(DAY_US + 1, DAY_US + 1, 1, 100.0, 1.0));
    let response = context
        .load(
            &service,
            BTreeMap::new(),
            DAY_US + 1,
            DAY_US + 2_000,
            vec![context.source.id.clone()],
            2 * DAY_US,
        )
        .await;
    assert_eq!(quantity_at(&response, DAY_US + 2_000), Some(1.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn refresh_only_respects_safety_lag_and_advances_cursor() {
    let context = TestContext::new().await;
    let mut config = context.config();
    config.ingestion.safety_lag_secs = 5;
    let now_us = 20 * DAY_US;
    DailyPositionHistory::default()
        .refresh_only(&context.pool, &config, &BTreeMap::new(), now_us)
        .await
        .expect("refresh-only must not turn its internal range invalid under safety lag");

    let cursor = sqlx::query_scalar::<_, i64>(
        "SELECT scanned_recv_ts_us FROM cta_position_history_sources WHERE source_id = $1",
    )
    .bind(&context.source.id)
    .fetch_one(&context.pool)
    .await
    .expect("refresh-only source cursor");
    assert_eq!(cursor, now_us - 5_000_000);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn bootstrap_uses_configured_overlap_for_its_duplicate_ledger() {
    let context = TestContext::new().await;
    let mut config = context.config();
    config.ingestion.overlap_secs = 600;
    let boundary = 2 * DAY_US;
    context.write(event(boundary - 500 * 1_000_000, DAY_US - 1, 1, 100.0, 1.0));
    let service = DailyPositionHistory::default();
    service
        .load(
            &context.pool,
            &config,
            &BTreeMap::new(),
            DAY_US / 1_000,
            (DAY_US + 1_000) / 1_000,
            vec![context.source.id.clone()],
            Vec::new(),
            Some(4_000),
            boundary,
        )
        .await
        .expect("bootstrap daily history");
    let response = service
        .load(
            &context.pool,
            &config,
            &BTreeMap::new(),
            DAY_US / 1_000,
            (DAY_US + 1_000) / 1_000,
            vec![context.source.id.clone()],
            Vec::new(),
            Some(4_000),
            boundary + 1_000_000,
        )
        .await
        .expect("overlap re-poll");

    assert_eq!(quantity_at(&response, DAY_US), Some(1.0));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn recent_window_keeps_available_source_start_at_original_first_fill() {
    let context = TestContext::new().await;
    let first_fill = DAY_US / 2;
    context.write(event(first_fill, first_fill, 1, 100.0, 1.0));
    let response = context
        .load(
            &DailyPositionHistory::default(),
            BTreeMap::new(),
            7 * DAY_US,
            7 * DAY_US + 1_000,
            vec![context.source.id.clone()],
            10 * DAY_US,
        )
        .await;

    assert_eq!(response.available_sources.len(), 1);
    assert_eq!(
        response.available_sources[0].first_ts_ms,
        first_fill / 1_000
    );
}
