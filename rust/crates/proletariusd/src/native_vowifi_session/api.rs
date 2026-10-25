//! Legacy HTTP shape over the one ordinary native dispatcher. A request owns
//! only a frozen binding and a reply waiter, never the session or its locks.
use crate::{
    json_response, native_ims_calls, native_registered_session as registered,
    native_vowifi_supervisor::SessionGeneration,
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode};
use serde::{
    Deserialize,
    de::{MapAccess, Visitor},
};
use serde_json::{Value, json};
use std::{borrow::Cow, future::Future, pin::Pin, time::Duration};
use tokio::sync::{mpsc, oneshot};

pub(super) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[derive(Clone)]
pub(super) struct ActiveBinding {
    pub(super) reader_generation: u64,
    pub(super) session_generation: SessionGeneration,
    pub(super) source: String,
    pub(super) controls: mpsc::Sender<registered::Control>,
}
pub(super) struct ApiFailure {
    pub(super) message: String,
}
impl ApiFailure {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
pub(super) trait Service: Sync {
    fn resolve(
        &self,
        generation: u64,
        verify_mutation: bool,
    ) -> BoxFuture<'_, Result<ActiveBinding, ApiFailure>>;
    fn revalidate<'a>(
        &'a self,
        binding: &'a ActiveBinding,
    ) -> BoxFuture<'a, Result<(), ApiFailure>>;
    fn history_error(&self) -> Option<String>;
}

/// Both public prefixes resolve Go's single active ordinary IMS session. In
/// particular, `reader/` is not a device selector: that session may be module
/// VoWiFi. A retained native intent owns its controls even while it is stale,
/// draining or terminal; its failure must never retry against a Go session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallRouteOwner {
    NativeIntent,
    LegacyActiveSession,
    AmbiguousOwners,
}

/// Inspect only routing metadata. The caller supplies the daemon's retained
/// intent observation, never a request field or readiness/registered flag.
/// HEAD has already been normalized to GET by public_http at this boundary.
pub(crate) fn call_route_owner(
    method: &hyper::Method,
    path: &str,
    retained_native_intent: bool,
    legacy_module_claim: bool,
) -> Option<CallRouteOwner> {
    let action = path
        .strip_prefix("/api/reader/vowifi/calls/")
        .or_else(|| path.strip_prefix("/api/vowifi/calls/"))?;
    let matches = match action {
        "status" => method == hyper::Method::GET,
        "dial" | "answer" | "reject" | "hangup" | "dtmf" => method == hyper::Method::POST,
        _ => false,
    };
    matches.then_some(if retained_native_intent && legacy_module_claim {
        CallRouteOwner::AmbiguousOwners
    } else if retained_native_intent {
        CallRouteOwner::NativeIntent
    } else {
        CallRouteOwner::LegacyActiveSession
    })
}

/// Return a legacy/unmatched request intact. Selection never polls its body;
/// only a selected native request reaches the existing single decode below.
/// The native response, including unavailable/stale errors, is always final.
pub(super) async fn handle_call_if_owned<B>(
    request: Request<B>,
    service: &impl Service,
    retained_native_intent: bool,
    legacy_module_claim: bool,
) -> Result<Response<Full<Bytes>>, Request<B>>
where
    B: hyper::body::Body<Data = Bytes> + Send,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    match call_route_owner(
        request.method(),
        request.uri().path(),
        retained_native_intent,
        legacy_module_claim,
    ) {
        Some(CallRouteOwner::NativeIntent) => Ok(handle(request, service).await),
        Some(CallRouteOwner::AmbiguousOwners) => Ok(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "VoWiFi 通话会话归属冲突，请等待资源释放",
        )),
        _ => Err(request),
    }
}

#[derive(Default, Debug)]
struct Input {
    peer: String,
    digit: String,
    command: String,
    session_id: String,
    input: String,
    cancel: bool,
    generation: u64,
    invalid: bool,
}
pub(super) struct Fields(pub(super) Vec<(String, Value)>);
impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = Fields;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a legacy control object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Fields, M::Error> {
                let mut fields = Vec::new();
                while let Some(key) = map.next_key()? {
                    fields.push((key, map.next_value()?));
                }
                Ok(Fields(fields))
            }
        }
        decoder.deserialize_map(Object)
    }
}
pub(super) fn field_name(key: &str) -> String {
    // Go encoding/json folds ASCII plus the Unicode simple-fold S/K cases.
    key.chars()
        .map(|c| match c {
            'ſ' => 's',
            'K' => 'k',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}
fn decode(bytes: &[u8], ussd: bool) -> Input {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let fields = match Option::<Fields>::deserialize(&mut decoder) {
        Ok(Some(fields)) => fields.0,
        Ok(None) => Vec::new(),
        Err(_) => {
            return Input {
                invalid: true,
                ..Default::default()
            };
        }
    };
    let mut result = Input::default();
    for (key, value) in fields {
        let key = field_name(&key);
        let destination = match key.as_str() {
            "peer" if !ussd => Some(&mut result.peer),
            "digit" if !ussd => Some(&mut result.digit),
            "command" if ussd => Some(&mut result.command),
            "session_id" if ussd => Some(&mut result.session_id),
            "input" if ussd => Some(&mut result.input),
            _ => None,
        };
        if let Some(destination) = destination {
            if value.is_null() {
                continue;
            }
            match value.as_str() {
                Some(s) => *destination = s.into(),
                None => result.invalid = true,
            }
        } else if key == "generation" {
            if value.is_null() {
                continue;
            }
            match value.as_u64() {
                Some(n) => result.generation = n,
                None => result.invalid = true,
            }
        } else if ussd && key == "cancel" {
            if value.is_null() {
                continue;
            }
            match value.as_bool() {
                Some(b) => result.cancel = b,
                None => result.invalid = true,
            }
        } else if ussd {
            result.invalid = true;
        }
    }
    result
}
fn error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    json_response(status, &json!({"error":message}))
}
fn failure_message(failure: &registered::Failure) -> Cow<'static, str> {
    if let Some(status) = failure.sip_status {
        match failure.code {
            "ims_ussd_rejected" => return Cow::Owned(format!("运营商拒绝 USSD：SIP {status}")),
            "ims_ussd_input_rejected" => {
                return Cow::Owned(format!("USSD 交互被拒绝：SIP {status}"));
            }
            _ => {}
        }
    }
    Cow::Borrowed(match failure.code {
        "ims_call_busy" => "imscore: another reader call is already active",
        "ims_call_not_ringing" | "ims_call_not_incoming" => "imscore: no incoming reader call",
        "ims_call_missing" => "imscore: DTMF requires an active call",
        "ims_call_dtmf_unavailable" => {
            "imscore: remote IMS endpoint did not negotiate telephone-event"
        }
        "ims_ussd_invalid_command" => "请输入有效 USSD 短码",
        "ims_ussd_input_invalid" => "USSD 输入须为 1–182 个有效字符",
        "ims_ussd_busy" => "已有 USSD 查询正在进行",
        "ims_ussd_stale_session" => "USSD 会话已结束或已切卡",
        "ims_ussd_waiting_network" => "USSD 正在等待网络响应",
        "ims_ussd_domain_invalid" => "无效 IMS 归属域",
        "ims_ussd_disabled_for_sms_session" => "IMS USSD 通道尚未就绪",
        "ims_ussd_session_stopped" | "ims_ussd_cancelled" => "USSD 会话已结束",
        "ims_ussd_timeout" => "USSD 查询超时或连接已结束",
        "ims_ussd_input_rejected" | "ims_ussd_rejected" => "运营商拒绝 USSD",
        "ims_ussd_dialog_invalid" => "USSD 响应缺少对话标识",
        "ims_ussd_content_invalid" => "无效 USSD XML",
        _ => failure.code,
    })
}
fn call_failure_message(action: &str, failure: &registered::Failure) -> Cow<'static, str> {
    // The typed owner shares its missing-dialog code across operations. Go's
    // Answer/Reject expose an incoming-call error, rather than DTMF wording.
    if failure.code == "ims_call_missing" && matches!(action, "answer" | "reject") {
        Cow::Borrowed("imscore: no incoming reader call")
    } else {
        failure_message(failure)
    }
}
fn call_projection(snapshot: native_ims_calls::Snapshot) -> Value {
    let mut value = serde_json::to_value(snapshot).expect("call snapshot is serializable");
    let map = value.as_object_mut().unwrap();
    map.remove("uncertain");
    for key in [
        "direction",
        "peer",
        "call_id",
        "codec",
        "started_at",
        "last_error",
        "audio_token",
    ] {
        if map.get(key).and_then(Value::as_str) == Some("") {
            map.remove(key);
        }
    }
    for key in ["audio_port", "audio_sample_rate"] {
        if map.get(key).and_then(Value::as_u64) == Some(0) {
            map.remove(key);
        }
    }
    value
}
fn call_response(
    service: &impl Service,
    binding: Option<&ActiveBinding>,
    snapshot: native_ims_calls::Snapshot,
    status: StatusCode,
) -> Response<Full<Bytes>> {
    let mut value = json!({"generation":binding.map_or(0, |b| b.reader_generation), "call":call_projection(snapshot)});
    if let Some(binding) = binding {
        value["source"] = json!(binding.source);
    }
    if let Some(error) = service.history_error().filter(|s| !s.is_empty()) {
        value["history_error"] = json!(error);
    }
    json_response(status, &value)
}
fn unavailable(service: &impl Service) -> Response<Full<Bytes>> {
    call_response(
        service,
        None,
        native_ims_calls::Snapshot {
            state: "unavailable".into(),
            signaling_ready: false,
            ..Default::default()
        },
        StatusCode::OK,
    )
}

pub(super) async fn handle<B>(request: Request<B>, service: &impl Service) -> Response<Full<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Send,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let path = request.uri().path().to_owned();
    // The legacy status handlers do not decode or consume a GET body.
    if path == "/api/reader/vowifi/calls/status" || path == "/api/vowifi/calls/status" {
        return handle_bytes(&path, &[], service).await;
    }
    let body = std::pin::pin!(request.into_body());
    let bytes = match crate::http_server::collect_body(body, 4096).await {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                if path.ends_with("/ussd") {
                    "无效 USSD 请求"
                } else {
                    "请求格式错误"
                },
            );
        }
    };
    handle_bytes(&path, &bytes, service).await
}
async fn handle_bytes(path: &str, bytes: &[u8], service: &impl Service) -> Response<Full<Bytes>> {
    if path == "/api/reader/vowifi/ussd" {
        return ussd(bytes, service).await;
    }
    let Some(action) = path
        .strip_prefix("/api/reader/vowifi/calls/")
        .or_else(|| path.strip_prefix("/api/vowifi/calls/"))
    else {
        return error(StatusCode::NOT_FOUND, "Not found");
    };
    call(action, bytes, service).await
}
async fn call(action: &str, bytes: &[u8], service: &impl Service) -> Response<Full<Bytes>> {
    let input = decode(bytes, false);
    let canonical_peer = crate::call_control::canonical_voice_number(&input.peer);
    let (command, successful, budget) = match action {
        "status" => (registered::CallCommand::Snapshot, StatusCode::OK, 3),
        "dial"
            if input.invalid
                || canonical_peer.is_empty()
                || !crate::call_control::valid_voice_number(&canonical_peer) =>
        {
            return error(StatusCode::BAD_REQUEST, "请输入有效的被叫号码");
        }
        "dial" => (
            registered::CallCommand::Dial(canonical_peer),
            StatusCode::ACCEPTED,
            10,
        ),
        "answer" => (registered::CallCommand::Answer, StatusCode::OK, 30),
        "reject" => (registered::CallCommand::Reject, StatusCode::OK, 10),
        "hangup" => (registered::CallCommand::Hangup, StatusCode::OK, 10),
        "dtmf"
            if input.invalid
                || input.digit.len() != 1
                || !input
                    .digit
                    .bytes()
                    .all(|c| c.is_ascii_digit() || c == b'*' || c == b'#') =>
        {
            return error(StatusCode::BAD_REQUEST, "按键只能是 0-9、* 或 #");
        }
        "dtmf" => (
            registered::CallCommand::Dtmf(input.digit.chars().next().unwrap()),
            StatusCode::OK,
            3,
        ),
        _ => return error(StatusCode::NOT_FOUND, "Not found"),
    };
    let binding = match service
        .resolve(
            if action == "status" {
                0
            } else {
                input.generation
            },
            action != "status",
        )
        .await
    {
        Ok(binding) => binding,
        Err(_) if action == "status" => return unavailable(service),
        Err(failure) => return error(StatusCode::CONFLICT, &failure.message),
    };
    let (reply, response) = oneshot::channel();
    let control = registered::Control::Call {
        generation: binding.session_generation,
        command,
        reply,
    };
    let result = tokio::time::timeout(Duration::from_secs(budget), async {
        binding
            .controls
            .send(control)
            .await
            .map_err(|_| registered::Failure::from("native_session_stopped"))?;
        response
            .await
            .map_err(|_| registered::Failure::from("native_session_stopped"))?
    })
    .await
    .unwrap_or_else(|_| {
        Err(registered::Failure {
            code: "context deadline exceeded",
            uncertain: true,
            sip_status: None,
        })
    });
    if service.revalidate(&binding).await.is_err() {
        return if action == "status" {
            unavailable(service)
        } else {
            error(
                StatusCode::CONFLICT,
                "读卡器或 eSIM Profile 已变更，请刷新后重试",
            )
        };
    }
    match result {
        Ok(snapshot) => call_response(service, Some(&binding), snapshot, successful),
        Err(_) if action == "status" => unavailable(service),
        Err(failure) => error(
            if matches!(action, "answer" | "reject") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_GATEWAY
            },
            &call_failure_message(action, &failure),
        ),
    }
}
async fn ussd(bytes: &[u8], service: &impl Service) -> Response<Full<Bytes>> {
    let input = decode(bytes, true);
    if input.invalid {
        return error(StatusCode::BAD_REQUEST, "无效 USSD 请求");
    }
    let binding = match service.resolve(input.generation, true).await {
        Ok(binding) => binding,
        Err(failure) => return error(StatusCode::CONFLICT, &failure.message),
    };
    let cancelling = input.cancel;
    let session_id = input.session_id.clone();
    let command = if input.session_id.is_empty() {
        if input.cancel || !input.input.is_empty() {
            return error(StatusCode::BAD_REQUEST, "缺少 USSD 会话");
        }
        registered::UssdCommand::Start(input.command)
    } else if !input.command.is_empty() {
        return error(StatusCode::BAD_REQUEST, "交互请求不能重复提交短码");
    } else if input.cancel {
        registered::UssdCommand::Cancel {
            session_id: input.session_id,
        }
    } else {
        registered::UssdCommand::Continue {
            session_id: input.session_id,
            input: input.input,
        }
    };
    let (reply, response) = oneshot::channel();
    let control = registered::Control::Ussd {
        generation: binding.session_generation,
        command,
        reply,
    };
    // Only the HTTP waiter waits for the network result. The dispatcher is free
    // to consume the INFO/200/BYE that completes this exact Pending receiver.
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        binding
            .controls
            .send(control)
            .await
            .map_err(|_| registered::Failure::from("native_session_stopped"))?;
        match response
            .await
            .map_err(|_| registered::Failure::from("native_session_stopped"))??
        {
            registered::UssdReply::Complete(result) => Ok(result),
            registered::UssdReply::Pending(pending) => pending
                .receiver
                .await
                .map_err(|_| registered::Failure::from("native_session_stopped"))?
                .map_err(Into::into),
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(registered::Failure {
            code: "context deadline exceeded",
            uncertain: true,
            sip_status: None,
        })
    });
    if service.revalidate(&binding).await.is_err() {
        return error(StatusCode::CONFLICT, "号卡已变更，已丢弃旧 USSD 响应");
    }
    match result {
        Err(failure) => error(StatusCode::BAD_GATEWAY, &failure_message(&failure)),
        Ok(result) => {
            let mut value = if cancelling {
                json!({"session_id":session_id,"text":"","needs_input":false,"complete":true})
            } else {
                serde_json::to_value(result).expect("USSD result is serializable")
            };
            if value.get("language").and_then(Value::as_str) == Some("") {
                value.as_object_mut().unwrap().remove("language");
            }
            json_response(
                StatusCode::OK,
                &json!({"generation":binding.reader_generation,"result":value}),
            )
        }
    }
}

#[cfg(test)]
mod tests;
