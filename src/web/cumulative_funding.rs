//! Background funding totals for account cards. Never replay history in an HTTP handler.
use super::*;

#[derive(Clone, Debug, Default, Serialize)]
pub struct CumulativeFunding {
    pub kind: String,
    pub income_quote: Option<f64>,
    pub complete: bool,
    pub loading: bool,
    pub refreshing: bool,
    pub unavailable_reason: Option<String>,
    pub start_ts_us: i64,
    pub end_ts_us: i64,
    pub generation_us: i64,
}

impl CumulativeFunding {
    pub(super) fn for_dashboard(cached: Option<&Self>, generation: i64) -> Self {
        match cached {
            Some(value) => Self {
                refreshing: value.refreshing || value.generation_us != generation,
                ..value.clone()
            },
            None => Self {
                loading: true,
                refreshing: true,
                unavailable_reason: Some("累计资金费计算中".into()),
                ..Default::default()
            },
        }
    }

    fn retain_known(previous: Option<&Self>, next: Self) -> Self {
        if next.income_quote.is_none()
            && let Some(previous) = previous.filter(|previous| {
                previous.income_quote.is_some()
                    && (next.start_ts_us == 0 || next.start_ts_us == previous.start_ts_us)
                    && (next.kind.is_empty() || next.kind == previous.kind)
            })
        {
            return Self {
                refreshing: true,
                ..previous.clone()
            };
        }
        next
    }
}

pub(super) fn spawn(state: WebState) {
    if !state.config.funding_rates.daily_estimates_enabled {
        return;
    }
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_generation = None;
        let mut next_retry = std::time::Instant::now();
        loop {
            timer.tick().await;
            let (timelines, generation, sources) = {
                let cache = state.cache.read().await;
                (
                    cache.nav_timelines.clone(),
                    cache.dashboard.generated_at_us,
                    cache
                        .dashboard
                        .report
                        .sources
                        .iter()
                        .map(|s| s.source_id.clone())
                        .collect::<Vec<_>>(),
                )
            };
            if last_generation == Some(generation) && std::time::Instant::now() < next_retry {
                continue;
            }
            last_generation = Some(generation);
            for id in sources {
                let Some(source) = state
                    .config
                    .sources
                    .iter()
                    .find(|s| s.id == id && s.enabled && s.venue == "binance-futures")
                else {
                    continue;
                };
                let timelines = timelines.clone();
                let rates = state.funding_rates.clone();
                let funding = state.funding.clone();
                let daily = state.funding_daily.clone();
                let source_id = source.id.clone();
                let result = tokio::task::spawn_blocking(move || -> Result<CumulativeFunding> {
                    let report = timelines
                        .cumulative_funding_scope(&source_id, generation)
                        .context("账户累计仓位历史尚未就绪")?;
                    let estimate = daily_estimate(&timelines, &rates, &daily, &report)?;
                    let actual = funding.summary(&source_id, report.start_ts_us, generation, &[]);
                    Ok(select(&actual, &estimate, report.start_ts_us, generation))
                })
                .await;
                let value = match result {
                    Ok(Ok(value)) => value,
                    error => {
                        warn!(source_id = %source.id, ?error, "cumulative funding deferred");
                        CumulativeFunding {
                            unavailable_reason: Some("累计资金费暂不可用".into()),
                            generation_us: generation,
                            ..Default::default()
                        }
                    }
                };
                let mut cached = state.cumulative_funding.write().await;
                let value = CumulativeFunding::retain_known(cached.get(&source.id), value);
                cached.insert(source.id.clone(), value);
            }
            next_retry = std::time::Instant::now() + Duration::from_secs(30);
        }
    });
}

fn daily_estimate(
    timelines: &nav::NavTimelineCache,
    rates: &crate::funding_rates::FundingRateCache,
    store: &crate::funding_daily::FundingDailyStore,
    report: &nav::NavTimelineReport,
) -> Result<crate::funding_rates::FundingEstimate> {
    use crate::funding_daily::{DAY_US, DailyEstimate, day_start};
    let today = day_start(report.end_ts_us);
    let mut day = day_start(report.start_ts_us);
    let mut cutoffs = Vec::new();
    while day < today {
        cutoffs.push(day + DAY_US - 1);
        day += DAY_US;
    }
    let id = &report.selected_source_ids[0];
    let fingerprints = timelines.funding_daily_fingerprints(id, &cutoffs)?;
    let symbols = timelines.funding_symbols(report, true);
    // On restart only backfill missing fee days; completed snapshots need no old public rates.
    let first_missing = cutoffs
        .iter()
        .find_map(|end| {
            let day = day_start(*end);
            match store.load(id, day) {
                Ok(Some(saved))
                    if saved.complete
                        && saved.scope_start_us == report.start_ts_us
                        && fingerprints.get(end) == Some(&saved.input_fingerprint) =>
                {
                    None
                }
                _ => Some(report.start_ts_us.max(day)),
            }
        })
        .unwrap_or(today.max(report.start_ts_us));
    rates.request(&symbols, first_missing, report.end_ts_us);
    let mut result = crate::funding_rates::FundingEstimate::default();
    let mut amount = 0.0;
    let mut known = false;
    let mut complete = true;
    let mut loading = false;
    day = day_start(report.start_ts_us);
    while day <= today {
        let mut scoped = report.clone();
        scoped.start_ts_us = report.start_ts_us.max(day);
        scoped.end_ts_us = report.end_ts_us.min(day + DAY_US - 1);
        let fingerprint = fingerprints.get(&scoped.end_ts_us);
        let saved = if day < today {
            store.load(id, day)?
        } else {
            None
        };
        let saved = saved.filter(|s| {
            s.scope_start_us == report.start_ts_us && Some(&s.input_fingerprint) == fingerprint
        });
        if let Some(saved) = saved.as_ref().filter(|s| s.complete) {
            amount += saved
                .income_quote
                .context("complete funding day has no amount")?;
            known = true;
        } else if day < today {
            let mut value = saved.unwrap_or_else(|| DailyEstimate {
                source_id: id.clone(),
                scope_start_us: report.start_ts_us,
                day_us: day,
                from_us: scoped.start_ts_us,
                through_us: scoped.end_ts_us,
                input_fingerprint: fingerprint.cloned().unwrap_or_default(),
                complete: false,
                income_quote: None,
                strategies: BTreeMap::new(),
                symbols: BTreeMap::new(),
            });
            let missing = symbols
                .iter()
                .filter(|symbol| !value.symbols.contains_key(*symbol))
                .cloned()
                .collect::<Vec<_>>();
            let coverage = rates.request(&missing, scoped.start_ts_us, scoped.end_ts_us);
            let measured = timelines.estimate_funding_records(&scoped, true, &coverage.rates)?;
            loading |= coverage.loading;
            // Freeze complete symbol-days individually so one unavailable market
            // cannot erase the other known daily payments and receipts.
            for symbol in missing {
                if !coverage.complete_symbols.contains(&symbol)
                    || measured.skipped_by_symbol.contains_key(&symbol)
                {
                    continue;
                }
                let mut income = 0.0;
                for record in measured.records.iter().filter(|r| r.symbol == symbol) {
                    income += record.income_quote;
                    for (strategy, v) in &record.strategies {
                        *value.strategies.entry(strategy.clone()).or_default() += v;
                    }
                }
                value.symbols.insert(symbol, income);
            }
            value.complete = symbols.iter().all(|s| value.symbols.contains_key(s));
            value.income_quote = (!value.symbols.is_empty() || symbols.is_empty())
                .then(|| value.symbols.values().sum());
            if value.income_quote.is_some() {
                store.save(&value)?;
            }
            complete &= value.complete;
            if let Some(v) = value.income_quote {
                amount += v;
                known = true;
            }
        } else {
            let coverage = rates.request(&symbols, scoped.start_ts_us, scoped.end_ts_us);
            let measured = timelines.estimate_funding_records(&scoped, true, &coverage.rates)?;
            let estimate = crate::funding_rates::estimate_timeline(&scoped, &coverage, &measured);
            complete &= estimate.income_quote.is_some() && estimate.unavailable_reason.is_none();
            loading |= estimate.loading;
            if let Some(v) = estimate.income_quote {
                amount += v;
                known = true;
            }
        }
        day += DAY_US;
    }
    result.income_quote = known.then_some(amount);
    result.loading = loading;
    result.unavailable_reason =
        (!complete).then(|| "累计 Funding 仅计入已覆盖日期及币种，缺失部分未知".into());
    Ok(result)
}

fn select(
    actual: &crate::funding::FundingSummary,
    estimate: &crate::funding_rates::FundingEstimate,
    start: i64,
    end: i64,
) -> CumulativeFunding {
    let actual_available = actual.record_count > 0
        || actual
            .symbols
            .iter()
            .any(|s| s.start_ts_us <= end && s.synced_through_ts_us.is_some_and(|ts| ts >= start));
    let estimate_complete = estimate.income_quote.is_some()
        && estimate.unavailable_reason.is_none()
        && !estimate.loading;
    let use_actual = actual_available && (actual.enabled || !estimate_complete);
    let complete_actual = actual.status == "ok" && actual.start_ts_us.is_some_and(|ts| ts <= start);
    CumulativeFunding {
        kind: if use_actual { "actual" } else { "estimated" }.into(),
        income_quote: if use_actual {
            Some(actual.income_quote)
        } else {
            estimate.income_quote
        },
        complete: if use_actual {
            complete_actual
        } else {
            estimate_complete
        },
        loading: !use_actual && estimate.loading,
        refreshing: false,
        unavailable_reason: if use_actual {
            (!complete_actual).then(|| {
                actual
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "实际资金费覆盖尚不完整".into())
            })
        } else {
            estimate.unavailable_reason.clone()
        },
        start_ts_us: start,
        end_ts_us: end,
        generation_us: end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_keeps_known_funding_until_a_replacement_is_ready() {
        let previous = CumulativeFunding {
            kind: "estimated".into(),
            income_quote: Some(-12.5),
            complete: true,
            start_ts_us: 10,
            end_ts_us: 100,
            generation_us: 100,
            ..Default::default()
        };
        let stale = CumulativeFunding::for_dashboard(Some(&previous), 200);
        assert_eq!(stale.income_quote, Some(-12.5));
        assert!(stale.refreshing && stale.complete);
        assert_eq!(stale.end_ts_us, 100);
        let failed = CumulativeFunding::retain_known(
            Some(&previous),
            CumulativeFunding {
                generation_us: 200,
                ..Default::default()
            },
        );
        assert_eq!(failed.income_quote, Some(-12.5));
        assert!(CumulativeFunding::for_dashboard(Some(&failed), 100).refreshing);
        let pending = CumulativeFunding {
            kind: "estimated".into(),
            start_ts_us: 10,
            generation_us: 200,
            loading: true,
            ..Default::default()
        };
        assert_eq!(
            CumulativeFunding::retain_known(Some(&previous), pending.clone()).income_quote,
            Some(-12.5)
        );
        let changed_anchor = CumulativeFunding {
            start_ts_us: 20,
            ..pending.clone()
        };
        assert!(
            CumulativeFunding::retain_known(Some(&previous), changed_anchor)
                .income_quote
                .is_none()
        );
        assert!(
            CumulativeFunding::retain_known(None, pending)
                .income_quote
                .is_none()
        );
        let replacement = CumulativeFunding {
            income_quote: Some(-13.0),
            generation_us: 200,
            end_ts_us: 200,
            ..previous
        };
        let ready = CumulativeFunding::retain_known(Some(&stale), replacement);
        let ready = CumulativeFunding::for_dashboard(Some(&ready), 200);
        assert_eq!(ready.income_quote, Some(-13.0));
        assert!(!ready.refreshing);
    }

    #[test]
    fn actual_and_estimates_are_never_added_and_unknown_is_not_zero() {
        let mut actual = crate::funding::FundingSummary {
            enabled: true,
            record_count: 1,
            income_quote: -3.0,
            status: "ok".into(),
            start_ts_us: Some(1),
            ..Default::default()
        };
        let mut estimate = crate::funding_rates::FundingEstimate {
            income_quote: Some(7.0),
            ..Default::default()
        };
        assert_eq!(select(&actual, &estimate, 10, 100).income_quote, Some(-3.0));
        actual.enabled = false;
        assert_eq!(select(&actual, &estimate, 10, 100).income_quote, Some(7.0));
        estimate.unavailable_reason = Some("partial".into());
        assert_eq!(select(&actual, &estimate, 10, 100).income_quote, Some(-3.0));
        actual.record_count = 0;
        estimate.income_quote = None;
        assert_eq!(select(&actual, &estimate, 10, 100).income_quote, None);
    }
}
