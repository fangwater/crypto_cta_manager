use super::*;
use crate::order_config::TargetPosition;
use crate::position_archive::{
    published_account,
    retention::{cutoff, verify_restore},
};

#[tokio::test]
async fn pruning_and_cold_restart_keep_exact_deltas_carry_and_dormant_markets() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Arc::new(
        PositionArchive::open(crate::manager_db::ManagerDb::open(dir.path()).unwrap()).unwrap(),
    );
    let mut config: AppConfig =
        toml::from_str(include_str!("../../config/cta-manager.example.toml")).unwrap();
    config.sources[1].enabled = true;
    let a = config.sources[0].id.clone();
    let b = config.sources[1].id.clone();
    let now = now_us();
    let floor = cutoff(now).0;
    let make = |name: &str, ts, qty| crate::strategy_catalog::PositionStrategy {
        strategy_name: name.into(),
        updated_at_us: ts,
        targets: BTreeMap::from([(
            "BTCUSDT".into(),
            TargetPosition {
                qty,
                signal: 1,
                clear: false,
            },
        )]),
        symbol_order_strategy_overrides: BTreeMap::new(),
    };
    // Same-microsecond records, independent bindings/sources, repeated and tiny
    // target changes. Their exact cumulative addition order is significant.
    for n in 0..100 {
        for (binding, scale) in [("alpha", 0.1), ("beta", -0.3)] {
            let ts = floor - 2_000_000_000 + n * 1_000_000;
            let qty = (n % 7) as f64 / 3.0 + (n % 2) as f64 * 1e-14;
            archive
                .append(
                    ts,
                    &make(binding, ts, qty),
                    vec![],
                    vec![
                        published_account(&a, binding, scale),
                        published_account(&b, binding, 0.7),
                    ],
                )
                .unwrap();
        }
    }
    // Dormant source/symbol: never appears in the retained suffix.
    let mut dormant = make("dormant", floor - 1_000_000_000, 0.45);
    dormant.targets = BTreeMap::from([(
        "ETHUSDT".into(),
        TargetPosition {
            qty: 0.45,
            signal: 0,
            clear: false,
        },
    )]);
    archive
        .append(
            dormant.updated_at_us,
            &dormant,
            vec![],
            vec![published_account(&a, "dormant", 1.0)],
        )
        .unwrap();
    for n in 0..10 {
        let ts = floor + n * MINUTE_US;
        let mut target = make("alpha", ts, if n == 5 { 0.0 } else { n as f64 * 0.11 });
        if n == 5 {
            target.targets.clear();
        }
        archive
            .append(
                ts,
                &target,
                vec![],
                vec![
                    published_account(&a, "alpha", 0.1),
                    published_account(&b, "alpha", 0.7),
                ],
            )
            .unwrap();
    }
    let config = Arc::new(config);
    let cache = TheoreticalTargetCache::new(config.clone(), archive.clone());
    let fees = BTreeMap::from([(a.clone(), 0.0004.into()), (b.clone(), (-0.0001).into())]);
    let sources = vec![a.clone(), b.clone()];
    let models = BTreeMap::from([
        (a.clone(), ExecutionModel::Twap5m),
        (b.clone(), ExecutionModel::Twap5m),
    ]);
    let start = floor + 5 * MINUTE_US;
    let end = floor + 20 * MINUTE_US;
    let before = cache
        .query_inputs(
            fees.clone(),
            end,
            &sources,
            None,
            Some(start),
            None,
            models.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let cost_before = cache
        .query_inputs(
            fees.clone(),
            end,
            &sources,
            Some("alpha"),
            None,
            Some(start - 300_000_000),
            models.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let backup = tempfile::tempdir().unwrap();
    let mut total = 0;
    loop {
        let mut bytes = Vec::new();
        if !archive.export_chunk(&mut bytes, now).unwrap() {
            break;
        }
        let receipt = verify_restore(bytes.as_slice(), &backup.path().join("db")).unwrap();
        total += receipt.count;
        archive.commit_receipt(&receipt, now).unwrap();
    }
    assert_eq!(total, 201);
    let warm = cache
        .query_inputs(
            fees.clone(),
            end,
            &sources,
            None,
            Some(start),
            None,
            models.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(warm.0, before.0);
    assert_eq!(warm.1, before.1);
    assert!(warm.1.keys().any(|key| key.1 == "ETHUSDT"));
    assert!(warm.2.prefixes.iter().any(|(key, _)| key.1 == "ETHUSDT"));
    drop(cache);
    drop(archive);
    let archive = Arc::new(
        PositionArchive::open(crate::manager_db::ManagerDb::open(dir.path()).unwrap()).unwrap(),
    );
    let cold = TheoreticalTargetCache::new(config, archive);
    let after = cold
        .query_inputs(
            fees.clone(),
            end,
            &sources,
            None,
            Some(start),
            None,
            models.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1);
    let cost_after = cold
        .query_inputs(
            fees,
            end,
            &sources,
            Some("alpha"),
            None,
            Some(start - 300_000_000),
            models,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cost_after.0, cost_before.0);
    assert_eq!(cold.status().processed_messages, 10);
}

#[tokio::test]
async fn retained_month_curves_are_identical_with_boundary_only_holdings() {
    use crate::kline::{DAY_US, Kline};
    let root = tempfile::tempdir().unwrap();
    let db = crate::manager_db::ManagerDb::open(&root.path().join("db")).unwrap();
    let trading_config = root.path().join("trade.toml");
    std::fs::write(&trading_config, "local_ips = [\"198.51.100.9\"]").unwrap();
    let store = KlineStore::from_db(
        db.clone(),
        crate::config::KlineConfig {
            enabled: true,
            local_ip: Some("10.1.1.2".parse().unwrap()),
            public_ip: Some("198.51.100.4".parse().unwrap()),
            trade_engine_configs: vec![trading_config],
            ..Default::default()
        },
    )
    .unwrap();
    let now = now_us();
    let start = now.div_euclid(MINUTE_US) * MINUTE_US - DAY_US;
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let bars = (-1..=60)
            .map(|n| {
                let price = 100.0 + n as f64 * 0.3;
                Kline {
                    open_ts_us: start + n * MINUTE_US,
                    open: price,
                    high: price + 1.0,
                    low: price - 1.0,
                    close: price + 0.1,
                    base_volume: (n + 2) as f64,
                    quote_volume: (n + 2) as f64 * price,
                    trades: 10,
                }
            })
            .collect::<Vec<_>>();
        store
            .save_page(symbol, start - MINUTE_US, start + 61 * MINUTE_US, &bars)
            .unwrap();
    }
    let archive = Arc::new(PositionArchive::open(db).unwrap());
    let mut config: AppConfig =
        toml::from_str(include_str!("../../config/cta-manager.example.toml")).unwrap();
    let env = root.path().join("backend.env");
    std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND=ltp\n").unwrap();
    config.sources[0].env_path = Some(env);
    let source = config.sources[0].id.clone();
    let mut target = crate::strategy_catalog::PositionStrategy {
        strategy_name: "alpha".into(),
        updated_at_us: now - 40 * DAY_US,
        targets: BTreeMap::from([
            (
                "BTCUSDT".into(),
                TargetPosition {
                    qty: 1.0,
                    signal: 0,
                    clear: false,
                },
            ),
            (
                "ETHUSDT".into(),
                TargetPosition {
                    qty: -0.3,
                    signal: 0,
                    clear: false,
                },
            ),
        ]),
        symbol_order_strategy_overrides: BTreeMap::new(),
    };
    let accounts = vec![published_account(&source, "alpha", 2.0)];
    archive
        .append(target.updated_at_us, &target, vec![], accounts.clone())
        .unwrap();
    // A separate dormant binding remains untouched after the retention boundary.
    target.strategy_name = "dormant".into();
    archive
        .append(
            target.updated_at_us,
            &target,
            vec![],
            vec![published_account(&source, "dormant", 0.7)],
        )
        .unwrap();
    target.strategy_name = "alpha".into();
    for (minute, qty) in [(2, 1.7), (2, 1.9), (15, -0.1), (35, 0.0)] {
        target.updated_at_us = start + minute * MINUTE_US + 123;
        target.targets.get_mut("BTCUSDT").unwrap().qty = qty;
        archive
            .append(target.updated_at_us, &target, vec![], accounts.clone())
            .unwrap();
    }
    let config = Arc::new(config);
    let cache = TheoreticalTargetCache::new(config.clone(), archive.clone());
    let fees = BTreeMap::from([(source.clone(), 0.0004.into())]);
    let scope = vec![source];
    let mut original = Vec::new();
    for model in [
        ExecutionModel::Twap5m,
        ExecutionModel::Twap3m,
        ExecutionModel::Vwap3m,
        ExecutionModel::InstantTaker,
    ] {
        let curve = load_timeline(
            fees.clone(),
            &cache,
            &store,
            start,
            start + 60 * MINUTE_US,
            &scope,
            1000,
            model,
        )
        .await
        .unwrap();
        assert!(
            curve.unavailable_reason.is_none(),
            "{:?}",
            curve.unavailable_reason
        );
        assert!(!curve.points.is_empty());
        assert_eq!(curve.execution_model, Some(model));
        original.push((model, curve));
    }
    let backup = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    archive.export_chunk(&mut bytes, now).unwrap();
    let receipt = verify_restore(bytes.as_slice(), &backup.path().join("db")).unwrap();
    archive.commit_receipt(&receipt, now).unwrap();
    // A new cache exercises checkpoint loading and cannot use old curve results.
    let cold = TheoreticalTargetCache::new(config, archive);
    for (model, before) in original {
        let after = load_timeline(
            fees.clone(),
            &cold,
            &store,
            start,
            start + 60 * MINUTE_US,
            &scope,
            1000,
            model,
        )
        .await
        .unwrap();
        assert_eq!(after, before);
    }
}
