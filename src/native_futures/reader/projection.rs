use super::*;
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Clone, Default)]
pub(super) struct Projection {
    pub last_key: Option<Vec<u8>>,
    pub last_sequence: i64,
    last_event_us: Option<i64>,
    errors: usize,
    latest_start: Option<i64>,
    transport: Option<(i64, bool, i64)>,
    guard: Option<(i64, &'static str, String)>,
    runtime: Option<(bool, i64)>,
    state: Option<State>,
    reconciled: Option<(i64, i64)>,
    funds: super::funds::FundsProjection,
}
#[derive(Clone)]
struct State {
    sequence: i64,
    received: i64,
    ready: bool,
    can_trade: Option<bool>,
    reason: String,
    day: Option<String>,
    budget: Option<f64>,
    positions: Vec<Value>,
}
#[derive(Deserialize)]
pub(super) struct Envelope<'a> {
    pub(super) sequence: i64,
    received_us: i64,
    #[serde(default, borrow)]
    pub(super) projection_error: Option<&'a RawValue>,
    #[serde(borrow)]
    pub(super) value: Evidence<'a>,
}
#[derive(Deserialize)]
pub(super) struct Evidence<'a> {
    #[serde(rename = "type", borrow)]
    pub(super) kind: &'a str,
    #[serde(default, borrow)]
    pub(super) event: Option<&'a RawValue>,
    #[serde(default, borrow)]
    pub(super) state: Option<&'a RawValue>,
    #[serde(default, borrow)]
    pub(super) received_us: Option<&'a RawValue>,
    #[serde(default, borrow)]
    pub(super) shutdown_tail: Option<&'a RawValue>,
    #[serde(default, borrow)]
    ready: Option<&'a RawValue>,
    #[serde(default, borrow)]
    transport_ready: Option<&'a RawValue>,
    #[serde(default, borrow)]
    can_trade: Option<&'a RawValue>,
    #[serde(default, borrow)]
    error: Option<&'a RawValue>,
    #[serde(default, borrow)]
    reason: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct Connection {
    #[serde(default)]
    kind: Option<i64>,
    #[serde(default)]
    replay: Option<i64>,
}
pub(super) fn boolean(raw: Option<&RawValue>) -> Option<bool> {
    match raw.map(RawValue::get) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}
fn message(raw: Option<&RawValue>, fallback: &str) -> String {
    raw.and_then(|v| serde_json::from_str::<String>(v.get()).ok())
        .unwrap_or_else(|| fallback.to_owned())
}

pub(super) fn read(db: &DB, adapter: &str, previous: Option<&Projection>) -> Result<Projection> {
    let mut next = previous.cloned().unwrap_or_default();
    let from = previous.and_then(|old| old.last_key.as_deref());
    let mode = from.map_or(IteratorMode::Start, |key| {
        IteratorMode::From(key, Direction::Forward)
    });
    let cf = db.cf_handle(EVENTS).context("events handle")?;
    let mut last_key = None;
    let mut state_key = None;
    let mut guard_error = None;
    for item in db.iterator_cf(cf, mode) {
        let (key, bytes) = item.context("scan domestic evidence")?;
        if from.is_some_and(|old| key.as_ref() <= old) {
            continue;
        }
        let event: Envelope<'_> =
            serde_json::from_slice(&bytes).context("decode domestic evidence envelope")?;
        if let Some(old) = previous {
            ensure!(
                event.sequence > old.last_sequence,
                "late or unordered native evidence requires a full read"
            );
        }
        next.last_sequence = event.sequence;
        next.last_event_us = Some(event.received_us);
        next.errors += usize::from(event.projection_error.is_some());
        let value = &event.value;
        let kind = value.kind;
        if kind == "reconciled" {
            next.reconciled = Some((event.sequence, event.received_us));
        }
        if kind == "runtime_start" {
            next.latest_start = Some(event.sequence);
        }
        if let Some(ready) = boolean(if kind == "transport_status" {
            value.ready
        } else {
            value.transport_ready
        }) {
            next.transport = Some((event.sequence, ready, event.received_us));
        }
        let native = if matches!(
            (adapter, kind),
            ("ctp", "ctp_event") | ("yida", "yida_event")
        ) {
            match value.event.context("missing native event").and_then(|raw| {
                serde_json::from_str::<Connection>(raw.get()).context("decode native connection")
            }) {
                Ok(native) => Some(native),
                Err(error) => {
                    guard_error = Some(error);
                    None
                }
            }
        } else {
            None
        };
        let guard = match kind {
            "trading_blocked" => Some(("blocked", message(value.error, "execution blocked"))),
            "reconciling" | "recovery_checkpoint" => Some(("recovering", String::new())),
            "restart_required" => Some((
                "blocked",
                message(value.reason, "execution restart required"),
            )),
            "ctp_event" | "yida_event" => native
                .as_ref()
                .filter(|raw| kind == "ctp_event" || raw.replay == Some(0))
                .and_then(|raw| match raw.kind {
                    Some(1) => Some(("recovering", String::new())),
                    Some(2) => {
                        let reason = if kind == "ctp_event" {
                            "CTP connection disconnected"
                        } else {
                            "native connection disconnected"
                        };
                        Some(("disconnected", reason.into()))
                    }
                    Some(14 | 16) if kind == "yida_event" => {
                        Some(("blocked", "native adapter invalidated or failed".into()))
                    }
                    _ => None,
                }),
            _ => None,
        };
        if let Some((phase, reason)) = guard {
            guard_error = None;
            next.guard = Some((event.sequence, phase, reason));
        }
        if matches!(kind, "runtime_start" | "runtime_stopped") {
            next.runtime = Some((kind == "runtime_stopped", event.sequence));
        }
        if matches!(kind, "state" | "reconciled") {
            state_key = Some(key.clone());
        }
        last_key = Some(key);
        next.funds
            .observe(adapter, &event, native.and_then(|raw| raw.kind));
    }
    if let Some(error) = guard_error {
        return Err(error);
    }
    if let Some(key) = last_key {
        next.last_key = Some(key.into_vec());
    }
    if let Some(key) = state_key {
        // Decode the latest inventory once. Older state rows can include
        // large snapshots and are superseded by the last forward record.
        let bytes = db
            .get_cf(cf, key)?
            .context("missing latest execution state")?;
        let event: Envelope<'_> = serde_json::from_slice(&bytes)?;
        let value = event.value;
        let state = json(
            value
                .state
                .context("missing execution state")?
                .get()
                .as_bytes(),
            EVENTS,
        )?;
        ensure!(state.is_object(), "missing execution state");
        next.state = Some(State {
            sequence: event.sequence,
            received: event.received_us,
            ready: state["ready"].as_bool().context("missing ready state")?,
            can_trade: boolean(value.can_trade),
            reason: state["reason"].as_str().unwrap_or_default().into(),
            day: state["trading_day"].as_str().map(str::to_owned),
            budget: state["available_local_budget"].as_f64(),
            positions: state["positions"]
                .as_array()
                .context("missing positions snapshot")?
                .clone(),
        });
    }
    Ok(next)
}

impl Projection {
    pub fn state_sequence(&self) -> i64 {
        self.state.as_ref().map_or(0, |s| s.sequence)
    }
    pub fn apply(&self, result: &mut Snapshot, stale_secs: u64) {
        result.last_event_us = self.last_event_us;
        result.projection_errors = self.errors;
        let capture = self
            .state
            .as_ref()
            .filter(|s| s.ready)
            .map(|s| (s.sequence, s.received))
            .or(self.reconciled);
        result.nav_anchor_observed_us = capture.map(|(_, at)| at);
        result.nav_anchor_evidence_sequence = capture.map(|(seq, _)| seq);
        if let Some(state) = &self.state {
            result.snapshot_ready = state.ready;
            result.reported_can_trade = state.can_trade;
            result.reason = state.reason.clone();
            result.trading_day = state.day.clone();
            result.available_local_budget = state.budget;
            result.positions = state.positions.clone();
            result.state_observed_us = Some(state.received);
            result.state_evidence_sequence = Some(state.sequence);
        }
        result.snapshot_current_runtime = self.state.is_some()
            && self
                .latest_start
                .is_none_or(|seq| self.state_sequence() >= seq);
        if !result.snapshot_current_runtime {
            result.reported_can_trade = None;
        }
        let transport = self
            .transport
            .filter(|(seq, _, _)| self.latest_start.is_none_or(|start| *seq >= start));
        result.transport_ready = transport.map(|(_, ready, _)| ready);
        result.transport_observed_us = transport.map(|(_, _, ts)| ts);
        result.status = if self.runtime.is_some_and(|(stopped, _)| stopped) {
            "stopped"
        } else if self.last_event_us.is_none() {
            "waiting"
        } else if let Some((_, phase, reason)) = self
            .guard
            .as_ref()
            .filter(|(seq, _, _)| *seq > self.state_sequence())
        {
            result.reason = reason.clone();
            phase
        } else if !result.snapshot_current_runtime
            || self
                .runtime
                .is_some_and(|(_, seq)| seq > self.state_sequence())
        {
            "recovering"
        } else if self.errors > 0 || !result.snapshot_ready {
            "blocked"
        } else if result.transport_ready == Some(false) {
            "transport_unavailable"
        } else if self
            .last_event_us
            .is_none_or(|ts| result.read_at_us - ts > stale_secs as i64 * 1_000_000)
        {
            "stale"
        } else {
            "observing"
        }
        .into();
        self.funds.apply(result, stale_secs);
    }
}
