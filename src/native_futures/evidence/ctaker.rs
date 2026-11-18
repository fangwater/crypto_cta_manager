//! Follow the Exec-written order key and immutable prepare pointer, never an
//! OrderRef alone or a time/symbol search.
use super::{Source, journal_entry, verified_live_origin};
use anyhow::{Context, Result, ensure};
use rocksdb::DB;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NativeKey {
    session: String,
    order_ref: String,
}

// Field order matches Exec's serialized cn_uniform_orders key.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeOrderIdentity {
    trading_day: String,
    venue: String,
    native: NativeKey,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CtakerTarget {
    pub source_id: String,
    pub batch_id: String,
    pub target_revision: i64,
    pub trading_day: String,
    pub expires_at_us: i64,
    pub net_targets: BTreeMap<String, i64>,
}

#[derive(Debug, Serialize)]
pub struct CtakerFillProof {
    pub target: CtakerTarget,
    pub order: NativeOrderIdentity,
    pub prepared_sequence: i64,
    pub prepared_sha256: String,
}

/// Old/unlinked fills and manual orders remain available for explicit manual
/// allocation. A claimed link with conflicting evidence is rejected.
pub fn ctaker_fill_proof(
    source: &Source,
    db: &DB,
    record: &Value,
) -> Result<Option<CtakerFillProof>> {
    let fill_seq = record["evidence_seq"]
        .as_i64()
        .context("fill evidence sequence")?;
    let normalized: Value = serde_json::from_slice(&journal_entry(db, fill_seq)?)?;
    let link = &normalized["value"]["order_link"];
    if link.is_null() {
        return Ok(None);
    }
    let order: NativeOrderIdentity = serde_json::from_value(link.clone())?;
    let fill = &record["fill"];
    let id = &fill["identity"];
    ensure!(
        normalized["sequence"].as_i64() == Some(fill_seq)
            && normalized["projection_error"].is_null()
            && id["trading_day"] == order.trading_day
            && id["venue"] == order.venue
            && fill["order_ref"] == order.native.order_ref
            && !order.native.order_ref.is_empty()
            && order
                .native
                .session
                .starts_with(&format!("{}:", source.adapter))
            && fill["system_id"].as_str().is_some_and(|id| !id.is_empty())
            && verified_live_origin(source, db, record, fill_seq, true)?,
        "invalid immutable native fill/order link"
    );
    let bytes = db
        .get_cf(
            db.cf_handle("cn_uniform_orders")
                .context("native orders CF")?,
            serde_json::to_vec(&order)?,
        )?
        .context("linked native order projection is absent")?;
    let projection: Value = serde_json::from_slice(&bytes)?;
    let request = &projection["order"]["request"];
    ensure!(
        projection["identity"] == *link
            && projection["order"]["key"] == link["native"]
            && projection["order"]["system_id"] == fill["system_id"]
            && request["venue"] == id["venue"]
            && request["symbol"] == id["symbol"]
            && request["side"] == id["side"]
            && request["offset"] == fill["offset"],
        "native order projection conflicts with immutable fill link"
    );
    if projection["prepared_evidence_seq"].is_null() {
        return Ok(None);
    }
    let seq = projection["prepared_evidence_seq"]
        .as_i64()
        .context("invalid native prepare sequence")?;
    ensure!(
        (1..fill_seq).contains(&seq),
        "native prepare does not precede the fill"
    );
    let raw = journal_entry(db, seq)?;
    let prepared: Value = serde_json::from_slice(&raw)?;
    let submit = &prepared["value"];
    ensure!(
        prepared["sequence"].as_i64() == Some(seq)
            && prepared["projection_error"].is_null()
            && submit["type"] == "prepared_submit"
            && submit["key"] == link["native"]
            && projection["locally_submitted"] == true
            && submit["intent"].is_object()
            && submit["intent"] == projection["submitted_intent"],
        "native prepare evidence conflicts with linked order"
    );
    if submit["ctaker_target"].is_null() {
        return Ok(None);
    }
    let target: CtakerTarget = serde_json::from_value(submit["ctaker_target"].clone())?;
    let at = submit["prepared_at_us"]
        .as_i64()
        .context("ctaker prepare time")?;
    let symbol = id["symbol"].as_str().context("ctaker fill symbol")?;
    let lots = fill["lots"].as_i64().context("ctaker fill lots")?;
    super::super::netting::valid_id(&target.batch_id)?;
    ensure!(
        target.source_id == source.id
            && target.trading_day == order.trading_day
            && target.target_revision > 0
            && at > 0
            && at < target.expires_at_us
            && record["received_us"]
                .as_i64()
                .is_some_and(|received| received >= at)
            && target
                .net_targets
                .contains_key(&format!("{}:{symbol}", order.venue))
            && submit["intent"]["venue"] == id["venue"]
            && submit["intent"]["symbol"] == id["symbol"]
            && submit["intent"]["side"] == id["side"]
            && submit["intent"]["offset"] == fill["offset"]
            && lots > 0
            && submit["intent"]["lots"]
                .as_i64()
                .is_some_and(|requested| requested >= lots),
        "ctaker batch proof conflicts with the factual fill"
    );
    Ok(Some(CtakerFillProof {
        target,
        order,
        prepared_sequence: seq,
        prepared_sha256: hex::encode(Sha256::digest(&raw)),
    }))
}
