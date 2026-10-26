use super::*;
use std::{
    fs,
    sync::{Mutex, atomic::AtomicUsize},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::Notify,
};

#[derive(Default)]
struct Hardware {
    captures: AtomicUsize,
    sends: AtomicUsize,
    released: AtomicUsize,
    validations: AtomicUsize,
    valid: AtomicBool,
    lose_ack: AtomicBool,
    fail_send: AtomicBool,
    block_send: AtomicBool,
    block_release: AtomicBool,
    release_entered: Notify,
    release_resume: Notify,
    entered: Notify,
    resume: Notify,
    bindings: Mutex<BTreeMap<String, Value>>,
    capture_error: Mutex<Option<(u16, Value)>>,
    owner: Mutex<String>,
}
struct Fixture {
    root: PathBuf,
    service: Service,
    hardware: Arc<Hardware>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    async fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ds-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("go.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let hardware = Arc::new(Hardware::default());
        hardware.valid.store(true, Ordering::Relaxed);
        let shared = hardware.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let h = shared.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let body = loop {
                        let mut part = [0; 4096];
                        let n = stream.read(&mut part).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&part[..n]);
                        assert!(bytes.len() < MAX_BODY + 4096);
                        if let Some(at) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..at]).to_lowercase();
                            assert!(head.contains("post /api/internal/device-sms-route "));
                            assert!(head.contains("x-switch-internal-token: secret"));
                            assert!(head.contains("x-proletarius-app-token: app"));
                            let len = head
                                .lines()
                                .find_map(|s| s.strip_prefix("content-length:"))
                                .unwrap()
                                .trim()
                                .parse::<usize>()
                                .unwrap();
                            if bytes.len() >= at + 4 + len {
                                break serde_json::from_slice::<Value>(
                                    &bytes[at + 4..at + 4 + len],
                                )
                                .unwrap();
                            }
                        }
                    };
                    let mut status = 200;
                    let value = match body["action"].as_str().unwrap() {
                        "capture" => {
                            if let Some((error_status, error)) =
                                h.capture_error.lock().unwrap().clone()
                            {
                                status = error_status;
                                error
                            } else {
                                let index = h.captures.fetch_add(1, Ordering::SeqCst);
                                let binding = format!("binding-{index}");
                                h.bindings
                                    .lock()
                                    .unwrap()
                                    .insert(binding.clone(), body.clone());
                                json!({"binding":binding,"device":"dji","subscription_id":"","owner":h.owner.lock().unwrap().clone()})
                            }
                        }
                        "validate" => {
                            h.validations.fetch_add(1, Ordering::SeqCst);
                            let bindings = h.bindings.lock().unwrap();
                            let valid = body["bindings"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|b| {
                                    let b = b.as_str().unwrap();
                                    (
                                        b.to_owned(),
                                        h.valid.load(Ordering::SeqCst) && bindings.contains_key(b),
                                    )
                                })
                                .collect::<BTreeMap<_, _>>();
                            json!({"valid":valid})
                        }
                        "release" => {
                            h.release_entered.notify_one();
                            if h.block_release.load(Ordering::SeqCst) {
                                h.release_resume.notified().await;
                            }
                            h.released.fetch_add(1, Ordering::SeqCst);
                            h.bindings
                                .lock()
                                .unwrap()
                                .remove(body["binding"].as_str().unwrap());
                            json!({"released":true})
                        }
                        "execute" => {
                            let bound = h
                                .bindings
                                .lock()
                                .unwrap()
                                .remove(body["binding"].as_str().unwrap());
                            assert!(bound.is_some(), "an execute was repeated");
                            h.sends.fetch_add(1, Ordering::SeqCst);
                            h.entered.notify_one();
                            if h.block_send.load(Ordering::SeqCst) {
                                h.resume.notified().await;
                            }
                            if h.lose_ack.load(Ordering::SeqCst) {
                                return;
                            }
                            json!({"accepted":!h.fail_send.load(Ordering::SeqCst),"segments":2})
                        }
                        other => panic!("unexpected bridge action {other}"),
                    };
                    let json = serde_json::to_vec(&value).unwrap();
                    let header = format!(
                        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        json.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&json).await;
                });
            }
        });
        let service = Service {
            job: None,
            #[cfg(test)]
            shutdown_hooks: Default::default(),
            #[cfg(test)]
            archive_executor: None,
            compatibility_fixture: true,
            store: crate::device_send_store::Store::open(&root).unwrap(),
            archive: ArchiveStore::new(root.join("sms-archive.json")),
            socket,
            app_token: Some("app".into()),
            internal_token: "secret".into(),
            usb_at: crate::usb_at::Owner::unavailable_for_tests("usb-producer-fixture".into()),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            module_radio: crate::test_module_radio(&root),
            availability: {
                let availability = crate::availability::BackendAvailability::new();
                availability.mark_ready();
                availability
            },
            native_sms: Arc::new(RuntimeReaderSms(Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ))),
            native_module_sms: Arc::new(RuntimeModuleSms(Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ))),
            io_timeout: RPC_TIMEOUT,
            claims: Arc::new(Semaphore::new(16)),
        };
        Self {
            root,
            service,
            hardware,
            task,
        }
    }
    async fn prepare(&self, id: &str) -> Value {
        self.service
            .post(request(id), true, &AtomicBool::new(false))
            .await
            .unwrap_or_else(|_| panic!("prepare failed"))
    }
    async fn public_request(
        &self,
        method: Method,
        path: &str,
        body: &str,
        authenticated: bool,
        ready: bool,
    ) -> (StatusCode, Value) {
        let availability = crate::BackendAvailability::new();
        if ready {
            availability.mark_ready();
        }
        let sms_inbox = crate::message_archive::InboxState::default();
        let state = crate::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("app".into()),
            internal_token: "secret".into(),
            multistandby_store: crate::multistandby_store::Store::open(&self.root).unwrap(),
            multistandby_operations: crate::multistandby_operations::Store::open(&self.root)
                .unwrap(),
            device_send_store: self.service.store.clone(),
            forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&self.root)
                .unwrap(),
            sms_forwarding: crate::sms_forwarding::Service::default(),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(
                sms_inbox.clone(),
                self.root.join("sms-archive.json"),
            ),
            call_archive: ArchiveStore::new(self.root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                ArchiveStore::new(self.root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: self.service.socket.clone(),
            availability,
            reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
            snapshot_instance_id: "test".into(),
            snapshot_revision: Arc::new(crate::AtomicU64::new(0)),
            eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: crate::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&self.root),
            native_vowifi_lifecycle: std::sync::Arc::new(
                crate::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
            multi_runtime_report: Arc::new(Mutex::new(
                crate::multistandby_runtime_report::Journal::default(),
            )),
        };
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move { crate::serve_connection(server, state).await });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
                .await
                .unwrap();
        let task = tokio::spawn(connection);
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        if authenticated {
            builder = builder.header(crate::APP_TOKEN_HEADER, "app");
        }
        let response = sender
            .send_request(
                builder
                    .body(Full::new(Bytes::from(body.to_owned())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        drop(sender);
        task.abort();
        server_task.abort();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[test]
fn rust_usb_route_binding_keeps_go_capability_opaque_and_reversible() {
    let raw = "a1b2c3-route";
    let stored = format!("{RUST_USB_BINDING_PREFIX}42:{raw}");
    assert_eq!(go_binding(&stored), raw);
    assert_eq!(go_binding(raw), raw);
    assert_eq!(usb_route_generation(&stored), Some(42));
    assert_eq!(
        usb_route_generation(&format!("{RUST_USB_BINDING_PREFIX}bad:{raw}")),
        None
    );
}

#[test]
fn exact_serial_binding_requires_epoch_session_and_sim_generation() {
    let token = "rust_modem_session_v2:epoch-1:4:7";
    let binding = serial_session_binding(token).unwrap();
    assert_eq!(binding.owner_epoch, "epoch-1");
    assert_eq!(binding.serial_session, 4);
    assert_eq!(binding.sim_generation, 7);
    assert!(!compatibility_binding(token));
    for old in [
        "rust_modem_sms_generation:7",
        "rust_modem:7:old-go-token",
        "rust_modem_session_v2:epoch-1:0:7",
        "rust_modem_session_v2:epoch-1:4:0",
        "rust_modem_session_v2:epoch-1:4:7:extra",
        "rust_modem_session_v2:epoch-1:04:7",
    ] {
        assert!(serial_session_binding(old).is_none(), "{old}");
        assert!(
            !compatibility_binding(old),
            "retired or malformed native token must not reach Go"
        );
    }
}

#[test]
fn usb_capture_must_match_the_configured_module_radio_session() {
    assert_eq!(
        matching_usb_transport_binding(
            Some("current-session"),
            Some(("current-session".into(), 9))
        ),
        Some(("current-session".into(), 9))
    );
    assert!(
        matching_usb_transport_binding(
            Some("current-session"),
            Some(("retired-session".into(), 9))
        )
        .is_none()
    );
    assert!(matching_usb_transport_binding(None, Some(("session".into(), 9))).is_none());
    assert!(matching_usb_transport_binding(Some("session"), Some(("session".into(), 0))).is_none());
}

#[tokio::test]
async fn rust_usb_session_binding_is_validated_without_go_route_rpc() {
    let f = Fixture::new().await;
    let binding = format!("{RUST_USB_SESSION_BINDING_PREFIX}session-token:42");
    assert_eq!(
        usb_route_session_generation(&binding),
        Some(("session-token", 42))
    );
    assert_eq!(usb_route_generation(&binding), Some(42));
    assert_eq!(go_binding(&binding), binding);

    let valid = f.service.valid(vec![binding.clone()]).await;
    assert_eq!(valid.get(&binding), Some(&false));
    assert_eq!(f.hardware.validations.load(Ordering::SeqCst), 0);
    f.service.release(&binding).await;
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), 0);
}

fn request(id: &str) -> SendRequest {
    SendRequest {
        identity_version: 1,
        intent_nonce: String::new(),
        client_request_id: id.into(),
        phone: "65075".into(),
        message: "fixture-only".into(),
        device: "dji".into(),
        expected_subscription_id: String::new(),
    }
}
fn code<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected failure"),
        Err(Failure(_, v)) => v["code"].as_str().unwrap().into(),
    }
}

#[tokio::test]
async fn quarantine_module_sms_rejects_before_capture_or_claim_and_preserves_prior_outcomes() {
    let mut f = Fixture::new().await;
    f.prepare("prepared-before-quarantine").await;
    f.prepare("accepted-before-quarantine").await;
    f.service
        .post(
            request("accepted-before-quarantine"),
            false,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
    f.service.module_radio = crate::module_radio::Service::new(
        f.root.clone(),
        f.service.usb_at.clone(),
        f.service.modem_actor.clone(),
        crate::BackendAvailability::with_module_recovery(
            crate::module_recovery::Gate::quarantined_for_tests(),
        ),
        "synthetic-quarantined".into(),
    );
    let captures = f.hardware.captures.load(Ordering::SeqCst);
    let validations = f.hardware.validations.load(Ordering::SeqCst);
    assert_eq!(
        code(
            f.service
                .post(request("new-module"), true, &AtomicBool::new(false))
                .await
        ),
        crate::module_recovery::CODE
    );
    let prepared: Record = f
        .service
        .ledger(
            "get",
            json!({"client_request_id":"prepared-before-quarantine"}),
        )
        .await
        .unwrap();
    assert_eq!(
        code(f.service.submit(prepared, &AtomicBool::new(false)).await),
        crate::module_recovery::CODE
    );
    let still_prepared: Record = f
        .service
        .ledger(
            "get",
            json!({"client_request_id":"prepared-before-quarantine"}),
        )
        .await
        .unwrap();
    assert_eq!(still_prepared.state, "prepared");
    assert!(still_prepared.attempt.is_empty());
    let accepted: Record = f
        .service
        .ledger(
            "get",
            json!({"client_request_id":"accepted-before-quarantine"}),
        )
        .await
        .unwrap();
    assert!(
        f.service
            .submit(accepted, &AtomicBool::new(false))
            .await
            .is_ok()
    );
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), captures);
    assert_eq!(f.hardware.validations.load(Ordering::SeqCst), validations);
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.service.get("").await.unwrap()["intents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn request_wire_preserves_json_null_case_and_message_whitespace() {
    let mut r: SendRequest = serde_json::from_str(
        r#"{"PHONE":" 65075 ","phone":null,"message":"  text\n","DEVICE":" DJI "}"#,
    )
    .unwrap();
    assert!(r.normalize());
    assert_eq!(r.phone, "65075");
    assert_eq!(r.device, "dji");
    assert_eq!(r.message, "  text\n");
    assert!(serde_json::from_str::<SendRequest>(r#"{"unknown":true}"#).is_err());
    assert!(serde_json::from_str::<SendRequest>(r#"{"phone":42}"#).is_err());
    let mut r: SendRequest = serde_json::from_str("null").unwrap();
    assert!(!r.normalize());
    let mut r = request("id");
    r.message = "中".repeat(22_000);
    assert!(!r.normalize());
}

#[test]
fn query_matches_go_duplicate_unknown_and_bad_escape_rules() {
    assert_eq!(
        query_fields("client_request_id=a&cursor=").unwrap_or_else(|_| panic!()),
        ("a".into(), String::new())
    );
    assert_eq!(
        query_fields("client_request_id=%ZZ&cursor=a").unwrap_or_else(|_| panic!()),
        (String::new(), "a".into())
    );
    assert_eq!(
        query_fields("ignored=a;b&cursor=a").unwrap_or_else(|_| panic!()),
        (String::new(), "a".into())
    );
    for q in [
        "client_request_id=a&client_request_id=b",
        "client_request_id=a&cursor=b",
        "other=x",
        "cursor=white+space",
    ] {
        assert_eq!(code(query_fields(q)), "invalid_request");
    }
}

#[tokio::test]
async fn prepare_is_durable_idempotent_and_public_fields_are_redacted() {
    let f = Fixture::new().await;
    let first = f.prepare("request").await;
    assert_eq!(first["can_submit"], true);
    assert!(first.get("binding").is_none());
    assert!(first.get("attempt").is_none());
    assert!(first.get("subscription_id").is_none());
    assert_eq!(f.prepare("request").await, first);
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 1);
    let mut changed = request("request");
    changed.message.push('!');
    assert_eq!(
        code(f.service.post(changed, true, &AtomicBool::new(false)).await),
        "sms_intent_conflict"
    );
    f.hardware.valid.store(false, Ordering::SeqCst);
    assert_eq!(f.prepare("request").await["can_submit"], false);
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rust_usb_binding_dispatches_to_usb_owner_and_never_go_execute() {
    let f = Fixture::new().await;
    let binding = f.service.capture(&request("rust-usb")).await.unwrap();
    let stored_binding = format!("{RUST_USB_BINDING_PREFIX}42:{}", binding.binding);
    let record: Record = f
        .service
        .ledger(
            "prepare",
            json!({
                "client_request_id":"rust-usb",
                "phone":"65075",
                "message":"fixture SMS",
                "device":"dji",
                "subscription_id":"",
                "binding":stored_binding
            }),
        )
        .await
        .unwrap();
    assert!(record.binding.starts_with(RUST_USB_BINDING_PREFIX));

    let failure = f
        .service
        .submit(record, &AtomicBool::new(false))
        .await
        .unwrap_err();
    assert_eq!(failure.1["code"], "sms_send_outcome_unknown");
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn legacy_modem_generation_binding_expires_without_go_or_physical_execute() {
    let f = Fixture::new().await;
    let binding = f.service.capture(&request("rust-modem")).await.unwrap();
    let stored_binding = format!("{RUST_MODEM_BINDING_PREFIX}7:{}", binding.binding);
    let record: Record = f
        .service
        .ledger(
            "prepare",
            json!({
                "client_request_id":"rust-modem",
                "phone":"+441234567890",
                "message":"fixture SMS",
                "device":"dji",
                "subscription_id":"",
                "binding":stored_binding
            }),
        )
        .await
        .unwrap();
    assert!(record.binding.starts_with(RUST_MODEM_BINDING_PREFIX));
    assert!(serial_session_binding(&record.binding).is_none());
    assert_eq!(go_binding(&record.binding), binding.binding);

    let failure = f
        .service
        .submit(record, &AtomicBool::new(false))
        .await
        .unwrap_err();
    assert_eq!(failure.1["code"], "sms_intent_binding_expired");
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.validations.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn id_required_for_prepare_and_known_id_send_does_not_auto_prepare() {
    let f = Fixture::new().await;
    assert_eq!(
        code(
            f.service
                .post(request(""), true, &AtomicBool::new(false))
                .await
        ),
        "sms_client_request_id_required"
    );
    assert_eq!(
        code(
            f.service
                .post(request("missing"), false, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_not_found"
    );
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_submit_consumes_one_permission_and_archives_once() {
    let f = Fixture::new().await;
    f.prepare("same").await;
    let mut jobs = Vec::new();
    for _ in 0..12 {
        let service = f.service.clone();
        jobs.push(tokio::spawn(async move {
            service
                .post(request("same"), false, &AtomicBool::new(false))
                .await
        }));
    }
    for job in jobs {
        let _ = job.await.unwrap();
    }
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
    let record: Record = f
        .service
        .ledger("get", json!({"client_request_id":"same"}))
        .await
        .unwrap_or_else(|_| panic!());
    assert_eq!(record.state, "accepted");
    assert!(record.archive_confirmed);
    let items = f.service.archive.load().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["send_request_id"], "same");
    assert_eq!(items[0]["direction"], "out");
    assert!(
        time::OffsetDateTime::parse(
            items[0]["timestamp"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339
        )
        .is_ok()
    );
}

#[tokio::test]
async fn lost_execute_ack_is_unknown_and_cannot_be_replayed() {
    let f = Fixture::new().await;
    f.prepare("lost").await;
    f.hardware.lose_ack.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(
            code(
                f.service
                    .post(request("lost"), false, &AtomicBool::new(false))
                    .await
            ),
            "sms_send_outcome_unknown"
        );
    }
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.service
            .get("client_request_id=lost")
            .await
            .unwrap_or_else(|_| panic!())["state"],
        "unknown"
    );
    assert!(f.service.archive.load().unwrap().is_empty());
}

#[tokio::test]
async fn partial_acceptance_and_failed_archive_stay_non_replayable() {
    for fail_send in [true, false] {
        let f = Fixture::new().await;
        f.prepare("failure").await;
        f.hardware.fail_send.store(fail_send, Ordering::SeqCst);
        if !fail_send {
            fs::create_dir(f.root.join("sms-archive.json")).unwrap();
        }
        let expected = if fail_send {
            "sms_send_outcome_unknown"
        } else {
            "sms_send_archive_outcome_unknown"
        };
        for _ in 0..2 {
            assert_eq!(
                code(
                    f.service
                        .post(request("failure"), false, &AtomicBool::new(false))
                        .await
                ),
                expected
            );
        }
        assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn projecting_submitting_does_not_mutate_ledger() {
    let f = Fixture::new().await;
    f.prepare("pending").await;
    let record: Record = f
        .service
        .ledger("get", json!({"client_request_id":"pending"}))
        .await
        .unwrap_or_else(|_| panic!());
    let _: Record = f
        .service
        .ledger(
            "claim",
            json!({"client_request_id":"pending","binding":record.binding,"attempt":"attempt"}),
        )
        .await
        .unwrap_or_else(|_| panic!());
    let projected = f
        .service
        .get("client_request_id=pending")
        .await
        .unwrap_or_else(|_| panic!());
    assert_eq!(projected["state"], "unknown");
    assert_eq!(projected["can_submit"], false);
    assert!(projected.get("binding").is_none());
    let still: Record = f
        .service
        .ledger("get", json!({"client_request_id":"pending"}))
        .await
        .unwrap_or_else(|_| panic!());
    assert_eq!(still.state, "submitting");
    f.service.store.recover().unwrap();
    assert_eq!(
        code(
            f.service
                .post(request("pending"), false, &AtomicBool::new(false))
                .await
        ),
        "sms_send_outcome_unknown"
    );
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expired_binding_and_changed_payload_do_not_claim_or_send() {
    let f = Fixture::new().await;
    f.prepare("old").await;
    let mut body = request("old");
    body.expected_subscription_id = "another".into();
    assert_eq!(
        code(f.service.post(body, false, &AtomicBool::new(false)).await),
        "sms_intent_conflict"
    );
    f.hardware.valid.store(false, Ordering::SeqCst);
    assert_eq!(
        code(
            f.service
                .post(request("old"), false, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_binding_expired"
    );
    let record: Record = f
        .service
        .ledger("get", json!({"client_request_id":"old"}))
        .await
        .unwrap_or_else(|_| panic!());
    assert_eq!(record.state, "prepared");
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn legacy_submit_without_client_identity_is_rejected_before_hardware() {
    let f = Fixture::new().await;
    assert_eq!(
        code(
            f.service
                .post(request(""), false, &AtomicBool::new(false))
                .await
        ),
        "sms_client_request_id_required"
    );
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    assert!(f.service.archive.load().unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_http_waiter_keeps_finishing_an_already_started_send() {
    let f = Fixture::new().await;
    f.prepare("cancel").await;
    f.hardware.block_send.store(true, Ordering::SeqCst);
    let service = f.service.clone();
    let waiter = tokio::spawn(async move { supervise(service, request("cancel"), false).await });
    tokio::time::timeout(Duration::from_secs(3), f.hardware.entered.notified())
        .await
        .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    f.hardware.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let record: Record = f
                .service
                .ledger("get", json!({"client_request_id":"cancel"}))
                .await
                .unwrap_or_else(|_| panic!());
            if record.state == "accepted" {
                assert!(record.archive_confirmed);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_prepare_releases_only_losing_binding() {
    let f = Fixture::new().await;
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let service = f.service.clone();
        jobs.push(tokio::spawn(async move {
            service
                .post(request("race"), true, &AtomicBool::new(false))
                .await
        }));
    }
    for job in jobs {
        assert!(job.await.unwrap().is_ok());
    }
    assert_eq!(f.hardware.bindings.lock().unwrap().len(), 1);
    assert!(
        f.service
            .post(request("race"), false, &AtomicBool::new(false))
            .await
            .is_ok()
    );
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn corrupt_ledger_refuses_new_submit_and_does_not_capture_again() {
    let f = Fixture::new().await;
    f.prepare("corrupt").await;
    fs::write(f.root.join("device-send-ledger.json"), b"not JSON").unwrap();
    assert_eq!(
        code(
            f.service
                .post(request("corrupt"), false, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_store_unavailable"
    );
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn go_selection_errors_preserve_status_code_and_recoverable_contract() {
    let f = Fixture::new().await;
    for (status, code, recoverable) in [
        (409, "sms_device_required", false),
        (400, "invalid_sms_device", false),
        (503, "sms_device_not_ready", true),
    ] {
        let mut value = json!({"error":"fixture device selection","code":code});
        if recoverable {
            value["recoverable"] = json!(true);
        }
        *f.hardware.capture_error.lock().unwrap() = Some((status, value.clone()));
        let failure = f
            .service
            .post(request("selection"), true, &AtomicBool::new(false))
            .await
            .unwrap_err();
        assert_eq!(failure.0.as_u16(), status);
        assert_eq!(failure.1, value);
    }
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn public_gateway_auth_body_and_missing_native_device_do_not_consult_retired_peer() {
    let f = Fixture::new().await;
    let payload =
        r#"{"client_request_id":"public","phone":"65075","message":"fixture-only","device":"dji"}"#;
    for ready in [false, true] {
        let (status, _) = f
            .public_request(Method::POST, "/api/sms/send-intents", payload, false, ready)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, value) = f
            .public_request(Method::POST, "/api/sms/send-intents", payload, true, ready)
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(value["code"], "sms_device_not_ready");
        let (status, value) = f
            .public_request(Method::POST, "/api/sms/send", payload, true, ready)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["code"], "sms_intent_not_found");
    }
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_public_write_is_rejected_without_retired_backend_recovery_gate() {
    let f = Fixture::new().await;
    for path in ["/api/sms/send", "/api/sms/send-intents"] {
        for body in [
            "{",
            "[]",
            "{} {}",
            "{}",
            r#"{"phone":"1","message":"x","unknown":true}"#,
        ] {
            let (status, value) = f
                .public_request(Method::POST, path, body, true, false)
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(value["error"].is_string());
            let (status, value) = f.public_request(Method::POST, path, body, true, true).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(value["error"].is_string());
        }
    }
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.validations.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    let page = f.service.get("").await.unwrap();
    assert!(page["intents"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn known_compatibility_prepared_and_accepted_posts_keep_recovery_error_without_new_io() {
    let f = Fixture::new().await;
    f.prepare("prepared-before-recovery").await;
    f.prepare("accepted-before-recovery").await;
    f.service
        .post(
            request("accepted-before-recovery"),
            false,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
    f.service.availability.mark_starting();
    let captures = f.hardware.captures.load(Ordering::SeqCst);
    let validations = f.hardware.validations.load(Ordering::SeqCst);
    for id in ["prepared-before-recovery", "accepted-before-recovery"] {
        for prepare_only in [true, false] {
            assert_eq!(
                code(
                    f.service
                        .post(request(id), prepare_only, &AtomicBool::new(false))
                        .await
                ),
                "go_backend_restarting_write_not_replayed"
            );
        }
    }
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), captures);
    assert_eq!(f.hardware.validations.load(Ordering::SeqCst), validations);
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
    let prepared: Record = f
        .service
        .ledger(
            "get",
            json!({"client_request_id":"prepared-before-recovery"}),
        )
        .await
        .unwrap();
    assert_eq!(prepared.state, "prepared");
    assert!(prepared.attempt.is_empty());
}

#[tokio::test]
async fn claim_wait_timeout_never_authorizes_execution_even_if_write_finishes_later() {
    let mut f = Fixture::new().await;
    f.prepare("slow").await;
    let record: Record = f
        .service
        .ledger("get", json!({"client_request_id":"slow"}))
        .await
        .unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.root.join("device-send-ledger.json.lock"))
        .unwrap();
    lock.lock().unwrap();
    f.service.io_timeout = Duration::from_millis(50);
    let failure = f
        .service
        .submit(record, &AtomicBool::new(false))
        .await
        .unwrap_err();
    assert_eq!(failure.1["code"], "sms_intent_store_unavailable");
    lock.unlock().unwrap();
    f.service.io_timeout = RPC_TIMEOUT;
    // Wait for the actual late claim and its cleanup, rather than assuming a
    // blocked worker has started before the short response timeout expires.
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f.hardware.bindings.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), 1);
    // The late write never carries permission into another submit continuation.
    f.service.store.recover().unwrap();
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    let current: Record = f
        .service
        .ledger("get", json!({"client_request_id":"slow"}))
        .await
        .unwrap();
    assert!(!current.can_submit);
    assert!(matches!(current.state.as_str(), "unknown" | "submitting"));
}

#[tokio::test]
async fn send_wait_timeout_detaches_submitted_owner_until_finalization() {
    let finished = Arc::new(AtomicBool::new(false));
    let task_finished = finished.clone();
    let owner = owner::Owner::default();
    let job = owner.admit().unwrap();
    let task = job.spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        task_finished.store(true, Ordering::Release);
        Ok(json!({"sent":true}))
    });

    let result = await_supervisor(task, Duration::from_millis(1)).await;
    assert_eq!(code(result), "sms_send_outcome_unknown");
    assert!(!finished.load(Ordering::Acquire));

    tokio::time::timeout(Duration::from_secs(1), async {
        while !finished.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn abandoned_claim_receipt_releases_only_its_own_winning_attempt() {
    for prior_winner in [false, true] {
        let f = Fixture::new().await;
        f.prepare("receipt").await;
        let record: Record = f
            .service
            .ledger("get", json!({"client_request_id":"receipt"}))
            .await
            .unwrap();
        if prior_winner {
            let winner: Record = f.service.ledger("claim", json!({"client_request_id":"receipt","binding":record.binding,"attempt":"other-winner"})).await.unwrap();
            assert!(winner.can_submit);
        }
        let job = f.service.start_claim(&record, "abandoned".into()).unwrap();
        // Cover both a dropped result and a received result whose explicit
        // acceptance is lost; a successful send into oneshot is not an ACK.
        if prior_winner {
            drop(job.result);
        } else {
            assert!(job.result.await.unwrap().unwrap().can_submit);
        }
        drop(job.accepted);
        tokio::time::timeout(Duration::from_secs(3), job._finished)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
        assert_eq!(
            f.hardware.released.load(Ordering::SeqCst),
            usize::from(!prior_winner)
        );
        assert_eq!(
            f.hardware.bindings.lock().unwrap().len(),
            usize::from(prior_winner)
        );
        assert_eq!(f.service.claims.available_permits(), 16);
        if prior_winner {
            // The losing abandoned waiter must not revoke the real winner's
            // pending hardware permission before it reaches execute.
            let sent = f
                .service
                .rpc(
                    json!({"action":"execute","binding":record.binding}),
                    RPC_TIMEOUT,
                )
                .await
                .unwrap();
            assert_eq!(sent["accepted"], true);
        }
    }
}

#[tokio::test]
async fn retired_bindings_expire_with_live_peer_without_consuming_ledger_permission() {
    let mut f = Fixture::new().await;
    f.prepare("retired").await;
    let captures = f.hardware.captures.load(Ordering::SeqCst);
    let validations = f.hardware.validations.load(Ordering::SeqCst);
    let released = f.hardware.released.load(Ordering::SeqCst);
    f.service.compatibility_fixture = false;
    let value = f.service.get("client_request_id=retired").await.unwrap();
    assert_eq!(value["can_submit"], false);
    assert_eq!(
        code(
            f.service
                .post(request("retired"), false, &AtomicBool::new(false))
                .await
        ),
        "sms_intent_binding_expired"
    );
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), captures);
    assert_eq!(f.hardware.validations.load(Ordering::SeqCst), validations);
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), released);
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    let record: Record = f
        .service
        .ledger("get", json!({"client_request_id":"retired"}))
        .await
        .unwrap();
    assert_eq!(record.state, "prepared");
    assert!(record.attempt.is_empty());
}

#[tokio::test]
async fn cancelled_durable_ledger_refusal_is_clean_but_real_io_remains_orphan_failure() {
    for real_io in [false, true] {
        let f = Fixture::new().await;
        f.prepare("original").await;
        let executor = Arc::new(crate::archive_io::IsolatedTestExecutor::new());
        let owner = f.service.store.send_owner();
        let mut service = f.service.clone();
        service.job = Some(owner.admit().unwrap());
        service.archive_executor = Some(executor.clone());
        service.io_timeout = Duration::from_millis(10);
        let entered = Arc::new(Notify::new());
        let notify = entered.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = Arc::new(Mutex::new(wait));
        *service.shutdown_hooks.ledger_worker.lock().unwrap() = Some(Arc::new(move || {
            notify.notify_one();
            wait.lock().unwrap().recv().unwrap();
        }));
        let result: Result<Value> = service
            .ledger("get", json!({"client_request_id":"missing"}))
            .await;
        assert_eq!(code(result), "sms_intent_store_unavailable");
        entered.notified().await;
        drop(service);
        owner.begin_shutdown();
        assert_eq!(owner.snapshot().0, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), owner.finish_pending())
                .await
                .is_err()
        );
        let moved = f.root.with_extension("actual-io-fixture");
        if real_io {
            fs::rename(&f.root, &moved).unwrap();
            fs::write(&f.root, b"not a directory").unwrap();
        }
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), owner.finish_pending())
            .await
            .unwrap()
            .unwrap();
        let settled = executor.finish_pending().await;
        assert_eq!(
            settled.is_err(),
            real_io,
            "only the actual I/O failure must remain sticky"
        );
        assert_eq!(executor.finish_pending().await.is_err(), real_io);
        if real_io {
            fs::remove_file(&f.root).unwrap();
            fs::rename(&moved, &f.root).unwrap();
        }
        assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn late_claim_and_lost_ack_retain_same_owner_until_actual_release_reply() {
    let f = Fixture::new().await;
    f.prepare("late-owner").await;
    let record: Record = f
        .service
        .ledger("get", json!({"client_request_id":"late-owner"}))
        .await
        .unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.root.join("device-send-ledger.json.lock"))
        .unwrap();
    lock.lock().unwrap();
    let owner = f.service.store.send_owner();
    let mut service = f.service.clone();
    service.job = Some(owner.admit().unwrap());
    service.io_timeout = Duration::from_millis(20);
    f.hardware.block_release.store(true, Ordering::SeqCst);
    assert_eq!(
        code(service.claim(&record, "late-attempt".into()).await),
        "sms_intent_store_unavailable"
    );
    drop(service);
    owner.begin_shutdown();
    lock.unlock().unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        f.hardware.release_entered.notified(),
    )
    .await
    .unwrap();
    assert_eq!(owner.snapshot().0, 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), owner.finish_pending())
            .await
            .is_err()
    );
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), 0);
    f.hardware.release_resume.notify_one();
    tokio::time::timeout(Duration::from_secs(3), owner.finish_pending())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.hardware.released.load(Ordering::SeqCst), 1);
    let pinned: Record = f
        .service
        .ledger("get", json!({"client_request_id":"late-owner"}))
        .await
        .unwrap();
    assert_eq!(pinned.state, "submitting");
    assert_eq!(pinned.attempt, "late-attempt");
}

#[tokio::test]
async fn archive_failure_recovery_keeps_one_physical_send_and_original_timestamp() {
    let f = Fixture::new().await;
    f.prepare("repair-archive").await;
    fs::create_dir(f.root.join("sms-archive.json")).unwrap();
    assert_eq!(code(f.service.post(request("repair-archive"), false, &AtomicBool::new(false)).await),
        "sms_send_archive_outcome_unknown");
    let original: Record = f.service.ledger("get", json!({"client_request_id":"repair-archive"}))
        .await.unwrap_or_else(|_| panic!());
    assert_eq!(original.state, "accepted");
    assert!(!original.archive_confirmed);
    fs::remove_dir(f.root.join("sms-archive.json")).unwrap();
    f.hardware.valid.store(false, Ordering::SeqCst);
    let mut wrong = request("repair-archive");
    wrong.message.push_str("-changed");
    assert_eq!(code(f.service.post(wrong, false, &AtomicBool::new(false)).await), "sms_intent_conflict");
    for _ in 0..3 {
        assert!(f.service.post(request("repair-archive"), false, &AtomicBool::new(false)).await.is_ok());
    }
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 1);
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst), 1);
    let rows = f.service.archive.load().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["timestamp"], archive_item(&original).unwrap()["timestamp"]);
    let confirmed: Record = f.service.ledger("get", json!({"client_request_id":"repair-archive"}))
        .await.unwrap_or_else(|_| panic!());
    assert!(confirmed.archive_confirmed);
    assert_eq!(confirmed.attempt, original.attempt);
}

async fn accepted_unarchived(f: &Fixture, id: &str) -> Record {
    f.prepare(id).await;
    let prepared: Record = f.service.ledger("get", json!({"client_request_id":id}))
        .await.unwrap_or_else(|_| panic!());
    let _: Record = f.service.ledger("claim", json!({"client_request_id":id,
        "binding":prepared.binding,"attempt":"archive-only-attempt"})).await.unwrap_or_else(|_| panic!());
    f.service.ledger("finish", json!({"client_request_id":id,"attempt":"archive-only-attempt",
        "state":"accepted","segments":1,"archive_confirmed":false})).await.unwrap_or_else(|_| panic!())
}

#[tokio::test]
async fn deleted_archive_receipt_suppresses_a_stale_reconcile_and_survives_reopen() {
    let f = Fixture::new().await;
    let original = accepted_unarchived(&f, "deleted-original").await;
    let item = archive_item(&original).unwrap();
    crate::message_archive::ingest_messages_to(f.service.archive.clone(), vec![item.clone()]).await.unwrap();
    let id = crate::message_archive::cache_digest(&item);
    assert_eq!(crate::message_archive::delete_ids_from_store(f.service.archive.clone(), vec![id]).await.unwrap(), 1);
    assert!(f.service.archive.load().unwrap().is_empty());
    // Deliberately retain the stale false observation across deletion.
    assert!(f.service.reconcile_archive(original).await.is_ok());
    let mut reopened = f.service.clone();
    reopened.store = crate::device_send_store::Store::open(&f.root).unwrap();
    assert!(reopened.post(request("deleted-original"), false, &AtomicBool::new(false)).await.is_ok());
    assert!(f.service.archive.load().unwrap().is_empty());
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn archive_reconciliation_rejects_changed_attempt_payload_and_concurrent_replay() {
    let f = Fixture::new().await;
    let original = accepted_unarchived(&f, "proof").await;
    let item = archive_item(&original).unwrap();
    assert!(f.service.store.archive_receipt(&item, Some("wrong-attempt"), true).is_err());
    for key in ["sender", "content", "device", "subscription_id", "timestamp"] {
        let mut changed = item.clone();
        changed[key] = json!("changed");
        assert!(f.service.store.archive_receipt(&changed, Some(&original.attempt), true).is_err());
    }
    assert!(f.service.store.archive_receipt(&item, Some(&original.attempt), false).unwrap());
    let mut jobs = Vec::new();
    for _ in 0..12 {
        let service = f.service.clone();
        jobs.push(tokio::spawn(async move {
            service.post(request("proof"), false, &AtomicBool::new(false)).await
        }));
    }
    for job in jobs { assert!(job.await.unwrap().is_ok()); }
    assert_eq!(f.service.archive.load().unwrap().len(), 1);
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst), 0);
    let claim: Record = f.service.ledger("claim", json!({"client_request_id":"proof",
        "binding":original.binding,"attempt":"another-attempt"})).await.unwrap_or_else(|_| panic!());
    assert!(!claim.can_submit);
    assert_eq!(claim.attempt, original.attempt);
}

#[tokio::test]
async fn v2_reserved_nonce_binding_claim_and_archive_are_one_dispatch() {
    let mut f=Fixture::new().await;
    let lease=crate::daemon_lifetime::Lease::acquire(&f.root).unwrap();
    f.service.store=crate::device_send_store::Store::open_with_authority(&f.root,lease.root_proof()).unwrap();
    let status:Value=f.service.ledger("authority",json!({})).await.unwrap_or_else(|_|panic!());
    let reserve=json!({"identity_version":2,"family":"device_sms","authority_id":status["authority_id"],
        "epoch":status["epoch"],"sequence":status["next_sequence"],"client_request_id":status["next_client_request_id"],
        "intent_nonce":"0123456789abcdef0123456789abcdef","purpose":"device_sms_send","device":"dji",
        "payload":{"phone":"65075","message":"fixture-only","expected_subscription_id":""}});
    let reserved:Record=f.service.ledger("reserve",reserve.clone()).await.unwrap_or_else(|_|panic!());
    assert_eq!(reserved.state,"reserved");
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst),0);
    let body=||{
        let mut value=request(&reserved.client_request_id);
        value.identity_version=2;value.intent_nonce="0123456789abcdef0123456789abcdef".into();value
    };
    let mut wrong=body();wrong.intent_nonce="ffffffffffffffffffffffffffffffff".into();
    assert_eq!(code(f.service.post(wrong,true,&AtomicBool::new(false)).await),"sms_intent_conflict");
    assert_eq!(f.hardware.captures.load(Ordering::SeqCst),0);
    let prepared=f.service.post(body(),true,&AtomicBool::new(false)).await.unwrap_or_else(|_|panic!());
    assert_eq!(prepared["identity_version"],2);
    assert_eq!(prepared["expected_subscription_id"],"");
    for _ in 0..3 {assert!(f.service.post(body(),false,&AtomicBool::new(false)).await.is_ok());}
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst),1);
    assert_eq!(f.service.archive.load().unwrap().len(),1);
    let replay:Record=f.service.ledger("reserve",reserve).await.unwrap_or_else(|_|panic!());
    assert_eq!(replay.state,"accepted");assert!(replay.archive_confirmed);
}

#[tokio::test]
async fn v2_authority_query_rejects_mixed_or_unknown_fields_without_issuing() {
    let mut f=Fixture::new().await;
    let lease=crate::daemon_lifetime::Lease::acquire(&f.root).unwrap();
    f.service.store=crate::device_send_store::Store::open_with_authority(&f.root,lease.root_proof()).unwrap();
    let (status,value)=f.public_request(Method::GET,"/api/sms/send-intents?authority=2","",true,true).await;
    assert_eq!(status,StatusCode::OK);assert_eq!(value["issued_high_water"],"0");
    for query in ["authority=2&cursor=old","authority=2&client_request_id=old","authority=3"] {
        let (status,_)=f.public_request(Method::GET,&format!("/api/sms/send-intents?{query}"),"",true,true).await;
        assert_eq!(status,StatusCode::BAD_REQUEST);
    }
    let unchanged:Value=f.service.ledger("authority",json!({})).await.unwrap_or_else(|_|panic!());
    assert_eq!(unchanged["issued_high_water"],"0");
    assert_eq!(f.hardware.sends.load(Ordering::SeqCst),0);
}
