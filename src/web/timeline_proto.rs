//! Browser-only projection. Financial reconstruction and export reports stay intact.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use prost::Message;
use serde::Deserialize;
use serde_json::{Value, json};

use super::TimelineSnapshot;
use crate::nav::{NavTimelinePoint, NavTotals};

pub(super) mod wire {
    include!(concat!(env!("OUT_DIR"), "/cta.nav.rs"));
}
use wire::{Column, Curve, CurveKind, Metric, TimeAxis, Timeline};

pub(super) const CONTENT_TYPE: &str = "application/x-protobuf";

pub(super) fn compressible(
    _: axum::http::StatusCode,
    _: axum::http::Version,
    headers: &axum::http::HeaderMap,
    _: &axum::http::Extensions,
) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .is_some_and(|value| value == CONTENT_TYPE)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) enum View {
    #[default]
    Portfolio,
    Symbols,
    Strategies,
    Theoretical,
}

fn proto_value(value: Value) -> prost_types::Value {
    use prost_types::value::Kind;
    prost_types::Value {
        kind: Some(match value {
            Value::Null => Kind::NullValue(0),
            Value::Bool(v) => Kind::BoolValue(v),
            Value::Number(v) => Kind::NumberValue(v.as_f64().expect("finite report number")),
            Value::String(v) => Kind::StringValue(v),
            Value::Array(v) => Kind::ListValue(prost_types::ListValue {
                values: v.into_iter().map(proto_value).collect(),
            }),
            Value::Object(v) => Kind::StructValue(prost_types::Struct {
                fields: v.into_iter().map(|(k, v)| (k, proto_value(v))).collect(),
            }),
        }),
    }
}

fn funding_metadata(summary: &crate::funding::FundingSummary, before: f64, after: f64) -> Value {
    json!({
        "summary": summary,
        "total_nav_before_fee_quote": before,
        "total_nav_after_fee_quote": after,
        "points": [],
        // The browser uses the aggregate and coverage, never individual income records.
        "records": [],
        "records_sampled": false,
    })
}

fn point_active(point: &NavTimelinePoint) -> bool {
    point.gross_position_value_quote != 0.0
        || point.net_position_value_quote != 0.0
        || point.totals != NavTotals::default()
}

fn metadata(snapshot: &TimelineSnapshot, view: View) -> Value {
    let mut result = json!({
        "generated_at_us": snapshot.generated_at_us,
        "generation_duration_ms": snapshot.generation_duration_ms,
        "execution_slippage": snapshot.execution_slippage,
        "theoretical": snapshot.theoretical.iter().map(|r| json!({
            "valuation": r.valuation,
            "execution_model": r.execution_model,
            "execution_window_secs": r.execution_window_secs,
            "price_basis": r.price_basis,
            "fee_basis": r.fee_basis,
            "available_from_us": r.available_from_us,
            "latest_point_ts_us": r.latest_point_ts_us,
            "sampled": r.sampled,
            "loading": r.loading,
            "unavailable_reason": r.unavailable_reason,
            "missing_price_count": r.missing_price_count,
            "legacy_fee_delta_count": r.legacy_fee_delta_count,
            "zero_volume_skipped_sample_count": r.zero_volume_skipped_sample_count,
            "zero_volume_fallback_sample_count": r.zero_volume_fallback_sample_count,
            "points": [],
        })).collect::<Vec<_>>(),
    });
    if view == View::Theoretical {
        return result;
    }
    let report = &snapshot.report;
    let funding = &snapshot.estimated_funding;
    result["report"] = json!({
        "valuation": report.valuation,
        "earliest_start_ts_us": report.earliest_start_ts_us,
        "start_ts_us": report.start_ts_us,
        "end_ts_us": report.end_ts_us,
        "selected_source_ids": report.selected_source_ids,
        "available_symbols": report.available_symbols,
        "selected_symbols": report.selected_symbols,
        "available_strategies": report.available_strategies,
        "summary": report.summary,
        "symbols": report.symbols,
        "sampled": report.sampled,
        "points": [],
        "symbol_points": [],
        "strategy_points": report.strategy_points.iter().map(|r| {
            let has_funding = funding.strategies.iter().filter(|f| f.strategy == r.strategy)
                .any(|f| f.income_quote != 0.0
                    || f.income_by_asset.values().any(|v| *v != 0.0)
                    || f.points.iter().any(|p| p.funding_income_quote != 0.0));
            json!({
                "strategy": r.strategy,
                "symbol_count": r.symbol_count,
                "gross_position_value_quote": r.gross_position_value_quote,
                "net_position_value_quote": r.net_position_value_quote,
                "summary": r.summary,
                "has_activity": has_funding || r.symbol_count != 0
                    || r.gross_position_value_quote != 0.0
                    || r.net_position_value_quote != 0.0
                    || r.summary != NavTotals::default()
                    || r.points.iter().any(point_active),
                "points": [],
            })
        }).collect::<Vec<_>>(),
    });
    result["funding"] = funding_metadata(
        &snapshot.funding.summary,
        snapshot.funding.total_nav_before_fee_quote,
        snapshot.funding.total_nav_after_fee_quote,
    );
    result["estimated_funding"] = json!({
        "reconciliation": funding.reconciliation,
        "loading": funding.loading,
        "unavailable_reason": funding.unavailable_reason,
        "synced_through_ts_us": funding.synced_through_ts_us,
        "income_quote": funding.income_quote,
        "income_by_asset": funding.income_by_asset,
        "total_nav_before_fee_quote": funding.total_nav_before_fee_quote,
        "total_nav_after_fee_quote": funding.total_nav_after_fee_quote,
        "settlement_count": funding.settlement_count,
        "boundary_fill_count": funding.boundary_fill_count,
        "points": [],
        "strategies": funding.strategies.iter().map(|r| json!({
            "strategy": r.strategy,
            "income_quote": r.income_quote,
            "income_by_asset": r.income_by_asset,
            "total_nav_before_fee_quote": r.total_nav_before_fee_quote,
            "total_nav_after_fee_quote": r.total_nav_after_fee_quote,
            "points": [],
        })).collect::<Vec<_>>(),
    });
    result
}

#[derive(Default)]
struct Encoder {
    axes: BTreeMap<Vec<i64>, u32>,
    message: Timeline,
}

impl Encoder {
    fn curve(
        &mut self,
        kind: CurveKind,
        key: String,
        timestamps: Vec<i64>,
        columns: Vec<(Metric, Vec<f64>)>,
    ) {
        if timestamps.is_empty() {
            return;
        }
        let next_axis = self.message.axes.len() as u32;
        let axis = *self.axes.entry(timestamps.clone()).or_insert_with(|| {
            self.message.axes.push(TimeAxis {
                start_us: timestamps[0],
                deltas_us: timestamps.windows(2).map(|t| t[1] - t[0]).collect(),
            });
            next_axis
        });
        let columns = columns
            .into_iter()
            .map(|(metric, mut values)| {
                // Preserve exact double bits, including negative zero and tiny rebates.
                if values.iter().all(|v| v.to_bits() == values[0].to_bits()) {
                    values.truncate(1);
                }
                Column {
                    metric: metric as i32,
                    values,
                }
            })
            .collect();
        self.message.curves.push(Curve {
            kind: kind as i32,
            key,
            axis,
            columns,
        });
    }

    fn actual(&mut self, kind: CurveKind, key: String, points: &[NavTimelinePoint]) {
        let column = |metric, selector: fn(&NavTimelinePoint) -> f64| {
            (metric, points.iter().map(selector).collect())
        };
        self.curve(
            kind,
            key,
            points.iter().map(|p| p.ts_us).collect(),
            vec![
                column(Metric::RealizedBeforeFee, |p| {
                    p.totals.realized_pnl_before_fee_quote
                }),
                column(Metric::FloatingPnl, |p| p.totals.floating_pnl_quote),
                column(Metric::TradingFee, |p| p.totals.estimated_trading_fee_quote),
                column(Metric::NavBeforeFee, |p| {
                    p.totals.nav_change_before_fee_quote
                }),
                column(Metric::NavAfterFee, |p| p.totals.nav_change_after_fee_quote),
                column(Metric::GrossPosition, |p| p.gross_position_value_quote),
                column(Metric::NetPosition, |p| p.net_position_value_quote),
            ],
        );
    }

    fn funding(
        &mut self,
        kind: CurveKind,
        key: String,
        points: &[crate::funding::FundingNavPoint],
    ) {
        self.curve(
            kind,
            key,
            points.iter().map(|p| p.ts_us).collect(),
            vec![
                (
                    Metric::FundingIncome,
                    points.iter().map(|p| p.funding_income_quote).collect(),
                ),
                (
                    Metric::TotalNavBeforeFee,
                    points
                        .iter()
                        .map(|p| p.total_nav_before_fee_quote)
                        .collect(),
                ),
                (
                    Metric::TotalNavAfterFee,
                    points.iter().map(|p| p.total_nav_after_fee_quote).collect(),
                ),
            ],
        );
    }
}

pub(super) fn encode(snapshot: &TimelineSnapshot, view: View) -> Result<Vec<u8>> {
    let mut encoder = Encoder::default();
    let meta = proto_value(metadata(snapshot, view));
    let Some(prost_types::value::Kind::StructValue(meta)) = meta.kind else {
        anyhow::bail!("invalid timeline metadata");
    };
    encoder.message.metadata = Some(meta);
    if view != View::Theoretical {
        let report = &snapshot.report;
        encoder.actual(CurveKind::Portfolio, String::new(), &report.points);
        if view == View::Symbols {
            for row in &report.symbol_points {
                // Catalog identity remains available. Only exactly empty curves are omitted.
                if row.points.iter().any(point_active) {
                    encoder.actual(CurveKind::Symbol, row.symbol.clone(), &row.points);
                }
            }
        }
        if view == View::Strategies {
            for row in &report.strategy_points {
                encoder.actual(CurveKind::Strategy, row.strategy.clone(), &row.points);
            }
            for row in &snapshot.estimated_funding.strategies {
                encoder.funding(
                    CurveKind::StrategyFunding,
                    row.strategy.clone(),
                    &row.points,
                );
            }
        }
        encoder.funding(
            CurveKind::ActualFunding,
            String::new(),
            &snapshot.funding.points,
        );
        encoder.funding(
            CurveKind::EstimatedFunding,
            String::new(),
            &snapshot.estimated_funding.points,
        );
    }
    for (index, reference) in snapshot.theoretical.iter().enumerate() {
        let points = &reference.points;
        encoder.curve(
            CurveKind::Theoretical,
            index.to_string(),
            points.iter().map(|p| p.ts_us).collect(),
            vec![
                (
                    Metric::NavBeforeFee,
                    points
                        .iter()
                        .map(|p| p.nav_change_before_fee_quote)
                        .collect(),
                ),
                (
                    Metric::NavAfterFee,
                    points
                        .iter()
                        .map(|p| p.nav_change_after_fee_quote)
                        .collect(),
                ),
                (
                    Metric::TradingFee,
                    points
                        .iter()
                        .map(|p| p.estimated_trading_fee_quote)
                        .collect(),
                ),
            ],
        );
    }
    let mut bytes = Vec::with_capacity(encoder.message.encoded_len());
    encoder
        .message
        .encode(&mut bytes)
        .context("encode browser NAV Protobuf")?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> TimelineSnapshot {
        let point = |ts_us, value: f64| NavTimelinePoint {
            ts_us,
            gross_position_value_quote: value.abs(),
            net_position_value_quote: value,
            totals: NavTotals {
                fill_count: u64::from(value != 0.0),
                realized_pnl_before_fee_quote: value,
                nav_change_before_fee_quote: value,
                nav_change_after_fee_quote: value + 0.000000000001,
                estimated_trading_fee_quote: -0.000000000001,
                ..Default::default()
            },
        };
        let times = [1_794_461_350_179_000, 1_794_462_250_179_000];
        let points = vec![point(times[0], 0.0), point(times[1], 3.125)];
        TimelineSnapshot {
            generated_at_us: times[1],
            generation_duration_ms: 12,
            report: crate::nav::NavTimelineReport {
                valuation: "quantity_fifo_closed_1m_window_delta",
                earliest_start_ts_us: times[0],
                start_ts_us: times[0],
                end_ts_us: times[1],
                selected_source_ids: vec!["account".into()],
                available_symbols: vec!["BTCUSDT".into(), "OLDUSDT".into(), "CARRYUSDT".into()],
                selected_symbols: vec!["BTCUSDT".into(), "OLDUSDT".into(), "CARRYUSDT".into()],
                available_strategies: vec!["alpha".into()],
                summary: points[1].totals,
                symbols: vec![],
                points: points.clone(),
                symbol_points: vec![
                    crate::nav::SymbolNavTimeline {
                        symbol: "BTCUSDT".into(),
                        points: points.clone(),
                    },
                    crate::nav::SymbolNavTimeline {
                        symbol: "OLDUSDT".into(),
                        points: times
                            .map(|ts_us| NavTimelinePoint {
                                ts_us,
                                gross_position_value_quote: 0.0,
                                net_position_value_quote: 0.0,
                                totals: NavTotals::default(),
                            })
                            .to_vec(),
                    },
                    crate::nav::SymbolNavTimeline {
                        symbol: "CARRYUSDT".into(),
                        points: times
                            .map(|ts_us| NavTimelinePoint {
                                ts_us,
                                gross_position_value_quote: 1e-15,
                                net_position_value_quote: -1e-15,
                                totals: NavTotals::default(),
                            })
                            .to_vec(),
                    },
                ],
                strategy_points: vec![crate::nav::StrategyNavTimeline {
                    strategy: "alpha".into(),
                    symbol_count: 1,
                    gross_position_value_quote: 3.125,
                    net_position_value_quote: 3.125,
                    summary: points[1].totals,
                    points,
                }],
                sampled: false,
            },
            theoretical: vec![Default::default()],
            execution_slippage: vec![Default::default()],
            funding: Default::default(),
            estimated_funding: Default::default(),
        }
    }

    #[test]
    fn views_share_exact_axes_and_keep_carried_dust_and_rebates() {
        let snapshot = snapshot();
        let symbols =
            Timeline::decode(encode(&snapshot, View::Symbols).unwrap().as_slice()).unwrap();
        assert_eq!(symbols.axes.len(), 1);
        assert_eq!(symbols.axes[0].start_us, snapshot.report.start_ts_us);
        assert_eq!(symbols.axes[0].deltas_us, [900_000_000]);
        let names = symbols
            .curves
            .iter()
            .filter(|c| c.kind == CurveKind::Symbol as i32)
            .map(|c| c.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["BTCUSDT", "CARRYUSDT"]);
        let fee = symbols.curves[0]
            .columns
            .iter()
            .find(|c| c.metric == Metric::TradingFee as i32)
            .unwrap();
        assert_eq!(fee.values, [-1e-12]);
        let nav = symbols.curves[0]
            .columns
            .iter()
            .find(|c| c.metric == Metric::NavAfterFee as i32)
            .unwrap();
        assert_eq!(
            nav.values[1].to_bits(),
            snapshot.report.points[1]
                .totals
                .nav_change_after_fee_quote
                .to_bits()
        );
        let portfolio =
            Timeline::decode(encode(&snapshot, View::Portfolio).unwrap().as_slice()).unwrap();
        assert_eq!(portfolio.curves.len(), 1);
        assert!(
            metadata(&snapshot, View::Portfolio)["report"]["strategy_points"][0]["has_activity"]
                .as_bool()
                .unwrap()
        );
        let theory =
            Timeline::decode(encode(&snapshot, View::Theoretical).unwrap().as_slice()).unwrap();
        assert!(theory.curves.is_empty());
        assert!(!theory.metadata.unwrap().fields.contains_key("report"));
    }

    #[test]
    fn protobuf_fixture_for_browser_and_corrupt_payload_checks() {
        let snapshot = snapshot();
        // Optional fixture generation lets the independent browser decoder verify this encoder.
        if let Ok(dir) = std::env::var("CTA_NAV_PROTO_FIXTURES") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                format!("{dir}/timeline.pb"),
                encode(&snapshot, View::Symbols).unwrap(),
            )
            .unwrap();
            std::fs::write(
                format!("{dir}/timeline.json"),
                serde_json::to_vec(&snapshot).unwrap(),
            )
            .unwrap();
        }
        assert!(Timeline::decode([0xff].as_slice()).is_err());
    }

    #[tokio::test]
    async fn protobuf_response_is_gzip_compressed_and_financial_bytes_round_trip() {
        use axum::{Router, routing::get};
        use std::io::Read;
        use tower_http::compression::{
            CompressionLayer,
            predicate::{DefaultPredicate, Predicate},
        };
        let expected = encode(&snapshot(), View::Symbols).unwrap();
        let payload = expected.clone();
        let app = Router::new()
            .route(
                "/timeline",
                get(move || {
                    let payload = payload.clone();
                    async move {
                        (
                            [
                                (axum::http::header::CONTENT_TYPE, CONTENT_TYPE),
                                (axum::http::header::VARY, "Accept"),
                            ],
                            payload,
                        )
                    }
                }),
            )
            .layer(
                CompressionLayer::new()
                    .gzip(true)
                    .compress_when(DefaultPredicate::new().and(compressible)),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{addr}/timeline"))
            .header("Accept-Encoding", "gzip")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.headers()["content-encoding"], "gzip");
        let vary = response
            .headers()
            .get_all("vary")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect::<Vec<_>>()
            .join(",");
        assert!(vary.contains("Accept"));
        assert!(vary.to_lowercase().contains("accept-encoding"));
        let compressed = response.bytes().await.unwrap();
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_ref())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, expected);
        task.abort();
    }

    #[tokio::test]
    async fn protobuf_handler_preserves_source_permissions_and_theory_is_an_overlay() {
        use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
        let (_dir, state, id, start, end) = super::super::tests::timeline_test_state(90);
        let query = || super::super::TimelineQuery {
            start_ms: Some(start / 1000),
            end_ms: Some(end / 1000),
            source_ids: Some(id.clone()),
            include_theoretical: Some(false),
            view: Some(View::Theoretical),
            ..Default::default()
        };
        let headers =
            HeaderMap::from_iter([(header::ACCEPT, HeaderValue::from_static(CONTENT_TYPE))]);
        let denied = super::super::browser_timeline_response(
            state.clone(),
            query(),
            true,
            Default::default(),
            headers.clone(),
        )
        .await
        .unwrap_or_else(|error| panic!("permission response failed: {}", error.0));
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let response = super::super::browser_timeline_response(
            state,
            query(),
            true,
            std::collections::BTreeSet::from([id]),
            headers,
        )
        .await
        .unwrap_or_else(|error| panic!("overlay response failed: {}", error.0));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], CONTENT_TYPE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let message = Timeline::decode(bytes.as_ref()).unwrap();
        let fields = message.metadata.unwrap().fields;
        assert!(!fields.contains_key("report"));
        assert!(!fields.contains_key("funding"));
        assert!(!fields.contains_key("estimated_funding"));
        assert!(fields.contains_key("theoretical"));
    }
}
