//! Session-scoped VoWiFi OPTIONS heartbeat fail-confirm decisions.
//!
//! Rust owns the transition from a primary probe miss through one delayed
//! confirmation probe. Go retains the protected SIP transport, timer, health
//! callbacks, and teardown execution.

use base64::Engine as _;
use bytes::Bytes;
use hyper::{Response, StatusCode};
use proletarius_sip::{Header, Headers, Message, MessageData, Method, Request as SipRequest, Uri};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::time::Instant as TokioInstant;

const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SESSION_CAPACITY: usize = 1024;
use proletarius_ims::options_heartbeat::{CONFIRM_DELAY, decide};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Healthy,
    Confirming,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingProbe {
    id: u64,
    confirmation: bool,
}

#[derive(Debug, Clone)]
struct Session {
    generation: u64,
    next_probe_id: u64,
    phase: Phase,
    pending: Option<PendingProbe>,
    retry_not_before: Option<Instant>,
    probe_interval: Option<Duration>,
    next_probe_at: Option<TokioInstant>,
    expires_at: Instant,
}

#[derive(Default)]
struct Store {
    next_generation: u64,
    sessions: HashMap<String, Session>,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

#[derive(Debug, Deserialize)]
struct Request {
    operation: String,
    session_id: String,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default)]
    probe_id: Option<u64>,
    #[serde(default)]
    status_code: Option<u16>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    local_host: Option<String>,
    #[serde(default)]
    local_port: Option<u16>,
    #[serde(default)]
    public_identity: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    security_verify: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    cseq: Option<u32>,
    #[serde(default)]
    interval_ms: Option<u64>,
}

pub(crate) fn handle(value: Value) -> Response<http_body_util::Full<Bytes>> {
    let Ok(request) = serde_json::from_value::<Request>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_heartbeat_request");
    };
    match request.operation.as_str() {
        "heartbeat_start" => start(request),
        "heartbeat_begin_probe" => begin_probe(request),
        "heartbeat_record_probe" => record_probe(request),
        "heartbeat_close" => close(request),
        "heartbeat_build_options" => build_options(request),
        _ => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_operation",
        ),
    }
}

pub(crate) async fn wait_for_probe(value: Value) -> Response<http_body_util::Full<Bytes>> {
    let Ok(request) = serde_json::from_value::<Request>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_heartbeat_request");
    };
    let Some(generation) = request.generation.filter(|value| *value > 0) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_generation",
        );
    };
    let Some(interval_ms) = request
        .interval_ms
        .filter(|value| (1..=86_400_000).contains(value))
    else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_interval",
        );
    };
    if !valid_session_id(&request.session_id) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_session",
        );
    }
    let session_id = request.session_id.trim().to_owned();
    let interval = Duration::from_millis(interval_ms);
    let deadline = {
        let Ok(mut state) = store().lock() else {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "heartbeat_supervisor_unavailable",
            );
        };
        let now = Instant::now();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
        };
        if session.generation != generation {
            return failure(StatusCode::CONFLICT, "stale_heartbeat_generation");
        }
        if session.expires_at <= now {
            state.sessions.remove(&session_id);
            return failure(StatusCode::REQUEST_TIMEOUT, "heartbeat_session_expired");
        }
        if session.phase == Phase::Failed {
            return failure(StatusCode::CONFLICT, "heartbeat_supervisor_failed");
        }
        if let Some(configured) = session.probe_interval {
            if configured != interval {
                return failure(StatusCode::CONFLICT, "heartbeat_interval_changed");
            }
        } else {
            session.probe_interval = Some(interval);
            session.next_probe_at = TokioInstant::now().checked_add(interval);
            if session.next_probe_at.is_none() {
                return failure(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_heartbeat_interval",
                );
            }
        }
        session
            .next_probe_at
            .expect("interval initializes a deadline")
    };

    tokio::time::sleep_until(deadline).await;

    let Ok(mut state) = store().lock() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_unavailable",
        );
    };
    let now = Instant::now();
    let Some(session) = state.sessions.get_mut(&session_id) else {
        return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
    };
    if session.generation != generation {
        return failure(StatusCode::CONFLICT, "stale_heartbeat_generation");
    }
    if session.expires_at <= now {
        state.sessions.remove(&session_id);
        return failure(StatusCode::REQUEST_TIMEOUT, "heartbeat_session_expired");
    }
    if session.phase == Phase::Failed {
        return failure(StatusCode::CONFLICT, "heartbeat_supervisor_failed");
    }
    if session.next_probe_at != Some(deadline) {
        return failure(StatusCode::CONFLICT, "heartbeat_probe_schedule_changed");
    }
    let interval = session.probe_interval.expect("scheduled interval exists");
    let next = deadline
        .checked_add(interval)
        .unwrap_or_else(TokioInstant::now);
    session.next_probe_at = Some(if next <= TokioInstant::now() {
        TokioInstant::now() + interval
    } else {
        next
    });
    session.expires_at = now + SESSION_TTL;
    response(
        StatusCode::OK,
        &json!({"status":100,"session_id":session_id,"generation":generation}),
    )
}

pub(crate) async fn wait_for_confirmation(value: Value) -> Response<http_body_util::Full<Bytes>> {
    let Ok(request) = serde_json::from_value::<Request>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_heartbeat_request");
    };
    let Some(generation) = request.generation.filter(|value| *value > 0) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_generation",
        );
    };
    if !valid_session_id(&request.session_id) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_session",
        );
    }
    let session_id = request.session_id.trim().to_owned();
    let deadline = {
        let Ok(mut state) = store().lock() else {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "heartbeat_supervisor_unavailable",
            );
        };
        let now = Instant::now();
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
        };
        if session.generation != generation {
            return failure(StatusCode::CONFLICT, "stale_heartbeat_generation");
        }
        if session.expires_at <= now {
            state.sessions.remove(&session_id);
            return failure(StatusCode::REQUEST_TIMEOUT, "heartbeat_session_expired");
        }
        if session.phase != Phase::Confirming {
            return failure(StatusCode::CONFLICT, "heartbeat_confirmation_not_pending");
        }
        let Some(deadline) = session.retry_not_before else {
            return failure(
                StatusCode::CONFLICT,
                "heartbeat_confirmation_deadline_missing",
            );
        };
        deadline
    };

    let remaining = deadline.saturating_duration_since(Instant::now());
    tokio::time::sleep(remaining).await;

    let Ok(mut state) = store().lock() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_unavailable",
        );
    };
    let now = Instant::now();
    let Some(session) = state.sessions.get_mut(&session_id) else {
        return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
    };
    if session.generation != generation {
        return failure(StatusCode::CONFLICT, "stale_heartbeat_generation");
    }
    if session.expires_at <= now {
        state.sessions.remove(&session_id);
        return failure(StatusCode::REQUEST_TIMEOUT, "heartbeat_session_expired");
    }
    if session.phase != Phase::Confirming || session.retry_not_before != Some(deadline) {
        return failure(
            StatusCode::CONFLICT,
            "heartbeat_confirmation_schedule_changed",
        );
    }
    session.expires_at = now + SESSION_TTL;
    response(
        StatusCode::OK,
        &json!({"status":100,"session_id":session_id,"generation":generation}),
    )
}

fn build_options(request: Request) -> Response<http_body_util::Full<Bytes>> {
    let (
        Some(target),
        Some(local_host),
        Some(local_port),
        Some(public_identity),
        Some(call_id),
        Some(cseq),
    ) = (
        request.target.as_deref(),
        request.local_host.as_deref(),
        request.local_port,
        request.public_identity.as_deref(),
        request.call_id.as_deref(),
        request.cseq,
    )
    else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_options_request");
    };
    let target = target.trim();
    let call_id = call_id.trim();
    let public_identity = public_identity.trim();
    let local_host = local_host.trim();
    if target.is_empty()
        || target.len() > 512
        || target.bytes().any(|b| b.is_ascii_control())
        || call_id.is_empty()
        || call_id.len() > 256
        || call_id.bytes().any(|b| b.is_ascii_control())
        || public_identity.is_empty()
        || public_identity.len() > 512
        || public_identity.bytes().any(|b| b.is_ascii_control())
        || local_port == 0
        || cseq == 0
    {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_options_request");
    }
    let local_ip = match local_host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip,
        Err(_) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_options_request"),
    };
    let via_host = match local_ip {
        std::net::IpAddr::V4(ip) => format!("{ip}:{local_port}"),
        std::net::IpAddr::V6(ip) => format!("[{ip}]:{local_port}"),
    };
    let uri = match Uri::parse(&format!("sip:{target}")) {
        Ok(uri) => uri,
        Err(_) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_options_request"),
    };
    let mut random = [0u8; 16];
    if getrandom::fill(&mut random).is_err() {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "entropy_unavailable");
    }
    let branch = format!("z9hG4bK{}", lower_hex(&random[..8]));
    let tag = lower_hex(&random[8..]);
    let mut headers = Headers::default();
    let fields = [
        ("Via", format!("SIP/2.0/TCP {via_host};branch={branch}")),
        ("From", format!("<{public_identity}>;tag={tag}")),
        ("To", format!("<sip:{target}>")),
        ("Call-ID", call_id.to_owned()),
        ("CSeq", format!("{cseq} OPTIONS")),
        ("Max-Forwards", "70".to_owned()),
        ("Route", format!("<sip:{target};lr>")),
    ];
    for (name, value) in fields {
        let Ok(header) = Header::new(name, value) else {
            return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_options_request");
        };
        headers.push(header);
    }
    for (name, value) in [
        (
            "P-Preferred-Identity",
            request.public_identity.as_deref().map(|v| format!("<{v}>")),
        ),
        (
            "User-Agent",
            request.user_agent.filter(|v| !v.trim().is_empty()),
        ),
        (
            "Security-Verify",
            request.security_verify.filter(|v| !v.trim().is_empty()),
        ),
    ] {
        if let Some(value) = value {
            let Ok(header) = Header::new(name, value) else {
                return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_options_request");
            };
            headers.push(header);
        }
    }
    let message = Message::Request(SipRequest {
        method: Method::Options,
        uri,
        data: MessageData::new("SIP/2.0", headers, Vec::new()),
    });
    let wire = message.encode();
    response(
        StatusCode::OK,
        &json!({"status":200,"session_id":request.session_id,"call_id":call_id,"cseq":cseq,"wire_base64":base64::engine::general_purpose::STANDARD.encode(wire)}),
    )
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn valid_session_id(session_id: &str) -> bool {
    let session_id = session_id.trim();
    !session_id.is_empty() && session_id.len() <= 256
}

fn start(request: Request) -> Response<http_body_util::Full<Bytes>> {
    if !valid_session_id(&request.session_id) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_session",
        );
    }
    let Ok(mut state) = store().lock() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_unavailable",
        );
    };
    let now = Instant::now();
    state.sessions.retain(|_, session| session.expires_at > now);
    let session_id = request.session_id.trim().to_owned();
    if !state.sessions.contains_key(&session_id) && state.sessions.len() >= SESSION_CAPACITY {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_capacity",
        );
    }
    let Some(generation) = state.next_generation.checked_add(1) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_generation_exhausted",
        );
    };
    state.next_generation = generation;
    state.sessions.insert(
        session_id.clone(),
        Session {
            generation,
            next_probe_id: 0,
            phase: Phase::Healthy,
            pending: None,
            retry_not_before: None,
            probe_interval: None,
            next_probe_at: None,
            expires_at: now + SESSION_TTL,
        },
    );
    response(
        StatusCode::OK,
        &json!({"status": 100, "session_id": session_id, "generation": generation}),
    )
}

fn begin_probe(request: Request) -> Response<http_body_util::Full<Bytes>> {
    let Some(generation) = request.generation.filter(|value| *value > 0) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_generation",
        );
    };
    if !valid_session_id(&request.session_id) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_session",
        );
    }
    let session_id = request.session_id.trim().to_owned();
    let Ok(mut state) = store().lock() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_unavailable",
        );
    };
    let now = Instant::now();
    let Some(current) = state.sessions.get(&session_id).cloned() else {
        return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
    };
    if current.generation != generation {
        return failure(StatusCode::CONFLICT, "stale_heartbeat_generation");
    }
    if current.expires_at <= now {
        state.sessions.remove(&session_id);
        return failure(StatusCode::REQUEST_TIMEOUT, "heartbeat_session_expired");
    }
    if current.phase == Phase::Failed {
        return failure(StatusCode::CONFLICT, "heartbeat_supervisor_failed");
    }
    if current.pending.is_some() {
        return failure(StatusCode::CONFLICT, "heartbeat_probe_already_pending");
    }
    if current.phase == Phase::Confirming
        && current
            .retry_not_before
            .is_some_and(|deadline| now < deadline)
    {
        return failure(StatusCode::CONFLICT, "heartbeat_confirmation_too_early");
    }
    let Some(probe_id) = current.next_probe_id.checked_add(1) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_probe_generation_exhausted",
        );
    };
    let confirmation = current.phase == Phase::Confirming;
    if let Some(session) = state.sessions.get_mut(&session_id) {
        session.next_probe_id = probe_id;
        session.pending = Some(PendingProbe {
            id: probe_id,
            confirmation,
        });
        session.expires_at = now + SESSION_TTL;
    }
    response(
        StatusCode::OK,
        &json!({
            "status": 100,
            "session_id": session_id,
            "generation": generation,
            "probe_id": probe_id,
            "stage": if confirmation { "confirmation" } else { "primary" }
        }),
    )
}

fn record_probe(request: Request) -> Response<http_body_util::Full<Bytes>> {
    let Some(generation) = request.generation.filter(|value| *value > 0) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_generation",
        );
    };
    let Some(probe_id) = request.probe_id.filter(|value| *value > 0) else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_heartbeat_probe");
    };
    let status_code = request.status_code.unwrap_or_default();
    if status_code > 699 || (status_code != 0 && status_code < 200) {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_heartbeat_status");
    }

    if !valid_session_id(&request.session_id) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_session",
        );
    }
    let session_id = request.session_id.trim().to_owned();
    let Ok(mut state) = store().lock() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_unavailable",
        );
    };
    let now = Instant::now();
    let Some(current) = state.sessions.get(&session_id).cloned() else {
        return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
    };
    if current.generation != generation {
        return failure(StatusCode::CONFLICT, "stale_heartbeat_generation");
    }
    if current.expires_at <= now {
        state.sessions.remove(&session_id);
        return failure(StatusCode::REQUEST_TIMEOUT, "heartbeat_session_expired");
    }
    let Some(pending) = current.pending else {
        return failure(StatusCode::CONFLICT, "heartbeat_probe_missing_or_consumed");
    };
    if pending.id != probe_id {
        return failure(StatusCode::CONFLICT, "stale_heartbeat_probe");
    }
    if pending.confirmation != (current.phase == Phase::Confirming) {
        return failure(StatusCode::CONFLICT, "heartbeat_probe_stage_mismatch");
    }

    let disposition = decide(pending.confirmation, status_code);
    let decision = disposition.name();
    let retry_after_ms = disposition.retry_after().as_millis() as u64;
    let Some(session) = state.sessions.get_mut(&session_id) else {
        return failure(StatusCode::CONFLICT, "heartbeat_session_missing_or_expired");
    };
    session.pending = None;
    session.expires_at = now + SESSION_TTL;
    match decision {
        "continue" => {
            session.phase = Phase::Healthy;
            session.retry_not_before = None;
        }
        "confirm_after_delay" => {
            session.phase = Phase::Confirming;
            session.retry_not_before = Some(now + CONFIRM_DELAY);
        }
        "recovered" => {
            session.phase = Phase::Healthy;
            session.retry_not_before = None;
        }
        "teardown" => {
            session.phase = Phase::Failed;
            session.retry_not_before = None;
        }
        _ => unreachable!("decision is exhaustive"),
    }
    response(
        StatusCode::OK,
        &json!({
            "status": 200,
            "session_id": session_id,
            "generation": generation,
            "probe_id": probe_id,
            "decision": decision,
            "retry_after_ms": retry_after_ms
        }),
    )
}

fn close(request: Request) -> Response<http_body_util::Full<Bytes>> {
    let Some(generation) = request.generation.filter(|value| *value > 0) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_generation",
        );
    };
    if !valid_session_id(&request.session_id) {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_heartbeat_session",
        );
    }
    let session_id = request.session_id.trim();
    let Ok(mut state) = store().lock() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "heartbeat_supervisor_unavailable",
        );
    };
    let closed = match state.sessions.get(session_id) {
        Some(session) if session.generation == generation => {
            state.sessions.remove(session_id);
            true
        }
        _ => false,
    };
    response(
        StatusCode::OK,
        &json!({"status": 200, "session_id": session_id, "generation": generation, "closed": closed}),
    )
}

fn failure(status: StatusCode, code: &str) -> Response<http_body_util::Full<Bytes>> {
    response(
        status,
        &json!({"error": "IMS heartbeat supervisor failed", "code": code, "retryable": false}),
    )
}

fn response<T: serde::Serialize>(
    status: StatusCode,
    value: &T,
) -> Response<http_body_util::Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(http_body_util::Full::new(Bytes::from(body)))
        .expect("static heartbeat response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

    fn session_id(label: &str) -> String {
        format!(
            "{label}-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn request(operation: &str, session_id: &str) -> Request {
        Request {
            operation: operation.to_owned(),
            session_id: session_id.to_owned(),
            generation: None,
            probe_id: None,
            status_code: None,
            target: None,
            local_host: None,
            local_port: None,
            public_identity: None,
            user_agent: None,
            security_verify: None,
            call_id: None,
            cseq: None,
            interval_ms: None,
        }
    }

    fn payload(response: Response<http_body_util::Full<Bytes>>) -> Value {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime builds");
        let bytes = runtime.block_on(async {
            http_body_util::BodyExt::collect(response.into_body())
                .await
                .expect("full body is available")
                .to_bytes()
        });
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn async_payload(response: Response<http_body_util::Full<Bytes>>) -> Value {
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("full body is available")
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn start_session(session_id: &str) -> u64 {
        let reply = start(request("heartbeat_start", session_id));
        assert_eq!(reply.status(), StatusCode::OK);
        payload(reply)["generation"].as_u64().unwrap()
    }

    #[tokio::test]
    async fn rust_schedule_releases_repeated_probe_ticks_for_the_active_generation() {
        let session = session_id("heartbeat-schedule");
        let started_session = start(request("heartbeat_start", &session));
        assert_eq!(started_session.status(), StatusCode::OK);
        let generation = async_payload(started_session).await["generation"]
            .as_u64()
            .unwrap();
        let started = TokioInstant::now();
        let mut input = request("heartbeat_wait_probe", &session);
        input.generation = Some(generation);
        input.interval_ms = Some(12);
        let reply = wait_for_probe(json!({"operation":input.operation,"session_id":input.session_id,"generation":input.generation,"interval_ms":input.interval_ms})).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let value = async_payload(reply).await;
        assert_eq!(value["generation"], generation);
        assert!(started.elapsed() >= Duration::from_millis(8));

        let mut input = request("heartbeat_wait_probe", &session);
        input.generation = Some(generation);
        input.interval_ms = Some(12);
        let reply = wait_for_probe(json!({"operation":input.operation,"session_id":input.session_id,"generation":input.generation,"interval_ms":input.interval_ms})).await;
        assert_eq!(reply.status(), StatusCode::OK);

        let mut changed = request("heartbeat_wait_probe", &session);
        changed.generation = Some(generation);
        changed.interval_ms = Some(13);
        let reply = wait_for_probe(json!({"operation":changed.operation,"session_id":changed.session_id,"generation":changed.generation,"interval_ms":changed.interval_ms})).await;
        assert_eq!(reply.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn cancelling_wait_does_not_consume_the_scheduled_probe_tick() {
        let session = session_id("heartbeat-schedule-cancel");
        let started_session = start(request("heartbeat_start", &session));
        let generation = async_payload(started_session).await["generation"]
            .as_u64()
            .unwrap();
        let wait = || {
            json!({
                "operation":"heartbeat_wait_probe",
                "session_id":session,
                "generation":generation,
                "interval_ms":30
            })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(5), wait_for_probe(wait()))
                .await
                .is_err()
        );

        let resumed_at = TokioInstant::now();
        let reply = wait_for_probe(wait()).await;
        assert_eq!(reply.status(), StatusCode::OK);
        assert!(resumed_at.elapsed() >= Duration::from_millis(15));
    }

    #[tokio::test]
    async fn rust_confirmation_deadline_survives_wait_cancellation_and_fences_probe_stage() {
        let session = session_id("heartbeat-confirmation");
        let started = start(request("heartbeat_start", &session));
        let generation = async_payload(started).await["generation"].as_u64().unwrap();
        let primary = begin_probe({
            let mut input = request("heartbeat_begin_probe", &session);
            input.generation = Some(generation);
            input
        });
        let primary_id = async_payload(primary).await["probe_id"].as_u64().unwrap();
        let mut result = request("heartbeat_record_probe", &session);
        result.generation = Some(generation);
        result.probe_id = Some(primary_id);
        result.status_code = Some(0);
        assert_eq!(record_probe(result).status(), StatusCode::OK);
        {
            let mut state = store().lock().unwrap();
            let current = state.sessions.get_mut(&session).unwrap();
            current.retry_not_before = Some(Instant::now() + Duration::from_millis(30));
        }
        let wait = || {
            json!({
                "operation":"heartbeat_wait_confirmation",
                "session_id":session,
                "generation":generation
            })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(5), wait_for_confirmation(wait()))
                .await
                .is_err()
        );
        let resumed_at = TokioInstant::now();
        let ready = wait_for_confirmation(wait()).await;
        assert_eq!(ready.status(), StatusCode::OK);
        assert!(resumed_at.elapsed() >= Duration::from_millis(15));

        let mut confirmation = request("heartbeat_begin_probe", &session);
        confirmation.generation = Some(generation);
        let confirmation = begin_probe(confirmation);
        assert_eq!(confirmation.status(), StatusCode::OK);
        assert_eq!(async_payload(confirmation).await["stage"], "confirmation");
    }

    fn begin(session_id: &str, generation: u64) -> (u64, String) {
        let mut input = request("heartbeat_begin_probe", session_id);
        input.generation = Some(generation);
        let reply = begin_probe(input);
        assert_eq!(reply.status(), StatusCode::OK);
        let value = payload(reply);
        (
            value["probe_id"].as_u64().unwrap(),
            value["stage"].as_str().unwrap().to_owned(),
        )
    }

    fn result(
        session_id: &str,
        generation: u64,
        probe_id: u64,
        status_code: u16,
    ) -> (StatusCode, Value) {
        let mut input = request("heartbeat_record_probe", session_id);
        input.generation = Some(generation);
        input.probe_id = Some(probe_id);
        input.status_code = Some(status_code);
        let reply = record_probe(input);
        let status = reply.status();
        (status, payload(reply))
    }

    #[test]
    fn primary_success_keeps_session_healthy_and_starts_the_next_primary_probe() {
        let session = session_id("heartbeat-success");
        let generation = start_session(&session);
        let (probe, stage) = begin(&session, generation);
        assert_eq!(stage, "primary");
        let (status, value) = result(&session, generation, probe, 200);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["decision"], "continue");
        assert_eq!(begin(&session, generation).1, "primary");
    }

    #[test]
    fn one_miss_requests_delayed_confirmation_then_success_restores_healthy_phase() {
        let session = session_id("heartbeat-recovery");
        let generation = start_session(&session);
        let (primary, stage) = begin(&session, generation);
        assert_eq!(stage, "primary");
        let (status, value) = result(&session, generation, primary, 0);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["decision"], "confirm_after_delay");
        assert_eq!(value["retry_after_ms"], CONFIRM_DELAY.as_millis() as u64);

        let early = {
            let mut input = request("heartbeat_begin_probe", &session);
            input.generation = Some(generation);
            begin_probe(input)
        };
        assert_eq!(early.status(), StatusCode::CONFLICT);
        assert_eq!(payload(early)["code"], "heartbeat_confirmation_too_early");

        {
            let mut state = store().lock().unwrap();
            state.sessions.get_mut(&session).unwrap().retry_not_before =
                Some(Instant::now() - Duration::from_millis(1));
        }
        let (confirmation, stage) = begin(&session, generation);
        assert_eq!(stage, "confirmation");
        let (status, value) = result(&session, generation, confirmation, 200);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["decision"], "recovered");
        assert_eq!(begin(&session, generation).1, "primary");
    }

    #[test]
    fn non_2xx_sip_status_is_classified_as_a_miss_by_rust() {
        let session = session_id("heartbeat-status-rejected");
        let generation = start_session(&session);
        let (primary, _) = begin(&session, generation);
        let (_, missed) = result(&session, generation, primary, 403);
        assert_eq!(missed["decision"], "confirm_after_delay");

        {
            let mut state = store().lock().unwrap();
            state.sessions.get_mut(&session).unwrap().retry_not_before =
                Some(Instant::now() - Duration::from_millis(1));
        }
        let (confirmation, stage) = begin(&session, generation);
        assert_eq!(stage, "confirmation");
        let (_, rejected) = result(&session, generation, confirmation, 403);
        assert_eq!(rejected["decision"], "teardown");
    }

    #[test]
    fn second_miss_is_terminal_and_duplicate_or_stale_results_do_not_advance_state() {
        let session = session_id("heartbeat-terminal");
        let old_generation = start_session(&session);
        let (primary, _) = begin(&session, old_generation);
        let (status, value) = result(&session, old_generation, primary, 0);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["decision"], "confirm_after_delay");

        let replacement_generation = start_session(&session);
        assert!(replacement_generation > old_generation);
        let stale = result(&session, old_generation, primary, 0);
        assert_eq!(stale.0, StatusCode::CONFLICT);
        assert_eq!(stale.1["code"], "stale_heartbeat_generation");

        let (confirmation, stage) = begin(&session, replacement_generation);
        assert_eq!(stage, "primary");
        let (_, primary_failed) = result(&session, replacement_generation, confirmation, 500);
        assert_eq!(primary_failed["decision"], "confirm_after_delay");
        {
            let mut state = store().lock().unwrap();
            state.sessions.get_mut(&session).unwrap().retry_not_before =
                Some(Instant::now() - Duration::from_millis(1));
        }
        let (confirmation, stage) = begin(&session, replacement_generation);
        assert_eq!(stage, "confirmation");
        let (status, failed) = result(&session, replacement_generation, confirmation, 0);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(failed["decision"], "teardown");
        assert_eq!(
            result(&session, replacement_generation, confirmation, 0).0,
            StatusCode::CONFLICT
        );
        let mut another_probe = request("heartbeat_begin_probe", &session);
        another_probe.generation = Some(replacement_generation);
        let terminal = begin_probe(another_probe);
        assert_eq!(terminal.status(), StatusCode::CONFLICT);
        assert_eq!(payload(terminal)["code"], "heartbeat_supervisor_failed");
    }

    #[test]
    fn close_is_generation_scoped_and_sessions_are_isolated() {
        let left = session_id("heartbeat-left");
        let right = session_id("heartbeat-right");
        let left_generation = start_session(&left);
        let right_generation = start_session(&right);
        let (left_probe, _) = begin(&left, left_generation);
        let (right_probe, _) = begin(&right, right_generation);

        let mut stale_close = request("heartbeat_close", &left);
        stale_close.generation = Some(right_generation);
        assert_eq!(payload(close(stale_close))["closed"], false);
        let mut close_right = request("heartbeat_close", &right);
        close_right.generation = Some(right_generation);
        assert_eq!(payload(close(close_right))["closed"], true);
        assert_eq!(
            result(&left, left_generation, left_probe, 200).0,
            StatusCode::OK
        );
        assert_eq!(
            result(&right, right_generation, right_probe, 200).0,
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn invalid_probe_status_does_not_consume_pending_probe_and_expired_session_is_fenced() {
        let session = session_id("heartbeat-expired");
        let generation = start_session(&session);
        let (probe, _) = begin(&session, generation);
        let invalid = result(&session, generation, probe, 100);
        assert_eq!(invalid.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(invalid.1["code"], "invalid_heartbeat_status");

        let (status, value) = result(&session, generation, probe, 200);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["decision"], "continue");

        {
            let mut state = store().lock().unwrap();
            state.sessions.get_mut(&session).unwrap().expires_at =
                Instant::now() - Duration::from_millis(1);
        }
        let mut expired_begin = request("heartbeat_begin_probe", &session);
        expired_begin.generation = Some(generation);
        let expired = begin_probe(expired_begin);
        let status = expired.status();
        let value = payload(expired);
        // Another concurrent start may sweep the expired entry before this
        // request acquires the store lock. Both exact error pairs fence it.
        match status {
            StatusCode::REQUEST_TIMEOUT => assert_eq!(value["code"], "heartbeat_session_expired"),
            StatusCode::CONFLICT => {
                assert_eq!(value["code"], "heartbeat_session_missing_or_expired")
            }
            _ => panic!("expired heartbeat session was not fenced: {status}"),
        }
        assert!(start_session(&session) > generation);
    }

    #[test]
    fn options_request_builder_returns_framed_rust_sip_message() {
        let mut input = request("heartbeat_build_options", "heartbeat-build-test");
        input.target = Some("pcscf.example.test:5060".into());
        input.local_host = Some("2001:db8::1".into());
        input.local_port = Some(5060);
        input.public_identity = Some("sip:+447700900123@ims.example.test".into());
        input.user_agent = Some("Switch".into());
        input.security_verify = Some("ipsec-3gpp;alg=hmac-sha-1-96".into());
        input.call_id = Some("options-call-1".into());
        input.cseq = Some(7);
        let reply = build_options(input);
        assert_eq!(reply.status(), StatusCode::OK);
        let value = payload(reply);
        let wire = base64::engine::general_purpose::STANDARD
            .decode(value["wire_base64"].as_str().unwrap())
            .unwrap();
        let (parsed, consumed) = proletarius_sip::Parser::default()
            .parse(&wire, false)
            .unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(parsed.method().unwrap().as_str(), "OPTIONS");
        assert_eq!(proletarius_sip::call_id(&parsed), Some("options-call-1"));
        assert_eq!(proletarius_sip::cseq(&parsed), Some((7, "OPTIONS")));
        assert_eq!(
            proletarius_sip::header_value(&parsed, "User-Agent"),
            Some("Switch")
        );
        assert_eq!(
            proletarius_sip::header_value(&parsed, "Security-Verify"),
            Some("ipsec-3gpp;alg=hmac-sha-1-96")
        );
        assert!(
            proletarius_sip::header_value(&parsed, "Via")
                .unwrap()
                .contains("[2001:db8::1]:5060")
        );
        assert_eq!(parsed.body(), b"");
    }
}
