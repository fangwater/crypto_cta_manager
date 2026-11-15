use super::*;
use crate::kline::{DAY_US, Kline};
use crate::manager_db::ManagerDb;
use crate::order_config::TargetPosition;
use crate::position_archive::{
    ArchivedFactualPosition, ArchivedSourcePositions, published_account,
};

// The oracle parses the original JSON and directly feeds the existing theory
// model. It cannot accidentally validate itself by reading the migrated store.
#[tokio::test]
async fn json_migration_keeps_all_priced_curves_fees_and_acquisition_inputs_exact() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy");
    let db = ManagerDb::open(&path).unwrap();
    let trade = root.path().join("trade.toml");
    std::fs::write(&trade, "local_ips = [\"198.51.100.9\"]").unwrap();
    let kline_config = crate::config::KlineConfig {
        enabled: true,
        local_ip: Some("10.1.1.2".parse().unwrap()),
        public_ip: Some("198.51.100.4".parse().unwrap()),
        trade_engine_configs: vec![trade],
        ..Default::default()
    };
    let marks = KlineStore::from_db(db.clone(), kline_config.clone()).unwrap();
    let start = now_us().div_euclid(MINUTE_US) * MINUTE_US - DAY_US;
    let end = start + 60 * MINUTE_US;
    for (symbol, offset) in [("BTCUSDT", 0.0), ("ETHUSDC", 7.0)] {
        let bars = (-31..=80)
            .map(|n| {
                let p = 100.0 + offset + n as f64 * 0.31;
                let v = (n + 33) as f64;
                Kline {
                    open_ts_us: start + n * MINUTE_US,
                    open: p,
                    high: p + 1.0,
                    low: p - 1.0,
                    close: p + 0.13,
                    base_volume: v,
                    quote_volume: v * p,
                    trades: 10,
                }
            })
            .collect::<Vec<_>>();
        marks
            .save_page(symbol, start - 31 * MINUTE_US, end + 21 * MINUTE_US, &bars)
            .unwrap();
    }
    let mut config: AppConfig =
        toml::from_str(include_str!("../../config/cta-manager.example.toml")).unwrap();
    config.sources[1].enabled = true;
    let env = root.path().join("backend.env");
    std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND=ltp\n").unwrap();
    for source in config.sources.iter_mut().take(2) {
        source.env_path = Some(env.clone());
    }
    let ids = vec![config.sources[0].id.clone(), config.sources[1].id.clone()];
    let config = Arc::new(config);
    let oracle_archive = Arc::new(
        PositionArchive::open(ManagerDb::open(&root.path().join("oracle")).unwrap()).unwrap(),
    );
    let oracle = TheoreticalTargetCache::new(config.clone(), oracle_archive);
    let cf = db
        .db()
        .cf_handle(crate::manager_db::POSITION_UPDATES_CF)
        .unwrap();
    for (i, (minute, quantity)) in [
        (-20, 0.12345678901234567),
        (-5, 0.7),
        (0, 0.9),
        (2, 1.7),
        (2, 1.9),
        (15, -0.1),
        (35, 0.0),
        (50, 0.33),
    ]
    .into_iter()
    .enumerate()
    {
        for (j, name) in ["alpha", "beta"].into_iter().enumerate() {
            let ts = start + minute * MINUTE_US + 123;
            let qty = quantity * (j + 1) as f64;
            let mut strategy = crate::strategy_catalog::PositionStrategy {
                strategy_name: name.into(),
                updated_at_us: ts,
                targets: BTreeMap::from([
                    (
                        "BTCUSDT".into(),
                        TargetPosition::new(qty, if i % 2 == 0 { 1 } else { -1 }).unwrap(),
                    ),
                    (
                        "ETHUSDC".into(),
                        TargetPosition::new(-qty * 0.3, 0).unwrap(),
                    ),
                ]),
                symbol_order_strategy_overrides: BTreeMap::new(),
            };
            if i == 6 {
                strategy.targets.get_mut("BTCUSDT").unwrap().clear = true;
            }
            if i == 7 && j == 1 {
                strategy.targets.remove("ETHUSDC");
            }
            let mut accounts = Vec::new();
            for (n, source) in ids.iter().enumerate() {
                let mut account = published_account(
                    source,
                    format!("{name}-binding"),
                    0.7 + (n + i) as f64 * 0.11,
                );
                account.theoretical_fee_rate = Some(-0.00001);
                account.taker_fee_rate = Some(0.0004);
                account
                    .theoretical_fee_rates_by_quote
                    .insert("USDC".into(), 0.00013 + i as f64 * 0.000001);
                account
                    .taker_fee_rates_by_quote
                    .insert("USDC".into(), 0.00037);
                if n == 0 {
                    let mut projected = strategy.targets.clone();
                    for target in projected.values_mut() {
                        target.qty *= 0.91;
                    }
                    account.target_projection = Some(crate::target_proxy::TargetProjection {
                        rules_fetched_at_us: ts - 1,
                        targets: projected,
                        matched: BTreeMap::new(),
                        filtered: BTreeMap::new(),
                    });
                }
                accounts.push(account);
            }
            let message = PositionUpdateMsg {
                msg_type: "position_update".into(),
                schema_version: 4,
                received_at_us: ts,
                seq: (2 * i + j) as u32,
                strategy,
                published_accounts: accounts,
                factual_positions: vec![ArchivedSourcePositions {
                    source_id: ids[0].clone(),
                    snapshot_ts_ms: ts / 1000 + 91,
                    position_ready: i % 2 == 0,
                    positions: (0..64)
                        .map(|n| ArchivedFactualPosition {
                            symbol: format!("S{n:03}USDT"),
                            qty: n as f64 * 0.01 + i as f64 * 1e-14,
                            usdt: if n % 2 == 0 { Some(-0.0) } else { None },
                        })
                        .collect(),
                }],
            };
            let key = crate::manager_db::encode_seq_key(ts, message.seq).unwrap();
            let original = serde_json::to_vec(&message).unwrap();
            let parsed: PositionUpdateMsg = serde_json::from_slice(&original).unwrap();
            ingest_target(&config, &mut oracle.state.lock().unwrap(), &parsed).unwrap();
            db.db().put_cf(&cf, key, original).unwrap();
        }
    }
    oracle.ready.store(true, Ordering::Release);
    let fees = BTreeMap::from([
        (ids[0].clone(), 0.0007.into()),
        (ids[1].clone(), 0.0009.into()),
    ]);
    let mut expected = Vec::new();
    let mut acquisition = Vec::new();
    for model in [
        ExecutionModel::Twap5m,
        ExecutionModel::Twap3m,
        ExecutionModel::Vwap3m,
        ExecutionModel::InstantTaker,
    ] {
        let curve = load_timeline(fees.clone(), &oracle, &marks, start, end, &ids, 1000, model)
            .await
            .unwrap();
        assert!(
            !curve.loading && curve.unavailable_reason.is_none(),
            "{:?}",
            curve.unavailable_reason
        );
        assert!(!curve.points.is_empty());
        expected.push((model, curve));
        let models = BTreeMap::from([(ids[0].clone(), model), (ids[1].clone(), model)]);
        for strategy in [None, Some("alpha"), Some("beta")] {
            let inputs = oracle
                .query_inputs(
                    fees.clone(),
                    end,
                    &ids,
                    strategy,
                    None,
                    Some(start - 300_000_000),
                    models.clone(),
                )
                .await
                .unwrap()
                .unwrap();
            acquisition.push((model, strategy, inputs.0, inputs.1));
        }
    }
    drop(cf);
    drop(marks);
    drop(db);
    crate::position_archive::migration::migrate_json(
        &path,
        &root.path().join("original-backup"),
        true,
    )
    .unwrap();
    let db = ManagerDb::open(&path).unwrap();
    let marks = KlineStore::from_db(db.clone(), kline_config).unwrap();
    let archive = Arc::new(PositionArchive::open(db).unwrap());
    let cold = TheoreticalTargetCache::new(config, archive);
    for (model, before) in expected {
        let after = load_timeline(fees.clone(), &cold, &marks, start, end, &ids, 1000, model)
            .await
            .unwrap();
        assert_eq!(after, before, "priced curve changed for {model:?}");
    }
    for (model, strategy, deltas, carry) in acquisition {
        let models = BTreeMap::from([(ids[0].clone(), model), (ids[1].clone(), model)]);
        let after = cold
            .query_inputs(
                fees.clone(),
                end,
                &ids,
                strategy,
                None,
                Some(start - 300_000_000),
                models,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.0, deltas,
            "acquisition inputs changed for {model:?}/{strategy:?}"
        );
        assert_eq!(after.1, carry);
    }
}
