//! Read-only comparison using immutable account target batches and native minute prices.
use super::*;
use crate::{native_futures::market_data, theoretical_nav::TheoreticalNavPoint};
use serde_json::Value;
use sqlx::PgPool;

#[derive(Clone, Copy)]
pub(crate) enum Model {
    Twap,
    Vwap,
}

impl Model {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Twap => "native_twap",
            Self::Vwap => "native_vwap",
        }
    }
}

pub(crate) const MODELS: [Model; 2] = [Model::Twap, Model::Vwap];

pub(crate) struct Reference {
    pub(crate) model: Model,
    pub(crate) points: Vec<TheoreticalNavPoint>,
    pub(crate) unavailable: Option<String>,
}

pub(crate) fn unavailable(reason: &str) -> Vec<Reference> {
    MODELS
        .iter()
        .map(|&model| Reference {
            model,
            points: vec![],
            unavailable: Some(reason.into()),
        })
        .collect()
}

#[derive(Deserialize)]
struct Batch {
    source_id: String,
    net_targets: BTreeMap<String, Target>,
}

#[derive(Deserialize)]
struct Target {
    target_lots: i64,
}

struct Intent {
    market: Market,
    accepted_us: i64,
    trading_day: Option<u32>,
    target_lots: i64,
}

pub(crate) async fn timeline(
    pool: &PgPool,
    market: Option<&market_data::Feed>,
    report: &Report,
    revision: i64,
    filtered: bool,
) -> Result<Vec<Reference>> {
    let receipts: Vec<(i64, Value, Option<String>)> = sqlx::query_as(
        "SELECT received_at_us,receipt,request->>'trading_day' FROM cta_native_netting_events
         WHERE source_id=$1 AND kind='target_batch' AND revision <= $2
           AND received_at_us BETWEEN $3 AND $4 ORDER BY received_at_us,revision LIMIT 20001",
    )
    .bind(&report.source_id)
    .bind(revision)
    .bind(report.start_us)
    .bind(report.end_us)
    .fetch_all(pool)
    .await?;
    ensure!(receipts.len() <= 20_000, "参考净值目标批次超出计算上限");
    let initial: BTreeMap<_, _> = report
        .markets
        .iter()
        .filter(|row| row.initial_net_lots != 0)
        .map(|row| {
            (
                (row.venue.clone(), row.symbol.clone()),
                row.initial_net_lots,
            )
        })
        .collect();
    let selected: BTreeSet<_> = report.selected_symbols.iter().map(String::as_str).collect();
    let mut targets = initial.clone();
    let mut intents = Vec::new();
    for (accepted_us, receipt, day) in receipts {
        let batch: Batch = serde_json::from_value(receipt)?;
        ensure!(
            batch.source_id == report.source_id,
            "参考目标账户与净值账户不符"
        );
        let trading_day = day
            .as_deref()
            .map(|day| {
                reference::valid_day(day)?;
                Ok::<u32, anyhow::Error>(day.parse()?)
            })
            .transpose()?;
        for (name, target) in batch.net_targets {
            if filtered && !selected.contains(name.as_str()) {
                continue;
            }
            super::super::validate_market(&name)?;
            let (venue, symbol) = name.split_once(':').context("缺少目标合约交易所")?;
            let market = (venue.to_owned(), symbol.to_owned());
            let previous = targets.entry(market.clone()).or_default();
            if *previous != target.target_lots {
                *previous = target.target_lots;
                intents.push(Intent {
                    market,
                    accepted_us,
                    trading_day,
                    target_lots: *previous,
                });
            }
        }
        ensure!(intents.len() <= 250_000, "参考净值目标变更超出计算上限");
    }
    let markets: BTreeSet<_> = initial
        .keys()
        .chain(intents.iter().map(|i| &i.market))
        .cloned()
        .collect();
    ensure!(markets.len() <= 64, "参考净值合约数超出计算上限");
    let mut prices = BTreeMap::new();
    for (venue, symbol) in markets {
        let feed = market.context("缺少具体合约的一分钟行情源")?;
        let window = feed
            .nav_prices(&venue, &symbol, report.start_us, report.end_us, true)
            .await?;
        prices.insert((venue, symbol), window);
    }
    let start = report.start_us;
    let end = report.end_us;
    let samples = report.points.clone();
    tokio::task::spawn_blocking(move || {
        let schedule = schedule(intents, &prices, end)?;
        Ok(MODELS
            .into_iter()
            .map(
                |model| match replay(model, &initial, &schedule, &prices, &samples, start, end) {
                    Ok(points) => Reference {
                        model,
                        points,
                        unavailable: None,
                    },
                    Err(error) => Reference {
                        model,
                        points: vec![],
                        unavailable: Some(error.to_string()),
                    },
                },
            )
            .collect())
    })
    .await?
}

type Schedule = BTreeMap<(i64, Market), Intent>;

fn schedule(
    intents: Vec<Intent>,
    prices: &BTreeMap<Market, MarketPrices>,
    end: i64,
) -> Result<Schedule> {
    let mut result = BTreeMap::new();
    for intent in intents {
        let first = intent
            .accepted_us
            .checked_add(MINUTE_US - 1)
            .context("参考目标时间溢出")?
            .div_euclid(MINUTE_US)
            * MINUTE_US;
        let sessions = &prices[&intent.market].sessions;
        ensure!(!sessions.is_empty(), "缺少参考执行交易时段");
        let second = (first / 1_000_000 + 8 * 3600).rem_euclid(86400) as u32;
        let active = |second| {
            sessions.iter().any(|&(start, end)| {
                if start < end {
                    (start..end).contains(&second)
                } else {
                    second >= start || second < end
                }
            })
        };
        let wait_seconds = if active(second) {
            0
        } else {
            sessions
                .iter()
                .map(|&(open, _)| (open + 86400 - second) % 86400)
                .min()
                .unwrap()
        };
        let open = first + i64::from(wait_seconds.div_ceil(60)) * MINUTE_US;
        if open + MINUTE_US <= end {
            ensure!(
                active((open / 1_000_000 + 8 * 3600).rem_euclid(86400) as u32),
                "缺少完整的参考执行分钟"
            );
            // Targets accepted for the same contract and minute share a price.
            result.insert((open + MINUTE_US, intent.market.clone()), intent);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_uses_native_references_and_the_same_total_fee() -> Result<()> {
        let start = reference::timestamp("2026-11-19T01:00:00Z")?;
        let execution = start + 2 * MINUTE_US;
        let end = execution + MINUTE_US;
        let market = ("SHFE".into(), "rb2701".into());
        let prices = BTreeMap::from([(
            market.clone(),
            MarketPrices {
                multiplier: 10.0,
                closes: BTreeMap::from([(execution, 105.0), (end, 110.0)]),
                sessions: vec![(9 * 3600, 11 * 3600 + 30 * 60)],
                references: BTreeMap::from([(
                    execution,
                    ReferenceMinute {
                        trading_day: 20261119,
                        twap: Some(102.0),
                        vwap: Some(104.0),
                    },
                )]),
            },
        )]);
        let intents = [1, 2]
            .into_iter()
            .map(|target_lots| Intent {
                market: market.clone(),
                accepted_us: start + target_lots,
                trading_day: Some(20261119),
                target_lots,
            })
            .collect();
        let schedule = schedule(intents, &prices, end)?;
        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule.keys().next().unwrap().0, execution);
        let sample = |timestamp_us, fee| Point {
            timestamp_us,
            fees_cny: Some(fee),
            nav_before_fees_cny: 0.0,
            realized_cny: 0.0,
            floating_cny: 0.0,
            nav_after_fees_cny: Some(-fee),
            gross_position_cny: 0.0,
            net_position_cny: 0.0,
        };
        let samples = [sample(start, 0.0), sample(execution, 2.5), sample(end, 2.5)];
        let initial = BTreeMap::new();
        let twap = replay(
            Model::Twap,
            &initial,
            &schedule,
            &prices,
            &samples,
            start,
            end,
        )?;
        let vwap = replay(
            Model::Vwap,
            &initial,
            &schedule,
            &prices,
            &samples,
            start,
            end,
        )?;
        assert_eq!(twap[0].nav_change_after_fee_quote, 0.0);
        assert_eq!(twap[1].nav_change_before_fee_quote, 60.0);
        assert_eq!(twap[2].nav_change_after_fee_quote, 157.5);
        assert_eq!(vwap[2].nav_change_after_fee_quote, 117.5);

        let mut missing = prices;
        missing
            .get_mut(&market)
            .unwrap()
            .references
            .get_mut(&execution)
            .unwrap()
            .vwap = None;
        assert!(
            replay(
                Model::Vwap,
                &initial,
                &schedule,
                &missing,
                &samples,
                start,
                end
            )
            .is_err()
        );
        assert_eq!(
            replay(
                Model::Twap,
                &initial,
                &schedule,
                &missing,
                &samples,
                start,
                end
            )?,
            twap
        );
        Ok(())
    }
}

fn replay(
    model: Model,
    initial: &BTreeMap<Market, i64>,
    schedule: &Schedule,
    prices: &BTreeMap<Market, MarketPrices>,
    samples: &[Point],
    start: i64,
    end: i64,
) -> Result<Vec<TheoreticalNavPoint>> {
    let mut inventory = initial.clone();
    let value = |positions: &BTreeMap<Market, i64>, at| -> Result<f64> {
        positions.iter().try_fold(0.0, |total, (market, &lots)| {
            let price = &prices[market];
            let mark = price
                .mark(at)
                .with_context(|| format!("{} 缺少净值估价所需的闭合分钟", market_name(market)))?;
            Ok(total + lots as f64 * price.multiplier * mark)
        })
    };
    let baseline = value(&inventory, start)?;
    let mut cash = 0.0;
    let mut due = schedule.iter().peekable();
    let mut samples = samples.iter().peekable();
    let mut times = evaluation_times(start, end, schedule.keys().map(|(at, _)| *at));
    times.insert(start);
    times.extend(samples.clone().map(|point| point.timestamp_us));
    let mut points = Vec::new();
    for at in times {
        while let Some(((execute_us, _), intent)) =
            due.next_if(|((execute_us, _), _)| *execute_us <= at)
        {
            let lots = inventory.get(&intent.market).copied().unwrap_or_default();
            let delta = intent
                .target_lots
                .checked_sub(lots)
                .context("参考目标手数溢出")?;
            if delta == 0 {
                continue;
            }
            let prices = &prices[&intent.market];
            let price = prices
                .references
                .get(execute_us)
                .filter(|minute| {
                    intent
                        .trading_day
                        .is_none_or(|day| minute.trading_day == day)
                })
                .and_then(|minute| match model {
                    Model::Twap => minute.twap,
                    Model::Vwap => minute.vwap,
                })
                .with_context(|| {
                    let minute = chrono::DateTime::from_timestamp_micros(*execute_us - MINUTE_US)
                        .and_then(|t| t.checked_add_signed(chrono::Duration::hours(8)))
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    format!(
                        "{} 缺少北京时间 {minute} 的合约分钟参考价",
                        market_name(&intent.market)
                    )
                })?;
            cash -= delta as f64 * prices.multiplier * price;
            if intent.target_lots == 0 {
                inventory.remove(&intent.market);
            } else {
                inventory.insert(intent.market.clone(), intent.target_lots);
            }
        }
        let gross = cash + value(&inventory, at)? - baseline;
        ensure!(gross.is_finite(), "参考净值溢出");
        if let Some(point) = samples.next_if(|point| point.timestamp_us == at) {
            let fees = point.fees_cny.context("缺少当前区间的累计人民币手续费")?;
            let net = gross - fees;
            ensure!(net.is_finite(), "参考费后净值溢出");
            points.push(TheoreticalNavPoint {
                ts_us: at,
                nav_change_before_fee_quote: gross,
                nav_change_after_fee_quote: net,
                estimated_trading_fee_quote: fees,
            });
        }
    }
    Ok(points)
}
