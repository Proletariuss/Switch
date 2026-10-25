//! Public N-wait SMS queue admission. Rust owns validation, durable admission
//! and the public response; the retained native manager supplies exact-session
//! readiness directly, without a second runtime or RPC executor.

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use proletarius_sms::stable_digest_hex;
use proletarius_store::StoreError;
use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::time::Duration;

const PUBLIC_PATH: &str = "/api/multistandby/messages/send";
const MAX_BODY: usize = 1024 * 1024;

struct SendRequest {
    profile_id: String,
    phone: String,
    body: String,
    client_request_id: String,
}

#[cfg(test)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MessagingFacts {
    registered: bool,
    ipsec_installed: bool,
    plain_ims: bool,
    transport_present: bool,
    transport_healthy: bool,
}

#[cfg(test)]
impl MessagingFacts {
    fn ready(&self) -> bool {
        self.registered
            && (self.ipsec_installed || self.plain_ims)
            && self.transport_present
            && self.transport_healthy
    }
}

#[cfg(test)]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReadinessFacts {
    profile_desired: bool,
    session_present: bool,
    session_reader: String,
    session_reader_generation: u64,
    selected_reader: String,
    selected_reader_generation: u64,
    tunnel_state: String,
    ims_state: String,
    messaging: MessagingFacts,
    sim_info_present: bool,
    smsc: String,
    sms_target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SendReadiness {
    session_current: bool,
    channel_ready: bool,
}

#[cfg(test)]
impl ReadinessFacts {
    fn project(self) -> Result<SendReadiness, ()> {
        if self.session_reader.len() > 512
            || self.selected_reader.len() > 512
            || self.tunnel_state.len() > 64
            || self.ims_state.len() > 64
            || self.smsc.len() > 256
            || self.sms_target.len() > 512
            || self.session_reader.contains('\0')
            || self.selected_reader.contains('\0')
            || self.tunnel_state.contains('\0')
            || self.ims_state.contains('\0')
            || self.smsc.contains('\0')
            || self.sms_target.contains('\0')
        {
            return Err(());
        }
        let session_current = self.profile_desired
            && self.session_present
            && !self.session_reader.trim().is_empty()
            && self.session_reader_generation != 0
            && self.session_reader == self.selected_reader
            && self.session_reader_generation == self.selected_reader_generation;
        let channel_ready = session_current
            && self.tunnel_state == "connected"
            && self.ims_state == "registered"
            && self.messaging.ready()
            && self.sim_info_present
            && !self.smsc.trim().is_empty()
            && !self.sms_target.trim().is_empty();
        Ok(SendReadiness {
            session_current,
            channel_ready,
        })
    }
}

fn string_field(object: &Map<String, Value>, field: &str) -> Result<String, ()> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(()),
    }
}

fn parse_send_request(value: Value) -> Result<SendRequest, ()> {
    let object = match value {
        Value::Null => Map::new(),
        Value::Object(object) => object,
        _ => return Err(()),
    };
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "profile_id" | "phone" | "body" | "client_request_id"
        )
    }) {
        return Err(());
    }
    Ok(SendRequest {
        profile_id: string_field(&object, "profile_id")?,
        phone: string_field(&object, "phone")?,
        body: string_field(&object, "body")?,
        client_request_id: string_field(&object, "client_request_id")?,
    })
}

fn public_error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"error":message}))
}

fn go_store_status(code: &str) -> StatusCode {
    match code {
        "invalid_request" => StatusCode::BAD_REQUEST,
        "profile_not_found" | "message_not_found" | "send_intent_not_found" | "route_not_found" => {
            StatusCode::NOT_FOUND
        }
        "state_conflict" | "profile_stopped" | "profile_deleted" => StatusCode::CONFLICT,
        "sms_store_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn legacy_store_error(error: StoreError) -> Response<Full<Bytes>> {
    // The former Go public handler wrapped the private store's status and JSON
    // error in a 409 response. Preserve that public shape during the cutover.
    let code = match &error {
        StoreError::Configuration(code) => code.as_str(),
        _ => "store_io_failed",
    };
    let private_status = go_store_status(code);
    let private_body = json!({
        "error":"多待存储操作未完成",
        "code":code,
        "retryable":false
    });
    let encoded = serde_json::to_string(&private_body).unwrap_or_default();
    public_error(
        StatusCode::CONFLICT,
        &format!(
            "多待存储拒绝请求 ({}): {}",
            private_status.as_u16(),
            encoded
        ),
    )
}

fn readiness_failure(readiness: SendReadiness) -> Option<Response<Full<Bytes>>> {
    if !readiness.session_current {
        Some(public_error(StatusCode::CONFLICT, "所选号码当前不可发送"))
    } else if !readiness.channel_ready {
        Some(public_error(
            StatusCode::CONFLICT,
            "所选号码的短信通道未就绪",
        ))
    } else {
        None
    }
}

fn queue_event_id(profile_id: &str, client_request_id: &str) -> String {
    let fingerprint =
        stable_digest_hex(format!("queue\0{profile_id}\0{client_request_id}").as_bytes());
    format!("queue_{}", &fingerprint[..32])
}

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    store: crate::multistandby_store::Store,
    runtime_service: std::sync::Arc<crate::multistandby_native_service::Service>,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST || request.uri().path() != PUBLIC_PATH {
        return public_error(StatusCode::NOT_FOUND, "route not found");
    }
    let body = match crate::http_server::collect_body(request.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(_) => return public_error(StatusCode::BAD_REQUEST, "无效请求"),
    };
    // The old Go JSON decoder consumed one value and ignored trailing bytes.
    // Keep that public request behavior while validating the first JSON value.
    let mut deserializer = serde_json::Deserializer::from_slice(&body);
    let value = match Value::deserialize(&mut deserializer) {
        Ok(value) => value,
        Err(_) => return public_error(StatusCode::BAD_REQUEST, "无效请求"),
    };
    let body = match parse_send_request(value) {
        Ok(body) => body,
        Err(()) => return public_error(StatusCode::BAD_REQUEST, "无效请求"),
    };
    if body.profile_id.is_empty()
        || body.phone.trim().is_empty()
        || body.body.trim().is_empty()
        || body.client_request_id.is_empty()
    {
        return public_error(StatusCode::BAD_REQUEST, "号码、正文及请求标识不能为空");
    }
    if body.profile_id.len() > 128 {
        return public_error(StatusCode::CONFLICT, "所选号码当前不可发送");
    }

    let native = runtime_service.ready(&body.profile_id);
    let readiness = SendReadiness {
        session_current: native.session_current,
        channel_ready: native.channel_ready,
    };
    if let Some(response) = readiness_failure(readiness) {
        return response;
    }

    let event_id = queue_event_id(&body.profile_id, &body.client_request_id);
    let event = json!({
        "event_id":event_id,
        "profile_id":body.profile_id,
        "client_request_id":body.client_request_id,
        "phone":body.phone,
        "body":body.body
    });
    let mut last_error = None;
    for attempt in 0..3 {
        let store = store.clone();
        let event = event.clone();
        match crate::archive_io::run(move || store.apply("queue", event)).await {
            Ok(result) => return crate::json_response(StatusCode::ACCEPTED, &result),
            Err(error) => last_error = Some(error),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis((attempt + 1) * 200)).await;
        }
    }
    legacy_store_error(last_error.expect("three queue attempts produce an error"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ready_facts() -> ReadinessFacts {
        ReadinessFacts {
            profile_desired: true,
            session_present: true,
            session_reader: "reader-A".into(),
            session_reader_generation: 7,
            selected_reader: "reader-A".into(),
            selected_reader_generation: 7,
            tunnel_state: "connected".into(),
            ims_state: "registered".into(),
            messaging: MessagingFacts {
                registered: true,
                ipsec_installed: true,
                plain_ims: false,
                transport_present: true,
                transport_healthy: true,
            },
            sim_info_present: true,
            smsc: "+441234567890".into(),
            sms_target: "sip:sms.example.test".into(),
        }
    }

    #[test]
    fn send_request_matches_legacy_empty_null_and_unknown_field_behavior() {
        let empty = parse_send_request(json!({"profile_id":null})).unwrap();
        assert_eq!(empty.profile_id, "");
        assert_eq!(empty.phone, "");
        assert!(parse_send_request(json!({"unexpected":true})).is_err());
        assert!(parse_send_request(json!({"phone":42})).is_err());
        assert_eq!(
            parse_send_request(Value::Null).unwrap().client_request_id,
            ""
        );
        let mut deserializer =
            serde_json::Deserializer::from_slice(br#"{"profile_id":"A"} trailing"#);
        assert_eq!(
            Value::deserialize(&mut deserializer).unwrap()["profile_id"],
            "A"
        );
    }

    #[test]
    fn readiness_errors_keep_the_existing_public_status_and_text() {
        let session = readiness_failure(SendReadiness {
            session_current: false,
            channel_ready: false,
        })
        .unwrap();
        assert_eq!(session.status(), StatusCode::CONFLICT);
        let channel = readiness_failure(SendReadiness {
            session_current: true,
            channel_ready: false,
        })
        .unwrap();
        assert_eq!(channel.status(), StatusCode::CONFLICT);
        assert!(
            readiness_failure(SendReadiness {
                session_current: true,
                channel_ready: true,
            })
            .is_none()
        );
    }

    #[test]
    fn rust_computes_readiness_from_raw_reader_ims_and_sms_facts() {
        let ready = ready_facts().project().unwrap();
        assert_eq!(
            ready,
            SendReadiness {
                session_current: true,
                channel_ready: true,
            }
        );

        let mut stale = ready_facts();
        stale.selected_reader_generation += 1;
        assert_eq!(
            stale.project().unwrap(),
            SendReadiness {
                session_current: false,
                channel_ready: false,
            }
        );

        let mut plain_ims = ready_facts();
        plain_ims.messaging.ipsec_installed = false;
        plain_ims.messaging.plain_ims = true;
        assert!(plain_ims.project().unwrap().channel_ready);

        let mut unhealthy = ready_facts();
        unhealthy.messaging.transport_healthy = false;
        let projected = unhealthy.project().unwrap();
        assert!(projected.session_current);
        assert!(!projected.channel_ready);
    }

    #[test]
    fn stale_or_incomplete_session_and_sms_facts_fail_closed() {
        let mut cases: Vec<(&str, Box<dyn Fn(&mut ReadinessFacts)>)> = vec![
            (
                "profile stopped",
                Box::new(|facts| facts.profile_desired = false),
            ),
            (
                "session missing",
                Box::new(|facts| facts.session_present = false),
            ),
            (
                "reader missing",
                Box::new(|facts| facts.session_reader.clear()),
            ),
            (
                "zero generation",
                Box::new(|facts| facts.session_reader_generation = 0),
            ),
            (
                "reader mismatch",
                Box::new(|facts| facts.selected_reader = "reader-B".into()),
            ),
            (
                "generation mismatch",
                Box::new(|facts| facts.selected_reader_generation += 1),
            ),
            (
                "tunnel",
                Box::new(|facts| facts.tunnel_state = "connecting".into()),
            ),
            (
                "IMS",
                Box::new(|facts| facts.ims_state = "registering".into()),
            ),
            (
                "registration",
                Box::new(|facts| facts.messaging.registered = false),
            ),
            (
                "security",
                Box::new(|facts| {
                    facts.messaging.ipsec_installed = false;
                    facts.messaging.plain_ims = false;
                }),
            ),
            (
                "transport missing",
                Box::new(|facts| facts.messaging.transport_present = false),
            ),
            (
                "transport unhealthy",
                Box::new(|facts| facts.messaging.transport_healthy = false),
            ),
            ("SIM info", Box::new(|facts| facts.sim_info_present = false)),
            ("SMSC", Box::new(|facts| facts.smsc = " \t".into())),
            ("SMS route", Box::new(|facts| facts.sms_target.clear())),
        ];
        for (label, mutate) in cases.drain(..) {
            let mut facts = ready_facts();
            mutate(&mut facts);
            let readiness = facts
                .project()
                .unwrap_or_else(|_| panic!("invalid fixture for {label}"));
            assert!(!readiness.channel_ready, "{label} admitted SMS");
        }
    }

    #[test]
    fn readiness_facts_are_bounded_and_unknown_fields_are_rejected() {
        let mut oversized = ready_facts();
        oversized.sms_target = "x".repeat(513);
        assert!(oversized.project().is_err());
        let mut value = serde_json::to_value(ready_facts()).unwrap();
        value["derived_channel_ready"] = json!(true);
        assert!(serde_json::from_value::<ReadinessFacts>(value).is_err());
    }

    #[test]
    fn durable_event_id_is_byte_compatible_with_go_multi_id() {
        assert_eq!(
            queue_event_id("profile-A", "request-1"),
            "queue_ececf539e3694bd8085d3398a8e7d0b5"
        );
    }

    #[test]
    fn store_queue_is_idempotent_for_the_same_profile_and_request() {
        let root = std::env::temp_dir().join(format!(
            "switch-multistandby-send-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = crate::multistandby_store::Store::open(&root).unwrap();
        store
            .apply(
                "catalog",
                json!({"event_id":"catalog","profiles":[{"profile_id":"A","card_id":"C","eid":"E","iccid":"1"}]}),
            )
            .unwrap();
        store
            .apply(
                "desired",
                json!({"event_id":"desired","profile_id":"A","desired_state":"keep_online"}),
            )
            .unwrap();
        let body = json!({
            "event_id":queue_event_id("A","request-1"),
            "profile_id":"A",
            "client_request_id":"request-1",
            "phone":"+12025550123",
            "body":"offline fixture"
        });
        let first = store.apply("queue", body.clone()).unwrap();
        let replay = store.apply("queue", body).unwrap();
        assert_eq!(first, replay);
        assert_eq!(first["state"], "queued");
        std::fs::remove_dir_all(root).unwrap();
    }
}
