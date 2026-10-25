//! One Rust execution boundary for every public call mutation.
//!
//! The GUI supplies an idle-call preference or a receipt from a Rust call
//! projection. It never chooses the physical API. Rust checks live owners,
//! rejects conflicting receipts, and forwards exactly once.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

const BODY_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Module,
    Reader,
    ModuleVoWiFi,
}

impl Owner {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "module" => Some(Self::Module),
            "reader" => Some(Self::Reader),
            "module-vowifi" => Some(Self::ModuleVoWiFi),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Reader => "reader",
            Self::ModuleVoWiFi => "module-vowifi",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observation {
    Idle,
    Active { owner: Owner, generation: u64 },
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VoWiFiObservation {
    call: Observation,
    /// The registered session binding is present even while no call is active.
    /// Capturing it before ledger admission prevents an idle dial with a zero
    /// generation from drifting to a replacement card/session after restart.
    binding: Option<(Owner, u64)>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    #[serde(default)]
    client_request_id: String,
    #[serde(default)]
    number: Option<String>,
    #[serde(default)]
    peer: Option<String>,
    #[serde(default)]
    digit: Option<String>,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    preferred_owner: Option<String>,
    #[serde(default)]
    observed_owner: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    cancel: bool,
}

pub(crate) fn is_mutation(method: &Method, path: &str) -> bool {
    mutation_action(method, path).is_some()
}

fn mutation_action(method: &Method, path: &str) -> Option<&'static str> {
    if *method != Method::POST {
        return None;
    }
    match path {
        "/api/call/dial" | "/api/vowifi/calls/dial" | "/api/reader/vowifi/calls/dial" => {
            Some("dial")
        }
        "/api/call/answer" | "/api/vowifi/calls/answer" | "/api/reader/vowifi/calls/answer" => {
            Some("answer")
        }
        "/api/vowifi/calls/reject" | "/api/reader/vowifi/calls/reject" => Some("reject"),
        "/api/call/hangup" | "/api/vowifi/calls/hangup" | "/api/reader/vowifi/calls/hangup" => {
            Some("hangup")
        }
        "/api/call/dtmf" | "/api/vowifi/calls/dtmf" | "/api/reader/vowifi/calls/dtmf" => {
            Some("dtmf")
        }
        "/api/reader/vowifi/ussd" => Some("ussd"),
        _ => None,
    }
}

fn is_vowifi_alias(path: &str) -> bool {
    path.starts_with("/api/vowifi/calls/")
        || path.starts_with("/api/reader/vowifi/calls/")
        || path == "/api/reader/vowifi/ussd"
}

fn selected_vowifi_owner(state: &crate::ServerState, path: &str) -> Owner {
    if state
        .native_vowifi_lifecycle
        .module_call_service(&Method::POST, path)
        .is_some()
    {
        Owner::ModuleVoWiFi
    } else {
        Owner::Reader
    }
}

fn valid_client_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn payload_fields_valid(action: &str, body: &[u8]) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(body) else {
        return body.is_empty();
    };
    let allowed = |field: &str| match action {
        "dial" => matches!(
            field,
            "client_request_id"
                | "number"
                | "peer"
                | "generation"
                | "preferred_owner"
                | "observed_owner"
        ),
        "answer" | "reject" => matches!(
            field,
            "client_request_id" | "generation" | "preferred_owner" | "observed_owner"
        ),
        "hangup" => matches!(
            field,
            "client_request_id" | "generation" | "preferred_owner" | "observed_owner" | "command"
        ),
        "dtmf" => matches!(
            field,
            "client_request_id" | "digit" | "generation" | "preferred_owner" | "observed_owner"
        ),
        "ussd" => matches!(
            field,
            "client_request_id"
                | "command"
                | "session_id"
                | "input"
                | "cancel"
                | "generation"
                | "preferred_owner"
                | "observed_owner"
        ),
        _ => false,
    };
    fields.keys().all(|field| allowed(field))
        && !(action == "dial" && fields.contains_key("number") && fields.contains_key("peer"))
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    let path = request.uri().path().to_owned();
    let Some(action) = mutation_action(request.method(), &path) else {
        return error(StatusCode::NOT_FOUND, "call_action_unknown", "未知通话命令");
    };
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        BODY_LIMIT,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "call_request_invalid",
                "通话请求无效",
            );
        }
    };
    let intent: Intent = if body.is_empty() {
        Intent::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(intent) => intent,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "call_request_invalid",
                    "通话请求无效",
                );
            }
        }
    };
    if !payload_fields_valid(action, &body) {
        return error(
            StatusCode::BAD_REQUEST,
            "call_request_invalid",
            "通话请求包含不适用于当前操作的字段",
        );
    }
    if !valid_client_request_id(&intent.client_request_id) {
        return error(
            StatusCode::BAD_REQUEST,
            "call_request_id_invalid",
            "通话请求编号无效",
        );
    }
    let fingerprint = crate::call_mutation_intent::fingerprint(&body);
    if let Some(reply) = crate::call_mutation_intent::preflight(
        state.multistandby_store.data_root(),
        &intent.client_request_id,
        action,
        &fingerprint,
    ) {
        return reply;
    }
    let requested_preferred = match parse_optional_owner(intent.preferred_owner.as_deref()) {
        Ok(owner) => owner,
        Err(reply) => return reply,
    };
    let observed = match parse_optional_owner(intent.observed_owner.as_deref()) {
        Ok(owner) => owner,
        Err(reply) => return reply,
    };
    let alias_owner = is_vowifi_alias(&path).then(|| selected_vowifi_owner(state, &path));
    if let (Some(alias_owner), Some(requested)) = (alias_owner, requested_preferred)
        && alias_owner != requested
    {
        return error(
            StatusCode::CONFLICT,
            "call_owner_changed",
            "VoWiFi 通话 owner 与请求不一致，请刷新状态后重试",
        );
    }
    let (owner, vowifi, vowifi_binding) = if action == "ussd" {
        if intent.generation == 0 {
            return error(
                StatusCode::BAD_REQUEST,
                "call_generation_required",
                "USSD 请求缺少当前会话 generation",
            );
        }
        (
            alias_owner.expect("USSD is a VoWiFi alias"),
            Observation::Idle,
            None,
        )
    } else {
        let module = observe_module(state).await;
        let vowifi_observation = observe_vowifi(state).await;
        let vowifi = vowifi_observation.call;
        let preferred = alias_owner.or(requested_preferred);
        let owner = match choose_owner(
            action,
            preferred,
            observed,
            intent.generation,
            module,
            vowifi,
        ) {
            Ok(owner) => owner,
            Err((status, code, message)) => return error(status, code, message),
        };
        if alias_owner.is_some() && owner == Owner::Module {
            return error(
                StatusCode::CONFLICT,
                "call_owner_changed",
                "VoWiFi 通话 owner 已变化，请刷新状态后重试",
            );
        }
        (owner, vowifi, vowifi_observation.binding)
    };
    let generation = match (owner, vowifi, vowifi_binding) {
        (
            Owner::Reader | Owner::ModuleVoWiFi,
            Observation::Active {
                owner: active_owner,
                generation,
            },
            _,
        ) if owner == active_owner => generation,
        (Owner::Reader | Owner::ModuleVoWiFi, _, Some((bound_owner, generation)))
            if owner == bound_owner =>
        {
            generation
        }
        (Owner::Reader | Owner::ModuleVoWiFi, _, _) => intent.generation,
        // The native module projection has no durable SIM + specific-call
        // generation. Zero explicitly means unbound, never a recoverable
        // identity: the ledger quarantines any existing module admission.
        (Owner::Module, _, _) => 0,
    };
    if matches!(owner, Owner::Reader | Owner::ModuleVoWiFi) {
        if generation == 0 {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "call_generation_unconfirmed",
                "VoWiFi 会话 generation 暂未确认，请刷新状态后重试",
            );
        }
        if intent.generation != 0 && intent.generation != generation {
            return error(
                StatusCode::CONFLICT,
                "call_generation_changed",
                "VoWiFi 会话 generation 已变化，请刷新状态后重试",
            );
        }
    }
    let request = crate::call_mutation_intent::MutationRequest {
        client_request_id: intent.client_request_id.clone(),
        action: action.to_owned(),
        raw_payload_fingerprint: fingerprint,
        owner: owner.as_str().to_owned(),
        generation,
    };
    crate::call_mutation_intent::execute(state.multistandby_store.data_root(), request, || async {
        match (action, owner) {
            ("ussd", Owner::Reader | Owner::ModuleVoWiFi) => {
                dispatch_ussd(owner, generation, &intent, state).await
            }
            (_, Owner::Module) => dispatch_module(action, &intent, state).await,
            (_, Owner::Reader | Owner::ModuleVoWiFi) => {
                dispatch_vowifi(action, owner, generation, &intent, state).await
            }
        }
    })
    .await
}

fn parse_optional_owner(value: Option<&str>) -> Result<Option<Owner>, Response<Full<Bytes>>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => Owner::parse(value).map(Some).ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "call_owner_invalid",
                "通话 owner 无效",
            )
        }),
    }
}

fn choose_owner(
    action: &str,
    preferred: Option<Owner>,
    observed: Option<Owner>,
    observed_generation: u64,
    module: Observation,
    vowifi: Observation,
) -> Result<Owner, (StatusCode, &'static str, &'static str)> {
    // A receipt identifies the last projection seen by the client; it cannot
    // prove that the other physical owner is idle now. Resolve both live
    // observations before considering a receipt, an active owner or an idle
    // dial preference. Otherwise a failed status read can admit a second call
    // on the selected device while the unobserved device already owns one.
    if module == Observation::Uncertain || vowifi == Observation::Uncertain {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "call_owner_unconfirmed",
            "通话 owner 暂未确认，请稍后重试",
        ));
    }
    let active_module = match module {
        Observation::Active { owner, generation } => Some((owner, generation)),
        _ => None,
    };
    let active_vowifi = match vowifi {
        Observation::Active { owner, generation } => Some((owner, generation)),
        _ => None,
    };
    if active_vowifi.is_some_and(|(_, generation)| generation == 0) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "call_owner_unconfirmed",
            "VoWiFi 通话 generation 暂未确认，请稍后重试",
        ));
    }
    if active_module.is_some() && active_vowifi.is_some() {
        return Err((
            StatusCode::CONFLICT,
            "call_owner_ambiguous",
            "检测到多个活动通话 owner，已拒绝跨设备命令",
        ));
    }
    let active = active_module.or(active_vowifi);
    if let (Some(observed), Some((active, _))) = (observed, active)
        && observed != active
    {
        return Err((
            StatusCode::CONFLICT,
            "call_owner_changed",
            "通话 owner 已变化，请刷新状态后重试",
        ));
    }
    if let (Some(observed), Some((active, generation))) = (observed, active)
        && observed == active
        && matches!(active, Owner::Reader | Owner::ModuleVoWiFi)
        && generation != 0
        && observed_generation != generation
    {
        return Err((
            StatusCode::CONFLICT,
            "call_generation_changed",
            "通话 generation 已变化，请刷新状态后重试",
        ));
    }
    if let Some((active, _)) = active {
        if preferred.is_some_and(|preferred| preferred != active) {
            return Err((
                StatusCode::CONFLICT,
                "call_owner_receipt_required",
                "活动通话 owner 与当前选择不一致，请刷新状态后重试",
            ));
        }
        return Ok(active);
    }
    if observed.is_some() {
        return Err((
            StatusCode::CONFLICT,
            "call_owner_changed",
            "活动通话已结束或 owner 已变化，请刷新状态后重试",
        ));
    }
    if action == "dial" {
        // An idle-call target is an explicit user preference. The selected
        // physical owner still performs its own busy/admission check.
        return Ok(preferred.unwrap_or(Owner::Module));
    }
    Err((
        StatusCode::CONFLICT,
        "call_owner_inactive",
        "当前没有可控制的活动通话",
    ))
}

async fn observe_module(state: &crate::ServerState) -> Observation {
    let reply = crate::call_control::handle_status(state.module_call()).await;
    if reply.status() != StatusCode::OK {
        return Observation::Uncertain;
    }
    let Some(value) = response_value(reply).await else {
        return Observation::Uncertain;
    };
    match value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "idle" => Observation::Idle,
        "incoming" | "ringing" | "dialing" | "alerting" | "connecting" | "active" => {
            Observation::Active {
                owner: Owner::Module,
                generation: 0,
            }
        }
        _ => Observation::Uncertain,
    }
}

async fn observe_vowifi(state: &crate::ServerState) -> VoWiFiObservation {
    let path = "/api/vowifi/calls/status";
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Full::new(Bytes::new()))
        .expect("static call status request");
    let reply = if let Some(service) = state
        .native_vowifi_lifecycle
        .module_call_service(&Method::GET, path)
    {
        service.calls(request).await
    } else if state
        .native_vowifi_lifecycle
        .handles_public_request(&Method::GET, path)
    {
        match state.native_vowifi_lifecycle.service(state) {
            Ok(service) => service.handle_public(request).await,
            Err(_) => {
                return VoWiFiObservation {
                    call: Observation::Uncertain,
                    binding: None,
                };
            }
        }
    } else {
        return VoWiFiObservation {
            call: Observation::Idle,
            binding: None,
        };
    };
    if reply.status() != StatusCode::OK {
        return VoWiFiObservation {
            call: Observation::Uncertain,
            binding: None,
        };
    }
    let Some(value) = response_value(reply).await else {
        return VoWiFiObservation {
            call: Observation::Uncertain,
            binding: None,
        };
    };
    decode_vowifi_observation(&value)
}

fn decode_vowifi_observation(value: &Value) -> VoWiFiObservation {
    let call = value.get("call").unwrap_or(&Value::Null);
    let state_name = call
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let owner = match value.get("source").and_then(Value::as_str) {
        Some("module") => Owner::ModuleVoWiFi,
        Some("reader") | None => Owner::Reader,
        _ => {
            return VoWiFiObservation {
                call: Observation::Uncertain,
                binding: None,
            };
        }
    };
    let generation = value.get("generation").and_then(Value::as_u64).unwrap_or(0);
    let binding = (generation != 0).then_some((owner, generation));
    if matches!(state_name, "idle" | "ended" | "failed" | "unavailable") {
        return VoWiFiObservation {
            call: Observation::Idle,
            binding,
        };
    }
    if !matches!(
        state_name,
        "incoming" | "outgoing" | "early" | "connecting" | "active"
    ) {
        return VoWiFiObservation {
            call: Observation::Uncertain,
            binding: None,
        };
    }
    VoWiFiObservation {
        call: Observation::Active { owner, generation },
        binding,
    }
}

async fn dispatch_module(
    action: &str,
    intent: &Intent,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    if state.availability.module_recovery().is_quarantined() {
        return crate::module_recovery::rejection();
    }
    let backend = state.module_call();
    match action {
        "dial" => {
            let number = intent
                .number
                .as_deref()
                .or(intent.peer.as_deref())
                .unwrap_or_default();
            crate::call_control::handle_dial(json_request(json!({"number":number})), backend).await
        }
        "answer" => crate::call_control::handle_answer(empty_request(), backend).await,
        "reject" => crate::call_control::handle_hangup(empty_request(), backend).await,
        "hangup" => crate::call_control::handle_hangup(empty_request(), backend).await,
        "dtmf" => {
            crate::call_control::handle_dtmf(
                json_request(json!({"digit":intent.digit.as_deref().unwrap_or_default()})),
                backend,
            )
            .await
        }
        _ => error(StatusCode::NOT_FOUND, "call_action_unknown", "未知通话命令"),
    }
}

async fn dispatch_ussd(
    owner: Owner,
    generation: u64,
    intent: &Intent,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    let path = "/api/reader/vowifi/ussd";
    let body = json!({
        "command": intent.command.as_deref().unwrap_or_default(),
        "session_id": intent.session_id.as_deref().unwrap_or_default(),
        "input": intent.input.as_deref().unwrap_or_default(),
        "cancel": intent.cancel,
        "generation": generation,
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
        .expect("validated USSD request");
    match owner {
        Owner::ModuleVoWiFi => {
            let Some(service) = state
                .native_vowifi_lifecycle
                .module_call_service(&Method::POST, path)
            else {
                return error(
                    StatusCode::CONFLICT,
                    "call_owner_changed",
                    "模块 VoWiFi owner 已变化，请刷新状态后重试",
                );
            };
            service.calls(request).await
        }
        Owner::Reader => {
            if state
                .native_vowifi_lifecycle
                .module_call_service(&Method::POST, path)
                .is_some()
            {
                return error(
                    StatusCode::CONFLICT,
                    "call_owner_changed",
                    "读卡器 VoWiFi owner 已变化，请刷新状态后重试",
                );
            }
            match state.native_vowifi_lifecycle.service(state) {
                Ok(service) => service.handle_public(request).await,
                Err(_) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "call_owner_unavailable",
                    "读卡器 VoWiFi 服务暂不可用",
                ),
            }
        }
        Owner::Module => unreachable!(),
    }
}

async fn dispatch_vowifi(
    action: &str,
    owner: Owner,
    generation: u64,
    intent: &Intent,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    let downstream_action = if action == "hangup" && intent.command.as_deref() == Some("reject") {
        "reject"
    } else {
        action
    };
    let path = format!("/api/vowifi/calls/{downstream_action}");
    let body = match downstream_action {
        "dial" => json!({
            "peer": intent.number.as_deref().or(intent.peer.as_deref()).unwrap_or_default(),
            "generation": generation,
        }),
        "dtmf" => json!({
            "digit": intent.digit.as_deref().unwrap_or_default(),
            "generation": generation,
        }),
        _ => json!({"generation": generation}),
    };
    let request = Request::builder()
        .method(Method::POST)
        .uri(&path)
        .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
        .expect("validated call request");
    match owner {
        Owner::ModuleVoWiFi => {
            let Some(service) = state
                .native_vowifi_lifecycle
                .module_call_service(&Method::POST, &path)
            else {
                return error(
                    StatusCode::CONFLICT,
                    "call_owner_changed",
                    "模块 VoWiFi owner 已变化，请刷新状态后重试",
                );
            };
            service.calls(request).await
        }
        Owner::Reader => {
            if state
                .native_vowifi_lifecycle
                .module_call_service(&Method::POST, &path)
                .is_some()
            {
                return error(
                    StatusCode::CONFLICT,
                    "call_owner_changed",
                    "读卡器 VoWiFi owner 已变化，请刷新状态后重试",
                );
            }
            match state.native_vowifi_lifecycle.service(state) {
                Ok(service) => service.handle_public(request).await,
                Err(_) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "call_owner_unavailable",
                    "读卡器 VoWiFi 通话服务暂不可用",
                ),
            }
        }
        Owner::Module => unreachable!(),
    }
}

fn json_request(value: Value) -> Request<Full<Bytes>> {
    Request::new(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
}

fn empty_request() -> Request<Full<Bytes>> {
    Request::new(Full::new(Bytes::new()))
}

async fn response_value(response: Response<Full<Bytes>>) -> Option<Value> {
    let body = response.into_body().collect().await.ok()?.to_bytes();
    serde_json::from_slice(&body).ok()
}

fn error(status: StatusCode, code: &'static str, message: &'static str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":message,"code":code,"recoverable":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_vowifi_call_alias_and_ussd_write_uses_the_durable_boundary() {
        for prefix in ["/api/vowifi/calls/", "/api/reader/vowifi/calls/"] {
            for action in ["dial", "answer", "reject", "hangup", "dtmf"] {
                let path = format!("{prefix}{action}");
                assert!(is_mutation(&Method::POST, &path), "unfenced route {path}");
                assert_eq!(mutation_action(&Method::POST, &path), Some(action));
                assert!(!is_mutation(&Method::GET, &path));
            }
        }
        assert_eq!(
            mutation_action(&Method::POST, "/api/reader/vowifi/ussd"),
            Some("ussd")
        );
        let main = include_str!("main.rs");
        let durable = main
            .find("if unified_call_control::is_mutation")
            .expect("durable public mutation dispatch");
        let direct = main
            .find(".module_call_service(request.method(), request.uri().path())")
            .expect("direct native call dispatch");
        assert!(
            durable < direct,
            "native alias dispatch precedes its durable ledger"
        );
    }

    #[test]
    fn public_mutation_payload_is_strict_and_carries_ussd_identity_fields() {
        assert!(serde_json::from_slice::<Intent>(
            br#"{"client_request_id":"r-1","command":"*#100#","session_id":"","input":"","cancel":false,"generation":9}"#
        )
        .is_ok());
        assert!(
            serde_json::from_slice::<Intent>(
                br#"{"client_request_id":"r-1","generation":9,"unexpected":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_slice::<Intent>(br#"{"client_request_id":9,"generation":9}"#).is_err()
        );
        assert!(payload_fields_valid(
            "ussd",
            br#"{"client_request_id":"r-1","command":"*#100#","generation":9}"#
        ));
        assert!(!payload_fields_valid(
            "ussd",
            br#"{"client_request_id":"r-1","digit":"1","generation":9}"#
        ));
        assert!(!payload_fields_valid(
            "answer",
            br#"{"client_request_id":"r-1","command":"*#100#","generation":9}"#
        ));
        assert!(!payload_fields_valid(
            "dial",
            br#"{"client_request_id":"r-1","number":"1","peer":"2","generation":9}"#
        ));
        for invalid in ["", " leading", "trailing ", "line\nbreak"] {
            assert!(!valid_client_request_id(invalid));
        }
        assert!(!valid_client_request_id(&"x".repeat(129)));
        assert!(valid_client_request_id("public-call-request-1"));
    }

    #[test]
    fn owner_selection_is_receipt_fenced_and_never_falls_across_owner() {
        let idle = Observation::Idle;
        assert_eq!(
            choose_owner("dial", Some(Owner::Reader), None, 0, idle, idle),
            Ok(Owner::Reader)
        );
        assert!(matches!(
            choose_owner(
                "hangup",
                Some(Owner::Module),
                Some(Owner::Reader),
                77,
                Observation::Uncertain,
                Observation::Uncertain,
            ),
            Err((StatusCode::SERVICE_UNAVAILABLE, "call_owner_unconfirmed", _))
        ));
        let conflict = choose_owner(
            "hangup",
            None,
            Some(Owner::Reader),
            0,
            Observation::Active {
                owner: Owner::Module,
                generation: 0,
            },
            idle,
        );
        assert!(matches!(
            conflict,
            Err((StatusCode::CONFLICT, "call_owner_changed", _))
        ));
    }

    #[test]
    fn idle_vowifi_projection_retains_its_generation_for_durable_dial_binding() {
        assert_eq!(
            decode_vowifi_observation(&json!({
                "source": "reader",
                "generation": 41,
                "call": {"state": "idle"}
            })),
            VoWiFiObservation {
                call: Observation::Idle,
                binding: Some((Owner::Reader, 41)),
            }
        );
        assert_eq!(
            decode_vowifi_observation(&json!({
                "source": "module",
                "generation": 52,
                "call": {"state": "active"}
            })),
            VoWiFiObservation {
                call: Observation::Active {
                    owner: Owner::ModuleVoWiFi,
                    generation: 52,
                },
                binding: Some((Owner::ModuleVoWiFi, 52)),
            }
        );
        assert_eq!(
            decode_vowifi_observation(&json!({
                "source": "reader",
                "generation": 0,
                "call": {"state": "idle"}
            })),
            VoWiFiObservation {
                call: Observation::Idle,
                binding: None,
            }
        );
    }

    #[test]
    fn any_uncertain_owner_fails_closed_before_receipt_active_or_dial_preference() {
        let idle = Observation::Idle;
        let active_reader = Observation::Active {
            owner: Owner::Reader,
            generation: 77,
        };
        for result in [
            choose_owner(
                "dial",
                Some(Owner::Reader),
                None,
                0,
                Observation::Uncertain,
                idle,
            ),
            choose_owner(
                "dial",
                Some(Owner::Module),
                None,
                0,
                idle,
                Observation::Uncertain,
            ),
            choose_owner(
                "hangup",
                Some(Owner::Module),
                Some(Owner::Reader),
                77,
                Observation::Uncertain,
                Observation::Uncertain,
            ),
            choose_owner(
                "hangup",
                Some(Owner::Reader),
                Some(Owner::Reader),
                77,
                Observation::Uncertain,
                active_reader,
            ),
            choose_owner(
                "answer",
                None,
                None,
                0,
                Observation::Active {
                    owner: Owner::Module,
                    generation: 0,
                },
                Observation::Uncertain,
            ),
        ] {
            assert!(matches!(
                result,
                Err((StatusCode::SERVICE_UNAVAILABLE, "call_owner_unconfirmed", _))
            ));
        }
    }

    #[test]
    fn vowifi_controls_require_a_live_nonzero_generation() {
        assert!(matches!(
            choose_owner(
                "dtmf",
                Some(Owner::Reader),
                Some(Owner::Reader),
                0,
                Observation::Idle,
                Observation::Active {
                    owner: Owner::Reader,
                    generation: 0,
                },
            ),
            Err((StatusCode::SERVICE_UNAVAILABLE, "call_owner_unconfirmed", _))
        ));
        assert!(matches!(
            choose_owner(
                "hangup",
                Some(Owner::Reader),
                Some(Owner::Reader),
                76,
                Observation::Idle,
                Observation::Active {
                    owner: Owner::Reader,
                    generation: 77,
                },
            ),
            Err((StatusCode::CONFLICT, "call_generation_changed", _))
        ));
        assert_eq!(
            choose_owner(
                "hangup",
                Some(Owner::Reader),
                Some(Owner::Reader),
                77,
                Observation::Idle,
                Observation::Active {
                    owner: Owner::Reader,
                    generation: 77,
                },
            ),
            Ok(Owner::Reader)
        );
    }

    #[test]
    fn restart_without_receipt_requires_an_unambiguous_live_owner() {
        assert_eq!(
            choose_owner(
                "answer",
                None,
                None,
                0,
                Observation::Idle,
                Observation::Active {
                    owner: Owner::Reader,
                    generation: 44,
                },
            ),
            Ok(Owner::Reader)
        );
        assert!(matches!(
            choose_owner(
                "hangup",
                None,
                None,
                0,
                Observation::Uncertain,
                Observation::Idle,
            ),
            Err((StatusCode::SERVICE_UNAVAILABLE, "call_owner_unconfirmed", _))
        ));
        assert!(matches!(
            choose_owner(
                "hangup",
                None,
                None,
                0,
                Observation::Active {
                    owner: Owner::Module,
                    generation: 0,
                },
                Observation::Active {
                    owner: Owner::Reader,
                    generation: 44,
                },
            ),
            Err((StatusCode::CONFLICT, "call_owner_ambiguous", _))
        ));
    }
}
