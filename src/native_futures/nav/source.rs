//! Frozen read-only native evidence cuts. Never derive trade/strategy identity
//! from order type, labels, source paths or a nearby timestamp.
use super::*;
use crate::native_futures::{config::Source, reader};
use rocksdb::{DB, IteratorMode};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FILLS: &str = "cn_trade_facts";
fn integer(v: &Value, k: &str) -> Result<i64> {
    v[k].as_i64()
        .with_context(|| format!("native NAV missing {k}"))
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .with_context(|| format!("native NAV missing {k}"))
}
pub use crate::native_futures::evidence::generation;
use crate::native_futures::evidence::{journal_entry as evidence, live_origin};
pub struct Candidate {
    pub anchor: Anchor,
    pub inventory: Vec<(String, String, Side, i64, i64)>,
}
pub fn candidate(
    source: &Source,
    db: &DB,
    snapshot: &reader::Snapshot,
    expected_us: i64,
    note: String,
) -> Result<Candidate> {
    ensure!(
        snapshot.projection_errors == 0,
        "起始持仓尚未完整追平，不能建立净值快照"
    );
    let at = snapshot
        .nav_anchor_observed_us
        .context("没有可用的原生持仓快照")?;
    ensure!(at == expected_us, "持仓快照已变化，请刷新后重新确认");
    let seq = snapshot
        .nav_anchor_evidence_sequence
        .context("missing native state cut")?;
    let raw = evidence(db, seq)?;
    let e: Value = serde_json::from_slice(&raw)?;
    ensure!(
        integer(&e, "sequence")? == seq
            && integer(&e, "received_us")? == at
            && e["projection_error"].is_null(),
        "原生快照证据与观察结果冲突"
    );
    ensure!(
        e["value"]["type"] == "reconciled"
            || (e["value"]["type"] == "state" && e["value"]["state"]["ready"] == true),
        "原生快照未完成恢复"
    );
    let state = &e["value"]["state"];
    let day = text(state, "trading_day")?;
    reference::valid_day(day)?;
    let mut inventory = vec![];
    for p in state["positions"]
        .as_array()
        .context("missing native initial positions")?
    {
        let venue = text(p, "venue")?.to_owned();
        let symbol = text(p, "symbol")?.to_owned();
        super::super::validate_market(&format!("{venue}:{symbol}"))?;
        let side: Side = serde_json::from_value(p["side"].clone())?;
        let today = integer(&p["position"], "today")?;
        let yesterday = integer(&p["position"], "yesterday")?;
        ensure!(
            today >= 0 && yesterday >= 0,
            "negative native gross position"
        );
        if today
            .checked_add(yesterday)
            .context("native position overflow")?
            > 0
        {
            inventory.push((venue, symbol, side, today, yesterday));
        }
    }
    let anchor = Anchor {
        source_id: source.id.clone(),
        snapshot_us: at,
        trading_day: day.into(),
        evidence_sequence: seq,
        evidence_sha256: hex::encode(Sha256::digest(&raw)),
        store_id: generation(source, db)?,
        positions: vec![],
        note,
    };
    anchor.validate()?;
    Ok(Candidate { anchor, inventory })
}
pub fn validate_anchor(source: &Source, db: &DB, anchor: &Anchor) -> Result<()> {
    anchor.validate()?;
    let raw = crate::native_futures::evidence::verify(
        source,
        db,
        &anchor.source_id,
        &anchor.store_id,
        anchor.evidence_sequence,
        &anchor.evidence_sha256,
    )?;
    let value: Value = serde_json::from_slice(&raw)?;
    ensure!(
        integer(&value, "sequence")? == anchor.evidence_sequence
            && integer(&value, "received_us")? == anchor.snapshot_us
            && value["projection_error"].is_null()
            && (value["value"]["type"] == "reconciled"
                || (value["value"]["type"] == "state" && value["value"]["state"]["ready"] == true))
            && text(&value["value"]["state"], "trading_day")? == anchor.trading_day,
        "起始快照时刻、交易日或恢复状态与原生证据不一致"
    );
    let mut original = vec![];
    for p in value["value"]["state"]["positions"]
        .as_array()
        .context("missing anchor evidence positions")?
    {
        let today = integer(&p["position"], "today")?;
        let yesterday = integer(&p["position"], "yesterday")?;
        if today > 0 || yesterday > 0 {
            original.push((
                text(p, "venue")?.to_owned(),
                text(p, "symbol")?.to_owned(),
                serde_json::from_value::<Side>(p["side"].clone())?,
                today,
                yesterday,
            ));
        }
    }
    let mut stored: Vec<_> = anchor
        .positions
        .iter()
        .map(|p| {
            (
                p.venue.clone(),
                p.symbol.clone(),
                p.side,
                p.today,
                p.yesterday,
            )
        })
        .collect();
    original.sort();
    stored.sort();
    ensure!(original == stored, "起始多空今昨仓与持久化原生证据不一致");
    Ok(())
}

pub fn fills(source: &Source, db: &DB, anchor: &Anchor, end_us: i64) -> Result<Vec<Fill>> {
    validate_anchor(source, db, anchor)?;
    let mut fills = vec![];
    scan_fills(
        db,
        anchor.evidence_sequence,
        end_us,
        |key, value, seq, received| {
            ensure!(
                fills.len() < 250_000,
                "native NAV retained fill budget exceeded"
            );
            fills.push(parse_fill(source, db, key, value, seq, received)?);
            Ok(())
        },
    )?;
    Ok(fills)
}

pub(super) fn scan_fills(
    db: &DB,
    after_seq: i64,
    end_us: i64,
    mut read: impl FnMut(&[u8], &Value, i64, i64) -> Result<()>,
) -> Result<()> {
    let mut scanned = 0;
    for item in db.iterator_cf(
        db.cf_handle(FILLS).context("native fills CF")?,
        IteratorMode::Start,
    ) {
        scanned += 1;
        ensure!(scanned <= 2_000_000, "native NAV fill scan budget exceeded");
        let (key, bytes) = item?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let seq = integer(&value, "evidence_seq")?;
        let received = integer(&value, "received_us")?;
        if seq <= after_seq || received > end_us {
            continue;
        }
        read(&key, &value, seq, received)?;
    }
    Ok(())
}

pub(super) fn parse_fill(
    source: &Source,
    db: &DB,
    key: &[u8],
    value: &Value,
    seq: i64,
    received: i64,
) -> Result<Fill> {
    let fill = &value["fill"];
    let id = &fill["identity"];
    ensure!(
        value["commission"].is_null() || value["commission"].as_f64().is_some_and(f64::is_finite),
        "malformed native commission"
    );
    ensure!(
        value["commission_currency"].is_null() || value["commission_currency"].is_string(),
        "malformed native commission currency"
    );
    let commission_cny = if value["commission_currency"] == "CNY" {
        value["commission"].as_f64()
    } else {
        None
    };
    Ok(Fill {
        key: key.to_vec(),
        source_id: source.id.clone(),
        venue: text(id, "venue")?.into(),
        symbol: text(id, "symbol")?.into(),
        trading_day: text(id, "trading_day")?.into(),
        trade_id: text(id, "trade_id")?.into(),
        side: serde_json::from_value(id["side"].clone())?,
        offset: serde_json::from_value(fill["offset"].clone())?,
        lots: integer(fill, "lots")?,
        price: fill["price"].as_f64().context("native fill price")?,
        received_us: received,
        evidence_sequence: seq,
        commission_cny,
        journal_rate: None,
        live_origin: live_origin(source, db, value, seq)?,
    })
}
