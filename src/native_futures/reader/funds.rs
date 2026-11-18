//! Broker query and notification evidence. The executor's reservation budget
//! and position marks are independent of these amounts.
use super::*;

const CTP_ACCOUNT_FIELDS: [&str; 44] = [
    "pre_mortgage",
    "pre_credit",
    "pre_deposit",
    "pre_balance",
    "pre_margin",
    "interest_base",
    "interest",
    "deposit",
    "withdraw",
    "frozen_margin",
    "frozen_cash",
    "frozen_commission",
    "curr_margin",
    "cash_in",
    "commission",
    "close_profit",
    "position_profit",
    "balance",
    "available",
    "withdraw_quota",
    "reserve",
    "credit",
    "mortgage",
    "exchange_margin",
    "delivery_margin",
    "exchange_delivery_margin",
    "reserve_balance",
    "pre_fund_mortgage_in",
    "pre_fund_mortgage_out",
    "fund_mortgage_in",
    "fund_mortgage_out",
    "fund_mortgage_available",
    "mortgageable_fund",
    "spec_product_margin",
    "spec_product_frozen_margin",
    "spec_product_commission",
    "spec_product_frozen_commission",
    "spec_product_position_profit",
    "spec_product_close_profit",
    "spec_product_position_profit_by_alg",
    "spec_product_exchange_margin",
    "frozen_swap",
    "remain_swap",
    "option_value",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountFundsSource {
    CtpTradingAccount,
    YidaExtendedAccount,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AccountFunds {
    pub source: AccountFundsSource,
    pub evidence_sequence: i64,
    /// Time the adapter received the callback, in Unix microseconds.
    pub observed_us: i64,
    pub trading_day: Option<String>,
    pub currency: Option<String>,
    pub valuation_basis: Option<String>,
    pub balance: Option<f64>,
    pub previous_balance: Option<f64>,
    pub margin: Option<f64>,
    pub available: Option<f64>,
    pub usable_cash: Option<f64>,
    pub deposit: Option<f64>,
    pub withdraw: Option<f64>,
    pub native_fields: BTreeMap<&'static str, f64>,
}

impl AccountFunds {
    fn ctp(raw: &Value, sequence: i64, login_day: Option<&str>) -> Option<Self> {
        if raw["exchange"] != "CNY" {
            return None;
        }
        let trading_day = match raw.get("trading_day") {
            None | Some(Value::Null) => None,
            Some(value) => optional_day(value.as_str()?)?,
        };
        if let Some(day) = trading_day.as_deref() {
            if login_day.is_some_and(|login| login != day) {
                return None;
            }
        }
        let mut fields = BTreeMap::new();
        if let Some(values) = raw["metadata"]["account_values"].as_array() {
            for (name, value) in CTP_ACCOUNT_FIELDS.into_iter().zip(values) {
                if let Some(value) = amount(value) {
                    fields.insert(name, value);
                }
            }
        }
        // Earlier account callbacks retained Available without the full array.
        // Both audit fields, when present, originate from the same CTP member.
        match (fields.get("available").copied(), amount(&raw["available"])) {
            (Some(field), Some(raw)) if field != raw => return None,
            (None, Some(raw)) => {
                fields.insert("available", raw);
            }
            _ => {}
        }
        if fields.is_empty() {
            return None;
        }
        Some(Self {
            source: AccountFundsSource::CtpTradingAccount,
            evidence_sequence: sequence,
            observed_us: receipt(&raw["received_us"])?,
            trading_day,
            currency: Some("CNY".into()),
            valuation_basis: None,
            balance: fields.get("balance").copied(),
            previous_balance: fields.get("pre_balance").copied(),
            margin: fields.get("curr_margin").copied(),
            available: fields.get("available").copied(),
            usable_cash: None,
            deposit: fields.get("deposit").copied(),
            withdraw: fields.get("withdraw").copied(),
            native_fields: fields,
        })
    }

    fn yida(raw: &Value, observed_us: i64, sequence: i64, day: String) -> Option<Self> {
        if raw["error"] != 0
            || raw["login_count"] != 1
            || !raw["right"]
                .as_i64()
                .is_some_and(|right| (0..=2).contains(&right))
        {
            return None;
        }
        // The bridge stores YDExtendedAccount::usable() in the audit
        // member called available. Its native Available member is distinct.
        let usable_cash = amount(&raw["available"])?;
        let mut native_fields = BTreeMap::from([("usable_cash", usable_cash)]);
        const YIDA_FIELDS: [&str; 24] = [
            "balance",
            "pre_balance",
            "pre_cash_balance",
            "pre_market_value",
            "pre_position_market_value",
            "deposit",
            "withdraw",
            "frozen_withdraw",
            "max_money_usage",
            "available",
            "close_profit",
            "other_close_profit",
            "cash_in",
            "commission",
            "message_commission",
            "margin",
            "margin2",
            "position_profit",
            "position_market_value",
            "exec_margin",
            "exec_allocated_amount",
            "exec_allocated_frozen_amount",
            "sdk_cash_balance",
            "sdk_market_value",
        ];
        let account = &raw["account"];
        let basis = account["valuation_basis"]
            .as_str()
            .filter(|&basis| basis == "sdk_mark_time_unverified" && account["currency"] == "CNY");
        if basis.is_some() {
            native_fields.extend(
                YIDA_FIELDS
                    .into_iter()
                    .filter_map(|name| amount(&account[name]).map(|value| (name, value))),
            );
        }
        Some(Self {
            source: AccountFundsSource::YidaExtendedAccount,
            evidence_sequence: sequence,
            observed_us,
            trading_day: Some(day),
            currency: basis.map(|_| "CNY".into()),
            valuation_basis: basis.map(str::to_owned),
            balance: None,
            previous_balance: native_fields.get("pre_market_value").copied(),
            margin: None,
            available: native_fields.get("available").copied(),
            usable_cash: Some(usable_cash),
            deposit: native_fields.get("deposit").copied(),
            withdraw: native_fields.get("withdraw").copied(),
            native_fields,
        })
    }
}

fn amount(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && v.abs() < f64::MAX)
}

fn receipt(value: &Value) -> Option<i64> {
    value.as_i64().filter(|&at| at > 0)
}

fn optional_day(day: &str) -> Option<Option<String>> {
    if day.is_empty() {
        return Some(None);
    }
    super::super::reference::valid_day(day).ok()?;
    Some(Some(day.to_owned()))
}

fn yida_day(raw: &Value) -> Option<String> {
    optional_day(&raw["day"].as_i64()?.to_string())?
}

#[derive(Clone)]
struct CtpQuery {
    request_id: i64,
    row: Option<AccountFunds>,
    invalid: bool,
}

#[derive(Clone)]
struct YidaRecovery {
    day: String,
    row: Option<AccountFunds>,
    ended: bool,
}

#[derive(Clone, Default)]
pub(super) struct FundsProjection {
    latest: Option<AccountFunds>,
    boundary_seq: i64,
    failed_seq: i64,
    stopped: bool,
    ctp_connected: Option<bool>,
    ctp_login_day: Option<String>,
    ctp_query: Option<CtpQuery>,
    ctp_last_completed_request: i64,
    yida_ready_day: Option<String>,
    yida_recovery: Option<YidaRecovery>,
}

impl FundsProjection {
    fn invalidate(&mut self, sequence: i64) {
        self.boundary_seq = sequence;
        self.ctp_query = None;
        self.ctp_last_completed_request = 0;
        self.ctp_login_day = None;
        self.yida_recovery = None;
        self.yida_ready_day = None;
    }

    pub(super) fn observe(
        &mut self,
        adapter: &str,
        row: &projection::Envelope<'_>,
        native_kind: Option<i64>,
    ) {
        let event = &row.value;
        let shutdown_tail = projection::boolean(event.shutdown_tail) == Some(true);
        match event.kind {
            "runtime_start" | "runtime_stopped" => {
                self.invalidate(row.sequence);
                self.stopped = event.kind == "runtime_stopped";
                self.ctp_connected = None;
            }
            "ctp_event" if adapter == "ctp" && !self.stopped && !shutdown_tail => {
                if matches!(native_kind, Some(1 | 2 | 4 | 7))
                    && let Some(raw) = event
                        .event
                        .and_then(|raw| serde_json::from_str(raw.get()).ok())
                {
                    self.ctp(&raw, row.sequence, row.projection_error.is_some());
                }
            }
            "yida_event" if adapter == "yida" && !self.stopped && !shutdown_tail => {
                if matches!(native_kind, Some(1 | 2 | 4 | 6 | 8 | 12 | 14 | 16))
                    && let Some(raw) = event
                        .event
                        .and_then(|raw| serde_json::from_str(raw.get()).ok())
                {
                    let observed_us = event
                        .received_us
                        .and_then(|ts| serde_json::from_str(ts.get()).ok());
                    self.yida(
                        &raw,
                        observed_us,
                        row.sequence,
                        row.projection_error.is_some(),
                    );
                }
            }
            "reconciled" if adapter == "yida" && !self.stopped => {
                let Some(recovery) = self.yida_recovery.take().filter(|r| r.ended) else {
                    return;
                };
                #[derive(Deserialize)]
                struct Reconciled<'a> {
                    connected: bool,
                    trading_day: &'a str,
                }
                if row.projection_error.is_none()
                    && let Some(state) = event
                        .state
                        .and_then(|raw| serde_json::from_str::<Reconciled>(raw.get()).ok())
                    && state.connected
                    && state.trading_day == recovery.day
                    && let Some(funds) = recovery.row
                {
                    self.yida_ready_day = Some(recovery.day);
                    self.latest = Some(funds);
                }
            }
            _ => {}
        }
    }

    fn ctp(&mut self, raw: &Value, sequence: i64, invalid: bool) {
        match raw["kind"].as_i64() {
            Some(1 | 2 | 4) => {
                self.invalidate(sequence);
                self.ctp_connected = Some(raw["kind"] == 4 && raw["error_code"] == 0);
                if self.ctp_connected == Some(true) {
                    self.ctp_login_day =
                        raw["trading_day"].as_str().and_then(optional_day).flatten();
                }
            }
            Some(7) if self.ctp_connected != Some(false) => {
                let Some(request_id) = raw["request_id"].as_i64().filter(|&id| id > 0) else {
                    return;
                };
                if request_id <= self.ctp_last_completed_request
                    || self
                        .ctp_query
                        .as_ref()
                        .is_some_and(|q| q.request_id > request_id)
                {
                    return;
                }
                let mut query = self
                    .ctp_query
                    .take()
                    .filter(|q| q.request_id == request_id)
                    .unwrap_or(CtpQuery {
                        request_id,
                        row: None,
                        invalid: false,
                    });
                query.invalid |= invalid || raw["error_code"] != 0;
                if raw["present"] == 1 {
                    if query.row.is_some() {
                        query.invalid = true;
                    } else {
                        query.row = AccountFunds::ctp(raw, sequence, self.ctp_login_day.as_deref());
                        query.invalid |= query.row.is_none();
                    }
                }
                if raw["last"] == 1 {
                    self.ctp_last_completed_request = request_id;
                    if !query.invalid
                        && let Some(row) = query.row
                    {
                        self.latest = Some(row);
                    } else {
                        self.failed_seq = sequence;
                    }
                } else {
                    self.ctp_query = Some(query);
                }
            }
            _ => {}
        }
    }

    fn yida(&mut self, raw: &Value, received_us: Option<i64>, sequence: i64, invalid: bool) {
        match raw["kind"].as_i64() {
            Some(1 | 2 | 4 | 14 | 16) if raw["replay"] == 0 => self.invalidate(sequence),
            Some(6) if !invalid => {
                self.invalidate(sequence);
                self.yida_recovery = yida_day(raw).map(|day| YidaRecovery {
                    day,
                    row: None,
                    ended: false,
                });
            }
            Some(8) => {
                let Some(day) = yida_day(raw) else {
                    return;
                };
                let funds = if invalid {
                    None
                } else {
                    received_us
                        .filter(|&at| at > 0)
                        .and_then(|at| AccountFunds::yida(raw, at, sequence, day.clone()))
                };
                match raw["replay"].as_i64() {
                    Some(1) => {
                        self.yida_recovery = self.yida_recovery.take().and_then(|mut recovery| {
                            if recovery.ended || recovery.day != day || recovery.row.is_some() {
                                return None;
                            }
                            recovery.row = Some(funds?);
                            Some(recovery)
                        });
                    }
                    Some(0) if self.yida_ready_day.as_ref() == Some(&day) => {
                        if let Some(funds) = funds {
                            self.latest = Some(funds);
                        } else {
                            self.failed_seq = sequence;
                        }
                    }
                    _ => {}
                }
            }
            Some(12) => {
                self.yida_recovery = self
                    .yida_recovery
                    .take()
                    .filter(|recovery| {
                        !invalid && !recovery.ended && yida_day(raw).as_ref() == Some(&recovery.day)
                    })
                    .map(|mut recovery| {
                        recovery.ended = true;
                        recovery
                    });
            }
            _ => {}
        }
    }

    pub(super) fn apply(&self, snapshot: &mut Snapshot, stale_secs: u64) {
        let Some(funds) = &self.latest else {
            return;
        };
        snapshot.funds_current_runtime =
            !self.stopped && funds.evidence_sequence > self.boundary_seq;
        snapshot.funds_stale = !snapshot.funds_current_runtime
            || self.failed_seq > funds.evidence_sequence
            || snapshot.replication_stale
            || snapshot
                .replication
                .as_ref()
                .is_some_and(|replica| replica.last_error.is_some())
            || funds.observed_us > snapshot.read_at_us
            || snapshot.read_at_us.saturating_sub(funds.observed_us)
                > stale_secs as i64 * 1_000_000;
        snapshot.funds = Some(funds.clone());
    }
}
