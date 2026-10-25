//! Native validation for the small, non-real-time RFC 4733 control surface.
//! RTP sequencing and writing stay with the call runtime so this route cannot
//! add IPC jitter to voice media.

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static NEXT_LOCAL_CALL_CSEQ: AtomicU32 = AtomicU32::new(1);

fn allocate_local_call_cseq() -> Result<u32, (StatusCode, &'static str)> {
    NEXT_LOCAL_CALL_CSEQ
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current != 0 && current <= i32::MAX as u32).then_some(current.saturating_add(1))
        })
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "call_policy_cseq_exhausted",
            )
        })
}

#[derive(Deserialize)]
struct DtmfRequest {
    digit: String,
    #[serde(default = "default_dtmf_duration_scale")]
    duration_scale: u8,
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
}

fn default_dtmf_duration_scale() -> u8 {
    2
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioSdpRequest {
    sdp: String,
    answer: bool,
    #[serde(default)]
    evs_only: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalAudioSdpRequest {
    session_id: u64,
    local_ip: String,
    local_port: u16,
    incoming: bool,
    codec: Option<String>,
    payload_type: Option<u8>,
    telephone_event_payload: Option<u8>,
    evs_header_full: bool,
    amr_wb_mode: u8,
    amr_octet_aligned: bool,
    evs_only: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitePolicyRequest {
    action: String,
    call_id: String,
    #[serde(default)]
    cseq: u32,
    #[serde(default)]
    status: u16,
    #[serde(default)]
    has_sdp: bool,
    #[serde(default)]
    reliable_rseq: Option<u32>,
    #[serde(default = "default_incoming_available")]
    available: bool,
    #[serde(default)]
    method: String,
    #[serde(default)]
    sms_only: bool,
    #[serde(default)]
    has_call_handler: bool,
    #[serde(default)]
    default_allow: String,
    #[serde(default)]
    request_call_id: String,
    #[serde(default)]
    active_ussd_call_id: String,
}

fn default_incoming_available() -> bool {
    true
}

struct StoredOutgoingCall {
    tracker: proletarius_call::OutgoingCallTracker,
    touched: Instant,
}

static OUTGOING_CALLS: OnceLock<Mutex<HashMap<String, StoredOutgoingCall>>> = OnceLock::new();
struct StoredIncomingCall {
    tracker: proletarius_call::IncomingCallTracker,
    touched: Instant,
}

static INCOMING_CALLS: OnceLock<Mutex<HashMap<String, StoredIncomingCall>>> = OnceLock::new();
const MAX_OUTGOING_CALL_TRACKERS: usize = 64;
const OUTGOING_CALL_TRACKER_TTL: Duration = Duration::from_secs(180);
const MAX_INCOMING_CALL_TRACKERS: usize = 64;
const INCOMING_CALL_TRACKER_TTL: Duration = Duration::from_secs(180);

pub(crate) async fn handle_invite_policy(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        4096,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return invite_policy_failure(StatusCode::BAD_REQUEST, "invalid_call_policy_request");
        }
        Err(crate::http_server::BodyReadError::Timeout) => {
            return invite_policy_failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<InvitePolicyRequest>(&body) else {
        return invite_policy_failure(StatusCode::BAD_REQUEST, "invalid_call_policy_request");
    };
    match apply_invite_policy(input) {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err((status, code)) => invite_policy_failure(status, code),
    }
}

fn apply_invite_policy(
    input: InvitePolicyRequest,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    if input.action == "incoming_request" {
        return apply_incoming_request_policy(input);
    }
    if input.call_id.is_empty()
        || input.call_id.len() > 128
        || !input
            .call_id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.'))
    {
        return Err((StatusCode::BAD_REQUEST, "invalid_call_id"));
    }
    if input.action.starts_with("incoming_") {
        return apply_incoming_invite_policy(input);
    }
    let now = Instant::now();
    let calls = OUTGOING_CALLS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut calls) = calls.lock() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "call_policy_unavailable"));
    };
    calls.retain(|_, stored| now.duration_since(stored.touched) < OUTGOING_CALL_TRACKER_TTL);
    match input.action.as_str() {
        "begin" => {
            if input.cseq > i32::MAX as u32 {
                return Err((StatusCode::BAD_REQUEST, "invalid_invite_cseq"));
            }
            if calls.contains_key(&input.call_id) {
                return Err((StatusCode::CONFLICT, "call_policy_exists"));
            }
            if calls.len() >= MAX_OUTGOING_CALL_TRACKERS {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "call_policy_capacity"));
            }
            // Production asks Rust to allocate the initial dialog sequence by
            // sending zero. A supplied value is retained for deterministic
            // in-process compatibility fixtures; the Go production bridge
            // rejects that path.
            let invite_cseq = if input.cseq == 0 {
                allocate_local_call_cseq()?
            } else {
                input.cseq
            };
            let tracker = proletarius_call::OutgoingCallTracker::begin(invite_cseq)
                .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_invite_cseq"))?;
            calls.insert(
                input.call_id,
                StoredOutgoingCall {
                    tracker,
                    touched: now,
                },
            );
            Ok(serde_json::json!({"action":"begun","phase":"inviting","request_cseq":invite_cseq}))
        }
        "cancel" => {
            let stored = calls
                .get_mut(&input.call_id)
                .ok_or((StatusCode::NOT_FOUND, "call_policy_missing"))?;
            stored
                .tracker
                .cancel()
                .map_err(|_| (StatusCode::CONFLICT, "call_policy_invalid_transition"))?;
            stored.touched = now;
            Ok(serde_json::json!({"action":"cancelled","phase":"cancelling"}))
        }
        "hangup" => {
            if input.cseq == 0 {
                return Err((StatusCode::BAD_REQUEST, "invalid_invite_cseq"));
            }
            let stored = calls
                .get_mut(&input.call_id)
                .ok_or((StatusCode::NOT_FOUND, "call_policy_missing"))?;
            let decision = stored
                .tracker
                .hangup()
                .map_err(|_| (StatusCode::CONFLICT, "call_policy_cseq_exhausted"))?;
            let (action, request_cseq) = match decision {
                proletarius_call::OutgoingHangupAction::SendCancel { cseq } => {
                    ("hangup_cancel", cseq)
                }
                proletarius_call::OutgoingHangupAction::SendBye { cseq } => ("hangup_bye", cseq),
                proletarius_call::OutgoingHangupAction::Ignore => ("hangup_ignore", 0),
            };
            let phase = match stored.tracker.phase() {
                proletarius_call::CallPhase::Idle => "idle",
                proletarius_call::CallPhase::Inviting => "inviting",
                proletarius_call::CallPhase::Early => "early",
                proletarius_call::CallPhase::Cancelling => "cancelling",
                proletarius_call::CallPhase::Active => "active",
                proletarius_call::CallPhase::Terminating => "terminating",
                proletarius_call::CallPhase::Ended => "ended",
                proletarius_call::CallPhase::Failed => "failed",
            };
            stored.touched = now;
            Ok(serde_json::json!({"action":action,"phase":phase,"request_cseq":request_cseq}))
        }
        "response" => {
            let stored = calls
                .get_mut(&input.call_id)
                .ok_or((StatusCode::NOT_FOUND, "call_policy_missing"))?;
            let action = stored
                .tracker
                .accept_invite_response(
                    input.status,
                    input.cseq,
                    input.has_sdp,
                    input.reliable_rseq,
                )
                .map_err(|_| (StatusCode::CONFLICT, "call_policy_invalid_response"))?;
            let request_cseq = match action {
                proletarius_call::InviteAction::SendPrack { .. }
                | proletarius_call::InviteAction::AckAndBye => Some(
                    stored
                        .tracker
                        .next_local_cseq()
                        .map_err(|_| (StatusCode::CONFLICT, "call_policy_cseq_exhausted"))?,
                ),
                proletarius_call::InviteAction::AckAndFail { .. }
                    if (200..300).contains(&input.status) =>
                {
                    Some(
                        stored
                            .tracker
                            .next_local_cseq()
                            .map_err(|_| (StatusCode::CONFLICT, "call_policy_cseq_exhausted"))?,
                    )
                }
                _ => None,
            };
            stored.touched = now;
            let value = match action {
                proletarius_call::InviteAction::Wait => serde_json::json!({"action":"wait"}),
                proletarius_call::InviteAction::StartEarlyMedia => {
                    serde_json::json!({"action":"start_early_media"})
                }
                proletarius_call::InviteAction::SendPrack { start_early_media } => {
                    serde_json::json!({"action":"send_prack","start_early_media":start_early_media})
                }
                proletarius_call::InviteAction::AckAndStartMedia => {
                    serde_json::json!({"action":"ack_start_media"})
                }
                proletarius_call::InviteAction::AckAndBye => {
                    serde_json::json!({"action":"ack_bye"})
                }
                proletarius_call::InviteAction::AckAndEnd => {
                    serde_json::json!({"action":"ack_end"})
                }
                proletarius_call::InviteAction::AckAndFail { status } => {
                    serde_json::json!({"action":"ack_fail","status":status})
                }
                proletarius_call::InviteAction::AckOnly => {
                    serde_json::json!({"action":"ack_only"})
                }
            };
            let phase = match stored.tracker.phase() {
                proletarius_call::CallPhase::Idle => "idle",
                proletarius_call::CallPhase::Inviting => "inviting",
                proletarius_call::CallPhase::Early => "early",
                proletarius_call::CallPhase::Cancelling => "cancelling",
                proletarius_call::CallPhase::Active => "active",
                proletarius_call::CallPhase::Terminating => "terminating",
                proletarius_call::CallPhase::Ended => "ended",
                proletarius_call::CallPhase::Failed => "failed",
            };
            let mut value = value;
            value["phase"] = serde_json::json!(phase);
            value["request_cseq"] = serde_json::json!(request_cseq.unwrap_or(0));
            Ok(value)
        }
        "end" => {
            calls.remove(&input.call_id);
            Ok(serde_json::json!({"action":"ended","phase":"ended"}))
        }
        _ => Err((StatusCode::BAD_REQUEST, "invalid_call_policy_action")),
    }
}

fn apply_incoming_request_policy(
    input: InvitePolicyRequest,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    let method = input.method.to_ascii_uppercase();
    if method.is_empty()
        || method.len() > 32
        || !method
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err((StatusCode::BAD_REQUEST, "invalid_incoming_request_method"));
    }
    if input.default_allow.len() > 256
        || input
            .default_allow
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0))
    {
        return Err((StatusCode::BAD_REQUEST, "invalid_incoming_request_allow"));
    }
    for call_id in [&input.request_call_id, &input.active_ussd_call_id] {
        if call_id.len() > 512
            || call_id
                .bytes()
                .any(|byte| matches!(byte, b'\r' | b'\n' | 0))
        {
            return Err((StatusCode::BAD_REQUEST, "invalid_incoming_request_call_id"));
        }
    }
    if !input.request_call_id.is_empty() && input.request_call_id == input.active_ussd_call_id {
        return Ok(serde_json::json!({"action":"ussd_dispatch","status":0,"reason":"","allow":""}));
    }
    let decision = match method.as_str() {
        "MESSAGE" => serde_json::json!({"action":"message","status":200,"reason":"OK","allow":""}),
        "OPTIONS" => serde_json::json!({"action":"options","status":200,"reason":"OK","allow":""}),
        "ACK" if !input.sms_only && input.has_call_handler => {
            serde_json::json!({"action":"ack_dispatch","status":0,"reason":"","allow":""})
        }
        "ACK" => serde_json::json!({"action":"ack_ignore","status":0,"reason":"","allow":""}),
        "INVITE" | "PRACK" | "UPDATE" | "CANCEL" | "BYE" | "INFO" if input.sms_only => {
            if method == "INVITE" {
                serde_json::json!({"action":"reject_488","status":488,"reason":"Not Acceptable Here","allow":""})
            } else {
                serde_json::json!({"action":"reject_481","status":481,"reason":"Call/Transaction Does Not Exist","allow":""})
            }
        }
        "INVITE" | "PRACK" | "UPDATE" | "CANCEL" | "BYE" | "INFO" if input.has_call_handler => {
            serde_json::json!({"action":"call_dispatch","status":0,"reason":"","allow":""})
        }
        "INVITE" => {
            serde_json::json!({"action":"reject_480","status":480,"reason":"Temporarily Unavailable","allow":""})
        }
        "PRACK" | "UPDATE" | "CANCEL" | "BYE" | "INFO" => {
            serde_json::json!({"action":"reject_481","status":481,"reason":"Call/Transaction Does Not Exist","allow":""})
        }
        _ => serde_json::json!({
            "action":"reject_405","status":405,"reason":"Method Not Allowed",
            "allow": if input.sms_only { "MESSAGE,OPTIONS" } else { input.default_allow.as_str() }
        }),
    };
    if decision["action"] == "reject_405" && decision["allow"].as_str().is_none_or(str::is_empty) {
        return Err((StatusCode::BAD_REQUEST, "missing_incoming_request_allow"));
    }
    Ok(decision)
}

fn apply_incoming_invite_policy(
    input: InvitePolicyRequest,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    let now = Instant::now();
    let calls = INCOMING_CALLS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut calls) = calls.lock() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "incoming_call_policy_unavailable",
        ));
    };
    calls.retain(|_, stored| now.duration_since(stored.touched) < INCOMING_CALL_TRACKER_TTL);

    if input.action == "incoming_admit" {
        if input.cseq == 0 {
            return Err((StatusCode::BAD_REQUEST, "invalid_incoming_invite_cseq"));
        }
        if let Some(stored) = calls.get_mut(&input.call_id) {
            let action = match stored.tracker.retransmission(input.cseq, input.available) {
                proletarius_call::IncomingInviteAdmission::Accept => "accept",
                proletarius_call::IncomingInviteAdmission::ResendRinging => "resend_ringing",
                proletarius_call::IncomingInviteAdmission::ResendAnswer => "resend_answer",
                proletarius_call::IncomingInviteAdmission::Busy => "busy",
            };
            stored.touched = now;
            let (response_status, response_reason) = match action {
                "accept" => (100, "Trying"),
                "resend_ringing" => (180, "Ringing"),
                "resend_answer" => (200, "OK"),
                "busy" => (486, "Busy Here"),
                _ => unreachable!("incoming admission action is exhaustive"),
            };
            return Ok(serde_json::json!({
                "action":action,"phase":incoming_phase_name(stored.tracker.phase()),
                "response_status":response_status,"response_reason":response_reason
            }));
        }
        if !input.available {
            return Ok(serde_json::json!({
                "action":"busy","phase":"busy",
                "response_status":486,"response_reason":"Busy Here"
            }));
        }
        if calls.len() >= MAX_INCOMING_CALL_TRACKERS {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "incoming_call_policy_capacity",
            ));
        }
        let tracker = proletarius_call::IncomingCallTracker::begin(input.cseq)
            .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_incoming_invite_cseq"))?;
        calls.insert(
            input.call_id,
            StoredIncomingCall {
                tracker,
                touched: now,
            },
        );
        return Ok(serde_json::json!({
            "action":"accept","phase":"trying",
            "response_status":100,"response_reason":"Trying"
        }));
    }

    if input.action == "incoming_end" {
        if calls
            .get(&input.call_id)
            .is_some_and(|stored| stored.tracker.cseq() != input.cseq)
        {
            return Err((
                StatusCode::CONFLICT,
                "incoming_call_policy_transaction_mismatch",
            ));
        }
        calls.remove(&input.call_id);
        return Ok(serde_json::json!({"action":"ended","phase":"ended"}));
    }

    if input.action == "incoming_cancel" {
        let Some(stored) = calls.get_mut(&input.call_id) else {
            return Ok(serde_json::json!({
                "action":"cancel_rejected","phase":"ended",
                "response_status":481,"response_reason":"Call/Transaction Does Not Exist"
            }));
        };
        let decision = stored.tracker.cancel(input.cseq);
        stored.touched = now;
        let response = match decision {
            proletarius_call::IncomingCancelDecision::Accept => serde_json::json!({
                "action":"cancel_accepted","phase":incoming_phase_name(stored.tracker.phase()),
                "response_status":200,"response_reason":"OK",
                "invite_response_status":487,"invite_response_reason":"Request Terminated"
            }),
            proletarius_call::IncomingCancelDecision::Reject => serde_json::json!({
                "action":"cancel_rejected","phase":incoming_phase_name(stored.tracker.phase()),
                "response_status":481,"response_reason":"Call/Transaction Does Not Exist"
            }),
        };
        return Ok(response);
    }

    if input.action == "incoming_timeout" {
        let Some(stored) = calls.get_mut(&input.call_id) else {
            return Ok(serde_json::json!({"action":"timeout_ignored","phase":"ended"}));
        };
        let decision = stored.tracker.timeout(input.cseq);
        stored.touched = now;
        let action = match decision {
            proletarius_call::IncomingTimeoutDecision::Expire => "timeout_expired",
            proletarius_call::IncomingTimeoutDecision::Ignore => "timeout_ignored",
        };
        return Ok(
            serde_json::json!({"action":action,"phase":incoming_phase_name(stored.tracker.phase())}),
        );
    }

    if input.action == "incoming_bye" {
        let Some(stored) = calls.get_mut(&input.call_id) else {
            return Ok(serde_json::json!({
                "action":"bye_rejected","phase":"ended",
                "response_status":481,"response_reason":"Call/Transaction Does Not Exist"
            }));
        };
        let decision = stored.tracker.bye(input.cseq);
        stored.touched = now;
        let response = match decision {
            proletarius_call::IncomingByeDecision::Accept => serde_json::json!({
                "action":"bye_accepted","phase":incoming_phase_name(stored.tracker.phase()),
                "response_status":200,"response_reason":"OK"
            }),
            proletarius_call::IncomingByeDecision::Reject => serde_json::json!({
                "action":"bye_rejected","phase":incoming_phase_name(stored.tracker.phase()),
                "response_status":481,"response_reason":"Call/Transaction Does Not Exist"
            }),
        };
        return Ok(response);
    }

    if input.action == "incoming_hangup" {
        let Some(stored) = calls.get_mut(&input.call_id) else {
            return Ok(
                serde_json::json!({"action":"hangup_ignore","phase":"ended","request_cseq":0}),
            );
        };
        if stored.tracker.cseq() != input.cseq {
            return Err((
                StatusCode::CONFLICT,
                "incoming_call_policy_transaction_mismatch",
            ));
        }
        let request_cseq = match stored.tracker.phase() {
            proletarius_call::IncomingCallPhase::Connecting
            | proletarius_call::IncomingCallPhase::Active => allocate_local_call_cseq()?,
            _ => 0,
        };
        let decision = stored
            .tracker
            .local_hangup(input.cseq, request_cseq)
            .map_err(|_| (StatusCode::CONFLICT, "incoming_call_policy_invalid_hangup"))?;
        stored.touched = now;
        let (action, request_cseq) = match decision {
            proletarius_call::IncomingLocalHangupDecision::Reject => ("hangup_reject", 0),
            proletarius_call::IncomingLocalHangupDecision::Bye { cseq } => ("hangup_bye", cseq),
            proletarius_call::IncomingLocalHangupDecision::Ignore => ("hangup_ignore", 0),
        };
        return Ok(serde_json::json!({
            "action": action,
            "phase": incoming_phase_name(stored.tracker.phase()),
            "request_cseq": request_cseq,
            "response_status": if action == "hangup_reject" { 486 } else { 0 },
            "response_reason": if action == "hangup_reject" { "Busy Here" } else { "" },
        }));
    }

    let stored = calls
        .get_mut(&input.call_id)
        .ok_or((StatusCode::NOT_FOUND, "incoming_call_policy_missing"))?;
    if input.cseq != stored.tracker.cseq() {
        return Err((
            StatusCode::CONFLICT,
            "incoming_call_policy_transaction_mismatch",
        ));
    }
    match input.action.as_str() {
        "incoming_ringing" => stored.tracker.ringing(),
        "incoming_answered" => stored.tracker.answered(),
        "incoming_acknowledged" => stored.tracker.acknowledged(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "invalid_incoming_call_policy_action",
            ));
        }
    }
    .map_err(|_| {
        (
            StatusCode::CONFLICT,
            "incoming_call_policy_invalid_transition",
        )
    })?;
    stored.touched = now;
    Ok(serde_json::json!({"action":"updated","phase":incoming_phase_name(stored.tracker.phase())}))
}

fn incoming_phase_name(phase: proletarius_call::IncomingCallPhase) -> &'static str {
    match phase {
        proletarius_call::IncomingCallPhase::Trying => "trying",
        proletarius_call::IncomingCallPhase::Ringing => "ringing",
        proletarius_call::IncomingCallPhase::Connecting => "connecting",
        proletarius_call::IncomingCallPhase::Active => "active",
        proletarius_call::IncomingCallPhase::Ended => "ended",
    }
}

fn invite_policy_failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &serde_json::json!({"error":"IMS call policy rejected","code":code,"retryable":false}),
    )
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        1024,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT);
        }
    };
    let Ok(input) = serde_json::from_slice::<DtmfRequest>(&body) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let digit = input.digit.trim();
    let Ok(event) = parse_event(digit) else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY);
    };
    if !matches!(input.duration_scale, 1 | 2) {
        return crate::json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({"error":"DTMF clock must be 8000 or 16000 Hz","code":"invalid_dtmf_clock","retryable":false}),
        );
    }
    let Ok((packets, next_sequence, next_timestamp)) = proletarius_call::dtmf_rtp_schedule(
        event,
        input.duration_scale,
        input.payload_type,
        input.sequence,
        input.timestamp,
        input.ssrc,
    ) else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY);
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "digit": digit,
            "event": event,
            "duration_scale": input.duration_scale,
            "payload_type": input.payload_type,
            "sequence": input.sequence,
            "timestamp": input.timestamp,
            "ssrc": input.ssrc,
            "packets": packets,
            "next_sequence": next_sequence,
            "next_timestamp": next_timestamp,
        }),
    )
}

pub(crate) async fn handle_audio_sdp(request: Request<Incoming>) -> Response<Full<Bytes>> {
    const MAX_SDP_BYTES: usize = 65_536;
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        MAX_SDP_BYTES * 6 + 1_024,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return sdp_failure(StatusCode::BAD_REQUEST, "invalid_sdp_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return sdp_failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<AudioSdpRequest>(&body) else {
        return sdp_failure(StatusCode::BAD_REQUEST, "invalid_sdp_request");
    };
    if input.sdp.len() > MAX_SDP_BYTES {
        return sdp_failure(StatusCode::PAYLOAD_TOO_LARGE, "sdp_too_large");
    }
    match negotiate_audio(&input) {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(code) => sdp_failure(StatusCode::UNPROCESSABLE_ENTITY, code),
    }
}

pub(crate) async fn handle_local_audio_sdp(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        4096,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return sdp_failure(StatusCode::BAD_REQUEST, "invalid_local_sdp_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return sdp_failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<LocalAudioSdpRequest>(&body) else {
        return sdp_failure(StatusCode::BAD_REQUEST, "invalid_local_sdp_request");
    };
    match build_local_audio_sdp(input) {
        Ok(sdp) => crate::json_response(StatusCode::OK, &serde_json::json!({"sdp":sdp})),
        Err(code) => sdp_failure(StatusCode::UNPROCESSABLE_ENTITY, code),
    }
}

fn build_local_audio_sdp(input: LocalAudioSdpRequest) -> Result<String, &'static str> {
    let local_ip = input
        .local_ip
        .parse::<IpAddr>()
        .map_err(|_| "invalid_local_audio_endpoint")?;
    let codec = match input.codec.as_deref() {
        None => None,
        Some("EVS") => Some(proletarius_call::sdp::AudioCodec::Evs {
            header_full: input.evs_header_full,
        }),
        Some("AMR-WB") if input.amr_wb_mode <= 8 => {
            Some(proletarius_call::sdp::AudioCodec::AmrWb {
                octet_aligned: input.amr_octet_aligned,
                mode: input.amr_wb_mode,
                policy: proletarius_call::sdp::AmrWbModePolicy::single(input.amr_wb_mode).unwrap(),
            })
        }
        _ => return Err("unsupported_local_audio_codec"),
    };
    proletarius_call::sdp::build_local_sdp(
        input.session_id,
        local_ip,
        input.local_port,
        input.incoming,
        codec,
        input.payload_type,
        input.telephone_event_payload,
        input.evs_only,
    )
    .map_err(|error| match error {
        proletarius_call::sdp::SdpError::InvalidAudioEndpoint => "invalid_local_audio_endpoint",
        _ => "unsupported_local_audio_codec",
    })
}

fn negotiate_audio(input: &AudioSdpRequest) -> Result<serde_json::Value, &'static str> {
    let (evs_available, amr_wb_modes) =
        proletarius_call::evs::available_audio_codecs(input.evs_only);
    let result = proletarius_call::sdp::negotiate_audio_with_modes(
        &input.sdp,
        input.answer,
        evs_available,
        &amr_wb_modes,
    )
    .map_err(|error| match error {
        proletarius_call::sdp::SdpError::MissingAudio => "missing_audio",
        proletarius_call::sdp::SdpError::InvalidAudioEndpoint => "invalid_audio_endpoint",
        proletarius_call::sdp::SdpError::UnsupportedAudioCodec => "unsupported_audio_codec",
    })?;
    let (codec, evs_header_full, amr_wb_mode, amr_octet_aligned) = match result.codec {
        proletarius_call::sdp::AudioCodec::Evs { header_full } => ("EVS", header_full, 0, false),
        proletarius_call::sdp::AudioCodec::AmrWb {
            octet_aligned,
            mode,
            ..
        } => ("AMR-WB", false, mode, octet_aligned),
    };
    Ok(serde_json::json!({
        "remote_ip": result.remote_ip.to_string(),
        "remote_port": result.remote_port,
        "payload_type": result.payload_type,
        "codec": codec,
        "telephone_event_payload": result.telephone_event_payload,
        "evs_header_full": evs_header_full,
        "amr_wb_mode": amr_wb_mode,
        "amr_octet_aligned": amr_octet_aligned,
    }))
}

fn sdp_failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &serde_json::json!({"error":"IMS call SDP rejected","code":code,"retryable":false}),
    )
}

fn parse_event(value: &str) -> Result<u8, proletarius_call::CallError> {
    let mut chars = value.trim().chars();
    let Some(digit) = chars.next() else {
        return Err(proletarius_call::CallError::InvalidDtmfDigit);
    };
    if chars.next().is_some() {
        return Err(proletarius_call::CallError::InvalidDtmfDigit);
    }
    proletarius_call::dtmf_event(digit)
}

fn failure(status: StatusCode) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"DTMF 仅支持 0-9、* 和 #","code":"invalid_dtmf","retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite_policy(
        action: &str,
        call_id: &str,
        cseq: u32,
        status: u16,
        has_sdp: bool,
        reliable_rseq: Option<u32>,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        apply_invite_policy(InvitePolicyRequest {
            action: action.to_owned(),
            call_id: call_id.to_owned(),
            cseq,
            status,
            has_sdp,
            reliable_rseq,
            available: true,
            method: String::new(),
            sms_only: false,
            has_call_handler: false,
            default_allow: String::new(),
            request_call_id: String::new(),
            active_ussd_call_id: String::new(),
        })
    }

    fn incoming_policy(
        action: &str,
        call_id: &str,
        cseq: u32,
        available: bool,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        apply_invite_policy(InvitePolicyRequest {
            action: action.to_owned(),
            call_id: call_id.to_owned(),
            cseq,
            status: 0,
            has_sdp: false,
            reliable_rseq: None,
            available,
            method: String::new(),
            sms_only: false,
            has_call_handler: false,
            default_allow: String::new(),
            request_call_id: String::new(),
            active_ussd_call_id: String::new(),
        })
    }

    fn incoming_request(method: &str, sms_only: bool, has_call_handler: bool) -> serde_json::Value {
        apply_invite_policy(InvitePolicyRequest {
            action: "incoming_request".to_owned(),
            call_id: String::new(),
            cseq: 0,
            status: 0,
            has_sdp: false,
            reliable_rseq: None,
            available: true,
            method: method.to_owned(),
            sms_only,
            has_call_handler,
            default_allow: "REGISTER,INVITE,ACK,CANCEL,BYE,OPTIONS,INFO,UPDATE,PRACK,MESSAGE"
                .to_owned(),
            request_call_id: String::new(),
            active_ussd_call_id: String::new(),
        })
        .unwrap()
    }

    #[test]
    fn incoming_request_route_preserves_sms_voice_and_unknown_method_semantics() {
        assert_eq!(incoming_request("MESSAGE", false, true)["status"], 200);
        assert_eq!(incoming_request("OPTIONS", false, true)["status"], 200);
        assert_eq!(
            incoming_request("ACK", false, true)["action"],
            "ack_dispatch"
        );
        assert_eq!(incoming_request("ACK", true, true)["action"], "ack_ignore");
        assert_eq!(incoming_request("INVITE", true, true)["status"], 488);
        assert_eq!(incoming_request("BYE", true, true)["status"], 481);
        assert_eq!(incoming_request("INVITE", false, false)["status"], 480);
        assert_eq!(incoming_request("PRACK", false, false)["status"], 481);
        assert_eq!(
            incoming_request("INVITE", false, true)["action"],
            "call_dispatch"
        );
        let unsupported = incoming_request("PUBLISH", false, false);
        assert_eq!(unsupported["status"], 405);
        assert_eq!(
            unsupported["allow"],
            "REGISTER,INVITE,ACK,CANCEL,BYE,OPTIONS,INFO,UPDATE,PRACK,MESSAGE"
        );
        assert_eq!(
            incoming_request("PUBLISH", true, false)["allow"],
            "MESSAGE,OPTIONS"
        );
    }

    #[test]
    fn incoming_request_route_rejects_unsafe_allow_header() {
        let error = apply_invite_policy(InvitePolicyRequest {
            action: "incoming_request".to_owned(),
            call_id: String::new(),
            cseq: 0,
            status: 0,
            has_sdp: false,
            reliable_rseq: None,
            available: true,
            method: "PUBLISH".to_owned(),
            sms_only: false,
            has_call_handler: false,
            default_allow: "MESSAGE\r\nX-Injected: yes".to_owned(),
            request_call_id: String::new(),
            active_ussd_call_id: String::new(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            (StatusCode::BAD_REQUEST, "invalid_incoming_request_allow")
        );
    }

    #[test]
    fn incoming_request_routes_only_exact_active_ussd_call_id() {
        let route = |request_call_id: &str, active_ussd_call_id: &str| {
            apply_incoming_request_policy(InvitePolicyRequest {
                action: "incoming_request".to_owned(),
                call_id: String::new(),
                cseq: 0,
                status: 0,
                has_sdp: false,
                reliable_rseq: None,
                available: true,
                method: "INFO".to_owned(),
                sms_only: false,
                has_call_handler: true,
                default_allow: "INVITE,ACK,BYE,INFO,MESSAGE,OPTIONS".to_owned(),
                request_call_id: request_call_id.to_owned(),
                active_ussd_call_id: active_ussd_call_id.to_owned(),
            })
            .unwrap()
        };

        assert_eq!(
            route("ussd-dialog-1", "ussd-dialog-1")["action"],
            "ussd_dispatch"
        );
        assert_eq!(
            route("other-dialog", "ussd-dialog-1")["action"],
            "call_dispatch"
        );
        assert_eq!(route("ussd-dialog-1", "")["action"], "call_dispatch");
        assert_eq!(route("", "")["action"], "call_dispatch");
    }

    #[test]
    fn incoming_request_rejects_unsafe_ussd_call_id_facts() {
        let error = apply_incoming_request_policy(InvitePolicyRequest {
            action: "incoming_request".to_owned(),
            call_id: String::new(),
            cseq: 0,
            status: 0,
            has_sdp: false,
            reliable_rseq: None,
            available: true,
            method: "INFO".to_owned(),
            sms_only: false,
            has_call_handler: true,
            default_allow: "INVITE,ACK,BYE,INFO,MESSAGE,OPTIONS".to_owned(),
            request_call_id: "dialog\r\nInjected: yes".to_owned(),
            active_ussd_call_id: String::new(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            (StatusCode::BAD_REQUEST, "invalid_incoming_request_call_id")
        );
    }

    #[test]
    fn incoming_invite_route_owns_admission_retransmissions_and_phase() {
        let call_id = "route-test-incoming-invite";
        let initial = incoming_policy("incoming_admit", call_id, 71, true).unwrap();
        assert_eq!(initial["action"], "accept");
        assert_eq!(initial["response_status"], 100);
        assert_eq!(initial["response_reason"], "Trying");
        let retry = incoming_policy("incoming_admit", call_id, 71, true).unwrap();
        assert_eq!(retry["action"], "accept");
        assert_eq!(retry["response_status"], 100);
        assert_eq!(
            incoming_policy("incoming_ringing", call_id, 71, false).unwrap()["phase"],
            "ringing"
        );
        let ringing = incoming_policy("incoming_admit", call_id, 71, false).unwrap();
        assert_eq!(ringing["action"], "resend_ringing");
        assert_eq!(ringing["response_status"], 180);
        assert_eq!(ringing["response_reason"], "Ringing");
        assert_eq!(
            incoming_policy("incoming_answered", call_id, 71, false).unwrap()["phase"],
            "connecting"
        );
        let answered = incoming_policy("incoming_admit", call_id, 71, false).unwrap();
        assert_eq!(answered["action"], "resend_answer");
        assert_eq!(answered["response_status"], 200);
        assert_eq!(answered["response_reason"], "OK");
        assert_eq!(
            incoming_policy("incoming_acknowledged", call_id, 71, false).unwrap()["phase"],
            "active"
        );
        incoming_policy("incoming_end", call_id, 71, false).unwrap();
    }

    #[test]
    fn incoming_invite_route_rejects_busy_and_stale_transitions() {
        let busy_id = "route-test-incoming-busy";
        let busy = incoming_policy("incoming_admit", busy_id, 72, false).unwrap();
        assert_eq!(busy["action"], "busy");
        assert_eq!(busy["response_status"], 486);
        assert_eq!(busy["response_reason"], "Busy Here");
        let call_id = "route-test-incoming-stale";
        incoming_policy("incoming_admit", call_id, 73, true).unwrap();
        assert_eq!(
            incoming_policy("incoming_ringing", call_id, 74, false),
            Err((
                StatusCode::CONFLICT,
                "incoming_call_policy_transaction_mismatch"
            ))
        );
        assert_eq!(
            incoming_policy("incoming_answered", call_id, 73, false),
            Err((
                StatusCode::CONFLICT,
                "incoming_call_policy_invalid_transition"
            ))
        );
        assert_eq!(
            incoming_policy("incoming_end", call_id, 74, false),
            Err((
                StatusCode::CONFLICT,
                "incoming_call_policy_transaction_mismatch"
            ))
        );
        incoming_policy("incoming_end", call_id, 73, false).unwrap();
    }

    #[test]
    fn incoming_cancel_route_accepts_only_matching_pending_invite() {
        let call_id = "route-test-incoming-cancel";
        incoming_policy("incoming_admit", call_id, 81, true).unwrap();
        incoming_policy("incoming_ringing", call_id, 81, false).unwrap();
        let rejected = incoming_policy("incoming_cancel", call_id, 82, false).unwrap();
        assert_eq!(rejected["action"], "cancel_rejected");
        assert_eq!(rejected["response_status"], 481);
        assert_eq!(
            rejected["response_reason"],
            "Call/Transaction Does Not Exist"
        );
        let accepted = incoming_policy("incoming_cancel", call_id, 81, false).unwrap();
        assert_eq!(accepted["action"], "cancel_accepted");
        assert_eq!(accepted["response_status"], 200);
        assert_eq!(accepted["response_reason"], "OK");
        assert_eq!(accepted["invite_response_status"], 487);
        assert_eq!(accepted["invite_response_reason"], "Request Terminated");
        assert_eq!(
            incoming_policy("incoming_admit", call_id, 81, true).unwrap()["action"],
            "busy"
        );

        let answered_id = "route-test-incoming-cancel-after-answer";
        incoming_policy("incoming_admit", answered_id, 83, true).unwrap();
        incoming_policy("incoming_ringing", answered_id, 83, false).unwrap();
        incoming_policy("incoming_answered", answered_id, 83, false).unwrap();
        assert_eq!(
            incoming_policy("incoming_cancel", answered_id, 83, false).unwrap()["action"],
            "cancel_rejected"
        );

        assert_eq!(
            incoming_policy("incoming_cancel", "missing-incoming-cancel", 84, false).unwrap()["action"],
            "cancel_rejected"
        );
    }

    #[test]
    fn incoming_timeout_route_expires_only_the_matching_ringing_invite() {
        let call_id = "route-test-incoming-timeout";
        incoming_policy("incoming_admit", call_id, 91, true).unwrap();
        incoming_policy("incoming_ringing", call_id, 91, false).unwrap();
        assert_eq!(
            incoming_policy("incoming_timeout", call_id, 92, false).unwrap()["action"],
            "timeout_ignored"
        );
        assert_eq!(
            incoming_policy("incoming_timeout", call_id, 91, false).unwrap()["action"],
            "timeout_expired"
        );
        incoming_policy("incoming_end", call_id, 91, false).unwrap();

        let answered_id = "route-test-incoming-timeout-after-answer";
        incoming_policy("incoming_admit", answered_id, 93, true).unwrap();
        incoming_policy("incoming_ringing", answered_id, 93, false).unwrap();
        incoming_policy("incoming_answered", answered_id, 93, false).unwrap();
        assert_eq!(
            incoming_policy("incoming_timeout", answered_id, 93, false).unwrap()["action"],
            "timeout_ignored"
        );
        incoming_policy("incoming_end", answered_id, 93, false).unwrap();

        assert_eq!(
            incoming_policy("incoming_timeout", "missing-incoming-timeout", 94, false).unwrap()["action"],
            "timeout_ignored"
        );
    }

    #[test]
    fn incoming_bye_route_ends_only_the_matching_live_dialog() {
        let call_id = "route-test-incoming-bye";
        incoming_policy("incoming_admit", call_id, 101, true).unwrap();
        incoming_policy("incoming_ringing", call_id, 101, false).unwrap();
        incoming_policy("incoming_answered", call_id, 101, false).unwrap();
        incoming_policy("incoming_acknowledged", call_id, 101, false).unwrap();
        let rejected = incoming_policy("incoming_bye", call_id, 102, false).unwrap();
        assert_eq!(rejected["action"], "bye_rejected");
        assert_eq!(rejected["response_status"], 481);
        assert_eq!(
            rejected["response_reason"],
            "Call/Transaction Does Not Exist"
        );
        let accepted = incoming_policy("incoming_bye", call_id, 101, false).unwrap();
        assert_eq!(accepted["action"], "bye_accepted");
        assert_eq!(accepted["response_status"], 200);
        assert_eq!(accepted["response_reason"], "OK");
        assert_eq!(
            incoming_policy("incoming_bye", call_id, 101, false).unwrap()["phase"],
            "ended"
        );
        assert_eq!(
            incoming_policy("incoming_bye", "missing-incoming-bye", 103, false).unwrap()["action"],
            "bye_rejected"
        );
    }

    #[test]
    fn incoming_local_hangup_uses_rust_cseq_and_preserves_decline_semantics() {
        let ringing_id = "route-test-incoming-local-reject";
        incoming_policy("incoming_admit", ringing_id, 111, true).unwrap();
        incoming_policy("incoming_ringing", ringing_id, 111, false).unwrap();
        let reject = incoming_policy("incoming_hangup", ringing_id, 111, false).unwrap();
        assert_eq!(reject["action"], "hangup_reject");
        assert_eq!(reject["phase"], "ended");
        assert_eq!(reject["request_cseq"], 0);
        assert_eq!(reject["response_status"], 486);
        assert_eq!(reject["response_reason"], "Busy Here");

        let active_id = "route-test-incoming-local-bye";
        incoming_policy("incoming_admit", active_id, 112, true).unwrap();
        incoming_policy("incoming_ringing", active_id, 112, false).unwrap();
        incoming_policy("incoming_answered", active_id, 112, false).unwrap();
        incoming_policy("incoming_acknowledged", active_id, 112, false).unwrap();
        let bye = incoming_policy("incoming_hangup", active_id, 112, false).unwrap();
        assert_eq!(bye["action"], "hangup_bye");
        assert_eq!(bye["phase"], "ended");
        assert_eq!(bye["response_status"], 0);
        let first_cseq = bye["request_cseq"].as_u64().unwrap();
        assert!(first_cseq > 0 && first_cseq <= i32::MAX as u64);
        assert_eq!(
            incoming_policy("incoming_acknowledged", active_id, 112, false).unwrap()["phase"],
            "ended",
            "a raced ACK must not resurrect a locally ended dialog"
        );

        let next_id = "route-test-incoming-local-bye-next";
        incoming_policy("incoming_admit", next_id, 113, true).unwrap();
        incoming_policy("incoming_ringing", next_id, 113, false).unwrap();
        incoming_policy("incoming_answered", next_id, 113, false).unwrap();
        let next = incoming_policy("incoming_hangup", next_id, 113, false).unwrap();
        assert!(next["request_cseq"].as_u64().unwrap() > first_cseq);
    }

    #[test]
    fn outgoing_invite_route_owns_reliable_provisional_and_duplicate_final_actions() {
        let call_id = "route-test-reliable-invite";
        assert_eq!(
            invite_policy("begin", call_id, 41, 0, false, None).unwrap()["action"],
            "begun"
        );
        assert_eq!(
            invite_policy("begin", "route-test-phase", 3, 0, false, None).unwrap()["phase"],
            "inviting"
        );
        let provisional = invite_policy("response", call_id, 41, 183, true, Some(10)).unwrap();
        assert_eq!(provisional["action"], "send_prack");
        assert_eq!(provisional["request_cseq"], 42);
        assert_eq!(provisional["start_early_media"], true);
        assert_eq!(provisional["phase"], "early");
        let final_response = invite_policy("response", call_id, 41, 200, false, None).unwrap();
        assert_eq!(final_response["action"], "ack_start_media");
        assert_eq!(final_response["request_cseq"], 0);
        assert_eq!(final_response["phase"], "active");
        let retransmission = invite_policy("response", call_id, 41, 200, false, None).unwrap();
        assert_eq!(retransmission["action"], "ack_only");
        assert_eq!(retransmission["request_cseq"], 0);
        assert_eq!(retransmission["phase"], "active");
        assert_eq!(
            invite_policy("end", call_id, 41, 0, false, None).unwrap()["action"],
            "ended"
        );
    }

    #[test]
    fn outgoing_invite_begin_allocates_unique_rust_owned_initial_cseq() {
        let first_id = "route-test-rust-cseq-first";
        let second_id = "route-test-rust-cseq-second";
        let first = invite_policy("begin", first_id, 0, 0, false, None).unwrap();
        let second = invite_policy("begin", second_id, 0, 0, false, None).unwrap();
        let first_cseq = first["request_cseq"].as_u64().unwrap() as u32;
        let second_cseq = second["request_cseq"].as_u64().unwrap() as u32;
        assert!((1..=i32::MAX as u32).contains(&first_cseq));
        assert!((1..=i32::MAX as u32).contains(&second_cseq));
        assert_ne!(first_cseq, second_cseq);
        assert_eq!(first["action"], "begun");
        assert_eq!(second["phase"], "inviting");
        invite_policy("end", first_id, first_cseq, 0, false, None).unwrap();
        invite_policy("end", second_id, second_cseq, 0, false, None).unwrap();
    }

    #[test]
    fn outgoing_invite_route_preserves_cancelled_487_termination() {
        let call_id = "route-test-cancelled-invite";
        invite_policy("begin", call_id, 9, 0, false, None).unwrap();
        assert_eq!(
            invite_policy("cancel", call_id, 9, 0, false, None).unwrap()["action"],
            "cancelled"
        );
        assert_eq!(
            invite_policy("response", call_id, 9, 180, false, None).unwrap()["phase"],
            "cancelling"
        );
        assert_eq!(
            invite_policy("response", call_id, 9, 487, false, None).unwrap()["action"],
            "ack_end"
        );
        assert_eq!(
            invite_policy("response", call_id, 9, 487, false, None).unwrap()["phase"],
            "ended"
        );
        invite_policy("end", call_id, 9, 0, false, None).unwrap();

        let raced_id = "route-test-cancelled-200-cseq";
        invite_policy("begin", raced_id, 10, 0, false, None).unwrap();
        invite_policy("cancel", raced_id, 10, 0, false, None).unwrap();
        let raced = invite_policy("response", raced_id, 10, 200, true, None).unwrap();
        assert_eq!(raced["action"], "ack_bye");
        assert_eq!(raced["request_cseq"], 11);

        let rejected_id = "route-test-invalid-sdp-cseq";
        invite_policy("begin", rejected_id, 20, 0, false, None).unwrap();
        let rejected = invite_policy("response", rejected_id, 20, 200, false, None).unwrap();
        assert_eq!(rejected["action"], "ack_fail");
        assert_eq!(rejected["request_cseq"], 21);
    }

    #[test]
    fn outgoing_invite_route_selects_cancel_or_bye_and_suppresses_repeats() {
        let cancel_id = "route-test-hangup-cancel";
        invite_policy("begin", cancel_id, 15, 0, false, None).unwrap();
        let cancel = invite_policy("hangup", cancel_id, 15, 0, false, None).unwrap();
        assert_eq!(cancel["action"], "hangup_cancel");
        assert_eq!(cancel["phase"], "cancelling");
        assert_eq!(cancel["request_cseq"], 15);
        assert_eq!(
            invite_policy("hangup", cancel_id, 15, 0, false, None).unwrap()["request_cseq"],
            0
        );
        invite_policy("response", cancel_id, 15, 487, false, None).unwrap();
        invite_policy("end", cancel_id, 15, 0, false, None).unwrap();

        let bye_id = "route-test-hangup-bye";
        invite_policy("begin", bye_id, 16, 0, false, None).unwrap();
        invite_policy("response", bye_id, 16, 200, true, None).unwrap();
        let bye = invite_policy("hangup", bye_id, 16, 0, false, None).unwrap();
        assert_eq!(bye["action"], "hangup_bye");
        assert_eq!(bye["phase"], "terminating");
        assert_eq!(bye["request_cseq"], 17);
        assert_eq!(
            invite_policy("hangup", bye_id, 16, 0, false, None).unwrap()["request_cseq"],
            0
        );
        invite_policy("end", bye_id, 16, 0, false, None).unwrap();
    }

    #[test]
    fn outgoing_invite_route_rejects_transaction_mismatch_and_duplicate_begin() {
        let call_id = "route-test-invalid-invite";
        invite_policy("begin", call_id, 5, 0, false, None).unwrap();
        assert_eq!(
            invite_policy("begin", call_id, 5, 0, false, None),
            Err((StatusCode::CONFLICT, "call_policy_exists"))
        );
        assert_eq!(
            invite_policy("response", call_id, 6, 486, false, None),
            Err((StatusCode::CONFLICT, "call_policy_invalid_response"))
        );
        invite_policy("end", call_id, 5, 0, false, None).unwrap();
    }

    fn handle_json(body: &[u8]) -> Response<Full<Bytes>> {
        let input = serde_json::from_slice::<DtmfRequest>(body).unwrap();
        let digit = input.digit.trim();
        let Ok(event) = parse_event(digit) else {
            return failure(StatusCode::UNPROCESSABLE_ENTITY);
        };
        let Ok((packets, next_sequence, next_timestamp)) = proletarius_call::dtmf_rtp_schedule(
            event,
            input.duration_scale,
            input.payload_type,
            input.sequence,
            input.timestamp,
            input.ssrc,
        ) else {
            return failure(StatusCode::UNPROCESSABLE_ENTITY);
        };
        crate::json_response(
            StatusCode::OK,
            &json!({"digit":digit,"event":event,"duration_scale":input.duration_scale,"payload_type":input.payload_type,"sequence":input.sequence,"timestamp":input.timestamp,"ssrc":input.ssrc,"packets":packets,"next_sequence":next_sequence,"next_timestamp":next_timestamp}),
        )
    }

    #[tokio::test]
    async fn emits_rfc4733_rtp_datagrams() {
        let response = handle_json(br##"{"digit":"#","duration_scale":2,"payload_type":110,"sequence":40,"timestamp":8000,"ssrc":99}"##);
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({"digit":"#","event":11,"duration_scale":2,"payload_type":110,"sequence":40,"timestamp":8000,"ssrc":99,"packets":[[128,238,0,40,0,0,31,64,0,0,0,99,11,10,1,64],[128,110,0,41,0,0,31,64,0,0,0,99,11,10,2,128],[128,110,0,42,0,0,31,64,0,0,0,99,11,10,3,192],[128,110,0,43,0,0,31,64,0,0,0,99,11,10,5,0],[128,110,0,44,0,0,31,64,0,0,0,99,11,138,6,64],[128,110,0,45,0,0,31,64,0,0,0,99,11,138,6,64],[128,110,0,46,0,0,31,64,0,0,0,99,11,138,6,64]],"next_sequence":47,"next_timestamp":10240})
        );
    }

    #[tokio::test]
    async fn rejects_invalid_dtmf_clock_scale() {
        let response = handle_json(br##"{"digit":"1","duration_scale":3,"payload_type":110,"sequence":0,"timestamp":0,"ssrc":1}"##);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn rejects_non_dtmf() {
        assert_eq!(
            handle_json(br#"{"digit":"A","payload_type":110,"sequence":0,"timestamp":0,"ssrc":1}"#)
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn local_sdp_route_maps_validated_runtime_facts() {
        let built = build_local_audio_sdp(LocalAudioSdpRequest {
            session_id: 123,
            local_ip: "192.0.2.10".to_owned(),
            local_port: 49170,
            incoming: false,
            codec: None,
            payload_type: None,
            telephone_event_payload: None,
            evs_header_full: false,
            amr_wb_mode: 0,
            amr_octet_aligned: false,
            evs_only: false,
        })
        .unwrap();
        assert!(built.contains("m=audio 49170 RTP/AVP 96 98 102\r\n"));
        assert!(built.contains("o=- 123 1 IN IP4 192.0.2.10\r\n"));
        assert_eq!(
            build_local_audio_sdp(LocalAudioSdpRequest {
                local_ip: "not-an-ip".to_owned(),
                ..LocalAudioSdpRequest {
                    session_id: 1,
                    local_ip: "192.0.2.1".to_owned(),
                    local_port: 49170,
                    incoming: false,
                    codec: None,
                    payload_type: None,
                    telephone_event_payload: None,
                    evs_header_full: false,
                    amr_wb_mode: 0,
                    amr_octet_aligned: false,
                    evs_only: false,
                }
            }),
            Err("invalid_local_audio_endpoint")
        );
    }

    #[test]
    fn sdp_policy_probes_linked_codecs_and_supports_evs_only_mode() {
        let sdp = concat!(
            "v=0\r\n",
            "c=IN IP4 192.0.2.8\r\n",
            "m=audio 49170 RTP/AVP 98 96 102\r\n",
            "a=rtpmap:98 AMR-WB/16000\r\n",
            "a=fmtp:98 octet-align=1;mode-set=0,2,4,7\r\n",
            "a=rtpmap:96 EVS/16000\r\n",
            "a=fmtp:96 br=13.2;bw=wb\r\n",
            "a=rtpmap:102 telephone-event/16000\r\n",
        );
        let input = AudioSdpRequest {
            sdp: sdp.to_owned(),
            answer: false,
            evs_only: false,
        };
        let (evs_available, amr_wb_modes) = proletarius_call::evs::available_audio_codecs(false);
        if evs_available {
            let selected = negotiate_audio(&input).unwrap();
            assert_eq!(selected["codec"], "EVS");
            assert_eq!(selected["payload_type"], 96);
        } else if !amr_wb_modes.is_empty() {
            let selected = negotiate_audio(&input).unwrap();
            assert_eq!(selected["codec"], "AMR-WB");
            assert_eq!(selected["payload_type"], 98);
        } else {
            assert_eq!(negotiate_audio(&input), Err("unsupported_audio_codec"));
        }
        let no_evs_input = AudioSdpRequest {
            sdp: sdp.replace("a=rtpmap:96 EVS/16000\r\n", ""),
            ..input.clone()
        };
        let wb_offered = [0, 2, 4, 7].iter().any(|mode| amr_wb_modes.contains(mode));
        if wb_offered {
            let selected = negotiate_audio(&no_evs_input).unwrap();
            assert_eq!(selected["codec"], "AMR-WB");
            assert_eq!(selected["payload_type"], 98);
        } else {
            assert_eq!(
                negotiate_audio(&no_evs_input),
                Err("unsupported_audio_codec")
            );
        }
        let evs_only_input = AudioSdpRequest {
            evs_only: true,
            ..input
        };
        if evs_available {
            let selected = negotiate_audio(&evs_only_input).unwrap();
            assert_eq!(selected["codec"], "EVS");
        } else {
            assert_eq!(
                negotiate_audio(&evs_only_input),
                Err("unsupported_audio_codec")
            );
        }
    }
}
