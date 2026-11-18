//! A portfolio row lock serializes target batches and factual allocations.
//! State, paired internal transfers, demand budgets and catalog targets commit together.
use super::*;
use crate::native_futures::evidence::CtakerFillProof;
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllocateFill {
    pub event_id: String,
    pub batch_id: String,
    pub expected_revision: i64,
    /// Exact cn_trade_facts RocksDB key, hex encoded. No caller-supplied price/fee.
    pub evidence_key: String,
    pub allocations: Quantities,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllocateCtakerFill {
    pub evidence_key: String,
}

impl AllocateCtakerFill {
    pub fn id(&self) -> Result<String> {
        Ok(format!("ctaker-fill:{}", fingerprint(&self.evidence_key)?))
    }
}

pub async fn load(pool: &PgPool, source: &str) -> Result<Option<Portfolio>> {
    sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT state FROM cta_native_netting_portfolios WHERE source_id=$1",
    )
    .bind(source)
    .fetch_optional(pool)
    .await?
    .map(serde_json::from_value)
    .transpose()
    .context("decode native attribution portfolio")
}

/// An acknowledged immutable event remains replayable without re-reading a
/// newer execution generation or a subsequently corrected raw fill record.
pub async fn replay(
    pool: &PgPool,
    source: &str,
    id: &str,
    request: &impl Serialize,
) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query(
        "SELECT request,receipt FROM cta_native_netting_events WHERE source_id=$1 AND event_id=$2",
    )
    .bind(source)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        require(
            row.try_get::<serde_json::Value, _>("request")? == serde_json::to_value(request)?,
            "event id already exists with different content",
        )?;
        Ok(row.try_get("receipt")?)
    })
    .transpose()
}

async fn locked(tx: &mut Transaction<'_, Postgres>, source: &str) -> Result<Portfolio> {
    let value: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT state FROM cta_native_netting_portfolios WHERE source_id=$1 FOR UPDATE",
    )
    .bind(source)
    .fetch_optional(&mut **tx)
    .await?;
    serde_json::from_value(value.context("initialize the source attribution portfolio first")?)
        .context("decode locked native portfolio")
}

async fn existing(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    id: &str,
    hash: &str,
) -> Result<Option<serde_json::Value>> {
    let row=sqlx::query("SELECT fingerprint,receipt FROM cta_native_netting_events WHERE source_id=$1 AND event_id=$2")
        .bind(source).bind(id).fetch_optional(&mut **tx).await?;
    row.map(|r| {
        require(
            r.try_get::<String, _>("fingerprint")? == hash,
            "event id already exists with different content",
        )?;
        Ok(r.try_get("receipt")?)
    })
    .transpose()
}

struct EventMeta<'a> {
    id: &'a str,
    kind: &'a str,
    hash: &'a str,
    batch: Option<&'a str>,
    factual_identity: Option<&'a str>,
    user_id: i64,
}

async fn save(
    tx: &mut Transaction<'_, Postgres>,
    state: &Portfolio,
    metadata: EventMeta<'_>,
    request: &impl Serialize,
    receipt: &impl Serialize,
) -> Result<()> {
    let EventMeta {
        id,
        kind,
        hash,
        batch,
        factual_identity,
        user_id,
    } = metadata;
    sqlx::query(
        "UPDATE cta_native_netting_portfolios SET state=$2,updated_at_us=$3 WHERE source_id=$1",
    )
    .bind(&state.source_id)
    .bind(serde_json::to_value(state)?)
    .bind(super::super::now_us())
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO cta_native_netting_events(source_id,event_id,revision,kind,fingerprint,batch_id,factual_identity,request,receipt,actor_user_id,received_at_us) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(&state.source_id).bind(id).bind(state.revision).bind(kind).bind(hash).bind(batch).bind(factual_identity)
        .bind(serde_json::to_value(request)?).bind(serde_json::to_value(receipt)?).bind(user_id).bind(super::super::now_us()).execute(&mut **tx).await?;
    Ok(())
}

async fn catalog(tx: &mut Transaction<'_, Postgres>, state: &Portfolio) -> Result<()> {
    for (name, rows) in &state.targets {
        let targets: BTreeMap<_, _> = rows
            .iter()
            .map(|(m, q)| (m, serde_json::json!({"qty":q,"signal":0})))
            .collect();
        let result = sqlx::query(
            "UPDATE cta_position_strategies SET targets=$2,updated_at_us=$3 WHERE strategy_name=$1",
        )
        .bind(name)
        .bind(serde_json::to_value(targets)?)
        .bind(super::super::now_us())
        .execute(&mut **tx)
        .await?;
        ensure!(
            result.rows_affected() == 1,
            "registered strategy disappeared"
        );
    }
    Ok(())
}

pub async fn initialize(
    pool: &PgPool,
    state: &Portfolio,
    request: &Initialize,
    user_id: i64,
) -> Result<serde_json::Value> {
    let hash = fingerprint(request)?;
    let mut tx = pool.begin().await?;
    let inserted=sqlx::query("INSERT INTO cta_native_netting_portfolios(source_id,state,updated_at_us) VALUES($1,$2,$3) ON CONFLICT(source_id) DO NOTHING")
        .bind(&state.source_id).bind(serde_json::to_value(state)?).bind(super::super::now_us()).execute(&mut *tx).await?;
    let _ = locked(&mut tx, &state.source_id).await?;
    if let Some(previous) = existing(&mut tx, &state.source_id, "initialize", &hash).await? {
        return Ok(previous);
    }
    require(
        inserted.rows_affected() == 1,
        "portfolio already initialized; implicit ledger replacement is forbidden",
    )?;
    // A strategy name belongs to one native account portfolio. Shared templates
    // across accounts require separately named attribution strategies.
    for (group, settings) in &state.strategy_groups {
        for name in &settings.members {
            let result=sqlx::query("INSERT INTO cta_native_netting_members(source_id,strategy_group,strategy_name) VALUES($1,$2,$3) ON CONFLICT(strategy_name) DO NOTHING")
            .bind(&state.source_id).bind(group).bind(name).execute(&mut *tx).await?;
            require(
                result.rows_affected() == 1,
                "strategy already belongs to a native portfolio",
            )?;
        }
    }
    catalog(&mut tx, state).await?;
    let receipt = serde_json::to_value(state)?;
    save(
        &mut tx,
        state,
        EventMeta {
            id: "initialize",
            kind: "initialize",
            hash: &hash,
            batch: None,
            factual_identity: None,
            user_id: user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}

pub async fn batch(
    pool: &PgPool,
    source: &str,
    request: &TargetBatch,
    user_id: i64,
) -> Result<serde_json::Value> {
    valid_id(&request.batch_id)?;
    let hash = fingerprint(request)?;
    let id = format!("batch:{}", request.batch_id);
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    let (next, receipt) = state.batch(request, super::super::now_us())?;
    // All rows become visible at commit. No per-strategy execution notification.
    catalog(&mut tx, &next).await?;
    for (strategy, rows) in &receipt.external_demands {
        for (market, qty) in rows {
            sqlx::query("INSERT INTO cta_native_netting_demands(source_id,batch_id,strategy_name,market,remaining_lots) VALUES($1,$2,$3,$4,$5)")
                .bind(source).bind(&request.batch_id).bind(strategy).bind(market).bind(qty).execute(&mut *tx).await?;
        }
    }
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind: "target_batch",
            hash: &hash,
            batch: Some(&request.batch_id),
            factual_identity: None,
            user_id: user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(serde_json::to_value(receipt)?)
}

pub async fn allocate(
    pool: &PgPool,
    source: &str,
    request: &AllocateFill,
    fill: &FactualFill,
    user_id: i64,
    proof: Option<&CtakerFillProof>,
) -> Result<serde_json::Value> {
    valid_id(&request.event_id)?;
    valid_id(&request.batch_id)?;
    let hash = fingerprint(&(request, fill))?;
    let id = format!("fill:{}", request.event_id);
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    require(
        state.revision == request.expected_revision,
        "portfolio changed; reload before allocating a fill",
    )?;
    let receipt = write_allocation(
        &mut tx,
        &state,
        &request.evidence_key,
        &request.allocations,
        fill,
        proof,
        EventMeta {
            id: &id,
            kind: "exchange_fill_allocation",
            hash: &hash,
            batch: Some(&request.batch_id),
            factual_identity: Some(&fill.identity),
            user_id,
        },
        request,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}

pub async fn allocate_ctaker(
    pool: &PgPool,
    source: &str,
    request: &AllocateCtakerFill,
    fill: &FactualFill,
    proof: &CtakerFillProof,
    user_id: i64,
) -> Result<serde_json::Value> {
    let id = request.id()?;
    let hash = fingerprint(request)?;
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    require(
        state.external_demand_open
            && state.batch_id.as_deref() == Some(proof.target.batch_id.as_str())
            && state.target_revision == proof.target.target_revision,
        "the proven ctaker order has no active frozen demand",
    )?;
    let allocations = ctaker_allocations(&state, fill)?;
    let receipt = write_allocation(
        &mut tx,
        &state,
        &request.evidence_key,
        &allocations,
        fill,
        Some(proof),
        EventMeta {
            id: &id,
            kind: "exchange_fill_allocation",
            hash: &hash,
            batch: Some(&proof.target.batch_id),
            factual_identity: Some(&fill.identity),
            user_id,
        },
        request,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}

fn ctaker_allocations(state: &Portfolio, fill: &FactualFill) -> Result<Quantities> {
    validate_lots(fill.lots)?;
    ensure!(fill.lots != 0, "invalid zero-lot ctaker fill");
    let mut left = fill.lots.abs();
    let sign = fill.lots.signum();
    let mut allocations = Quantities::new();
    // Split the account fill in strategy-name order over the declared batch demands.
    for (strategy, demands) in state.external_demands() {
        let qty = demands.get(&fill.market).copied().unwrap_or_default();
        if qty.signum() == sign {
            let take = left.min(qty.abs());
            allocations.insert(strategy, take * sign);
            left -= take;
            if left == 0 {
                break;
            }
        }
    }
    ensure!(
        left == 0,
        "proven ctaker fill exceeds frozen same-side demand"
    );
    Ok(allocations)
}

#[allow(clippy::too_many_arguments)]
async fn write_allocation(
    tx: &mut Transaction<'_, Postgres>,
    state: &Portfolio,
    evidence_key: &str,
    allocations: &Quantities,
    fill: &FactualFill,
    proof: Option<&CtakerFillProof>,
    metadata: EventMeta<'_>,
    request: &impl Serialize,
) -> Result<serde_json::Value> {
    let source = &state.source_id;
    let batch_id = metadata.batch.context("missing allocation batch")?;
    let prior:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cta_native_netting_events WHERE source_id=$1 AND factual_identity=$2)")
        .bind(source).bind(&fill.identity).fetch_one(&mut **tx).await?;
    require(!prior, "factual fill has already been allocated")?;
    let batch:Option<serde_json::Value>=sqlx::query_scalar("SELECT receipt FROM cta_native_netting_events WHERE source_id=$1 AND event_id=$2 AND kind='target_batch'")
        .bind(source).bind(format!("batch:{batch_id}")).fetch_optional(&mut **tx).await?;
    let batch: BatchReceipt =
        serde_json::from_value(batch.context("unknown fill attribution batch")?)?;
    ensure!(
        fill.received_us >= batch.decision_us,
        "fill precedes the referenced decision"
    );
    if let Some(proof) = proof {
        let target = &proof.target;
        ensure!(
            &target.source_id == source
                && target.batch_id == batch_id
                && target.target_revision == batch.execution_plan.target_revision
                && target.net_targets == batch.execution_plan.net_targets
                && proof.prepared_sequence > state.anchor.evidence_sequence,
            "ctaker order proof differs from the immutable Manager batch"
        );
    }
    for (name, qty) in allocations {
        validate_lots(*qty)?;
        let remaining:Option<i64>=sqlx::query_scalar("SELECT remaining_lots FROM cta_native_netting_demands WHERE source_id=$1 AND batch_id=$2 AND strategy_name=$3 AND market=$4 FOR UPDATE")
            .bind(source).bind(batch_id).bind(name).bind(&fill.market).fetch_optional(&mut **tx).await?;
        let remaining = remaining.context("strategy has no external demand in this batch")?;
        ensure!(
            *qty != 0 && qty.signum() == remaining.signum() && qty.abs() <= remaining.abs(),
            "fill allocation exceeds this strategy's remaining external demand"
        );
        sqlx::query("UPDATE cta_native_netting_demands SET remaining_lots=$5 WHERE source_id=$1 AND batch_id=$2 AND strategy_name=$3 AND market=$4")
            .bind(source).bind(batch_id).bind(name).bind(&fill.market).bind(remaining-qty).execute(&mut **tx).await?;
    }
    let next = state.fill(fill, allocations)?;
    let mut receipt = serde_json::json!({"kind":"exchange_fill_allocation","source_id":source,"revision":next.revision,
        "batch_id":batch_id,"evidence_key":evidence_key,"fill":fill,"allocations":allocations,"net_targets":next.net_targets()?});
    if let Some(proof) = proof {
        receipt["ctaker_order_proof"] = serde_json::to_value(proof)?;
    }
    save(tx, &next, metadata, request, &receipt).await?;
    Ok(receipt)
}

pub async fn history(
    pool: &PgPool,
    source: &str,
    after: i64,
    limit: i64,
) -> Result<Vec<serde_json::Value>> {
    ensure!(
        after >= -1 && (1..=100).contains(&limit),
        "invalid attribution history range"
    );
    let rows=sqlx::query("SELECT event_id,revision,kind,receipt,actor_user_id,received_at_us FROM cta_native_netting_events WHERE source_id=$1 AND revision>$2 ORDER BY revision LIMIT $3")
        .bind(source).bind(after).bind(limit).fetch_all(pool).await?;
    rows.into_iter().map(|r|Ok(serde_json::json!({"event_id":r.try_get::<String,_>("event_id")?,"revision":r.try_get::<i64,_>("revision")?,
        "kind":r.try_get::<String,_>("kind")?,"receipt":r.try_get::<serde_json::Value,_>("receipt")?,
        "actor_user_id":r.try_get::<i64,_>("actor_user_id")?,"received_at_us":r.try_get::<i64,_>("received_at_us")?}))).collect()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseDemand {
    pub event_id: String,
    pub expected_revision: i64,
    pub note: String,
}

/// Caller supplies a freshly verified, order-free native snapshot, not an
/// operator assertion that a timeout/cancel probably succeeded.
pub async fn release(
    pool: &PgPool,
    source: &str,
    request: &ReleaseDemand,
    proof: &super::super::evidence::Anchor,
    account_lots: &Quantities,
    user_id: i64,
) -> Result<serde_json::Value> {
    valid_id(&request.event_id)?;
    ensure!(
        !request.note.trim().is_empty() && request.note.len() <= 4096,
        "release requires an audit note"
    );
    let hash = fingerprint(request)?;
    let id = format!("release:{}", request.event_id);
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    require(
        state.revision == request.expected_revision,
        "portfolio changed before demand release",
    )?;
    require(
        state.external_demand_open,
        "there is no unsettled external demand",
    )?;
    proof.validate()?;
    ensure!(
        proof.source_id == source
            && proof.store_id == state.anchor.store_id
            && proof.snapshot_us > state.decision_us
            && proof.evidence_sequence > state.anchor.evidence_sequence,
        "release requires a later snapshot of the same execution store"
    );
    for market in state.contracts.keys() {
        ensure!(
            state.account[market].quantity() == *account_lots.get(market).unwrap_or(&0),
            "unallocated or missing factual fills block demand release for {market}"
        );
    }
    let mut next = state.clone();
    next.external_demand_open = false;
    let group = next
        .last_strategy_group
        .clone()
        .context("missing active strategy group")?;
    next.strategy_groups
        .get_mut(&group)
        .context("missing active group state")?
        .held = true;
    next.revision = next.revision.checked_add(1).context("revision overflow")?;
    sqlx::query("UPDATE cta_native_netting_demands SET remaining_lots=0 WHERE source_id=$1 AND remaining_lots<>0")
        .bind(source).execute(&mut *tx).await?;
    let receipt = serde_json::json!({"kind":"external_demand_release","source_id":source,"revision":next.revision,
        "batch_id":state.batch_id,"note":request.note,"proof":proof,"account_lots":account_lots,
        "external_demand_open":false,"targets_preserved":true,"positions_preserved":true});
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind: "external_demand_release",
            hash: &hash,
            batch: state.batch_id.as_deref(),
            factual_identity: None,
            user_id: user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}
