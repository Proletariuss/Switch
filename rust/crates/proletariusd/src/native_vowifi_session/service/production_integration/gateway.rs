//! Actual public HTTP ingress: production authentication/normalization, route
//! ownership and handler dispatch. No test router or injected readiness result.
use super::*;
pub(super) async fn request(
    fixture: &Fixture,
    request: Request<Full<Bytes>>,
) -> hyper::Response<Full<Bytes>> {
    send(&fixture.sms_state(), request).await
}
pub(super) async fn send(
    state: &crate::ServerState,
    mut request: Request<Full<Bytes>>,
) -> hyper::Response<Full<Bytes>> {
    request.headers_mut().insert(
        hyper::header::HeaderName::from_static("x-proletarius-app-token"),
        hyper::header::HeaderValue::from_static(APP_TOKEN),
    );
    send_without_app_token(state, request).await
}
async fn send_without_app_token(
    state: &crate::ServerState,
    request: Request<Full<Bytes>>,
) -> hyper::Response<Full<Bytes>> {
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let state = state.clone();
    let serving = tokio::spawn(async move {
        let result = crate::http_server::builder()
            .serve_connection(
                hyper_util::rt::TokioIo::new(server),
                hyper::service::service_fn(move |request| {
                    crate::handle_request(request, state.clone())
                }),
            )
            .await;
        if let Err(error) = result {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
            .await
            .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender.send_request(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    serving.await.unwrap();
    hyper::Response::from_parts(parts, Full::new(bytes))
}
pub(super) async fn stop(fixture: &Fixture) {
    let response = request(
        fixture,
        Request::builder()
            .method("POST")
            .uri("/api/reader/vowifi")
            .body(Full::new(Bytes::from(serde_json::to_vec(&json!({"enable":false,"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id()})).unwrap())))
            .unwrap(),
    )
    .await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["accepted"], true);
    assert_eq!(value["operation"]["requested_enable"], false);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fixture
                .service
                .operation_projection()
                .unwrap()
                .cleanup_confirmed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("public stop must observe actual owner cleanup");
}

async fn json(
    state: &crate::ServerState,
    method: &str,
    path: &str,
    value: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = send(
        state,
        Request::builder()
            .method(method)
            .uri(path)
            .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
            .unwrap(),
    )
    .await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_gateway_fences_legacy_claim_without_forwarding_retired_module_routes() {
    let fixture = Fixture::new();
    let mut state = fixture.sms_state();
    let unauthorized = send_without_app_token(
        &state,
        Request::builder()
            .method("POST")
            .uri("/api/reader/vowifi")
            .body(Full::new(Bytes::from(
                serde_json::to_vec(&json!({
                    "observe_operation": true,
                    "client_request_id": crate::snapshot::new_instance_id(),
                    "enable": true
                }))
                .unwrap(),
            )))
            .unwrap(),
    )
    .await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let unauthorized: serde_json::Value =
        serde_json::from_slice(&unauthorized.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert_eq!(unauthorized["code"], "authentication_required");
    assert!(fixture.service.record().unwrap().is_none());
    assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 0);
    assert!(!fixture.lifecycle.ordinary_admission().has_ordinary_claim());
    // The public no-session result must remain native when Go is absent.
    for prefix in ["/api/reader/vowifi", "/api/vowifi"] {
        let (status, value) =
            json(&state, "GET", &format!("{prefix}/calls/status"), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{prefix}: {value}");
        assert_eq!(value["call"]["state"], "unavailable");
    }
    let (status, value) = json(
        &state,
        "POST",
        "/api/reader/vowifi/ussd",
        json!({"client_request_id":"production-gateway-ussd-no-session","generation":1,"command":"*123#"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"], "读卡器 VoWiFi 尚未连接");
    // The clean-build source path may exceed Darwin's sockaddr_un limit.
    // Its audited TMPDIR is short; normal workspace tests keep TMPDIR under
    // Switch/build/tmp.
    let path = std::env::temp_dir().join(format!(
        "g498-{}.sock",
        &crate::multistandby_store::random_token().unwrap()[..8]
    ));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    state.go_socket_path = path.clone();
    state.availability.mark_ready();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let (stop, mut stopping) = watch::channel(false);
    let legacy = tokio::spawn(async move {
        loop {
            tokio::select! {
                pair=listener.accept()=>{let(stream,_)=pair.unwrap();let observed=observed.clone();tokio::spawn(async move {let _=crate::http_server::builder().serve_connection(hyper_util::rt::TokioIo::new(stream),hyper::service::service_fn(move |request:Request<hyper::body::Incoming>|{observed.fetch_add(1,Ordering::SeqCst);let path=request.uri().path().to_owned();async move{Ok::<_,std::convert::Infallible>(crate::json_response(StatusCode::OK,&if path=="/api/internal/reader/vowifi/call-status" {json!({"generation":498,"source":"module","call":{"state":"unavailable","media_ready":false,"early_media":false,"signaling_ready":false,"rtp_packets_sent":0,"rtp_packets_received":0,"rtp_decode_errors":0,"audio_frames_to_host":0}})}else{json!({"legacy_module":true,"path":path})}))}})).await;});},
                _=stopping.changed()=>break,
            }
        }
    });
    let admission = fixture.lifecycle.ordinary_admission();
    admission.set_go_producer("module-isolation-498").unwrap();
    let claim = admission
        .acquire_go("module-isolation-498", "module", "module-claim", 1)
        .unwrap();
    // A stale compatibility claim still fences acquisition, but cannot route
    // retired reader/module APIs back to Go or fabricate a native capability.
    // The real listener is a trap: every request must leave its count at zero.
    for path in [
        "/api/reader/vowifi/calls/status",
        "/api/vowifi/calls/status",
        "/api/vowifi",
    ] {
        let (status, value) = json(&state, "GET", path, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        if path.ends_with("/calls/status") {
            assert!(value.get("source").is_none(), "{path}: {value}");
            assert_eq!(value["generation"], 0);
            assert_eq!(value["call"]["state"], "unavailable");
            assert_eq!(value["call"]["signaling_ready"], false);
        } else {
            assert_eq!(value["state"], "unavailable", "{path}: {value}");
            assert_eq!(value["ims_registered"], false);
            assert_eq!(value["sms_ready"], false);
            assert_eq!(value["call_ready"], false);
            assert!(value.get("legacy_module").is_none());
        }
    }
    let (status, value) = json(
        &state,
        "POST",
        "/api/reader/vowifi/ussd",
        json!({"client_request_id":"production-gateway-ussd-stale-claim","generation":1,"command":"*123#"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"], "读卡器 VoWiFi 尚未连接");
    assert!(value.get("legacy_module").is_none());
    let before = count.load(Ordering::SeqCst);
    assert_eq!(before, 0, "no retired module API may contact the Go trap");
    let preview = crate::snapshot::capture_reader_vowifi(&state)
        .await
        .unwrap();
    assert_eq!(preview.ims_state, "idle");
    assert_eq!(
        count.load(Ordering::SeqCst),
        before,
        "idle reader snapshot must not call the legacy module or retired reader observer"
    );
    let (status, value) = json(&state, "POST", "/api/reader/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(
        count.load(Ordering::SeqCst),
        before,
        "reader start must not invoke the legacy module worker"
    );
    let (status, value) = json(&state, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(
        !state
            .native_vowifi_lifecycle
            .retained_module_service()
            .unwrap()
            .owns_session()
    );
    assert_eq!(fixture.card.auth.load(Ordering::SeqCst), 0);
    assert!(claim.is_current());
    claim.release_confirmed().unwrap();
    let (status, value) = json(&state, "GET", "/api/reader/vowifi/calls/status", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["call"]["state"], "unavailable");
    assert_eq!(count.load(Ordering::SeqCst), before);
    stop.send_replace(true);
    legacy.await.unwrap();
    std::fs::remove_file(path).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    fixture.service.begin_shutdown();
    fixture.service.finish_before(deadline).await.unwrap();
    fixture.lifecycle.finish_shutdown(deadline).await.unwrap();
    fixture
        .service
        .0
        .resources
        .pcsc
        .native_close(&fixture.session)
        .unwrap();
    assert_eq!(
        fixture.card.closed.load(Ordering::SeqCst),
        0,
        "routing-only fixture never opened the lazy physical transport"
    );
    assert!(!fixture.lifecycle.ordinary_admission().has_ordinary_claim());
}
