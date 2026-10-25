//! Actual public gateway + retained Service + protected REGISTER and TCP EOF.
use super::renewal_peer::{Command, Event};
use super::*;
use serde_json::Value;

async fn http(
    state: &crate::ServerState,
    method: &str,
    path: &str,
) -> (StatusCode, hyper::HeaderMap, Bytes) {
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let state = state.clone();
    let serving = tokio::spawn(async move {
        crate::http_server::builder()
            .serve_connection(
                hyper_util::rt::TokioIo::new(server),
                hyper::service::service_fn(move |request| {
                    crate::handle_request(request, state.clone())
                }),
            )
            .await
            .unwrap();
    });
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
            .await
            .unwrap();
    let connected = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method(method)
                .uri(path)
                .header("x-proletarius-app-token", APP_TOKEN)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    // The complete response was checked above. macOS socket pairs can return
    // ENOTCONN on the client's final half-close after the peer has closed.
    let _connection_close = connected.await.unwrap();
    serving.await.unwrap();
    (status, headers, bytes)
}
async fn get(state: &crate::ServerState, path: &str) -> Value {
    let (status, _, body) = http(state, "GET", path).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_sessions_public_gateway_without_go_tracks_real_eof_cleanup() {
    if std::env::var_os("SWITCH_PRODUCTION_500_SESSIONS_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/sessions-500-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output = tokio::task::spawn_blocking(move || std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", "native_vowifi_session::service::production_integration::sessions_exchange::native_sessions_public_gateway_without_go_tracks_real_eof_cleanup", "--nocapture"]).env("SWITCH_PRODUCTION_500_SESSIONS_CHILD", "1").env("PROLETARIUS_VOWIFI_CONFIG", config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut fixture = Fixture::new();
        // The clean-build capsule can have a long build path. Keep this
        // synthetic legacy trap on its audited short TMPDIR so AF_UNIX bind
        // remains portable on Darwin.
        let socket = std::env::temp_dir().join(format!(
            "s500-{}.sock",
            &crate::multistandby_store::random_token().unwrap()[..8]
        ));
        Arc::get_mut(&mut fixture.service.0)
            .unwrap()
            .resources
            .socket = socket.clone();
        // A listening socket with no responder detects any regression to the
        // old Go observation RPC: native requests must complete independently.
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let state = fixture.sms_state();
        let subscription = native_device_sms_subscription_id(ICCID, IMSI).unwrap();
        let (server, mut client) = renewal_peer::new(fixture.root.clone(), subscription);
        let peer = ike_peer::Peer::with_renewal(server).await;
        fixture
            .service
            .start(intent::FrozenRequest {
                proxy: peer.proxy.to_string(),
                epdg: "epdg.invalid".into(),
                epdg_port: 500,
            })
            .await
            .unwrap();
        assert_eq!(client.next().await, Event::Registered(1));
        fixture.registered().await;
        assert!(
            fixture
                .lifecycle
                .handles_public_request(&hyper::Method::GET, "/api/reader/vowifi/sessions")
        );
        let list = get(&state, "/api/reader/vowifi/sessions").await;
        assert!(list.get("legacy_sessions_unavailable").is_none());
        assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(list["sessions"][0]["sim_id"], ICCID);
        assert_eq!(list["sessions"][0]["ims_state"], "registered");
        assert_eq!(list["sessions"][0]["sms_ready"], true);
        for path in ["/api/reader/vowifi", "/api/reader/vowifi/sessions"] {
            let (status, headers, body) = http(&state, "HEAD", path).await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.is_empty());
            assert!(
                headers[hyper::header::CONTENT_LENGTH]
                    .to_str()
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
                    > 0
            );
        }
        client.command(1, Command::CloseSignaling).await;
        assert_eq!(client.next().await, Event::SignalingClosed(1));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .service
                    .record()
                    .unwrap()
                    .unwrap()
                    .progress
                    .borrow()
                    .phase
                    == intent::Phase::Backoff
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let list = get(&state, "/api/reader/vowifi/sessions").await;
        assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
        let native = &list["sessions"][0];
        assert_eq!(native["tunnel_state"], "connecting");
        assert_eq!(native["ims_state"], "idle");
        assert_eq!(native["sms_ready"], false);
        assert_eq!(native["incoming_listener_started"], false);
        assert_eq!(native["cleanup_confirmed"], true);
        assert_eq!(native["progress_desc"], "连接已中断，正在等待重新连接");
        assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 1);
        fixture
            .service
            .stop(None, tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
        let list = get(&state, "/api/reader/vowifi/sessions").await;
        assert_eq!(list["sessions"], json!([]));
        assert_eq!(
            get(&state, "/api/reader/vowifi").await["cleanup_confirmed"],
            true
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "native list must not connect to Go"
        );
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
        let (status, _, _) = http(&state, "GET", "/api/reader/vowifi/sessions").await;
        assert_eq!(status, StatusCode::OK);
        fixture.finish().await;
        peer.finish().await;
    })
    .await
    .expect("actual sessions composition deadline");
}
