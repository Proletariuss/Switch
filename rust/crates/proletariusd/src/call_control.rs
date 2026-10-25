//! Public module-call DTO boundary. The in-process owner performs admission,
//! session-fenced physical execution and observation; no reverse Go RPC.
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::{Request, Response, StatusCode};
use serde::Deserialize;
#[cfg(test)]
use serde_json::Value;
use serde_json::json;
use std::{future::Future, pin::Pin, time::Duration};

pub(crate) type Reply = Response<Full<Bytes>>;
pub(crate) type ReplyFuture = Pin<Box<dyn Future<Output = Reply> + Send>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Dial,
    Answer,
    Hangup,
    Dtmf,
}
pub(crate) trait Backend: Clone + Send + Sync + 'static {
    fn status(&self) -> ReplyFuture;
    fn execute(&self, action: Action, value: String) -> ReplyFuture;
}

const MAX_DIAL_REQUEST_BYTES: usize = 1 << 20;
const MAX_DTMF_REQUEST_BYTES: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DialRequest {
    #[serde(default)]
    number: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DtmfRequest {
    digit: String,
}

pub(crate) async fn handle_status(backend: impl Backend) -> Reply {
    backend.status().await
}
pub(crate) async fn handle_dial<B>(request: Request<B>, backend: impl Backend) -> Reply
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let body = match read_body(request, MAX_DIAL_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let input: DialRequest = match serde_json::from_slice(&body) {
        Ok(input) => input,
        Err(error) => return invalid_json(&error.to_string()),
    };
    let number = canonical_voice_number(input.number.as_deref().unwrap_or_default());
    if number.is_empty() {
        return invalid_json("number is required");
    }
    if !valid_voice_number(&number) {
        return invalid_json("电话号码只能包含数字、开头的 +、* 和 #，且最长 32 位");
    }
    backend.execute(Action::Dial, number).await
}
pub(crate) async fn handle_dtmf<B>(request: Request<B>, backend: impl Backend) -> Reply
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let body = match read_body(request, MAX_DTMF_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let input: DtmfRequest = match serde_json::from_slice(&body) {
        Ok(input) => input,
        Err(error) => return invalid_json(&error.to_string()),
    };
    let digit = input.digit.trim();
    if !valid_dtmf_digit(digit) {
        return invalid_json("按键只能是 0-9、* 或 #");
    }
    backend.execute(Action::Dtmf, digit.to_owned()).await
}
pub(crate) async fn handle_answer<B>(_request: Request<B>, backend: impl Backend) -> Reply {
    // The existing API ignores the body for these fixed actions.
    backend.execute(Action::Answer, String::new()).await
}
pub(crate) async fn handle_hangup<B>(_request: Request<B>, backend: impl Backend) -> Reply {
    backend.execute(Action::Hangup, String::new()).await
}
// This adapter only decodes finite Full DTOs reconstructed after the public
// unified_call_control ingress boundary; it does not collect network bodies.
async fn read_body<B>(request: Request<B>, limit: usize) -> Result<Bytes, Reply>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match tokio::time::timeout(
        Duration::from_secs(2),
        Limited::new(request.into_body(), limit).collect(),
    )
    .await
    {
        Ok(Ok(body)) => Ok(body.to_bytes()),
        Ok(Err(_)) => Err(invalid_json("请求体无效或超过限制")),
        Err(_) => Err(crate::json_response(
            StatusCode::REQUEST_TIMEOUT,
            &json!({"error":"读取请求超时"}),
        )),
    }
}
pub(crate) fn valid_dtmf_digit(value: &str) -> bool {
    value.len() == 1 && (value.as_bytes()[0].is_ascii_digit() || value == "*" || value == "#")
}
pub(crate) fn valid_voice_number(value: &str) -> bool {
    if value.is_empty() || value.len() > 32 || value == "+" {
        return false;
    }
    value.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_digit() || byte == b'*' || byte == b'#' || (index == 0 && byte == b'+')
    })
}
/// Canonicalize the PSTN international access prefix at the Rust execution
/// boundary. UI callers may submit formatted text, but every physical owner
/// receives the same value regardless of which client invoked the API.
pub(crate) fn canonical_voice_number(value: &str) -> String {
    let trimmed = value.trim();
    let compact: String = trimmed
        .chars()
        .filter(|character| !matches!(character, ' ' | '-' | '(' | ')'))
        .collect();
    if compact.starts_with("00")
        && compact.len() > 2
        && compact[2..].bytes().all(|byte| byte.is_ascii_digit())
    {
        format!("+{}", &compact[2..])
    } else {
        trimmed.to_owned()
    }
}
fn invalid_json(message: &str) -> Reply {
    crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":message}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Fake {
        seen: Arc<Mutex<Vec<(Action, String)>>>,
        value: Value,
        status: StatusCode,
    }
    impl Fake {
        fn new(value: Value) -> Self {
            Self {
                seen: Default::default(),
                value,
                status: StatusCode::OK,
            }
        }
    }
    impl Backend for Fake {
        fn status(&self) -> ReplyFuture {
            let this = self.clone();
            Box::pin(async move { crate::json_response(this.status, &this.value) })
        }
        fn execute(&self, action: Action, value: String) -> ReplyFuture {
            self.seen.lock().unwrap().push((action, value));
            self.status()
        }
    }
    fn req(body: &str) -> Request<Full<Bytes>> {
        Request::new(Full::new(Bytes::copy_from_slice(body.as_bytes())))
    }
    async fn value(reply: Reply) -> Value {
        serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    #[test]
    fn validates_exactly_one_ascii_dtmf_symbol() {
        for v in ["0", "9", "*", "#"] {
            assert!(valid_dtmf_digit(v));
        }
        for v in ["", "12", "+", "Ａ", "a", " "] {
            assert!(!valid_dtmf_digit(v));
        }
    }
    #[test]
    fn voice_number_validation_matches_the_public_contract() {
        for v in ["+8613800000000", "001234", "*123#", "123"] {
            assert!(valid_voice_number(v));
        }
        for v in ["", "+", "12+3", "1 2", "１２３", "a123"] {
            assert!(!valid_voice_number(v));
        }
        assert!(valid_voice_number(&"1".repeat(32)));
        assert!(!valid_voice_number(&"1".repeat(33)));
    }
    #[test]
    fn canonicalizes_only_numeric_double_zero_international_prefix() {
        for (input, expected) in [
            ("00442079460000", "+442079460000"),
            (" 00 44 (20) 7946-0000 ", "+442079460000"),
            ("+442079460000", "+442079460000"),
            ("1033", "1033"),
            ("00", "00"),
            ("*123#", "*123#"),
            ("00*123", "00*123"),
        ] {
            assert_eq!(canonical_voice_number(input), expected, "{input}");
        }
    }
    #[tokio::test]
    async fn invalid_dial_json_or_number_never_reaches_physical_owner() {
        let fake = Fake::new(json!({"accepted":true}));
        for body in [
            "",
            "null",
            "[]",
            "{}",
            "{\"number\":null}",
            "{\"number\":23}",
            "{\"number\":\"12+3\"}",
            "{\"number\":\"1\",\"extra\":true}",
        ] {
            assert_eq!(
                handle_dial(req(body), fake.clone()).await.status(),
                StatusCode::BAD_REQUEST,
                "{body}"
            );
        }
        assert!(fake.seen.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn accepted_dial_canonicalizes_at_the_rust_owner_boundary() {
        let fake = Fake::new(json!({"accepted":true,"response":"OK"}));
        assert_eq!(
            value(handle_dial(req("{\"number\":\" 001234 \"}"), fake.clone()).await).await,
            fake.value
        );
        assert_eq!(
            *fake.seen.lock().unwrap(),
            vec![(Action::Dial, "+1234".into())]
        );
    }
    #[tokio::test]
    async fn dtmf_trims_and_rejects_unknown_fields_or_nonascii() {
        let fake = Fake::new(json!({"accepted":true,"response":"OK"}));
        for body in [
            "{}",
            "{\"digit\":null}",
            "{\"digit\":\"12\"}",
            "{\"digit\":\"１\"}",
            "{\"digit\":\"1\",\"extra\":true}",
        ] {
            assert_eq!(
                handle_dtmf(req(body), fake.clone()).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert!(fake.seen.lock().unwrap().is_empty());
        assert_eq!(
            handle_dtmf(req("{\"digit\":\" # \"}"), fake.clone())
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(*fake.seen.lock().unwrap(), vec![(Action::Dtmf, "#".into())]);
    }
    #[tokio::test]
    async fn oversized_bodies_never_reach_owner() {
        let fake = Fake::new(json!({}));
        assert_eq!(
            handle_dial(req(&" ".repeat(MAX_DIAL_REQUEST_BYTES + 1)), fake.clone())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            handle_dtmf(req(&" ".repeat(MAX_DTMF_REQUEST_BYTES + 1)), fake.clone())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert!(fake.seen.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn fixed_actions_ignore_body_and_preserve_hangup_warning() {
        let fake = Fake::new(json!({"accepted":true,"response":"OK","warning":"cleanup pending"}));
        assert_eq!(
            value(handle_answer(req("ignored"), fake.clone()).await).await,
            fake.value
        );
        assert_eq!(
            value(handle_hangup(req("ignored"), fake.clone()).await).await,
            fake.value
        );
        assert_eq!(
            *fake.seen.lock().unwrap(),
            vec![(Action::Answer, "".into()), (Action::Hangup, "".into())]
        );
    }
    #[tokio::test]
    async fn native_admission_errors_and_history_error_keep_their_shapes() {
        let mut fake = Fake::new(
            json!({"error":"当前通话状态为 unknown，不能发起新的呼叫","code":"call_not_idle","recoverable":true}),
        );
        fake.status = StatusCode::CONFLICT;
        let response = handle_dial(req("{\"number\":\"123\"}"), fake.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(value(response).await, fake.value);
        let fake = Fake::new(
            json!({"state":"incoming","number":"123","incoming":true,"active":false,"history_error":"archive paused"}),
        );
        assert_eq!(value(handle_status(fake.clone()).await).await, fake.value);
    }
}
