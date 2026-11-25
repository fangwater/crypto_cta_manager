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

fn change(state: &Portfolio, id: &str, members: &[&str]) -> MemberChange {
    MemberChange {
        event_id: id.into(),
        expected_revision: state.revision,
        strategy_group: "test".into(),
        members: members.iter().map(|name| (*name).to_owned()).collect(),
        note: "operator retirement".into(),
    }
}

/// A live batch, so plans built afterwards carry a native trading day.
fn live(state: &Portfolio, a: i64, b: i64) -> Portfolio {
    let mut request = batch(state, a, b);
    request.trading_day = Some("20261117".into());
    state.batch(&request, request.decision_us).unwrap().0
}

fn opening_nav(state: &Portfolio, note: &str, at: i64) -> super::super::nav::Anchor {
    super::super::nav::Anchor {
        source_id: state.source_id.clone(),
        snapshot_us: at,
        trading_day: "20261118".into(),
        trades_key: 30,
        store_id: state.anchor.store_id.clone(),
        positions: vec![super::super::nav::InitialPosition {
            venue: "SHFE".into(),
            symbol: "sn2612".into(),
            side: super::super::nav::Side::Buy,
            today: 0,
            yesterday: 2,
            reference_price: 150.0,
            contract_multiplier: 5.0,
        }],
        note: note.into(),
    }
}

#[test]
fn stop_forces_zero_targets_and_issues_a_live_plan_at_event_time() {
    let state = live(&portfolio(2, -1), 2, -1);
    assert_eq!(state.exec_decision_us(), Some(AT));
    let request = change(&state, "halt-b", &["b-1h"]);
    let (next, receipt) = state.stop(&request, AT + 50).unwrap();
    assert_eq!(next.targets["b-1h"][MARKET], 0);
    assert_eq!(next.targets["a-1h"][MARKET], 2);
    assert_eq!(next.stopped, BTreeSet::from(["b-1h".into()]));
    assert_eq!(
        (next.revision, next.target_revision),
        (state.revision + 1, state.revision + 1)
    );
    assert_eq!(next.batch_id.as_deref(), Some("stop:halt-b"));
    // The plan carries its own time and keeps the last live trading day.
    assert_eq!(next.exec_decision_us(), Some(AT + 50));
    assert_eq!(next.target_trading_day.as_deref(), Some("20261117"));
    assert_eq!(receipt.kind, "strategy_stop");
    assert_eq!(receipt.batch_id, "stop:halt-b");
    assert_eq!(receipt.net_targets[MARKET], 2);
    assert_eq!(receipt.execution_plan.target_revision, next.revision);
    assert_eq!(receipt.execution_plan.decision_us, Some(AT + 50));
    assert_eq!(receipt.execution_plan.stopped, next.stopped);
    // Members stay registered; attribution rows are untouched.
    assert!(next.strategy_groups["test"].members.contains("b-1h"));
    assert_eq!(
        next.positions["b-1h"][MARKET],
        state.positions["b-1h"][MARKET]
    );

    let mut stale = request.clone();
    stale.expected_revision += 1;
    assert!(
        state
            .stop(&stale, AT + 50)
            .unwrap_err()
            .downcast_ref::<Conflict>()
            .is_some()
    );
    assert!(
        next.stop(&change(&next, "again", &["b-1h"]), AT + 60)
            .is_err()
    );
    assert!(
        state
            .stop(&change(&state, "x", &["c-1h"]), AT + 50)
            .is_err()
    );
    assert!(state.stop(&change(&state, "x", &[]), AT + 50).is_err());
    let mut silent = change(&state, "x", &["b-1h"]);
    silent.note = " ".into();
    assert!(state.stop(&silent, AT + 50).is_err());
    let mut long = change(&state, &"x".repeat(124), &["b-1h"]);
    assert!(state.stop(&long, AT + 50).is_err());
    long.event_id.truncate(123);
    assert!(state.stop(&long, AT + 50).is_ok());
}

#[test]
fn later_batches_keep_the_full_roster_but_stopped_members_stay_zero() {
    let state = live(&portfolio(0, 0), 1, 1);
    let (stopped, _) = state
        .stop(&change(&state, "halt-b", &["b-1h"]), AT + 50)
        .unwrap();
    let mut request = batch(&stopped, 3, 5);
    request.trading_day = Some("20261118".into());
    request.decision_us = AT + 60;
    let (next, receipt) = stopped.batch(&request, AT + 60).unwrap();
    assert_eq!(next.targets["b-1h"][MARKET], 0);
    assert_eq!(next.targets["a-1h"][MARKET], 3);
    assert_eq!(receipt.net_targets[MARKET], 3);
    assert_eq!(receipt.execution_plan.strategies["b-1h"][MARKET], 0);
    // Batch semantics are unchanged: the plan follows the batch decision.
    assert_eq!(next.exec_decision_us(), Some(AT + 60));
    assert_eq!(next.batch_id.as_deref(), Some("hour-2"));
    let mut partial = request.clone();
    partial.strategies.remove("b-1h");
    assert!(stopped.batch(&partial, AT + 60).is_err());
}

#[test]
fn retirement_requires_a_stop_and_a_flat_exec_ledger() {
    let state = live(&portfolio(2, -1), 2, -1);
    let cut = |s: &Portfolio| later(s, "operator retirement");
    // Not stopped yet.
    let request = change(&state, "drop-b", &["b-1h"]);
    assert!(
        state
            .retire(&request, &cut(&state), &ExecLedger::new(), AT + 50)
            .is_err()
    );
    let (stopped, _) = state
        .stop(&change(&state, "halt-b", &["b-1h"]), AT + 50)
        .unwrap();
    let request = change(&stopped, "drop-b", &["b-1h"]);
    // Exec still holds b's lots: refused with a conflict.
    let error = stopped
        .retire(&request, &cut(&stopped), &ledger(2, -1), AT + 60)
        .unwrap_err();
    assert!(error.downcast_ref::<Conflict>().is_some(), "{error}");
    // Another strategy's lots do not block it.
    let (next, receipt) = stopped
        .retire(&request, &cut(&stopped), &ledger(2, 0), AT + 60)
        .unwrap();
    assert!(!next.targets.contains_key("b-1h"));
    assert_eq!(
        next.strategy_groups["test"].members,
        BTreeSet::from(["a-1h".into()])
    );
    assert!(next.stopped.is_empty());
    assert_eq!(next.retired, BTreeSet::from(["b-1h".into()]));
    // The anchor allocation keeps b's row, so the account still reconciles.
    assert_eq!(next.positions["b-1h"][MARKET].quantity(), -1);
    next.check().unwrap();
    assert_eq!(next.target_revision, next.revision);
    assert_eq!(next.batch_id.as_deref(), Some("retire:drop-b"));
    assert_eq!(next.exec_decision_us(), Some(AT + 60));
    assert_eq!(receipt.kind, "strategy_retire");
    assert_eq!(receipt.removed_group, None);
    assert_eq!(receipt.cut, Some(cut(&stopped)));
    assert_eq!(
        receipt.execution_plan.strategies.keys().collect::<Vec<_>>(),
        ["a-1h"]
    );
    // A cut of another execution store cannot prove the ledger flat.
    let foreign = Anchor {
        store_id: "replacement-store".into(),
        ..cut(&stopped)
    };
    assert!(
        stopped
            .retire(&request, &foreign, &ExecLedger::new(), AT + 60)
            .is_err()
    );
    // Retired names cannot come back into this account.
    let extension = ExtendScope {
        event_id: "re-add".into(),
        expected_revision: next.revision,
        strategy_group: "test".into(),
        members: BTreeSet::from(["b-1h".into()]),
        contracts: BTreeMap::new(),
        note: "re-add".into(),
    };
    let proof = Anchor {
        snapshot_us: AT + 100,
        ..later(&next, "re-add")
    };
    assert!(
        next.extend_scope(&extension, &proof, &ExecLedger::new())
            .is_err()
    );
}

#[test]
fn retiring_every_member_removes_the_group_and_leaves_a_valid_empty_plan() {
    let state = live(&portfolio(0, 0), 1, -1);
    let (stopped, _) = state
        .stop(&change(&state, "halt", &["a-1h", "b-1h"]), AT + 50)
        .unwrap();
    assert_eq!(stopped.net_targets().unwrap()[MARKET], 0);
    let request = change(&stopped, "drop", &["a-1h", "b-1h"]);
    let (empty, receipt) = stopped
        .retire(
            &request,
            &later(&stopped, &request.note),
            &ExecLedger::new(),
            AT + 60,
        )
        .unwrap();
    assert!(empty.strategy_groups.is_empty() && empty.targets.is_empty());
    assert_eq!(empty.last_strategy_group, None);
    assert_eq!(receipt.removed_group.as_deref(), Some("test"));
    // Contracts stay registered; the account target is flat.
    assert_eq!(
        empty.net_targets().unwrap(),
        Quantities::from([(MARKET.into(), 0)])
    );
    let plan = empty.execution_plan().unwrap();
    assert!(plan.strategies.is_empty());
    assert_eq!(plan.target_revision, empty.revision);
    // Readiness no longer needs the removed group.
    assert_eq!(empty.exec_decision_us(), Some(AT + 60));
    empty.check().unwrap();
    // Batches for the removed group are rejected; the account can grow again.
    assert!(empty.batch(&batch(&empty, 0, 0), AT + 61).is_err());
    let extension = ExtendScope {
        event_id: "fresh-group".into(),
        expected_revision: empty.revision,
        strategy_group: "next".into(),
        members: BTreeSet::from(["c-1h".into()]),
        contracts: BTreeMap::new(),
        note: "new strategy".into(),
    };
    let proof = Anchor {
        snapshot_us: AT + 100,
        ..later(&empty, "new strategy")
    };
    let grown = empty
        .extend_scope(&extension, &proof, &ExecLedger::new())
        .unwrap();
    assert_eq!(grown.targets["c-1h"][MARKET], 0);
    // Persisted state round-trips with the new fields.
    let restored: Portfolio =
        serde_json::from_value(serde_json::to_value(&grown).unwrap()).unwrap();
    assert_eq!(
        fingerprint(&restored).unwrap(),
        fingerprint(&grown).unwrap()
    );
}

#[test]
fn legacy_state_without_plan_time_derives_readiness_from_the_last_group() {
    let state = live(&portfolio(0, 0), 1, 0);
    let mut value = serde_json::to_value(&state).unwrap();
    let object = value.as_object_mut().unwrap();
    assert!(object.remove("plan_decision_us").is_some());
    assert!(!object.contains_key("stopped") && !object.contains_key("retired"));
    let legacy: Portfolio = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.plan_decision_us, None);
    assert_eq!(legacy.exec_decision_us(), Some(AT));
}

#[test]
fn rebase_after_retirement_uses_the_reduced_roster() {
    let state = live(&portfolio(0, 0), 0, 0);
    let (stopped, _) = state
        .stop(&change(&state, "halt-b", &["b-1h"]), AT + 50)
        .unwrap();
    let request = change(&stopped, "drop-b", &["b-1h"]);
    let (retired, _) = stopped
        .retire(
            &request,
            &later(&stopped, &request.note),
            &ExecLedger::new(),
            AT + 60,
        )
        .unwrap();
    let note = "baseline after retirement";
    let rebase = |state: &Portfolio, groups: BTreeMap<String, BTreeSet<String>>, owner: &str| {
        let at = AT + 1_000;
        let request = Rebase {
            event_id: "fresh".into(),
            expected_revision: state.revision,
            strategy_groups: groups,
            positions: BTreeMap::from([(owner.into(), BTreeMap::from([(MARKET.into(), 2)]))]),
            trading_day: "20261118".into(),
            inventory: vec![],
            note: note.into(),
        };
        let cut = Anchor {
            snapshot_us: at,
            ..later(state, note)
        };
        let exec = if owner == UNALLOCATED {
            ExecLedger::new()
        } else {
            BTreeMap::from([(MARKET.into(), Quantities::from([(owner.into(), 2)]))])
        };
        state.rebase(&request, &opening_nav(state, note, at), cut, &exec)
    };
    let single = BTreeMap::from([("test".into(), BTreeSet::from(["a-1h".into()]))]);
    // The former group set no longer matches.
    let former = BTreeMap::from([(
        "test".into(),
        BTreeSet::from(["a-1h".into(), "b-1h".into()]),
    )]);
    assert!(rebase(&retired, former, "a-1h").is_err());
    let next = rebase(&retired, single.clone(), "a-1h").unwrap();
    assert_eq!(next.targets.keys().collect::<Vec<_>>(), ["a-1h"]);
    assert_eq!(next.retired, retired.retired);
    assert_eq!(next.target_trading_day, retired.target_trading_day);
    assert_eq!(next.target_revision, retired.target_revision);
    assert_eq!(next.positions["a-1h"][MARKET].fifo[0].price, 150.0);

    // A stopped member keeps a zero target across a rebase.
    let (halted, _) = retired
        .stop(&change(&retired, "halt-a", &["a-1h"]), AT + 70)
        .unwrap();
    let next = rebase(&halted, single, "a-1h").unwrap();
    assert_eq!(next.stopped, BTreeSet::from(["a-1h".into()]));
    assert_eq!(next.targets["a-1h"][MARKET], 0);

    // With every member retired, the account rebases into an empty roster.
    let request = change(&halted, "drop-a", &["a-1h"]);
    let (empty, _) = halted
        .retire(
            &request,
            &later(&halted, &request.note),
            &ExecLedger::new(),
            AT + 80,
        )
        .unwrap();
    let next = rebase(&empty, BTreeMap::new(), UNALLOCATED).unwrap();
    assert!(next.strategy_groups.is_empty() && next.targets.is_empty());
    assert_eq!(next.positions[UNALLOCATED][MARKET].quantity(), 2);
    next.check().unwrap();
}
