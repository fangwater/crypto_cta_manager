//! Assemble native NAV from the Exec trade-log cut and independent market/rate APIs.
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
    for Inventory {
        venue,
        symbol,
        side,
        today,
        yesterday,
    } in inventory
    {
        let key = (venue.clone(), symbol.clone());
        if !prices.contains_key(&key) {
            let feed = market
                .context("持有非零仓位时必须连接分钟行情，不能用本地预算或成交价代替起始计价")?;
            prices.insert(
                key.clone(),
                feed.nav_prices(
                    &venue,
                    &symbol,
                    anchor.snapshot_us,
                    anchor.snapshot_us,
                    false,
                )
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

pub(crate) struct Query<'a> {
    pub start_us: i64,
    pub end_us: i64,
    pub through_us: i64,
    pub max_points: usize,
    pub symbols: &'a [String],
    pub symbol_curves: bool,
    pub interval_us: Option<i64>,
}

pub(crate) async fn timeline(
    anchor: &Anchor,
    fills: Vec<Fill>,
    market: Option<&market_data::Feed>,
    reference: Option<&reference_data::Feed>,
    query: Query<'_>,
) -> Report {
    let Query {
        start_us,
        end_us,
        through_us,
        max_points,
        symbols,
        symbol_curves,
        interval_us,
    } = query;
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
    if fills.iter().any(|f| f.trading_day < anchor.trading_day) {
        return unavailable(
            "起始快照之后入库的成交属于更早的交易日；起始持仓可能已包含该成交，请另建快照",
        );
    }
    let markets = available_markets(anchor, &fills, end_us);
    let selected = match select_markets(&markets, symbols) {
        Ok(selected) => selected,
        Err(error) => return unavailable(&error.to_string()),
    };
    let assemble = async {
        let prices = prices(market, &selected, start_us, end_us).await?;
        let end_us = covered_end(&prices, end_us);
        ensure!(end_us >= start_us, "分钟行情尚未覆盖所选起点；请稍后重试");
        let fills: Vec<_> = fills.into_iter().filter(|f| f.received_us <= end_us).collect();
        // Every traded contract, not only fills without counter fees: the rate
        // model is also shown beside actual fees for comparison.
        let traded: Vec<_> = selected
            .into_iter()
            .filter(|(venue, symbol)| {
                fills.iter().any(|f| {
                    &f.venue == venue && &f.symbol == symbol && f.received_us >= start_us
                        && f.received_us <= end_us
                })
            })
            .collect();
        let mut rates = BTreeMap::new();
        if let Some(feed) = reference {
            let rows = futures_util::future::join_all(traded.iter().map(|(venue, symbol)| {
                feed.history(&anchor.source_id, venue, symbol, start_us, end_us)
            }))
            .await;
            for (market, row) in traded.into_iter().zip(rows) {
                match row {
                    Ok(observations) => { rates.insert(market, observations); }
                    Err(error) => tracing::warn!(source_id=%anchor.source_id,error=?error,"native NAV fee observations unavailable; retain gross NAV"),
                }
            }
        }
        let anchor = anchor.clone();
        let symbols = symbols.to_vec();
        tokio::task::spawn_blocking(move || build_selected(Inputs {
            anchor: &anchor,
            fills: &fills,
            prices: &prices,
            rates: &rates,
            start_us,
            end_us,
            source_through_us: through_us,
            max_points,
        }, &symbols, symbol_curves, interval_us)).await?
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

/// Value only through the minutes the provider has published: a later minute
/// of an in-progress day is pending, not missing. The effective end is reported.
pub(super) fn covered_end(prices: &BTreeMap<Market, MarketPrices>, end_us: i64) -> i64 {
    prices
        .values()
        .filter_map(|p| p.covered_through_us)
        .fold(end_us, i64::min)
}

pub(super) async fn prices(
    market: Option<&market_data::Feed>,
    markets: &BTreeSet<Market>,
    start_us: i64,
    end_us: i64,
) -> Result<BTreeMap<Market, MarketPrices>> {
    ensure!(
        markets.len() <= 64,
        "native NAV market count exceeds budget"
    );
    if markets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let feed = market.context("尚未配置持仓计价所需的 market-replay 行情")?;
    // Concurrent per contract; the feed's own permits bound the provider load.
    let windows = futures_util::future::try_join_all(
        markets
            .iter()
            .map(|(venue, symbol)| feed.nav_prices(venue, symbol, start_us, end_us, false)),
    )
    .await?;
    Ok(markets.iter().cloned().zip(windows).collect())
}
