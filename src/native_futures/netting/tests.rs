use super::*;

const MARKET: &str = "SHFE:sn2612";
const AT: i64 = 1_000_000_000;

fn ledger(a: i64, b: i64) -> ExecLedger {
    let row: Quantities = [("a-1h", a), ("b-1h", b)]
        .into_iter()
        .filter(|(_, lots)| *lots != 0)
        .map(|(name, lots)| (name.to_owned(), lots))
        .collect();
    if row.is_empty() {
        ExecLedger::new()
    } else {
        BTreeMap::from([(MARKET.into(), row)])
    }
}

fn initial(lots: i64) -> BTreeMap<String, InitialPosition> {
    BTreeMap::from([(MARKET.into(), InitialPosition { lots, price: 100.0 })])
}

fn request(a: i64, b: i64, unallocated: i64) -> Initialize {
    Initialize {
        strategy_groups: BTreeMap::from([(
            "test".into(),
            BTreeSet::from(["a-1h".into(), "b-1h".into()]),
        )]),
        contracts: BTreeMap::from([(MARKET.into(), Contract { multiplier: 5.0 })]),
        positions: BTreeMap::from([
            ("a-1h".into(), initial(a)),
            ("b-1h".into(), initial(b)),
            (UNALLOCATED.into(), initial(unallocated)),
        ]),
        note: "explicit test allocation".into(),
    }
}

fn anchor(note: &str) -> Anchor {
    Anchor {
        source_id: "account_a".into(),
        snapshot_us: AT - 10,
        orders_key: 10,
        store_id: "test-store".into(),
        note: note.into(),
    }
}

/// A later cut of the same store.
fn later(state: &Portfolio, note: &str) -> Anchor {
    Anchor {
        snapshot_us: AT,
        orders_key: state.anchor.orders_key + 1,
        note: note.into(),
        ..state.anchor.clone()
    }
}

fn portfolio(a: i64, b: i64) -> Portfolio {
    let request = request(a, b, 0);
    Portfolio::initialize("account_a", &request, &ledger(a, b), anchor(&request.note)).unwrap()
}

fn batch(state: &Portfolio, a: i64, b: i64) -> TargetBatch {
    TargetBatch {
        trading_day: None,
        strategy_group: "test".into(),
        signal_times: BTreeMap::new(),
        batch_id: format!("hour-{}", state.revision),
        expected_revision: state.revision,
        decision_us: AT + state.revision,
        strategies: BTreeMap::from([
            ("a-1h".into(), BTreeMap::from([(MARKET.into(), a)])),
            ("b-1h".into(), BTreeMap::from([(MARKET.into(), b)])),
        ]),
    }
}

#[test]
fn anchor_named_strategies_must_equal_exec_records_and_remainder_is_unallocated() {
    let state = portfolio(2, -1);
    assert_eq!(state.positions["a-1h"][MARKET].quantity(), 2);
    assert_eq!(state.positions[UNALLOCATED][MARKET].quantity(), 0);
    assert!(!state.targets.contains_key(UNALLOCATED));

    // Cutover: Exec has no attribution yet, so the account is wholly unallocated.
    let cutover = request(0, 0, 3);
    let state = Portfolio::initialize(
        "account_a",
        &cutover,
        &ExecLedger::new(),
        anchor(&cutover.note),
    )
    .unwrap();
    assert_eq!(state.positions[UNALLOCATED][MARKET].quantity(), 3);
    assert_eq!(state.account[MARKET].quantity(), 3);
    // The anchor must belong to this source and request.
    assert!(
        Portfolio::initialize(
            "account_b",
            &cutover,
            &ExecLedger::new(),
            anchor(&cutover.note)
        )
        .is_err()
    );
    assert!(
        Portfolio::initialize(
            "account_a",
            &cutover,
            &ExecLedger::new(),
            anchor("other note")
        )
        .is_err()
    );

    // A named allocation that Exec does not hold is rejected.
    let claimed = request(3, 0, 0);
    assert!(
        Portfolio::initialize(
            "account_a",
            &claimed,
            &ExecLedger::new(),
            anchor(&claimed.note)
        )
        .is_err()
    );
    // Exec strategies outside the roster are rejected.
    let mut foreign = ledger(1, 0);
    foreign
        .get_mut(MARKET)
        .unwrap()
        .insert("unregistered-1h".into(), 1);
    let partial = request(1, 0, 1);
    assert!(Portfolio::initialize("account_a", &partial, &foreign, anchor(&partial.note)).is_err());
}

#[test]
fn rebase_uses_inventory_marks_exec_lots_and_a_monotonic_target_revision() {
    let mut old = portfolio(1, -2);
    old.revision = 12;
    old.target_revision = 11;
    let request = Rebase {
        event_id: "fresh-start".into(),
        expected_revision: 12,
        strategy_groups: BTreeMap::from([("test".into(), BTreeSet::from(["a-1h".into()]))]),
        positions: BTreeMap::from([(UNALLOCATED.into(), BTreeMap::from([(MARKET.into(), -2)]))]),
        trading_day: "20261117".into(),
        inventory: vec![super::super::nav::Inventory {
            venue: "SHFE".into(),
            symbol: "sn2612".into(),
            side: super::super::nav::Side::Sell,
            today: 0,
            yesterday: 2,
        }],
        note: "new factual baseline".into(),
    };
    let cut = Anchor {
        snapshot_us: AT + 10,
        ..later(&old, &request.note)
    };
    let nav = super::super::nav::Anchor {
        source_id: old.source_id.clone(),
        snapshot_us: AT + 10,
        trading_day: request.trading_day.clone(),
        trades_key: 20,
        store_id: old.anchor.store_id.clone(),
        positions: vec![super::super::nav::InitialPosition {
            venue: "SHFE".into(),
            symbol: "sn2612".into(),
            side: super::super::nav::Side::Sell,
            today: 0,
            yesterday: 2,
            reference_price: 150.0,
            contract_multiplier: 5.0,
        }],
        note: request.note.clone(),
    };
    let next = old
        .rebase(&request, &nav, cut.clone(), &ExecLedger::new())
        .unwrap();
    assert_eq!(next.anchor, cut);
    assert_eq!(
        (next.revision, next.baseline_revision, next.target_revision),
        (13, 13, 11)
    );
    assert_eq!(
        next.strategy_groups["test"].members,
        BTreeSet::from(["a-1h".into()])
    );
    assert_eq!(next.positions[UNALLOCATED][MARKET].fifo[0].price, 150.0);
    assert_eq!(next.net_targets().unwrap()[MARKET], 0);
    let mut mismatch = request.clone();
    mismatch
        .positions
        .get_mut(UNALLOCATED)
        .unwrap()
        .insert(MARKET.into(), -1);
    assert!(
        old.rebase(&mismatch, &nav, cut.clone(), &ExecLedger::new())
            .is_err()
    );
    // Named lots must be Exec's.
    let exec = BTreeMap::from([(MARKET.into(), Quantities::from([("a-1h".into(), -2)]))]);
    assert!(old.rebase(&request, &nav, cut.clone(), &exec).is_err());
    // The cut must be later and from the same store.
    assert!(
        old.rebase(&request, &nav, old.anchor.clone(), &ExecLedger::new())
            .is_err()
    );
    let foreign = Anchor {
        store_id: "replacement-store".into(),
        ..cut
    };
    assert!(
        old.rebase(&request, &nav, foreign, &ExecLedger::new())
            .is_err()
    );
}

#[test]
fn batches_replace_targets_without_waiting_for_fills() {
    let state = portfolio(1, 0);
    let (next, receipt) = state.batch(&batch(&state, 0, 1), AT).unwrap();
    // Exec crosses a -> b itself; Manager only records the new targets.
    assert_eq!(receipt.net_targets[MARKET], 1);
    assert_eq!(receipt.execution_plan.strategies["b-1h"][MARKET], 1);
    assert_eq!(next.positions["a-1h"][MARKET].quantity(), 1);
    // The next decision publishes immediately; nothing is held for settlement.
    let (after, receipt) = next.batch(&batch(&next, 3, -1), AT + 10).unwrap();
    assert_eq!(receipt.net_targets[MARKET], 2);
    // Reference NAV reads stored receipts: net targets are plain lots.
    let stored = serde_json::to_value(&receipt).unwrap();
    assert_eq!(stored["net_targets"][MARKET], serde_json::json!(2));
    assert_eq!(after.target_revision, after.revision);
    let restored: Portfolio = serde_json::from_slice(&serde_json::to_vec(&after).unwrap()).unwrap();
    assert_eq!(
        fingerprint(&restored).unwrap(),
        fingerprint(&after).unwrap()
    );
}

#[test]
fn complete_roster_zero_vectors_revision_and_fresh_decisions_are_required() {
    let state = portfolio(0, 0);
    let mut missing = batch(&state, 1, 0);
    missing.strategies.remove("b-1h");
    assert!(state.batch(&missing, AT).is_err());
    let mut empty = batch(&state, 1, 0);
    empty.strategies.get_mut("b-1h").unwrap().clear();
    assert!(state.batch(&empty, AT).is_err());
    let mut stale = batch(&state, 1, 0);
    stale.expected_revision += 1;
    assert!(state.batch(&stale, AT).is_err());
    let old = batch(&state, 1, 0);
    assert!(state.batch(&old, AT + 121_000_000).is_err());
    let (next, _) = state.batch(&old, AT).unwrap();
    let mut backwards = batch(&next, 1, 0);
    backwards.decision_us = old.decision_us;
    assert!(next.batch(&backwards, AT).is_err());
}

#[test]
fn scope_extension_preserves_anchor_fifo_and_adds_only_flat_targets() {
    let state = portfolio(2, -1);
    let added = "CZCE:SA701";
    let extension = ExtendScope {
        event_id: "add-soda-ash".into(),
        expected_revision: state.revision,
        strategy_group: "test".into(),
        members: BTreeSet::from(["sa-1h".into()]),
        contracts: BTreeMap::from([(added.into(), Contract { multiplier: 20.0 })]),
        note: "verified new contract flat".into(),
    };
    let proof = later(&state, &extension.note);
    let held = ledger(2, -1);
    let expanded = state.extend_scope(&extension, &proof, &held).unwrap();
    assert_eq!(expanded.revision, state.revision + 1);
    assert_eq!(expanded.target_revision, state.target_revision);
    assert_eq!(expanded.account[MARKET], state.account[MARKET]);
    assert_eq!(
        expanded.positions["a-1h"][MARKET],
        state.positions["a-1h"][MARKET]
    );
    assert_eq!(expanded.positions["sa-1h"][MARKET].quantity(), 0);
    assert_eq!(expanded.positions[UNALLOCATED][added].quantity(), 0);
    assert_eq!(expanded.targets["a-1h"][added], 0);
    assert_eq!(expanded.targets["sa-1h"][added], 0);
    assert!(expanded.extend_scope(&extension, &proof, &held).is_err());
    // Exec must not already attribute the new contract or member.
    let mut attributed = held.clone();
    attributed.insert(added.into(), Quantities::from([("a-1h".into(), 1)]));
    assert!(state.extend_scope(&extension, &proof, &attributed).is_err());
    let mut member = held.clone();
    member.get_mut(MARKET).unwrap().insert("sa-1h".into(), 1);
    assert!(state.extend_scope(&extension, &proof, &member).is_err());
    assert!(
        state
            .extend_scope(&extension, &state.anchor, &held)
            .is_err()
    );
    let reserved = ExtendScope {
        event_id: "reserved".into(),
        members: BTreeSet::from([UNALLOCATED.into()]),
        contracts: BTreeMap::new(),
        ..extension
    };
    assert!(state.extend_scope(&reserved, &proof, &held).is_err());
}

#[test]
fn product_groups_keep_independent_clocks_and_share_one_account_plan() {
    let state = portfolio(2, -1);
    let market = "SHFE:rb2701";
    let group = "cn-futures-01-rb";
    let member = "cn-futures-01-rb-1h";
    let extension = ExtendScope {
        event_id: "add-rebar-group".into(),
        expected_revision: state.revision,
        strategy_group: group.into(),
        members: BTreeSet::from([member.into()]),
        contracts: BTreeMap::from([(market.into(), Contract { multiplier: 10.0 })]),
        note: "verified new contract flat".into(),
    };
    let extended = state
        .extend_scope(&extension, &later(&state, &extension.note), &ledger(2, -1))
        .unwrap();
    let mut request = batch(&extended, 2, -1);
    request.strategy_group = group.into();
    request.decision_us = AT + 1;
    request.strategies = BTreeMap::from([(
        member.into(),
        BTreeMap::from([(MARKET.into(), 0), (market.into(), 1)]),
    )]);
    let (published, receipt) = extended.batch(&request, request.decision_us).unwrap();
    assert_eq!(
        published.strategy_groups["test"].decision_us,
        state.strategy_groups["test"].decision_us
    );
    assert_eq!(
        published.strategy_groups[group].decision_us,
        request.decision_us
    );
    assert_eq!(published.targets["a-1h"], extended.targets["a-1h"]);
    assert_eq!(receipt.net_targets[market], 1);
    assert_eq!(receipt.net_targets[MARKET], 1);
    assert_eq!(receipt.execution_plan.strategies.len(), 3);
}

#[test]
fn exec_decision_is_bound_to_the_active_batch_with_a_native_trading_day() {
    let state = portfolio(0, 0);
    let mut request = batch(&state, 1, 0);
    let (analytical, _) = state.batch(&request, AT).unwrap();
    assert_eq!(analytical.exec_decision_us(), None);

    request.trading_day = Some("20261117".into());
    let (mut live, _) = state.batch(&request, AT).unwrap();
    live.decision_us = AT + 1;
    assert_eq!(live.exec_decision_us(), Some(AT));
}

#[test]
fn fifo_positions_realize_against_the_oldest_lot_and_keep_unknown_fees_unknown() {
    let mut position = Position::default();
    position.apply(2, 100.0, 5.0, Some(1.0)).unwrap();
    position.apply(-3, 110.0, 5.0, None).unwrap();
    assert_eq!(position.quantity(), -1);
    assert_eq!(position.realized_cny, 2.0 * 10.0 * 5.0);
    let value = position.valued(120.0, 5.0).unwrap();
    assert_eq!(value.floating_cny, -10.0 * 5.0);
    assert_eq!(value.unknown_fee_fills, 1);
    assert_eq!(value.pnl_after_fees_cny, None);
}
