//! Aggregate admission budget for Rust-owned HTTP request bodies.
//!
//! Route handlers retain their own exact per-request byte limits. This layer
//! reserves those maxima before dispatch so concurrent collectors cannot each
//! consume their full individual allowance at once. A reserved pool stays
//! available for durable SMS acknowledgement/ingestion and resource release.

use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::CONTENT_LENGTH;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const UNIT_BYTES: usize = 64 * 1024;
const NORMAL_UNITS: usize = 128; // 8 MiB
const CONTROL_UNITS: usize = 128; // 8 MiB reserved for durable SMS and resource release

#[derive(Clone)]
struct Budget {
    normal: Arc<Semaphore>,
    control: Arc<Semaphore>,
}

impl Budget {
    fn new() -> Self {
        Self {
            normal: Arc::new(Semaphore::new(NORMAL_UNITS)),
            control: Arc::new(Semaphore::new(CONTROL_UNITS)),
        }
    }

    fn reserve(&self, maximum_bytes: usize, control: bool) -> Result<OwnedSemaphorePermit, ()> {
        let units = maximum_bytes.div_ceil(UNIT_BYTES).max(1);
        let units = u32::try_from(units).map_err(|_| ())?;
        let pool = if control { &self.control } else { &self.normal };
        pool.clone().try_acquire_many_owned(units).map_err(|_| ())
    }
}

fn global() -> &'static Budget {
    static BUDGET: OnceLock<Budget> = OnceLock::new();
    BUDGET.get_or_init(Budget::new)
}

/// Acquire aggregate capacity for a Rust-owned body. `None` means that this
/// request route does not collect a request body in Rust.
pub(crate) fn try_reserve<B>(request: &Request<B>) -> Result<Option<OwnedSemaphorePermit>, ()> {
    try_reserve_in(global(), request)
}

fn try_reserve_in<B>(
    budget: &Budget,
    request: &Request<B>,
) -> Result<Option<OwnedSemaphorePermit>, ()> {
    let Some((maximum_bytes, control)) = body_limit(request.method(), request.uri().path()) else {
        return Ok(None);
    };
    let declared_bytes = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    // A valid Content-Length is enforced by Hyper's framing parser. Chunked
    // or malformed lengths reserve the route's entire byte ceiling.
    let reservation_bytes = declared_bytes
        .map(|length| length.min(maximum_bytes))
        .unwrap_or(maximum_bytes);
    budget.reserve(reservation_bytes, control).map(Some)
}

pub(crate) fn capacity_response() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({
            "error":"请求体处理容量已满，操作尚未提交",
            "code":"request_body_capacity_exhausted",
            "retryable":true
        }),
    )
}

fn body_limit(method: &Method, path: &str) -> Option<(usize, bool)> {
    if method != Method::POST
        && method != Method::PUT
        && method != Method::PATCH
        && method != Method::DELETE
    {
        return None;
    }

    if path == crate::ordinary_vowifi_admission::PATH {
        return Some((4 * 1024, true));
    }
    if path == crate::ims_sms_transactions::INTERNAL_PATH {
        return Some((128 * 1024, true));
    }
    // Session stop and exact-ID reconciliation must retain ingress capacity
    // while ordinary callers are saturated. Collection is still bounded by
    // the selected handler; the permit never represents a released owner.
    if matches!(
        path,
        "/api/vowifi"
            | "/api/reader/vowifi"
            | "/api/vowifi/reconcile"
            | "/api/reader/vowifi/reconcile"
            | "/api/reader/vowifi/stop-control"
            | "/api/vowifi/stop-control"
    ) {
        return Some((4 * 1024, true));
    }
    if let Some(action) = path
        .strip_prefix("/api/vowifi/calls/")
        .or_else(|| path.strip_prefix("/api/reader/vowifi/calls/"))
    {
        if matches!(action, "dial" | "answer" | "reject" | "hangup" | "dtmf") {
            return Some((4 * 1024, matches!(action, "reject" | "hangup")));
        }
    }
    if matches!(
        path,
        "/api/reader/vowifi/ussd" | "/api/reader/vowifi/proxy-check"
    ) {
        return Some((4 * 1024, false));
    }

    if path.starts_with("/api/internal/profile-notes/") {
        return Some((crate::profile_notes::MAX_REQUEST_BYTES, false));
    }
    if path == "/api/internal/diagnostics/events/append" {
        return Some((4 * 1024, false));
    }
    if path.starts_with("/api/internal/usb-at/") {
        // The three SIM RPCs use this same collector too. Charging a smaller
        // DTO expectation would allow concurrent partial bodies to exceed the
        // pool's byte ceiling before decoding ever rejects them.
        return Some((
            crate::usb_at::MAX_BYTES,
            matches!(
                path,
                "/api/internal/usb-at/release" | "/api/internal/usb-at/close"
            ),
        ));
    }
    if path.starts_with("/api/internal/usb-sms/") {
        let maximum = if path == "/api/internal/usb-sms/storage-operation" {
            128 * 1024
        } else {
            40 * 1024
        };
        return Some((maximum, false));
    }
    if path.starts_with("/api/internal/module-radio/") {
        // The handler checks the action and old-token authority again. Reserve
        // cleanup capacity even when ordinary API collectors are saturated.
        return Some((
            32 * 1024,
            matches!(
                path,
                "/api/internal/module-radio/reservation" | "/api/internal/module-radio/vowifi"
            ),
        ));
    }
    if path.starts_with("/api/internal/call-archive/") {
        return Some((16 * 1024, true));
    }
    if path.starts_with("/api/internal/modem/") {
        if path == "/api/internal/modem/submit-sms" {
            return Some((400 * 1024, false));
        }
        if path == "/api/internal/modem/project-call-state" {
            return Some((
                crate::modem_actor::MAX_CALL_STATE_PROJECTION_REQUEST_BYTES,
                false,
            ));
        }
        return Some((12 * 1024, path == "/api/internal/modem/close"));
    }
    if path == "/api/internal/reader-lifecycle" {
        return Some((32 * 1024, true));
    }
    if path.starts_with("/api/internal/qmi/") {
        return Some((16 * 1024, path.ends_with("/close")));
    }
    if path == "/api/internal/reader-business" {
        return Some((4 * 1024, true));
    }
    if path.starts_with("/api/internal/device-send/") {
        return Some((128 * 1024, path.ends_with("/finish")));
    }
    if path.starts_with("/api/internal/multistandby/store/") {
        let control = matches!(
            path,
            "/api/internal/multistandby/store/inbound" | "/api/internal/multistandby/store/outcome"
        );
        return Some((1024 * 1024, control));
    }
    if path.starts_with("/api/internal/sms/") {
        let maximum = match path {
            "/api/internal/sms/devices/select" => 32 * 1024,
            "/api/internal/sms/archive/ingest" => 8 * 1024 * 1024,
            "/api/internal/sms/archive/delete-ids" => 128 * 1024,
            _ => 16 * 1024,
        };
        return Some((maximum, path == "/api/internal/sms/archive/ingest"));
    }
    if path.starts_with("/api/sms/forwarding/") {
        let maximum = if path == "/api/sms/forwarding/queue-action" {
            16 * 1024
        } else {
            8 * 1024 * 1024
        };
        return Some((maximum, false));
    }

    match path {
        "/api/at" => Some((1024 * 1024, false)),
        "/api/call/audio/start"
        | "/api/call/audio/stop"
        | "/api/call-mode/enable"
        | "/api/call-mode/restore" => Some((
            4 * 1024,
            path.ends_with("/stop") || path.ends_with("/restore"),
        )),
        "/api/internal/call-mode/adb/voice-route/stop"
        | "/api/internal/call-mode/adb/voice-route/status" => Some((4 * 1024, true)),
        "/api/internal/call-mode/adb/probe"
        | "/api/internal/call-mode/adb/deploy-runtime"
        | "/api/internal/call-mode/adb/voice-route/start"
        | "/api/internal/call-mode/adb/voice-runtime/status"
        | "/api/internal/call-mode/adb/voice-runtime/generation" => Some((4 * 1024, false)),
        "/api/internal/call-mode/lifecycle/invalidate"
        | "/api/internal/call-mode/lifecycle/shutdown" => Some((1024, true)),
        "/api/internal/call-mode/lifecycle/readiness" => Some((1024, false)),
        "/api/internal/call-mode/backups/create" => Some((64 * 1024, false)),
        "/api/internal/call-mode/backups/restore-data" => Some((1024, false)),
        "/api/internal/call-mode/status/project" => Some((8 * 1024, false)),
        "/api/call-mode/download" => Some((1024 * 1024, false)),
        "/api/call-mode/backups/delete" => Some((1024 * 1024, false)),
        "/api/internal/call-mode/backups/validate-restore" => Some((1024, false)),
        "/api/call-mode/retry" => Some((1024 * 1024, false)),
        "/api/call-mode/backups/import" => Some((64 * 1024, false)),
        "/api/internal/multistandby/inventory" => Some((2 * 1024, false)),
        "/api/internal/multistandby/runtime/retry-schedule" => Some((4 * 1024, false)),
        "/api/internal/multistandby/runtime/registration-schedule" => Some((4 * 1024, false)),
        "/api/internal/runtime/status-snapshot" => Some((4 * 1024, false)),
        "/api/internal/esim/health-snapshot" => Some((4 * 1024, false)),
        "/api/esim/notes" | "/api/esim/switch" | "/api/esim/profile" | "/api/esim/download" => {
            Some((1024 * 1024, false))
        }
        "/api/esim/download/confirm" | "/api/esim/operation/acknowledge" => Some((8 * 1024, false)),
        "/api/esim/module-notes" => Some((64 * 1024, false)),
        "/api/network/services-order" => Some((1024 * 1024, false)),
        "/api/voice/enable" => Some((1024 * 1024, false)),
        "/api/module/flight-mode" => Some((4 * 1024, false)),
        "/api/network/recovery" | "/api/network/reboot-module" => Some((4 * 1024, false)),
        "/api/network/usbnet" => Some((1024 * 1024, false)),
        "/api/multistandby/stop" => Some((1024 * 1024, true)),
        "/api/multistandby/messages/send" => Some((1024 * 1024, false)),
        "/api/multistandby/start"
        | "/api/multistandby/retry"
        | "/api/multistandby/control-intents"
        | "/api/multistandby/control-intents/ack" => Some((1024 * 1024, false)),
        "/api/multistandby/send-intents" | "/api/multistandby/send-intents/submit" => {
            Some((1024 * 1024, false))
        }
        "/api/v2/sms/prepare-tpdu" => Some((256 * 1024, false)),
        "/api/v2/sms/decode-stored-snapshot" => Some((512 * 1024, false)),
        "/api/v2/sms/encode-rpdu"
        | "/api/v2/sms/decode-rpdu"
        | "/api/v2/sms/decode-incoming-tpdu"
        | "/api/v2/carrier/resolve"
        | "/api/v2/reader/identity"
        | "/api/v2/vowifi/proxy-probe"
        | "/api/v2/vowifi/session-policy"
        | "/api/v2/vowifi/native-session-plan" => Some((4 * 1024, false)),
        "/api/v2/vowifi/eap-exchange/release" => Some((4 * 1024, true)),
        "/api/v2/sms-archive/clear" | "/api/calls/clear" => Some((16 * 1024, false)),
        "/api/v2/call/dtmf" => Some((1024, false)),
        "/api/v2/call/negotiate-sdp" => Some((400 * 1024, false)),
        "/api/v2/call/build-local-sdp" => Some((8 * 1024, false)),
        "/api/v2/call/invite-policy" => Some((8 * 1024, false)),
        "/api/sms/send" | "/api/sms/send-intents" | "/api/sms/adopt" | "/api/sms/delete" => {
            Some((1024 * 1024, false))
        }
        "/api/v2/sms-export" => Some((4 * 1024, false)),
        "/api/sms/delete-sender" | "/api/sms/clear" => Some((16 * 1024, false)),
        "/api/call/dial" | "/api/call/answer" | "/api/call/dtmf" => Some((4 * 1024, false)),
        "/api/call/hangup" => Some((4 * 1024, true)),
        "/api/reader/auth"
        | "/api/reader/imei"
        | "/api/internal/reader-device-identity/resolve" => Some((8 * 1024, false)),
        "/api/reader/ble/scan" | "/api/reader/ble/connect" => Some((4 * 1024, false)),
        "/api/reader/ble/disconnect" | "/api/internal/reader-native-owner" => {
            Some((4 * 1024, true))
        }
        "/api/reader/connect" => Some((16 * 1024, false)),
        "/api/reader/disconnect" => Some((16 * 1024, true)),
        "/api/v2/ims/validate-register-response" => Some((128 * 1024, false)),
        "/api/v2/reader/authenticate" => Some((8 * 1024, false)),
        "/api/reader/esim/download"
        | "/api/reader/esim/switch"
        | "/api/reader/esim/notifications/retry" => Some((8 * 1024, false)),
        "/api/reader/esim/download/confirm" => Some((8 * 1024, false)),
        "/api/reader/esim/profile" => Some((8 * 1024, false)),
        "/api/v2/reader/eap-aka" | "/api/v2/vowifi/eap-identity" => Some((32 * 1024, false)),
        "/api/v2/vowifi/proxy-policy" => Some((16 * 1024, false)),
        "/api/v2/vowifi/select-transport" => Some((8 * 1024, false)),
        "/api/v2/sms/forwarding/handoff" => Some((8 * 1024 * 1024, false)),
        "/api/calls/delete"
        | "/api/v2/sms-archive/delete"
        | "/api/v2/sms-archive/delete-sender" => Some((16 * 1024, false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_live_collector_routes_never_undercharge_their_byte_ceiling() {
        // Independent collector audit, rather than deriving expectations from
        // body_limit itself. New routes still require the production-route
        // audit; this fixture deliberately makes no discovery claim.
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("request_body_budget_ingress_fixture.json")).unwrap();
        let routes = fixture["routes"].as_array().unwrap();
        assert!(!routes.is_empty());
        for route in routes {
            let method: Method = route["method"].as_str().unwrap().parse().unwrap();
            let path = route["path"].as_str().unwrap();
            let maximum = route["collector_maximum"].as_u64().unwrap() as usize;
            let (reserved, control) = body_limit(&method, path)
                .unwrap_or_else(|| panic!("unbudgeted collector: {route}"));
            assert!(reserved >= maximum, "undercharged collector: {route}");
            let budget = Budget::new();
            let request = Request::builder()
                .method(method)
                .uri(path)
                .body(())
                .unwrap();
            let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
            assert!(permit.num_permits() * UNIT_BYTES >= maximum, "{route}");
            assert_eq!(
                if control {
                    budget.control.available_permits()
                } else {
                    budget.normal.available_permits()
                },
                NORMAL_UNITS - permit.num_permits()
            );
            assert_eq!(body_limit(&Method::GET, path), None, "read: {route}");
            assert_eq!(body_limit(&Method::HEAD, path), None, "head: {route}");
        }
    }

    #[test]
    fn usb_sim_rpcs_share_the_actual_one_mib_collector_budget() {
        let budget = Budget::new();
        let mut permits = Vec::new();
        for index in 0..8 {
            let path = ["sim-identity", "sim-profile-check", "sim-aka"][index % 3];
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!("/api/internal/usb-at/{path}"))
                .body(())
                .unwrap();
            let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
            assert_eq!(permit.num_permits() * UNIT_BYTES, crate::usb_at::MAX_BYTES);
            permits.push(permit);
        }
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/internal/usb-at/sim-identity")
            .body(())
            .unwrap();
        assert_eq!(budget.normal.available_permits(), 0);
        assert!(try_reserve_in(&budget, &request).is_err());
        drop(permits.pop());
        let recovered = try_reserve_in(&budget, &request).unwrap().unwrap();
        assert_eq!(
            recovered.num_permits() * UNIT_BYTES,
            crate::usb_at::MAX_BYTES
        );
    }

    #[test]
    fn cleanup_routes_remain_admitted_when_only_the_normal_pool_is_exhausted() {
        let budget = Budget::new();
        let _normal = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        for path in [
            "/api/internal/call-mode/adb/voice-route/stop",
            "/api/internal/call-mode/adb/voice-route/status",
            "/api/internal/call-mode/lifecycle/invalidate",
            "/api/internal/call-mode/lifecycle/shutdown",
            "/api/internal/reader-native-owner",
            "/api/reader/disconnect",
            "/api/reader/ble/disconnect",
            "/api/call/hangup",
            "/api/multistandby/stop",
            "/api/v2/vowifi/eap-exchange/release",
        ] {
            assert!(body_limit(&Method::POST, path).unwrap().1, "{path}");
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(())
                .unwrap();
            let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
            drop(permit);
            assert_eq!(budget.control.available_permits(), CONTROL_UNITS);
        }
        for path in [
            "/api/internal/call-mode/lifecycle/readiness",
            "/api/internal/call-mode/adb/voice-route/start",
            "/api/reader/connect",
            "/api/reader/ble/connect",
            "/api/multistandby/start",
            "/api/call/dial",
        ] {
            assert!(!body_limit(&Method::POST, path).unwrap().1, "{path}");
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(())
                .unwrap();
            assert!(try_reserve_in(&budget, &request).is_err(), "{path}");
        }
    }

    struct PendingCleanupBody {
        polled: Arc<tokio::sync::Notify>,
        _pin: std::marker::PhantomPinned,
    }
    impl hyper::body::Body for PendingCleanupBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            self.as_ref().get_ref().polled.notify_one();
            std::task::Poll::Pending
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_real_cleanup_handler_retires_ingress_without_releasing_the_physical_owner() {
        let budget = Budget::new();
        let _normal = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        let _other_control = budget
            .reserve((CONTROL_UNITS - 1) * UNIT_BYTES, true)
            .unwrap();
        let owner = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
        owner.set_go_producer("ingress-test").unwrap();
        let claim = owner
            .acquire_go("ingress-test", "reader", "original", 1)
            .unwrap();
        let cleanup = crate::shutdown::CleanupGate::default();
        let cleanup_permit = cleanup.enter().await.unwrap();
        let polled = Arc::new(tokio::sync::Notify::new());
        let request = Request::builder()
            .method(Method::POST)
            .uri(crate::ordinary_vowifi_admission::PATH)
            .body(PendingCleanupBody {
                polled: polled.clone(),
                _pin: std::marker::PhantomPinned,
            })
            .unwrap();
        let body_permit = try_reserve_in(&budget, &request).unwrap().unwrap();
        let worker_owner = owner.clone();
        let task = tokio::spawn(async move {
            let _cleanup_permit = cleanup_permit;
            let _body_permit = body_permit;
            crate::ordinary_vowifi_admission::handle(request, worker_owner, true).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), polled.notified())
            .await
            .unwrap();
        assert_eq!(budget.control.available_permits(), 0);
        assert!(claim.is_current());
        let closing = cleanup.clone();
        let close = tokio::spawn(async move { closing.close().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        // The real handler keeps its original 5s/400 contract. No outer
        // timeout is used as the mechanism that releases either permit.
        let response = tokio::time::timeout(std::time::Duration::from_secs(8), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(budget.control.available_permits(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(2), close)
            .await
            .unwrap()
            .unwrap();
        assert!(cleanup.enter().await.is_none());
        assert!(
            claim.is_current(),
            "ingress timeout is not physical cleanup proof"
        );
        claim.release_confirmed().unwrap();
    }

    #[tokio::test]
    async fn real_confirmed_cleanup_uses_reserved_capacity_and_returns_the_slot() {
        let budget = Budget::new();
        let _normal = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        let _other_control = budget
            .reserve((CONTROL_UNITS - 1) * UNIT_BYTES, true)
            .unwrap();
        let owner = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
        owner.set_go_producer("ingress-test").unwrap();
        let claim = owner
            .acquire_go("ingress-test", "reader", "original", 1)
            .unwrap();
        let request = Request::builder().method(Method::POST)
            .uri(crate::ordinary_vowifi_admission::PATH)
            .body(Full::new(Bytes::from_static(br#"{"action":"release","producer_id":"ingress-test","source":"reader","token":"original","sequence":1,"finished":true}"#)))
            .unwrap();
        let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
        let response = crate::ordinary_vowifi_admission::handle(request, owner.clone(), true).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!claim.is_current());
        assert_eq!(budget.control.available_permits(), 0);
        drop(permit);
        assert_eq!(budget.control.available_permits(), 1);
        assert!(budget.reserve(4 * 1024, true).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sixty_four_slow_http_bodies_retire_and_return_all_reserved_ingress_capacity() {
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Composition fixture: the production HTTP parser, body budget,
        // cleanup gate and ordinary handler, at main's public connection cap.
        // This does not start the daemon's device-discovery/accept loop.
        let budget = Arc::new(Budget::new());
        let _normal = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        let _other_control = budget
            .reserve(
                (CONTROL_UNITS - crate::PUBLIC_CONNECTION_LIMIT) * UNIT_BYTES,
                true,
            )
            .unwrap();
        let owner = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
        owner.set_go_producer("ingress-many").unwrap();
        let claim = owner
            .acquire_go("ingress-many", "reader", "original", 1)
            .unwrap();
        let cleanup = crate::shutdown::CleanupGate::default();
        let entered = Arc::new(AtomicUsize::new(0));
        let changed = Arc::new(tokio::sync::Notify::new());
        let mut tasks = tokio::task::JoinSet::new();
        let mut clients = Vec::new();
        for _ in 0..crate::PUBLIC_CONNECTION_LIMIT {
            let (mut client, server) = tokio::io::duplex(4096);
            let budget = budget.clone();
            let owner = owner.clone();
            let cleanup = cleanup.clone();
            let entered = entered.clone();
            let changed = changed.clone();
            tasks.spawn(async move {
                crate::http_server::builder()
                    .serve_connection(
                        TokioIo::new(server),
                        service_fn(move |request| {
                            let budget = budget.clone();
                            let owner = owner.clone();
                            let cleanup = cleanup.clone();
                            let entered = entered.clone();
                            let changed = changed.clone();
                            async move {
                                let _cleanup_permit = cleanup.enter().await.unwrap();
                                let _body_permit =
                                    try_reserve_in(&budget, &request).unwrap().unwrap();
                                entered.fetch_add(1, Ordering::SeqCst);
                                changed.notify_one();
                                Ok::<_, std::convert::Infallible>(
                                    crate::ordinary_vowifi_admission::handle(request, owner, true)
                                        .await,
                                )
                            }
                        }),
                    )
                    .await
                    .unwrap();
            });
            client.write_all(b"POST /api/internal/ordinary-vowifi/admission HTTP/1.1\r\nHost: local\r\nContent-Length: 4096\r\n\r\n{").await.unwrap();
            clients.push(client);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let wait = changed.notified();
                tokio::pin!(wait);
                wait.as_mut().enable();
                if entered.load(Ordering::SeqCst) == crate::PUBLIC_CONNECTION_LIMIT {
                    break;
                }
                wait.await;
            }
        })
        .await
        .unwrap();
        assert_eq!(tasks.len(), crate::PUBLIC_CONNECTION_LIMIT);
        assert_eq!(budget.control.available_permits(), 0);
        assert!(budget.reserve(4096, true).is_err());
        let closing = cleanup.clone();
        let close = tokio::spawn(async move { closing.close().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        tokio::time::timeout(Duration::from_secs(8), async {
            for mut client in clients {
                let mut response = Vec::new();
                client.read_to_end(&mut response).await.unwrap();
                assert!(response.starts_with(b"HTTP/1.1 400"));
                assert!(String::from_utf8_lossy(&response).contains("invalid_request"));
            }
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
            close.await.unwrap();
        })
        .await
        .unwrap();
        assert!(tasks.is_empty());
        assert_eq!(
            budget.control.available_permits(),
            crate::PUBLIC_CONNECTION_LIMIT
        );
        assert!(cleanup.enter().await.is_none());
        assert!(claim.is_current());

        // A fresh, fully framed status request can use a returned body slot;
        // it observes the existing owner without claiming physical retirement.
        let request = Request::builder().method(Method::POST)
            .uri(crate::ordinary_vowifi_admission::PATH)
            .body(Full::new(Bytes::from_static(br#"{"action":"status","producer_id":"ingress-many","source":"reader","token":"original","sequence":1}"#)))
            .unwrap();
        let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
        let response = crate::ordinary_vowifi_admission::handle(request, owner.clone(), true).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(claim.is_current());
        drop(permit);
        assert_eq!(
            budget.control.available_permits(),
            crate::PUBLIC_CONNECTION_LIMIT
        );
        claim.release_confirmed().unwrap();
    }

    #[test]
    fn internal_collectors_charge_actual_maxima_and_keep_modem_close_capacity() {
        for (path, capacity, maximum) in [
            ("/api/internal/profile-notes/remember-phone", 8, 1024 * 1024),
            ("/api/internal/modem/project-call-state", 64, 68 * 1024),
        ] {
            let budget = Budget::new();
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(())
                .unwrap();
            assert_eq!(body_limit(request.method(), path), Some((maximum, false)));
            let mut permits = Vec::new();
            for _ in 0..capacity {
                permits.push(try_reserve_in(&budget, &request).unwrap().unwrap());
            }
            assert_eq!(budget.normal.available_permits(), 0);
            assert!(try_reserve_in(&budget, &request).is_err());
            let close = Request::builder()
                .method(Method::POST)
                .uri("/api/internal/modem/close")
                .body(())
                .unwrap();
            let release = try_reserve_in(&budget, &close).unwrap().unwrap();
            assert_eq!(budget.control.available_permits(), CONTROL_UNITS - 1);
            drop(release);
            drop(permits.pop());
            assert!(try_reserve_in(&budget, &request).is_ok());
        }
    }

    #[test]
    fn vowifi_cleanup_and_reconciliation_remain_admitted_under_body_pressure() {
        let budget = Budget::new();
        let _full = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        for path in [
            "/api/vowifi",
            "/api/reader/vowifi",
            "/api/vowifi/reconcile",
            "/api/reader/vowifi/reconcile",
            "/api/reader/vowifi/stop-control",
            "/api/vowifi/stop-control",
            "/api/vowifi/calls/hangup",
            "/api/reader/vowifi/calls/reject",
        ] {
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(())
                .unwrap();
            assert_eq!(body_limit(request.method(), path), Some((4096, true)));
            let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
            assert_eq!(budget.control.available_permits(), CONTROL_UNITS - 1);
            drop(permit);
            assert_eq!(budget.control.available_permits(), CONTROL_UNITS);
        }
        for path in [
            "/api/vowifi/calls/dial",
            "/api/reader/vowifi/calls/answer",
            "/api/reader/vowifi/calls/dtmf",
            "/api/reader/vowifi/ussd",
            "/api/reader/vowifi/proxy-check",
        ] {
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(())
                .unwrap();
            assert_eq!(body_limit(request.method(), path), Some((4096, false)));
            assert!(try_reserve_in(&budget, &request).is_err());
        }
        assert_eq!(body_limit(&Method::GET, "/api/vowifi"), None);
        assert_eq!(
            body_limit(&Method::GET, "/api/reader/vowifi/calls/status"),
            None
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/reader/vowifi/calls/unsupported"),
            None
        );
    }

    #[test]
    fn radio_mutation_receipts_reserve_bounded_bodies_and_audio_cleanup_capacity() {
        assert_eq!(
            body_limit(&Method::POST, "/api/at"),
            Some((1024 * 1024, false))
        );
        for path in ["/api/call/audio/start", "/api/call-mode/enable"] {
            assert_eq!(body_limit(&Method::POST, path), Some((4096, false)));
        }
        for path in ["/api/call/audio/stop", "/api/call-mode/restore"] {
            assert_eq!(body_limit(&Method::POST, path), Some((4096, true)));
        }
    }
    #[test]
    fn weighted_admission_caps_normal_body_collectors_at_eight_mib() {
        let budget = Budget::new();
        let maximum = NORMAL_UNITS * UNIT_BYTES;
        let full = budget.reserve(maximum, false).unwrap();
        assert!(budget.reserve(UNIT_BYTES, false).is_err());
        drop(full);
        assert!(budget.reserve(maximum, false).is_ok());
    }

    #[test]
    fn reserved_control_capacity_survives_normal_body_saturation() {
        let budget = Budget::new();
        let full = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        let release = budget.reserve(8 * 1024 * 1024, true).unwrap();
        assert!(budget.reserve(UNIT_BYTES, true).is_err());
        drop(release);
        drop(full);
        assert!(budget.reserve(UNIT_BYTES, false).is_ok());
    }

    #[test]
    fn ordinary_session_cleanup_remains_admitted_when_normal_bodies_are_saturated() {
        let budget = Budget::new();
        let _normal = budget.reserve(NORMAL_UNITS * UNIT_BYTES, false).unwrap();
        let request = Request::builder()
            .method(Method::POST)
            .uri(crate::ordinary_vowifi_admission::PATH)
            .body(())
            .unwrap();
        assert_eq!(
            body_limit(request.method(), request.uri().path()),
            Some((4 * 1024, true))
        );
        let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
        assert_eq!(budget.control.available_permits(), CONTROL_UNITS - 1);
        drop(permit);
        assert_eq!(budget.control.available_permits(), CONTROL_UNITS);
    }

    #[test]
    fn support_snapshot_never_reserves_a_request_body_collector() {
        for method in [Method::GET, Method::HEAD, Method::POST] {
            assert_eq!(body_limit(&method, "/api/diagnostics/support"), None);
        }
    }

    #[test]
    fn routes_reserve_their_existing_maximum_and_reads_do_not() {
        assert_eq!(
            body_limit(&Method::POST, crate::ims_sms_transactions::INTERNAL_PATH),
            Some((128 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/runtime/status-snapshot"),
            Some((4 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/esim/health-snapshot"),
            Some((4 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/call-mode/backups/delete"),
            Some((1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/network/usbnet"),
            Some((1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/multistandby/stop"),
            Some((1024 * 1024, true))
        );
        for path in [
            "/api/multistandby/start",
            "/api/multistandby/retry",
            "/api/multistandby/control-intents",
            "/api/multistandby/control-intents/ack",
        ] {
            assert_eq!(
                body_limit(&Method::POST, path),
                Some((1024 * 1024, false)),
                "{path}"
            );
        }
        assert_eq!(
            body_limit(&Method::PUT, "/api/esim/module-notes"),
            Some((64 * 1024, false))
        );
        assert_eq!(body_limit(&Method::GET, "/api/esim/module-notes"), None);
        for path in [
            "/api/internal/reader-pcsc/transmit",
            "/api/internal/reader-pcsc/status",
            "/api/internal/reader-pcsc/release",
            "/api/internal/reader-pcsc/close",
            "/api/internal/reader-ble-transport",
            "/api/internal/reader-service",
            "/api/internal/multistandby/reader-reservation/operation/release",
        ] {
            assert_eq!(body_limit(&Method::POST, path), None, "{path}");
        }
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/usb-at/device-status"),
            Some((1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/usb-at/sim-identity"),
            Some((crate::usb_at::MAX_BYTES, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/usb-at/sim-profile-check"),
            Some((crate::usb_at::MAX_BYTES, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/usb-at/sim-aka"),
            Some((crate::usb_at::MAX_BYTES, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/call-archive/ingest"),
            Some((16 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/diagnostics/events/append"),
            Some((4 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/modem/write"),
            Some((12 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/modem/discover-at-port"),
            Some((12 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/modem/execute"),
            Some((12 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/modem/submit-sms"),
            Some((400 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/qmi/set-ims"),
            Some((16 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/qmi/close"),
            Some((16 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/sms/forwarding/enqueue"),
            Some((8 * 1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/sms/forwarding/queue-action"),
            Some((16 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/sms/archive/ingest"),
            Some((8 * 1024 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/multistandby/store/outcome"),
            Some((1024 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/reader-business"),
            Some((4 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/reader-lifecycle"),
            Some((32 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/device-send/finish"),
            Some((128 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/sms/forwarding/device-snapshot"),
            Some((8 * 1024 * 1024, false))
        );
        assert_eq!(body_limit(&Method::POST, "/not-a-rust-collector"), None);
        assert_eq!(
            body_limit(&Method::POST, "/api/sms/delete"),
            Some((1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/sms/delete-sender"),
            Some((16 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/sms/clear"),
            Some((16 * 1024, false))
        );
        assert_eq!(body_limit(&Method::GET, "/api/sms"), None);
        assert_eq!(
            body_limit(&Method::PUT, "/api/network/services-order"),
            Some((1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/voice/enable"),
            Some((1024 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/module/flight-mode"),
            Some((4 * 1024, false))
        );
        assert_eq!(body_limit(&Method::GET, "/api/module/flight-mode"), None);
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/module-radio/action"),
            Some((32 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/module-radio/reservation"),
            Some((32 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/module-radio/vowifi"),
            Some((32 * 1024, true))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/sms/archive/delete-ids"),
            Some((128 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/internal/usb-sms/storage-operation"),
            Some((128 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/v2/sms/decode-incoming-tpdu"),
            Some((4 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/v2/sms/decode-stored-snapshot"),
            Some((512 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/reader/esim/download"),
            Some((8 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/reader/esim/switch"),
            Some((8 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/reader/esim/notifications/retry"),
            Some((8 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::POST, "/api/reader/esim/download/confirm"),
            Some((8 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::PATCH, "/api/reader/esim/profile"),
            Some((8 * 1024, false))
        );
        assert_eq!(
            body_limit(&Method::DELETE, "/api/reader/esim/profile"),
            Some((8 * 1024, false))
        );
    }

    #[test]
    fn declared_lengths_reserve_only_the_framed_body_size() {
        let budget = Budget::new();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/sms/forwarding/enqueue")
            .header(CONTENT_LENGTH, "128")
            .body(())
            .unwrap();
        let permit = try_reserve_in(&budget, &request).unwrap().unwrap();
        assert_eq!(permit.num_permits(), 1);
        drop(permit);

        let capped = Request::builder()
            .method(Method::POST)
            .uri("/api/v2/sms/decode-rpdu")
            .header(CONTENT_LENGTH, "1048576")
            .body(())
            .unwrap();
        let permit = try_reserve_in(&budget, &capped).unwrap().unwrap();
        assert_eq!(permit.num_permits(), 1);
        drop(permit);

        let chunked = Request::builder()
            .method(Method::POST)
            .uri("/api/sms/forwarding/enqueue")
            .body(())
            .unwrap();
        let permit = try_reserve_in(&budget, &chunked).unwrap().unwrap();
        assert_eq!(permit.num_permits(), 128);
    }
}
