use super::super::tests::{event, record};
use super::*;

fn fixture() -> (tempfile::TempDir, SourceConfig, State) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exec");
    let env = dir.path().join("exec.env");
    std::fs::write(
        &env,
        "TRADE_ENGINE_EXEC_BACKEND=rapidx\nLTP_PORTFOLIO_ID=123\n",
    )
    .unwrap();
    let source: SourceConfig = toml::from_str(&format!(
        "id='rx'\naccount='rx'\nvenue='binance-futures'\nrocksdb_path='{}'\nenv_path='{}'",
        path.display(),
        env.display()
    ))
    .unwrap();
    let mut options = Options::default();
    options.create_if_missing(true);
    let mut db = DB::open(&options, &path).unwrap();
    for cf in [crate::model::UNIFORM_ORDERS_CF, EXECUTIONS_CF] {
        db.create_cf(cf, &Options::default()).unwrap();
    }
    drop(db);
    let state = State::open(&source, Some(&dir.path().join("scratch"))).unwrap();
    (dir, source, state)
}
fn write(source: &SourceConfig, rows: &[RawRocksRecord]) {
    let db = DB::open_cf(
        &Options::default(),
        &source.rocksdb_path,
        [crate::model::UNIFORM_ORDERS_CF, EXECUTIONS_CF],
    )
    .unwrap();
    let cf = db.cf_handle(EXECUTIONS_CF).unwrap();
    for row in rows {
        db.put_cf(cf, &row.key, &row.value).unwrap();
    }
}
fn events(view: &View) -> im::Vector<Arc<NavEvent>> {
    let mut result = im::Vector::new();
    view.visit(|e| {
        result.push_back(e);
        Ok(())
    })
    .unwrap();
    result.sort_by(|a, b| a.record_key.cmp(&b.record_key));
    result
}

#[test]
fn disk_projection_matches_reference_and_freezes_queries_across_late_lifecycle() {
    let (_dir, source, mut state) = fixture();
    let zero = event(0.0, 0.0, 90_000);
    state.ingest(&zero).unwrap();
    let empty = state.finish(&source, None, false).unwrap().view;
    assert!(events(&empty).is_empty());
    let rows = [
        record("123", "MARGIN", "a", 1.0, 100.0, false),
        record("123", "MARGIN", "a", 1.0, 100.0, true),
        record("123", "MARGIN", "b", 2.0, 103.0, true),
    ];
    write(&source, &rows);
    let update = state.finish(&source, None, true).unwrap();
    assert_eq!(update.events.len(), 2);
    assert!(update.removed.is_empty());
    assert!(events(&empty).is_empty());
    let wanted = BTreeSet::from([order_key(&zero)]);
    let fills = decode_fills(&rows, "123", "BINANCE", &wanted).unwrap();
    let expected = replace_orders(&im::vector![zero.clone()], &wanted, &fills).unwrap();
    assert_eq!(
        events(&update.view),
        expected
            .iter()
            .filter(|e| e.amount_update > 0.0)
            .cloned()
            .collect::<im::Vector<_>>()
    );
    let old = update.view;
    let priced = event(102.0, 3.0, 200_000);
    state.ingest(&priced).unwrap();
    let next = state.finish(&source, None, true).unwrap();
    assert_eq!(next.removed.len(), 2);
    assert_eq!(next.events.len(), 1);
    assert_eq!(events(&old).len(), 2);
    assert_eq!(events(&next.view), im::vector![priced]);
    assert!(
        state
            .finish(&source, None, true)
            .unwrap()
            .changed_from
            .is_none()
    );
}

#[test]
fn missing_evidence_retries_without_losing_dirty_orders_or_advancing_a_view() {
    let (_dir, source, mut state) = fixture();
    state.ingest(&event(0.0, 3.0, 200_000)).unwrap();
    write(&source, &[record("123", "MARGIN", "a", 1.0, 100.0, false)]);
    assert!(state.finish(&source, None, false).is_err());
    assert_eq!(state.generation, 0);
    write(&source, &[record("123", "MARGIN", "b", 2.0, 103.0, false)]);
    let view = state.finish(&source, None, false).unwrap().view;
    assert_eq!(
        events(&view).iter().map(|e| e.amount_update).sum::<f64>(),
        3.0
    );
    let conflict = record("123", "MARGIN", "a", 1.0, 101.0, true);
    write(&source, &[conflict]);
    assert!(state.finish(&source, None, true).is_err());
    assert_eq!(
        events(&view)
            .iter()
            .map(|e| e.price * e.amount_update)
            .sum::<f64>(),
        306.0
    );
}

fn role_line(tx: &str, qty: f64, price: f64, role: &str, timestamp: i64) -> Vec<u8> {
    let mut row=serde_json::to_vec(&serde_json::json!({"message":{"channel":"Trades","data":{
        "portfolioId":"123","exchangeType":"BINANCE","transactionId":tx,
        "orderId":"88","clientOrderId":"7","sym":"BINANCE_MARGIN_FIL_USDT",
        "side":"SELL","quantity":qty.to_string(),"price":price.to_string(),"createAt":timestamp.to_string(),"execType":role
    }}})).unwrap();
    row.push(b'\n');
    row
}

#[test]
fn journal_only_role_arrival_partial_tail_rotation_and_old_window_trigger() {
    use std::io::Write;
    let (_dir, source, mut state) = fixture();
    let original = event(0.0, 3.0, 200_000);
    state.ingest(&original).unwrap();
    write(
        &source,
        &[
            record("123", "MARGIN", "a", 1.0, 100.0, false),
            record("123", "MARGIN", "b", 2.0, 103.0, false),
        ],
    );
    let old = state.finish(&source, None, false).unwrap().view;
    std::fs::create_dir_all(&state.journal).unwrap();
    let file = state.journal.join("a.jsonl");
    let first = role_line("a", 1.0, 100.0, "MAKER", 100);
    let second = role_line("b", 2.0, 103.0, "TAKER", 110);
    std::fs::write(&file, &first).unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    writer.write_all(&second[..second.len() - 1]).unwrap();
    let one = state.finish(&source, Some(105_000), true).unwrap();
    assert!(one.requires_rebuild);
    assert_eq!(
        events(&one.view)[0].fill_liquidity,
        Some(FillLiquidity::Maker)
    );
    assert!(
        events(&old)
            .iter()
            .all(|e| e.fill_liquidity == Some(FillLiquidity::Unknown))
    );
    writer.write_all(b"\n").unwrap();
    let two = state.finish(&source, None, true).unwrap();
    assert_eq!(
        events(&two.view)[1].fill_liquidity,
        Some(FillLiquidity::Taker)
    );
    drop(writer);
    std::fs::write(&file, &second).unwrap();
    let replaced = state.finish(&source, None, true).unwrap();
    assert_eq!(
        events(&replaced.view)[0].fill_liquidity,
        Some(FillLiquidity::Unknown)
    );
    assert_eq!(
        events(&replaced.view)[1].fill_liquidity,
        Some(FillLiquidity::Taker)
    );
}

#[test]
fn mixed_liquidity_matches_reference_without_repricing_aggregate_lifecycle() {
    let (_dir, source, mut state) = fixture();
    let mut original = (*event(100.0, 3.0, 100_000)).clone();
    original.update_ts_us = 100_000;
    let original = Arc::new(original);
    state.ingest(&original).unwrap();
    let a = record("123", "MARGIN", "a", 1.0, 100.0, false);
    let mut b = record("123", "MARGIN", "b", 2.0, 100.0, false);
    let mut value: serde_json::Value = serde_json::from_slice(&b.value).unwrap();
    value["execution"]["timestamp_ms"] = 100.into();
    b.value = serde_json::to_vec(&value).unwrap();
    write(&source, &[a, b]);
    std::fs::create_dir_all(&state.journal).unwrap();
    let mut lines = role_line("a", 1.0, 100.0, "MAKER", 100);
    lines.extend(role_line("b", 2.0, 100.0, "TAKER", 100));
    std::fs::write(state.journal.join("a.jsonl"), lines).unwrap();
    let actual = events(&state.finish(&source, None, false).unwrap().view);
    let expected = [(FillLiquidity::Maker, 1.0), (FillLiquidity::Taker, 2.0)]
        .into_iter()
        .enumerate()
        .map(|(i, (role, qty))| {
            let mut e = with_role(&original, Some(role)).unwrap();
            e.record_key = format!("{}:liquidity:{i:08}", e.record_key);
            e.amount_update = qty;
            Arc::new(e)
        })
        .collect::<im::Vector<_>>();
    assert_eq!(actual, expected.into_iter().collect::<im::Vector<_>>());
}

#[test]
fn one_large_mixed_order_streams_past_the_delta_budget_without_losing_fills() {
    let (_dir, source, mut state) = fixture();
    let mut original = (*event(100.0, 8192.0, 100_000)).clone();
    original.update_ts_us = 100_000;
    state.ingest(&original).unwrap();
    let before = state.finish(&source, None, false).unwrap().view;
    let mut rows = Vec::new();
    let mut journal = Vec::new();
    for i in 0..8192 {
        let tx = format!("t-{i:08}");
        let mut row = record("123", "MARGIN", &tx, 1.0, 100.0, false);
        let mut value: serde_json::Value = serde_json::from_slice(&row.value).unwrap();
        value["execution"]["timestamp_ms"] = 100.into();
        row.value = serde_json::to_vec(&value).unwrap();
        rows.push(row);
        journal.extend(role_line(
            &tx,
            1.0,
            100.0,
            if i % 2 == 0 { "MAKER" } else { "TAKER" },
            100,
        ));
    }
    write(&source, &rows);
    drop(rows);
    std::fs::create_dir_all(&state.journal).unwrap();
    std::fs::write(state.journal.join("a.jsonl"), journal).unwrap();
    let update = state.finish(&source, None, true).unwrap();
    assert!(update.requires_rebuild);
    assert!(update.events.is_empty());
    assert!(update.removed.is_empty());
    let (mut count, mut maker, mut taker, mut quantity) = (0, 0, 0, 0.0);
    update
        .view
        .visit(|e| {
            count += 1;
            quantity += e.amount_update;
            maker += usize::from(e.fill_liquidity == Some(FillLiquidity::Maker));
            taker += usize::from(e.fill_liquidity == Some(FillLiquidity::Taker));
            Ok(())
        })
        .unwrap();
    assert_eq!((count, maker, taker), (8192, 4096, 4096));
    assert_eq!(quantity, 8192.0);
    assert_eq!(events(&before), im::vector![Arc::new(original)]);
}

#[test]
fn projection_generations_keep_live_readers_and_release_unused_old_rows() {
    let (_dir, source, mut state) = fixture();
    let original = event(0.0, 0.0, 90_000);
    state.ingest(&original).unwrap();
    let empty = state.finish(&source, None, false).unwrap().view;
    let mut frozen = None;
    for i in 0..25 {
        write(
            &source,
            &[record(
                "123",
                "MARGIN",
                &format!("t-{i:08}"),
                1.0,
                100.0,
                false,
            )],
        );
        let view = state.finish(&source, None, true).unwrap().view;
        if i == 2 {
            frozen = Some(view.clone());
        }
        let hash = id(&order_key(&original)).unwrap();
        let mut versions = 0;
        state
            .disk
            .visit(&key(b'R', &hash), |_, _| {
                versions += 1;
                Ok(())
            })
            .unwrap();
        assert!(
            versions <= 3,
            "unused projection generations retained: {versions}"
        );
        assert_eq!(events(&view).len(), i + 1);
    }
    assert!(events(&empty).is_empty());
    assert_eq!(events(frozen.as_ref().unwrap()).len(), 3);
}

#[test]
fn raw_trade_role_can_precede_the_lifecycle_identity() {
    let (_dir, source, mut state) = fixture();
    state
        .observe_report(crate::model::TradeUpdateEvent {
            record_key: "00000000000000000100".into(),
            record_ts_us: 100,
            recv_ts_us: 100,
            event_ts_us: 100,
            trade_ts_us: 100,
            symbol: "FILUSDT".into(),
            order_id: 88,
            client_order_id: 7,
            side_code: 2,
            price: 100.0,
            is_maker: true,
            venue_code: 0,
            cumulative_filled_quantity: 1.0,
            status_code: None,
        })
        .unwrap();
    let mut original = (*event(100.0, 1.0, 100)).clone();
    original.fill_liquidity = None;
    state.ingest(&original).unwrap();
    let view = state.finish(&source, None, false).unwrap().view;
    assert_eq!(
        events(&view)[0].report_liquidity,
        Some(FillLiquidity::Maker)
    );
}
