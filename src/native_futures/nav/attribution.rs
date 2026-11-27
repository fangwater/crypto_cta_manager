//! Historical strategy valuation: the Manager attribution anchor plus Exec's
//! strategy fill shares and internal-cross legs in `cn_uniform_orders` after it,
//! valued crypto-style (price × lots × multiplier, commission per record).
use super::*;
use crate::native_futures::{
    config::Source,
    market_data, netting,
    records::{self, ORDERS, Uniform},
    reference_data,
};
use rocksdb::DB;
use serde_json::Value;
use sqlx::PgPool;

type PositionKey = (String, Market);

struct Scope {
    anchor: netting::Anchor,
    multiplier: f64,
}

#[derive(Deserialize)]
struct Extension {
    anchor: netting::Anchor,
    contracts: BTreeMap<String, netting::Contract>,
}

struct Move {
    at: i64,
    market: Market,
    strategy: String,
    /// Signed lots for `strategy`.
    lots: i64,
    price: f64,
    /// The exchange-fill share; `None` for an internal-cross leg (no fee).
    fill: Option<Fill>,
}

pub(crate) struct Replay {
    anchor: netting::Anchor,
    markets: BTreeMap<Market, Scope>,
    positions: BTreeMap<PositionKey, netting::Position>,
    moves: Vec<Move>,
    members: BTreeSet<String>,
}

pub(crate) struct StrategyReport {
    pub report: Report,
    pub strategies: Vec<StrategySummary>,
}

pub(crate) struct StrategySummary {
    pub name: String,
    pub market_count: usize,
    pub fees: Fees,
    pub summary: Point,
    pub points: Vec<Point>,
}

fn market_key(name: &str) -> Result<Market> {
    super::super::validate_market(name)?;
    let (venue, symbol) = name.split_once(':').context("native market scope")?;
    Ok((venue.into(), symbol.into()))
}

impl Replay {
    pub(crate) async fn load(pool: &PgPool, portfolio: netting::Portfolio) -> Result<Self> {
        ensure!(
            portfolio.baseline_revision >= 0
                && (0..20_000).contains(&(portfolio.revision - portfolio.baseline_revision)),
            "native attribution revision budget exceeded"
        );
        let rows: Vec<(i64, String, Value)> = sqlx::query_as(
            "SELECT revision,kind,receipt FROM cta_native_netting_events
             WHERE source_id=$1 AND revision BETWEEN $2 AND $3 ORDER BY revision LIMIT 20001",
        )
        .bind(&portfolio.source_id)
        .bind(portfolio.baseline_revision)
        .bind(portfolio.revision)
        .fetch_all(pool)
        .await?;
        Self::from_receipts(portfolio, rows)
    }

    fn from_receipts(
        portfolio: netting::Portfolio,
        rows: Vec<(i64, String, Value)>,
    ) -> Result<Self> {
        ensure!(
            rows.len() as i64 == portfolio.revision - portfolio.baseline_revision + 1,
            "native attribution history is missing revisions"
        );
        let mut rows = rows.into_iter();
        let (initial_revision, kind, value) =
            rows.next().context("missing attribution anchor receipt")?;
        let initial: netting::Portfolio = serde_json::from_value(value)?;
        ensure!(
            initial_revision == portfolio.baseline_revision
                && kind == "initialize"
                && initial.revision == initial_revision
                && initial.baseline_revision == initial_revision
                && initial.source_id == portfolio.source_id
                && initial.anchor == portfolio.anchor,
            "native attribution initialization receipt conflicts with portfolio"
        );
        let mut replay = Self {
            anchor: initial.anchor,
            markets: BTreeMap::new(),
            positions: BTreeMap::new(),
            moves: vec![],
            // Retired members keep their anchor rows and historical records.
            members: portfolio
                .targets
                .into_keys()
                .chain(portfolio.retired)
                .chain([netting::UNALLOCATED.to_owned()])
                .collect(),
        };
        replay.add_scope(&replay.anchor.clone(), initial.contracts)?;
        for (name, positions) in initial.positions {
            ensure!(
                replay.members.contains(&name),
                "unknown initial attribution strategy"
            );
            for (symbol, position) in positions {
                let key = market_key(&symbol)?;
                ensure!(
                    replay.markets.contains_key(&key),
                    "unknown initial attribution contract"
                );
                if !position.fifo.is_empty() || position.realized_cny != 0.0 {
                    replay.positions.insert((name.clone(), key), position);
                }
            }
        }
        for (offset, (revision, kind, value)) in rows.enumerate() {
            ensure!(
                revision == initial_revision + offset as i64 + 1
                    && value["revision"].as_i64() == Some(revision)
                    && value["source_id"].as_str() == Some(&replay.anchor.source_id),
                "native attribution revision/source mismatch"
            );
            match kind.as_str() {
                "scope_extension" => {
                    let Extension { anchor, contracts } = serde_json::from_value(value)?;
                    replay.add_scope(&anchor, contracts)?;
                }
                // Targets and roster changes never move positions; Exec records do.
                "target_batch" | "strategy_stop" | "strategy_resume" | "strategy_retire" => {}
                _ => anyhow::bail!("unknown native attribution event kind"),
            }
        }

        Ok(replay)
    }

    fn add_scope(
        &mut self,
        anchor: &netting::Anchor,
        contracts: BTreeMap<String, netting::Contract>,
    ) -> Result<()> {
        anchor.validate()?;
        ensure!(
            anchor.source_id == self.anchor.source_id
                && anchor.store_id == self.anchor.store_id
                && anchor.snapshot_us >= self.anchor.snapshot_us
                && anchor.orders_key >= self.anchor.orders_key,
            "attribution scope source/generation mismatch"
        );
        for (market, contract) in contracts {
            ensure!(
                contract.multiplier.is_finite()
                    && contract.multiplier > 0.0
                    && self
                        .markets
                        .insert(
                            market_key(&market)?,
                            Scope {
                                anchor: anchor.clone(),
                                multiplier: contract.multiplier,
                            }
                        )
                        .is_none(),
                "invalid or repeated attribution contract"
            );
        }
        Ok(())
    }

    /// Reads Exec's strategy records after each contract's anchor cut.
    pub(in crate::native_futures) fn read_records(
        &mut self,
        source: &Source,
        db: &DB,
        start: i64,
        end: i64,
        symbols: &[String],
    ) -> Result<()> {
        self.anchor.verify(source, db)?;
        let selected = select_markets(&self.available(end), symbols)?;
        let mut checked = BTreeSet::from([self.anchor.orders_key]);
        for key in &selected {
            let anchor = &self.markets[key].anchor;
            if checked.insert(anchor.orders_key) {
                anchor.verify(source, db)?;
            }
        }
        let mut moves = Vec::new();
        records::scan(db, ORDERS, self.anchor.orders_key, |key, value| {
            let at = records::key_us(key)?;
            if at > end {
                return Ok(false);
            }
            let Uniform::Fill(fill) = records::decode_uniform(key, value)? else {
                return Ok(true);
            };
            let market = market_key(&fill.market())?;
            if !selected.contains(&market) || at <= self.markets[&market].anchor.orders_key {
                return Ok(true);
            }
            ensure!(
                self.members.contains(&fill.strategy),
                "Exec attribution names an unregistered strategy {}",
                fill.strategy
            );
            let lots = fill.signed_lots();
            let share = (!fill.internal_cross)
                .then(|| -> Result<Fill> {
                    Ok(Fill {
                        key_us: at,
                        source_id: source.id.clone(),
                        venue: fill.venue.clone(),
                        symbol: fill.symbol.clone(),
                        trading_day: fill.trading_day.clone(),
                        trade_id: fill.trade_id.clone().context("strategy fill trade id")?,
                        side: fill.side,
                        offset: fill.offset.context("strategy fill offset")?,
                        lots: fill.lots,
                        price: fill.price,
                        received_us: at,
                        commission_cny: fill.commission_cny,
                    })
                })
                .transpose()?;
            moves.push(Move {
                at,
                market,
                strategy: fill.strategy,
                lots,
                price: fill.price,
                fill: share,
            });
            ensure!(
                moves.len() <= 250_000,
                "native attribution movement budget exceeded"
            );
            Ok(true)
        })?;
        ensure!(start >= self.anchor.snapshot_us, "所选起点早于策略归因快照");
        self.moves = moves;
        self.positions.retain(|(_, key), _| selected.contains(key));
        Ok(())
    }

    fn available(&self, end: i64) -> BTreeSet<Market> {
        self.markets
            .iter()
            .filter(|(_, scope)| scope.anchor.snapshot_us <= end)
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) async fn timeline(
        self,
        market: Option<&market_data::Feed>,
        reference: Option<&reference_data::Feed>,
        query: service::Query<'_>,
    ) -> Result<StrategyReport> {
        let available = self.available(query.end_us);
        let selected = select_markets(&available, query.symbols)?;
        let active: BTreeSet<_> = self
            .positions
            .keys()
            .map(|(_, market)| market.clone())
            .chain(self.moves.iter().map(|mv| mv.market.clone()))
            .collect();
        let prices = service::prices(market, &active, query.start_us, query.end_us).await?;
        for (key, price) in &prices {
            ensure!(
                self.markets[key].multiplier == price.multiplier,
                "策略归因乘数与合约行情冲突"
            );
        }
        let mut rates = BTreeMap::new();
        if let Some(feed) = reference {
            // Every traded contract: the rate model is shown beside actual fees.
            let traded: Vec<_> = active
                .into_iter()
                .filter(|(venue, symbol)| {
                    self.moves.iter().any(|mv| {
                        &mv.market.0 == venue
                            && &mv.market.1 == symbol
                            && mv.at >= query.start_us
                            && mv.fill.is_some()
                    })
                })
                .collect();
            let rows = futures_util::future::join_all(traded.iter().map(|(venue, symbol)| {
                feed.history(
                    &self.anchor.source_id,
                    venue,
                    symbol,
                    query.start_us,
                    query.end_us,
                )
            }))
            .await;
            for (market, row) in traded.into_iter().zip(rows) {
                match row {
                    Ok(rows) => {
                        rates.insert(market, rows);
                    }
                    Err(error) => {
                        tracing::warn!(source_id=%self.anchor.source_id,error=?error,"strategy NAV fee observations unavailable")
                    }
                }
            }
        }
        let service::Query {
            start_us,
            end_us,
            through_us,
            max_points,
            symbol_curves,
            interval_us,
            ..
        } = query;
        tokio::task::spawn_blocking(move || {
            self.build(
                &prices,
                &rates,
                &available,
                &selected,
                (
                    start_us,
                    end_us,
                    through_us,
                    max_points,
                    symbol_curves,
                    interval_us,
                ),
            )
        })
        .await?
    }
}

#[derive(Default)]
struct Frame {
    portfolio: Amounts,
    markets: BTreeMap<Market, Amounts>,
    strategies: BTreeMap<String, Amounts>,
    quantities: BTreeMap<Market, (i64, i64)>,
}

fn frame(
    positions: &BTreeMap<PositionKey, netting::Position>,
    prices: &BTreeMap<Market, MarketPrices>,
    at: i64,
) -> Result<Frame> {
    let mut result = Frame::default();
    let mut marks = BTreeMap::new();
    for ((strategy, market), position) in positions {
        let mut values = Amounts {
            realized: position.realized_cny,
            ..Default::default()
        };
        let quantity = position.quantity();
        if quantity != 0 {
            let feed = &prices[market];
            let mark = match marks.entry(market) {
                std::collections::btree_map::Entry::Occupied(slot) => *slot.get(),
                std::collections::btree_map::Entry::Vacant(slot) => *slot.insert(feed.mark(at)?),
            };
            values.floating = position.valued(mark, feed.multiplier)?.floating_cny;
            let lots = result.quantities.entry(market.clone()).or_default();
            if quantity > 0 {
                values.long_value = quantity as f64 * mark * feed.multiplier;
                lots.0 = lots
                    .0
                    .checked_add(quantity)
                    .context("attribution long lots overflow")?;
            } else {
                values.short_value = -(quantity as f64) * mark * feed.multiplier;
                lots.1 = lots
                    .1
                    .checked_sub(quantity)
                    .context("attribution short lots overflow")?;
            }
        }
        result.portfolio += values;
        *result.markets.entry(market.clone()).or_default() += values;
        *result.strategies.entry(strategy.clone()).or_default() += values;
    }
    Ok(result)
}

#[derive(Default)]
struct WindowFees {
    portfolio: Fees,
    markets: BTreeMap<Market, Fees>,
    strategies: BTreeMap<String, Fees>,
}

fn apply(
    positions: &mut BTreeMap<PositionKey, netting::Position>,
    mv: &Move,
    multiplier: f64,
    rates: Option<&Vec<AccountRates>>,
    window: Option<&mut WindowFees>,
) -> Result<()> {
    let position = positions
        .entry((mv.strategy.clone(), mv.market.clone()))
        .or_default();
    // Crosses carry no fee; fees before the window are outside its totals.
    let Some((fill, window)) = mv.fill.as_ref().zip(window) else {
        return position.apply(mv.lots, mv.price, multiplier, Some(0.0));
    };
    let estimated = fill.estimated_fee(multiplier, rates.map(Vec::as_slice))?;
    let rate = fill.rate_fee(multiplier, rates.map(Vec::as_slice))?;
    let volume = fill.price * multiplier * fill.lots as f64;
    window
        .portfolio
        .add(volume, fill.commission_cny, estimated, rate)?;
    window.markets.entry(mv.market.clone()).or_default().add(
        volume,
        fill.commission_cny,
        estimated,
        rate,
    )?;
    window
        .strategies
        .entry(mv.strategy.clone())
        .or_default()
        .add(volume, fill.commission_cny, estimated, rate)?;
    position.apply(
        mv.lots,
        mv.price,
        multiplier,
        fill.commission_cny.or(estimated),
    )
}

struct WindowCurves {
    portfolio: Sampler,
    markets: BTreeMap<Market, Sampler>,
    strategies: BTreeMap<String, Sampler>,
}

impl WindowCurves {
    fn add(
        &mut self,
        at: i64,
        frame: &Frame,
        baseline: &Frame,
        fees: &WindowFees,
        max: usize,
    ) -> Result<()> {
        self.portfolio.add(
            point(at, frame.portfolio, baseline.portfolio, &fees.portfolio)?,
            max,
        );
        let no_fees = Fees::default();
        for (key, curve) in &mut self.markets {
            curve.add(
                point(
                    at,
                    frame.markets.get(key).copied().unwrap_or_default(),
                    baseline.markets.get(key).copied().unwrap_or_default(),
                    fees.markets.get(key).unwrap_or(&no_fees),
                )?,
                max,
            );
        }
        for (name, curve) in &mut self.strategies {
            curve.add(
                point(
                    at,
                    frame.strategies.get(name).copied().unwrap_or_default(),
                    baseline.strategies.get(name).copied().unwrap_or_default(),
                    fees.strategies.get(name).unwrap_or(&no_fees),
                )?,
                max,
            );
        }
        Ok(())
    }
}

impl Replay {
    fn build(
        mut self,
        prices: &BTreeMap<Market, MarketPrices>,
        rates: &BTreeMap<Market, Vec<AccountRates>>,
        available: &BTreeSet<Market>,
        selected: &BTreeSet<Market>,
        (start, end, through, max_points, symbol_curves, interval_us): (
            i64,
            i64,
            i64,
            usize,
            bool,
            Option<i64>,
        ),
    ) -> Result<StrategyReport> {
        ensure!(
            self.anchor.snapshot_us <= start && start <= end && end <= through,
            "strategy NAV window not covered"
        );
        let first = self.moves.partition_point(|mv| mv.at < start);
        for mv in &self.moves[..first] {
            apply(
                &mut self.positions,
                mv,
                self.markets[&mv.market].multiplier,
                None,
                None,
            )?;
        }
        let baseline = frame(&self.positions, prices, start)?;
        let mut activity: BTreeMap<String, BTreeSet<Market>> = BTreeMap::new();
        for ((name, market), position) in &self.positions {
            if position.quantity() != 0 {
                activity
                    .entry(name.clone())
                    .or_default()
                    .insert(market.clone());
            }
        }
        for mv in &self.moves[first..] {
            activity
                .entry(mv.strategy.clone())
                .or_default()
                .insert(mv.market.clone());
        }

        let ticks = evaluation_times(start, end, self.moves[first..].iter().map(|mv| mv.at));
        let active_cells = activity
            .values()
            .map(BTreeSet::len)
            .sum::<usize>()
            .max(self.positions.len());
        ensure!(
            ticks.len() <= 600_000 && ticks.len().saturating_mul(active_cells) <= 12_000_000,
            "native strategy NAV replay budget exceeded"
        );
        let tick_count = ticks
            .iter()
            .filter(|&&at| sample_tick(at, end, interval_us))
            .count()
            + usize::from(start < end);
        let sampler = || Sampler::new(tick_count, max_points);
        let mut fees = WindowFees::default();
        let mut curves = WindowCurves {
            portfolio: sampler(),
            markets: selected
                .iter()
                .filter(|_| symbol_curves)
                .map(|key| (key.clone(), sampler()))
                .collect(),
            strategies: activity
                .keys()
                .map(|name| (name.clone(), sampler()))
                .collect(),
        };
        if start < end {
            curves.add(start, &baseline, &baseline, &fees, max_points)?;
        }
        let mut next = first;
        let mut terminal = None;
        for tick in ticks {
            while let Some(mv) = self.moves.get(next).filter(|mv| mv.at <= tick) {
                apply(
                    &mut self.positions,
                    mv,
                    self.markets[&mv.market].multiplier,
                    rates.get(&mv.market),
                    Some(&mut fees),
                )?;
                next += 1;
            }
            let frame = frame(&self.positions, prices, tick)?;
            if sample_tick(tick, end, interval_us) {
                curves.add(tick, &frame, &baseline, &fees, max_points)?;
            }
            terminal = Some(frame);
        }
        let terminal = terminal.context("missing final strategy valuation")?;
        let final_summary = point(end, terminal.portfolio, baseline.portfolio, &fees.portfolio)?;
        let mut market_summaries = Vec::with_capacity(selected.len());
        for key in selected {
            let values = terminal.markets.get(key).copied().unwrap_or_default();
            let (long_lots, short_lots) = terminal.quantities.get(key).copied().unwrap_or_default();
            let (initial_long, initial_short) =
                baseline.quantities.get(key).copied().unwrap_or_default();
            let fees = fees.markets.remove(key).unwrap_or_default();
            let summary = point(
                end,
                values,
                baseline.markets.get(key).copied().unwrap_or_default(),
                &fees,
            )?;
            market_summaries.push(MarketSummary {
                venue: key.0.clone(),
                symbol: key.1.clone(),
                long_lots,
                short_lots,
                realized_cny: summary.realized_cny,
                floating_cny: summary.floating_cny,
                nav_before_fees_cny: summary.nav_before_fees_cny,
                initial_net_lots: initial_long - initial_short,
                long_value_cny: values.long_value,
                short_value_cny: values.short_value,
                fees,
                summary,
                points: curves
                    .markets
                    .remove(key)
                    .map(|s| s.points)
                    .unwrap_or_default(),
            });
        }
        let strategies = curves
            .strategies
            .into_iter()
            .map(|(name, curve)| {
                let fees = fees.strategies.remove(&name).unwrap_or_default();
                let summary = point(
                    end,
                    terminal.strategies.get(&name).copied().unwrap_or_default(),
                    baseline.strategies.get(&name).copied().unwrap_or_default(),
                    &fees,
                )?;
                Ok(StrategySummary {
                    market_count: activity[&name].len(),
                    name,
                    fees,
                    summary,
                    points: curve.points,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let reason = (fees.portfolio.missing_count != 0)
            .then(|| "部分归因成交缺少可验证的 CNY 手续费依据".into());
        let points = curves.portfolio.points;
        Ok(StrategyReport {
            report: Report {
                source_id: self.anchor.source_id,
                ready: true,
                reason,
                currency: "CNY",
                valuation: "native_strategy_attribution_completed_minute_close",
                snapshot_us: Some(self.anchor.snapshot_us),
                start_us: start,
                end_us: end,
                source_through_us: Some(through),
                fees: fees.portfolio,
                summary: Some(final_summary),
                markets: market_summaries,
                sampled: points.len() < tick_count,
                points,
                available_symbols: available.iter().map(market_name).collect(),
                selected_symbols: selected.iter().map(market_name).collect(),
                trading_segments_us: super::trading_segments(prices, start, end),
            },
            strategies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_futures::records::fixture;

    const BASE: i64 = 1_794_000_000_000_000;
    const M: &str = "SHFE:cu2612";
    const DAY: &str = "20261117";

    fn order(reference: &str) -> fixture::Order {
        fixture::Order::new(
            M,
            DAY,
            "ctp:1:2",
            reference,
            Side::Buy,
            Offset::Open,
            1,
            101.0,
        )
    }

    fn fill(trade: &str) -> fixture::Fill {
        fixture::Fill {
            trade_id: trade.into(),
            lots: 1,
            price: 101.0,
            commission: Some(2.0),
            order_id: 7,
        }
    }

    fn fixture(temp: &std::path::Path, before: impl FnOnce(&DB)) -> (DB, Source, Replay) {
        let db = fixture::open(temp);
        before(&db);
        let source = Source {
            id: "account".into(),
            account: "test".into(),
            adapter: "ctp".into(),
            rocksdb_path: temp.into(),
            enabled: true,
            sync: None,
        };
        let request = netting::Initialize {
            strategy_groups: BTreeMap::from([(
                "g".into(),
                BTreeSet::from(["alpha".into(), "beta".into()]),
            )]),
            contracts: BTreeMap::from([(M.into(), netting::Contract { multiplier: 10.0 })]),
            positions: BTreeMap::new(),
            note: "flat synthetic anchor".into(),
        };
        let (mut anchor, ledger) =
            netting::Anchor::capture(&source, &db, request.note.clone()).unwrap();
        anchor.snapshot_us = BASE;
        let portfolio =
            netting::Portfolio::initialize("account", &request, &ledger, anchor).unwrap();
        let receipt = serde_json::to_value(&portfolio).unwrap();
        let replay =
            Replay::from_receipts(portfolio, vec![(0, "initialize".into(), receipt)]).unwrap();
        (db, source, replay)
    }

    #[test]
    fn crosses_and_fill_shares_after_the_cut_replay_with_their_own_commission() {
        let temp = tempfile::tempdir().unwrap();
        let (db, source, mut replay) = fixture(temp.path(), |db| {
            // Before the anchor cut: already in the anchor allocation.
            fixture::put_order(
                db,
                BASE - 5,
                &order("1").share(
                    &fill("t0"),
                    netting::UNALLOCATED,
                    1,
                    Some(2.0),
                    fixture::FILLED,
                    BASE - 5,
                ),
            );
        });
        let quote = BASE + 9;
        for (leg, (side, name)) in [(Side::Buy, "alpha"), (Side::Sell, "beta")]
            .into_iter()
            .enumerate()
        {
            fixture::put_order(
                &db,
                BASE + 10 + leg as i64,
                &fixture::cross_leg(M, DAY, quote, 0, side, name, 1, 100.0, BASE + 10),
            );
        }
        let two = fixture::Fill {
            lots: 2,
            ..fill("t1")
        };
        let mut placed = order("2");
        placed.lots = 2;
        fixture::put_order(
            &db,
            BASE + 19,
            &placed.lifecycle(fixture::NEW, "exec", BASE + 19, BASE + 19),
        );
        fixture::put_trade(
            &db,
            BASE + 20,
            &placed.trade(&two, 2, Some(fixture::FILLED), BASE + 20),
        );
        fixture::put_order(
            &db,
            BASE + 21,
            &placed.share(&two, "alpha", 1, Some(1.5), fixture::FILLED, BASE + 21),
        );
        fixture::put_order(
            &db,
            BASE + 22,
            &placed.share(
                &two,
                netting::UNALLOCATED,
                1,
                Some(1.5),
                fixture::FILLED,
                BASE + 22,
            ),
        );
        replay
            .read_records(&source, &db, BASE, BASE + 100, &[])
            .unwrap();
        let moves: Vec<_> = replay
            .moves
            .iter()
            .map(|mv| {
                (
                    mv.at,
                    mv.strategy.as_str(),
                    mv.lots,
                    mv.price,
                    mv.fill.as_ref().map(|f| f.commission_cny),
                )
            })
            .collect();
        assert_eq!(
            moves,
            vec![
                (BASE + 10, "alpha", 1, 100.0, None),
                (BASE + 11, "beta", -1, 100.0, None),
                (BASE + 21, "alpha", 1, 101.0, Some(Some(1.5))),
                (BASE + 22, netting::UNALLOCATED, 1, 101.0, Some(Some(1.5))),
            ]
        );
    }

    #[test]
    fn stop_and_retire_events_replay_and_retired_strategies_keep_their_fills() {
        let temp = tempfile::tempdir().unwrap();
        let (db, source, initial) = fixture(temp.path(), |_| {});
        let portfolio = {
            let request = netting::Initialize {
                strategy_groups: BTreeMap::from([(
                    "g".into(),
                    BTreeSet::from(["alpha".into(), "beta".into()]),
                )]),
                contracts: BTreeMap::from([(M.into(), netting::Contract { multiplier: 10.0 })]),
                positions: BTreeMap::new(),
                note: "flat synthetic anchor".into(),
            };
            let mut anchor = initial.anchor.clone();
            anchor.note = request.note.clone();
            netting::Portfolio::initialize("account", &request, &BTreeMap::new(), anchor).unwrap()
        };
        let change = |state: &netting::Portfolio, id: &str| netting::MemberChange {
            event_id: id.into(),
            expected_revision: state.revision,
            strategy_group: "g".into(),
            members: BTreeSet::from(["beta".into()]),
            note: "retire beta".into(),
        };
        let (stopped, stop) = portfolio
            .stop(&change(&portfolio, "halt"), BASE + 5)
            .unwrap();
        let (cut, _) = netting::Anchor::capture(&source, &db, "retire beta".into()).unwrap();
        let (retired, retire) = stopped
            .retire(&change(&stopped, "drop"), &cut, BASE + 6)
            .unwrap();
        let receipts = vec![
            (
                0,
                "initialize".into(),
                serde_json::to_value(&portfolio).unwrap(),
            ),
            (
                1,
                "strategy_stop".into(),
                serde_json::to_value(&stop).unwrap(),
            ),
            (
                2,
                "strategy_retire".into(),
                serde_json::to_value(&retire).unwrap(),
            ),
        ];
        let mut replay = Replay::from_receipts(retired.clone(), receipts.clone()).unwrap();
        assert!(replay.members.contains("beta"));
        // Exec crosses beta out to alpha after the cut; the leg stays beta's.
        for (leg, (side, name)) in [(Side::Buy, "alpha"), (Side::Sell, "beta")]
            .into_iter()
            .enumerate()
        {
            fixture::put_order(
                &db,
                BASE + 10 + leg as i64,
                &fixture::cross_leg(M, DAY, BASE + 9, 0, side, name, 1, 100.0, BASE + 10),
            );
        }
        replay
            .read_records(&source, &db, BASE, BASE + 100, &[])
            .unwrap();
        let moves: Vec<_> = replay
            .moves
            .iter()
            .map(|mv| (mv.strategy.as_str(), mv.lots))
            .collect();
        assert_eq!(moves, vec![("alpha", 1), ("beta", -1)]);
        // Without the retired set, beta's anchor row and fills are unregistered.
        let mut forgotten = retired;
        forgotten.retired.clear();
        assert!(Replay::from_receipts(forgotten, receipts).is_err());
    }

    #[test]
    fn unknown_strategy_or_foreign_store_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let (db, source, mut replay) = fixture(temp.path(), |_| {});
        fixture::put_order(
            &db,
            BASE + 10,
            &fixture::cross_leg(M, DAY, BASE, 0, Side::Buy, "gamma", 1, 100.0, BASE + 10),
        );
        assert!(
            replay
                .read_records(&source, &db, BASE, BASE + 100, &[])
                .is_err()
        );

        let other = tempfile::tempdir().unwrap();
        let (_, _, mut foreign) = fixture(other.path(), |_| {});
        assert!(
            foreign
                .read_records(&source, &db, BASE, BASE + 100, &[])
                .is_err()
        );
    }
}
