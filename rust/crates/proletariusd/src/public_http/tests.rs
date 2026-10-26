use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const APP: &str = "public-http-app-fixture";
const INTERNAL: &str = "public-http-internal-fixture";

struct Fixture {
    root: PathBuf,
    state: crate::ServerState,
}

impl Fixture {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        // The clean-build capsule can have a deliberately long source path.
        // Use its audited short TMPDIR so the synthetic Go trap remains below
        // Darwin's sockaddr_un limit. Normal workspace tests still point
        // TMPDIR at Switch/build/tmp.
        let root = std::env::temp_dir().join(format!(
            "h097-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let inbox = crate::message_archive::InboxState::default();
        let state = crate::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some(APP.to_owned()),
            internal_token: INTERNAL.to_owned(),
            multistandby_store: crate::multistandby_store::Store::open(&root).unwrap(),
            multistandby_operations: crate::multistandby_operations::Store::open(&root).unwrap(),
            device_send_store: crate::device_send_store::Store::open(&root).unwrap(),
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&root).unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::new(&root),
            sms_inbox: inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(inbox, root.join("sms-archive.json")),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&root),
            call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            go_socket_path: root.join("go.sock"),
            availability: crate::BackendAvailability::new(),
            reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
            snapshot_instance_id: "public-http-fixture".into(),
            snapshot_revision: Arc::new(AtomicU64::new(0)),
            eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: Arc::new(Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        };
        Self { root, state }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Copy)]
enum Surface {
    Public,
    Callback,
    Internal,
    ShutdownCallback,
}

#[derive(Debug)]
struct WireResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl WireResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn content_length(&self) -> usize {
        self.header("content-length")
            .expect("representation Content-Length")
            .parse()
            .unwrap()
    }

    fn assert_allow(&self, expected: &[&str]) {
        let actual: BTreeSet<_> = self
            .header("allow")
            .expect("405 must include Allow")
            .split(',')
            .map(str::trim)
            .collect();
        assert_eq!(actual, expected.iter().copied().collect());
    }
}

async fn exchange(
    state: crate::ServerState,
    surface: Surface,
    method: &str,
    path: &str,
    app_token: Option<&str>,
    internal_token: Option<&str>,
    body: &[u8],
) -> WireResponse {
    let (mut client, server) = UnixStream::pair().unwrap();
    let serving = tokio::spawn(async move {
        let result = match surface {
            Surface::Public | Surface::Callback => crate::serve_connection(server, state).await,
            Surface::Internal => crate::serve_internal_connection(server, state).await,
            Surface::ShutdownCallback => {
                crate::serve_shutdown_callback_connection(server, state).await;
                return;
            }
        };
        if let Err(error) = result {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "unexpected HTTP server error: {error}"
            );
        }
    });
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(token) = app_token {
        request.push_str(&format!("x-proletarius-app-token: {token}\r\n"));
    }
    if let Some(token) = internal_token {
        request.push_str(&format!("x-switch-internal-token: {token}\r\n"));
    }
    request.push_str("\r\n");
    client.write_all(request.as_bytes()).await.unwrap();
    client.write_all(body).await.unwrap();
    // Read raw wire bytes. A Hyper HEAD client deliberately hides response
    // bodies, and therefore cannot catch a server sending forbidden bytes.
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut bytes))
        .await
        .expect("HTTP response deadline")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("HTTP server joined")
        .unwrap();
    let end = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .expect("complete response headers");
    let header = std::str::from_utf8(&bytes[..end]).unwrap();
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    WireResponse {
        status,
        headers,
        body: bytes[end + 4..].to_vec(),
    }
}

async fn public(fixture: &Fixture, method: &str, path: &str, body: &[u8]) -> WireResponse {
    exchange(
        fixture.state.clone(),
        Surface::Public,
        method,
        path,
        Some(APP),
        None,
        body,
    )
    .await
}

#[tokio::test]
async fn support_snapshot_method_contract_is_native_authenticated_and_read_only() {
    let fixture = Fixture::new();
    let path = "/api/diagnostics/support";
    assert!(!super::LEGACY_METHOD_PATHS.contains(&path));
    assert!(super::NATIVE_METHOD_PATHS.contains(&path));
    assert!(!fixture.state.go_socket_path.exists());
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        for token in [None, Some("wrong-token")] {
            let denied = exchange(
                fixture.state.clone(),
                Surface::Public,
                method,
                path,
                token,
                None,
                &[],
            )
            .await;
            assert_eq!(denied.status, 401, "{method}: {denied:?}");
            assert!(denied.header("allow").is_none());
            assert_eq!(denied.header("cache-control"), Some("no-store"));
            if method == "HEAD" {
                assert!(denied.body.is_empty());
            }
        }
    }
    for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        let rejected = public(&fixture, method, path, b"ignored").await;
        assert_eq!(rejected.status, 405, "{method}: {rejected:?}");
        rejected.assert_allow(&["GET", "HEAD"]);
        assert_eq!(rejected.header("cache-control"), Some("no-store"));
        assert_eq!(rejected.body, b"Method Not Allowed\n");
    }
    // The wire fixture deliberately does not initialize the process-wide
    // recorder. Exercise the real unavailable handler and HEAD transformation;
    // export_snapshot tests separately cover successful bounded exports.
    let get = public(&fixture, "GET", path, &[]).await;
    let head = public(&fixture, "HEAD", path, &[]).await;
    assert_eq!(get.status, 503);
    let body: Value = serde_json::from_slice(&get.body).unwrap();
    assert_eq!(body["code"], "support_snapshot_unavailable");
    assert_eq!(head.status, get.status);
    assert_eq!(head.content_length(), get.body.len());
    assert_eq!(head.header("content-type"), get.header("content-type"));
    assert!(head.body.is_empty());
    for response in [&get, &head] {
        assert!(response.header("allow").is_none());
        assert_eq!(response.header("cache-control"), Some("no-store"));
    }
    assert!(!fixture.state.go_socket_path.exists());
    assert!(!fixture.root.join("support-diagnostics").exists());
}

#[tokio::test]
async fn g3_stop_control_public_rejections_always_disable_caching() {
    let fixture = Fixture::new(); // Neither reader nor module service is retained.
    for (route, scope) in [
        ("/api/reader/vowifi/stop-control", "reader"),
        ("/api/vowifi/stop-control", "module"),
        ("/api/vowifi/%73top-control", "module"),
    ] {
        let target = format!(
            "{route}?scope={scope}&daemon_epoch=epoch-1&owner_id=owner-1&owner_generation=1"
        );
        for method in ["GET", "HEAD", "POST"] {
            let stale = public(&fixture, method, &target, &[]).await;
            assert_eq!(stale.status, 409, "{method} {route}: no retained service");
            assert_eq!(stale.header("cache-control"), Some("no-store"));
            let invalid = public(&fixture, method, route, &[]).await;
            assert_eq!(invalid.status, 400);
            assert_eq!(invalid.header("cache-control"), Some("no-store"));
            let unauthorized = exchange(
                fixture.state.clone(),
                Surface::Public,
                method,
                &target,
                None,
                None,
                &[],
            )
            .await;
            assert_eq!(unauthorized.status, 401);
            assert_eq!(unauthorized.header("cache-control"), Some("no-store"));
        }
        let method = public(&fixture, "PUT", &target, &[]).await;
        assert!((400..600).contains(&method.status));
        assert_eq!(method.header("cache-control"), Some("no-store"));
        let nonempty = public(&fixture, "POST", &target, b"x").await;
        assert_eq!(nonempty.status, 400);
        assert_eq!(nonempty.header("cache-control"), Some("no-store"));
    }
}

#[tokio::test]
async fn runtime_reports_backend_app_build_and_source_identity_separately() {
    let fixture = Fixture::new();
    let response = public(&fixture, "GET", "/api/v2/runtime", &[]).await;
    assert_eq!(response.status, 200);
    let runtime: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(runtime["backend"], "rust");
    assert_eq!(runtime["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        runtime["app_release_version"],
        option_env!("SWITCH_APP_VERSION").unwrap_or("unknown")
    );
    assert_eq!(
        runtime["build_number"],
        option_env!("SWITCH_BUILD_NUMBER").unwrap_or("unknown")
    );
    assert_eq!(
        runtime["source_digest"],
        option_env!("SWITCH_SOURCE_DIGEST").unwrap_or("unknown")
    );
}

#[tokio::test]
async fn native_head_has_get_representation_headers_without_wire_body_or_go() {
    let fixture = Fixture::new();
    proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"))
        .replace(vec![
            serde_json::json!({"id":"archived-1","sender":"10086","content":"one"}),
            serde_json::json!({"id":"archived-2","sender":"10010","content":"two"}),
        ])
        .unwrap();
    assert!(!fixture.state.go_socket_path.exists());
    for path in [
        "/api/v2/runtime",
        "/api/v2/migration-readiness",
        "/api/v2/sms/forwarding/handoff",
        "/api/calls",
        "/api/sms/storage",
    ] {
        let _handoff_fixture = if path == "/api/v2/sms/forwarding/handoff" {
            Some(
                crate::forwarding_handoff_store::REQUEST_TEST_LOCK
                    .lock()
                    .await,
            )
        } else {
            None
        };
        let get = public(&fixture, "GET", path, &[]).await;
        let head = public(&fixture, "HEAD", path, &[]).await;
        assert_eq!(get.status, 200, "{path}: {get:?}");
        assert_eq!(head.status, get.status, "{path}");
        assert!(!get.body.is_empty(), "{path}");
        assert_eq!(head.content_length(), get.body.len(), "{path}");
        assert_eq!(head.header("content-type"), get.header("content-type"));
        assert!(head.body.is_empty(), "HEAD sent body for {path}: {head:?}");
    }
    let usage = public(&fixture, "GET", "/api/sms/storage", &[]).await;
    let usage: Value = serde_json::from_slice(&usage.body).unwrap();
    assert_eq!(usage["usage"]["LOCAL"]["used"], 2);
    assert_eq!(usage["usage"]["LOCAL"]["total"], 1000);
    assert!(usage["usage"].get("SM").is_none());
    assert!(!fixture.state.go_socket_path.exists());
}

#[tokio::test]
async fn sms_refresh_is_native_and_does_not_forward_when_no_serial_owner_is_open() {
    let fixture = Fixture::new();
    assert!(!fixture.state.go_socket_path.exists());
    let response = public(&fixture, "POST", "/api/sms/refresh", &[]).await;
    assert_eq!(response.status, 502);
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["code"], "usb_sms_refresh_unconfirmed");
    assert_eq!(body["error"], "USB 短信刷新尚未获得服务确认");
    assert!(body["recoverable"].as_bool().unwrap());
    assert!(!fixture.state.go_socket_path.exists());
}

#[tokio::test]
async fn public_method_rejections_are_authenticated_and_do_not_dispatch_writes() {
    let fixture = Fixture::new();
    for token in [None, Some("wrong-token")] {
        let response = exchange(
            fixture.state.clone(),
            Surface::Public,
            "HEAD",
            "/api/v2/runtime",
            token,
            None,
            &[],
        )
        .await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(response.header("allow").is_none());
    }
    for (method, path, methods) in [
        (
            "PATCH",
            "/api/module/flight-mode",
            &["GET", "HEAD", "POST"][..],
        ),
        ("GET", "/api/reader/esim/profile", &["DELETE", "PATCH"][..]),
        ("HEAD", "/api/sms/send", &["POST"][..]),
        ("GET", "/api/reader/connect", &["POST"][..]),
        ("HEAD", "/api/reader/disconnect", &["POST"][..]),
        ("DELETE", "/api/reader/refresh", &["POST"][..]),
        ("GET", "/api/reader/auth", &["POST"][..]),
        ("POST", "/api/reader/info", &["GET", "HEAD"][..]),
        ("GET", "/api/multistandby/start", &["POST"][..]),
        (
            "POST",
            "/api/multistandby/operations/op",
            &["GET", "HEAD"][..],
        ),
    ] {
        let response = public(&fixture, method, path, &[]).await;
        assert_eq!(response.status, 405, "{method} {path}: {response:?}");
        response.assert_allow(methods);
        assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(response.header("x-frame-options"), Some("DENY"));
        assert_eq!(response.header("referrer-policy"), Some("no-referrer"));
        if method == "HEAD" {
            assert!(response.body.is_empty());
        }
    }
    assert!(!fixture.root.join("sms-export-tmp").exists());
    assert!(!fixture.root.join("sms-forwarding-handoff.json").exists());
}

#[tokio::test]
async fn head_does_not_expand_internal_authentication_or_shutdown_cleanup() {
    let fixture = Fixture::new();
    for surface in [Surface::Public, Surface::Callback, Surface::Internal] {
        let unauthorized = exchange(
            fixture.state.clone(),
            surface,
            "HEAD",
            "/api/internal/usb-sms/inbox",
            Some(APP),
            None,
            &[],
        )
        .await;
        assert_eq!(
            unauthorized.status,
            if matches!(surface, Surface::Internal) {
                401
            } else {
                404
            }
        );
        assert!(unauthorized.body.is_empty());
        let get = exchange(
            fixture.state.clone(),
            surface,
            "GET",
            "/api/internal/usb-sms/inbox",
            None,
            Some(INTERNAL),
            &[],
        )
        .await;
        assert_eq!(
            get.status,
            if matches!(surface, Surface::Internal) {
                200
            } else {
                404
            }
        );
        let head = exchange(
            fixture.state.clone(),
            surface,
            "HEAD",
            "/api/internal/usb-sms/inbox",
            None,
            Some(INTERNAL),
            &[],
        )
        .await;
        assert_eq!(head.status, 404, "private GET did not gain HEAD alias");
        assert!(head.body.is_empty());
    }
    let private_public = exchange(
        fixture.state.clone(),
        Surface::Internal,
        "HEAD",
        "/api/v2/runtime",
        Some(APP),
        Some(INTERNAL),
        &[],
    )
    .await;
    assert_eq!(private_public.status, 404);
    assert!(private_public.body.is_empty());
    for token in [None, Some(APP)] {
        let response = exchange(
            fixture.state.clone(),
            Surface::ShutdownCallback,
            "HEAD",
            "/api/v2/vowifi/eap-exchange/release",
            token,
            None,
            &[],
        )
        .await;
        assert_eq!(response.status, if token.is_some() { 503 } else { 401 });
        assert!(response.body.is_empty());
    }
}

#[tokio::test]
async fn private_routes_require_the_internal_socket_even_with_both_tokens() {
    let fixture = Fixture::new();
    for path in [
        "/api/internal",
        "/api/internal/usb-sms/inbox",
        "/api/internal/module-radio/query",
        "/api/internal/multistandby/inventory",
        "/api/internal/reader-device-identity/resolve",
        "/api/internal/retired-route",
    ] {
        for surface in [Surface::Public, Surface::Callback] {
            for token in [None, Some(INTERNAL)] {
                let response = exchange(
                    fixture.state.clone(),
                    surface,
                    "GET",
                    path,
                    Some(APP),
                    token,
                    &[],
                )
                .await;
                assert_eq!(response.status, 404, "{path}: {response:?}");
                let body: Value = serde_json::from_slice(&response.body).unwrap();
                assert_eq!(body["code"], "route_not_found");
            }
        }
    }
    let admitted = exchange(
        fixture.state.clone(),
        Surface::Internal,
        "GET",
        "/api/internal/usb-sms/inbox",
        None,
        Some(INTERNAL),
        &[],
    )
    .await;
    assert_eq!(admitted.status, 200);
}

#[tokio::test]
async fn retired_private_snapshot_routes_cannot_dispatch_to_a_listening_go_socket() {
    let fixture = Fixture::new();
    let trap = UnixListener::bind(&fixture.state.go_socket_path).unwrap();
    fixture.state.availability.mark_ready();
    for path in [
        "/api/internal/device-sms-route",
        "/api/internal/esim/health-snapshot",
        "/api/internal/esim/overview-snapshot",
        "/api/internal/runtime/health-snapshot",
        "/api/internal/runtime/status-snapshot",
        "/api/internal/sms/devices/facts",
    ] {
        for surface in [Surface::Public, Surface::Internal] {
            let response = exchange(
                fixture.state.clone(),
                surface,
                "POST",
                path,
                Some(APP),
                Some(INTERNAL),
                b"{}",
            )
            .await;
            assert_eq!(response.status, 404, "{path}: {response:?}");
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["code"], "route_not_found", "{path}: {body}");
        }
    }
    let unknown = public(&fixture, "PATCH", "/api/future-fixture?keep=1", &[]).await;
    assert_eq!(unknown.status, 404);
    assert_eq!(
        unknown.body,
        br#"{"code":"route_not_found","error":"route not found"}"#
    );
    let wrong_method = public(&fixture, "POST", "/api/v2/runtime", &[]).await;
    assert_eq!(wrong_method.status, 404);
    assert!(wrong_method.header("allow").is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(40), trap.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disconnected_public_health_and_status_keep_authentication_and_head_parity() {
    let fixture = Fixture::new();
    let trap = UnixListener::bind(&fixture.state.go_socket_path).unwrap();
    // Even stale Ready metadata and a connectable legacy socket cannot revive
    // physical device facts or become the source of a public observation.
    fixture.state.availability.mark_ready();
    for path in ["/api/status", "/api/health"] {
        for method in ["GET", "HEAD"] {
            for token in [None, Some("wrong-token")] {
                let response = exchange(
                    fixture.state.clone(),
                    Surface::Public,
                    method,
                    path,
                    token,
                    Some(INTERNAL),
                    &[],
                )
                .await;
                assert_eq!(response.status, 401, "{method} {path}: {response:?}");
                if method == "HEAD" {
                    assert!(response.body.is_empty());
                }
            }
        }
        let get = public(&fixture, "GET", path, &[]).await;
        assert_eq!(get.status, 200, "{path}: {get:?}");
        let value: Value = serde_json::from_slice(&get.body).unwrap();
        assert!(value["usb_device"].is_null(), "{path}: {value}");
        if path == "/api/status" {
            assert_eq!(value["sim_inserted"], false);
            assert_eq!(value["ps_attached"], false);
            assert!(value["iccid"].is_null());
            assert!(value["imsi"].is_null());
        } else {
            assert_eq!(value["ok"], true);
            assert_eq!(value["esim_available"], false);
        }
        let head = public(&fixture, "HEAD", path, &[]).await;
        assert_eq!(head.status, get.status);
        assert_eq!(head.content_length(), get.body.len());
        assert_eq!(head.header("content-type"), get.header("content-type"));
        assert!(head.body.is_empty());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), trap.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disconnected_esim_health_has_native_error_schema_and_head_parity() {
    let fixture = Fixture::new();
    let trap = UnixListener::bind(&fixture.state.go_socket_path).unwrap();
    fixture.state.availability.mark_ready();
    let get = public(&fixture, "GET", "/api/esim/health", &[]).await;
    assert_eq!(get.status, 503, "{get:?}");
    let body: Value = serde_json::from_slice(&get.body).unwrap();
    assert_eq!(body["code"], "esim_unavailable");
    assert_eq!(body["retryable"], true);
    assert_eq!(body["recoverable"], true);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
    let head = public(&fixture, "HEAD", "/api/esim/health", &[]).await;
    assert_eq!(head.status, get.status);
    assert_eq!(head.content_length(), get.body.len());
    assert!(head.body.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(40), trap.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disconnected_esim_overview_cannot_refresh_through_legacy_socket() {
    let fixture = Fixture::new();
    let trap = UnixListener::bind(&fixture.state.go_socket_path).unwrap();
    fixture.state.availability.mark_ready();
    for path in ["/api/esim", "/api/esim?refresh=TRUE"] {
        let get = public(&fixture, "GET", path, &[]).await;
        assert_eq!(get.status, 503, "{get:?}");
        let body: Value = serde_json::from_slice(&get.body).unwrap();
        assert_eq!(body["code"], "esim_unavailable");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
        assert_eq!(body["recoverable"], true);
        let head = public(&fixture, "HEAD", path, &[]).await;
        assert_eq!(head.status, get.status);
        assert_eq!(head.content_length(), get.body.len());
        assert!(head.body.is_empty());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), trap.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn export_head_reads_existing_chunk_without_preparing_or_consuming_it() {
    let fixture = Fixture::new();
    let rejected = public(&fixture, "HEAD", "/api/v2/sms-export", &[]).await;
    assert_eq!(rejected.status, 400);
    assert!(rejected.header("allow").is_none());
    assert!(rejected.body.is_empty());
    assert!(!fixture.root.join("sms-export-tmp").exists());

    let prepared = public(
        &fixture,
        "POST",
        "/api/v2/sms-export",
        br#"{"source":"nwait","privacy":"masked"}"#,
    )
    .await;
    assert_eq!(prepared.status, 200, "{prepared:?}");
    let metadata: Value = serde_json::from_slice(&prepared.body).unwrap();
    let token = metadata["token"].as_str().unwrap();
    let path = format!("/api/v2/sms-export/{token}?offset=0");
    let first = public(&fixture, "GET", &path, &[]).await;
    assert_eq!(first.status, 200);
    let head = public(&fixture, "HEAD", &path, &[]).await;
    assert_eq!(head.status, first.status);
    assert_eq!(head.content_length(), first.body.len());
    assert_eq!(
        head.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(head.header("cache-control"), Some("no-store"));
    assert!(head.body.is_empty());
    let second = public(&fixture, "GET", &path, &[]).await;
    assert_eq!(
        second.body, first.body,
        "HEAD did not consume or rebuild export"
    );
    let invalid = public(
        &fixture,
        "HEAD",
        &format!("/api/v2/sms-export/{token}?offset=invalid"),
        &[],
    )
    .await;
    assert_eq!(invalid.status, 400);
    assert!(invalid.body.is_empty());
    let deleted = public(
        &fixture,
        "DELETE",
        &format!("/api/v2/sms-export/{token}"),
        &[],
    )
    .await;
    assert_eq!(deleted.status, 200);
    let expired = public(&fixture, "HEAD", &path, &[]).await;
    assert_eq!(expired.status, 410);
    assert!(expired.body.is_empty());
}
