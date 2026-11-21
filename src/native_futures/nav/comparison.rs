//! Read-only comparison using immutable account target batches and native minute prices.
use super::*;
use crate::{native_futures::market_data, theoretical_nav::TheoreticalNavPoint};
use serde_json::Value;
use sqlx::PgPool;

#[derive(Clone, Copy)]
pub(crate) enum Model {
    Twap,
    Vwap,
    Twap5m,
    Vwap5m,
}

impl Model {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Twap => "native_twap",
            Self::Vwap => "native_vwap",
            Self::Twap5m => "native_twap5m",
            Self::Vwap5m => "native_vwap5m",
        }
    }

    pub(crate) fn window_secs(self) -> usize {
        match self {
            Self::Twap | Self::Vwap => 60,
            Self::Twap5m | Self::Vwap5m => 300,
        }
    }
}

const MODEL_GROUPS: [[Model; 2]; 2] = [[Model::Twap, Model::Vwap], [Model::Twap5m, Model::Vwap5m]];

pub(crate) struct Reference {
    pub(crate) model: Model,
    pub(crate) points: Vec<TheoreticalNavPoint>,
    pub(crate) unavailable: Option<String>,
}

pub(crate) fn unavailable(reason: &str) -> Vec<Reference> {
    MODEL_GROUPS
        .into_iter()
        .flatten()
        .map(|model| Reference {
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
        let mut references = Vec::new();
        for models in MODEL_GROUPS {
            let schedule = schedule(&intents, &prices, end, models[0].window_secs() / 60)?;
            references.extend(models.into_iter().map(|model| {
                match replay(model, &initial, &schedule, &prices, &samples, start, end) {
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
                }
            }));
        }
        Ok(references)
    })
    .await?
}

struct Execution<'a> {
    intent: &'a Intent,
    minute_ends: [i64; 5],
}

type Schedule<'a> = BTreeMap<(i64, Market), Execution<'a>>;

fn schedule<'a>(
    intents: &'a [Intent],
    prices: &BTreeMap<Market, MarketPrices>,
    end: i64,
    minutes: usize,
) -> Result<Schedule<'a>> {
    let mut result = BTreeMap::new();
    'intent: for intent in intents {
        let first = intent
            .accepted_us
            .checked_add(MINUTE_US - 1)
            .context("参考目标时间溢出")?
            .div_euclid(MINUTE_US)
            * MINUTE_US;
        let sessions = &prices[&intent.market].sessions;
        ensure!(!sessions.is_empty(), "缺少参考执行交易时段");
        let mut open = first;
        let mut minute_ends = [0; 5];
        for close in &mut minute_ends[..minutes] {
            let second = shanghai_second(open);
            if !active(sessions, second) {
                let wait = sessions
                    .iter()
                    .map(|&(session_open, _)| (session_open + 86400 - second) % 86400)
                    .min()
                    .unwrap();
                open += i64::from(wait.div_ceil(60)) * MINUTE_US;
            }
            *close = open + MINUTE_US;
            if *close > end {
                continue 'intent;
            }
            ensure!(
                active(sessions, shanghai_second(open)),
                "缺少完整的参考执行分钟"
            );
            open = *close;
        }
        // The latest target for the same contract and execution window wins.
        result.insert(
            (open, intent.market.clone()),
            Execution {
                intent,
                minute_ends,
            },
        );
    }
    Ok(result)
}

fn shanghai_second(timestamp: i64) -> u32 {
    (timestamp / 1_000_000 + 8 * 3600).rem_euclid(86400) as u32
}

fn active(sessions: &[(u32, u32)], second: u32) -> bool {
    sessions.iter().any(|&(start, end)| {
        if start < end {
            (start..end).contains(&second)
        } else {
            second >= start || second < end
        }
    })
}

fn execution_price(model: Model, execution: &Execution<'_>, prices: &MarketPrices) -> Result<f64> {
    let mut day = execution.intent.trading_day;
    let mut traded_minutes = 0;
    let mut price_sum = 0.0;
    let mut lots = 0.0;
    let mut turnover = 0.0;
    for &end in &execution.minute_ends[..model.window_secs() / 60] {
        let minute = prices.references.get(&end).with_context(|| {
            let open = chrono::DateTime::from_timestamp_micros(end - MINUTE_US)
                .and_then(|t| t.checked_add_signed(chrono::Duration::hours(8)))
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            format!("缺少北京时间 {open} 的合约分钟行情")
        })?;
        ensure!(
            *day.get_or_insert(minute.trading_day) == minute.trading_day,
            "参考执行窗口超出目标交易日"
        );
        match model {
            Model::Twap => return minute.twap.context("缺少合约分钟 TWAP"),
            Model::Vwap => return minute.vwap.context("缺少合约分钟 VWAP"),
            Model::Twap5m | Model::Vwap5m if minute.volume_lots == 0.0 => continue,
            Model::Twap5m => {
                price_sum += minute.vwap.context("缺少合约分钟 VWAP")?;
                traded_minutes += 1;
            }
            Model::Vwap5m => {
                lots += minute.volume_lots;
                turnover += minute.turnover_cny;
            }
        }
    }
    let price = match model {
        Model::Twap5m => price_sum / f64::from(traded_minutes),
        Model::Vwap5m => turnover / lots / prices.multiplier,
        _ => unreachable!("single-minute models return the supplied minute price"),
    };
    ensure!(price.is_finite() && price > 0.0, "参考窗口缺少有效成交价格");
    Ok(price)
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
                        volume_lots: 1.0,
                        turnover_cny: 1040.0,
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
            .collect::<Vec<_>>();
        let schedule = schedule(&intents, &prices, end, 1)?;
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

    #[test]
    fn five_minute_comparison_requires_closed_minutes_from_one_trading_day() -> Result<()> {
        let start = reference::timestamp("2026-11-19T02:13:00Z")?;
        let market = ("SHFE".into(), "rb2701".into());
        let intents = [Intent {
            market: market.clone(),
            accepted_us: start + 1,
            trading_day: Some(20261119),
            target_lots: -2,
        }];
        // One minute before the recess and four after it.
        let minute_ends = [2, 18, 19, 20, 21].map(|offset| start + offset * MINUTE_US);
        let references = minute_ends
            .into_iter()
            .zip([
                (100.0, 1.0),
                (110.0, 2.0),
                (120.0, 0.0),
                (130.0, 3.0),
                (140.0, 4.0),
            ])
            .map(|(end, (vwap, volume_lots))| {
                (
                    end,
                    ReferenceMinute {
                        trading_day: 20261119,
                        twap: Some(100.0),
                        vwap: (volume_lots > 0.0).then_some(vwap),
                        volume_lots,
                        turnover_cny: vwap * volume_lots * 10.0,
                    },
                )
            })
            .collect();
        let mut prices = BTreeMap::from([(
            market.clone(),
            MarketPrices {
                multiplier: 10.0,
                closes: BTreeMap::new(),
                sessions: vec![
                    (9 * 3600, 10 * 3600 + 15 * 60),
                    (10 * 3600 + 30 * 60, 11 * 3600 + 30 * 60),
                ],
                references,
            },
        )]);
        let end = minute_ends[4];
        assert!(schedule(&intents, &prices, end - 1, 5)?.is_empty());
        let scheduled = schedule(&intents, &prices, end, 5)?;
        let execution = &scheduled[&(end, market.clone())];
        assert_eq!(execution.minute_ends, minute_ends);
        assert_eq!(
            execution_price(Model::Twap5m, execution, &prices[&market])?,
            120.0
        );
        assert_eq!(
            execution_price(Model::Vwap5m, execution, &prices[&market])?,
            127.0
        );

        let window = prices.get_mut(&market).unwrap();
        window
            .references
            .get_mut(&minute_ends[4])
            .unwrap()
            .trading_day = 20261120;
        assert!(execution_price(Model::Twap5m, execution, window).is_err());
        window.references.remove(&minute_ends[4]);
        assert!(execution_price(Model::Vwap5m, execution, window).is_err());
        Ok(())
    }
}

fn replay(
    model: Model,
    initial: &BTreeMap<Market, i64>,
    schedule: &Schedule<'_>,
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
        while let Some((_, execution)) = due.next_if(|((execute_us, _), _)| *execute_us <= at) {
            let intent = execution.intent;
            let lots = inventory.get(&intent.market).copied().unwrap_or_default();
            let delta = intent
                .target_lots
                .checked_sub(lots)
                .context("参考目标手数溢出")?;
            if delta == 0 {
                continue;
            }
            let prices = &prices[&intent.market];
            let price = execution_price(model, execution, prices)
                .map_err(|error| anyhow::anyhow!("{} {error}", market_name(&intent.market)))?;
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
