//! Assemble native NAV from the frozen Exec cut and independent market/rate APIs.
use super::*;
use crate::native_futures::{market_data, reference_data};

pub async fn price_anchor(
    candidate: source::Candidate,
    market: Option<&market_data::Feed>,
) -> Result<Anchor> {
    let source::Candidate {
        mut anchor,
        inventory,
    } = candidate;
    let mut prices = BTreeMap::new();
    for (venue, symbol, side, today, yesterday) in inventory {
        let key = (venue.clone(), symbol.clone());
        if !prices.contains_key(&key) {
            let feed = market
                .context("持有非零仓位时必须连接分钟行情，不能用本地预算或成交价代替起始计价")?;
            prices.insert(
                key.clone(),
                feed.nav_prices(&venue, &symbol, anchor.snapshot_us, anchor.snapshot_us)
                    .await?,
            );
        }
        let market = &prices[&key];
        anchor.positions.push(InitialPosition {
            venue,
            symbol,
            side,
            today,
            yesterday,
            reference_price: market.mark(anchor.snapshot_us)?,
            contract_multiplier: market.multiplier,
        });
    }
    anchor.validate()?;
    Ok(anchor)
}

pub async fn timeline(
    anchor: &Anchor,
    fills: Vec<Fill>,
    market: Option<&market_data::Feed>,
    reference: Option<&reference_data::Feed>,
    start_us: i64,
    end_us: i64,
    through_us: i64,
    max_points: usize,
) -> Report {
    let unavailable = |reason: &str| {
        Report::unavailable(
            &anchor.source_id,
            anchor.snapshot_us,
            start_us,
            end_us,
            through_us,
            reason.into(),
        )
    };
    if end_us > through_us {
        return unavailable("执行端证据尚未覆盖所选结束时刻；请选择已覆盖时段或等待同步追平");
    }
    if fills.iter().any(|f| {
        !f.live_origin || f.received_us < anchor.snapshot_us || f.trading_day < anchor.trading_day
    }) {
        return unavailable(
            "发现起始快照之后才入库的恢复成交，缺少可验证的原始成交时刻；不能按查询回报时间重排历史，可在完整恢复后另建快照继续跟踪",
        );
    }
    let assemble=async {
        let mut markets=BTreeSet::new();
        for p in &anchor.positions {markets.insert((p.venue.clone(),p.symbol.clone()));}
        for f in &fills {markets.insert((f.venue.clone(),f.symbol.clone()));}
        ensure!(markets.len()<=64,"native NAV market count exceeds budget");
        let mut prices=BTreeMap::new();
        let mut rates=BTreeMap::new();
        for (venue,symbol) in markets {
            let feed=market.context("尚未配置持仓计价所需的 market-replay 行情")?;
            prices.insert((venue.clone(),symbol.clone()),feed.nav_prices(&venue,&symbol,start_us,end_us).await?);
            if fills.iter().any(|f|f.venue==venue && f.symbol==symbol && f.received_us>=start_us && f.commission_cny.is_none()) {
                if let Some(feed)=reference {
                    match feed.history(&anchor.source_id,&venue,&symbol,start_us,end_us).await {
                        Ok(observations)=>{rates.insert((venue,symbol),observations);}
                        Err(error)=>tracing::warn!(source_id=%anchor.source_id,error=?error,"native NAV fee observations unavailable; retain gross NAV"),
                    }
                }
            }
        }
        let anchor=anchor.clone();
        tokio::task::spawn_blocking(move ||build(Inputs{anchor:&anchor,fills:&fills,prices:&prices,rates:&rates,start_us,end_us,source_through_us:through_us,max_points})).await?
    }.await;
    match assemble {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(source_id=%anchor.source_id,error=?error,"native NAV evidence/valuation unavailable");
            unavailable(
                "原生持仓／成交证据或已完成分钟行情覆盖不足；无法形成完整净值曲线，请检查起始快照、恢复补报及行情状态",
            )
        }
    }
}
