//! Retired reader capabilities are rejected by the real production ingress,
//! independently of historical wire-oracle tests and Go availability.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::net::{UnixListener, UnixStream};

async fn request(
    state: crate::ServerState,
    path: &str,
    method: Method,
    boundary: u8,
    credential: bool,
) -> (StatusCode, Bytes) {
    let (client, server) = UnixStream::pair().unwrap();
    let serving = tokio::spawn(async move {
        let result = match boundary {
            0 => crate::serve_connection(server, state).await,
            1 => crate::serve_internal_connection(server, state).await,
            _ => {
                crate::serve_internal_shutdown_connection(server, state).await;
                return;
            }
        };
        if let Err(error) = result {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let mut request = Request::builder().method(method).uri(path);
    if credential {
        request = request
            .header(crate::APP_TOKEN_HEADER, "app-fixture")
            .header("X-Switch-Internal-Token", "internal-fixture");
    }
    // Parsing this as a legacy capability must not be reached.
    let response = sender
        .send_request(
            request
                .body(Full::new(Bytes::from_static(b"{invalid")))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    serving.await.unwrap();
    (status, body)
}

#[tokio::test]
async fn retired_reader_wire_cannot_reach_go_or_physical_owner_on_any_ingress() {
    let root = crate::tests::test_directory("retired-reader-wire-505");
    let mut state = crate::reader_pcsc_gateway_tests::state(&root);
    let go_path = root.join("retired-go.sock");
    let listener = UnixListener::bind(&go_path).unwrap();
    state.go_socket_path = go_path;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let trap = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                incoming = listener.accept() => {
                    let (stream, _) = incoming.unwrap();
                    let count = observed.clone();
                    clients.spawn(async move {
                        let _ = crate::http_server::builder().serve_connection(
                            TokioIo::new(stream), hyper::service::service_fn(move |_| {
                                count.fetch_add(1, Ordering::SeqCst);
                                async { Ok::<_, Infallible>(crate::json_response(
                                    StatusCode::OK, &serde_json::json!({"unexpected_go":true}))) }
                            })).await;
                    });
                }
                _ = &mut stopped => break,
            }
        }
        while let Some(joined) = clients.join_next().await {
            joined.unwrap();
        }
    });
    let mut paths = vec![
        "/api/internal/reader-service".to_owned(),
        "/api/internal/reader-ble-transport".to_owned(),
    ];
    for action in [
        "open", "status", "acquire", "renew", "release", "transmit", "reset", "close",
    ] {
        paths.push(format!("/api/internal/reader-pcsc/{action}"));
    }
    for action in ["acquire", "status", "release", "renew", "quiesce"] {
        paths.push(format!(
            "/api/internal/multistandby/reader-reservation/operation/{action}"
        ));
    }
    let before = state.reader_gate.selection_snapshot();
    for ready in [false, true] {
        if ready {
            state.availability.mark_ready();
        }
        for boundary in 0..3 {
            for credential in [false, true] {
                for path in &paths {
                    for method in [Method::POST, Method::GET, Method::HEAD, Method::DELETE] {
                        let (status, body) =
                            request(state.clone(), path, method.clone(), boundary, credential)
                                .await;
                        assert_eq!(
                            status,
                            StatusCode::NOT_FOUND,
                            "{path} {method} boundary={boundary} credential={credential} ready={ready}: {body:?}"
                        );
                        if method != Method::HEAD {
                            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                            assert_eq!(value["code"], "route_not_found");
                        }
                    }
                    assert_eq!(
                        crate::ownership::route_owner("POST", path),
                        crate::ownership::RouteOwner::Unknown
                    );
                }
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.reader_gate.selection_snapshot(), before);
    stop.send(()).unwrap();
    trap.await.unwrap();
    state.reader_pcsc.shutdown().await.unwrap();
    state
        .modem_actor
        .shutdown()
        .expect("final modem close acknowledgement");
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}
