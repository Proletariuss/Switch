//! Rust owns REGISTER/MESSAGE response validation and TCP SIP framing/response
//! parsing, OPTIONS heartbeat transaction leases, and heartbeat fail-confirm
//! decisions. The compatibility runtime still owns socket I/O, timers, and
//! network-session lifecycle transitions.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

const OPTIONS_LEASE_TTL: Duration = Duration::from_secs(30);
const OPTIONS_LEASE_CAPACITY: usize = 1024;

#[derive(Clone, Deserialize)]
struct ValidateRequest {
    #[serde(default)]
    response: Vec<u8>,
    #[serde(default)]
    register_request: Vec<u8>,
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    cseq: u32,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default = "default_operation")]
    operation: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    generation: Option<u64>,
}

#[derive(Clone)]
struct OptionsLease {
    generation: u64,
    call_id: String,
    cseq: u32,
    expires_at: Instant,
}

#[derive(Default)]
struct OptionsLeaseStore {
    next_generation: u64,
    sessions: HashMap<String, OptionsLease>,
}

fn options_leases() -> &'static Mutex<OptionsLeaseStore> {
    static STORE: OnceLock<Mutex<OptionsLeaseStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(OptionsLeaseStore::default()))
}

fn default_method() -> String {
    "REGISTER".to_owned()
}

fn default_operation() -> String {
    "validate".to_owned()
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        131_072,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("heartbeat_wait_probe") {
        return crate::ims_heartbeat_supervisor::wait_for_probe(value).await;
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("heartbeat_wait_confirmation")
    {
        return crate::ims_heartbeat_supervisor::wait_for_confirmation(value).await;
    }
    dispatch_value(value)
}

fn dispatch_value(value: serde_json::Value) -> Response<Full<Bytes>> {
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("normalize_call_target") {
        return normalize_call_target(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("classify_call_rejection")
    {
        return classify_call_rejection(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("encode_ussd_xml") {
        return encode_ussd_xml(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("build_ussd_invite_body")
    {
        return build_ussd_invite_body(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("decode_ussd_xml") {
        return decode_ussd_xml(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("decode_ussd_content") {
        return decode_ussd_content(value);
    }
    if matches!(
        value.get("operation").and_then(serde_json::Value::as_str),
        Some("encode_sip_request") | Some("encode_ussd_sip_request")
    ) {
        return encode_ussd_sip_request(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("classify_ussd_invite_response")
    {
        return classify_ussd_invite_response(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("classify_ussd_inbound_request")
    {
        return classify_ussd_inbound_request(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("classify_ussd_command") {
        return classify_ussd_command(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("effective_register_expiry")
    {
        return effective_register_expiry(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("probe_sip_frame") {
        return probe_sip_frame(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("parse_sip_response") {
        return parse_sip_response(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("encode_register_request")
    {
        return encode_register_request(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("build_register_transport_headers")
    {
        return build_register_transport_headers(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("decide_initial_register_security")
    {
        return decide_initial_register_security(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("decide_challenge_security")
    {
        return decide_challenge_security(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("decide_register_auth_progress")
    {
        return decide_register_auth_progress(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("expand_register_candidates")
    {
        return expand_register_candidates(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("evaluate_register_lease")
    {
        return evaluate_register_lease(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("evaluate_register_lease_headers")
    {
        return evaluate_register_lease_headers(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("project_registered_message_routing")
    {
        return project_registered_message_routing(value);
    }
    if value
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|operation| operation.starts_with("heartbeat_"))
    {
        return crate::ims_heartbeat_supervisor::handle(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("aka_digest") {
        return compute_aka_digest(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("eap_direct_initial_authorization")
    {
        return build_eap_direct_initial_authorization(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("resolve_register_retry_delay")
    {
        return resolve_register_retry_delay(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("resolve_register_candidate_retry")
    {
        return resolve_register_candidate_retry(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("parse_aka_challenge") {
        return parse_aka_challenge(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("select_aka_challenge") {
        return select_aka_challenge(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("negotiate_security_server")
    {
        return negotiate_security_server(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("build_security_client") {
        return build_security_client(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("build_contact") {
        return build_contact(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str)
        == Some("initial_register_fallback")
    {
        return initial_register_fallback(value);
    }
    if value.get("operation").and_then(serde_json::Value::as_str) == Some("decode_aka_nonce") {
        return decode_aka_nonce(value);
    }
    let Ok(input) = serde_json::from_value::<ValidateRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match input.operation.as_str() {
        "begin" => begin_options_lease(input),
        "complete" => complete_options_lease(input),
        "observe" if input.method == "OPTIONS" => complete_options_lease(input),
        "observe" if input.method == "MESSAGE" => observe_message_response(input),
        "abort" => abort_options_lease(input),
        "validate" => validate(input),
        _ => failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_operation"),
    }
}

#[derive(Deserialize)]
struct CallTargetInput {
    peer: String,
    #[serde(default)]
    call_phone_context: String,
    #[serde(default)]
    home_domain: String,
    #[serde(default)]
    realm: String,
}

#[derive(Deserialize)]
struct CallRejectionInput {
    status: u16,
    #[serde(default)]
    response_reason: String,
    #[serde(default)]
    reason_header: String,
    #[serde(default)]
    fallback_reason: String,
}

fn classify_call_rejection(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<CallRejectionInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_call_rejection_input");
    };
    if input.response_reason.len() > 4096
        || input.reason_header.len() > 4096
        || input.fallback_reason.len() > 4096
    {
        input.response_reason.zeroize();
        input.reason_header.zeroize();
        input.fallback_reason.zeroize();
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "call_rejection_input_too_large",
        );
    }
    let result = proletarius_ims::call_rejection::rejection_message(
        input.status,
        &input.response_reason,
        &input.reason_header,
        &input.fallback_reason,
    );
    input.response_reason.zeroize();
    input.reason_header.zeroize();
    input.fallback_reason.zeroize();
    match result {
        Ok(message) => {
            crate::json_response(StatusCode::OK, &json!({"result":"ok","message":message}))
        }
        Err(proletarius_ims::call_rejection::CallRejectionError::InvalidStatus) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_call_rejection_status",
        ),
        Err(proletarius_ims::call_rejection::CallRejectionError::InputTooLarge) => failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "call_rejection_input_too_large",
        ),
    }
}

fn normalize_call_target(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<CallTargetInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_call_target_input");
    };
    if input.peer.len() > 512
        || input.call_phone_context.len() > 512
        || input.home_domain.len() > 512
        || input.realm.len() > 512
    {
        input.peer.zeroize();
        input.call_phone_context.zeroize();
        input.home_domain.zeroize();
        input.realm.zeroize();
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "call_target_too_large");
    }
    let result = proletarius_ims::call_target::normalize(
        &input.peer,
        &input.call_phone_context,
        &input.home_domain,
        &input.realm,
    );
    input.peer.zeroize();
    input.call_phone_context.zeroize();
    input.home_domain.zeroize();
    input.realm.zeroize();
    let response = match result {
        Ok(uri) if uri.len() <= 2048 => json!({"result":"ok","uri":uri}),
        Ok(_) => {
            return failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "call_target_result_too_large",
            );
        }
        Err(proletarius_ims::call_target::CallTargetError::Required) => {
            json!({"result":"target_required"})
        }
        Err(proletarius_ims::call_target::CallTargetError::PhoneContextRequired) => {
            json!({"result":"phone_context_required"})
        }
        Err(proletarius_ims::call_target::CallTargetError::EmergencyCallingDisabled) => {
            json!({"result":"emergency_calling_disabled"})
        }
        Err(proletarius_ims::call_target::CallTargetError::UssdUnavailable) => {
            json!({"result":"ussd_unavailable"})
        }
    };
    crate::json_response(StatusCode::OK, &response)
}

fn encode_ussd_xml(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Some(text) = value.get("text").and_then(serde_json::Value::as_str) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_text");
    };
    match proletarius_ims::ussd_xml::encode(text) {
        Ok(xml) => crate::json_response(
            StatusCode::OK,
            &json!({"status": 200, "xml_base64": STANDARD.encode(xml)}),
        ),
        Err(_) => failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_ussd_text"),
    }
}

fn build_ussd_invite_body(value: serde_json::Value) -> Response<Full<Bytes>> {
    let (Some(text), Some(local_ip)) = (
        value.get("text").and_then(serde_json::Value::as_str),
        value.get("local_ip").and_then(serde_json::Value::as_str),
    ) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_invite_input");
    };
    match proletarius_ims::ussd_xml::build_invite_body(text, local_ip) {
        Ok((body, content_type)) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status": 200,
                "body_base64": STANDARD.encode(body),
                "content_type": content_type,
            }),
        ),
        Err(proletarius_ims::ussd_xml::UssdXmlError::InvalidText) => {
            failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_ussd_text")
        }
        Err(proletarius_ims::ussd_xml::UssdXmlError::InvalidAddress) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_ussd_local_address",
        ),
        Err(proletarius_ims::ussd_xml::UssdXmlError::EntropyUnavailable) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "ussd_boundary_entropy_unavailable",
        ),
        Err(proletarius_ims::ussd_xml::UssdXmlError::InvalidXml) => {
            failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_ussd_xml")
        }
    }
}

fn decode_ussd_xml(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Some(encoded) = value.get("xml_base64").and_then(serde_json::Value::as_str) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_xml_input");
    };
    if encoded.is_empty() || encoded.len() > 24 * 1024 {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_ussd_xml");
    }
    let Ok(document) = STANDARD.decode(encoded) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_xml_input");
    };
    match proletarius_ims::ussd_xml::decode(&document) {
        Ok(result) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status": 200,
                "language": result.language,
                "text": result.text,
                "error_code": result.error_code,
            }),
        ),
        Err(_) => failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_ussd_xml"),
    }
}

fn decode_ussd_content(value: serde_json::Value) -> Response<Full<Bytes>> {
    let (Some(content_type), Some(encoded)) = (
        value
            .get("content_type")
            .and_then(serde_json::Value::as_str),
        value.get("body_base64").and_then(serde_json::Value::as_str),
    ) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_content_input");
    };
    if content_type.is_empty() || content_type.len() > 4096 {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_ussd_content_input",
        );
    }
    if encoded.len() > 88 * 1024 {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "ussd_response_too_large");
    }
    let Ok(body) = STANDARD.decode(encoded) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_content_input");
    };
    match proletarius_ims::ussd_xml::decode_content(content_type, &body) {
        Ok(result) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status": 200,
                "language": result.language,
                "text": result.text,
                "error_code": result.error_code,
            }),
        ),
        Err(error) => {
            use proletarius_ims::ussd_xml::UssdContentError as E;
            let code = match error {
                E::ResponseTooLarge => "ussd_response_too_large",
                E::InvalidContentType => "invalid_ussd_content_type",
                E::MissingContent => "missing_ussd_content",
                E::UnsupportedContentType => "unsupported_ussd_content_type",
                E::InvalidXml => "invalid_ussd_xml",
                E::CarrierError => "ussd_carrier_error",
            };
            failure(StatusCode::UNPROCESSABLE_ENTITY, code)
        }
    }
}

#[derive(Deserialize)]
struct UssdSipRequestInput {
    method: String,
    uri: String,
    headers: Vec<[String; 2]>,
    body_base64: String,
}

fn encode_ussd_sip_request(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<UssdSipRequestInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_sip_request_input");
    };
    if input.method.len() > 32
        || input.uri.len() > 4096
        || input.headers.len() > 120
        || input.body_base64.len() > 88 * 1024
    {
        input.method.zeroize();
        input.uri.zeroize();
        input.body_base64.zeroize();
        for header in &mut input.headers {
            header[0].zeroize();
            header[1].zeroize();
        }
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "ussd_sip_request_too_large");
    }
    let decoded = STANDARD.decode(&input.body_base64);
    input.body_base64.zeroize();
    let Ok(body) = decoded else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_sip_request_input");
    };
    let body = Zeroizing::new(body);
    let headers: Vec<(&str, &str)> = input
        .headers
        .iter()
        .map(|header| (header[0].as_str(), header[1].as_str()))
        .collect();
    let encoded = proletarius_ims::sip_request::SipRequest {
        method: &input.method,
        uri: &input.uri,
        headers: &headers,
        body: &body,
    }
    .encode();
    input.method.zeroize();
    input.uri.zeroize();
    for header in &mut input.headers {
        header[0].zeroize();
        header[1].zeroize();
    }
    let Ok(wire) = encoded else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_ussd_sip_request");
    };
    let wire_base64 = Zeroizing::new(STANDARD.encode(&wire));
    crate::json_response(
        StatusCode::OK,
        &json!({"status":200,"wire_base64":wire_base64.as_str()}),
    )
}

fn classify_ussd_invite_response(value: serde_json::Value) -> Response<Full<Bytes>> {
    let (Some(status), Some(established), Some(remote_tag_present)) = (
        value.get("status").and_then(serde_json::Value::as_u64),
        value
            .get("established")
            .and_then(serde_json::Value::as_bool),
        value
            .get("remote_tag_present")
            .and_then(serde_json::Value::as_bool),
    ) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_invite_response");
    };
    let Ok(status) = u16::try_from(status) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_invite_response");
    };
    let action = match proletarius_ims::ussd_dialog::invite_response_action(
        status,
        established,
        remote_tag_present,
    ) {
        proletarius_ims::ussd_dialog::InviteResponseAction::IgnoreProvisional => {
            "ignore_provisional"
        }
        proletarius_ims::ussd_dialog::InviteResponseAction::Reject => "reject",
        proletarius_ims::ussd_dialog::InviteResponseAction::Establish => "establish",
        proletarius_ims::ussd_dialog::InviteResponseAction::AcknowledgeEstablished => {
            "acknowledge_established"
        }
        proletarius_ims::ussd_dialog::InviteResponseAction::MissingRemoteTag => {
            "missing_remote_tag"
        }
    };
    crate::json_response(StatusCode::OK, &json!({"status":200,"action":action}))
}

fn classify_ussd_inbound_request(value: serde_json::Value) -> Response<Full<Bytes>> {
    let (
        Some(established),
        Some(pending_count),
        Some(remote_tag_matches),
        Some(local_tag_matches),
        Some(method),
        Some(cseq_present),
        Some(cseq),
        Some(remote_seq),
        Some(info_package_valid),
    ) = (
        value
            .get("established")
            .and_then(serde_json::Value::as_bool),
        value
            .get("pending_count")
            .and_then(serde_json::Value::as_u64),
        value
            .get("remote_tag_matches")
            .and_then(serde_json::Value::as_bool),
        value
            .get("local_tag_matches")
            .and_then(serde_json::Value::as_bool),
        value.get("method").and_then(serde_json::Value::as_str),
        value
            .get("cseq_present")
            .and_then(serde_json::Value::as_bool),
        value.get("cseq").and_then(serde_json::Value::as_u64),
        value.get("remote_seq").and_then(serde_json::Value::as_u64),
        value
            .get("info_package_valid")
            .and_then(serde_json::Value::as_bool),
    )
    else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_inbound_request");
    };
    let (Ok(pending_count), Ok(cseq), Ok(remote_seq)) = (
        usize::try_from(pending_count),
        u32::try_from(cseq),
        u32::try_from(remote_seq),
    ) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_inbound_request");
    };
    if method.is_empty() || method.len() > 32 || !method.is_ascii() {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_inbound_request");
    }
    let action = match proletarius_ims::ussd_dialog::inbound_request_action(
        established,
        pending_count,
        remote_tag_matches,
        local_tag_matches,
        method,
        cseq_present,
        cseq,
        remote_seq,
        info_package_valid,
    ) {
        proletarius_ims::ussd_dialog::InboundRequestAction::QueuePending => "queue_pending",
        proletarius_ims::ussd_dialog::InboundRequestAction::RejectServiceUnavailable => {
            "reject_service_unavailable"
        }
        proletarius_ims::ussd_dialog::InboundRequestAction::RejectDialogMissing => {
            "reject_dialog_missing"
        }
        proletarius_ims::ussd_dialog::InboundRequestAction::IgnoreAck => "ignore_ack",
        proletarius_ims::ussd_dialog::InboundRequestAction::RejectMethod => "reject_method",
        proletarius_ims::ussd_dialog::InboundRequestAction::RejectBadRequest => {
            "reject_bad_request"
        }
        proletarius_ims::ussd_dialog::InboundRequestAction::AcknowledgeDuplicate => {
            "acknowledge_duplicate"
        }
        proletarius_ims::ussd_dialog::InboundRequestAction::RejectInfoPackage => {
            "reject_info_package"
        }
        proletarius_ims::ussd_dialog::InboundRequestAction::Accept => "accept",
    };
    crate::json_response(StatusCode::OK, &json!({"status":200,"action":action}))
}

fn classify_ussd_command(value: serde_json::Value) -> Response<Full<Bytes>> {
    let (Some(established), Some(waiting_input), Some(cancel)) = (
        value
            .get("established")
            .and_then(serde_json::Value::as_bool),
        value
            .get("waiting_input")
            .and_then(serde_json::Value::as_bool),
        value.get("cancel").and_then(serde_json::Value::as_bool),
    ) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_ussd_command");
    };
    let action =
        match proletarius_ims::ussd_dialog::command_action(established, waiting_input, cancel) {
            proletarius_ims::ussd_dialog::CommandAction::RejectNotEstablished => {
                "reject_not_established"
            }
            proletarius_ims::ussd_dialog::CommandAction::RejectWaitingForNetwork => {
                "reject_waiting_for_network"
            }
            proletarius_ims::ussd_dialog::CommandAction::SendInput => "send_input",
            proletarius_ims::ussd_dialog::CommandAction::Cancel => "cancel",
        };
    crate::json_response(StatusCode::OK, &json!({"status":200,"action":action}))
}

#[derive(Deserialize)]
struct EffectiveRegisterExpiryInput {
    configured_seconds: i64,
    template_seconds: i64,
}

fn effective_register_expiry(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<EffectiveRegisterExpiryInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_register_expiry_input");
    };
    let seconds = proletarius_ims::register_lease::effective_register_expiry(
        input.configured_seconds,
        input.template_seconds,
    );
    crate::json_response(
        StatusCode::OK,
        &json!({"status":200,"expires_seconds":seconds}),
    )
}

#[derive(Deserialize)]
struct SIPFrameProbeRequest {
    buffer_base64: String,
}

fn probe_sip_frame(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<SIPFrameProbeRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_sip_frame_probe_request");
    };
    if input.buffer_base64.len() > 96_000 {
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "sip_frame_probe_input_too_large",
        );
    }
    let decoded = STANDARD.decode(&input.buffer_base64);
    input.buffer_base64.zeroize();
    let Ok(buffer) = decoded else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_sip_frame_probe_input",
        );
    };
    let buffer = Zeroizing::new(buffer);
    match proletarius_ims::stream::probe_frame(&buffer) {
        Ok(proletarius_ims::stream::FrameProbe::NeedMore) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"disposition":"need_more","consumed_bytes":0}),
        ),
        Ok(proletarius_ims::stream::FrameProbe::KeepAlive) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"disposition":"keepalive","consumed_bytes":2}),
        ),
        Ok(proletarius_ims::stream::FrameProbe::Message(length)) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"disposition":"message","consumed_bytes":length}),
        ),
        Err(_) => failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_sip_stream_frame"),
    }
}

#[derive(Deserialize)]
struct SIPResponseParseRequest {
    frame_base64: String,
}

fn parse_sip_response(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<SIPResponseParseRequest>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_sip_response_parse_request",
        );
    };
    if input.frame_base64.len() > 96_000 {
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "sip_response_input_too_large",
        );
    }
    let decoded = STANDARD.decode(&input.frame_base64);
    input.frame_base64.zeroize();
    let Ok(frame) = decoded else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_sip_response_input",
        );
    };
    let frame = Zeroizing::new(frame);
    let Ok(mut parsed) = proletarius_ims::SipResponse::parse(&frame) else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_sip_response");
    };
    let headers: Vec<_> = parsed
        .header_pairs()
        .map(|(name, value)| json!({"name":name,"value":value}))
        .collect();
    let body_base64 = Zeroizing::new(STANDARD.encode(&parsed.body));
    parsed.body.zeroize();
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status":200,
            "sip_status":parsed.status,
            "reason":parsed.reason,
            "headers":headers,
            "body_base64":body_base64.as_str()
        }),
    )
}

#[derive(Deserialize)]
struct RegisterTransportHeadersInput {
    transport: String,
    local_ip: String,
    local_port: u16,
    call_id: String,
    cseq: u32,
}

fn build_register_transport_headers(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<RegisterTransportHeadersInput>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_register_transport_headers_input",
        );
    };
    if input.transport.len() > 32 || input.local_ip.len() > 64 || input.call_id.len() > 256 {
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "register_transport_headers_input_too_large",
        );
    }
    let headers = proletarius_ims::register_request::build_transport_headers(
        &input.transport,
        &input.local_ip,
        input.local_port,
        &input.call_id,
        input.cseq,
    );
    let Ok(headers) = headers else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_register_transport_headers",
        );
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status":200,
            "via":headers.via,
            "call_id":headers.call_id,
            "cseq":headers.cseq,
            "max_forwards":headers.max_forwards
        }),
    )
}

#[derive(Deserialize)]
struct InitialRegisterSecurityInput {
    sip_status: u16,
    has_security_server: bool,
    sec_agree_enabled: bool,
    sec_agree_required: bool,
}

fn decide_initial_register_security(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<InitialRegisterSecurityInput>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_initial_register_security_input",
        );
    };
    match proletarius_ims::register_request::decide_initial_register_security(
        input.sip_status,
        input.has_security_server,
        input.sec_agree_enabled,
        input.sec_agree_required,
    ) {
        Ok(decision) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status": 200,
                "accept": true,
                "require_ipsec": decision.require_ipsec,
                "plain_ok": decision.plain_ok,
                "reason": decision.reason,
                "security_server": decision.security_server
            }),
        ),
        Err(proletarius_ims::SipError::MissingSecurityServer) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "sec_agree_required_security_server_missing",
        ),
        Err(_) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_initial_register_security_transition",
        ),
    }
}

#[derive(Deserialize)]
struct ChallengeSecurityInput {
    has_security_server: bool,
    sec_agree_enabled: bool,
    sec_agree_required: bool,
    security_client_mechanism_count: usize,
}

fn decide_challenge_security(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<ChallengeSecurityInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_challenge_security_input");
    };
    if input.security_client_mechanism_count > 32 {
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "challenge_security_mechanisms_too_large",
        );
    }
    match proletarius_ims::register_request::decide_challenge_security(
        input.has_security_server,
        input.sec_agree_enabled,
        input.sec_agree_required,
        input.security_client_mechanism_count,
    ) {
        Ok(decision) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status":200,
                "install_ipsec":decision.install_ipsec,
                "require_verify":decision.require_verify,
                "missing_security_client":decision.missing_security_client,
                "reason":decision.reason
            }),
        ),
        Err(proletarius_ims::register_request::ChallengeSecurityError::MissingSecurityServer) => {
            failure(StatusCode::UNPROCESSABLE_ENTITY, "sec_agree_required_security_server_missing")
        }
        Err(proletarius_ims::register_request::ChallengeSecurityError::MissingSecurityClientMechanisms) => {
            failure(StatusCode::UNPROCESSABLE_ENTITY, "missing_security_client_mechanisms")
        }
    }
}

#[derive(Deserialize)]
struct RegisterAuthProgressInput {
    previous_challenge_valid: bool,
    previous_challenge_algorithm: String,
    response_action: String,
    completed_attempts: u16,
    attempt_limit: u16,
    stop_plain_digest: bool,
}

fn decide_register_auth_progress(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<RegisterAuthProgressInput>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_register_auth_progress_input",
        );
    };
    if input.previous_challenge_algorithm.len() > 64 {
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "register_auth_algorithm_too_large",
        );
    }
    match proletarius_ims::register_auth_progress::decide_challenge_progress_with_policy(
        input.previous_challenge_valid,
        &input.previous_challenge_algorithm,
        &input.response_action,
        input.completed_attempts,
        input.attempt_limit,
        input.stop_plain_digest,
    ) {
        Ok(decision) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status":200,
                "continue_authentication":decision.continue_authentication,
                "reason":decision.reason
            }),
        ),
        Err(_) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_register_auth_progress",
        ),
    }
}

#[derive(Deserialize)]
struct RegisterCandidatesInput {
    candidates: Vec<String>,
    #[serde(default)]
    local_ip: String,
}

fn expand_register_candidates(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<RegisterCandidatesInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_register_candidates_input");
    };
    if input.candidates.len() > 64
        || input.local_ip.len() > 64
        || input.candidates.iter().any(|value| value.len() > 512)
    {
        input.local_ip.zeroize();
        for candidate in &mut input.candidates {
            candidate.zeroize();
        }
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "register_candidates_input_too_large",
        );
    }
    let local_ip = if input.local_ip.trim().is_empty() {
        None
    } else {
        Some(input.local_ip.trim())
    };
    let result = proletarius_ims::registrar_candidates::expand_register_candidates(
        &input.candidates,
        local_ip,
    );
    input.local_ip.zeroize();
    for candidate in &mut input.candidates {
        candidate.zeroize();
    }
    match result {
        Ok(candidates) => {
            let candidates: Vec<_> = candidates
                .into_iter()
                .map(|candidate| {
                    json!({
                        "registrar":candidate.registrar,
                        "transport":candidate.transport
                    })
                })
                .collect();
            crate::json_response(
                StatusCode::OK,
                &json!({"status":200,"candidates":candidates}),
            )
        }
        Err(_) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_register_candidates",
        ),
    }
}

#[derive(Deserialize)]
struct RegisterLeaseCandidateInput {
    compatible: bool,
    instance_match: bool,
    uri_match: bool,
    invalid_params: bool,
    expires: Option<String>,
}

#[derive(Deserialize)]
struct RegisterLeaseInput {
    contacts_present: bool,
    candidates: Vec<RegisterLeaseCandidateInput>,
    global_expires: Vec<String>,
    requested_seconds: i64,
}

fn evaluate_register_lease(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<RegisterLeaseInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_register_lease_input");
    };
    if input.candidates.len() > 128
        || input.global_expires.len() > 64
        || input.global_expires.iter().any(|value| value.len() > 32)
        || input.candidates.iter().any(|candidate| {
            candidate
                .expires
                .as_ref()
                .is_some_and(|value| value.len() > 32)
        })
    {
        for candidate in &mut input.candidates {
            if let Some(expires) = &mut candidate.expires {
                expires.zeroize();
            }
        }
        for value in &mut input.global_expires {
            value.zeroize();
        }
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "register_lease_input_too_large",
        );
    }
    let candidates: Vec<_> = input
        .candidates
        .iter()
        .map(
            |candidate| proletarius_ims::register_lease::RegisterLeaseCandidate {
                compatible: candidate.compatible,
                instance_match: candidate.instance_match,
                uri_match: candidate.uri_match,
                invalid_params: candidate.invalid_params,
                expires: candidate.expires.clone(),
            },
        )
        .collect();
    let result = proletarius_ims::register_lease::evaluate_register_lease(
        proletarius_ims::register_lease::RegisterLeaseInput {
            contacts_present: input.contacts_present,
            candidates: &candidates,
            global_expires: &input.global_expires,
            requested_seconds: input.requested_seconds,
        },
    );
    for candidate in &mut input.candidates {
        if let Some(expires) = &mut candidate.expires {
            expires.zeroize();
        }
    }
    for value in &mut input.global_expires {
        value.zeroize();
    }
    match result {
        Ok(seconds) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"expires_seconds":seconds}),
        ),
        Err(error) => crate::json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({"status":422,"code":error.code()}),
        ),
    }
}

#[derive(Deserialize, Zeroize)]
#[zeroize(drop)]
struct RegisterLeaseHeadersInput {
    request_contacts: Vec<String>,
    contacts: Vec<String>,
    global_expires: Vec<String>,
    requested_seconds: i64,
}

fn evaluate_register_lease_headers(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<RegisterLeaseHeadersInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_register_lease_input");
    };
    if input.request_contacts.len() > 16
        || input.contacts.len() > 128
        || input.global_expires.len() > 64
        || input
            .request_contacts
            .iter()
            .chain(&input.contacts)
            .any(|value| value.len() > 8192)
        || input.global_expires.iter().any(|value| value.len() > 32)
    {
        input.zeroize();
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "register_lease_input_too_large",
        );
    }
    let result = proletarius_ims::register_lease::evaluate_register_lease_headers(
        &input.request_contacts,
        &input.contacts,
        &input.global_expires,
        input.requested_seconds,
    );
    input.zeroize();
    match result {
        Ok(seconds) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"expires_seconds":seconds}),
        ),
        Err(error) => crate::json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({"status":422,"code":error.code()}),
        ),
    }
}

#[derive(Deserialize)]
struct RegisteredMessageRoutingInput {
    associated_uri_headers: Vec<String>,
    service_route_headers: Vec<String>,
    configured_identity: String,
    fallback_routes: Vec<String>,
}

fn project_registered_message_routing(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<RegisteredMessageRoutingInput>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_registered_message_routing_input",
        );
    };
    let oversized = input.associated_uri_headers.len() > 64
        || input.service_route_headers.len() > 64
        || input.fallback_routes.len() > 8
        || input.configured_identity.len() > 4096
        || input
            .associated_uri_headers
            .iter()
            .any(|header| header.len() > 8192)
        || input
            .service_route_headers
            .iter()
            .chain(input.fallback_routes.iter())
            .any(|route| route.len() > 4096);
    if oversized {
        zeroize_registered_message_routing_input(&mut input);
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "registered_message_routing_input_too_large",
        );
    }
    let result = proletarius_vowifi::message_routing::RegisteredMessageRouting::from_headers(
        &input.associated_uri_headers,
        &input.service_route_headers,
        &input.configured_identity,
        &input.fallback_routes,
    );
    let response = match result {
        Ok(routing) => crate::json_response(
            StatusCode::OK,
            &json!({
                "status":200,
                "identity":routing.identity(),
                "service_routes":routing.routes(),
                "registered_msisdn":routing.registered_msisdn()
            }),
        ),
        Err(code) => crate::json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({"status":422,"code":code}),
        ),
    };
    zeroize_registered_message_routing_input(&mut input);
    response
}

fn zeroize_registered_message_routing_input(input: &mut RegisteredMessageRoutingInput) {
    for header in &mut input.associated_uri_headers {
        header.zeroize();
    }
    for route in &mut input.service_route_headers {
        route.zeroize();
    }
    input.configured_identity.zeroize();
    for route in &mut input.fallback_routes {
        route.zeroize();
    }
}

#[derive(Deserialize)]
struct RegisterRequestEncodeInput {
    uri: String,
    headers: Vec<[String; 2]>,
    body_base64: String,
}

fn encode_register_request(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<RegisterRequestEncodeInput>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_register_request_encode_input",
        );
    };
    if input.body_base64.len() > 88_000 || input.headers.len() > 120 {
        input.body_base64.zeroize();
        for header in &mut input.headers {
            header[1].zeroize();
        }
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "register_request_encode_input_too_large",
        );
    }
    let decoded = STANDARD.decode(&input.body_base64);
    input.body_base64.zeroize();
    let Ok(body) = decoded else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_register_request_body",
        );
    };
    let body = Zeroizing::new(body);
    let encoded = {
        let headers: Vec<(&str, &str)> = input
            .headers
            .iter()
            .map(|header| (header[0].as_str(), header[1].as_str()))
            .collect();
        proletarius_ims::register_request::RegisterRequest {
            uri: &input.uri,
            headers: &headers,
            body: &body,
        }
        .encode(None)
    };
    for header in &mut input.headers {
        header[1].zeroize();
    }
    let Ok(wire) = encoded else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_register_request");
    };
    let wire_base64 = Zeroizing::new(STANDARD.encode(&wire));
    crate::json_response(
        StatusCode::OK,
        &json!({"status":200,"wire_base64":wire_base64.as_str()}),
    )
}

#[derive(Deserialize)]
struct SecurityClientMechanismRequest {
    #[serde(default)]
    algorithm: String,
    #[serde(default)]
    encryption: String,
    protocol: Option<String>,
    mode: Option<String>,
}

#[derive(Deserialize)]
struct SecurityServerNegotiationRequest {
    #[serde(default)]
    security_server: String,
    #[serde(default)]
    client_mechanisms: Vec<SecurityClientMechanismRequest>,
    #[serde(default)]
    strict: bool,
}

fn negotiate_security_server(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<SecurityServerNegotiationRequest>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_security_server_negotiation_request",
        );
    };
    if input.security_server.is_empty()
        || input.security_server.len() > 8192
        || input.client_mechanisms.len() > 32
        || input.client_mechanisms.iter().any(|mechanism| {
            mechanism.algorithm.is_empty()
                || mechanism.algorithm.len() > 128
                || mechanism.encryption.len() > 128
                || mechanism
                    .protocol
                    .as_ref()
                    .is_some_and(|value| value.len() > 128)
                || mechanism
                    .mode
                    .as_ref()
                    .is_some_and(|value| value.len() > 128)
        })
    {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_security_server_negotiation_request",
        );
    }
    let client = input
        .client_mechanisms
        .iter()
        .map(
            |mechanism| proletarius_ims::security_offer::ClientSecurityMechanism {
                algorithm: &mechanism.algorithm,
                encryption: &mechanism.encryption,
                protocol: mechanism.protocol.as_deref(),
                mode: mechanism.mode.as_deref(),
            },
        )
        .collect::<Vec<_>>();
    let Ok(negotiated) = proletarius_ims::security_offer::negotiate_security_server(
        &input.security_server,
        &client,
        input.strict,
    ) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "security_server_negotiation_failed",
        );
    };
    let verify = negotiated.security_verify().to_owned();
    let selected = negotiated.selected;
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": 200,
            "verify": verify,
            "selected": {
                "mechanism": "ipsec-3gpp",
                "algorithm": selected.algorithm,
                "encryption": selected.encryption,
                "protocol": selected.protocol,
                "mode": selected.mode,
                "preference_milli": selected.preference,
                "spi_c": selected.spi_c,
                "spi_s": selected.spi_s,
                "port_c": selected.port_c,
                "port_s": selected.port_s
            }
        }),
    )
}

#[derive(Deserialize)]
struct SecurityClientBuildRequest {
    #[serde(default)]
    client_mechanisms: Vec<SecurityClientMechanismRequest>,
    #[serde(default)]
    spi_c: u32,
    #[serde(default)]
    spi_s: u32,
    #[serde(default)]
    port_c: u16,
    #[serde(default)]
    port_s: u16,
}

fn build_security_client(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<SecurityClientBuildRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_security_client_request");
    };
    if input.client_mechanisms.len() > 32
        || input.client_mechanisms.iter().any(|mechanism| {
            mechanism.algorithm.len() > 128
                || mechanism.encryption.len() > 128
                || mechanism
                    .protocol
                    .as_ref()
                    .is_some_and(|value| value.len() > 128)
                || mechanism
                    .mode
                    .as_ref()
                    .is_some_and(|value| value.len() > 128)
        })
    {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_security_client_request",
        );
    }
    let client = input
        .client_mechanisms
        .iter()
        .map(
            |mechanism| proletarius_ims::security_offer::ClientSecurityMechanism {
                algorithm: &mechanism.algorithm,
                encryption: &mechanism.encryption,
                protocol: mechanism.protocol.as_deref(),
                mode: mechanism.mode.as_deref(),
            },
        )
        .collect::<Vec<_>>();
    let Ok(header) = proletarius_ims::security_offer::build_security_client_header(
        &client,
        input.spi_c,
        input.spi_s,
        input.port_c,
        input.port_s,
    ) else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "security_client_build_failed",
        );
    };
    crate::json_response(
        StatusCode::OK,
        &json!({"status":200,"security_client":header}),
    )
}

#[derive(Deserialize)]
struct ContactHeaderTemplateRequest {
    #[serde(default)]
    transport: String,
    #[serde(default)]
    contact_mode: String,
    #[serde(default)]
    contact_features: String,
    contact_param_order: Option<Vec<String>>,
    #[serde(default)]
    access_type: String,
    #[serde(default)]
    icsi_ref: String,
    #[serde(default)]
    expires: i64,
    #[serde(default)]
    force_header_port_5060: bool,
}

#[derive(Deserialize)]
struct ContactHeaderInputRequest {
    #[serde(default)]
    imsi: String,
    #[serde(default)]
    public_uri: String,
    #[serde(default)]
    local_ip: String,
    #[serde(default)]
    local_port: i64,
    #[serde(default)]
    sip_instance_urn: String,
    #[serde(default)]
    register_expiry_secs: i64,
    #[serde(default)]
    sms_only: bool,
}

#[derive(Deserialize)]
struct ContactHeaderBuildRequest {
    template: ContactHeaderTemplateRequest,
    input: ContactHeaderInputRequest,
}

fn build_contact(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(request) = serde_json::from_value::<ContactHeaderBuildRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_contact_request");
    };
    if request
        .template
        .contact_param_order
        .as_ref()
        .is_some_and(|order| order.len() > 64)
        || request.input.local_ip.len() > 128
        || request.input.imsi.len() > 128
        || request.input.public_uri.len() > 2048
        || request.input.sip_instance_urn.len() > 512
    {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_contact_request");
    }
    let template = proletarius_ims::contact_header::ContactTemplate {
        transport: &request.template.transport,
        contact_mode: &request.template.contact_mode,
        contact_features: &request.template.contact_features,
        contact_param_order: request.template.contact_param_order.as_deref(),
        access_type: &request.template.access_type,
        icsi_ref: &request.template.icsi_ref,
        expires: request.template.expires,
        force_header_port_5060: request.template.force_header_port_5060,
    };
    let input = proletarius_ims::contact_header::ContactBuildInput {
        imsi: &request.input.imsi,
        public_uri: &request.input.public_uri,
        local_ip: &request.input.local_ip,
        local_port: request.input.local_port,
        sip_instance_urn: &request.input.sip_instance_urn,
        register_expiry_secs: request.input.register_expiry_secs,
        sms_only: request.input.sms_only,
    };
    let Ok(contact) = proletarius_ims::contact_header::build_contact_header(&template, &input)
    else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "contact_build_failed");
    };
    crate::json_response(StatusCode::OK, &json!({"status":200,"contact":contact}))
}

#[derive(Deserialize)]
struct InitialRegisterFallbackRequest {
    #[serde(default)]
    initial_auth: String,
    #[serde(default)]
    include_pani: bool,
    #[serde(default)]
    include_cellular: bool,
    #[serde(default)]
    configured_enabled: bool,
    #[serde(default)]
    environment_enabled: bool,
    status_codes: Option<Vec<u16>>,
    status_code: Option<u16>,
}

fn initial_register_fallback(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(request) = serde_json::from_value::<InitialRegisterFallbackRequest>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_initial_register_fallback_request",
        );
    };
    if request.initial_auth.len() > 128
        || request.status_codes.as_ref().is_some_and(|codes| {
            codes.len() > 64 || codes.iter().any(|code| !(100..=699).contains(code))
        })
        || request
            .status_code
            .is_some_and(|code| !(100..=699).contains(&code))
    {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_initial_register_fallback_request",
        );
    }
    let result = proletarius_ims::register_fallback::evaluate_initial_register_fallback(
        proletarius_ims::register_fallback::InitialRegisterFallbackInput {
            initial_auth: &request.initial_auth,
            include_pani: request.include_pani,
            include_cellular: request.include_cellular,
            configured_enabled: request.configured_enabled,
            environment_enabled: request.environment_enabled,
            status_codes: request.status_codes.as_deref(),
            status_code: request.status_code,
        },
    );
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status":200,
            "variants":result.variants.iter().map(|variant| json!({
                "initial_auth":variant.initial_auth,
                "include_pani":variant.include_pani,
                "include_cellular":variant.include_cellular
            })).collect::<Vec<_>>(),
            "retry":result.retry
        }),
    )
}

#[derive(Deserialize)]
struct ParseAkaChallengeRequest {
    #[serde(default)]
    header: String,
}

fn parse_aka_challenge(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<ParseAkaChallengeRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_aka_challenge_request");
    };
    if input.header.is_empty() || input.header.len() > 8192 {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_aka_challenge_request",
        );
    }
    let Some(challenge) = proletarius_ims::aka_authorization::parse_digest_challenge(&input.header)
    else {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_aka_digest_challenge",
        );
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": 200,
            "realm": challenge.realm,
            "domain": challenge.domain,
            "nonce": challenge.nonce,
            "opaque": challenge.opaque,
            "stale": challenge.stale,
            "algorithm": challenge.algorithm,
            "qop": challenge.qop,
            "charset": challenge.charset,
            "userhash": challenge.userhash
        }),
    )
}

#[derive(Deserialize)]
struct SelectAkaChallengeRequest {
    #[serde(default)]
    status: u16,
    #[serde(default)]
    realm: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    www_authenticate: Vec<String>,
    #[serde(default)]
    proxy_authenticate: Vec<String>,
}

fn select_aka_challenge(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<SelectAkaChallengeRequest>(value) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_aka_challenge_selection_request",
        );
    };
    if !matches!(input.status, 401 | 407)
        || !matches!(input.mode.as_str(), "first" | "best")
        || input.realm.len() > 4096
        || input.www_authenticate.len() > 32
        || input.proxy_authenticate.len() > 32
        || input
            .www_authenticate
            .iter()
            .chain(input.proxy_authenticate.iter())
            .any(|header| header.is_empty() || header.len() > 8192)
    {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_aka_challenge_selection_request",
        );
    }
    let headers = if !input.www_authenticate.is_empty() {
        &input.www_authenticate
    } else if input.status == 407 {
        &input.proxy_authenticate
    } else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "missing_aka_challenge");
    };
    if headers.is_empty() {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "missing_aka_challenge");
    }

    let mut selected: Option<(
        proletarius_ims::aka_authorization::DigestChallenge,
        i32,
        usize,
    )> = None;
    for header in headers {
        let candidates = if input.mode == "best" {
            split_digest_challenges(header)
        } else {
            vec![header.trim().to_owned()]
        };
        for raw in candidates {
            let Some(challenge) = proletarius_ims::aka_authorization::parse_digest_challenge(&raw)
            else {
                continue;
            };
            let nonce_len = decoded_digest_nonce_len(&challenge.nonce);
            let score = digest_challenge_score(&challenge, &input.realm, nonce_len);
            if selected
                .as_ref()
                .is_none_or(|(_, best, _)| input.mode == "best" && score > *best)
            {
                selected = Some((challenge, score, nonce_len));
                if input.mode == "first" {
                    break;
                }
            }
        }
        if input.mode == "first" && selected.is_some() {
            break;
        }
    }
    let Some((challenge, score, nonce_len)) = selected else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "no_usable_aka_challenge");
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": 200,
            "realm": challenge.realm,
            "domain": challenge.domain,
            "nonce": challenge.nonce,
            "opaque": challenge.opaque,
            "stale": challenge.stale,
            "algorithm": challenge.algorithm,
            "qop": challenge.qop,
            "charset": challenge.charset,
            "userhash": challenge.userhash,
            "score": score,
            "nonce_len": nonce_len,
        }),
    )
}

fn split_digest_challenges(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut starts = Vec::new();
    let (mut quoted, mut escaped) = (false, false);
    for index in 0..bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quoted = !quoted;
            continue;
        }
        if quoted
            || index + 7 > bytes.len()
            || !bytes[index..index + 7].eq_ignore_ascii_case(b"digest ")
        {
            continue;
        }
        let boundary = if index == 0 {
            true
        } else {
            let mut previous = index;
            while previous > 0 && matches!(bytes[previous - 1], b' ' | b'\t') {
                previous -= 1;
            }
            previous > 0 && bytes[previous - 1] == b','
        };
        if boundary {
            starts.push(index);
        }
    }
    if starts.len() <= 1 {
        return vec![value.trim().trim_matches([',', ' ']).to_owned()];
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
            let part = value[*start..end].trim().trim_matches([',', ' ']);
            (!part.is_empty()).then(|| part.to_owned())
        })
        .collect()
}

fn decoded_digest_nonce_len(value: &str) -> usize {
    use base64::{Engine, engine::general_purpose};
    let nonce = value.trim();
    if nonce.is_empty() {
        return 0;
    }
    if nonce.len() % 2 == 0 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return nonce.len() / 2;
    }
    for engine in [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(nonce) {
            return decoded.len();
        }
    }
    0
}

fn digest_challenge_score(
    challenge: &proletarius_ims::aka_authorization::DigestChallenge,
    preferred_realm: &str,
    nonce_len: usize,
) -> i32 {
    let mut score = 0;
    if challenge
        .realm
        .trim()
        .eq_ignore_ascii_case(preferred_realm.trim())
    {
        score += 100;
    }
    if challenge.algorithm.eq_ignore_ascii_case("AKAv1-MD5") {
        score += 40;
    } else if challenge.algorithm.eq_ignore_ascii_case("AKAv2-MD5") {
        score += 35;
    } else if challenge.algorithm.eq_ignore_ascii_case("MD5") {
        score += 10;
    }
    if nonce_len >= 32 {
        score += 20;
    } else if nonce_len > 0 {
        score -= 20;
    }
    score
}

#[derive(Deserialize)]
struct DecodeAkaNonceRequest {
    #[serde(default)]
    nonce: String,
}

fn decode_aka_nonce(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<DecodeAkaNonceRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_aka_nonce_request");
    };
    let decoded = proletarius_ims::aka_nonce::decode_aka_nonce(&input.nonce);
    input.nonce.zeroize();
    let Ok(decoded) = decoded else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_aka_nonce");
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": 200,
            "rand": decoded.rand.as_slice(),
            "autn": decoded.autn.as_slice()
        }),
    )
}

#[derive(Deserialize)]
struct AkaDigestRequest {
    #[serde(default)]
    algorithm: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    realm: String,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    uri: String,
    #[serde(default)]
    opaque: String,
    #[serde(default)]
    userhash: bool,
    #[serde(default)]
    qop_offers: Vec<String>,
    #[serde(default)]
    res: Vec<u8>,
    #[serde(default)]
    auts: Vec<u8>,
    #[serde(default)]
    sync_failure: bool,
}

fn compute_aka_digest(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<AkaDigestRequest>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_aka_digest_request");
    };
    if input.algorithm.is_empty()
        || input.algorithm.len() > 64
        || input.username.is_empty()
        || input.username.len() > 4096
        || input.method != "REGISTER"
        || input.realm.is_empty()
        || input.nonce.is_empty()
        || input.uri.is_empty()
        || input.uri.len() > 4096
        || input.opaque.len() > 4096
        || input.opaque.bytes().any(|b| b.is_ascii_control())
        || input.qop_offers.len() > 8
        || input
            .qop_offers
            .iter()
            .any(|offer| offer.len() > 64 || offer.bytes().any(|b| b.is_ascii_control()))
    {
        input.res.zeroize();
        input.auts.zeroize();
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_aka_digest_request",
        );
    }
    let selected_qop = if input.qop_offers.iter().any(|offer| offer == "auth") {
        "auth"
    } else if input.qop_offers.iter().any(|offer| offer == "auth-int") {
        "auth-int"
    } else {
        ""
    };
    let mut nonce_count = 0;
    let mut cnonce = String::new();
    if !selected_qop.is_empty() {
        let mut random = [0u8; 8];
        if getrandom::fill(&mut random).is_err() {
            input.res.zeroize();
            input.auts.zeroize();
            return aka_digest_failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "ims_aka_entropy_unavailable",
            );
        }
        cnonce.reserve(16);
        for byte in random {
            cnonce.push_str(&format!("{byte:02x}"));
        }
        random.zeroize();
        nonce_count = 1;
    }
    let qop = match selected_qop {
        "" => proletarius_ims::aka_digest::DigestQop::None,
        "auth" => proletarius_ims::aka_digest::DigestQop::Auth {
            cnonce: &cnonce,
            count: nonce_count,
        },
        "auth-int" => proletarius_ims::aka_digest::DigestQop::AuthInt {
            cnonce: &cnonce,
            count: nonce_count,
            body: b"",
        },
        _ => unreachable!("qop validated above"),
    };
    let mut sync_auts = if input.sync_failure {
        let Ok(auts) = <[u8; 14]>::try_from(input.auts.as_slice()) else {
            input.res.zeroize();
            input.auts.zeroize();
            return aka_digest_failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_aka_sync_failure",
            );
        };
        Some(auts)
    } else {
        None
    };
    let password = if let Some(auts) = sync_auts.as_ref() {
        proletarius_ims::aka_digest::AkaPassword::SynchronizationFailure(auts)
    } else {
        proletarius_ims::aka_digest::AkaPassword::Res(&input.res)
    };
    let result = proletarius_ims::aka_digest::compute_aka_digest(
        &proletarius_ims::aka_digest::AkaDigestInput {
            algorithm: &input.algorithm,
            username: &input.username,
            realm: &input.realm,
            nonce: &input.nonce,
            method: &input.method,
            uri: &input.uri,
            qop,
        },
        password,
    );
    input.res.zeroize();
    input.auts.zeroize();
    sync_auts.zeroize();
    match result {
        Ok(result) => {
            let username = if input.userhash {
                let mut digest = md5::Md5::new();
                use md5::Digest as _;
                digest.update(input.username.as_bytes());
                digest.update(b":");
                digest.update(input.realm.as_bytes());
                format!("{:x}", digest.finalize())
            } else {
                input.username.clone()
            };
            let mut fields = vec![
                format!("username={username:?}"),
                format!("realm={:?}", input.realm),
                format!("nonce={:?}", input.nonce),
                format!("uri={:?}", input.uri),
                format!("algorithm={}", input.algorithm),
            ];
            if !selected_qop.is_empty() {
                fields.push(format!("cnonce={cnonce:?}"));
            }
            if !input.opaque.is_empty() {
                fields.push(format!("opaque={:?}", input.opaque));
            }
            if !selected_qop.is_empty() {
                fields.push(format!("qop={selected_qop}"));
                fields.push(format!("nc={nonce_count:08x}"));
            }
            if input.userhash {
                fields.push("userhash=true".to_string());
            }
            fields.push(format!("response={:?}", result.response.as_str()));
            let mut authorization = Zeroizing::new(format!("Digest {}", fields.join(", ")));
            if let Some(auts) = result.auts.as_deref() {
                authorization.push_str(&format!(", auts={auts:?}"));
            }
            crate::json_response(
                StatusCode::OK,
                &json!({"status":200,"response":result.response.as_str(),"auts":result.auts.as_deref().map(|value| value.as_str()),"username":username,"authorization":authorization.as_str(),"qop":selected_qop,"cnonce":cnonce,"nonce_count":nonce_count}),
            )
        }
        Err(_) => aka_digest_failure(StatusCode::UNPROCESSABLE_ENTITY, "ims_aka_digest_failed"),
    }
}

#[derive(Deserialize, Zeroize)]
#[zeroize(drop)]
struct EAPDirectInitialAuthorizationInput {
    username: String,
    domain: String,
    realm: String,
    res: Vec<u8>,
    rand: Vec<u8>,
    autn: Vec<u8>,
}

fn build_eap_direct_initial_authorization(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(mut input) = serde_json::from_value::<EAPDirectInitialAuthorizationInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_eap_direct_authorization");
    };
    if input.res.len() > 16 || input.rand.len() > 4096 || input.autn.len() > 4096 {
        input.zeroize();
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_eap_direct_authorization",
        );
    }
    let result = proletarius_ims::aka_authorization::build_eap_direct_initial_authorization(
        &input.username,
        &input.domain,
        &input.realm,
        &input.res,
        &input.rand,
        &input.autn,
    );
    input.zeroize();
    match result {
        Ok(header) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"header":header.value()}),
        ),
        Err(_) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "eap_direct_authorization_failed",
        ),
    }
}

#[derive(Deserialize)]
struct RegisterRetryDelayInput {
    status_code: u16,
    temporary_status_codes: Option<Vec<u16>>,
    temporary_retry_seconds: i64,
}

fn resolve_register_retry_delay(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<RegisterRetryDelayInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_register_retry_delay");
    };
    match proletarius_ims::register_retry::resolve_register_retry_delay_ms(
        input.status_code,
        input.temporary_status_codes.as_deref(),
        input.temporary_retry_seconds,
    ) {
        Ok(retry_after_ms) => crate::json_response(
            StatusCode::OK,
            &json!({"status":200,"retry_after_ms":retry_after_ms}),
        ),
        Err(_) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "register_retry_delay_policy_invalid",
        ),
    }
}

#[derive(Deserialize)]
struct RegisterCandidateRetryInput {
    status_code: u16,
    failure_kind: String,
    has_more: bool,
    forbidden_status_codes: Option<Vec<u16>>,
    temporary_status_codes: Option<Vec<u16>>,
}

fn resolve_register_candidate_retry(value: serde_json::Value) -> Response<Full<Bytes>> {
    let Ok(input) = serde_json::from_value::<RegisterCandidateRetryInput>(value) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_register_candidate_retry");
    };
    match proletarius_ims::register_retry::should_advance_register_candidate(
        input.status_code,
        &input.failure_kind,
        input.has_more,
        input.forbidden_status_codes.as_deref(),
        input.temporary_status_codes.as_deref(),
    ) {
        Ok(advance) => {
            crate::json_response(StatusCode::OK, &json!({"status":200,"advance":advance}))
        }
        Err(_) => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "register_candidate_retry_policy_invalid",
        ),
    }
}

fn aka_digest_failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"IMS AKA 摘要计算失败","code":code,"retryable":false}),
    )
}

fn observe_message_response(input: ValidateRequest) -> Response<Full<Bytes>> {
    if input.call_id.trim().is_empty() || input.cseq == 0 {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_message_transaction",
            "MESSAGE",
        );
    }
    let Ok(response) = proletarius_ims::SipResponse::parse(&input.response) else {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_sip_response",
            "MESSAGE",
        );
    };
    let Ok(transaction) = response.transaction() else {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_message_transaction",
            "MESSAGE",
        );
    };
    let matched = transaction.call_id == input.call_id.trim()
        && transaction.cseq == input.cseq
        && transaction.method == "MESSAGE";
    let (disposition, final_response) = if !matched {
        ("ignored", false)
    } else if response.status < 200 {
        ("provisional", false)
    } else if response.status < 300 {
        ("accepted", true)
    } else {
        ("rejected", true)
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": response.status,
            "call_id": transaction.call_id,
            "cseq": transaction.cseq,
            "method": transaction.method,
            "matched": matched,
            "final": final_response,
            "disposition": disposition
        }),
    )
}

fn begin_options_lease(input: ValidateRequest) -> Response<Full<Bytes>> {
    if input.method != "OPTIONS"
        || input.session_id.trim().is_empty()
        || input.session_id.len() > 256
        || input.call_id.trim().is_empty()
        || input.call_id.len() > 256
        || input.cseq == 0
    {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_options_lease",
            "OPTIONS",
        );
    }
    let Ok(mut store) = options_leases().lock() else {
        return failure_for_method(
            StatusCode::SERVICE_UNAVAILABLE,
            "options_lease_unavailable",
            "OPTIONS",
        );
    };
    let now = Instant::now();
    store.sessions.retain(|_, lease| lease.expires_at > now);
    let session_id = input.session_id.trim().to_owned();
    if !store.sessions.contains_key(&session_id) && store.sessions.len() >= OPTIONS_LEASE_CAPACITY {
        return failure_for_method(
            StatusCode::SERVICE_UNAVAILABLE,
            "options_lease_capacity",
            "OPTIONS",
        );
    }
    let Some(generation) = store.next_generation.checked_add(1) else {
        return failure_for_method(
            StatusCode::SERVICE_UNAVAILABLE,
            "options_generation_exhausted",
            "OPTIONS",
        );
    };
    store.next_generation = generation;
    store.sessions.insert(
        session_id,
        OptionsLease {
            generation,
            call_id: input.call_id.trim().to_owned(),
            cseq: input.cseq,
            expires_at: now + OPTIONS_LEASE_TTL,
        },
    );
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": 100,
            "call_id": input.call_id.trim(),
            "cseq": input.cseq,
            "method": "OPTIONS",
            "generation": generation,
            "expires_in_ms": OPTIONS_LEASE_TTL.as_millis() as u64
        }),
    )
}

fn complete_options_lease(input: ValidateRequest) -> Response<Full<Bytes>> {
    let Some(generation) = input.generation.filter(|generation| *generation > 0) else {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_options_generation",
            "OPTIONS",
        );
    };
    let session_id = input.session_id.trim();
    if session_id.is_empty() || input.method != "OPTIONS" {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_options_lease",
            "OPTIONS",
        );
    }
    let provisional_status = proletarius_ims::SipResponse::parse(&input.response)
        .ok()
        .filter(|response| (100..200).contains(&response.status))
        .and_then(|response| {
            let transaction = response.transaction().ok()?;
            (transaction.call_id == input.call_id.trim()
                && transaction.cseq == input.cseq
                && transaction.method == "OPTIONS")
                .then_some(response.status)
        });
    {
        let Ok(mut store) = options_leases().lock() else {
            return failure_for_method(
                StatusCode::SERVICE_UNAVAILABLE,
                "options_lease_unavailable",
                "OPTIONS",
            );
        };
        match store.sessions.get(session_id).cloned() {
            Some(lease) if lease.generation != generation => {
                return failure_for_method(
                    StatusCode::CONFLICT,
                    "stale_options_generation",
                    "OPTIONS",
                );
            }
            Some(lease) if lease.expires_at <= Instant::now() => {
                store.sessions.remove(session_id);
                return failure_for_method(
                    StatusCode::REQUEST_TIMEOUT,
                    "options_lease_expired",
                    "OPTIONS",
                );
            }
            Some(lease) if lease.call_id != input.call_id.trim() || lease.cseq != input.cseq => {
                return failure_for_method(
                    StatusCode::CONFLICT,
                    "unexpected_options_transaction",
                    "OPTIONS",
                );
            }
            Some(_) if provisional_status.is_some() => {}
            Some(_) => {
                // A matching final-response attempt is terminal even when its
                // SIP framing is malformed. A provisional response leaves the
                // transaction lease open for its later final response.
                store.sessions.remove(session_id);
            }
            None => {
                return failure_for_method(
                    StatusCode::CONFLICT,
                    "options_lease_missing_or_expired",
                    "OPTIONS",
                );
            }
        }
    }
    if let Some(status) = provisional_status {
        return crate::json_response(
            StatusCode::OK,
            &json!({
                "status": status,
                "call_id": input.call_id.trim(),
                "cseq": input.cseq,
                "method": "OPTIONS",
                "generation": generation,
                "final": false
            }),
        );
    }
    validate(input)
}

fn abort_options_lease(input: ValidateRequest) -> Response<Full<Bytes>> {
    let Some(generation) = input.generation.filter(|generation| *generation > 0) else {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_options_generation",
            "OPTIONS",
        );
    };
    if input.method != "OPTIONS" || input.session_id.trim().is_empty() {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_options_lease",
            "OPTIONS",
        );
    }
    let Ok(mut store) = options_leases().lock() else {
        return failure_for_method(
            StatusCode::SERVICE_UNAVAILABLE,
            "options_lease_unavailable",
            "OPTIONS",
        );
    };
    let retired = match store.sessions.get(input.session_id.trim()) {
        Some(lease)
            if lease.generation == generation
                && lease.call_id == input.call_id.trim()
                && lease.cseq == input.cseq =>
        {
            store.sessions.remove(input.session_id.trim());
            true
        }
        _ => false,
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "status": 200,
            "call_id": input.call_id.trim(),
            "cseq": input.cseq,
            "method": "OPTIONS",
            "generation": generation,
            "retired": retired
        }),
    )
}

fn validate(input: ValidateRequest) -> Response<Full<Bytes>> {
    let expected_method = input.method.as_str();
    if input.call_id.trim().is_empty()
        || input.cseq == 0
        || !matches!(expected_method, "REGISTER" | "MESSAGE" | "OPTIONS")
    {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_transaction",
            expected_method,
        );
    }
    let Ok(response) = proletarius_ims::SipResponse::parse(&input.response) else {
        return failure_for_method(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_sip_response",
            expected_method,
        );
    };
    let register_identity = if expected_method == "REGISTER" && !input.register_request.is_empty() {
        match proletarius_ims::registration_connection::request_transaction(&input.register_request)
        {
            Ok((call_id, cseq)) => Some((call_id.to_owned(), cseq)),
            Err(_) => {
                return failure_for_method(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_register_request_transaction",
                    expected_method,
                );
            }
        }
    } else {
        None
    };
    let expected_call_id = register_identity
        .as_ref()
        .map(|(call_id, _)| call_id.as_str())
        .unwrap_or_else(|| input.call_id.trim());
    let expected_cseq = register_identity
        .as_ref()
        .map(|(_, cseq)| *cseq)
        .unwrap_or(input.cseq);
    let transaction = if expected_method == "REGISTER" {
        match response.register_transaction() {
            Ok(transaction) => proletarius_ims::SipTransaction {
                call_id: transaction.call_id,
                cseq: transaction.cseq,
                method: "REGISTER",
            },
            Err(_) => {
                return failure_for_method(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_register_transaction",
                    expected_method,
                );
            }
        }
    } else {
        match response.transaction() {
            Ok(transaction) if transaction.method == expected_method => transaction,
            _ => {
                return failure_for_method(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    if expected_method == "OPTIONS" {
                        "invalid_options_transaction"
                    } else {
                        "invalid_message_transaction"
                    },
                    expected_method,
                );
            }
        }
    };
    if transaction.call_id != expected_call_id
        || transaction.cseq != expected_cseq
        || transaction.method != expected_method
    {
        return failure_for_method(
            StatusCode::CONFLICT,
            if expected_method == "REGISTER" {
                "unexpected_register_transaction"
            } else if expected_method == "OPTIONS" {
                "unexpected_options_transaction"
            } else {
                "unexpected_message_transaction"
            },
            expected_method,
        );
    }
    let mut summary = json!({
        "status":response.status,
        "reason":response.reason,
        "call_id":transaction.call_id,
        "cseq":transaction.cseq,
        "method":transaction.method,
        "aka_challenge":response.has_aka_challenge(),
        "security_server":response.has_security_server()
    });
    if expected_method == "REGISTER" {
        let action = if response.status == 200 {
            "accepted"
        } else if matches!(response.status, 401 | 407) {
            "challenge"
        } else {
            "rejected"
        };
        summary["register_action"] = json!(action);
    }
    if expected_method == "OPTIONS" {
        summary["final"] = json!(response.status >= 200);
    }
    crate::json_response(StatusCode::OK, &summary)
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"IMS REGISTER 响应校验失败","code":code,"retryable":false}),
    )
}

fn failure_for_method(status: StatusCode, code: &str, method: &str) -> Response<Full<Bytes>> {
    let label = match method {
        "MESSAGE" => "MESSAGE",
        "OPTIONS" => "OPTIONS",
        _ => "REGISTER",
    };
    crate::json_response(
        status,
        &json!({"error":format!("IMS {label} 响应校验失败"),"code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(response: Response<Full<Bytes>>) -> serde_json::Value {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime builds");
        let bytes = runtime.block_on(async {
            response
                .into_body()
                .collect()
                .await
                .expect("full response body is available")
                .to_bytes()
        });
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn encode_ussd_xml_operation_matches_existing_wire_shape_and_limits() {
        let response = dispatch_value(json!({
            "operation":"encode_ussd_xml", "text":"余额 & <信息> \"'"
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(
            STANDARD.decode(result["xml_base64"].as_str().unwrap()).unwrap(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ussd-data><language>en</language><ussd-string>余额 &amp; &lt;信息&gt; &#34;&#39;</ussd-string></ussd-data>".as_bytes()
        );
        for invalid in ["", "a\r", "a\n", "a\0", &"a".repeat(183)] {
            let rejected = dispatch_value(json!({"operation":"encode_ussd_xml", "text":invalid}));
            assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        let malformed = dispatch_value(json!({"operation":"encode_ussd_xml", "text":17}));
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_ussd_invite_body_operation_returns_valid_bounded_multipart() {
        let response = dispatch_value(json!({
            "operation":"build_ussd_invite_body",
            "text":"##107#",
            "local_ip":"2001:db8::1"
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        let body = STANDARD
            .decode(result["body_base64"].as_str().expect("body"))
            .expect("base64 body");
        let content_type = result["content_type"].as_str().expect("content type");
        assert!(content_type.starts_with("multipart/mixed; boundary=switch-ussd-"));
        assert!(String::from_utf8_lossy(&body).contains("o=- 1 1 IN IP6 2001:db8::1"));
        assert!(String::from_utf8_lossy(&body).contains("application/vnd.3gpp.ussd+xml"));
        assert!(body.len() < 16 * 1024);

        let invalid = dispatch_value(json!({
            "operation":"build_ussd_invite_body",
            "text":"##107#",
            "local_ip":"invalid"
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let invalid = dispatch_value(json!({
            "operation":"build_ussd_invite_body",
            "text":"",
            "local_ip":"192.0.2.1"
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn encode_ussd_sip_request_operation_preserves_order_and_adds_exact_length() {
        let response = dispatch_value(json!({
            "operation":"encode_sip_request",
            "method":"INFO",
            "uri":"sip:service@ims.example",
            "headers":[
                ["Via","SIP/2.0/TCP 192.0.2.1:5060;branch=z9hG4bK-test"],
                ["From","<sip:user@ims.example>;tag=local"],
                ["To","<sip:service@ims.example>;tag=remote"],
                ["Call-ID","ussd-1"],
                ["CSeq","2 INFO"],
                ["Max-Forwards","70"],
                ["Info-Package","g.3gpp.ussd"]
            ],
            "body_base64":STANDARD.encode([0, 0xff])
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let value = payload(response);
        let wire = STANDARD
            .decode(value["wire_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            wire,
            b"INFO sip:service@ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 192.0.2.1:5060;branch=z9hG4bK-test\r\nFrom: <sip:user@ims.example>;tag=local\r\nTo: <sip:service@ims.example>;tag=remote\r\nCall-ID: ussd-1\r\nCSeq: 2 INFO\r\nMax-Forwards: 70\r\nInfo-Package: g.3gpp.ussd\r\nContent-Length: 2\r\n\r\n\0\xff"
        );
    }

    #[test]
    fn encode_ussd_sip_request_operation_rejects_header_injection_and_bad_cseq() {
        let base = json!({
            "operation":"encode_sip_request",
            "method":"INFO",
            "uri":"sip:service@ims.example",
            "headers":[
                ["Via","SIP/2.0/TCP 192.0.2.1:5060;branch=z9hG4bK-test"],
                ["From","<sip:user@ims.example>;tag=local"],
                ["To","<sip:service@ims.example>;tag=remote"],
                ["Call-ID","ussd-1"],
                ["CSeq","2 INFO"],
                ["Max-Forwards","70"]
            ],
            "body_base64":""
        });
        assert_eq!(dispatch_value(base.clone()).status(), StatusCode::OK);
        let mut injected = base.clone();
        injected["headers"][0][1] = json!("SIP/2.0/TCP host;branch=x\r\nX-Bad: 1");
        assert_eq!(
            dispatch_value(injected).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut bad_cseq = base;
        bad_cseq["headers"][4][1] = json!("2 INVITE");
        assert_eq!(
            dispatch_value(bad_cseq).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn encode_generic_call_request_accepts_prack_and_tel_target() {
        let response = dispatch_value(json!({
            "operation":"encode_sip_request",
            "method":"PRACK",
            "uri":"tel:+8618907313060",
            "headers":[
                ["Via","SIP/2.0/TCP 192.0.2.1:5064;branch=z9hG4bK-prack;rport"],
                ["From","<sip:user@ims.example>;tag=local"],
                ["To","<tel:+8618907313060>;tag=remote"],
                ["Call-ID","call-1"],
                ["CSeq","8 PRACK"],
                ["Max-Forwards","70"],
                ["RAck","2 7 INVITE"]
            ],
            "body_base64":""
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let value = payload(response);
        let wire = STANDARD
            .decode(value["wire_base64"].as_str().unwrap())
            .unwrap();
        assert!(wire.starts_with(b"PRACK tel:+8618907313060 SIP/2.0\r\n"));
        let cseq = b"CSeq: 8 PRACK\r\n";
        assert!(wire.windows(cseq.len()).any(|window| window == cseq));
    }

    #[test]
    fn normalize_call_target_operation_preserves_context_precedence_and_errors() {
        let normalized = dispatch_value(json!({
            "operation":"normalize_call_target",
            "peer":" 888 ",
            "call_phone_context":"sip:ims.carrier.test",
            "home_domain":"ims.home.test",
            "realm":"ims.realm.test"
        }));
        assert_eq!(normalized.status(), StatusCode::OK);
        let value = payload(normalized);
        assert_eq!(value["result"], "ok");
        assert_eq!(value["uri"], "tel:888;phone-context=ims.carrier.test");

        let missing_context = dispatch_value(json!({
            "operation":"normalize_call_target","peer":"888"
        }));
        assert_eq!(missing_context.status(), StatusCode::OK);
        let missing_context = payload(missing_context);
        assert_eq!(missing_context["result"], "phone_context_required");
        assert!(missing_context.get("peer").is_none());

        let missing_target = dispatch_value(json!({
            "operation":"normalize_call_target","peer":"  ","home_domain":"ims.example.test"
        }));
        assert_eq!(missing_target.status(), StatusCode::OK);
        assert_eq!(payload(missing_target)["result"], "target_required");

        let emergency = normalize_call_target(json!({
            "operation":"normalize_call_target","peer":"+1 911","home_domain":"ims.example.test"
        }));
        assert_eq!(payload(emergency)["result"], "emergency_calling_disabled");

        let ussd = normalize_call_target(json!({
            "operation":"normalize_call_target","peer":"*100#","home_domain":"ims.example.test"
        }));
        assert_eq!(payload(ussd)["result"], "ussd_unavailable");
    }

    #[test]
    fn classify_call_rejection_preserves_user_facing_error_contract() {
        let cases = [
            (
                487,
                "Request Terminated",
                "",
                "",
                "呼叫请求被终止：SIP 487 Request Terminated",
            ),
            (
                487,
                "Request Terminated",
                "Q.850;cause=1",
                "",
                "呼叫未接通：SIP 487 / Q.850 1，号码不存在或未分配",
            ),
            (
                487,
                "Request Terminated",
                "",
                "q.850;cause=20",
                "呼叫未接通：SIP 487 / Q.850 20，被叫暂时不可达",
            ),
            (
                403,
                "Forbidden",
                "",
                "",
                "IMS call rejected: SIP 403 Forbidden",
            ),
        ];
        for (status, response_reason, reason_header, fallback_reason, expected) in cases {
            let response = dispatch_value(json!({
                "operation":"classify_call_rejection",
                "status":status,
                "response_reason":response_reason,
                "reason_header":reason_header,
                "fallback_reason":fallback_reason
            }));
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(payload(response)["message"], expected);
        }
        let invalid = dispatch_value(json!({"operation":"classify_call_rejection","status":700}));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn decode_ussd_xml_operation_returns_fields_or_stable_xml_error() {
        let response = dispatch_value(json!({
            "operation":"decode_ussd_xml",
            "xml_base64":STANDARD.encode("<ussd-data><language>en</language><ussd-string>A &amp; B</ussd-string></ussd-data>")
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(result["language"], "en");
        assert_eq!(result["text"], "A & B");
        assert_eq!(result["error_code"], "");

        let malformed = dispatch_value(json!({
            "operation":"decode_ussd_xml",
            "xml_base64":STANDARD.encode("<wrong/>")
        }));
        assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let oversized = dispatch_value(json!({
            "operation":"decode_ussd_xml",
            "xml_base64":STANDARD.encode(vec![b'a'; 16 * 1024 + 1])
        }));
        assert_eq!(oversized.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn decode_ussd_content_operation_owns_direct_and_multipart_response_parsing() {
        let direct = dispatch_value(json!({
            "operation":"decode_ussd_content",
            "content_type":"application/vnd.3gpp.ussd+xml",
            "body_base64":STANDARD.encode("<ussd-data><language>en</language><ussd-string>ok</ussd-string></ussd-data>")
        }));
        assert_eq!(direct.status(), StatusCode::OK);
        assert_eq!(payload(direct)["text"], "ok");

        let boundary = "rust-usssd";
        let body = format!(
            "--{boundary}\r\nContent-Type: application/sdp\r\n\r\nv=0\r\n--{boundary}\r\nContent-Disposition: render;handling=optional\r\nContent-Type: application/vnd.3gpp.ussd+xml\r\n\r\n<ussd-data><ussd-string>menu</ussd-string></ussd-data>\r\n--{boundary}--\r\n"
        );
        let multipart = dispatch_value(json!({
            "operation":"decode_ussd_content",
            "content_type":format!("multipart/mixed; boundary=\"{boundary}\""),
            "body_base64":STANDARD.encode(body)
        }));
        assert_eq!(multipart.status(), StatusCode::OK);
        assert_eq!(payload(multipart)["text"], "menu");

        let error = dispatch_value(json!({
            "operation":"decode_ussd_content",
            "content_type":"text/plain",
            "body_base64":STANDARD.encode("unsupported")
        }));
        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(payload(error)["code"], "unsupported_ussd_content_type");

        let malformed = dispatch_value(json!({
            "operation":"decode_ussd_content",
            "content_type":"multipart/mixed; boundary=x",
            "body_base64":STANDARD.encode("not multipart")
        }));
        assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(payload(malformed)["code"], "missing_ussd_content");
    }

    #[test]
    fn classify_ussd_invite_response_operation_is_bounded_and_stable() {
        let cases = [
            (
                json!({"operation":"classify_ussd_invite_response","status":183,"established":false,"remote_tag_present":false}),
                "ignore_provisional",
            ),
            (
                json!({"operation":"classify_ussd_invite_response","status":403,"established":false,"remote_tag_present":true}),
                "reject",
            ),
            (
                json!({"operation":"classify_ussd_invite_response","status":200,"established":false,"remote_tag_present":true}),
                "establish",
            ),
            (
                json!({"operation":"classify_ussd_invite_response","status":200,"established":true,"remote_tag_present":true}),
                "acknowledge_established",
            ),
            (
                json!({"operation":"classify_ussd_invite_response","status":200,"established":false,"remote_tag_present":false}),
                "missing_remote_tag",
            ),
        ];
        for (input, expected) in cases {
            let response = dispatch_value(input);
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(payload(response)["action"], expected);
        }
        for input in [
            json!({"operation":"classify_ussd_invite_response","status":70000,"established":false,"remote_tag_present":false}),
            json!({"operation":"classify_ussd_invite_response","status":200,"established":false}),
        ] {
            assert_eq!(dispatch_value(input).status(), StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn classify_ussd_inbound_request_operation_returns_allowlisted_actions() {
        let cases = [
            (
                json!({"established":false,"pending_count":7,"remote_tag_matches":false,"local_tag_matches":false,"method":"BYE","cseq_present":false,"cseq":0,"remote_seq":0,"info_package_valid":false}),
                "queue_pending",
            ),
            (
                json!({"established":false,"pending_count":8,"remote_tag_matches":false,"local_tag_matches":false,"method":"INFO","cseq_present":false,"cseq":0,"remote_seq":0,"info_package_valid":false}),
                "reject_service_unavailable",
            ),
            (
                json!({"established":true,"pending_count":0,"remote_tag_matches":true,"local_tag_matches":true,"method":"INFO","cseq_present":true,"cseq":5,"remote_seq":4,"info_package_valid":false}),
                "reject_info_package",
            ),
            (
                json!({"established":true,"pending_count":0,"remote_tag_matches":true,"local_tag_matches":true,"method":"OPTIONS","cseq_present":true,"cseq":5,"remote_seq":4,"info_package_valid":false}),
                "reject_method",
            ),
        ];
        for (facts, expected) in cases {
            let mut input = facts;
            input["operation"] = json!("classify_ussd_inbound_request");
            let response = dispatch_value(input);
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(payload(response)["action"], expected);
        }
        let missing = dispatch_value(json!({"operation":"classify_ussd_inbound_request"}));
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn classify_ussd_command_operation_returns_allowlisted_actions() {
        let cases = [
            (
                json!({"established":false,"waiting_input":true,"cancel":true}),
                "reject_not_established",
            ),
            (
                json!({"established":true,"waiting_input":false,"cancel":false}),
                "reject_waiting_for_network",
            ),
            (
                json!({"established":true,"waiting_input":true,"cancel":false}),
                "send_input",
            ),
            (
                json!({"established":true,"waiting_input":false,"cancel":true}),
                "cancel",
            ),
        ];
        for (facts, expected) in cases {
            let mut input = facts;
            input["operation"] = json!("classify_ussd_command");
            let response = dispatch_value(input);
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(payload(response)["action"], expected);
        }
        let missing = dispatch_value(json!({"operation":"classify_ussd_command"}));
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn heartbeat_json_operations_dispatch_through_the_existing_ims_codec_owner() {
        let session_id = format!("ims-codec-heartbeat-{}", std::process::id());
        let start = dispatch_value(json!({"operation":"heartbeat_start", "session_id":session_id}));
        assert_eq!(start.status(), StatusCode::OK);
        let started = payload(start);
        assert_eq!(started["status"], 100);
        let generation = started["generation"].as_u64().unwrap();

        let begin = dispatch_value(
            json!({"operation":"heartbeat_begin_probe", "session_id":session_id, "generation":generation}),
        );
        assert_eq!(begin.status(), StatusCode::OK);
        let probe = payload(begin);
        assert_eq!(probe["stage"], "primary");
        let probe_id = probe["probe_id"].as_u64().unwrap();

        let result = dispatch_value(json!({
            "operation":"heartbeat_record_probe", "session_id":session_id,
            "generation":generation, "probe_id":probe_id, "status_code":0, "succeeded":false
        }));
        assert_eq!(result.status(), StatusCode::OK);
        let decision = payload(result);
        assert_eq!(decision["status"], 200);
        assert_eq!(decision["decision"], "confirm_after_delay");
        assert_eq!(decision["retry_after_ms"], 3000);

        let close = dispatch_value(
            json!({"operation":"heartbeat_close", "session_id":session_id, "generation":generation}),
        );
        assert_eq!(payload(close)["closed"], true);
    }

    #[test]
    fn sip_frame_probe_operation_returns_bounded_exact_framing_decisions() {
        let frame = b"SIP/2.0 401 Unauthorized\r\nCall-ID: reg-1\r\nCSeq: 1 REGISTER\r\nContent-Length: 0\r\n\r\n";
        let encoded = STANDARD.encode(frame);
        let complete =
            dispatch_value(json!({"operation":"probe_sip_frame","buffer_base64":encoded}));
        assert_eq!(complete.status(), StatusCode::OK);
        let complete = payload(complete);
        assert_eq!(complete["status"], 200);
        assert_eq!(complete["disposition"], "message");
        assert_eq!(complete["consumed_bytes"], frame.len());

        let fragment = dispatch_value(json!({
            "operation":"probe_sip_frame",
            "buffer_base64":STANDARD.encode(b"SIP/2.0 401 Unauthorized\r\nContent-Len")
        }));
        assert_eq!(payload(fragment)["disposition"], "need_more");

        let keepalive = dispatch_value(json!({
            "operation":"probe_sip_frame",
            "buffer_base64":STANDARD.encode(b"\r\nSIP/2.0 200 OK")
        }));
        assert_eq!(payload(keepalive)["disposition"], "keepalive");

        let malformed = dispatch_value(json!({
            "operation":"probe_sip_frame",
            "buffer_base64":STANDARD.encode(b"SIP/2.0 200 OK\r\n\r\n")
        }));
        assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn sip_response_parse_operation_unfolds_headers_and_preserves_binary_body() {
        let frame = b"SIP/2.0 401 Unauthorized\r\nWWW-Authenticate: Digest realm=ims.example,\r\n nonce=abc\r\nContent-Length: 2\r\n\r\n\0\xff";
        let response = dispatch_value(json!({
            "operation":"parse_sip_response",
            "frame_base64":STANDARD.encode(frame)
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = payload(response);
        assert_eq!(parsed["status"], 200);
        assert_eq!(parsed["sip_status"], 401);
        assert_eq!(parsed["reason"], "Unauthorized");
        assert_eq!(
            parsed["headers"][0],
            json!({"name":"WWW-Authenticate","value":"Digest realm=ims.example, nonce=abc"})
        );
        assert_eq!(parsed["body_base64"], STANDARD.encode(b"\0\xff"));

        let malformed = dispatch_value(json!({
            "operation":"parse_sip_response",
            "frame_base64":STANDARD.encode(b"SIP/2.0 200 OK\r\n folded\r\nContent-Length: 0\r\n\r\n")
        }));
        assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn register_request_encode_operation_preserves_header_order_and_binary_body() {
        let headers = json!([
            ["Via", "SIP/2.0/TCP 192.0.2.10:5060;branch=z9hG4bK-one"],
            ["From", "<sip:u@ims.example>;tag=local"],
            ["To", "<sip:u@ims.example>"],
            ["Call-ID", "reg-1"],
            ["CSeq", "1 REGISTER"],
            ["Max-Forwards", "70"],
            ["Contact", "<sip:u@192.0.2.10:5060;transport=tcp>"],
            ["Supported", "path,sec-agree"],
            ["Authorization", "Digest username=\"u\""]
        ]);
        let response = dispatch_value(json!({
            "operation":"encode_register_request",
            "uri":"sip:ims.example",
            "headers":headers,
            "body_base64":STANDARD.encode(b"a\0b")
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        let wire = STANDARD
            .decode(result["wire_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            wire,
            b"REGISTER sip:ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 192.0.2.10:5060;branch=z9hG4bK-one\r\nFrom: <sip:u@ims.example>;tag=local\r\nTo: <sip:u@ims.example>\r\nCall-ID: reg-1\r\nCSeq: 1 REGISTER\r\nMax-Forwards: 70\r\nContact: <sip:u@192.0.2.10:5060;transport=tcp>\r\nSupported: path,sec-agree\r\nAuthorization: Digest username=\"u\"\r\nContent-Length: 3\r\n\r\na\0b"
        );

        let duplicate = dispatch_value(json!({
            "operation":"encode_register_request",
            "uri":"sip:ims.example",
            "headers":[["Via","SIP/2.0/TCP 192.0.2.10:5060;branch=x"],["From","<sip:u@ims.example>"],["To","<sip:u@ims.example>"],["Call-ID","reg-1"],["i","other"],["CSeq","1 REGISTER"],["Max-Forwards","70"],["Contact","<sip:u@192.0.2.10>" ]],
            "body_base64":""
        }));
        assert_eq!(duplicate.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn register_transport_header_operation_builds_bounded_ipv6_transaction_fields() {
        let response = dispatch_value(json!({
            "operation":"build_register_transport_headers",
            "transport":"tcp",
            "local_ip":"2001:db8::10",
            "local_port":5060,
            "call_id":"session@example.test",
            "cseq":7
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        let via = result["via"].as_str().unwrap();
        assert!(via.starts_with("SIP/2.0/TCP [2001:db8::10]:5060;branch=z9hG4bK."));
        assert_eq!(via.split(";branch=").nth(1).unwrap().len(), 24);
        assert_eq!(result["call_id"], "session@example.test");
        assert_eq!(result["cseq"], "7 REGISTER");
        assert_eq!(result["max_forwards"], "70");

        let invalid = dispatch_value(json!({
            "operation":"build_register_transport_headers",
            "transport":"tcp",
            "local_ip":"2001:db8::10",
            "local_port":5060,
            "call_id":"bad\\r\\nVia: injected",
            "cseq":7
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn initial_register_security_operation_preserves_decline_and_required_policy() {
        let ipsec = dispatch_value(json!({
            "operation":"decide_initial_register_security",
            "sip_status":200,
            "has_security_server":true,
            "sec_agree_enabled":true,
            "sec_agree_required":true
        }));
        assert_eq!(ipsec.status(), StatusCode::OK);
        let decision = payload(ipsec);
        assert_eq!(decision["require_ipsec"], true);
        assert_eq!(decision["plain_ok"], false);
        assert_eq!(decision["security_server"], true);
        assert_eq!(
            decision["reason"],
            "initial_200_security_server_without_ipsec_install_required"
        );

        let missing = dispatch_value(json!({
            "operation":"decide_initial_register_security",
            "sip_status":200,
            "has_security_server":false,
            "sec_agree_enabled":true,
            "sec_agree_required":true
        }));
        assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            payload(missing)["code"],
            "sec_agree_required_security_server_missing"
        );
    }

    #[test]
    fn challenge_security_operation_distinguishes_auto_and_required_errors() {
        let auto = dispatch_value(json!({
            "operation":"decide_challenge_security",
            "has_security_server":false,
            "sec_agree_enabled":true,
            "sec_agree_required":false,
            "security_client_mechanism_count":2
        }));
        assert_eq!(auto.status(), StatusCode::OK);
        let decision = payload(auto);
        assert_eq!(decision["install_ipsec"], false);
        assert_eq!(decision["require_verify"], false);
        assert_eq!(decision["reason"], "security_server_missing_auto_disable");

        let required = dispatch_value(json!({
            "operation":"decide_challenge_security",
            "has_security_server":false,
            "sec_agree_enabled":true,
            "sec_agree_required":true,
            "security_client_mechanism_count":2
        }));
        assert_eq!(required.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            payload(required)["code"],
            "sec_agree_required_security_server_missing"
        );

        let missing_client = dispatch_value(json!({
            "operation":"decide_challenge_security",
            "has_security_server":true,
            "sec_agree_enabled":true,
            "sec_agree_required":false,
            "security_client_mechanism_count":0
        }));
        assert_eq!(missing_client.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            payload(missing_client)["code"],
            "missing_security_client_mechanisms"
        );
    }

    #[test]
    fn register_auth_progress_operation_stops_plain_digest_and_caps_aka_retries() {
        let plain = dispatch_value(json!({
            "operation":"decide_register_auth_progress",
            "previous_challenge_valid":true,
            "previous_challenge_algorithm":"MD5",
            "response_action":"challenge",
            "completed_attempts":1,
            "attempt_limit":6,
            "stop_plain_digest":true
        }));
        assert_eq!(plain.status(), StatusCode::OK);
        let plain = payload(plain);
        assert_eq!(plain["continue_authentication"], false);
        assert_eq!(plain["reason"], "plain_digest_rejected");

        let retry = dispatch_value(json!({
            "operation":"decide_register_auth_progress",
            "previous_challenge_valid":true,
            "previous_challenge_algorithm":"AKAv1-MD5",
            "response_action":"challenge",
            "completed_attempts":5,
            "attempt_limit":6,
            "stop_plain_digest":true
        }));
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(payload(retry)["continue_authentication"], true);

        let exhausted = dispatch_value(json!({
            "operation":"decide_register_auth_progress",
            "previous_challenge_valid":true,
            "previous_challenge_algorithm":"AKAv1-MD5",
            "response_action":"challenge",
            "completed_attempts":6,
            "attempt_limit":6,
            "stop_plain_digest":true
        }));
        assert_eq!(exhausted.status(), StatusCode::OK);
        let exhausted = payload(exhausted);
        assert_eq!(exhausted["continue_authentication"], false);
        assert_eq!(exhausted["reason"], "challenge_limit_reached");

        let legacy = dispatch_value(json!({
            "operation":"decide_register_auth_progress",
            "previous_challenge_valid":true,
            "previous_challenge_algorithm":"MD5",
            "response_action":"challenge",
            "completed_attempts":1,
            "attempt_limit":2,
            "stop_plain_digest":false
        }));
        assert_eq!(legacy.status(), StatusCode::OK);
        let legacy = payload(legacy);
        assert_eq!(legacy["continue_authentication"], true);
        assert_eq!(legacy["reason"], "retry_challenge");
    }

    #[test]
    fn registered_message_routing_projection_uses_authorized_identity_and_route_order() {
        let response = dispatch_value(json!({
            "operation":"project_registered_message_routing",
            "associated_uri_headers":["<tel:+447700900123>, \"temporary\" <tel:+447700900123>, <sip:public@ims.example>"],
            "service_route_headers":["<sip:first;lr>"," <sip:second;lr> "],
            "configured_identity":"sip:public@ims.example",
            "fallback_routes":["<sip:fallback;lr>"]
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let projected = payload(response);
        assert_eq!(projected["status"], 200);
        assert_eq!(projected["identity"], "sip:public@ims.example");
        assert_eq!(projected["registered_msisdn"], "+447700900123");
        assert_eq!(
            projected["service_routes"],
            json!(["<sip:first;lr>", "<sip:second;lr>"])
        );

        let fallback = dispatch_value(json!({
            "operation":"project_registered_message_routing",
            "associated_uri_headers":[],
            "service_route_headers":[],
            "configured_identity":"sip:public@ims.example",
            "fallback_routes":["<sip:fallback;lr>"]
        }));
        assert_eq!(
            payload(fallback)["service_routes"],
            json!(["<sip:fallback;lr>"])
        );

        let invalid = dispatch_value(json!({
            "operation":"project_registered_message_routing",
            "associated_uri_headers":[],
            "service_route_headers":[],
            "configured_identity":"not-a-sip-identity",
            "fallback_routes":["<sip:fallback;lr>"]
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            payload(invalid)["code"],
            "invalid_registered_message_identity"
        );
    }

    #[test]
    fn register_candidate_operation_preserves_ranked_gateway_pairs() {
        let response = dispatch_value(json!({
            "operation":"expand_register_candidates",
            "local_ip":"2a03:dd00:1381:7dce:fb36:ef7b:d917:548b",
            "candidates":[
                "[2a03:dd00:1381:7dce:fb36:ef7b:d917:548b]:5060",
                "[2a03:dd00:1f81:3010::4]:5060",
                "[2a03:dd00:1f80:4860::4]:5060"
            ]
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        let values = result["candidates"].as_array().unwrap();
        assert_eq!(values.len(), 4);
        assert_eq!(
            values[0]["registrar"],
            "[2a03:dd00:1381:7dce:fb36:ef7b:d917:548b]:5060"
        );
        assert_eq!(values[0]["transport"], "[2a03:dd00:1f80:4860::4]:5060");
        assert_eq!(values[1]["transport"], "[2a03:dd00:1f81:3010::4]:5060");

        let invalid = dispatch_value(json!({
            "operation":"expand_register_candidates",
            "local_ip":"not-an-ip",
            "candidates":["registrar.example:5060"]
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn register_lease_operation_applies_binding_precedence_and_revocation() {
        let response = dispatch_value(json!({
            "operation":"evaluate_register_lease",
            "contacts_present":true,
            "candidates":[
                {"compatible":true,"instance_match":false,"uri_match":true,"invalid_params":false,"expires":"90"},
                {"compatible":true,"instance_match":true,"uri_match":false,"invalid_params":false,"expires":"600"},
                {"compatible":false,"instance_match":true,"uri_match":true,"invalid_params":false,"expires":"1"}
            ],
            "global_expires":["7200"],
            "requested_seconds":3600
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(payload(response)["expires_seconds"], 600);

        let revoked = dispatch_value(json!({
            "operation":"evaluate_register_lease",
            "contacts_present":true,
            "candidates":[{"compatible":true,"instance_match":false,"uri_match":true,"invalid_params":false,"expires":"0"}],
            "global_expires":["7200"],
            "requested_seconds":3600
        }));
        assert_eq!(revoked.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(payload(revoked)["code"], "binding_revoked");
    }

    #[test]
    fn register_lease_raw_headers_are_parsed_and_decided_in_rust() {
        let response = dispatch_value(json!({
            "operation":"evaluate_register_lease_headers",
            "request_contacts":[r#"<sip:subscriber@[2001:db8::10]:5063;transport=tcp>;+sip.instance="<urn:uuid:own>";expires=7200"#],
            "contacts":[r#"<sip:rewritten@registrar.example>;+sip.instance="<urn:uuid:own>";expires=600,<sip:other@ims.example>;+sip.instance="<urn:uuid:other>";expires=10"#],
            "global_expires":["7200"],
            "requested_seconds":3600
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(payload(response)["expires_seconds"], 600);

        let invalid = dispatch_value(json!({
            "operation":"evaluate_register_lease_headers",
            "request_contacts":["<sip:subscriber@ims.example>"],
            "contacts":["<sip:subscriber@ims.example>;expires=600;Expires=0"],
            "global_expires":[],
            "requested_seconds":3600
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(payload(invalid)["code"], "contact_invalid");
    }

    #[test]
    fn register_expiry_operation_preserves_legacy_precedence() {
        for (configured, template, expected) in [
            (7200, 600, 7200),
            (0, 600, 600),
            (0, 0, 3600),
            (-1, -1, 3600),
        ] {
            let response = dispatch_value(json!({
                "operation":"effective_register_expiry",
                "configured_seconds":configured,
                "template_seconds":template
            }));
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(payload(response)["expires_seconds"], expected);
        }
        let invalid = dispatch_value(json!({"operation":"effective_register_expiry"}));
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn aka_challenge_parser_operation_returns_typed_digest_fields() {
        let response = dispatch_value(json!({
            "operation":"parse_aka_challenge",
            "header":r#"Digest realm="ims,home",nonce="opaque-nonce",algorithm=AKAv1-MD5,qop="auth-int,auth",userhash=true"#
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(result["realm"], "ims,home");
        assert_eq!(result["nonce"], "opaque-nonce");
        assert_eq!(result["algorithm"], "AKAv1-MD5");
        assert_eq!(result["qop"], json!(["auth-int", "auth"]));
        assert_eq!(result["userhash"], true);
    }

    #[test]
    fn aka_challenge_parser_operation_rejects_duplicate_parameters() {
        let response = dispatch_value(json!({
            "operation":"parse_aka_challenge",
            "header":"Digest nonce=x,Nonce=y"
        }));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn aka_challenge_selection_preserves_first_mode_and_scores_all_digest_candidates() {
        let first = dispatch_value(json!({
            "operation":"select_aka_challenge", "status":401, "mode":"first",
            "realm":"ims.example", "www_authenticate":[
                "Digest realm=\"other\",nonce=\"short\",algorithm=MD5"
            ]
        }));
        assert_eq!(first.status(), StatusCode::OK);
        let first = payload(first);
        assert_eq!(first["realm"], "other");
        assert_eq!(first["algorithm"], "MD5");

        let aka_nonce = "07".repeat(32);
        let header = format!(
            "Digest realm=\"other\",nonce=\"short\",algorithm=MD5, Digest realm=\"ims.example\",nonce=\"{aka_nonce}\",algorithm=AKAv1-MD5"
        );
        let best = dispatch_value(json!({
            "operation":"select_aka_challenge", "status":401, "mode":"best",
            "realm":"ims.example", "www_authenticate":[header]
        }));
        assert_eq!(best.status(), StatusCode::OK);
        let best = payload(best);
        assert_eq!(best["realm"], "ims.example");
        assert_eq!(best["algorithm"], "AKAv1-MD5");
        assert_eq!(best["nonce_len"], 32);
        assert_eq!(best["score"], 160);
    }

    #[test]
    fn security_server_operation_selects_ipsec_and_preserves_exact_verify_echo() {
        let raw = concat!(
            "ipsec-3gpp;q=0.9;alg=hmac-sha-1-96;mod=trans;ealg=null,",
            "ipsec-3gpp;q=1.0;alg=hmac-sha-1-96;mod=trans;prot=esp;ealg=aes-cbc;",
            "spi-c=111060141;spi-s=117229022;port-c=6050;port-s=6060"
        );
        let response = dispatch_value(json!({
            "operation":"negotiate_security_server",
            "security_server":raw,
            "strict":true,
            "client_mechanisms":[{"algorithm":"hmac-sha-1-96","encryption":"aes-cbc","protocol":"esp","mode":"trans"}]
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(result["verify"], raw);
        assert_eq!(result["selected"]["mechanism"], "ipsec-3gpp");
        assert_eq!(result["selected"]["algorithm"], "hmac-sha-1-96");
        assert_eq!(result["selected"]["encryption"], "aes-cbc");
        assert_eq!(result["selected"]["preference_milli"], 1000);
        assert_eq!(result["selected"]["spi_c"], 111060141);
        assert_eq!(result["selected"]["port_s"], 6060);
    }

    #[test]
    fn security_server_operation_rejects_strict_mismatch_and_oversized_input() {
        let raw = "ipsec-3gpp;q=1;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c=1;spi-s=2;port-c=3;port-s=4";
        let mismatch = dispatch_value(json!({
            "operation":"negotiate_security_server",
            "security_server":raw,
            "strict":true,
            "client_mechanisms":[{"algorithm":"hmac-sha-256-128","encryption":"aes-cbc"}]
        }));
        assert_eq!(mismatch.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let oversized = dispatch_value(json!({
            "operation":"negotiate_security_server",
            "security_server":"x".repeat(8193),
            "client_mechanisms":[]
        }));
        assert_eq!(oversized.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn security_client_operation_returns_ordered_legacy_header() {
        let response = dispatch_value(json!({
            "operation":"build_security_client",
            "spi_c":111,
            "spi_s":222,
            "port_c":5064,
            "port_s":5063,
            "client_mechanisms":[
                {"algorithm":"hmac-sha-1-96","encryption":"aes-cbc","protocol":"esp","mode":"trans"},
                {"algorithm":"hmac-md5-96","encryption":"","protocol":null,"mode":null}
            ]
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(
            result["security_client"],
            "ipsec-3gpp; alg=hmac-sha-1-96; ealg=aes-cbc; prot=esp; mod=trans; spi-c=111; spi-s=222; port-c=5064; port-s=5063,ipsec-3gpp; alg=hmac-md5-96; ealg=null; spi-c=111; spi-s=222; port-c=5064; port-s=5063"
        );
    }

    #[test]
    fn contact_operation_preserves_default_and_sms_only_compatibility() {
        let response = dispatch_value(json!({
            "operation":"build_contact",
            "template":{
                "transport":"tcp","contact_mode":"android_default","contact_features":"",
                "contact_param_order":null,"access_type":"IEEE-802.11",
                "icsi_ref":"urn:urn-7:3gpp-service.ims.icsi.mmtel","expires":3600,
                "force_header_port_5060":false
            },
            "input":{
                "imsi":"310260123456789","public_uri":"sip:310260123456789@ims.example.test",
                "local_ip":"2001:db8::20","local_port":5070,"sip_instance_urn":"urn:uuid:test",
                "register_expiry_secs":0,"sms_only":false
            }
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            payload(response)["contact"],
            "<sip:310260123456789@[2001:db8::20]:5070;transport=tcp>;+g.3gpp.accesstype=\"IEEE-802.11\";audio;+g.3gpp.smsip;+g.3gpp.icsi-ref=\"urn%3Aurn-7%3A3gpp-service.ims.icsi.mmtel\";+sip.instance=\"<urn:uuid:test>\";expires=3600"
        );
    }

    #[test]
    fn initial_register_fallback_operation_preserves_environment_and_status_policy() {
        let response = dispatch_value(json!({
            "operation":"initial_register_fallback",
            "initial_auth":" eap_direct ",
            "include_pani":false,
            "include_cellular":false,
            "configured_enabled":false,
            "environment_enabled":true,
            "status_codes":null,
            "status_code":403
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["retry"], true);
        assert_eq!(result["variants"].as_array().unwrap().len(), 5);
        assert_eq!(result["variants"][0]["initial_auth"], "eap_direct");
        assert_eq!(result["variants"][4]["initial_auth"], "none");

        let explicit_empty = dispatch_value(json!({
            "operation":"initial_register_fallback",
            "configured_enabled":true,
            "status_codes":[],
            "status_code":403
        }));
        assert_eq!(explicit_empty.status(), StatusCode::OK);
        assert_eq!(payload(explicit_empty)["retry"], false);
    }

    #[test]
    fn initial_register_fallback_operation_rejects_invalid_status_and_limits() {
        let invalid_status = dispatch_value(json!({
            "operation":"initial_register_fallback", "configured_enabled":true,
            "status_code":700
        }));
        assert_eq!(invalid_status.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let too_many = dispatch_value(json!({
            "operation":"initial_register_fallback", "status_codes":vec![403;65]
        }));
        assert_eq!(too_many.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn aka_challenge_selection_does_not_split_digest_text_inside_quotes() {
        let header = r#"Digest realm="ims",nonce="opaque, Digest impostor",algorithm=AKAv1-MD5"#;
        let result = dispatch_value(json!({
            "operation":"select_aka_challenge", "status":401, "mode":"best",
            "realm":"ims", "www_authenticate":[header]
        }));
        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(payload(result)["nonce"], "opaque, Digest impostor");
    }

    #[test]
    fn aka_nonce_operation_returns_only_rand_and_autn() {
        let nonce = format!(
            "{}",
            (0..35).map(|i| format!("{:02x}", i)).collect::<String>()
        );
        let response = dispatch_value(json!({"operation":"decode_aka_nonce", "nonce":nonce}));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(result["rand"], json!((0..16).collect::<Vec<u8>>()));
        assert_eq!(result["autn"], json!((16..32).collect::<Vec<u8>>()));

        let invalid = dispatch_value(json!({"operation":"decode_aka_nonce", "nonce":"bad"}));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn aka_digest_operation_uses_rust_for_raw_res_and_userhash() {
        let response = dispatch_value(json!({
            "operation":"aka_digest", "algorithm":"AKAv1-MD5",
            "username":"user@example.test", "realm":"ims.example.test",
            "nonce":"0101010101010101010101010101010101010101010101010101010101010101",
            "method":"REGISTER", "uri":"sip:ims.example.test", "opaque":"", "qop_offers":[],
            "userhash":false,
            "res":[222,173,0,190,239], "auts":[], "sync_failure":false
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["response"], "daa545a7774d3ac56e434f9bdd42beea");
        assert_eq!(result["username"], "user@example.test");
        assert_eq!(
            result["authorization"],
            "Digest username=\"user@example.test\", realm=\"ims.example.test\", nonce=\"0101010101010101010101010101010101010101010101010101010101010101\", uri=\"sip:ims.example.test\", algorithm=AKAv1-MD5, response=\"daa545a7774d3ac56e434f9bdd42beea\""
        );
        assert!(result["auts"].is_null());

        let sync = dispatch_value(json!({
            "operation":"aka_digest", "algorithm":"AKAv1-MD5",
            "username":"user@example.test", "realm":"ims.example.test",
            "nonce":"0101010101010101010101010101010101010101010101010101010101010101",
            "method":"REGISTER", "uri":"sip:ims.example.test", "opaque":"opaque-value", "qop_offers":["auth-int","auth"],
            "userhash":true,
            "res":[], "auts":[90,90,90,90,90,90,90,90,90,90,90,90,90,90], "sync_failure":true
        }));
        assert_eq!(sync.status(), StatusCode::OK);
        let sync = payload(sync);
        assert_eq!(sync["qop"], "auth");
        assert_eq!(sync["nonce_count"], 1);
        let cnonce = sync["cnonce"].as_str().unwrap();
        assert_eq!(cnonce.len(), 16);
        assert!(
            cnonce
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_eq!(sync["auts"], "WlpaWlpaWlpaWlpaWlo=");
        let mut hash = md5::Md5::new();
        use md5::Digest as _;
        hash.update(b"user@example.test:ims.example.test");
        let userhash = format!("{:x}", hash.finalize());
        assert_eq!(sync["username"], userhash);
        assert_eq!(
            sync["authorization"],
            format!(
                "Digest username=\"{userhash}\", realm=\"ims.example.test\", nonce=\"0101010101010101010101010101010101010101010101010101010101010101\", uri=\"sip:ims.example.test\", algorithm=AKAv1-MD5, cnonce=\"{cnonce}\", opaque=\"opaque-value\", qop=auth, nc=00000001, userhash=true, response=\"{}\", auts=\"WlpaWlpaWlpaWlpaWlo=\"",
                sync["response"].as_str().unwrap()
            )
        );

        let auth_int = dispatch_value(json!({
            "operation":"aka_digest", "algorithm":"AKAv1-MD5",
            "username":"user@example.test", "realm":"ims.example.test",
            "nonce":"0101010101010101010101010101010101010101010101010101010101010101",
            "method":"REGISTER", "uri":"sip:ims.example.test", "opaque":"", "qop_offers":["auth-int"],
            "userhash":false, "res":[222,173,0,190,239], "auts":[], "sync_failure":false
        }));
        assert_eq!(auth_int.status(), StatusCode::OK);
        let auth_int = payload(auth_int);
        assert_eq!(auth_int["qop"], "auth-int");
        assert_eq!(auth_int["nonce_count"], 1);
        assert_eq!(auth_int["cnonce"].as_str().unwrap().len(), 16);
        assert!(
            auth_int["authorization"]
                .as_str()
                .unwrap()
                .contains(", qop=auth-int, nc=00000001, response=")
        );
    }

    #[test]
    fn eap_direct_initial_authorization_operation_matches_wire_contract() {
        let response = dispatch_value(json!({
            "operation":"eap_direct_initial_authorization",
            "username":"private@ims.example.test", "domain":"ims.example.test", "realm":"ims.example.test",
            "res":[0,128,255,1,34,51,68,85], "rand":(0..16).collect::<Vec<u8>>(), "autn":(16..32).collect::<Vec<u8>>()
        }));
        assert_eq!(response.status(), StatusCode::OK);
        let result = payload(response);
        assert_eq!(result["status"], 200);
        assert_eq!(
            result["header"],
            "Digest username=\"private@ims.example.test\",realm=\"ims.example.test\",nonce=\"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=\",uri=\"sip:ims.example.test\",response=\"b8597f4181552744727b69351857e1b5\",algorithm=AKAv1-MD5"
        );
        let invalid = dispatch_value(
            json!({"operation":"eap_direct_initial_authorization","username":"u","domain":"d","realm":"r","res":[],"rand":[],"autn":[]}),
        );
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn register_retry_delay_operation_preserves_default_and_template_rules() {
        let default = dispatch_value(json!({
            "operation":"resolve_register_retry_delay", "status_code":503,
            "temporary_status_codes":null, "temporary_retry_seconds":11
        }));
        assert_eq!(default.status(), StatusCode::OK);
        assert_eq!(payload(default)["retry_after_ms"], 11_000);

        let disabled = dispatch_value(json!({
            "operation":"resolve_register_retry_delay", "status_code":503,
            "temporary_status_codes":[], "temporary_retry_seconds":11
        }));
        assert_eq!(payload(disabled)["retry_after_ms"], 150);

        let custom = dispatch_value(json!({
            "operation":"resolve_register_retry_delay", "status_code":429,
            "temporary_status_codes":[429], "temporary_retry_seconds":3
        }));
        assert_eq!(payload(custom)["retry_after_ms"], 3_000);

        let invalid = dispatch_value(json!({
            "operation":"resolve_register_retry_delay", "status_code":700,
            "temporary_status_codes":null, "temporary_retry_seconds":1
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn register_candidate_retry_operation_preserves_probe_and_status_policy() {
        let default_forbidden = dispatch_value(json!({
            "operation":"resolve_register_candidate_retry", "status_code":403,
            "failure_kind":"", "has_more":true,
            "forbidden_status_codes":null, "temporary_status_codes":null
        }));
        assert_eq!(default_forbidden.status(), StatusCode::OK);
        assert_eq!(payload(default_forbidden)["advance"], true);

        let custom_temporary = dispatch_value(json!({
            "operation":"resolve_register_candidate_retry", "status_code":429,
            "failure_kind":"", "has_more":true,
            "forbidden_status_codes":[], "temporary_status_codes":[429]
        }));
        assert_eq!(payload(custom_temporary)["advance"], true);

        let probe_timeout = dispatch_value(json!({
            "operation":"resolve_register_candidate_retry", "status_code":0,
            "failure_kind":"timeout_text", "has_more":true,
            "forbidden_status_codes":null, "temporary_status_codes":null
        }));
        assert_eq!(payload(probe_timeout)["advance"], true);

        let terminal_candidate = dispatch_value(json!({
            "operation":"resolve_register_candidate_retry", "status_code":503,
            "failure_kind":"", "has_more":false,
            "forbidden_status_codes":null, "temporary_status_codes":null
        }));
        assert_eq!(payload(terminal_candidate)["advance"], false);

        let invalid = dispatch_value(json!({
            "operation":"resolve_register_candidate_retry", "status_code":503,
            "failure_kind":"arbitrary", "has_more":true,
            "forbidden_status_codes":null, "temporary_status_codes":null
        }));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    async fn body(response: Response<Full<Bytes>>) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn request(raw: &str, call_id: &str, cseq: u32) -> ValidateRequest {
        ValidateRequest {
            response: raw.as_bytes().to_vec(),
            register_request: Vec::new(),
            call_id: call_id.into(),
            cseq,
            method: default_method(),
            operation: default_operation(),
            session_id: String::new(),
            generation: None,
        }
    }

    fn options_request(
        operation: &str,
        session_id: &str,
        call_id: &str,
        cseq: u32,
    ) -> ValidateRequest {
        ValidateRequest {
            response: Vec::new(),
            register_request: Vec::new(),
            call_id: call_id.into(),
            cseq,
            method: "OPTIONS".into(),
            operation: operation.into(),
            session_id: session_id.into(),
            generation: None,
        }
    }

    async fn generation(response: Response<Full<Bytes>>) -> u64 {
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["generation"]
            .as_u64()
            .unwrap()
    }

    #[tokio::test]
    async fn validates_matching_aka_challenge() {
        let raw = "SIP/2.0 401 Unauthorized\r\nCall-ID: reg-1\r\nCSeq: 7 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims\",nonce=\"redacted\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc\r\nContent-Length: 0\r\n\r\n";
        let response = validate(request(raw, "reg-1", 7));
        assert_eq!(response.status(), StatusCode::OK);
        let payload = body(response).await;
        assert_eq!(payload["aka_challenge"], true);
        assert_eq!(payload["register_action"], "challenge");
    }

    #[tokio::test]
    async fn omitted_method_remains_register_compatible() {
        let raw = "SIP/2.0 200 OK\r\nCall-ID: reg-legacy\r\nCSeq: 7 REGISTER\r\nContent-Length: 0\r\n\r\n";
        let input = serde_json::from_value(json!({
            "response": raw.as_bytes().to_vec(),
            "call_id": "reg-legacy",
            "cseq": 7
        }))
        .unwrap();
        let response = validate(input);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await["register_action"], "accepted");
    }

    #[tokio::test]
    async fn register_status_action_is_classified_after_transaction_validation() {
        for (status, reason, expected) in [
            (200, "OK", "accepted"),
            (401, "Unauthorized", "challenge"),
            (407, "Proxy Authentication Required", "challenge"),
            (403, "Forbidden", "rejected"),
            (503, "Service Unavailable", "rejected"),
        ] {
            let raw = format!(
                "SIP/2.0 {status} {reason}\r\nCall-ID: reg-action\r\nCSeq: 9 REGISTER\r\nContent-Length: 0\r\n\r\n"
            );
            let response = validate(request(&raw, "reg-action", 9));
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(body(response).await["register_action"], expected);
        }

        let mismatched =
            "SIP/2.0 200 OK\r\nCall-ID: foreign\r\nCSeq: 9 REGISTER\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            validate(request(mismatched, "reg-action", 9)).status(),
            StatusCode::CONFLICT,
            "Rust must not classify a response before matching its SIP transaction"
        );
    }

    #[tokio::test]
    async fn register_request_bytes_are_the_identity_authority() {
        let request_wire = b"REGISTER sip:ims.example SIP/2.0\r\nCall-ID: request-owner\r\nCSeq: 12 REGISTER\r\nContent-Length: 0\r\n\r\n";
        let response_wire = b"SIP/2.0 401 Unauthorized\r\nCall-ID: request-owner\r\nCSeq: 12 REGISTER\r\nContent-Length: 0\r\n\r\n";
        let mut input = request(
            std::str::from_utf8(response_wire).unwrap(),
            "untrusted-caller-field",
            99,
        );
        input.register_request = request_wire.to_vec();
        let result = validate(input);
        assert_eq!(result.status(), StatusCode::OK);
        let summary = body(result).await;
        assert_eq!(summary["call_id"], "request-owner");
        assert_eq!(summary["cseq"], 12);
        assert_eq!(summary["register_action"], "challenge");

        let mut invalid = request(
            std::str::from_utf8(response_wire).unwrap(),
            "request-owner",
            12,
        );
        invalid.register_request = b"REGISTER sip:ims.example SIP/2.0\r\nCall-ID: request-owner\r\nCSeq: 0 REGISTER\r\nContent-Length: 0\r\n\r\n".to_vec();
        assert_eq!(validate(invalid).status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn rejects_wrong_transaction_and_smuggled_body() {
        let raw =
            "SIP/2.0 200 OK\r\nCall-ID: other\r\nCSeq: 7 REGISTER\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            validate(request(raw, "reg-1", 7)).status(),
            StatusCode::CONFLICT
        );
        let smuggled =
            "SIP/2.0 200 OK\r\nCall-ID: reg-1\r\nCSeq: 7 REGISTER\r\nContent-Length: 0\r\n\r\nx";
        assert_eq!(
            validate(request(smuggled, "reg-1", 7)).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn validates_message_transaction_and_rejects_mismatched_method_or_cseq() {
        let raw = "SIP/2.0 202 Accepted\r\nCall-ID: sms-1\r\nCSeq: 9 MESSAGE\r\nContent-Length: 0\r\n\r\n";
        let mut input = request(raw, "sms-1", 9);
        input.method = "MESSAGE".into();
        let response = validate(input);
        assert_eq!(response.status(), StatusCode::OK);

        let mut wrong_cseq = request(raw, "sms-1", 10);
        wrong_cseq.method = "MESSAGE".into();
        let response = validate(wrong_cseq);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let payload = body(response).await;
        assert_eq!(payload["code"], "unexpected_message_transaction");
        assert_eq!(payload["error"], "IMS MESSAGE 响应校验失败");

        let mut wrong_method = request(raw, "sms-1", 9);
        wrong_method.method = "REGISTER".into();
        let response = validate(wrong_method);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body(response).await["error"], "IMS REGISTER 响应校验失败");
    }

    #[tokio::test]
    async fn validates_options_heartbeat_transaction_before_status_is_trusted() {
        let raw =
            "SIP/2.0 200 OK\r\nCall-ID: options-1\r\nCSeq: 17 OPTIONS\r\nContent-Length: 0\r\n\r\n";
        let mut input = request(raw, "options-1", 17);
        input.method = "OPTIONS".into();
        let response = validate(input);
        assert_eq!(response.status(), StatusCode::OK);
        let payload = body(response).await;
        assert_eq!(payload["status"], 200);
        assert_eq!(payload["method"], "OPTIONS");
        assert_eq!(payload["call_id"], "options-1");
        assert_eq!(payload["cseq"], 17);

        let mut wrong_cseq = request(raw, "options-1", 18);
        wrong_cseq.method = "OPTIONS".into();
        let response = validate(wrong_cseq);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let payload = body(response).await;
        assert_eq!(payload["code"], "unexpected_options_transaction");
        assert_eq!(payload["error"], "IMS OPTIONS 响应校验失败");

        let mut wrong_call_id = request(raw, "other-options", 17);
        wrong_call_id.method = "OPTIONS".into();
        let response = validate(wrong_call_id);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body(response).await["code"],
            "unexpected_options_transaction"
        );

        let mut wrong_method = request(raw, "options-1", 17);
        wrong_method.method = "MESSAGE".into();
        let response = validate(wrong_method);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body(response).await["code"], "invalid_message_transaction");
    }

    #[tokio::test]
    async fn options_lease_is_single_use_and_rejects_old_generation_without_consuming_newer_one() {
        let session = format!("options-generation-{}", std::process::id());
        let first = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "options-old",
            17,
        )))
        .await;
        let second = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "options-new",
            18,
        )))
        .await;
        assert!(second > first);

        let raw_old = "SIP/2.0 200 OK\r\nCall-ID: options-old\r\nCSeq: 17 OPTIONS\r\nContent-Length: 0\r\n\r\n";
        let mut stale = options_request("complete", &session, "options-old", 17);
        stale.generation = Some(first);
        stale.response = raw_old.as_bytes().to_vec();
        let rejected = complete_options_lease(stale);
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(body(rejected).await["code"], "stale_options_generation");

        let mut mismatched_call = options_request("complete", &session, "wrong-call-id", 18);
        mismatched_call.generation = Some(second);
        mismatched_call.response = b"SIP/2.0 200 OK\r\nCall-ID: wrong-call-id\r\nCSeq: 18 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();
        let rejected = complete_options_lease(mismatched_call);
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(
            body(rejected).await["code"],
            "unexpected_options_transaction"
        );

        let raw_new = "SIP/2.0 200 OK\r\nCall-ID: options-new\r\nCSeq: 18 OPTIONS\r\nContent-Length: 0\r\n\r\n";
        let mut current = options_request("complete", &session, "options-new", 18);
        current.generation = Some(second);
        current.response = raw_new.as_bytes().to_vec();
        assert_eq!(
            complete_options_lease(current.clone()).status(),
            StatusCode::OK
        );

        let duplicate = complete_options_lease(current);
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        assert_eq!(
            body(duplicate).await["code"],
            "options_lease_missing_or_expired"
        );
    }

    #[tokio::test]
    async fn options_provisional_observation_keeps_lease_open_until_final_response() {
        let session = format!("options-observe-{}", std::process::id());
        let generation = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "observe-call",
            31,
        )))
        .await;
        let mut provisional = options_request("observe", &session, "observe-call", 31);
        provisional.generation = Some(generation);
        provisional.response = b"SIP/2.0 180 Ringing\r\nCall-ID: observe-call\r\nCSeq: 31 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();
        let pending = complete_options_lease(provisional);
        assert_eq!(pending.status(), StatusCode::OK);
        let pending = body(pending).await;
        assert_eq!(pending["status"], 180);
        assert_eq!(pending["final"], false);
        assert!(
            options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&session)
        );

        let mut final_response = options_request("observe", &session, "observe-call", 31);
        final_response.generation = Some(generation);
        final_response.response = b"SIP/2.0 200 OK\r\nCall-ID: observe-call\r\nCSeq: 31 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();
        let completed = complete_options_lease(final_response);
        assert_eq!(completed.status(), StatusCode::OK);
        let completed = body(completed).await;
        assert_eq!(completed["status"], 200);
        assert_eq!(completed["final"], true);
        assert!(
            !options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&session)
        );
    }

    #[tokio::test]
    async fn options_mismatched_provisional_response_cannot_keep_lease_open() {
        let session = format!("options-observe-mismatch-{}", std::process::id());
        let generation = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "expected-call",
            41,
        )))
        .await;
        let mut provisional = options_request("observe", &session, "expected-call", 41);
        provisional.generation = Some(generation);
        provisional.response = b"SIP/2.0 180 Ringing\r\nCall-ID: expected-call\r\nCSeq: 42 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();

        let rejected = complete_options_lease(provisional);
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert!(
            !options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&session)
        );
    }

    #[tokio::test]
    async fn message_response_observer_owns_transaction_matching_and_final_status() {
        async fn observe(raw: &str, call_id: &str, cseq: u32) -> serde_json::Value {
            let response = dispatch_value(json!({
                "operation": "observe",
                "method": "MESSAGE",
                "call_id": call_id,
                "cseq": cseq,
                "response": raw.as_bytes()
            }));
            assert_eq!(response.status(), StatusCode::OK);
            body(response).await
        }

        let provisional = observe(
            "SIP/2.0 100 Trying\r\nCall-ID: msg-1\r\nCSeq: 8 MESSAGE\r\nContent-Length: 0\r\n\r\n",
            "msg-1",
            8,
        )
        .await;
        assert_eq!(provisional["disposition"], "provisional");
        assert_eq!(provisional["matched"], true);
        assert_eq!(provisional["final"], false);

        let unrelated = observe(
            "SIP/2.0 202 Accepted\r\nCall-ID: msg-1\r\nCSeq: 9 OPTIONS\r\nContent-Length: 0\r\n\r\n",
            "msg-1",
            8,
        )
        .await;
        assert_eq!(unrelated["disposition"], "ignored");
        assert_eq!(unrelated["matched"], false);
        assert_eq!(unrelated["final"], false);

        let accepted = observe(
            "SIP/2.0 202 Accepted\r\nCall-ID: msg-1\r\nCSeq: 8 MESSAGE\r\nContent-Length: 0\r\n\r\n",
            "msg-1",
            8,
        )
        .await;
        assert_eq!(accepted["disposition"], "accepted");
        assert_eq!(accepted["matched"], true);
        assert_eq!(accepted["final"], true);

        let rejected = observe(
            "SIP/2.0 403 Forbidden\r\nCall-ID: msg-1\r\nCSeq: 8 MESSAGE\r\nContent-Length: 0\r\n\r\n",
            "msg-1",
            8,
        )
        .await;
        assert_eq!(rejected["disposition"], "rejected");
        assert_eq!(rejected["status"], 403);
        assert_eq!(rejected["final"], true);
    }

    #[tokio::test]
    async fn options_lease_rejects_expiry_and_abort_is_generation_scoped() {
        let session = format!("options-expiry-{}", std::process::id());
        let first = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "options-expired",
            4,
        )))
        .await;
        {
            let mut store = options_leases().lock().unwrap();
            store.sessions.get_mut(&session).unwrap().expires_at =
                Instant::now() - Duration::from_millis(1);
        }
        let mut expired = options_request("complete", &session, "options-expired", 4);
        expired.generation = Some(first);
        expired.response = b"SIP/2.0 200 OK\r\nCall-ID: options-expired\r\nCSeq: 4 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();
        let rejected = complete_options_lease(expired);
        let rejected_body = body(rejected).await;
        // Another concurrent begin may prune expired leases before this
        // completion check, so both explicit expiry and missing/expired are
        // valid fail-closed outcomes.
        assert!(
            matches!(
                rejected_body["code"].as_str(),
                Some("options_lease_expired" | "options_lease_missing_or_expired")
            ),
            "unexpected expired-lease response: {rejected_body}"
        );

        let current = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "options-current",
            5,
        )))
        .await;
        let mut stale_abort = options_request("abort", &session, "options-old", 4);
        stale_abort.generation = Some(first);
        assert_eq!(abort_options_lease(stale_abort).status(), StatusCode::OK);
        {
            let store = options_leases().lock().unwrap();
            assert_eq!(store.sessions.get(&session).unwrap().generation, current);
        }
        let mut mismatched_abort = options_request("abort", &session, "other-call", 5);
        mismatched_abort.generation = Some(current);
        assert_eq!(
            abort_options_lease(mismatched_abort).status(),
            StatusCode::OK
        );
        assert!(
            options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&session)
        );
        let mut abort = options_request("abort", &session, "options-current", 5);
        abort.generation = Some(current);
        assert_eq!(abort_options_lease(abort).status(), StatusCode::OK);
        assert!(
            !options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&session)
        );
    }

    #[tokio::test]
    async fn matching_malformed_final_response_is_terminal_and_consumes_its_lease() {
        let session = format!("options-malformed-{}", std::process::id());
        let generation = generation(begin_options_lease(options_request(
            "begin",
            &session,
            "options-malformed",
            22,
        )))
        .await;
        let mut malformed = options_request("complete", &session, "options-malformed", 22);
        malformed.generation = Some(generation);
        malformed.response = b"SIP/2.0 200 OK\r\nCall-ID: options-malformed\r\nCSeq: 22 OPTIONS\r\nContent-Length: 0\r\n\r\nbody".to_vec();
        let rejected = complete_options_lease(malformed.clone());
        assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body(rejected).await["code"], "invalid_sip_response");

        malformed.response = b"SIP/2.0 200 OK\r\nCall-ID: options-malformed\r\nCSeq: 22 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();
        let duplicate = complete_options_lease(malformed);
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        assert_eq!(
            body(duplicate).await["code"],
            "options_lease_missing_or_expired"
        );
    }

    #[tokio::test]
    async fn options_lease_sessions_are_isolated_and_cseq_mismatch_consumes_only_its_lease() {
        let suffix = std::process::id();
        let left = format!("options-left-{suffix}");
        let right = format!("options-right-{suffix}");
        let left_generation = generation(begin_options_lease(options_request(
            "begin",
            &left,
            "left-call",
            9,
        )))
        .await;
        let right_generation = generation(begin_options_lease(options_request(
            "begin",
            &right,
            "right-call",
            11,
        )))
        .await;

        let mut wrong_sequence = options_request("complete", &left, "left-call", 10);
        wrong_sequence.generation = Some(left_generation);
        wrong_sequence.response = b"SIP/2.0 200 OK\r\nCall-ID: left-call\r\nCSeq: 10 OPTIONS\r\nContent-Length: 0\r\n\r\n".to_vec();
        let rejected = complete_options_lease(wrong_sequence);
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        assert_eq!(
            body(rejected).await["code"],
            "unexpected_options_transaction"
        );
        assert!(
            options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&left)
        );

        let mut right = options_request("abort", &right, "right-call", 11);
        right.generation = Some(right_generation);
        assert_eq!(abort_options_lease(right).status(), StatusCode::OK);
        assert!(
            options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&left)
        );
        let mut left_abort = options_request("abort", &left, "left-call", 9);
        left_abort.generation = Some(left_generation);
        assert_eq!(abort_options_lease(left_abort).status(), StatusCode::OK);
        assert!(
            !options_leases()
                .lock()
                .unwrap()
                .sessions
                .contains_key(&left)
        );
    }
}
