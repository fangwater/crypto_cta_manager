//! Historical strategy valuation from the immutable native attribution ledger.
use super::*;
use crate::native_futures::{config::Source, evidence, market_data, netting, reference_data};
use rocksdb::DB;
use serde_json::Value;
use sqlx::PgPool;

type PositionKey = (String, Market);

struct Scope {
    proof: evidence::Anchor,
    multiplier: f64,
}

#[derive(Deserialize)]
struct Extension {
    proof: evidence::Anchor,
    contracts: BTreeMap<String, netting::Contract>,
}

#[derive(Deserialize)]
struct Allocation {
    evidence_key: String,
    fill: netting::FactualFill,
    allocations: netting::Quantities,
}

struct Move {
    at: i64,
    market: Market,
    price: f64,
    allocations: netting::Quantities,
    fill: Option<Fill>,
}

pub(crate) struct Replay {
    anchor: evidence::Anchor,
    markets: BTreeMap<Market, Scope>,
    positions: BTreeMap<PositionKey, netting::Position>,
    moves: Vec<Move>,
    allocations: BTreeMap<Vec<u8>, (usize, netting::FactualFill)>,
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
                && serde_json::to_value(&initial.anchor)?
                    == serde_json::to_value(&portfolio.anchor)?,
            "native attribution initialization receipt conflicts with portfolio"
        );
        let mut replay = Self {
            anchor: initial.anchor,
            markets: BTreeMap::new(),
            positions: BTreeMap::new(),
            moves: vec![],
            allocations: BTreeMap::new(),
            members: portfolio.targets.into_keys().collect(),
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
                    let Extension { proof, contracts } = serde_json::from_value(value)?;
                    replay.add_scope(&proof, contracts)?;
                }
                "target_batch" => {
                    let batch: netting::BatchReceipt = serde_json::from_value(value)?;
                    for cross in batch.internal_crosses {
                        let mid = cross.quote.mid(batch.decision_us)?;
                        ensure!(
                            cross.kind == "internal_cross"
                                && cross.lots > 0
                                && cross.fee_cny == 0.0
                                && cross.buyer != cross.seller
                                && (cross.price - mid).abs() <= mid.abs() * 4.0 * f64::EPSILON,
                            "invalid immutable internal-cross receipt"
                        );
                        replay.moves.push(Move {
                            at: batch.decision_us,
                            market: market_key(&cross.market)?,
                            price: cross.price,
                            allocations: BTreeMap::from([
                                (cross.buyer, cross.lots),
                                (cross.seller, -cross.lots),
                            ]),
                            fill: None,
                        });
                    }
                }
                "exchange_fill_allocation" => {
                    let Allocation {
                        evidence_key,
                        fill,
                        allocations,
                    } = serde_json::from_value(value)?;
                    let at = fill.received_us;
                    let market = market_key(&fill.market)?;
                    ensure!(
                        replay
                            .allocations
                            .insert(hex::decode(evidence_key)?, (replay.moves.len(), fill))
                            .is_none(),
                        "duplicate native fill allocation"
                    );
                    replay.moves.push(Move {
                        at,
                        market,
                        price: 0.0,
                        allocations,
                        fill: None,
                    });
                }
                "external_demand_release" => {}
                _ => anyhow::bail!("unknown native attribution event kind"),
            }
            ensure!(
                replay.moves.len() <= 250_000,
                "native attribution movement budget exceeded"
            );
        }

        Ok(replay)
    }

    fn add_scope(
        &mut self,
        proof: &evidence::Anchor,
        contracts: BTreeMap<String, netting::Contract>,
    ) -> Result<()> {
        proof.validate()?;
        ensure!(
            proof.source_id == self.anchor.source_id
                && proof.store_id == self.anchor.store_id
                && proof.snapshot_us >= self.anchor.snapshot_us
                && proof.evidence_sequence >= self.anchor.evidence_sequence,
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
                                proof: proof.clone(),
                                multiplier: contract.multiplier,
                            }
                        )
                        .is_none(),
                "invalid or repeated attribution contract"
            );
        }
        Ok(())
    }

    pub(in crate::native_futures) fn validate_fills(
        &mut self,
        source: &Source,
        db: &DB,
        commissions: &commission::History,
        start: i64,
        end: i64,
        symbols: &[String],
    ) -> Result<()> {
        evidence::validate_anchor(source, db, &self.anchor)?;
        let selected = select_markets(&self.available(end), symbols)?;
        // Preserve committed FIFO order in each selected market. A later
        // allocation with a backdated observation cannot move in front of
        // an earlier committed operation, even across the request boundary.
        let mut clocks = BTreeMap::new();
        for mv in &self.moves {
            if selected.contains(&mv.market) {
                let previous = clocks.insert(&mv.market, mv.at);
                ensure!(
                    mv.at > end
                        || (mv.at >= self.markets[&mv.market].proof.snapshot_us
                            && previous.is_none_or(|old| old <= mv.at)
                            && mv
                                .allocations
                                .keys()
                                .all(|name| self.members.contains(name))),
                    "attribution operations are out of causal market order or scope"
                );
            }
        }
        let mut checked_scopes = BTreeSet::from([self.anchor.evidence_sequence]);
        for key in &selected {
            let proof = &self.markets[key].proof;
            if checked_scopes.insert(proof.evidence_sequence) {
                evidence::validate_anchor(source, db, proof)?;
            }
        }
        source::scan_fills(
            db,
            self.anchor.evidence_sequence,
            end,
            |key, value, seq, received| {
                let id = &value["fill"]["identity"];
                let market = (
                    id["venue"]
                        .as_str()
                        .context("native fill venue")?
                        .to_owned(),
                    id["symbol"]
                        .as_str()
                        .context("native fill contract")?
                        .to_owned(),
                );
                if !selected.contains(&market)
                    || seq <= self.markets[&market].proof.evidence_sequence
                {
                    return Ok(());
                }
                let (index, stored) = self
                    .allocations
                    .remove(key)
                    .context("合约存在未归因的真实成交；请先完成明确的成交归因")?;
                let mut fill = source::parse_fill(source, db, key, value, seq, received)?;
                commissions.enrich(&mut fill);
                let verified = netting::FactualFill::from_evidence(value)?;
                ensure!(
                    fill.live_origin
                        && fill.received_us >= self.markets[&market].proof.snapshot_us
                        && fill.trading_day >= self.markets[&market].proof.trading_day
                        && verified.evidence_sha256 == stored.evidence_sha256
                        && verified.identity == stored.identity
                        && verified.market == stored.market
                        && verified.lots == stored.lots
                        && verified.evidence_sequence == stored.evidence_sequence
                        && verified.received_us == stored.received_us,
                    "策略分配与真实成交回报不符或回报时间不可核验"
                );
                let mv = &mut self.moves[index];
                let sum = mv.allocations.values().try_fold(0i64, |sum, qty| {
                    ensure!(
                        *qty != 0 && qty.signum() == stored.lots.signum(),
                        "fill allocation side mismatch"
                    );
                    sum.checked_add(*qty)
                        .context("attribution fill allocation overflow")
                })?;
                ensure!(
                    sum == stored.lots,
                    "attribution fill allocation quantity mismatch"
                );
                mv.price = fill.price;
                mv.fill = Some(fill);
                Ok(())
            },
        )?;
        ensure!(
            self.allocations.values().all(|(i, _)| {
                let mv = &self.moves[*i];
                mv.at > end || !selected.contains(&mv.market)
            }),
            "策略分配所引用的真实成交在当前执行证据中缺失"
        );
        ensure!(start >= self.anchor.snapshot_us, "所选起点早于策略归因快照");
        self.allocations.clear();
        self.moves
            .retain(|mv| mv.at <= end && selected.contains(&mv.market));
        self.moves.sort_by_key(|mv| mv.at);
        self.positions.retain(|(_, key), _| selected.contains(key));
        Ok(())
    }

    fn available(&self, end: i64) -> BTreeSet<Market> {
        self.markets
            .iter()
            .filter(|(_, scope)| scope.proof.snapshot_us <= end)
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
            for (venue, symbol) in active {
                if self.moves.iter().any(|mv| {
                    mv.market.0 == venue
                        && mv.market.1 == symbol
                        && mv.at >= query.start_us
                        && mv.fill.as_ref().is_some_and(Fill::needs_reference_fee)
                }) {
                    match feed
                        .history(
                            &self.anchor.source_id,
                            &venue,
                            &symbol,
                            query.start_us,
                            query.end_us,
                        )
                        .await
                    {
                        Ok(rows) => {
                            rates.insert((venue, symbol), rows);
                        }
                        Err(error) => {
                            tracing::warn!(source_id=%self.anchor.source_id,error=?error,"strategy NAV fee observations unavailable")
                        }
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
    let Some((fill, window)) = mv.fill.as_ref().zip(window) else {
        for (name, lots) in &mv.allocations {
            positions
                .entry((name.clone(), mv.market.clone()))
                .or_default()
                .apply(*lots, mv.price, multiplier, Some(0.0))?;
        }
        return Ok(());
    };
    let estimated = fill.estimated_fee(multiplier, rates.map(Vec::as_slice))?;
    let volume = fill.price * multiplier * fill.lots as f64;
    window
        .portfolio
        .add(volume, fill.commission_cny, estimated)?;
    window.markets.entry(mv.market.clone()).or_default().add(
        volume,
        fill.commission_cny,
        estimated,
    )?;

    let fee = fill.commission_cny.or(estimated);
    let mut assigned = 0.0;
    for (i, (name, lots)) in mv.allocations.iter().enumerate() {
        let share = lots.unsigned_abs() as f64 / fill.lots as f64;
        let fee_share = fee.map(|total| {
            let part = if i + 1 == mv.allocations.len() {
                total - assigned
            } else {
                total * share
            };
            assigned += part;
            part
        });
        positions
            .entry((name.clone(), mv.market.clone()))
            .or_default()
            .apply(*lots, mv.price, multiplier, fee_share)?;
        window.strategies.entry(name.clone()).or_default().add(
            fill.price * multiplier * lots.unsigned_abs() as f64,
            fill.commission_cny.and(fee_share),
            estimated.and(fee_share),
        )?;
    }
    Ok(())
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
            for name in mv.allocations.keys() {
                activity
                    .entry(name.clone())
                    .or_default()
                    .insert(mv.market.clone());
            }
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
            },
            strategies,
        })
    }
}
