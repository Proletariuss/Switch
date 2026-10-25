//! Exercise both production HTTP entry points against the existing lifecycle
//! Gate. Only committed physical receipts and the compatibility peer are fixtures.
use super::*;
use crate::reader_reservation::{NativeAuthority, NativeReaderFacts, NativeTransitionKind};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::sync::{Arc, Mutex};

const ICCID: &str = "8988212345678901234";
const NEXT_ICCID: &str = "8988212345678901235";

pub(super) fn publish_reader(state: &crate::ServerState, reader: &str, iccid: &str, phone: &str) {
    state.reader_lifecycle().unwrap();
    let gate = &state.reader_gate;
    let before = gate.selection_snapshot();
    let producer = gate.native_reader_producer().unwrap();
    let transition = gate
        .prepare_native_transition(
            &producer,
            &before,
            if before.reader.is_empty() {
                NativeTransitionKind::Connect
            } else {
                NativeTransitionKind::Identity
            },
            reader,
            NativeAuthority::Idle,
        )
        .unwrap();
    let physical = (!reader.starts_with("BLE:")).then(|| crate::reader_pcsc::LifecycleBinding {
        producer,
        reader: reader.into(),
        session_id: "sms-facts-physical-receipt".into(),
        session_sequence: 1,
    });
    gate.commit_native_transition(
        &transition,
        physical.as_ref(),
        NativeReaderFacts {
            metadata: Some(json!({"iccid":iccid,"imsi":"234150123456789","phone_number":phone})),
            ..Default::default()
        },
    )
    .unwrap();
}

struct Fixture {
    root: std::path::PathBuf,
    state: crate::ServerState,
}
impl Fixture {
    fn new(reader: &str, phone: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("sr505-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let state = crate::reader_pcsc_gateway_tests::state(&root);
        publish_reader(&state, reader, ICCID, phone);
        Self { root, state }
    }
    fn remember(&self, iccid: &str, phone: &str) {
        crate::profile_notes::Store::new(self.state.multistandby_store.data_root())
            .remember_registered_phone(iccid, phone, || true)
            .unwrap();
    }
    async fn get(&self) -> Value {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let state = self.state.clone();
        let serving = tokio::spawn(async move { crate::serve_connection(server, state).await });
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let response = sender
            .send_request(
                Request::builder()
                    .method("GET")
                    .uri("/api/sms/devices")
                    .header(crate::APP_TOKEN_HEADER, "app-fixture")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        drop(sender);
        if let Err(error) = connection.await.unwrap() {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
        if let Err(error) = serving.await.unwrap() {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
        body
    }
    async fn select(&self, requested: &str, module: ModuleFacts, retired_reader: Value) -> Value {
        let body = serde_json::to_vec(&json!({"requested":requested,
            "facts":{"module":module,"reader":retired_reader}}))
        .unwrap();
        let (code, value) = crate::reader_pcsc_gateway_tests::request_with_body(
            self.state.clone(),
            SELECT_PATH,
            false,
            false,
            Some(("X-Switch-Internal-Token", "internal-fixture")),
            &body,
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{value}");
        value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Any contact with the retired peer is counted, including swallowed errors.
struct GoTrap {
    server: tokio::task::JoinHandle<()>,
    paths: Arc<Mutex<Vec<String>>>,
}
impl GoTrap {
    fn install(f: &Fixture, module: ModuleFacts, outcome: &'static str, _delayed: bool) -> Self {
        let listener = tokio::net::UnixListener::bind(&f.state.go_socket_path).unwrap();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let seen = paths.clone();
        let mut value = json!({"module":module,"reader":{"connected":true,"session_active":true,
            "identity_valid":true,"ims_registered":true,"sms_ready":true,
            "registered_phone":"+447700900999","subscription_id":"STALE-GO"}});
        if outcome == "module-only" {
            value.as_object_mut().unwrap().remove("reader");
        }
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (seen, value) = (seen.clone(), value.clone());
                let service = service_fn(move |request: Request<Incoming>| {
                    let (seen, value) = (seen.clone(), value.clone());
                    async move {
                        let path = request.uri().path().to_owned();
                        seen.lock().unwrap().push(path.clone());
                        assert_eq!(request.method(), Method::POST);
                        assert_eq!(
                            request.headers()["x-switch-internal-token"],
                            "internal-fixture"
                        );
                        assert_eq!(request.headers()[crate::APP_TOKEN_HEADER], "app-fixture");
                        assert_eq!(
                            request
                                .into_body()
                                .collect()
                                .await
                                .unwrap()
                                .to_bytes()
                                .as_ref(),
                            b"{}"
                        );
                        let bytes = if outcome == "malformed" {
                            b"not JSON".to_vec()
                        } else {
                            serde_json::to_vec(&value).unwrap()
                        };
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(if path != FACTS_PATH || outcome == "failure" {
                                    StatusCode::BAD_GATEWAY
                                } else {
                                    StatusCode::OK
                                })
                                .body(Full::new(Bytes::from(bytes)))
                                .unwrap(),
                        )
                    }
                });
                // socket_is_live opens a separate connection without HTTP.
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            }
        });
        f.state.availability.mark_ready();
        Self { server, paths }
    }
    fn assert_module_only(&self, count: usize) {
        assert_eq!(
            *self.paths.lock().unwrap(),
            vec![FACTS_PATH.to_owned(); count]
        );
    }
}
impl Drop for GoTrap {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn assert_passive(value: &Value, phone: &str) {
    let reader = &value["devices"][1];
    assert_eq!(reader["available"], true, "{value}");
    assert_eq!(reader["ready"], false, "{value}");
    assert_eq!(reader["phone"], phone, "{value}");
    assert!(reader.get("subscription_id").is_none(), "{value}");
    assert_eq!(reader["detail"], "读卡器 IMS 尚未注册", "{value}");
}

#[tokio::test]
async fn passive_pcsc_and_ble_http_facts_are_local_with_go_absent_or_alive() {
    for reader in ["Reader A", "BLE:SMS fixture"] {
        let f = Fixture::new(reader, "+447700900444");
        f.remember(ICCID, "+447700900555");
        assert!(
            f.state
                .native_vowifi_lifecycle
                .reader_observation()
                .is_none()
        );
        let absent = f.get().await;
        assert_passive(&absent, "+447700900444");
        assert_eq!(absent["devices"][0]["detail"], "未检测到 DJI 模块");
        assert_eq!(absent["devices"][0]["ready"], false);
        let trap = GoTrap::install(&f, ModuleFacts::default(), "module-only", false);
        let alive = f.get().await;
        assert_passive(&alive, "+447700900444");
        assert_eq!(alive["devices"][0]["detail"], "未检测到 DJI 模块");
        publish_reader(&f.state, reader, ICCID, "unknown");
        assert_passive(&f.get().await, "+447700900555");
        trap.assert_module_only(0);
    }
}

#[tokio::test]
async fn internal_selection_discards_legacy_reader_facts_and_preserves_module_errors() {
    let f = Fixture::new("Reader A", "+447700900444");
    let trap = GoTrap::install(&f, ModuleFacts::default(), "success", false);
    for old in [
        json!({"connected":true,"session_active":true,"identity_valid":true,"ims_registered":true,"sms_ready":true,"registered_phone":"+447700900999","subscription_id":"STALE-GO"}),
        json!("invalid retired shape"),
        json!({"sms_ready":true,"saved_phone":"x".repeat(4096)}),
    ] {
        let module = ModuleFacts {
            modem_present: true,
            ..Default::default()
        };
        assert_eq!(
            f.select("", module.clone(), old.clone()).await["error"]["code"],
            "sms_device_unavailable"
        );
        let denied = f.select("reader", module, old.clone()).await;
        assert_eq!(denied["error"]["code"], "sms_device_not_ready");
        assert_eq!(denied["error"]["status"], 503);
        let unavailable = f.select("", ModuleFacts::default(), old.clone()).await;
        assert_eq!(unavailable["error"]["code"], "sms_device_unavailable");
        let unknown = f.select("bogus", ModuleFacts::default(), old).await;
        assert_eq!(unknown["error"]["code"], "invalid_sms_device");
    }
    trap.assert_module_only(0);
}

#[tokio::test]
async fn retired_peer_cannot_restore_pending_or_previous_reader_generation() {
    for phase in ["pending", "replaced", "disconnected"] {
        for outcome in ["success", "failure", "malformed"] {
            let f = Arc::new(Fixture::new("BLE:SMS fixture", "+447700900444"));
            f.remember(NEXT_ICCID, "+447700900666");
            let trap = GoTrap::install(&f, ModuleFacts::default(), outcome, true);
            let gate = &f.state.reader_gate;
            let before = gate.selection_snapshot();
            let producer = gate.native_reader_producer().unwrap();
            let transition = gate
                .prepare_native_transition(
                    &producer,
                    &before,
                    if phase == "disconnected" {
                        NativeTransitionKind::Disconnect
                    } else {
                        NativeTransitionKind::Identity
                    },
                    &before.reader,
                    NativeAuthority::Idle,
                )
                .unwrap();
            if phase != "pending" {
                gate.commit_native_transition(
                    &transition,
                    None,
                    NativeReaderFacts {
                        metadata: Some(json!({"iccid":NEXT_ICCID,"phone_number":""})),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            let value = f.get().await;
            if phase == "replaced" {
                assert!(gate.selection_snapshot().generation > before.generation);
                assert_passive(&value, "+447700900666");
            } else {
                assert_eq!(
                    value["devices"][1]["available"], false,
                    "{phase}/{outcome}: {value}"
                );
                assert_eq!(value["devices"][1]["ready"], false);
                assert!(value["devices"][1].get("phone").is_none());
            }
            assert_eq!(value["devices"][0]["detail"], "未检测到 DJI 模块");
            let selected = f
                .select("reader", ModuleFacts::default(), json!({"sms_ready":true}))
                .await;
            assert_eq!(selected["error"]["code"], "sms_device_not_ready");
            trap.assert_module_only(0);
            if phase == "pending" {
                gate.abort_native_transition(&transition).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn internal_selection_accepts_module_only_contract_without_reader_snapshot_rpc() {
    let f = Fixture::new("BLE:SMS fixture", "+447700900444");
    let trap = GoTrap::install(&f, ModuleFacts::default(), "module-only", false);
    let body = serde_json::to_vec(&json!({"requested":"reader",
        "facts":{"module":ModuleFacts::default()}}))
    .unwrap();
    let (code, value) = crate::reader_pcsc_gateway_tests::request_with_body(
        f.state.clone(),
        SELECT_PATH,
        false,
        false,
        Some(("X-Switch-Internal-Token", "internal-fixture")),
        &body,
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{value}");
    assert_eq!(value["error"]["code"], "sms_device_not_ready");
    assert_eq!(value["error"]["status"], 503);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("+447700900444")
    );
    trap.assert_module_only(0);
}

#[test]
fn compatibility_decoder_requires_bounded_module_and_discards_any_retired_reader_shape() {
    for reader in [
        Value::Null,
        json!(42),
        json!([true, "legacy"]),
        json!({"bogus":"ignored"}),
    ] {
        let input = json!({"requested":"reader", "facts":{"module":ModuleFacts::default(),"reader":reader}});
        assert!(decode_selection_input(&serde_json::to_vec(&input).unwrap()).is_ok());
    }
    for input in [
        json!({"requested":"reader","facts":{"reader":{}}}),
        json!({"requested":"reader","facts":{"module":null}}),
        json!({"requested":"reader","facts":{"module":ModuleFacts::default(),"unexpected":true}}),
        json!({"requested":"reader","facts":{"module":ModuleFacts { phone:"x".repeat(129),..Default::default() }}}),
    ] {
        assert!(
            decode_selection_input(&serde_json::to_vec(&input).unwrap()).is_err(),
            "{input}"
        );
    }
}
