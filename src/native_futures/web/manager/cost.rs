use super::*;
use crate::{
    acquisition_cost::{AcquisitionRowSort, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE},
    native_futures::{cost::acquisition, reference::Pack},
};
use std::{
    cmp::Reverse,
    collections::BTreeSet,
    time::{Duration, Instant},
};

const DAY_US: i64 = 24 * 60 * 60 * 1_000_000;
const MAX_MARKET_DAYS: usize = 32;
const MARKET_DEADLINE: Duration = Duration::from_secs(12);
const MAX_REFERENCE_FILES: usize = 128;
const MAX_REFERENCE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CostQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    source_ids: Option<String>,
    strategy_name: Option<String>,
    row_symbol: Option<String>,
    row_strategy: Option<String>,
    #[serde(default)]
    row_sort: AcquisitionRowSort,
    page: Option<usize>,
    page_size: Option<usize>,
}

fn strategy_filter(value: Option<String>) -> Result<Option<String>, ApiError> {
    let value = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(name) = value {
        crate::order_config::validate_strategy_name(name).map_err(bad)?;
    }
    Ok(value.map(str::to_owned))
}

fn symbol_filter(value: Option<String>) -> Result<Option<String>, ApiError> {
    let value = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(symbol) = value {
        if symbol.contains(':') {
            crate::native_futures::validate_market(symbol).map_err(bad)?;
        } else if !crate::native_futures::VENUES.iter().any(|venue| {
            crate::native_futures::validate_market(&format!("{venue}:{symbol}")).is_ok()
        }) {
            return Err(bad("复盘合约必须使用原生合约代码"));
        }
    }
    Ok(value.map(str::to_owned))
}

fn ms_to_us(ms: i64, name: &str) -> Result<i64, ApiError> {
    ms.checked_mul(1000)
        .filter(|&us| us >= 0)
        .ok_or_else(|| bad(format!("{name} 必须为有效的 Unix 毫秒时间")))
}

impl CostQuery {
    fn validate(self, source_id: String, now_us: i64) -> Result<acquisition::Query, ApiError> {
        let start_us = self
            .start_ms
            .map(|ms| ms_to_us(ms, "startMs"))
            .transpose()?
            .unwrap_or(now_us - DAY_US);
        let end_us = self
            .end_ms
            .map(|ms| ms_to_us(ms, "endMs"))
            .transpose()?
            .unwrap_or(now_us);
        if start_us > end_us || end_us > now_us || start_us < now_us - 30 * DAY_US {
            return Err(bad(
                "成本复盘仅支持最近 30 天，结束时间不得早于开始或晚于当前时间",
            ));
        }
        let page = self.page.unwrap_or(1);
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page == 0 || !(1..=MAX_PAGE_SIZE).contains(&page_size) {
            return Err(bad(format!(
                "page 必须大于 0，pageSize 必须在 1–{MAX_PAGE_SIZE} 之间"
            )));
        }
        Ok(acquisition::Query {
            source_id,
            now_us,
            start_us,
            end_us,
            strategy_name: strategy_filter(self.strategy_name)?,
            row_strategy: strategy_filter(self.row_strategy)?,
            row_symbol: symbol_filter(self.row_symbol)?,
            row_sort: self.row_sort,
            page,
            page_size,
        })
    }
}

struct PreparedDay {
    trading_day: String,
    pack: Option<Pack>,
    facts: reader::CostFacts,
}

struct Prepared {
    days: Vec<PreparedDay>,
    reference_error: bool,
    replication: Option<crate::native_futures::sync::ReplicaStatus>,
}

fn prepare(s: &Service, source: &Source, query: &acquisition::Query) -> Result<Prepared> {
    let mut window = reader::with_db(&s.rocks, source, |db| {
        reader::read_cost_window(source, db, query.start_us, query.end_us)
    })?;
    let (available_references, mut reference_error) = match reference::days(&s.config, &source.id) {
        Ok(days) => (days, false),
        Err(error) => {
            tracing::warn!(?error, "domestic cost reference listing unavailable");
            (BTreeSet::new(), true)
        }
    };
    let selected_references: Vec<_> = available_references
        .iter()
        .rev()
        .filter(|day| window.days.contains_key(*day))
        .chain(
            available_references
                .iter()
                .rev()
                .filter(|day| !window.days.contains_key(*day)),
        )
        .take(MAX_REFERENCE_FILES)
        .cloned()
        .collect();
    reference_error |= selected_references.len() < available_references.len();
    let mut bytes = MAX_REFERENCE_BYTES;
    let mut prepared = Vec::new();
    for trading_day in selected_references {
        let mut pack =
            match reference::load_bounded(&s.config, &source.id, &trading_day, &mut bytes) {
                Ok(pack) => pack,
                Err(error) => {
                    tracing::warn!(?error, "domestic acquisition reference unavailable");
                    reference_error = true;
                    None
                }
            };
        let facts = window.days.remove(&trading_day).unwrap_or_default();
        if let Some(pack) = &mut pack {
            retain_relevant_signals(pack, &facts, query)?;
        }
        if !facts.fills.is_empty() || pack.as_ref().is_some_and(|p| !p.signals.is_empty()) {
            prepared.push(PreparedDay {
                trading_day,
                pack,
                facts,
            });
        }
    }
    prepared.extend(
        window
            .days
            .into_iter()
            .map(|(trading_day, facts)| PreparedDay {
                trading_day,
                pack: None,
                facts,
            }),
    );
    Ok(Prepared {
        days: prepared,
        reference_error,
        replication: window.replication,
    })
}

fn retain_relevant_signals(
    pack: &mut Pack,
    facts: &reader::CostFacts,
    query: &acquisition::Query,
) -> Result<()> {
    let order_keys: BTreeSet<_> = facts
        .fills
        .iter()
        .filter_map(|row| {
            let fill = &row["fill"];
            Some((
                fill["identity"]["venue"].as_str()?,
                fill["order_key"]["session"].as_str()?,
                fill["order_key"]["order_ref"].as_str()?,
            ))
        })
        .collect();
    let linked_signals: BTreeSet<_> = pack
        .order_links
        .iter()
        .filter(|link| {
            order_keys.contains(&(
                link.venue.as_str(),
                link.session.as_str(),
                link.order_ref.as_str(),
            ))
        })
        .map(|link| link.signal_id.as_str())
        .collect();
    let mut selected = BTreeSet::new();
    for signal in &pack.signals {
        let received_us = reference::timestamp(&signal.received_at)?;
        if (query.start_us..=query.end_us).contains(&received_us)
            || linked_signals.contains(signal.id.as_str())
        {
            selected.insert(signal.id.clone());
        }
    }
    pack.signals.retain(|signal| selected.contains(&signal.id));
    pack.order_links
        .retain(|link| selected.contains(&link.signal_id));
    Ok(())
}

async fn populate_fact_multipliers(
    feed: &Arc<Feed>,
    reports: &mut [crate::native_futures::cost::Report],
    now_us: i64,
) -> bool {
    let mut needed = BTreeMap::<_, i64>::new();
    let mut multipliers = BTreeMap::new();
    for f in reports.iter().flat_map(|r| &r.fills) {
        if f.contract_multiplier.is_none() {
            let market_day = (f.trading_day.clone(), f.venue.clone(), f.symbol.clone());
            let receipt = needed.entry(market_day).or_default();
            *receipt = (*receipt).max(f.received_us);
        }
    }
    if needed.is_empty() {
        return false;
    }
    let mut needed: Vec<_> = needed.into_iter().collect();
    needed.sort_by_key(|(_, receipt)| Reverse(*receipt));
    let mut limited = needed.len() > MAX_MARKET_DAYS;
    needed.truncate(MAX_MARKET_DAYS);
    let deadline = tokio::time::Instant::now() + MARKET_DEADLINE;
    for batch in needed.chunks(4) {
        let mut requests = tokio::task::JoinSet::new();
        for ((day, venue, symbol), receipt) in batch {
            let (feed, day, venue, symbol, receipt) = (
                feed.clone(),
                day.clone(),
                venue.clone(),
                symbol.clone(),
                *receipt,
            );
            requests.spawn(async move {
                let value = feed
                    .day_multiplier(&venue, &symbol, &day, receipt, now_us)
                    .await;
                ((day, venue, symbol), value)
            });
        }
        loop {
            let result = match tokio::time::timeout_at(deadline, requests.join_next()).await {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(_) => {
                    limited = true;
                    break;
                }
            };
            match result {
                Ok((key, Ok(Some(multiplier)))) => {
                    multipliers.insert(key, multiplier);
                }
                Ok((_, Ok(None))) => {}
                Ok((_, Err(error))) => {
                    tracing::warn!(?error, "factual native contract multiplier unavailable");
                }
                Err(error) => {
                    tracing::warn!(?error, "factual native contract multiplier task failed");
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    for f in reports.iter_mut().flat_map(|r| &mut r.fills) {
        if f.contract_multiplier.is_none() {
            f.contract_multiplier = multipliers
                .get(&(f.trading_day.clone(), f.venue.clone(), f.symbol.clone()))
                .copied();
        }
    }
    limited
}

pub(super) async fn report(
    State(s): State<Service>,
    Extension(access): Extension<Arc<SourceAccess>>,
    Query(query): Query<CostQuery>,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    let now_us = crate::native_futures::now_us();
    let source = {
        let ids = query.source_ids.as_deref().unwrap_or_default();
        let specified: BTreeSet<_> = ids
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .collect();
        let mut sources = if specified.is_empty() {
            visible(&s, &access).collect::<Vec<_>>()
        } else {
            specified
                .into_iter()
                .map(|id| get_source(&s, &access, id))
                .collect::<Result<Vec<_>, _>>()?
        };
        match sources.len() {
            0 => return Err(forbidden()),
            1 => sources.remove(0).clone(),
            _ => return Err(bad("请选择一个可查看的账户")),
        }
    };
    let query = query.validate(source.id.clone(), now_us)?;
    let mut warnings = Vec::new();
    if query.strategy_name.is_some() || query.row_strategy.is_some() {
        warnings.push("原生执行成本证据未提供可确认的策略归属，无法按策略筛选或汇总。".to_owned());
    }

    let admission = if query.strategy_name.is_none() {
        let admission = Arc::new(s.cost_admission.clone().try_acquire_owned().map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "成本复盘请求繁忙，请稍后重试".into(),
            )
        })?);
        Some(admission)
    } else {
        None
    };
    let mut reports = Vec::new();
    let mut factual_through_us = None;
    if let Some(admission) = &admission {
        let (owner, input) = (s.clone(), query.clone());
        let scan_permit = admission.clone();
        let Prepared {
            mut days,
            reference_error,
            replication,
        } = tokio::task::spawn_blocking(move || {
            let _permit = scan_permit;
            prepare(&owner, &source, &input)
        })
        .await
        .context("native cost evidence scan task")??;
        factual_through_us = replication
            .as_ref()
            .and_then(|replica| replica.source_captured_at_us);
        if replication.as_ref().is_some_and(|r| {
            r.last_error.is_some() || r.source_captured_at_us.is_none_or(|t| t < query.end_us)
        }) {
            warnings.push("执行副本尚未确认覆盖整个查询区间，事实指标仅统计已同步成交。".into());
        }
        if reference_error {
            warnings.push("部分原生参考文件未读入，可能是文件校验或读取上限所致。".into());
        }
        let market_deadline = tokio::time::Instant::now() + MARKET_DEADLINE;
        let mut market_budget = MAX_MARKET_DAYS;
        let mut market_error = false;
        if let Some(feed) = &s.market_data {
            for pack in days.iter_mut().filter_map(|day| day.pack.as_mut()) {
                match tokio::time::timeout_at(
                    market_deadline,
                    feed.augment_bounded(pack, now_us, &mut market_budget),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(?error, "domestic acquisition minute data unavailable");
                        market_error = true;
                    }
                    Err(_) => {
                        market_error = true;
                        break;
                    }
                }
            }
        }
        if market_error {
            warnings.push("部分原生分钟行情暂不可用；保留已覆盖的参考及事实成交。".into());
        }
        let id = query.source_id.clone();
        let build_permit = admission.clone();
        reports = tokio::task::spawn_blocking(move || {
            let _permit = build_permit;
            days.into_iter()
                .map(|day| {
                    crate::native_futures::cost::build(
                        &id,
                        &day.trading_day,
                        day.pack.as_ref(),
                        day.facts,
                        now_us,
                    )
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .context("native cost aggregation task")??;
        if let Some(feed) = &s.market_data
            && populate_fact_multipliers(feed, &mut reports, now_us).await
        {
            warnings.push("本次查询的事实成交乘数补齐达到处理上限。".into());
        }
    }
    let report = tokio::task::spawn_blocking(move || {
        let _permit = admission;
        acquisition::report(&query, &reports, warnings, factual_through_us)
    })
    .await
    .context("native cost response task")?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "generated_at_us": now_us,
            "generation_duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "reports": [report],
        })),
    ).into_response())
}

pub(super) async fn kline_status(State(s): State<Service>) -> ApiResult {
    body(s.market_data.as_ref().map_or_else(
        crate::native_futures::market_data::CacheStatus::default,
        |feed| feed.cache_status(),
    ))
}
