//! A portfolio row lock serializes target batches, anchors and scope changes.
//! State, receipts and catalog targets commit together. Exec owns attribution.
use super::*;
use sqlx::{PgPool, Postgres, Row, Transaction};

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
        "SELECT e.request,e.receipt FROM cta_native_netting_events e
         JOIN cta_native_netting_portfolios p USING(source_id)
         WHERE source_id=$1 AND event_id=$2
           AND e.revision >= COALESCE((p.state->>'baseline_revision')::bigint, 0)",
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
    let row = sqlx::query(
        "SELECT e.fingerprint,e.receipt,
         e.revision >= COALESCE((p.state->>'baseline_revision')::bigint, 0) AS current
         FROM cta_native_netting_events e JOIN cta_native_netting_portfolios p USING(source_id)
         WHERE source_id=$1 AND event_id=$2",
    )
    .bind(source)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|r| {
        require(
            r.try_get::<bool, _>("current")? && r.try_get::<String, _>("fingerprint")? == hash,
            "event id belongs to a prior baseline or has different content",
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
    sqlx::query("INSERT INTO cta_native_netting_events(source_id,event_id,revision,kind,fingerprint,batch_id,factual_identity,request,receipt,actor_user_id,received_at_us) VALUES($1,$2,$3,$4,$5,$6,NULL,$7,$8,$9,$10)")
        .bind(&state.source_id).bind(id).bind(state.revision).bind(kind).bind(hash).bind(batch)
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
            user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}

pub async fn extend_scope(
    pool: &PgPool,
    source: &str,
    request: &ExtendScope,
    anchor: &Anchor,
    exec_ledger: &ExecLedger,
    user_id: i64,
) -> Result<serde_json::Value> {
    valid_id(&request.event_id)?;
    let id = format!("scope:{}", request.event_id);
    let hash = fingerprint(request)?;
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    let next = state.extend_scope(request, anchor, exec_ledger)?;
    for name in &request.members {
        // A name deleted from this account may come back, in any group.
        let result = sqlx::query("INSERT INTO cta_native_netting_members(source_id,strategy_group,strategy_name) VALUES($1,$2,$3) ON CONFLICT(strategy_name) DO UPDATE SET strategy_group=EXCLUDED.strategy_group WHERE cta_native_netting_members.source_id=EXCLUDED.source_id")
            .bind(source).bind(&request.strategy_group).bind(name).execute(&mut *tx).await?;
        require(
            result.rows_affected() == 1,
            "strategy already belongs to a native portfolio",
        )?;
    }
    catalog(&mut tx, &next).await?;
    let receipt = serde_json::json!({
        "kind":"scope_extension","source_id":source,"revision":next.revision,
        "strategy_group":request.strategy_group,"members":request.members,
        "contracts":request.contracts,"anchor":anchor,
    });
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind: "scope_extension",
            hash: &hash,
            batch: None,
            user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(receipt)
}

#[allow(clippy::too_many_arguments)] // One rebase transaction: identity, revision, anchor and opening state.
pub(in crate::native_futures) async fn rebase(
    pool: &PgPool,
    source: &str,
    request: &Rebase,
    nav: &super::super::nav::Anchor,
    anchor: Anchor,
    exec_ledger: &ExecLedger,
    user_id: i64,
    username: &str,
) -> Result<serde_json::Value> {
    valid_id(&request.event_id)?;
    let id = format!("rebase:{}", request.event_id);
    let hash = fingerprint(request)?;
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    let next = state.rebase(request, nav, anchor, exec_ledger)?;
    let retired: Vec<_> = state
        .targets
        .keys()
        .filter(|name| !next.targets.contains_key(*name))
        .collect();
    if !retired.is_empty() {
        // Reserve the historical strategies' ownership and keep their original
        // allocation receipts. Only the current target catalog is cleared.
        sqlx::query("UPDATE cta_position_strategies SET targets='{}'::jsonb,updated_at_us=$2 WHERE strategy_name = ANY($1)")
            .bind(&retired)
            .bind(super::super::now_us())
            .execute(&mut *tx).await?;
    }
    catalog(&mut tx, &next).await?;
    super::super::nav::store::insert_in_transaction(&mut tx, nav, username).await?;
    let receipt = serde_json::to_value(&next)?;
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind: "initialize",
            hash: &hash,
            batch: None,
            user_id,
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
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind: "target_batch",
            hash: &hash,
            batch: Some(&request.batch_id),
            user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(serde_json::to_value(receipt)?)
}

/// 停止并清仓: zero targets and a new live plan, committed with the catalog.
/// 停止并清仓 (`resume == false`) or 恢复 (`resume == true`).
pub async fn stop(
    pool: &PgPool,
    source: &str,
    request: &MemberChange,
    resume: bool,
    user_id: i64,
) -> Result<serde_json::Value> {
    valid_id(&request.event_id)?;
    let (prefix, kind) = if resume {
        ("resume", "strategy_resume")
    } else {
        ("stop", "strategy_stop")
    };
    let id = format!("{prefix}:{}", request.event_id);
    let hash = fingerprint(request)?;
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    let now = super::super::now_us();
    let (next, receipt) = if resume {
        state.resume(request, now)?
    } else {
        state.stop(request, now)?
    };
    catalog(&mut tx, &next).await?;
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind,
            hash: &hash,
            batch: next.batch_id.as_deref(),
            user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(serde_json::to_value(receipt)?)
}

/// 删除配置: removes stopped members (and an emptied group with its publish
/// tokens). The names stay owned by this account, so the attribution history
/// keeps a single owner, and extend-scope may add them back.
pub async fn retire(
    pool: &PgPool,
    source: &str,
    request: &MemberChange,
    cut: &Anchor,
    user_id: i64,
) -> Result<serde_json::Value> {
    valid_id(&request.event_id)?;
    let id = format!("retire:{}", request.event_id);
    let hash = fingerprint(request)?;
    let mut tx = pool.begin().await?;
    let state = locked(&mut tx, source).await?;
    if let Some(previous) = existing(&mut tx, source, &id, &hash).await? {
        return Ok(previous);
    }
    let (next, receipt) = state.retire(request, cut, super::super::now_us())?;
    let retired: Vec<_> = request.members.iter().collect();
    sqlx::query("UPDATE cta_position_strategies SET targets='{}'::jsonb,updated_at_us=$2 WHERE strategy_name = ANY($1)")
        .bind(&retired)
        .bind(super::super::now_us())
        .execute(&mut *tx)
        .await?;
    if let Some(group) = &receipt.removed_group {
        sqlx::query(
            "DELETE FROM cta_native_netting_publishers WHERE source_id=$1 AND strategy_group=$2",
        )
        .bind(source)
        .bind(group)
        .execute(&mut *tx)
        .await?;
    }
    catalog(&mut tx, &next).await?;
    save(
        &mut tx,
        &next,
        EventMeta {
            id: &id,
            kind: "strategy_retire",
            hash: &hash,
            batch: next.batch_id.as_deref(),
            user_id,
        },
        request,
        &receipt,
    )
    .await?;
    tx.commit().await?;
    Ok(serde_json::to_value(receipt)?)
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
    let rows=sqlx::query("SELECT event_id,revision,kind,receipt,actor_user_id,received_at_us FROM cta_native_netting_events
        JOIN cta_native_netting_portfolios USING(source_id)
        WHERE source_id=$1 AND revision>$2 AND revision >= COALESCE((state->>'baseline_revision')::bigint, 0)
        ORDER BY revision LIMIT $3")
        .bind(source).bind(after).bind(limit).fetch_all(pool).await?;
    rows.into_iter().map(|r|Ok(serde_json::json!({"event_id":r.try_get::<String,_>("event_id")?,"revision":r.try_get::<i64,_>("revision")?,
        "kind":r.try_get::<String,_>("kind")?,"receipt":r.try_get::<serde_json::Value,_>("receipt")?,
        "actor_user_id":r.try_get::<i64,_>("actor_user_id")?,"received_at_us":r.try_get::<i64,_>("received_at_us")?}))).collect()
}
