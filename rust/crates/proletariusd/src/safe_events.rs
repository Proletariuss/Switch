//! Rust-owned, bounded diagnostic timeline.
//!
//! Events are deliberately closed enums plus an opaque random correlation ID.
//! They are display-only telemetry, never an operation or message ledger.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

const CAPACITY: usize = 256;
const MAX_APPEND_BYTES: usize = 4096;
pub(crate) const INTERNAL_PREFIX: &str = "/api/internal/diagnostics/events/";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub(crate) sequence: u64,
    pub(crate) time_ms: i64,
    pub(crate) operation: String,
    pub(crate) correlation_id: String,
    pub(crate) generation: u64,
    pub(crate) stage: String,
    pub(crate) code: String,
    pub(crate) duration_ms: i64,
    pub(crate) result: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendEvent {
    time_ms: i64,
    operation: String,
    correlation_id: String,
    generation: u64,
    stage: String,
    code: String,
    duration_ms: i64,
    result: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Snapshot {
    schema_version: u8,
    capacity: usize,
    latest_sequence: u64,
    events: Vec<Event>,
}

#[derive(Default)]
struct State {
    next_sequence: u64,
    events: VecDeque<Event>,
}

pub(crate) struct Hub(Mutex<State>);

impl Hub {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(State::default()))
    }

    fn append(&self, event: AppendEvent) -> Result<u64, ()> {
        if !valid_event(&event) {
            return Err(());
        }
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = state.next_sequence.checked_add(1).ok_or(())?;
        state.next_sequence = sequence;
        if state.events.len() == CAPACITY {
            state.events.pop_front();
        }
        state.events.push_back(Event {
            sequence,
            time_ms: event.time_ms,
            operation: event.operation,
            correlation_id: event.correlation_id,
            generation: event.generation,
            stage: event.stage,
            code: event.code,
            duration_ms: event.duration_ms,
            result: event.result,
        });
        Ok(sequence)
    }

    fn snapshot(&self) -> Snapshot {
        let state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Snapshot {
            schema_version: 1,
            capacity: CAPACITY,
            latest_sequence: state.next_sequence,
            events: state.events.iter().cloned().collect(),
        }
    }
}

pub(crate) fn global() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(Hub::new)
}

pub(crate) struct NetworkDiagnosticSpan {
    correlation_id: String,
    started: Instant,
}

pub(crate) fn begin_network_diagnostic() -> Option<NetworkDiagnosticSpan> {
    let mut correlation_id = crate::multistandby_store::random_token().ok()?;
    correlation_id.truncate(32);
    let span = NetworkDiagnosticSpan {
        correlation_id,
        started: Instant::now(),
    };
    span.record("queued", "started", "pending");
    Some(span)
}

impl NetworkDiagnosticSpan {
    pub(crate) fn record(&self, stage: &'static str, code: &'static str, result: &'static str) {
        let duration_ms = self.started.elapsed().as_millis().min(i64::MAX as u128) as i64;
        let time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let _ = global().append(AppendEvent {
            time_ms,
            operation: "network_diagnostic".into(),
            correlation_id: self.correlation_id.clone(),
            generation: 0,
            stage: stage.into(),
            code: code.into(),
            duration_ms,
            result: result.into(),
        });
    }
}

/// One retained native N-wait attempt. Inputs are closed facts only: no
/// subscriber identity, profile ID, network address or error text is accepted.
pub(crate) struct NwaitSpan<'a> {
    hub: &'a Hub,
    correlation_id: String,
    started: Instant,
    generation: u64,
    ready: Option<bool>,
    finished: bool,
}
impl<'a> NwaitSpan<'a> {
    pub(crate) fn begin(hub: &'a Hub) -> Option<Self> {
        let mut correlation_id = crate::multistandby_store::random_token().ok()?;
        correlation_id.truncate(32);
        let span = Self {
            hub,
            correlation_id,
            started: Instant::now(),
            generation: 0,
            ready: None,
            finished: false,
        };
        span.record("prepare", "started", "pending");
        Some(span)
    }
    pub(crate) fn connected(&mut self, generation: u64) {
        debug_assert!(generation != 0 && self.generation == 0);
        self.generation = generation;
        self.record("connect", "started", "pending");
    }
    pub(crate) fn readiness(&mut self, ready: bool) {
        if self.generation == 0 || self.ready == Some(ready) {
            return;
        }
        self.ready = Some(ready);
        self.record(
            "ready",
            if ready { "ready" } else { "unavailable" },
            if ready { "succeeded" } else { "pending" },
        );
    }
    pub(crate) fn finish(&mut self, reason: &'static str, confirmed: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let (code, result) = if !confirmed {
            ("result_unknown", "unknown")
        } else {
            match reason {
                "planned_renewal" => ("planned_renewal", "succeeded"),
                "planned_ike_reauth" => ("planned_ike_reauth", "succeeded"),
                "manual_retry" => ("manual_retry", "cancelled"),
                "manual_stop" => ("cancelled", "cancelled"),
                _ => ("transport_failed", "failed"),
            }
        };
        self.record("retire", code, result);
        if confirmed {
            self.record("complete", code, result);
        }
    }
    fn record(&self, stage: &'static str, code: &'static str, result: &'static str) {
        let _ = self.hub.append(AppendEvent {
            time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64,
            operation: "nwait_session".into(),
            correlation_id: self.correlation_id.clone(),
            generation: self.generation,
            stage: stage.into(),
            code: code.into(),
            result: result.into(),
            duration_ms: self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,
        });
    }
}
impl Drop for NwaitSpan<'_> {
    fn drop(&mut self) {
        self.finish("backend_exit", false);
    }
}

pub(crate) fn handle_public(hub: &Hub) -> Response<Full<Bytes>> {
    crate::json_response(StatusCode::OK, &hub.snapshot())
}

pub(crate) async fn handle_internal<B>(request: Request<B>, hub: &Hub) -> Response<Full<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
{
    if request.method() != Method::POST
        || request.uri().path() != "/api/internal/diagnostics/events/append"
    {
        return crate::json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"code":"route_not_found","error":"route not found"}),
        );
    }
    let body = match crate::http_server::collect_body(
        std::pin::pin!(request.into_body()),
        MAX_APPEND_BYTES,
    )
    .await
    {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Ok(body) => body,
        Err(_) => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"code":"invalid_diagnostic_event","error":"invalid diagnostic event"}),
            );
        }
    };
    let event: AppendEvent = match serde_json::from_slice(&body) {
        Ok(event) if valid_event(&event) => event,
        _ => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"code":"invalid_diagnostic_event","error":"invalid diagnostic event"}),
            );
        }
    };
    match hub.append(event) {
        Ok(sequence) => crate::json_response(
            StatusCode::OK,
            &serde_json::json!({"accepted":true,"sequence":sequence}),
        ),
        Err(()) => crate::json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"code":"invalid_diagnostic_event","error":"invalid diagnostic event"}),
        ),
    }
}

fn valid_event(event: &AppendEvent) -> bool {
    event.time_ms >= 0
        && event.duration_ms >= 0
        && event.correlation_id.len() == 32
        && event
            .correlation_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && matches!(
            event.operation.as_str(),
            "ims_register"
                | "nwait_session"
                | "network_diagnostic"
                | "reader_auth"
                | "sms_send"
                | "sms_forward"
        )
        && matches!(
            event.stage.as_str(),
            "queued"
                | "waiting_lock"
                | "prepare"
                | "connect"
                | "authenticate"
                | "ready"
                | "submit"
                | "retire"
                | "complete"
        )
        && matches!(
            event.code.as_str(),
            "started"
                | "ready"
                | "planned_renewal"
                | "planned_ike_reauth"
                | "manual_retry"
                | "success"
                | "cancelled"
                | "busy"
                | "timeout"
                | "transport_failed"
                | "authentication_failed"
                | "registration_rejected"
                | "binding_revoked"
                | "binding_unconfirmed"
                | "binding_ambiguous"
                | "submission_failed"
                | "result_unknown"
                | "stopped"
                | "unavailable"
                | "invalid_response"
        )
        && matches!(
            event.result.as_str(),
            "pending" | "succeeded" | "failed" | "cancelled" | "unknown"
        )
}

#[cfg(test)]
mod tests {
    use super::{AppendEvent, CAPACITY, Hub, NwaitSpan, handle_internal, handle_public};
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Method, Request, StatusCode};

    fn event(sequence: u64) -> AppendEvent {
        AppendEvent {
            time_ms: 1_760_000_000_000 + sequence as i64,
            operation: "network_diagnostic".into(),
            correlation_id: format!("{sequence:032x}"),
            generation: 0,
            stage: "queued".into(),
            code: "started".into(),
            duration_ms: sequence as i64,
            result: "pending".into(),
        }
    }

    fn append_request(body: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::POST)
            .uri("/api/internal/diagnostics/events/append")
            .body(Full::new(body.into()))
            .unwrap()
    }

    #[test]
    fn native_nwait_span_records_transitions_and_separates_attempts() {
        let hub = Hub::new();
        let mut first = NwaitSpan::begin(&hub).unwrap();
        first.readiness(true); // No session exists yet.
        first.connected(17);
        for ready in [false, false, true, true, false, false, true] {
            first.readiness(ready);
        }
        first.finish("manual_retry", true);
        first.finish("manual_stop", true);
        drop(first);
        let mut second = NwaitSpan::begin(&hub).unwrap();
        second.connected(18);
        second.readiness(true);
        second.finish("planned_renewal", true);
        drop(second);
        let events = hub.snapshot().events;
        let first = &events[..8];
        assert_eq!(
            first
                .iter()
                .map(|e| (e.stage.as_str(), e.code.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("prepare", "started"),
                ("connect", "started"),
                ("ready", "unavailable"),
                ("ready", "ready"),
                ("ready", "unavailable"),
                ("ready", "ready"),
                ("retire", "manual_retry"),
                ("complete", "manual_retry")
            ]
        );
        assert_eq!(first[0].generation, 0);
        assert!(first[1..].iter().all(|e| e.generation == 17));
        assert!(
            first
                .iter()
                .all(|e| e.correlation_id == first[0].correlation_id)
        );
        assert_ne!(events[8].correlation_id, first[0].correlation_id);
        assert!(events[9..].iter().all(|e| e.generation == 18));
        assert_eq!(events.last().unwrap().result, "succeeded");
    }

    #[test]
    fn native_nwait_retirement_preserves_frozen_go_classifications() {
        for (reason, code, result) in [
            ("planned_renewal", "planned_renewal", "succeeded"),
            ("planned_ike_reauth", "planned_ike_reauth", "succeeded"),
            ("manual_retry", "manual_retry", "cancelled"),
            ("manual_stop", "cancelled", "cancelled"),
            ("tunnel_lost", "transport_failed", "failed"),
        ] {
            let hub = Hub::new();
            let mut span = NwaitSpan::begin(&hub).unwrap();
            span.connected(7);
            span.finish(reason, true);
            let events = hub.snapshot().events;
            let retired = events.iter().find(|e| e.stage == "retire").unwrap();
            assert_eq!(
                (
                    retired.code.as_str(),
                    retired.result.as_str(),
                    retired.generation
                ),
                (code, result, 7)
            );
        }
    }

    #[test]
    fn native_nwait_unconfirmed_exit_never_records_complete() {
        let hub = Hub::new();
        let mut span = NwaitSpan::begin(&hub).unwrap();
        span.connected(4);
        drop(span);
        let events = hub.snapshot().events;
        assert_eq!(events.len(), 3);
        assert_eq!(events.last().unwrap().stage, "retire");
        assert_eq!(events.last().unwrap().code, "result_unknown");
        assert_eq!(events.last().unwrap().result, "unknown");
        assert!(!events.iter().any(|e| e.stage == "complete"));
    }

    #[test]
    fn bounded_snapshot_keeps_only_ordered_safe_tail() {
        let hub = Hub::new();
        for sequence in 1..=CAPACITY as u64 + 2 {
            hub.append(event(sequence)).unwrap();
        }
        let snapshot = hub.snapshot();
        assert_eq!(snapshot.capacity, CAPACITY);
        assert_eq!(snapshot.events.len(), CAPACITY);
        assert_eq!(snapshot.latest_sequence, CAPACITY as u64 + 2);
        assert_eq!(snapshot.events.first().unwrap().sequence, 3);
        assert_eq!(
            snapshot.events.last().unwrap().sequence,
            CAPACITY as u64 + 2
        );
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("raw_error"));
    }

    #[tokio::test]
    async fn internal_append_validates_closed_enums_and_body_budget() {
        let hub = Hub::new();
        let body = serde_json::to_vec(&serde_json::json!({
            "time_ms":1760000000000i64,
            "operation":"network_diagnostic",
            "correlation_id":"00000000000000000000000000000001",
            "generation":0,
            "stage":"queued",
            "code":"started",
            "duration_ms":0,
            "result":"pending"
        }))
        .unwrap();
        let response = handle_internal(append_request(body), &hub).await;
        assert_eq!(response.status(), StatusCode::OK);
        let response: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(response["sequence"], 1);

        let unknown = append_request(Bytes::from_static(
            br#"{"time_ms":1,"operation":"network_diagnostic","correlation_id":"00000000000000000000000000000002","generation":0,"stage":"queued","code":"started","duration_ms":0,"result":"pending","error":"private"}"#,
        ));
        assert_eq!(
            handle_internal(unknown, &hub).await.status(),
            StatusCode::BAD_REQUEST
        );
        let oversized = append_request(Bytes::from(vec![b' '; 4097]));
        assert_eq!(
            handle_internal(oversized, &hub).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(hub.snapshot().latest_sequence, 1);
    }

    // Exercise the production handler with !Unpin bodies, rather than a
    // duplicate collector. Both malformed streams must expire before append.
    struct AdversarialBody {
        ready_frames: bool,
        frames: std::sync::atomic::AtomicUsize,
        dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        _pinned: std::marker::PhantomPinned,
    }

    impl hyper::body::Body for AdversarialBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            if !self.ready_frames {
                return std::task::Poll::Pending;
            }
            let frame = if self
                .frames
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                % 2
                == 0
            {
                hyper::body::Frame::data(Bytes::new())
            } else {
                hyper::body::Frame::trailers(hyper::HeaderMap::new())
            };
            std::task::Poll::Ready(Some(Ok(frame)))
        }
    }

    impl Drop for AdversarialBody {
        fn drop(&mut self) {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn s07_append_pending_and_always_ready_bodies_expire_without_dispatch_and_recover() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::Duration;
        let hub = Hub::new();
        let dropped = Arc::new(AtomicUsize::new(0));
        let request = |ready_frames| {
            Request::builder()
                .method(Method::POST)
                .uri("/api/internal/diagnostics/events/append")
                .body(AdversarialBody {
                    ready_frames,
                    frames: AtomicUsize::new(0),
                    dropped: dropped.clone(),
                    _pinned: std::marker::PhantomPinned,
                })
                .unwrap()
        };
        let heartbeat = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert_eq!(hub.snapshot().latest_sequence, 0);
            assert_eq!(
                dropped.load(Ordering::SeqCst),
                0,
                "ready frames starved the runtime until the ingress deadline"
            );
        };
        let (pending, ready, ()) = tokio::join!(
            handle_internal(request(false), &hub),
            handle_internal(request(true), &hub),
            heartbeat,
        );
        for response in [pending, ready] {
            assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["code"], "request_body_timeout");
        }
        assert_eq!(hub.snapshot().latest_sequence, 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert_eq!(
            handle_internal(append_request(Bytes::from(vec![b' '; 4097])), &hub)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(hub.snapshot().latest_sequence, 0);
        let valid = br#"{"time_ms":1,"operation":"network_diagnostic","correlation_id":"00000000000000000000000000000001","generation":0,"stage":"queued","code":"started","duration_ms":0,"result":"pending"}"#;
        assert_eq!(
            handle_internal(append_request(Bytes::copy_from_slice(valid)), &hub)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(hub.snapshot().latest_sequence, 1);
    }

    #[tokio::test]
    async fn internal_append_rejects_wrong_method_and_unknown_path() {
        let hub = Hub::new();
        let wrong_method = Request::builder()
            .method(Method::GET)
            .uri("/api/internal/diagnostics/events/append")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            handle_internal(wrong_method, &hub).await.status(),
            StatusCode::NOT_FOUND
        );
        let unknown_path = Request::builder()
            .method(Method::POST)
            .uri("/api/internal/diagnostics/events/other")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            handle_internal(unknown_path, &hub).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(hub.snapshot().latest_sequence, 0);
    }

    #[test]
    fn invalid_enum_and_identifier_are_not_recorded() {
        let hub = Hub::new();
        let mut invalid = event(1);
        invalid.code = "secret phone number".into();
        assert!(hub.append(invalid).is_err());
        let mut invalid = event(2);
        invalid.correlation_id = "8901000000000000000".into();
        assert!(hub.append(invalid).is_err());
        assert_eq!(hub.snapshot().latest_sequence, 0);
        let response = handle_public(&hub);
        assert_eq!(response.status(), StatusCode::OK);
    }
}
