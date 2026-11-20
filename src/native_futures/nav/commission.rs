//! Investor-specific commission observations already recorded by CTP Exec.
use super::{Fill, Offset, reference::Fee};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub(super) struct Rate {
    sequence: i64,
    received_us: i64,
    values: [f64; 6],
}

impl Rate {
    pub(super) fn estimate(self, fill: &Fill, multiplier: f64) -> Option<f64> {
        let i = match fill.offset {
            Offset::Open => 0,
            Offset::CloseYesterday => 2,
            Offset::CloseToday => 4,
            Offset::Close if self.values[2..4] == self.values[4..6] => 2,
            Offset::Close => return None,
        };
        let fee = Fee {
            notional_rate: self.values[i],
            per_lot: self.values[i + 1],
        }
        .amount(fill.lots, fill.price, multiplier);
        fee.is_finite().then_some(fee)
    }
}

#[derive(Clone, Default)]
pub(in crate::native_futures) struct History {
    login_day: Option<String>,
    // The commission callback may omit ExchangeID. Resolve it using product
    // status callbacks from the same login, and reject ambiguous products.
    venues: Arc<BTreeMap<String, Option<String>>>,
    rates: Arc<BTreeMap<String, Vec<Rate>>>,
}

impl History {
    pub(in crate::native_futures) fn reset_login(&mut self) {
        self.login_day = None;
        self.venues = Arc::default();
    }

    pub(in crate::native_futures) fn observe(&mut self, kind: i64, sequence: i64, row: &Value) {
        match kind {
            1 | 2 => self.reset_login(),
            4 => {
                self.reset_login();
                self.login_day = row["trading_day"]
                    .as_str()
                    .filter(|day| {
                        row["error_code"] == 0
                            && row["present"] == 1
                            && super::reference::valid_day(day).is_ok()
                    })
                    .map(str::to_owned);
            }
            15 => {
                let Some((product, venue)) = row["symbol"].as_str().zip(row["exchange"].as_str())
                else {
                    return;
                };
                if self.login_day.is_some()
                    && row["error_code"] == 0
                    && row["present"] == 1
                    && (1..=3).contains(&product.len())
                    && product.bytes().all(|c| c.is_ascii_alphabetic())
                    && super::super::VENUES.contains(&venue)
                    && self
                        .venues
                        .get(product)
                        .is_none_or(|known| known.as_ref().is_some_and(|s| s != venue))
                {
                    Arc::make_mut(&mut self.venues)
                        .entry(product.to_owned())
                        .and_modify(|known| *known = None)
                        .or_insert_with(|| Some(venue.to_owned()));
                }
            }
            17 => self.commission(sequence, row),
            _ => {}
        }
    }

    fn commission(&mut self, sequence: i64, row: &Value) {
        let Some(day) = self.login_day.as_deref() else {
            return;
        };
        if row["error_code"] != 0
            || row["present"] != 1
            || row["last"] != 1
            || !row["request_id"].as_i64().is_some_and(|id| id > 0)
            || !row["trading_day"]
                .as_str()
                .is_some_and(|raw| raw.is_empty() || raw == day)
        {
            return;
        }
        let Some(symbol) = row["symbol"].as_str() else {
            return;
        };
        let product = symbol.trim_end_matches(|c: char| c.is_ascii_digit());
        if product.len() == symbol.len() || !product.bytes().all(|c| c.is_ascii_alphabetic()) {
            return;
        }
        let venue = match row["exchange"].as_str() {
            Some("") => self.venues.get(product).and_then(Option::as_deref),
            explicit => explicit,
        };
        let Some(venue) = venue else {
            return;
        };
        if super::super::validate_market(&format!("{venue}:{symbol}")).is_err() {
            return;
        }
        let Some(received_us) = row["received_us"].as_i64().filter(|at| *at > 0) else {
            return;
        };
        let Ok(values) =
            serde_json::from_value::<[f64; 6]>(row["metadata"]["commission_rates"].clone())
        else {
            return;
        };
        if !values.iter().all(|x| x.is_finite() && *x >= 0.0) {
            return;
        }
        let key = format!("{day}:{venue}:{symbol}");
        if self
            .rates
            .get(&key)
            .and_then(|rates| rates.last())
            .is_some_and(|last| last.received_us > received_us || last.values == values)
        {
            return;
        }
        Arc::make_mut(&mut self.rates)
            .entry(key)
            .or_default()
            .push(Rate {
                sequence,
                received_us,
                values,
            });
    }

    pub(in crate::native_futures) fn enrich(&self, fill: &mut Fill) {
        if fill.commission_cny.is_some() || !fill.live_origin {
            return;
        }
        let key = format!("{}:{}:{}", fill.trading_day, fill.venue, fill.symbol);
        fill.journal_rate = self.rates.get(&key).and_then(|rates| {
            let at = rates.partition_point(|r| {
                r.sequence < fill.evidence_sequence && r.received_us <= fill.received_us
            });
            at.checked_sub(1).map(|i| rates[i])
        });
    }
}
