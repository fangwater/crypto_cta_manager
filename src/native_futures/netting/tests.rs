use super::*;

const MARKET: &str = "SHFE:sn2612";
const AT: i64 = 1_000_000_000;

#[test]
fn rebase_uses_factual_lots_one_strategy_and_a_monotonic_target_revision() {
    let mut old = portfolio(1, -2);
    old.revision = 12;
    old.target_revision = 11;
    let request = Rebase {
        event_id: "fresh-start".into(),
        expected_revision: 12,
        snapshot_evidence_sequence: 20,
        strategy_groups: BTreeMap::from([("test".into(), BTreeSet::from(["a-1h".into()]))]),
        positions: BTreeMap::from([("a-1h".into(), BTreeMap::from([(MARKET.into(), -2)]))]),
        note: "new factual baseline".into(),
    };
    let nav = super::super::nav::Anchor {
        source_id: old.source_id.clone(),
        snapshot_us: AT + 10,
        trading_day: old.anchor.trading_day.clone(),
        evidence_sequence: 20,
        evidence_sha256: "b".repeat(64),
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
    let next = old.rebase(&request, &nav).unwrap();
    assert_eq!(
        (next.revision, next.baseline_revision, next.target_revision),
        (13, 13, 11)
    );
    assert_eq!(
        next.strategy_groups["test"].members,
        BTreeSet::from(["a-1h".into()])
    );
    assert_eq!(next.positions["a-1h"][MARKET].fifo[0].price, 150.0);
    assert_eq!(next.net_targets().unwrap()[MARKET].attributed_lots, -2);
    assert_eq!(next.net_targets().unwrap()[MARKET].residual_lots, 0);
    assert!(!next.execution_plan().unwrap().publishable);
    let mut mismatch = request.clone();
    mismatch
        .positions
        .get_mut("a-1h")
        .unwrap()
        .insert(MARKET.into(), -1);
    assert!(old.rebase(&mismatch, &nav).is_err());
    old.external_demand_open = true;
    assert!(old.rebase(&request, &nav).is_err());
}

fn portfolio(a: i64, b: i64) -> Portfolio {
    let request = Initialize {
        strategy_groups: BTreeMap::from([(
            "test".into(),
            BTreeSet::from(["a-1h".into(), "b-1h".into()]),
        )]),
        contracts: BTreeMap::from([(MARKET.into(), Contract { multiplier: 5.0 })]),
        positions: BTreeMap::from([
            (
                "a-1h".into(),
                BTreeMap::from([(
                    MARKET.into(),
                    InitialPosition {
                        lots: a,
                        price: 100.0,
                    },
                )]),
            ),
            (
                "b-1h".into(),
                BTreeMap::from([(
                    MARKET.into(),
                    InitialPosition {
                        lots: b,
                        price: 100.0,
                    },
                )]),
            ),
        ]),
        snapshot_evidence_sequence: 10,
        note: "explicit test allocation".into(),
    };
    Portfolio::initialize(
        "account_a",
        &request,
        &BTreeMap::from([(MARKET.into(), a + b)]),
        crate::native_futures::evidence::Anchor {
            source_id: "account_a".into(),
            snapshot_us: AT - 10,
            trading_day: "20261117".into(),
            evidence_sequence: 10,
            evidence_sha256: "a".repeat(64),
            store_id: "test-store".into(),
            note: request.note.clone(),
        },
    )
    .unwrap()
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
        quotes: BTreeMap::from([(
            MARKET.into(),
            Quote {
                bid: 109.0,
                ask: 111.0,
                observed_us: AT,
                source: "factual_test_bbo".into(),
            },
        )]),
    }
}
fn factual(lots: i64) -> FactualFill {
    FactualFill {
        identity: "native-trade".into(),
        market: MARKET.into(),
        lots,
        price: 112.0,
        commission_cny: Some(3.0),
        evidence_sequence: 11,
        received_us: AT + 1,
        evidence_sha256: "factual-hash".into(),
    }
}

#[test]
fn scope_extension_preserves_factual_fifo_and_adds_only_flat_targets() {
    let state = portfolio(2, -1);
    let added = "CZCE:SA701";
    let extension = ExtendScope {
        event_id: "add-soda-ash".into(),
        expected_revision: state.revision,
        strategy_group: "test".into(),
        members: BTreeSet::from(["sa-1h".into()]),
        contracts: BTreeMap::from([(added.into(), Contract { multiplier: 20.0 })]),
        snapshot_evidence_sequence: 11,
        note: "verified new contract flat".into(),
    };
    let mut proof = state.anchor.clone();
    proof.snapshot_us = AT;
    proof.evidence_sequence = 11;
    proof.note = extension.note.clone();
    let expanded = state.extend_scope(&extension, &proof).unwrap();
    assert_eq!(expanded.revision, state.revision + 1);
    assert_eq!(expanded.target_revision, state.target_revision);
    assert_eq!(expanded.account[MARKET], state.account[MARKET]);
    assert_eq!(
        expanded.positions["a-1h"][MARKET],
        state.positions["a-1h"][MARKET]
    );
    assert_eq!(expanded.positions["sa-1h"][MARKET].quantity(), 0);
    assert_eq!(expanded.account[added].quantity(), 0);
    assert_eq!(expanded.targets["a-1h"][added], 0);
    assert_eq!(expanded.targets["sa-1h"][added], 0);
    assert!(expanded.extend_scope(&extension, &proof).is_err());
    let (open, _) = state.batch(&batch(&state, 4, -1), AT).unwrap();
    assert!(
        open.extend_scope(
            &ExtendScope {
                expected_revision: open.revision,
                ..extension
            },
            &proof
        )
        .is_err()
    );
}

#[test]
fn scope_extension_can_register_a_product_group_with_an_independent_clock() {
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
        snapshot_evidence_sequence: 11,
        note: "verified new contract flat".into(),
    };
    let mut proof = state.anchor.clone();
    proof.snapshot_us = AT;
    proof.evidence_sequence = 11;
    proof.note = extension.note.clone();
    assert!(
        state
            .extend_scope(
                &ExtendScope {
                    members: BTreeSet::new(),
                    ..extension.clone()
                },
                &proof
            )
            .is_err()
    );

    let extended = state.extend_scope(&extension, &proof).unwrap();
    assert_eq!(
        extended.strategy_groups["test"].members,
        state.strategy_groups["test"].members
    );
    assert_eq!(extended.strategy_groups[group].members, extension.members);
    assert_eq!(
        extended.positions["a-1h"][MARKET],
        state.positions["a-1h"][MARKET]
    );
    let mut request = batch(&extended, 2, -1);
    request.strategy_group = group.into();
    request.decision_us = AT + 1;
    request.strategies = BTreeMap::from([(
        member.into(),
        BTreeMap::from([(MARKET.into(), 0), (market.into(), 1)]),
    )]);
    request.quotes.clear();
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
    assert_eq!(published.targets["b-1h"], extended.targets["b-1h"]);
    assert_eq!(receipt.external_demands[member][market], 1);
    assert!(receipt.internal_crosses.is_empty());
}

#[test]
fn ctaker_lease_is_bound_to_the_active_batch_with_a_native_trading_day() {
    let state = portfolio(0, 0);
    let mut request = batch(&state, 1, 0);
    let (analytical, _) = state.batch(&request, AT).unwrap();
    assert_eq!(analytical.ctaker_lease(), None);

    request.trading_day = Some("20261117".into());
    let (mut live, receipt) = state.batch(&request, AT).unwrap();
    assert!(receipt.execution_plan.publishable);
    live.decision_us = AT + 1;
    assert_eq!(live.ctaker_lease(), Some((AT, AT + CTAKER_LEASE_US)));
}

#[test]
fn position_handoff_has_no_external_order_and_preserves_total_pnl() {
    let state = portfolio(1, 0);
    let (next, receipt) = state.batch(&batch(&state, 0, 1), AT).unwrap();
    assert_eq!(state.positions["a-1h"][MARKET].quantity(), 1);
    assert_eq!(next.positions["a-1h"][MARKET].quantity(), 0);
    assert_eq!(next.positions["b-1h"][MARKET].quantity(), 1);
    assert!(receipt.external_demands.is_empty());
    assert_eq!(receipt.net_targets[MARKET].residual_lots, 0);
    let cross = &receipt.internal_crosses[0];
    assert_eq!(
        (
            &*cross.buyer,
            &*cross.seller,
            cross.lots,
            cross.price,
            cross.fee_cny
        ),
        ("b-1h", "a-1h", 1, 110.0, 0.0)
    );
    assert_eq!(next.positions["a-1h"][MARKET].realized_cny, 50.0);
    assert_eq!(next.account, state.account);
    let strategy_pnl: f64 = next
        .positions
        .values()
        .map(|p| {
            p[MARKET]
                .valued(120.0, 5.0)
                .unwrap()
                .pnl_after_fees_cny
                .unwrap()
        })
        .sum();
    assert_eq!(
        strategy_pnl,
        next.account[MARKET]
            .valued(120.0, 5.0)
            .unwrap()
            .pnl_after_fees_cny
            .unwrap()
    );
}

#[test]
fn offsetting_virtual_openings_keep_the_exchange_account_flat() {
    let state = portfolio(0, 0);
    let (next, receipt) = state.batch(&batch(&state, 1, -1), AT).unwrap();
    assert_eq!(next.account[MARKET].quantity(), 0);
    assert_eq!(next.positions["a-1h"][MARKET].quantity(), 1);
    assert_eq!(next.positions["b-1h"][MARKET].quantity(), -1);
    assert!(receipt.external_demands.is_empty());
}

#[test]
fn unfilled_residual_is_a_demand_and_does_not_advance_positions() {
    let state = portfolio(0, 0);
    let (next, r) = state.batch(&batch(&state, 3, -1), AT).unwrap();
    assert_eq!(r.internal_crosses[0].lots, 1);
    assert_eq!(r.external_demands["a-1h"][MARKET], 2);
    assert_eq!(next.positions["a-1h"][MARKET].quantity(), 1);
    assert_eq!(r.net_targets[MARKET].target_lots, 2);
    assert_eq!(r.net_targets[MARKET].attributed_lots, 0);
    assert_eq!(r.net_targets[MARKET].residual_lots, 2);
    assert!(next.external_demand_open);
    assert!(
        next.batch(&batch(&next, 0, 0), AT + 1)
            .unwrap_err()
            .downcast_ref::<Conflict>()
            .is_some()
    );
    let filled = next
        .fill(&factual(1), &BTreeMap::from([("a-1h".into(), 1)]))
        .unwrap();
    assert_eq!(filled.net_targets().unwrap()[MARKET].residual_lots, 1);
    assert!(filled.external_demand_open);
    assert_eq!(filled.account[MARKET].known_fees_cny, 3.0);
    assert_eq!(filled.positions["a-1h"][MARKET].known_fees_cny, 3.0);
}

#[test]
fn invalid_later_contract_rolls_back_the_whole_pure_plan() {
    let mut state = portfolio(0, 0);
    let other = "SHFE:sn2701";
    state
        .contracts
        .insert(other.into(), Contract { multiplier: 5.0 });
    state.account.insert(other.into(), Position::default());
    for rows in state.positions.values_mut() {
        rows.insert(other.into(), Position::default());
    }
    for rows in state.targets.values_mut() {
        rows.insert(other.into(), 0);
    }
    let before = fingerprint(&state).unwrap();
    let mut request = batch(&state, 1, -1);
    request
        .strategies
        .get_mut("a-1h")
        .unwrap()
        .insert(other.into(), 1);
    request
        .strategies
        .get_mut("b-1h")
        .unwrap()
        .insert(other.into(), -1);
    assert!(state.batch(&request, AT).is_err());
    assert_eq!(fingerprint(&state).unwrap(), before);
}

#[test]
fn stale_missing_crossed_or_future_quotes_are_rejected() {
    let state = portfolio(1, 0);
    for case in 0..5 {
        let mut r = batch(&state, 0, 1);
        match case {
            0 => {
                r.quotes.clear();
            }
            1 => r.quotes.get_mut(MARKET).unwrap().observed_us = AT - 5_000_001,
            2 => r.quotes.get_mut(MARKET).unwrap().observed_us = AT + 1,
            3 => r.quotes.get_mut(MARKET).unwrap().bid = 112.0,
            _ => r.quotes.get_mut(MARKET).unwrap().source.clear(),
        }
        assert!(state.batch(&r, AT).is_err());
    }
    let mut no_cross = batch(&state, 2, 0);
    no_cross.quotes.clear();
    assert!(state.batch(&no_cross, AT).is_ok());
}

#[test]
fn complete_roster_zero_vectors_and_revision_are_required() {
    let state = portfolio(1, 0);
    let mut r = batch(&state, 0, 1);
    r.strategies.remove("b-1h");
    assert!(state.batch(&r, AT).is_err());
    let mut r = batch(&state, 0, 1);
    r.strategies.get_mut("a-1h").unwrap().clear();
    assert!(state.batch(&r, AT).is_err());
    let mut r = batch(&state, 0, 1);
    r.expected_revision = 1;
    assert!(
        state
            .batch(&r, AT)
            .unwrap_err()
            .downcast_ref::<Conflict>()
            .is_some()
    );
    let (next, _) = state.batch(&batch(&state, 0, 1), AT).unwrap();
    let mut stale = batch(&next, 1, 0);
    stale.decision_us = AT;
    assert!(next.batch(&stale, AT + 1).is_err());
}

#[test]
fn factual_quantity_direction_anchor_and_fee_allocation_are_checked() {
    let state = portfolio(0, 0);
    let fill = factual(3);
    let allocation = BTreeMap::from([("a-1h".into(), 1), ("b-1h".into(), 2)]);
    let next = state.fill(&fill, &allocation).unwrap();
    assert_eq!(next.positions["a-1h"][MARKET].known_fees_cny, 1.0);
    assert_eq!(next.positions["b-1h"][MARKET].known_fees_cny, 2.0);
    assert!(
        state
            .fill(&fill, &BTreeMap::from([("a-1h".into(), 2)]))
            .is_err()
    );
    assert!(
        state
            .fill(
                &fill,
                &BTreeMap::from([("a-1h".into(), -1), ("b-1h".into(), 4)])
            )
            .is_err()
    );
    let mut old = fill.clone();
    old.evidence_sequence = 10;
    assert!(state.fill(&old, &allocation).is_err());
    let mut unknown = fill;
    unknown.commission_cny = None;
    let next = state.fill(&unknown, &allocation).unwrap();
    assert!(
        next.account[MARKET]
            .valued(113.0, 5.0)
            .unwrap()
            .pnl_after_fees_cny
            .is_none()
    );
}

#[test]
fn restart_retains_fifo_costs_revision_and_realized_pnl() {
    let state = portfolio(1, 0);
    let (next, _) = state.batch(&batch(&state, 0, 1), AT).unwrap();
    let restored: Portfolio = serde_json::from_slice(&serde_json::to_vec(&next).unwrap()).unwrap();
    assert_eq!(fingerprint(&restored).unwrap(), fingerprint(&next).unwrap());
    assert_eq!(restored.positions["a-1h"][MARKET].realized_cny, 50.0);
}

#[test]
fn exhaustive_small_position_changes_preserve_quantities_and_marked_pnl() {
    for old_a in -2..=2 {
        for old_b in -2..=2 {
            for new_a in -2..=2 {
                for new_b in -2..=2 {
                    let state = portfolio(old_a, old_b);
                    let (next, receipt) = state.batch(&batch(&state, new_a, new_b), AT).unwrap();
                    let residual: i64 = receipt
                        .external_demands
                        .values()
                        .map(|r| r.get(MARKET).copied().unwrap_or(0))
                        .sum();
                    assert_eq!(residual, (new_a + new_b) - (old_a + old_b));
                    let mut filled = next;
                    if residual != 0 {
                        let allocations = receipt
                            .external_demands
                            .iter()
                            .map(|(s, r)| (s.clone(), r[MARKET]))
                            .collect();
                        filled = filled.fill(&factual(residual), &allocations).unwrap();
                    }
                    assert_eq!(filled.positions["a-1h"][MARKET].quantity(), new_a);
                    assert_eq!(filled.positions["b-1h"][MARKET].quantity(), new_b);
                    let total: f64 = filled
                        .positions
                        .values()
                        .map(|p| {
                            p[MARKET]
                                .valued(120.0, 5.0)
                                .unwrap()
                                .pnl_after_fees_cny
                                .unwrap()
                        })
                        .sum();
                    let account = filled.account[MARKET]
                        .valued(120.0, 5.0)
                        .unwrap()
                        .pnl_after_fees_cny
                        .unwrap();
                    assert!(
                        (total - account).abs() < 1e-9,
                        "{old_a},{old_b} -> {new_a},{new_b}: {total} != {account}"
                    );
                }
            }
        }
    }
}

#[test]
fn factual_identity_uses_native_contract_day_side_and_trade_id() {
    let raw: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/yida_native_records.json"
    ))
    .unwrap();
    let fill = FactualFill::from_evidence(&raw["fill"]).unwrap();
    assert_eq!(
        (fill.market.as_str(), fill.lots, fill.commission_cny),
        ("SHFE:cu2612", 1, Some(26.785))
    );
    let mut unknown = raw["fill"].clone();
    unknown["commission_currency"] = serde_json::json!(null);
    assert_eq!(
        FactualFill::from_evidence(&unknown).unwrap().commission_cny,
        None
    );
}

#[test]
fn strategy_groups_publish_complete_owned_batches_and_share_one_account_plan() {
    let mut state = portfolio(0, 0);
    state.strategy_groups.insert(
        "cn-futures-02".into(),
        StrategyGroup {
            members: BTreeSet::from(["other-1h".into()]),
            decision_us: 0,
            held: false,
        },
    );
    state.positions.insert(
        "other-1h".into(),
        BTreeMap::from([(MARKET.into(), Position::default())]),
    );
    state
        .targets
        .insert("other-1h".into(), BTreeMap::from([(MARKET.into(), 0)]));
    let first = batch(&state, 1, -1);
    let (state, receipt) = state.batch(&first, AT).unwrap();
    assert_eq!(receipt.execution_plan.net_targets[MARKET], 0);
    assert!(!receipt.execution_plan.publishable);
    assert_eq!(receipt.internal_crosses.len(), 1);
    let mut second = first.clone();
    second.strategy_group = "cn-futures-02".into();
    second.batch_id = "other-group".into();
    second.expected_revision = state.revision;
    second.strategies = BTreeMap::from([("other-1h".into(), BTreeMap::from([(MARKET.into(), 0)]))]);
    let (next, _) = state.batch(&second, AT).unwrap();
    assert_eq!(next.targets["a-1h"][MARKET], 1);
    assert_eq!(next.targets["b-1h"][MARKET], -1);
    assert_eq!(next.execution_plan().unwrap().net_targets[MARKET], 0);
    let mut wrong = second.clone();
    wrong.expected_revision = next.revision;
    wrong.decision_us += 1;
    wrong
        .strategies
        .insert("a-1h".into(), BTreeMap::from([(MARKET.into(), 20)]));
    assert!(next.batch(&wrong, AT + 1).is_err());

    // A restored group with canceled remainder retains its original audit
    // target, but another group's publication must use its attributed holdings.
    let mut released = next;
    released.strategy_groups.get_mut("test").unwrap().held = true;
    released
        .targets
        .get_mut("a-1h")
        .unwrap()
        .insert(MARKET.into(), 3);
    second.batch_id = "after-release".into();
    second.expected_revision = released.revision;
    second.decision_us += 1;
    second
        .strategies
        .get_mut("other-1h")
        .unwrap()
        .insert(MARKET.into(), 2);
    let (next, receipt) = released.batch(&second, AT + 1).unwrap();
    assert_eq!(next.targets["a-1h"][MARKET], 3);
    assert_eq!(receipt.execution_plan.net_targets[MARKET], 2);
    assert_eq!(receipt.external_demands.len(), 1);
    assert_eq!(receipt.external_demands["other-1h"][MARKET], 2);
}
