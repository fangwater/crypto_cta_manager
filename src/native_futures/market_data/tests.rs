use super::*;
use cn_market_proto::{
    Contract, DataState, GetDataStateRequest, GetIntradayFramesRequest, GetMinuteBarsRequest,
    GetMinuteBarsResponse, GetSizeThresholdsRequest, GetSizeThresholdsResponse,
    GetTradingSessionsRequest, GetTradingSessionsResponse, IntradayFrameBatch, MinuteBar,
    cn_futures_market_server::{CnFuturesMarket, CnFuturesMarketServer},
};
use tonic::{Request, Response, Status};

fn pack() -> Pack {
    serde_json::from_str(include_str!(
        "../../../config/cn-futures-reference.example.json"
    ))
    .unwrap()
}
fn response() -> GetMinuteBarsResponse {
    let p = pack();
    GetMinuteBarsResponse {
        contract: Some(Contract {
            exchange: Exchange::Shfe as i32,
            product: "CU".into(),
            native_code: "cu2612".into(),
            canonical_code: "cu2612".into(),
            volume_multiple: p.contracts[0].contract_multiplier,
            price_tick: p.contracts[0].price_tick,
            price_tick_source: "instrument".into(),
        }),
        data_state: Some(DataState {
            source: "baseline_data_1min".into(),
            latest_trading_day: p.trading_day.parse().unwrap(),
            session_snapshot_as_of: "2026-11-14".into(),
            ..Default::default()
        }),
        bars: p
            .bars
            .iter()
            .map(|b| MinuteBar {
                open_time_ms: reference::timestamp(&b.open_time).unwrap() / 1000,
                trading_day: p.trading_day.parse().unwrap(),
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume_lots: b.volume_lots,
                turnover_cny: b.turnover,
                vwap: b.turnover / b.volume_lots / p.contracts[0].contract_multiplier,
                ..Default::default()
            })
            .collect(),
    }
}
fn now() -> i64 {
    reference::timestamp("2026-11-14T00:00:00Z").unwrap()
}
#[derive(Clone)]
struct Mock {
    response: Arc<Mutex<GetMinuteBarsResponse>>,
    queries: Arc<Mutex<Vec<GetMinuteBarsRequest>>>,
}
#[tonic::async_trait]
impl CnFuturesMarket for Mock {
    async fn get_minute_bars(
        &self,
        request: Request<GetMinuteBarsRequest>,
    ) -> Result<Response<GetMinuteBarsResponse>, Status> {
        let request = request.into_inner();
        let mut response = self.response.lock().unwrap().clone();
        match &request.range {
            Some(Range::Time(window)) => response
                .bars
                .retain(|b| (window.start_ms..=window.end_ms).contains(&b.open_time_ms)),
            Some(Range::TradingDay(day)) => response.bars.retain(|b| b.trading_day == *day),
            None => {}
        }
        self.queries.lock().unwrap().push(request);
        Ok(Response::new(response))
    }
    async fn get_data_state(
        &self,
        _: Request<GetDataStateRequest>,
    ) -> Result<Response<DataState>, Status> {
        Err(Status::unimplemented("not used"))
    }
    async fn get_size_thresholds(
        &self,
        _: Request<GetSizeThresholdsRequest>,
    ) -> Result<Response<GetSizeThresholdsResponse>, Status> {
        Err(Status::unimplemented("not used"))
    }
    async fn get_trading_sessions(
        &self,
        _: Request<GetTradingSessionsRequest>,
    ) -> Result<Response<GetTradingSessionsResponse>, Status> {
        Err(Status::unimplemented("not used"))
    }
    type StreamIntradayFramesStream =
        tokio_stream::wrappers::ReceiverStream<Result<IntradayFrameBatch, Status>>;
    async fn stream_intraday_frames(
        &self,
        _: Request<GetIntradayFramesRequest>,
    ) -> Result<Response<Self::StreamIntradayFramesStream>, Status> {
        Err(Status::unimplemented("not used"))
    }
}

#[tokio::test]
async fn exact_receipt_minute_proves_factual_multiplier_and_coalesces_parallel_requests() {
    let (state, url, task) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let feed = Feed::new(settings(url, temp.path())).unwrap();
    let day = pack().trading_day;
    let open_ms = response().bars[0].open_time_ms;
    let lookup = || feed.day_multiplier("SHFE", "cu2612", &day, open_ms * 1000 + 5, now());
    let (a, b) = tokio::join!(lookup(), lookup());
    assert_eq!(a.unwrap(), Some(5.0));
    assert_eq!(b.unwrap(), Some(5.0));
    let recorded = state.queries.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].range,
        Some(Range::Time(TimeRange {
            start_ms: open_ms,
            end_ms: open_ms
        }))
    );
    assert_eq!(feed.cache_status().fetched_candles, 0);
    task.abort();
}

#[tokio::test]
async fn recovered_fills_fall_back_to_their_own_trading_day_and_reject_unproved_days() {
    let (state, url, task) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let feed = Feed::new(settings(url, temp.path())).unwrap();
    let day = pack().trading_day;
    let open_ms = response().bars[0].open_time_ms;
    assert_eq!(
        feed.day_multiplier("SHFE", "cu2612", &day, now() - 1, now())
            .await
            .unwrap(),
        Some(5.0)
    );
    // A receipt minute whose trading day belongs to a different fill is
    // insufficient even when the provider returns an identical contract name.
    assert_eq!(
        feed.day_multiplier("SHFE", "cu2612", "20261112", open_ms * 1000, now())
            .await
            .unwrap(),
        None
    );
    let recorded = state.queries.lock().unwrap();
    assert_eq!(recorded.len(), 4);
    assert_eq!(recorded[1].range, Some(Range::TradingDay(20261113)));
    assert_eq!(recorded[3].range, Some(Range::TradingDay(20261112)));
    assert!(!recorded[1].include_empty_minutes);
    drop(recorded);
    assert_eq!(
        feed.day_multiplier("SHFE", "cu2612", "20261112", open_ms * 1000, now())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        state.queries.lock().unwrap().len(),
        4,
        "unsupported exact day is cached briefly"
    );
    task.abort();
}

#[tokio::test]
async fn multiplier_reuses_verified_archived_days_and_rejects_wrong_contracts() {
    let (state, url, task) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let settings = settings(url, temp.path());
    let day = pack().trading_day;
    let feed = Feed::new(settings.clone()).unwrap();
    let _ = feed.day("SHFE", "cu2612", &day, now()).await.unwrap();
    let feed = Feed::new(settings).unwrap();
    assert_eq!(
        feed.day_multiplier("SHFE", "cu2612", &day, now() - 1, now())
            .await
            .unwrap(),
        Some(5.0)
    );
    assert_eq!(state.queries.lock().unwrap().len(), 1);
    let receipt_us = response().bars[0].open_time_ms * 1000;
    assert!(
        feed.day_multiplier("SHFE", "ag2612", &day, receipt_us, now())
            .await
            .is_err()
    );
    assert_eq!(state.queries.lock().unwrap().len(), 2);
    task.abort();
}
async fn server() -> (Mock, String, tokio::task::JoinHandle<()>) {
    let state = Mock {
        response: Arc::new(Mutex::new(response())),
        queries: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let service = CnFuturesMarketServer::new(state.clone());
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (state, url, task)
}
fn settings(url: String, root: &Path) -> Settings {
    Settings {
        grpc_url: url,
        cache_root: root.to_owned(),
    }
}

#[tokio::test]
async fn native_grpc_units_concurrent_cost_misses_and_durable_archive_cache() {
    let (state, url, task) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let settings = settings(url, temp.path());
    let feed = Feed::new(settings.clone()).unwrap();
    let mut p = pack();
    p.bars.clear();
    let mut another = p.clone();
    another.source_id = "another-source".into();
    let (a, b) = tokio::join!(
        feed.augment(&mut p, now()),
        feed.augment(&mut another, now())
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(p.bars.len(), 2);
    assert_eq!(another.bars.len(), 2);
    assert_eq!(state.queries.lock().unwrap().len(), 1);
    let q = state.queries.lock().unwrap()[0].clone();
    assert_eq!(q.exchange, Exchange::Shfe as i32);
    assert_eq!(q.contract, "cu2612");
    assert_eq!(
        q.range,
        Some(Range::TradingDay(p.trading_day.parse().unwrap()))
    );
    assert!(q.include_empty_minutes);
    let report = cost::build(
        &p.source_id,
        &p.trading_day,
        Some(&p),
        super::super::reader::CostFacts {
            replication: None,
            orders: vec![],
            fills: vec![],
        },
        now(),
    )
    .unwrap();
    assert_eq!(report.signals[0].theoretical_price, Some(105.0));
    drop(feed);
    p.bars.clear();
    Feed::new(settings)
        .unwrap()
        .augment(&mut p, now())
        .await
        .unwrap();
    assert_eq!(p.bars.len(), 2);
    assert_eq!(
        state.queries.lock().unwrap().len(),
        1,
        "restart must reuse finalized archived candles"
    );
    task.abort();
}

#[tokio::test]
async fn live_finality_uses_provider_watermark_and_never_persists_the_in_progress_day() {
    let (state, url, task) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let feed = Feed::new(settings(url, temp.path())).unwrap();
    {
        let mut r = state.response.lock().unwrap();
        let day = r.bars[0].trading_day;
        let through = r.bars[0].open_time_ms + 60_000;
        let s = r.data_state.as_mut().unwrap();
        s.latest_trading_day = day - 1;
        s.live_trading_day = day;
        s.live_through_ms = through;
        for bar in &mut r.bars {
            bar.provisional = true;
        }
    }
    let day = pack().trading_day;
    let first = feed.day("SHFE", "cu2612", &day, now()).await.unwrap();
    assert_eq!(first.bars.len(), 1);
    assert_eq!(first.pending_bar_count, 1);
    assert!(!first.archived);
    assert!(!temp.path().join(format!("SHFE/cu2612/{day}.json")).exists());
    {
        let mut r = state.response.lock().unwrap();
        r.data_state.as_mut().unwrap().live_through_ms = r.bars[1].open_time_ms + 60_000;
    }
    let second = feed.day("SHFE", "cu2612", &day, now()).await.unwrap();
    assert_eq!(second.bars.len(), 2);
    assert_eq!(second.pending_bar_count, 0);
    assert_eq!(state.queries.lock().unwrap().len(), 2);
    task.abort();
}

#[tokio::test]
async fn omitted_archived_minutes_retry_and_provider_identity_changes_fail_closed() {
    let (state, url, task) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let feed = Feed::new(settings(url.clone(), temp.path())).unwrap();
    let missing = state.response.lock().unwrap().bars.pop().unwrap();
    let mut p = pack();
    p.bars.clear();
    feed.augment(&mut p, now()).await.unwrap();
    assert_eq!(p.bars.len(), 1);
    state.response.lock().unwrap().bars.push(missing);
    p.bars.clear();
    feed.augment(&mut p, now()).await.unwrap();
    assert_eq!(p.bars.len(), 2);
    assert_eq!(state.queries.lock().unwrap().len(), 2);
    let other = Feed::new(settings(url.replace("127.0.0.1", "localhost"), temp.path())).unwrap();
    assert!(
        other
            .day("SHFE", "cu2612", &p.trading_day, now())
            .await
            .is_err()
    );
    task.abort();
}

#[test]
fn native_conversion_preserves_original_czce_identity_and_rejects_wrong_units_and_days() {
    let mut r = response();
    let c = r.contract.as_mut().unwrap();
    c.exchange = Exchange::Czce as i32;
    c.native_code = "AP611".into();
    c.canonical_code = "ap2611".into();
    let day = pack().trading_day;
    let market = convert(r.clone(), "CZCE", "AP611", &day, now()).unwrap();
    assert_eq!(market.symbol, "AP611");
    assert_eq!(market.bars[0].symbol, "AP611");
    assert!(convert(r.clone(), "SHFE", "AP611", &day, now()).is_err());
    r.bars[0].turnover_cny *= 2.0;
    assert!(convert(r, "CZCE", "AP611", &day, now()).is_err());
    let mut r = response();
    r.bars[0].trading_day += 1;
    assert!(convert(r, "SHFE", "cu2612", &day, now()).is_err());
    let mut r = response();
    r.bars.reverse();
    assert!(convert(r, "SHFE", "cu2612", &day, now()).is_err());
}

#[test]
fn unknown_zero_volume_prices_remain_missing_and_no_close_mid_is_used_as_arrival_price() {
    let mut r = response();
    r.bars[0].volume_lots = 0.0;
    r.bars[0].turnover_cny = 0.0;
    r.bars[0].open = f64::NAN;
    r.bars[0].close = f64::NAN;
    r.bars[0].mid_price = Some(99999.0);
    r.bars[0].twap = Some(99999.0);
    let day = convert(r, "SHFE", "cu2612", &pack().trading_day, now()).unwrap();
    assert_eq!(day.bars.len(), 1);
    assert_eq!(day.unpriced_empty_minutes, 1);
    assert_eq!(day.bars[0].open, pack().bars[1].open);
}
