use super::*;
use crate::native_futures::netting::UNALLOCATED;

const DAY: &str = "20261120";
const AT: i64 = 1_795_000_000_000_000;

fn order() -> fixture::Order {
    fixture::Order::new(
        "SHFE:rb2701",
        DAY,
        "yida:20261120:group:3",
        "17",
        Side::Sell,
        Offset::CloseToday,
        3,
        3100.0,
    )
}

fn fill() -> fixture::Fill {
    fixture::Fill {
        trade_id: "  123456".into(),
        lots: 2,
        price: 3101.0,
        commission: Some(3.0),
        order_id: 9001,
    }
}

#[test]
fn lifecycle_share_cross_and_trade_round_trip_with_native_identity() {
    let key = key(AT);
    let Uniform::Order(lifecycle) = decode_uniform(
        key.as_bytes(),
        &order().lifecycle(fixture::NEW, "exec:batch-1", AT - 5, AT),
    )
    .unwrap() else {
        panic!("lifecycle record")
    };
    assert_eq!(
        (
            lifecycle.venue.as_str(),
            lifecycle.symbol.as_str(),
            lifecycle.lots
        ),
        ("SHFE", "rb2701", 3)
    );
    assert_eq!(
        lifecycle.order,
        OrderKey {
            session: "yida:20261120:group:3".into(),
            order_ref: "17".into()
        }
    );
    assert_eq!(lifecycle.offset, Offset::CloseToday);
    assert_eq!(lifecycle.status, OrderStatus::New);
    assert_eq!(lifecycle.from_key, "exec:batch-1");
    assert_eq!(lifecycle.created_us, AT - 5);
    assert_eq!(lifecycle.client_order_id, order().client_order_id());

    let Uniform::Fill(share) = decode_uniform(
        key.as_bytes(),
        &order().share(
            &fill(),
            "alpha",
            1,
            Some(1.5),
            fixture::PARTIALLY_FILLED,
            AT,
        ),
    )
    .unwrap() else {
        panic!("strategy share")
    };
    assert_eq!(share.market(), "SHFE:rb2701");
    assert_eq!(share.signed_lots(), -1);
    assert_eq!(share.fill_lots, Some(2));
    assert_eq!(share.trade_id.as_deref(), Some("  123456"));
    assert_eq!(share.commission_cny, Some(1.5));
    assert!(!share.internal_cross);

    let leg = fixture::cross_leg("SHFE:rb2701", DAY, AT, 0, Side::Buy, "beta", 2, 3100.5, AT);
    let Uniform::Fill(cross) = decode_uniform(key.as_bytes(), &leg).unwrap() else {
        panic!("cross leg")
    };
    assert!(cross.internal_cross && cross.offset.is_none() && cross.commission_cny.is_none());
    assert_eq!((cross.strategy.as_str(), cross.signed_lots()), ("beta", 2));

    let trade = decode_trade(
        key.as_bytes(),
        &order().trade(&fill(), 2, Some(fixture::PARTIALLY_FILLED), AT),
    )
    .unwrap();
    assert_eq!(trade.lots, 2);
    assert_eq!(trade.cumulative_lots, 2);
    assert_eq!(trade.order_id, 9001);
    assert_eq!(trade.order.order_ref, "17");
    assert_eq!(trade.order_status, Some(OrderStatus::PartiallyFilled));
    assert_eq!(trade.client_order_id, lifecycle.client_order_id);
}

#[test]
fn non_domestic_or_malformed_records_are_rejected() {
    let key = key(AT);
    assert!(decode_uniform(b"bad", &order().lifecycle(fixture::NEW, "exec", AT, AT)).is_err());
    let mut crypto = order().trade(&fill(), 2, None, AT);
    crypto.truncate(crypto.len() - 1);
    assert!(decode_trade(key.as_bytes(), &crypto).is_err());
    let mut fractional = fill();
    fractional.price = f64::NAN;
    assert!(decode_trade(key.as_bytes(), &order().trade(&fractional, 2, None, AT)).is_err());
    let oversized = order().share(&fill(), "alpha", 3, None, fixture::FILLED, AT);
    assert!(decode_uniform(key.as_bytes(), &oversized).is_err());
    let anonymous = order().share(&fill(), "", 1, None, fixture::FILLED, AT);
    assert!(decode_uniform(key.as_bytes(), &anonymous).is_err());
}

#[test]
fn strategy_positions_fold_shares_and_crosses_through_the_cut() {
    let temp = tempfile::tempdir().unwrap();
    let db = fixture::open(temp.path());
    assert_eq!(tip(&db, ORDERS).unwrap(), 0);
    let mut buy = order();
    buy.side = Side::Buy;
    buy.offset = Offset::Open;
    fixture::put_order(&db, AT, &buy.lifecycle(fixture::NEW, "exec", AT, AT));
    fixture::put_order(
        &db,
        AT + 1,
        &buy.share(&fill(), "alpha", 1, None, fixture::PARTIALLY_FILLED, AT + 1),
    );
    fixture::put_order(
        &db,
        AT + 2,
        &buy.share(
            &fill(),
            UNALLOCATED,
            1,
            None,
            fixture::PARTIALLY_FILLED,
            AT + 2,
        ),
    );
    for (leg, (side, name)) in [(Side::Buy, "beta"), (Side::Sell, "alpha")]
        .into_iter()
        .enumerate()
    {
        fixture::put_order(
            &db,
            AT + 3 + leg as i64,
            &fixture::cross_leg("SHFE:rb2701", DAY, AT, 0, side, name, 1, 3100.0, AT + 3),
        );
    }
    let through_share = strategy_positions(&db, AT + 2).unwrap();
    assert_eq!(
        through_share,
        BTreeMap::from([(
            "SHFE:rb2701".to_owned(),
            BTreeMap::from([("alpha".to_owned(), 1)])
        )])
    );
    // After the cross alpha is flat and omitted; the remainder is never named.
    assert_eq!(
        strategy_positions(&db, i64::MAX).unwrap(),
        BTreeMap::from([(
            "SHFE:rb2701".to_owned(),
            BTreeMap::from([("beta".to_owned(), 1)])
        )])
    );
    assert_eq!(tip(&db, ORDERS).unwrap(), AT + 4);
}
