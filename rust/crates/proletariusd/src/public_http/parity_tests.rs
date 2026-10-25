use super::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Deserialize)]
struct Fixture {
    schema_version: u32,
    patterns: Vec<String>,
    marker_body: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    method: String,
    path_query: String,
    status: u16,
    allow: String,
    content_type: String,
    content_length: String,
    body: String,
    handler_calls: usize,
    matched_pattern: String,
}

// Rust-owned projections are removed from the frozen Go mux fixture while
// their legacy method contracts remain represented in LEGACY_METHOD_PATHS.
const RUST_OWNED_GO_MUX_PATHS: &[&str] = &[
    "/api/status",
    "/api/esim",
    "/api/esim/operation",
    "/api/esim/health",
    "/api/esim/download",
    "/api/esim/download/confirm",
    "/api/esim/switch",
    "/api/call-mode/retry",
    "/api/call/audio/start",
    "/api/call/audio/stop",
];

/// Expected statuses, Allow ordering, representation length and body bytes come
/// from Go's independently executed frozen ServeMux, not a Rust projection.
#[tokio::test]
async fn frozen_go_mux_method_and_head_wire_parity() {
    let bytes = std::env::var("SWITCH_HTTP_METHOD_PARITY_FIXTURE")
        .map(|path| std::fs::read(path).expect("read explicit HTTP method oracle"))
        .unwrap_or_else(|_| {
            include_bytes!("../../../../testdata/retired-go-oracles/http-methods.json").to_vec()
        });
    let fixture: Fixture = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.patterns.len(), 102);
    assert_eq!(fixture.cases.len(), 839, "complete frozen HTTP corpus");
    let mut names = BTreeSet::new();
    let paths: BTreeSet<_> = fixture
        .patterns
        .iter()
        .map(|p| p.split_once(' ').unwrap().1)
        .collect();
    let expected_go_paths = LEGACY_METHOD_PATHS
        .iter()
        .copied()
        .filter(|path| !RUST_OWNED_GO_MUX_PATHS.contains(path))
        .collect();
    assert_eq!(paths, expected_go_paths);
    let total_cases = fixture.cases.len();
    let mut native_cases = 0;
    let mut fallback_cases = 0;
    for case in fixture.cases {
        assert!(
            names.insert(case.name.clone()),
            "duplicate HTTP fixture name"
        );
        let method = Method::from_bytes(case.method.as_bytes()).unwrap();
        let uri: hyper::Uri = case.path_query.parse().unwrap();
        if admission(&method, uri.path()) == Admission::Unknown {
            assert_eq!(
                case.status, 404,
                "{}: preserve original fallback",
                case.name
            );
            assert_eq!(case.handler_calls, 0);
            fallback_cases += 1;
            continue;
        }
        native_cases += 1;
        let (mut client, server) = UnixStream::pair().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let body = fixture.marker_body.clone();
        let serving = tokio::spawn(async move {
            crate::http_server::builder()
                .serve_connection(
                    hyper_util::rt::TokioIo::new(server),
                    hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let calls = observed.clone();
                            let body = body.clone();
                            async move {
                                let head = request.method() == Method::HEAD;
                                let native = head_uses_get(request.uri().path());
                                let response =
                                    match admission(request.method(), request.uri().path()) {
                                        Admission::MethodNotAllowed(allow) => {
                                            method_not_allowed(allow)
                                        }
                                        Admission::Allowed | Admission::HeadAsGet => {
                                            let method = if head {
                                                "GET"
                                            } else {
                                                request.method().as_str()
                                            };
                                            calls
                                                .lock()
                                                .unwrap()
                                                .push(format!("{method} {}", request.uri().path()));
                                            Response::builder()
                                                .status(StatusCode::OK)
                                                .header(
                                                    CONTENT_TYPE,
                                                    "application/json; charset=utf-8",
                                                )
                                                .body(Full::new(Bytes::from(body)))
                                                .unwrap()
                                        }
                                        Admission::Unknown => {
                                            unreachable!("checked before connecting")
                                        }
                                    };
                                Ok::<_, Infallible>(if head {
                                    without_head_body(response, native)
                                } else {
                                    response
                                })
                            }
                        },
                    ),
                )
                .await
                .unwrap();
        });
        client.write_all(format!("{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", case.method, case.path_query).as_bytes()).await.unwrap();
        let mut wire = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut wire),
        )
        .await
        .unwrap()
        .unwrap();
        serving.await.unwrap();
        let boundary = wire.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
        let headers = std::str::from_utf8(&wire[..boundary]).unwrap();
        let mut lines = headers.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u16>()
            .unwrap();
        let fields: BTreeMap<_, _> = lines
            .map(|line| {
                let (k, v) = line.split_once(':').unwrap();
                (k.to_ascii_lowercase(), v.trim().to_owned())
            })
            .collect();
        let header = |key: &str| fields.get(key).map(String::as_str).unwrap_or("");
        assert_eq!(status, case.status, "{}", case.name);
        assert_eq!(header("allow"), case.allow, "{}", case.name);
        assert_eq!(header("content-type"), case.content_type, "{}", case.name);
        assert_eq!(
            header("content-length"),
            case.content_length,
            "{}",
            case.name
        );
        assert_eq!(&wire[boundary + 4..], case.body.as_bytes(), "{}", case.name);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), case.handler_calls, "{}", case.name);
        if let Some(pattern) = calls.first() {
            assert_eq!(pattern, &case.matched_pattern, "{}", case.name);
        }
    }
    assert_eq!(native_cases + fallback_cases, total_cases);
    assert!(native_cases > 190);
    assert!(fallback_cases > 0);
    println!(
        "Go mux differential: {native_cases} native wire cases, {fallback_cases} unchanged fallback decisions"
    );
}
