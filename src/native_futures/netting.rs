//! Manager-owned virtual attribution. Exchange evidence is never synthesized.
pub mod store;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub type Quantities = BTreeMap<String, i64>;
pub type Strategies = BTreeMap<String, Quantities>;
const MAX_LOTS: i64 = i32::MAX as i64;
const CTAKER_LEASE_US: i64 = 60_000_000;

#[derive(Debug)]
pub struct Conflict(pub String);
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Conflict {}
pub fn require(condition: bool, message: &str) -> Result<()> {
    if !condition {
        return Err(Conflict(message.into()).into());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub multiplier: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InitialPosition {
    pub lots: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Initialize {
    pub strategy_groups: BTreeMap<String, BTreeSet<String>>,
    pub contracts: BTreeMap<String, Contract>,
    pub positions: BTreeMap<String, BTreeMap<String, InitialPosition>>,
    pub snapshot_evidence_sequence: i64,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtendScope {
    pub event_id: String,
    pub expected_revision: i64,
    pub strategy_group: String,
    pub members: BTreeSet<String>,
    pub contracts: BTreeMap<String, Contract>,
    pub snapshot_evidence_sequence: i64,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
    pub observed_us: i64,
    pub source: String,
}
impl Quote {
    pub fn mid(&self, at_us: i64) -> Result<f64> {
        ensure!(
            self.bid.is_finite()
                && self.ask.is_finite()
                && self.bid > 0.0
                && self.ask >= self.bid
                && self.observed_us > 0
                && self.observed_us <= at_us
                && at_us - self.observed_us <= 5_000_000
                && !self.source.trim().is_empty()
                && self.source.len() <= 256,
            "internal crossing requires a positive, non-crossed BBO no older than five seconds at the decision"
        );
        Ok(self.bid / 2.0 + self.ask / 2.0)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetBatch {
    pub strategy_group: String,
    pub batch_id: String,
    pub expected_revision: i64,
    pub decision_us: i64,
    /// Explicit native day for Exec readers. Legacy planning batches stay analytical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trading_day: Option<String>,
    #[serde(default)]
    pub signal_times: BTreeMap<String, i64>,
    /// Complete roster, each with a complete contract vector (including zeros).
    pub strategies: Strategies,
    pub quotes: BTreeMap<String, Quote>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Lot {
    pub lots: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Position {
    pub fifo: VecDeque<Lot>,
    pub realized_cny: f64,
    /// Unknown exchange fees remain unknown, independently of known subtotals.
    pub known_fees_cny: f64,
    pub unknown_fee_fills: u64,
}
impl Position {
    pub fn quantity(&self) -> i64 {
        self.fifo.iter().map(|lot| lot.lots).sum()
    }
    fn apply(
        &mut self,
        mut lots: i64,
        price: f64,
        multiplier: f64,
        fee: Option<f64>,
    ) -> Result<()> {
        validate_lots(lots)?;
        ensure!(
            lots != 0 && price.is_finite() && price > 0.0,
            "invalid attribution quantity/price"
        );
        validate_lots(
            self.quantity()
                .checked_add(lots)
                .context("position overflow")?,
        )?;
        while let Some(front) = self.fifo.front_mut() {
            if lots == 0 || front.lots.signum() == lots.signum() {
                break;
            }
            let matched = front.lots.abs().min(lots.abs());
            self.realized_cny +=
                matched as f64 * (price - front.price) * front.lots.signum() as f64 * multiplier;
            front.lots -= matched * front.lots.signum();
            lots -= matched * lots.signum();
            if front.lots == 0 {
                self.fifo.pop_front();
            }
        }
        if lots != 0 {
            self.fifo.push_back(Lot { lots, price });
        }
        match fee {
            Some(fee) => {
                ensure!(fee.is_finite(), "invalid actual fee");
                self.known_fees_cny += fee;
            }
            None => {
                self.unknown_fee_fills = self
                    .unknown_fee_fills
                    .checked_add(1)
                    .context("fee counter overflow")?
            }
        }
        ensure!(
            self.realized_cny.is_finite() && self.known_fees_cny.is_finite(),
            "attribution PnL overflow"
        );
        Ok(())
    }
    pub fn valued(&self, mark: f64, multiplier: f64) -> Result<Valuation> {
        ensure!(mark.is_finite() && mark > 0.0, "invalid attribution mark");
        let floating = self
            .fifo
            .iter()
            .map(|lot| lot.lots as f64 * (mark - lot.price) * multiplier)
            .sum::<f64>();
        let before = floating + self.realized_cny;
        ensure!(before.is_finite(), "valuation overflow");
        Ok(Valuation {
            lots: self.quantity(),
            realized_cny: self.realized_cny,
            floating_cny: floating,
            known_fees_cny: self.known_fees_cny,
            unknown_fee_fills: self.unknown_fee_fills,
            pnl_after_fees_cny: (self.unknown_fee_fills == 0)
                .then_some(before - self.known_fees_cny),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct Valuation {
    pub lots: i64,
    pub realized_cny: f64,
    pub floating_cny: f64,
    pub known_fees_cny: f64,
    pub unknown_fee_fills: u64,
    pub pnl_after_fees_cny: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StrategyGroup {
    pub members: BTreeSet<String>,
    pub decision_us: i64,
    pub held: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Portfolio {
    pub source_id: String,
    pub revision: i64,
    pub decision_us: i64,
    pub batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_trading_day: Option<String>,
    pub external_demand_open: bool,
    pub target_revision: i64,
    pub last_strategy_group: Option<String>,
    pub strategy_groups: BTreeMap<String, StrategyGroup>,
    pub contracts: BTreeMap<String, Contract>,
    pub targets: Strategies,
    pub positions: BTreeMap<String, BTreeMap<String, Position>>,
    /// Account attribution anchor + imported factual fills; never target quantity.
    pub account: BTreeMap<String, Position>,
    pub anchor: super::evidence::Anchor,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InternalCross {
    pub kind: String,
    pub market: String,
    pub buyer: String,
    pub seller: String,
    pub lots: i64,
    pub price: f64,
    pub fee_cny: f64,
    pub quote: Quote,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetTarget {
    pub target_lots: i64,
    pub attributed_lots: i64,
    pub residual_lots: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchReceipt {
    pub strategy_group: String,
    pub source_id: String,
    pub batch_id: String,
    pub revision: i64,
    pub decision_us: i64,
    pub internal_crosses: Vec<InternalCross>,
    pub external_demands: Strategies,
    pub net_targets: BTreeMap<String, NetTarget>,
    pub execution_plan: ExecutionPlan,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutionPlan {
    pub source_id: String,
    pub batch_id: Option<String>,
    pub target_revision: i64,
    pub account_revision: i64,
    pub publishable: bool,
    /// One signed account target per native contract, independently of how
    /// many factor strategies contributed to it.
    pub net_targets: Quantities,
    pub attribution: Strategies,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FactualFill {
    pub identity: String,
    pub market: String,
    pub lots: i64,
    pub price: f64,
    pub commission_cny: Option<f64>,
    pub evidence_sequence: i64,
    pub received_us: i64,
    pub evidence_sha256: String,
}

impl FactualFill {
    /// Called only with a record read from this source's cn_trade_facts CF.
    pub fn from_evidence(record: &serde_json::Value) -> Result<Self> {
        let fill = &record["fill"];
        let id = &fill["identity"];
        let venue = id["venue"].as_str().context("fill venue")?;
        let symbol = id["symbol"].as_str().context("fill symbol")?;
        let day = id["trading_day"].as_str().context("fill trading day")?;
        super::reference::valid_day(day)?;
        let trade = id["trade_id"].as_str().context("fill trade id")?;
        ensure!(!trade.is_empty(), "missing native trade id");
        let side = match id["side"].as_str() {
            Some("buy") => 1,
            Some("sell") => -1,
            _ => anyhow::bail!("invalid factual fill side"),
        };
        let qty = fill["lots"].as_i64().context("fill lots")?;
        ensure!(qty > 0, "invalid factual fill lots");
        let market = format!("{venue}:{symbol}");
        super::validate_market(&market)?;
        let commission_cny = if record["commission_currency"].as_str() == Some("CNY") {
            record["commission"].as_f64()
        } else {
            None
        };
        Ok(Self {
            identity: serde_json::to_string(&(venue, symbol, day, trade, side))?,
            market,
            lots: qty.checked_mul(side).context("fill lots overflow")?,
            price: fill["price"].as_f64().context("fill price")?,
            commission_cny,
            evidence_sequence: record["evidence_seq"]
                .as_i64()
                .context("fill evidence sequence")?,
            received_us: record["received_us"]
                .as_i64()
                .context("fill observation time")?,
            evidence_sha256: fingerprint(record)?,
        })
    }
}

impl Portfolio {
    /// Explicit operator allocation, checked against a complete factual snapshot.
    pub fn initialize(
        source_id: &str,
        request: &Initialize,
        account_lots: &Quantities,
        anchor: super::evidence::Anchor,
    ) -> Result<Self> {
        anchor.validate()?;
        ensure!(
            anchor.source_id == source_id
                && anchor.evidence_sequence == request.snapshot_evidence_sequence
                && anchor.note == request.note,
            "initial attribution evidence does not match source/request"
        );
        let at_us = anchor.snapshot_us;
        ensure!(
            (1..=32).contains(&request.strategy_groups.len()),
            "require 1..32 strategy groups"
        );
        let mut strategies = BTreeSet::new();
        for (name, members) in &request.strategy_groups {
            valid_id(name)?;
            ensure!(!members.is_empty(), "strategy group is empty");
            for member in members {
                ensure!(
                    strategies.insert(member.clone()),
                    "strategy belongs to multiple groups"
                );
            }
        }

        ensure!(
            (1..=128).contains(&strategies.len())
                && (1..=64).contains(&request.contracts.len())
                && request.snapshot_evidence_sequence > 0
                && at_us > 0
                && !request.note.trim().is_empty()
                && request.note.len() <= 4096,
            "invalid native attribution initialization"
        );
        for name in &strategies {
            crate::order_config::validate_strategy_name(name).map_err(anyhow::Error::msg)?;
        }
        for (market, contract) in &request.contracts {
            super::validate_market(market)?;
            ensure!(
                contract.multiplier.is_finite() && contract.multiplier > 0.0,
                "invalid contract multiplier"
            );
        }
        ensure!(
            request
                .positions
                .keys()
                .all(|name| strategies.contains(name)),
            "initial position has unknown strategy"
        );
        let mut positions = BTreeMap::new();
        let mut account: BTreeMap<String, Position> = BTreeMap::new();
        let mut targets = BTreeMap::new();
        for name in &strategies {
            let mut rows = BTreeMap::new();
            let mut desired = BTreeMap::new();
            if let Some(initial) = request.positions.get(name) {
                ensure!(
                    initial.keys().all(|m| request.contracts.contains_key(m)),
                    "unknown initial contract"
                );
            }
            for (market, contract) in &request.contracts {
                let initial = request.positions.get(name).and_then(|p| p.get(market));
                let mut position = Position::default();
                if let Some(p) = initial {
                    validate_lots(p.lots)?;
                    if p.lots != 0 {
                        position.apply(p.lots, p.price, contract.multiplier, Some(0.0))?;
                        account.entry(market.clone()).or_default().apply(
                            p.lots,
                            p.price,
                            contract.multiplier,
                            Some(0.0),
                        )?;
                    }
                }
                desired.insert(market.clone(), position.quantity());
                rows.insert(market.clone(), position);
                account.entry(market.clone()).or_default();
            }
            positions.insert(name.clone(), rows);
            targets.insert(name.clone(), desired);
        }
        for market in request.contracts.keys() {
            ensure!(
                account[market].quantity() == *account_lots.get(market).unwrap_or(&0),
                "initial allocations do not match factual account net lots for {market}"
            );
        }
        let portfolio = Self {
            source_id: source_id.into(),
            revision: 0,
            decision_us: 0,
            batch_id: None,
            target_trading_day: None,
            external_demand_open: false,
            target_revision: 0,
            last_strategy_group: None,
            strategy_groups: request
                .strategy_groups
                .iter()
                .map(|(name, members)| {
                    (
                        name.clone(),
                        StrategyGroup {
                            members: members.clone(),
                            decision_us: 0,
                            held: false,
                        },
                    )
                })
                .collect(),
            contracts: request.contracts.clone(),
            targets,
            positions,
            account,
            anchor,
        };
        portfolio.check()?;
        Ok(portfolio)
    }
    pub fn extend_scope(
        &self,
        request: &ExtendScope,
        proof: &super::evidence::Anchor,
    ) -> Result<Self> {
        require(
            self.revision == request.expected_revision,
            "portfolio revision changed",
        )?;
        require(
            !self.external_demand_open,
            "settle external demand before extending scope",
        )?;
        proof.validate()?;
        ensure!(
            proof.source_id == self.source_id
                && proof.store_id == self.anchor.store_id
                && proof.evidence_sequence == request.snapshot_evidence_sequence
                && proof.evidence_sequence > self.anchor.evidence_sequence
                && proof.snapshot_us > self.anchor.snapshot_us.max(self.decision_us)
                && proof.note == request.note,
            "scope extension requires a later factual snapshot from the same execution store"
        );
        ensure!(
            (!request.members.is_empty() || !request.contracts.is_empty())
                && self.targets.len() + request.members.len() <= 128
                && self.contracts.len() + request.contracts.len() <= 64,
            "invalid attribution scope extension size"
        );
        valid_id(&request.strategy_group)?;
        ensure!(
            self.strategy_groups.contains_key(&request.strategy_group)
                || (!request.members.is_empty() && self.strategy_groups.len() < 32),
            "new strategy group requires members and at most 32 groups"
        );
        for name in &request.members {
            crate::order_config::validate_strategy_name(name).map_err(anyhow::Error::msg)?;
            ensure!(
                !self.targets.contains_key(name),
                "strategy {name} already registered"
            );
        }
        for (market, contract) in &request.contracts {
            super::validate_market(market)?;
            ensure!(
                !self.contracts.contains_key(market),
                "contract {market} already registered"
            );
            ensure!(
                contract.multiplier.is_finite() && contract.multiplier > 0.0,
                "invalid contract multiplier"
            );
        }

        let mut next = self.clone();
        next.strategy_groups
            .entry(request.strategy_group.clone())
            .or_insert_with(|| StrategyGroup {
                members: BTreeSet::new(),
                decision_us: 0,
                held: false,
            })
            .members
            .extend(request.members.iter().cloned());
        next.contracts.extend(request.contracts.clone());
        for market in request.contracts.keys() {
            next.account.insert(market.clone(), Position::default());
            for targets in next.targets.values_mut() {
                targets.insert(market.clone(), 0);
            }
            for positions in next.positions.values_mut() {
                positions.insert(market.clone(), Position::default());
            }
        }
        for name in &request.members {
            next.targets.insert(
                name.clone(),
                next.contracts.keys().map(|m| (m.clone(), 0)).collect(),
            );
            next.positions.insert(
                name.clone(),
                next.account
                    .keys()
                    .map(|m| (m.clone(), Position::default()))
                    .collect(),
            );
        }
        next.revision = self.revision.checked_add(1).context("revision overflow")?;
        next.check()?;
        Ok(next)
    }

    pub fn external_demands(&self) -> Strategies {
        if !self.external_demand_open {
            return BTreeMap::new();
        }
        self.effective_targets()
            .iter()
            .filter_map(|(name, rows)| {
                let remaining: Quantities = rows
                    .iter()
                    .filter_map(|(market, target)| {
                        let delta = target - self.positions[name][market].quantity();
                        (delta != 0).then_some((market.clone(), delta))
                    })
                    .collect();
                (!remaining.is_empty()).then_some((name.clone(), remaining))
            })
            .collect()
    }
    pub(crate) fn ctaker_lease(&self) -> Option<(i64, i64)> {
        self.target_trading_day.as_ref()?;
        let group = self
            .strategy_groups
            .get(self.last_strategy_group.as_ref()?)?;
        Some((
            group.decision_us,
            group.decision_us.saturating_add(CTAKER_LEASE_US),
        ))
    }

    pub fn execution_plan(&self) -> Result<ExecutionPlan> {
        Ok(ExecutionPlan {
            source_id: self.source_id.clone(),
            batch_id: self.batch_id.clone(),
            target_revision: self.target_revision,
            account_revision: self.revision,
            publishable: self.external_demand_open,
            net_targets: self
                .net_targets()?
                .into_iter()
                .map(|(market, row)| (market, row.target_lots))
                .collect(),
            attribution: self.external_demands(),
        })
    }
    fn effective_targets(&self) -> Strategies {
        let mut targets = self.targets.clone();
        for group in self.strategy_groups.values().filter(|g| g.held) {
            for name in &group.members {
                for (market, position) in &self.positions[name] {
                    targets
                        .get_mut(name)
                        .unwrap()
                        .insert(market.clone(), position.quantity());
                }
            }
        }
        targets
    }
    pub fn net_targets(&self) -> Result<BTreeMap<String, NetTarget>> {
        let effective = self.effective_targets();
        self.contracts
            .keys()
            .map(|market| {
                let target = effective.values().try_fold(0i64, |n, rows| {
                    n.checked_add(rows[market])
                        .context("aggregate target overflow")
                })?;
                validate_lots(target)?;
                let current = self.account[market].quantity();
                Ok((
                    market.clone(),
                    NetTarget {
                        target_lots: target,
                        attributed_lots: current,
                        residual_lots: target - current,
                    },
                ))
            })
            .collect()
    }
    pub fn check(&self) -> Result<()> {
        for market in self.contracts.keys() {
            let sum = self.positions.values().try_fold(0i64, |n, p| {
                n.checked_add(p[market].quantity())
                    .context("allocation overflow")
            })?;
            ensure!(
                sum == self.account[market].quantity(),
                "strategy/account attribution mismatch for {market}"
            );
        }
        Ok(())
    }
    /// Pure plan: callers persist the returned state and complete receipt together.
    pub fn batch(&self, request: &TargetBatch, now_us: i64) -> Result<(Self, BatchReceipt)> {
        if let Some(day) = &request.trading_day {
            super::reference::valid_day(day)?;
        }
        valid_id(&request.batch_id)?;
        let group = self
            .strategy_groups
            .get(&request.strategy_group)
            .context("unknown strategy group")?;
        require(
            !self.external_demand_open,
            "previous external demand is unsettled; import factual fills or release confirmed canceled demand first",
        )?;
        require(
            request.expected_revision == self.revision,
            "portfolio changed; reload before publishing",
        )?;
        require(
            request.decision_us > group.decision_us
                && request.decision_us >= self.anchor.snapshot_us,
            "stale decision timestamp",
        )?;
        ensure!(
            request.decision_us <= now_us && now_us - request.decision_us <= 120_000_000,
            "decision must be within the last 120 seconds"
        );
        ensure!(
            request.strategies.keys().eq(group.members.iter()),
            "batch must contain the complete registered group roster"
        );
        ensure!(
            request.signal_times.is_empty()
                || (request.signal_times.keys().eq(request.strategies.keys())
                    && request
                        .signal_times
                        .values()
                        .all(|at| *at >= 0 && *at <= request.decision_us)),
            "invalid per-strategy signal observation times"
        );
        for rows in request.strategies.values() {
            ensure!(
                rows.keys().eq(self.contracts.keys()),
                "each strategy must explicitly include every contract, including zero targets"
            );
            for lots in rows.values() {
                validate_lots(*lots)?;
            }
        }
        ensure!(
            request
                .quotes
                .keys()
                .all(|m| self.contracts.contains_key(m)),
            "quote for unregistered contract"
        );
        let mut next = self.clone();
        next.targets.extend(request.strategies.clone());
        let updated = next
            .strategy_groups
            .get_mut(&request.strategy_group)
            .unwrap();
        updated.decision_us = request.decision_us;
        updated.held = false;
        next.last_strategy_group = Some(request.strategy_group.clone());
        next.revision = next.revision.checked_add(1).context("revision overflow")?;
        next.target_revision = next.revision;
        next.decision_us = self.decision_us.max(request.decision_us);
        next.batch_id = Some(request.batch_id.clone());
        next.target_trading_day = request.trading_day.clone();
        let mut crosses = vec![];
        let mut external = Strategies::new();
        let effective = next.effective_targets();
        for (market, contract) in &self.contracts {
            let mut buyers = VecDeque::new();
            let mut sellers = VecDeque::new();
            for (name, rows) in &effective {
                let delta = rows[market] - self.positions[name][market].quantity();
                if delta > 0 {
                    buyers.push_back((name.clone(), delta));
                } else if delta < 0 {
                    sellers.push_back((name.clone(), -delta));
                }
            }
            while let (Some((buyer, buy)), Some((seller, sell))) =
                (buyers.front_mut(), sellers.front_mut())
            {
                let quote = request
                    .quotes
                    .get(market)
                    .context("missing BBO for internal crossing")?;
                let price = quote.mid(request.decision_us)?;
                let qty = (*buy).min(*sell);
                next.positions
                    .get_mut(buyer)
                    .unwrap()
                    .get_mut(market)
                    .unwrap()
                    .apply(qty, price, contract.multiplier, Some(0.0))?;
                next.positions
                    .get_mut(seller)
                    .unwrap()
                    .get_mut(market)
                    .unwrap()
                    .apply(-qty, price, contract.multiplier, Some(0.0))?;
                crosses.push(InternalCross {
                    kind: "internal_cross".into(),
                    market: market.clone(),
                    buyer: buyer.clone(),
                    seller: seller.clone(),
                    lots: qty,
                    price,
                    fee_cny: 0.0,
                    quote: quote.clone(),
                });
                *buy -= qty;
                *sell -= qty;
                if *buy == 0 {
                    buyers.pop_front();
                }
                if *sell == 0 {
                    sellers.pop_front();
                }
            }
            for (name, qty) in buyers {
                external
                    .entry(name)
                    .or_default()
                    .insert(market.clone(), qty);
            }
            for (name, qty) in sellers {
                external
                    .entry(name)
                    .or_default()
                    .insert(market.clone(), -qty);
            }
        }
        next.external_demand_open = !external.is_empty();
        next.check()?;
        let receipt = BatchReceipt {
            strategy_group: request.strategy_group.clone(),
            source_id: self.source_id.clone(),
            batch_id: request.batch_id.clone(),
            revision: next.revision,
            decision_us: request.decision_us,
            internal_crosses: crosses,
            external_demands: external,
            net_targets: next.net_targets()?,
            execution_plan: next.execution_plan()?,
        };
        Ok((next, receipt))
    }
    /// Only imported, explicitly attributed factual fills advance the account ledger.
    pub fn fill(&self, fill: &FactualFill, allocations: &Quantities) -> Result<Self> {
        let contract = self
            .contracts
            .get(&fill.market)
            .context("unregistered factual fill contract")?;
        ensure!(
            fill.evidence_sequence > self.anchor.evidence_sequence
                && fill.received_us >= self.anchor.snapshot_us,
            "fill is already included in the initial snapshot or predates the anchor"
        );
        validate_lots(fill.lots)?;
        ensure!(
            fill.lots != 0
                && !allocations.is_empty()
                && allocations.keys().all(|s| self.positions.contains_key(s)),
            "invalid fill allocation strategies"
        );
        let mut sum = 0i64;
        for qty in allocations.values() {
            validate_lots(*qty)?;
            ensure!(
                *qty != 0 && qty.signum() == fill.lots.signum(),
                "allocation must have factual fill direction"
            );
            sum = sum.checked_add(*qty).context("fill allocation overflow")?;
        }
        ensure!(
            sum == fill.lots,
            "allocations must sum to the factual fill quantity"
        );
        let mut next = self.clone();
        let mut assigned_fee = 0.0;
        for (i, (name, qty)) in allocations.iter().enumerate() {
            let fee = fill.commission_cny.map(|total| {
                let part = if i + 1 == allocations.len() {
                    total - assigned_fee
                } else {
                    total * (*qty as f64 / fill.lots as f64)
                };
                assigned_fee += part;
                part
            });
            next.positions
                .get_mut(name)
                .unwrap()
                .get_mut(&fill.market)
                .unwrap()
                .apply(*qty, fill.price, contract.multiplier, fee)?;
        }
        next.account.get_mut(&fill.market).unwrap().apply(
            fill.lots,
            fill.price,
            contract.multiplier,
            fill.commission_cny,
        )?;
        next.revision = next.revision.checked_add(1).context("revision overflow")?;
        next.external_demand_open = self.external_demand_open
            && next
                .net_targets()?
                .values()
                .any(|row| row.residual_lots != 0);
        next.check()?;
        Ok(next)
    }
}

pub fn fingerprint(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}
pub fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':')),
        "invalid batch/event identifier"
    );
    Ok(())
}
fn validate_lots(qty: i64) -> Result<()> {
    ensure!(
        (-MAX_LOTS..=MAX_LOTS).contains(&qty),
        "quantity exceeds native integer-lot bounds"
    );
    Ok(())
}
