//! Strategy target roster and attribution anchor. Like crypto, Exec owns the live
//! strategy ledger: it crosses strategy gaps, trades the net and attributes its
//! fills in `cn_uniform_orders`. Manager only routes complete strategy targets
//! and folds those records.
pub mod store;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Account position no named strategy explains; Exec's implicit remainder.
pub const UNALLOCATED: &str = "__unallocated__";

pub type Quantities = BTreeMap<String, i64>;
pub type Strategies = BTreeMap<String, Quantities>;
const MAX_LOTS: i64 = i32::MAX as i64;

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

/// A cut of the Exec `cn_uniform_orders` log in one execution store
/// generation. Records with a later key are attribution changes after it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub source_id: String,
    /// Manager time of the cut.
    pub snapshot_us: i64,
    /// Last `cn_uniform_orders` key folded into the anchor; 0 for an empty log.
    pub orders_key: i64,
    pub store_id: String,
    pub note: String,
}
impl Anchor {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.source_id.is_empty()
                && self.snapshot_us > 0
                && self.orders_key >= 0
                && !self.store_id.is_empty()
                && !self.note.trim().is_empty()
                && self.note.len() <= 4096,
            "invalid native attribution anchor"
        );
        Ok(())
    }

    /// Cuts the source's uniform-order log at its current tip and folds the
    /// named-strategy positions through it.
    pub fn capture(
        source: &super::config::Source,
        db: &rocksdb::DB,
        note: String,
    ) -> Result<(Self, ExecLedger)> {
        let orders_key = super::records::tip(db, super::records::ORDERS)?;
        let anchor = Self {
            source_id: source.id.clone(),
            snapshot_us: super::now_us(),
            orders_key,
            store_id: super::records::generation(source, db)?,
            note,
        };
        anchor.validate()?;
        Ok((anchor, super::records::strategy_positions(db, orders_key)?))
    }

    /// The anchor's store generation is the one being read and its cut exists.
    pub fn verify(&self, source: &super::config::Source, db: &rocksdb::DB) -> Result<()> {
        self.validate()?;
        ensure!(
            self.source_id == source.id
                && super::records::generation(source, db)? == self.store_id
                && self.orders_key <= super::records::tip(db, super::records::ORDERS)?,
            "attribution anchor belongs to a different source or execution store generation"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Initialize {
    pub strategy_groups: BTreeMap<String, BTreeSet<String>>,
    pub contracts: BTreeMap<String, Contract>,
    pub positions: BTreeMap<String, BTreeMap<String, InitialPosition>>,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rebase {
    pub event_id: String,
    pub expected_revision: i64,
    pub strategy_groups: BTreeMap<String, BTreeSet<String>>,
    /// Signed opening lots per strategy (including [`UNALLOCATED`]) and contract.
    pub positions: BTreeMap<String, Quantities>,
    /// Native trading day of the opening inventory.
    pub trading_day: String,
    /// The account's opening gross inventory, valued at the cut's minute close.
    pub inventory: Vec<super::nav::Inventory>,
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
    pub note: String,
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
    pub(crate) fn apply(
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Portfolio {
    pub source_id: String,
    pub revision: i64,
    #[serde(default, skip_serializing_if = "is_zero_revision")]
    pub baseline_revision: i64,
    pub decision_us: i64,
    pub batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_trading_day: Option<String>,
    pub target_revision: i64,
    pub last_strategy_group: Option<String>,
    pub strategy_groups: BTreeMap<String, StrategyGroup>,
    pub contracts: BTreeMap<String, Contract>,
    pub targets: Strategies,
    /// Strategy allocation at the anchor, including [`UNALLOCATED`]. Later changes
    /// are Exec's uniform-order records; Manager never advances these rows itself.
    pub positions: BTreeMap<String, BTreeMap<String, Position>>,
    /// Account allocation at the anchor: the sum of all strategy rows.
    pub account: BTreeMap<String, Position>,
    pub anchor: Anchor,
}

fn is_zero_revision(revision: &i64) -> bool {
    *revision == 0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchReceipt {
    pub strategy_group: String,
    pub source_id: String,
    pub batch_id: String,
    pub revision: i64,
    pub decision_us: i64,
    pub net_targets: Quantities,
    pub execution_plan: ExecutionPlan,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutionPlan {
    pub source_id: String,
    pub batch_id: Option<String>,
    pub target_revision: i64,
    pub account_revision: i64,
    /// Complete registered roster; Exec nets, crosses and attributes them.
    pub strategies: Strategies,
    pub net_targets: Quantities,
}

/// Named-strategy lots per contract folded from `cn_uniform_orders` at a cut.
pub type ExecLedger = BTreeMap<String, Quantities>;

impl Portfolio {
    pub(super) fn rebase(
        &self,
        request: &Rebase,
        nav: &super::nav::Anchor,
        anchor: Anchor,
        exec_ledger: &ExecLedger,
    ) -> Result<Self> {
        require(
            self.revision == request.expected_revision,
            "portfolio changed before rebase",
        )?;
        require(
            anchor.source_id == self.source_id
                && nav.source_id == self.source_id
                && anchor.store_id == self.anchor.store_id
                && nav.store_id == anchor.store_id
                && nav.snapshot_us == anchor.snapshot_us
                && anchor.snapshot_us > self.anchor.snapshot_us.max(self.decision_us)
                && anchor.orders_key >= self.anchor.orders_key,
            "rebase requires a later cut of the same account and execution store",
        )?;
        require(
            request
                .strategy_groups
                .keys()
                .eq(self.strategy_groups.keys())
                && request.strategy_groups.iter().all(|(group, members)| {
                    !members.is_empty() && members.is_subset(&self.strategy_groups[group].members)
                }),
            "rebase requires existing strategies for every registered group",
        )?;
        let mut account = Quantities::new();
        let mut marks = BTreeMap::new();
        for position in &nav.positions {
            let market = format!("{}:{}", position.venue, position.symbol);
            require(
                self.contracts
                    .get(&market)
                    .is_some_and(|c| c.multiplier == position.contract_multiplier)
                    && !account.contains_key(&market),
                "rebase requires registered contracts and unhedged opening inventory",
            )?;
            let lots = position
                .today
                .checked_add(position.yesterday)
                .context("gross position overflow")?;
            let sign = match position.side {
                super::nav::Side::Buy => 1,
                super::nav::Side::Sell => -1,
            };
            account.insert(market.clone(), lots * sign);
            marks.insert(market, position.reference_price);
        }
        let positions = request
            .positions
            .iter()
            .map(|(name, rows)| {
                let rows = rows
                    .iter()
                    .map(|(market, &lots)| {
                        let price = *marks.get(market).context(
                            "rebase allocation has no opening inventory in that contract",
                        )?;
                        Ok((market.clone(), InitialPosition { lots, price }))
                    })
                    .collect::<Result<_>>()?;
                Ok((name.clone(), rows))
            })
            .collect::<Result<_>>()?;
        let init = Initialize {
            strategy_groups: request.strategy_groups.clone(),
            contracts: self.contracts.clone(),
            positions,
            note: request.note.clone(),
        };
        let mut next = Self::initialize(&self.source_id, &init, exec_ledger, anchor)?;
        for (market, position) in &next.account {
            ensure!(
                position.quantity() == *account.get(market).unwrap_or(&0),
                "rebase allocations do not match the opening inventory for {market}"
            );
        }
        next.revision = self.revision.checked_add(1).context("revision overflow")?;
        // Keep the next issued target newer than any target the Exec has seen.
        next.target_revision = self.target_revision;
        next.baseline_revision = next.revision;
        Ok(next)
    }

    /// Anchors attribution at a cut of the Exec log. Named strategies must hold
    /// exactly the positions folded from Exec's records through the cut; the
    /// rest of the account is the operator-stated [`UNALLOCATED`] remainder.
    pub fn initialize(
        source_id: &str,
        request: &Initialize,
        exec_ledger: &ExecLedger,
        anchor: Anchor,
    ) -> Result<Self> {
        anchor.validate()?;
        ensure!(
            anchor.source_id == source_id && anchor.note == request.note,
            "attribution anchor does not match source/request"
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
                && at_us > 0
                && !request.note.trim().is_empty()
                && request.note.len() <= 4096,
            "invalid native attribution initialization"
        );
        for name in &strategies {
            crate::order_config::validate_strategy_name(name).map_err(anyhow::Error::msg)?;
            ensure!(name != UNALLOCATED, "reserved strategy name");
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
                .all(|name| strategies.contains(name) || name == UNALLOCATED),
            "initial position has unknown strategy"
        );
        for (market, row) in exec_ledger {
            ensure!(
                request.contracts.contains_key(market)
                    && row.keys().all(|name| strategies.contains(name)),
                "Exec records attribute {market} outside the registered roster"
            );
        }
        let mut positions = BTreeMap::new();
        let mut account: BTreeMap<String, Position> = BTreeMap::new();
        let mut targets = BTreeMap::new();
        for name in strategies.iter().map(String::as_str).chain([UNALLOCATED]) {
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
                let lots = initial.map_or(0, |p| p.lots);
                if name != UNALLOCATED {
                    let exec = exec_ledger
                        .get(market)
                        .and_then(|row| row.get(name))
                        .copied()
                        .unwrap_or(0);
                    ensure!(
                        lots == exec,
                        "initial {name} {market} lots {lots} differ from Exec's attributed {exec}"
                    );
                }
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
            positions.insert(name.to_owned(), rows);
            if name != UNALLOCATED {
                targets.insert(name.to_owned(), desired);
            }
        }
        let portfolio = Self {
            source_id: source_id.into(),
            revision: 0,
            baseline_revision: 0,
            decision_us: 0,
            batch_id: None,
            target_trading_day: None,
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
    /// New contracts and members start flat; Exec must not already attribute
    /// lots to a named strategy there (checked against the fold at `anchor`).
    pub fn extend_scope(
        &self,
        request: &ExtendScope,
        anchor: &Anchor,
        exec_ledger: &ExecLedger,
    ) -> Result<Self> {
        require(
            self.revision == request.expected_revision,
            "portfolio revision changed",
        )?;
        anchor.validate()?;
        ensure!(
            anchor.source_id == self.source_id
                && anchor.store_id == self.anchor.store_id
                && anchor.orders_key >= self.anchor.orders_key
                && anchor.snapshot_us > self.anchor.snapshot_us.max(self.decision_us)
                && anchor.note == request.note,
            "scope extension requires a later cut of the same execution store"
        );
        for (market, row) in exec_ledger {
            ensure!(
                !request.contracts.contains_key(market)
                    && row.keys().all(|name| !request.members.contains(name)),
                "Exec already attributes {market} to a strategy being added"
            );
        }
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
                name != UNALLOCATED && !self.targets.contains_key(name),
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

    /// Decision time of the active live batch (one published with a native
    /// trading day). Exec targets have no lease: the plan stands until replaced.
    pub(crate) fn exec_decision_us(&self) -> Option<i64> {
        self.target_trading_day.as_ref()?;
        let group = self
            .strategy_groups
            .get(self.last_strategy_group.as_ref()?)?;
        Some(group.decision_us)
    }

    pub fn execution_plan(&self) -> Result<ExecutionPlan> {
        Ok(ExecutionPlan {
            source_id: self.source_id.clone(),
            batch_id: self.batch_id.clone(),
            target_revision: self.target_revision,
            account_revision: self.revision,
            strategies: self.targets.clone(),
            net_targets: self.net_targets()?,
        })
    }

    /// Signed account target per contract: the sum of all strategy targets.
    pub fn net_targets(&self) -> Result<Quantities> {
        self.contracts
            .keys()
            .map(|market| {
                let target = self.targets.values().try_fold(0i64, |n, rows| {
                    n.checked_add(rows[market])
                        .context("aggregate target overflow")
                })?;
                validate_lots(target)?;
                Ok((market.clone(), target))
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
    /// Like crypto, a new batch replaces targets at any time; Exec owns execution.
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
        let mut next = self.clone();
        next.targets.extend(request.strategies.clone());
        next.strategy_groups
            .get_mut(&request.strategy_group)
            .unwrap()
            .decision_us = request.decision_us;
        next.last_strategy_group = Some(request.strategy_group.clone());
        next.revision = next.revision.checked_add(1).context("revision overflow")?;
        next.target_revision = next.revision;
        next.decision_us = self.decision_us.max(request.decision_us);
        next.batch_id = Some(request.batch_id.clone());
        next.target_trading_day = request.trading_day.clone();
        let receipt = BatchReceipt {
            strategy_group: request.strategy_group.clone(),
            source_id: self.source_id.clone(),
            batch_id: request.batch_id.clone(),
            revision: next.revision,
            decision_us: request.decision_us,
            net_targets: next.net_targets()?,
            execution_plan: next.execution_plan()?,
        };
        Ok((next, receipt))
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
