//! Only immutable native initial-position anchors live in PostgreSQL.
use super::*;
use sqlx::{PgPool, types::Json};

pub async fn insert(pool: &PgPool, anchor: &Anchor, created_by: &str) -> Result<()> {
    anchor.validate()?;
    ensure!(
        !created_by.trim().is_empty(),
        "native snapshot creator is required"
    );
    let mut tx = pool.begin().await?;
    let result=sqlx::query("INSERT INTO cn_futures_nav_snapshots(source_id,snapshot_us,anchor,created_by) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(&anchor.source_id).bind(anchor.snapshot_us).bind(Json(anchor)).bind(created_by).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        let Json(stored): Json<Anchor> = sqlx::query_scalar(
            "SELECT anchor FROM cn_futures_nav_snapshots WHERE source_id=$1 AND snapshot_us=$2",
        )
        .bind(&anchor.source_id)
        .bind(anchor.snapshot_us)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            serde_json::to_value(stored)? == serde_json::to_value(anchor)?,
            "该时刻已有不同净值快照；不可覆盖历史记录"
        );
    }
    tx.commit().await?;
    Ok(())
}
pub async fn list(pool: &PgPool, source: &str) -> Result<Vec<Anchor>> {
    let rows:Vec<Json<Anchor>>=sqlx::query_scalar("SELECT anchor FROM cn_futures_nav_snapshots WHERE source_id=$1 ORDER BY snapshot_us DESC LIMIT 200")
        .bind(source).fetch_all(pool).await?;
    rows.into_iter()
        .map(|Json(anchor)| {
            anchor.validate()?;
            ensure!(anchor.source_id == source, "native anchor source mismatch");
            Ok(anchor)
        })
        .collect()
}
pub async fn load(pool: &PgPool, source: &str, at_us: i64) -> Result<Option<Anchor>> {
    let row:Option<Json<Anchor>>=sqlx::query_scalar("SELECT anchor FROM cn_futures_nav_snapshots WHERE source_id=$1 AND snapshot_us<=$2 ORDER BY snapshot_us DESC LIMIT 1")
        .bind(source).bind(at_us).fetch_optional(pool).await?;
    row.map(|Json(anchor)| {
        anchor.validate()?;
        ensure!(anchor.source_id == source, "native anchor source mismatch");
        Ok(anchor)
    })
    .transpose()
}

pub async fn first(pool: &PgPool, source: &str) -> Result<Option<Anchor>> {
    let anchor: Option<Json<Anchor>> = sqlx::query_scalar(
        "SELECT anchor FROM cn_futures_nav_snapshots WHERE source_id=$1 ORDER BY snapshot_us LIMIT 1",
    ).bind(source).fetch_optional(pool).await?;
    anchor
        .map(|Json(anchor)| {
            anchor.validate()?;
            ensure!(anchor.source_id == source, "native anchor source mismatch");
            Ok(anchor)
        })
        .transpose()
}
