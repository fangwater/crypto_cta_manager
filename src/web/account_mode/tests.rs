use super::*;
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
struct Exchange {
    source_ip: IpAddr,
    calls: Arc<Mutex<BTreeMap<String, usize>>>,
    corrected: Arc<AtomicBool>,
}

async fn endpoint(
    State(exchange): State<Exchange>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    assert_eq!(peer.ip(), exchange.source_ip);
    assert_eq!(method, Method::GET);
    let key = headers
        .get("X-MBX-APIKEY")
        .or_else(|| headers.get("OK-ACCESS-KEY"))
        .unwrap()
        .to_str()
        .unwrap();
    *exchange
        .calls
        .lock()
        .unwrap()
        .entry(key.into())
        .or_default() += 1;
    if key == "okx" {
        assert_eq!(uri.path(), "/api/v5/account/config");
        return Json(json!({"code":"0","data":[{"acctLv":"3"}]})).into_response();
    }
    assert_eq!(uri.path(), "/fapi/v1/accountConfig");
    assert!(uri.query().unwrap().contains("recvWindow=5000"));
    assert!(uri.query().unwrap().contains("timestamp="));
    if key == "failed" && !exchange.corrected.load(Ordering::SeqCst) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"code":-1021,"msg":"Timestamp outside recvWindow"})),
        )
            .into_response();
    }
    Json(
        json!({"multiAssetsMargin":key != "rejected" || exchange.corrected.load(Ordering::SeqCst)}),
    )
    .into_response()
}

fn targets(qty: f64) -> BTreeMap<String, TargetPosition> {
    BTreeMap::from([("BTCUSDT".into(), TargetPosition::new(qty, 0).unwrap())])
}

#[tokio::test]
async fn startup_checks_once_isolates_failures_and_publish_never_requeries() {
    // Resolve a local address without sending a packet. Requests still use only
    // this test's local HTTP server, with the source's explicit address bound.
    let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap();
    let source_ip = route.local_addr().unwrap().ip();
    let exchange = Exchange {
        source_ip,
        calls: Arc::default(),
        corrected: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(any(endpoint))
        .with_state(exchange.clone());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut sources = Vec::new();
    for id in [
        "verified",
        "rejected",
        "failed",
        "okx",
        "disabled",
        "missing-ip",
    ] {
        let home = dir.path().join(id);
        std::fs::create_dir(&home).unwrap();
        let env = home.join("fixture.env");
        let venue = if id == "okx" {
            "okex-futures"
        } else {
            "binance-futures"
        };
        std::fs::write(&env, format!(
            "BINANCE_API_KEY={id}\nBINANCE_API_SECRET=fixture-secret\nBINANCE_ACCOUNT_MODE=STANDARD\nBINANCE_FAPI_URL={base}\nOKX_API_KEY={id}\nOKX_API_SECRET=fixture-secret\nOKX_PASSPHRASE=fixture-passphrase\nOKX_BASE_URL={base}\n"
        )).unwrap();
        if id != "missing-ip" {
            std::fs::write(
                home.join("trade_engine.toml"),
                format!("local_ips=['{source_ip}']\n"),
            )
            .unwrap();
        }
        sources.push(toml::from_str::<SourceConfig>(&format!(
            "id='{id}'\naccount='same-label'\nvenue='{venue}'\nrocksdb_path='{}'\nenv_path='{}'\nexec_config_url='http://127.0.0.1:1'\nenabled={}\n",
            home.join("data/persist_manager").display(), env.display(), id != "disabled"
        )).unwrap());
    }
    let modes = AccountModes::initialize(&sources).await.unwrap();
    assert_eq!(exchange.calls.lock().unwrap().len(), 4);
    assert!(exchange.calls.lock().unwrap().values().all(|n| *n == 1));
    exchange.corrected.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        for (index, expected) in [
            (0, None),
            (1, Some(StatusCode::CONFLICT)),
            (2, Some(StatusCode::BAD_GATEWAY)),
            (3, None),
            (4, Some(StatusCode::SERVICE_UNAVAILABLE)),
            (5, Some(StatusCode::BAD_GATEWAY)),
        ] {
            for qty in [1.0, -1.0] {
                assert_eq!(
                    modes
                        .check(&sources[index], &targets(qty))
                        .err()
                        .map(|e| e.status),
                    expected
                );
            }
            assert!(modes.check(&sources[index], &targets(0.0)).is_ok());
            assert!(modes.check(&sources[index], &BTreeMap::new()).is_ok());
        }
    }
    let mut unknown = sources[0].clone();
    unknown.id = "new-source".into();
    assert_eq!(
        modes.check(&unknown, &targets(1.0)).err().unwrap().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(exchange.calls.lock().unwrap().values().all(|n| *n == 1));

    // A new startup reevaluates corrected accounts; requests cannot refresh or
    // accidentally transfer a successful check to a different source ID.
    let restarted = AccountModes::initialize(&sources).await.unwrap();
    assert!(exchange.calls.lock().unwrap().values().all(|n| *n == 2));
    for source in &sources[..4] {
        std::fs::remove_file(source.env_path()).unwrap();
        assert!(restarted.check(source, &targets(1.0)).is_ok());
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
