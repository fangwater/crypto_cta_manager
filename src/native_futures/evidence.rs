//! Shared, SDK-free verification of immutable native state and live-fill evidence.
use super::{config::Source, reader};
use anyhow::{Context, Result, ensure};
use rocksdb::{DB, Direction, IteratorMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const EVENTS: &str = "cn_trading_events";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub source_id: String,
    pub snapshot_us: i64,
    pub trading_day: String,
    pub evidence_sequence: i64,
    pub evidence_sha256: String,
    pub store_id: String,
    pub note: String,
}
impl Anchor {
    pub fn validate(&self) -> Result<()> {
        super::reference::valid_day(&self.trading_day)?;
        ensure!(
            !self.source_id.is_empty()
                && self.snapshot_us > 0
                && self.evidence_sequence > 0
                && !self.store_id.is_empty()
                && self.evidence_sha256.len() == 64
                && self.evidence_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                && !self.note.trim().is_empty()
                && self.note.len() <= 4096,
            "invalid native state evidence anchor"
        );
        Ok(())
    }
}

pub fn journal_entry(db: &DB, sequence: i64) -> Result<Vec<u8>> {
    ensure!(sequence > 0, "invalid native evidence cut");
    db.get_cf(
        db.cf_handle(EVENTS).context("native evidence CF")?,
        sequence.to_be_bytes(),
    )?
    .context("native anchor evidence is absent from this source")
}

pub fn generation(source: &Source, db: &DB) -> Result<String> {
    if source.sync.is_some() {
        Ok(format!(
            "sync:{}",
            super::sync::replica_status(db, &source.id)?
                .store_id
                .context("replica lacks origin store identity")?
        ))
    } else {
        Ok(format!(
            "native:{}",
            hex::encode(Sha256::digest(std::fs::read(db.path().join("IDENTITY"))?))
        ))
    }
}

pub fn verify(
    source: &Source,
    db: &DB,
    source_id: &str,
    store_id: &str,
    sequence: i64,
    sha256: &str,
) -> Result<Vec<u8>> {
    ensure!(
        source.id == source_id && generation(source, db)? == store_id,
        "native evidence belongs to a different source or execution store generation"
    );
    let raw = journal_entry(db, sequence)?;
    ensure!(
        hex::encode(Sha256::digest(&raw)) == sha256,
        "native anchor evidence has changed"
    );
    Ok(raw)
}

pub fn validate_anchor(source: &Source, db: &DB, anchor: &Anchor) -> Result<()> {
    anchor.validate()?;
    verify(
        source,
        db,
        &anchor.source_id,
        &anchor.store_id,
        anchor.evidence_sequence,
        &anchor.evidence_sha256,
    )?;
    Ok(())
}

pub fn capture_current(
    source: &Source,
    db: &DB,
    snapshot: &reader::Snapshot,
    expected_sequence: i64,
    note: String,
) -> Result<Anchor> {
    ensure!(
        snapshot.source_id == source.id
            && snapshot.snapshot_ready
            && snapshot.snapshot_current_runtime
            && !snapshot.positions_may_lag
            && !snapshot.replication_stale
            && snapshot.projection_errors == 0,
        "current native snapshot is incomplete or has unaccounted fills"
    );
    let sequence = snapshot
        .state_evidence_sequence
        .context("missing current state evidence sequence")?;
    ensure!(
        sequence == expected_sequence,
        "current state evidence changed before capture"
    );
    let at = snapshot
        .state_observed_us
        .context("missing current state observation time")?;
    let raw = journal_entry(db, sequence)?;
    let event: Value = serde_json::from_slice(&raw)?;
    ensure!(
        event["sequence"].as_i64() == Some(sequence)
            && event["received_us"].as_i64() == Some(at)
            && event["projection_error"].is_null()
            && matches!(
                event["value"]["type"].as_str(),
                Some("state" | "reconciled")
            )
            && event["value"]["state"]["ready"] == true
            && event["value"]["state"]["positions"] == Value::Array(snapshot.positions.clone()),
        "current snapshot conflicts with immutable native state evidence"
    );
    let anchor = Anchor {
        source_id: source.id.clone(),
        snapshot_us: at,
        trading_day: event["value"]["state"]["trading_day"]
            .as_str()
            .context("native trading day")?
            .into(),
        evidence_sequence: sequence,
        evidence_sha256: hex::encode(Sha256::digest(&raw)),
        store_id: generation(source, db)?,
        note,
    };
    anchor.validate()?;
    Ok(anchor)
}

fn native_text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    let mut bytes = vec![];
    for item in v.as_array()? {
        let byte = u8::try_from(item.as_u64()?).ok()?;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    String::from_utf8(bytes).ok()
}
pub fn live_origin(source: &Source, db: &DB, record: &Value, seq: i64) -> Result<bool> {
    let original: Value = serde_json::from_slice(&journal_entry(db, seq)?)?;
    let id = &record["fill"]["identity"];
    if original["value"]["type"] != "normalized_trade"
        || original["value"]["identity"] != *id
        || original["value"]["origin"] != "live_report"
    {
        return Ok(false);
    }
    // Bound the raw-report lookup and require every factual identifier/value to
    // agree. An omitted or recovery-only origin stays unclassified/unusable.
    for item in db
        .iterator_cf(
            db.cf_handle(EVENTS).context("events handle")?,
            IteratorMode::From(&seq.to_be_bytes(), Direction::Reverse),
        )
        .take(33)
    {
        let (_, bytes) = item?;
        let raw: Value = serde_json::from_slice(&bytes)?;
        let value = &raw["value"];
        let event = &value["event"];
        if source.adapter == "ctp"
            && value["type"] == "ctp_event"
            && event["received_us"] == record["received_us"]
            && event["exchange"] == id["venue"]
            && event["symbol"] == id["symbol"]
            && event["trading_day"] == id["trading_day"]
            && event["trade_id"] == id["trade_id"]
            && event["system_id"] == record["fill"]["system_id"]
            && event["volume"] == record["fill"]["lots"]
            && event["price"] == record["fill"]["price"]
        {
            let direction = if id["side"] == "buy" { 48 } else { 49 };
            return Ok(event["kind"].as_i64() == Some(10)
                && event["direction"].as_i64() == Some(direction));
        }
        if source.adapter == "yida"
            && value["type"] == "yida_event"
            && value["received_us"] == record["received_us"]
            && native_text(&event["exchange"]).as_deref() == id["venue"].as_str()
            && native_text(&event["symbol"]).as_deref() == id["symbol"].as_str()
            && event["day"].as_i64().map(|v| v.to_string()).as_deref() == id["trading_day"].as_str()
            && event["trade_id"].as_i64().map(|v| v.to_string()).as_deref()
                == id["trade_id"].as_str()
            && event["system_id"]
                .as_i64()
                .map(|v| v.to_string())
                .as_deref()
                == record["fill"]["system_id"].as_str()
            && event["lots"] == record["fill"]["lots"]
            && event["price"] == record["fill"]["price"]
        {
            let side = if id["side"] == "buy" { 0 } else { 1 };
            return Ok(event["kind"].as_i64() == Some(11)
                && event["replay"].as_i64() == Some(0)
                && event["direction"].as_i64() == Some(side));
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
