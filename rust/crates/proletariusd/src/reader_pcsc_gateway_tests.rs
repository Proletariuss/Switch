//! Actual native gateway tests plus explicitly named historical wire oracles.
//! Only legacy_wire_* helpers bypass main; native requests use real dispatch.
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{UnixListener, UnixStream};

pub(crate) fn state(root: &std::path::Path) -> crate::ServerState {
    let inbox = crate::message_archive::InboxState::default();
    let reader_gate = Arc::new(crate::reader_reservation::Gate::default());
    reader_gate.initialize_business_producer("reader-producer-fixture");
    crate::ServerState {
        call_mode: Default::default(),
        module_call: Default::default(),
        reader_lifecycle: Default::default(),
        multistandby_card: Default::default(),
        multistandby_service: Default::default(),
        app_token: Some("app-fixture".into()),
        internal_token: "internal-fixture".into(),
        multistandby_store: crate::multistandby_store::Store::open(&root.join("multi")).unwrap(),
        multistandby_operations: crate::multistandby_operations::Store::open(root).unwrap(),
        device_send_store: crate::device_send_store::Store::open(&root.join("device")).unwrap(),
        forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(root).unwrap(),
        sms_forwarding: crate::sms_forwarding::Service::new(root),
        sms_inbox: inbox.clone(),
        usb_sms_receive: crate::test_usb_sms_receiver(inbox, root.join("sms-archive.json")),
        usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
        module_radio: crate::test_module_radio(root),
        call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
        call_facts: crate::call_facts::CallFacts::new(
            proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            "call-producer-fixture".into(),
        ),
        modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
        qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
        network_traffic: crate::network_traffic::Service::default(),
        go_socket_path: root.join("no-go.sock"),
        availability: crate::availability::BackendAvailability::new(),
        reader_discovery: Arc::new(Mutex::new(proletarius_reader::ReaderDiscovery::default())),
        reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
        reader_gate,
        snapshot_instance_id: "pcsc-routing-fixture".into(),
        snapshot_revision: Arc::new(crate::AtomicU64::new(0)),
        eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
        native_vowifi_lifecycle: std::sync::Arc::new(
            crate::native_vowifi_session::NativeLifecycle::default(),
        ),
        ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
        multi_runtime_report: Arc::new(Mutex::new(
            crate::multistandby_runtime_report::Journal::default(),
        )),
    }
}

#[tokio::test]
async fn g3_shared_gateway_fixture_keeps_forwarding_in_its_private_root() {
    let root = crate::tests::test_directory("g3-private-forwarding-fixture");
    let state = state(&root);
    // Construction itself enrolls the forwarding receipt store. These exact
    // files must belong to the supplied root before any request is dispatched.
    for file in [
        "sms-forwarding-test-receipts-v1.json",
        ".sms-forwarding-test-receipts-v1.json.presence-v1",
        "sms-forwarding-test-receipts-v1.json.lock",
    ] {
        assert!(root.join(file).is_file(), "forwarding escaped fixture root");
    }
    state.sms_forwarding.shutdown().await.unwrap();
    state
        .native_vowifi_lifecycle
        .finish_shutdown(tokio::time::Instant::now() + std::time::Duration::from_secs(2))
        .await
        .unwrap();
    state.reader_pcsc.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(!root.exists());
}

async fn request(
    state: crate::ServerState,
    path: &str,
    public: bool,
    stopping: bool,
    credential: Option<(&str, &str)>,
) -> (StatusCode, serde_json::Value) {
    request_with_body(state, path, public, stopping, credential, b"{}").await
}

pub(crate) async fn request_with_body(
    state: crate::ServerState,
    path: &str,
    public: bool,
    stopping: bool,
    credential: Option<(&str, &str)>,
    body: &[u8],
) -> (StatusCode, serde_json::Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        if stopping {
            crate::serve_internal_shutdown_connection(server, state).await;
        } else if public {
            if let Err(error) = crate::serve_connection(server, state).await {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
        } else {
            if let Err(error) = crate::serve_internal_connection(server, state).await {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let mut request = Request::builder().method("POST").uri(path);
    if let Some((header, value)) = credential {
        request = request.header(header, value);
    }
    let response = sender
        .send_request(
            request
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

// Historical adapter harness only. Production route retirement is exercised by
// reader_compat_retirement_tests; native tests above continue through actual main.
async fn legacy_wire_request(
    state: crate::ServerState,
    path: &str,
    public: bool,
    stopping: bool,
    credential: Option<(&str, &str)>,
) -> (StatusCode, serde_json::Value) {
    legacy_wire_request_with_body(state, path, public, stopping, credential, b"{}").await
}

async fn legacy_wire_request_with_body(
    state: crate::ServerState,
    path: &str,
    _public: bool,
    stopping: bool,
    credential: Option<(&str, &str)>,
    body: &[u8],
) -> (StatusCode, serde_json::Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            let state = state.clone();
            async move {
                let response = if !crate::multistandby_store::internal_authenticated(
                    &request,
                    &state.internal_token,
                ) {
                    crate::json_response(
                        StatusCode::UNAUTHORIZED,
                        &serde_json::json!({"code":"internal_authentication_required","error":"unauthorized"}),
                    )
                } else if request.uri().path().starts_with(crate::reader_pcsc::PREFIX) {
                    crate::reader_pcsc::handle_legacy_wire(
                        request,
                        state.reader_pcsc,
                        stopping,
                        state.reader_gate,
                    )
                    .await
                } else if request.uri().path() == crate::reader_ble_gateway::PATH {
                    crate::reader_ble_gateway::handle(request, None, None, stopping).await
                } else {
                    crate::json_response(
                        StatusCode::NOT_FOUND,
                        &serde_json::json!({"error":"not found"}),
                    )
                };
                Ok::<_, Infallible>(response)
            }
        });
        let result = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server), service)
            .await;
        if let Err(error) = result {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let mut request = Request::builder().method("POST").uri(path);
    if let Some((header, value)) = credential {
        request = request.header(header, value);
    }
    let response = sender
        .send_request(
            request
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

struct DirectSupplier {
    session: Option<crate::reader_pcsc::LifecycleBinding>,
    reject: bool,
    esim_manager_available: Option<bool>,
    identity: bool,
}
impl crate::reader_session_lease::NativeSessionSupplier for DirectSupplier {
    fn pre_admit(
        self: Arc<Self>,
        _: String,
        _: crate::reader_reservation::NativePinPolicy,
        _ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> crate::reader_session_lease::PreAdmissionFuture {
        Box::pin(async { Ok(1) })
    }
    fn admit(
        self: Arc<Self>,
        _: crate::reader_reservation::NativePinBinding,
    ) -> crate::reader_session_lease::AdmissionFuture {
        Box::pin(async move {
            if self.reject {
                Err("reader native admission rejected".into())
            } else {
                Ok(())
            }
        })
    }
    fn snapshot(
        &self,
        binding: &crate::reader_reservation::NativePinBinding,
    ) -> Result<serde_json::Value, String> {
        Ok(
            serde_json::json!({"reader":binding.reader,"reader_generation":binding.reader_generation,"session":self.session,
            "identity":{"iccid":if self.identity {"8901000000000000001"} else {""},"imsi":if self.identity {"001010123456789"} else {""},"mcc":"","mnc":""},
            "esim_manager_available":self.esim_manager_available,"notification_aids":[]}),
        )
    }
    fn finalize(
        self: Arc<Self>,
        _: crate::reader_reservation::NativePinBinding,
        _: Option<crate::reader_session_lease::NativeCardContext>,
        _: String,
        _: String,
    ) -> crate::reader_session_lease::SupplierFuture {
        Box::pin(async { Err("finalizer is not part of this fixture".into()) })
    }
}
fn install_supplier(
    state: &crate::ServerState,
    supplier: Arc<DirectSupplier>,
) -> Arc<DirectSupplier> {
    let erased: Arc<dyn crate::reader_session_lease::NativeSessionSupplier> = supplier.clone();
    state
        .reader_gate
        .set_native_session_supplier(Arc::downgrade(&erased));
    supplier
}

#[tokio::test]
async fn legacy_wire_oracle_reader_ble_transport_requires_internal_auth_and_stops_new_work_at_shutdown()
 {
    let root = crate::tests::test_directory("reader-ble-private-route");
    for public in [false, true] {
        let unauthorized = legacy_wire_request(
            state(&root),
            crate::reader_ble_gateway::PATH,
            public,
            false,
            Some(("X-Proletarius-App-Token", "app-fixture")),
        )
        .await;
        assert_eq!(unauthorized.0, StatusCode::UNAUTHORIZED);
        let invalid = legacy_wire_request(
            state(&root),
            crate::reader_ble_gateway::PATH,
            public,
            false,
            Some(("X-Switch-Internal-Token", "internal-fixture")),
        )
        .await;
        assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
        assert_eq!(invalid.1["code"], "ble_invalid_request");
    }
    let stopped = legacy_wire_request_with_body(
        state(&root),
        crate::reader_ble_gateway::PATH,
        false,
        true,
        Some(("X-Switch-Internal-Token", "internal-fixture")),
        br#"{"producer_id":"fixture-go","action":"reset","reader":"BLE:12345678-1234-1234-1234-123456789abc"}"#,
    )
    .await;
    // Shutdown allows only restoration under an existing exact workflow.
    // An unscoped reader name is still invalid and cannot submit a reset.
    assert_eq!(stopped.0, StatusCode::BAD_REQUEST);
    assert_eq!(stopped.1["code"], "ble_invalid_request");
    let unknown = legacy_wire_request(
        state(&root),
        &format!("{}/other", crate::reader_ble_gateway::PATH),
        false,
        false,
        Some(("X-Switch-Internal-Token", "internal-fixture")),
    )
    .await;
    assert_eq!(unknown.0, StatusCode::NOT_FOUND);
}

async fn public_health_request(
    state: crate::ServerState,
    selected: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    if let Some(reader) = selected {
        state.reader_gate.test_seed_selection(reader, 1);
    }
    let (client, server) = UnixStream::pair().unwrap();
    let selected = selected.map(str::to_owned);
    let server = tokio::spawn(async move {
        if let Some(reader) = selected {
            // Exercise the same selected-reader handler with per-test
            // authority; discovery is only a presence observation now.
            let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                let state = state.clone();
                let reader = reader.clone();
                async move {
                    assert_eq!(request.uri().path(), "/api/reader/esim/health");
                    let confirmed_reader = reader.clone();
                    Ok::<_, Infallible>(
                        crate::reader_esim_download::health_or_forward_with_selection(
                            request,
                            &state,
                            reader,
                            move || Some(confirmed_reader),
                        )
                        .await,
                    )
                }
            });
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server), service)
                .await
            {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
            return;
        }
        if let Err(error) = crate::serve_connection(server, state).await {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method("GET")
                .uri("/api/reader/esim/health")
                .header(crate::APP_TOKEN_HEADER, "app-fixture")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn public_auth_request(
    state: crate::ServerState,
    body: &'static str,
) -> (StatusCode, serde_json::Value) {
    state.reader_gate.test_seed_selection("Reader A", 1);
    let (client, server) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        // Selection dispatch is covered independently. Keep this fixture on
        // the real pre-submit acquisition failure and unchanged-body forward.
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            let state = state.clone();
            async move {
                assert_eq!(request.uri().path(), "/api/reader/auth");
                Ok::<_, Infallible>(
                    crate::reader_auth::handle_public(request, &state, "Reader A".into()).await,
                )
            }
        });
        if let Err(error) = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server), service)
            .await
        {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender
        .send_request(
            Request::builder()
                .method("POST")
                .uri("/api/reader/auth")
                .header(crate::APP_TOKEN_HEADER, "app-fixture")
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from_static(body.as_bytes())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn public_auth_native_admission_failure_does_not_forward_to_go() {
    let root = crate::tests::test_directory("pcsc-public-auth-native-error");
    let socket = root.join("go.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut state = state(&root);
    state.go_socket_path = socket;
    let _supplier = install_supplier(
        &state,
        Arc::new(DirectSupplier {
            session: None,
            reject: true,
            esim_manager_available: None,
            identity: false,
        }),
    );
    let expected_payload = r#"{"rand":"00000000000000000000000000000000","autn":"11111111111111111111111111111111","mode":"eap-aka"}"#;
    let (status, body) = public_auth_request(state.clone(), expected_payload).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "auth_failed");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    assert!(state.reader_gate.admit("Reader A").is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn health_route_native_admission_failure_is_local_and_never_forwards_to_go() {
    let root = crate::tests::test_directory("pcsc-health-native-admission-error");
    let mut state = state(&root);
    let trap = native_lpa_routes::GoTrap::install(&mut state, &root);
    let _supplier = install_supplier(
        &state,
        Arc::new(DirectSupplier {
            session: None,
            reject: true,
            esim_manager_available: Some(true),
            identity: false,
        }),
    );
    let (status, body) = public_health_request(state.clone(), Some("Reader A")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "reader_esim_health_failed");
    assert!(state.reader_gate.admit("Reader A").is_ok());
    trap.assert_unused().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn health_route_does_not_revive_unselected_reader_from_discovery() {
    for (index, observed) in [
        None,
        Some("Reader A"),
        Some("BLE:12345678-1234-1234-1234-123456789abc"),
    ]
    .into_iter()
    .enumerate()
    {
        let root = crate::tests::test_directory(&format!("pcsc-health-no-selection-{index}"));
        let mut state = state(&root);
        let trap = native_lpa_routes::GoTrap::install(&mut state, &root);
        if let Some(reader) = observed {
            state.reader_discovery.lock().unwrap().reconcile(
                vec![proletarius_reader::ReaderObservation {
                    name: reader.into(),
                    card: proletarius_reader::CardPresence::Present,
                    in_use: false,
                    muted: false,
                    atr: Vec::new(),
                }],
                Some(reader),
            );
        }
        let (status, body) = public_health_request(state, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "reader_esim_unavailable");
        trap.assert_unused().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}

struct ReleaseHeldPhysical(
    Arc<
        Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                std::sync::mpsc::Receiver<()>,
            )>,
        >,
    >,
);
impl proletarius_sim::ApduTransport for ReleaseHeldPhysical {
    type Error = crate::reader_pcsc::Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![0x6a, 0x82])
    }
}
impl crate::reader_pcsc::Physical for ReleaseHeldPhysical {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(
            &mut dyn proletarius_sim::ApduTransport<Error = crate::reader_pcsc::Failure>,
        ),
    ) -> Result<(), crate::reader_pcsc::Failure> {
        body(self);
        if let Some((pending, allow)) = self.0.lock().unwrap().take() {
            let _ = pending.send(());
            allow
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        Ok(())
    }
    fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
        Ok(())
    }
}
#[tokio::test]
async fn health_route_retains_response_and_authority_until_actual_pcsc_release() {
    let root = crate::tests::test_directory("pcsc-health-native-release");
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    let (allow_tx, allow_rx) = std::sync::mpsc::channel();
    let release = Arc::new(Mutex::new(Some((pending_tx, allow_rx))));
    let opening = release.clone();
    let resetting = release.clone();
    let owner = crate::reader_pcsc::Owner::with_factory(
        "reader-producer-fixture".into(),
        Arc::new(move |_| Ok(Box::new(ReleaseHeldPhysical(opening.clone())))),
        Arc::new(|| Ok(vec!["Reader A".into()])),
        Arc::new(move |_, _| Ok(Box::new(ReleaseHeldPhysical(resetting.clone())))),
    );
    let session = owner.native_open("Reader A").unwrap();
    let mut state = state(&root);
    state.reader_pcsc = owner;
    let trap = native_lpa_routes::GoTrap::install(&mut state, &root);
    let _supplier = install_supplier(
        &state,
        Arc::new(DirectSupplier {
            session: Some(session),
            reject: false,
            esim_manager_available: Some(true),
            identity: false,
        }),
    );
    let gate = state.reader_gate.clone();
    let mut response = tokio::spawn(public_health_request(state.clone(), Some("Reader A")));
    tokio::time::timeout(std::time::Duration::from_secs(2), pending_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut response)
            .await
            .is_err()
    );
    assert!(
        gate.begin_native_pin(
            "Reader A",
            crate::reader_reservation::NativePinPolicy::CardOnly
        )
        .is_err()
    );
    allow_tx.send(()).unwrap();
    let (status, body) = tokio::time::timeout(std::time::Duration::from_secs(3), response)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "reader_esim_health_failed");
    trap.assert_unused().await;
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn legacy_wire_oracle_every_pcsc_action_requires_internal_credential_on_both_sockets() {
    let root = crate::tests::test_directory("pcsc-private-routing");
    let state = state(&root);
    for action in [
        "open", "status", "acquire", "renew", "release", "transmit", "reset", "close",
    ] {
        let path = format!("/api/internal/reader-pcsc/{action}");
        for public in [false, true] {
            for credential in [None, Some((crate::APP_TOKEN_HEADER, "app-fixture"))] {
                let (status, body) =
                    legacy_wire_request(state.clone(), &path, public, false, credential).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}, public={public}");
                assert_eq!(body["code"], "internal_authentication_required");
            }
        }
    }
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reader_lifecycle_requires_private_auth_and_bounded_valid_body() {
    let root = crate::tests::test_directory("reader-lifecycle-routing");
    let state = state(&root);
    let path = crate::reader_reservation::LIFECYCLE_PATH;
    for public in [false, true] {
        for credential in [None, Some((crate::APP_TOKEN_HEADER, "app-fixture"))] {
            let (status, body) = request(state.clone(), path, public, false, credential).await;
            let (expected, code) = if public {
                (StatusCode::NOT_FOUND, "route_not_found")
            } else {
                (StatusCode::UNAUTHORIZED, "internal_authentication_required")
            };
            assert_eq!(status, expected);
            assert_eq!(body["code"], code);
        }
        for body in [b"{}".to_vec(), vec![b' '; 4097]] {
            let (status, body) = request_with_body(
                state.clone(),
                path,
                public,
                false,
                Some((
                    crate::multistandby_store::INTERNAL_HEADER,
                    "internal-fixture",
                )),
                &body,
            )
            .await;
            let (expected, code) = if public {
                // A valid internal credential cannot open a public RPC route.
                (StatusCode::NOT_FOUND, "route_not_found")
            } else {
                (StatusCode::BAD_REQUEST, "invalid_request")
            };
            assert_eq!(status, expected);
            assert_eq!(body["code"], code);
        }
    }
    // Finishing an accepted transition must still reach the handler during
    // shutdown. Malformed input is rejected there without touching a card.
    let (status, body) = request(
        state.clone(),
        path,
        false,
        true,
        Some((
            crate::multistandby_store::INTERNAL_HEADER,
            "internal-fixture",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn legacy_wire_oracle_shutdown_routes_only_allow_pcsc_status_and_resource_release() {
    let root = crate::tests::test_directory("pcsc-shutdown-routing");
    let state = state(&root);
    state.reader_pcsc.begin_shutdown();
    for action in ["open", "acquire", "renew", "transmit", "reset"] {
        let path = format!("/api/internal/reader-pcsc/{action}");
        let (status, _) = legacy_wire_request(
            state.clone(),
            &path,
            false,
            true,
            Some((
                crate::multistandby_store::INTERNAL_HEADER,
                "internal-fixture",
            )),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
    for action in ["status", "release", "close"] {
        let path = format!("/api/internal/reader-pcsc/{action}");
        let (status, _) = legacy_wire_request(
            state.clone(),
            &path,
            false,
            true,
            Some((
                crate::multistandby_store::INTERNAL_HEADER,
                "internal-fixture",
            )),
        )
        .await;
        // An empty body cannot name any resource. It reaches the validated
        // cleanup handler instead of being lost to a generic stopping gate.
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn native_owner_route_requires_private_auth_bounded_body_and_shutdown_cleanup_authority() {
    let root = crate::tests::test_directory("native-owner-private-route");
    let state = state(&root);
    let path = crate::reader_reservation::NATIVE_OWNER_PATH;
    let credential = Some((
        crate::multistandby_store::INTERNAL_HEADER,
        "internal-fixture",
    ));
    for public in [false, true] {
        for token in [None, Some((crate::APP_TOKEN_HEADER, "app-fixture"))] {
            let (status, body) = request(state.clone(), path, public, false, token).await;
            let (expected, code) = if public {
                (StatusCode::NOT_FOUND, "route_not_found")
            } else {
                (StatusCode::UNAUTHORIZED, "internal_authentication_required")
            };
            assert_eq!(status, expected);
            assert_eq!(body["code"], code);
        }
        for body in [b"{}".to_vec(), vec![b' '; 4097]] {
            let (status, body) =
                request_with_body(state.clone(), path, public, false, credential, &body).await;
            let (expected, code) = if public {
                (StatusCode::NOT_FOUND, "route_not_found")
            } else {
                (StatusCode::BAD_REQUEST, "invalid_request")
            };
            assert_eq!(status, expected);
            assert_eq!(body["code"], code);
        }
    }
    state
        .reader_gate
        .test_seed_selection("BLE:12345678-1234-1234-1234-123456789abc", 7);
    let binding = state
        .reader_gate
        .begin_native_pin(
            "BLE:12345678-1234-1234-1234-123456789abc",
            crate::reader_reservation::NativePinPolicy::CardOnly,
        )
        .unwrap();
    let pin =
        serde_json::json!({"action":"validate","binding":binding,"purpose":"pin"}).to_string();
    let (status, body) = request_with_body(
        state.clone(),
        path,
        false,
        false,
        credential,
        pin.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let (status, body) =
        request_with_body(state.clone(), path, false, true, credential, pin.as_bytes()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "backend_stopping");
    state.reader_gate.drain_native_pin(&binding).unwrap();
    let unpin =
        serde_json::json!({"action":"validate","binding":binding,"purpose":"unpin"}).to_string();
    let (status, body) = request_with_body(
        state.clone(),
        path,
        false,
        true,
        credential,
        unpin.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let mut wrong = binding.clone();
    wrong.reader_generation += 1;
    let invalid =
        serde_json::json!({"action":"validate","binding":wrong,"purpose":"unpin"}).to_string();
    let (status, _) = request_with_body(
        state.clone(),
        path,
        false,
        true,
        credential,
        invalid.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    state.reader_gate.finish_native_pin(&binding).unwrap();
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn legacy_wire_oracle_pcsc_io_requires_exact_live_transaction_and_cleanup_survives_draining()
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Card(Arc<AtomicUsize>);
    impl proletarius_sim::ApduTransport for Card {
        type Error = crate::reader_pcsc::Failure;
        fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![0x90, 0])
        }
    }
    impl crate::reader_pcsc::Physical for Card {
        fn transaction(
            &mut self,
            body: &mut dyn FnMut(
                &mut dyn proletarius_sim::ApduTransport<Error = crate::reader_pcsc::Failure>,
            ),
        ) -> Result<(), crate::reader_pcsc::Failure> {
            body(self);
            Ok(())
        }
        fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
            Ok(())
        }
    }
    let root = crate::tests::test_directory("pcsc-transaction-authority");
    let mut state = state(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let opened = calls.clone();
    let reset = calls.clone();
    state.reader_pcsc = crate::reader_pcsc::Owner::with_factory(
        "reader-producer-fixture".into(),
        Arc::new(move |_| Ok(Box::new(Card(opened.clone())))),
        Arc::new(|| Ok(vec!["Reader A".into()])),
        Arc::new(move |_, _| Ok(Box::new(Card(reset.clone())))),
    );
    state.reader_gate.test_seed_selection("Reader A", 1);
    let physical = crate::reader_pcsc::Binding {
        producer_id: "reader-producer-fixture".into(),
        reader: "Reader A".into(),
        session_id: "session".into(),
        lease_id: "lease".into(),
        lease_seq: 1,
    };
    state.reader_pcsc.test_open_acquire(&physical, 1).unwrap();
    let binding = serde_json::json!({"producer_id":"reader-producer-fixture","operation_token":"a".repeat(64),"operation_sequence":1,"reader":"Reader A","reader_generation":1,"purpose":"card","authority":{"kind":"idle"}});
    state
        .reader_gate
        .test_operation_command("acquire", binding.clone())
        .unwrap();
    let mut input = serde_json::to_value(&physical).unwrap();
    input["operation_id"] = "apdu-1".into();
    input["operation_seq"] = 1.into();
    input["command_hex"] = "00A40000".into();
    let path = "/api/internal/reader-pcsc/transmit";
    let auth = Some((
        crate::multistandby_store::INTERNAL_HEADER,
        "internal-fixture",
    ));
    let response = legacy_wire_request_with_body(
        state.clone(),
        path,
        false,
        false,
        auth,
        input.to_string().as_bytes(),
    )
    .await;
    assert_eq!(response.0, StatusCode::BAD_REQUEST);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut wrong = binding.clone();
    wrong["reader_generation"] = 2.into();
    input["transaction"] = serde_json::json!({"kind":"operation","binding":wrong});
    let response = legacy_wire_request_with_body(
        state.clone(),
        path,
        false,
        false,
        auth,
        input.to_string().as_bytes(),
    )
    .await;
    assert_ne!(response.0, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    input["transaction"] = serde_json::json!({"kind":"operation","binding":binding.clone()});
    let response = legacy_wire_request_with_body(
        state.clone(),
        path,
        false,
        false,
        auth,
        input.to_string().as_bytes(),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(response.1["response_hex"], "9000");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    state
        .reader_gate
        .test_operation_command("release", binding)
        .unwrap();
    input["operation_id"] = "apdu-2".into();
    input["operation_seq"] = 2.into();
    let response = legacy_wire_request_with_body(
        state.clone(),
        path,
        false,
        false,
        auth,
        input.to_string().as_bytes(),
    )
    .await;
    assert_ne!(response.0, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let response = legacy_wire_request_with_body(
        state.clone(),
        "/api/internal/reader-pcsc/release",
        false,
        false,
        auth,
        serde_json::to_string(&physical).unwrap().as_bytes(),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn legacy_wire_oracle_native_lifecycle_owner_rejects_compatibility_open_and_close_on_both_sockets()
 {
    let root = crate::tests::test_directory("pcsc-native-lifecycle-wire-fence");
    let state = state(&root);
    state
        .reader_gate
        .initialize_native_lifecycle("native-lifecycle-fixture")
        .unwrap();
    for public in [false, true] {
        for action in ["open", "close"] {
            let (status, body) = legacy_wire_request(
                state.clone(),
                &format!("/api/internal/reader-pcsc/{action}"),
                public,
                false,
                Some(("X-Switch-Internal-Token", "internal-fixture")),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["code"], "reader_native_lifecycle_owned");
        }
    }
    state.reader_pcsc.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[path = "reader_pcsc_gateway_tests/native_lpa_routes.rs"]
mod native_lpa_routes;
