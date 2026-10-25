//! Actual public/private UDS routing and retained native shutdown with a PTY owner.
//! Reuses the complete serial lifecycle fixture, never a hardware port/Go reader.
use crate::module_startup::{SERIAL_TEST_ICCID, SERIAL_TEST_PRODUCER, SerialTestFixture};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

const GO: &str = "compatibility-fixture";
struct Fixture {
    physical: SerialTestFixture,
    state: crate::ServerState,
    startup: crate::module_startup::Service,
    session: crate::modem_actor::NativeSerialSession,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_missing_phone(false).await
    }
    async fn with_missing_phone(missing: bool) -> Self {
        let physical = SerialTestFixture::new();
        if missing {
            physical.omit_phone();
        }
        let mut state = crate::reader_pcsc_gateway_tests::state(&physical.root);
        state.modem_actor = physical.modem.clone();
        state.usb_at = physical.usb.clone();
        state.module_radio = physical.radio.clone();
        state.sms_inbox = physical.inbox.clone();
        state
            .native_vowifi_lifecycle
            .ordinary_admission()
            .set_go_producer(GO)
            .unwrap();
        state.call_facts = crate::call_facts::CallFacts::new(state.call_archive.clone(), GO.into());
        let startup = physical.start();
        tokio::time::timeout(Duration::from_secs(12), async {
            while !state.modem_actor.native_serial_observation().ready {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        let session = state.modem_actor.native_serial_session().unwrap();
        // Drain the initial empty-storage scan before taking a no-I/O baseline.
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut previous = physical.command_count();
            loop {
                tokio::time::sleep(Duration::from_millis(35)).await;
                let current = physical.command_count();
                if current == previous {
                    break;
                }
                previous = current;
            }
        })
        .await
        .unwrap();
        Self {
            physical,
            state,
            startup,
            session,
        }
    }
    fn assert_retained(&self) {
        assert!(self.state.modem_actor.native_serial_managed());
        assert!(self.state.modem_actor.native_esim_managed());
        assert_eq!(
            self.state
                .modem_actor
                .native_serial_observation()
                .serial_session,
            Some(self.session.serial_session)
        );
        assert_eq!(
            self.state.module_radio.serial_transport_session(),
            Some(self.session.serial_session)
        );
        assert!(self.state.modem_actor.native_serial_observation().ready);
        assert!(self.session.observation().terminal.is_none());
        assert_eq!(self.physical.open_count(), 1);
        assert_eq!(
            self.state.sms_inbox.current_module_producer_id().as_deref(),
            Some(SERIAL_TEST_PRODUCER)
        );
    }
    async fn finish(self) {
        self.startup.begin_shutdown();
        if self.state.modem_actor.native_serial_session().is_some() {
            assert!(
                self.session.observation().terminal.is_none(),
                "phase one cannot close the physical cleanup owner"
            );
        }
        self.state.module_radio.finish_shutdown().await.unwrap();
        self.startup.shutdown().await.unwrap();
        assert!(self.session.observation().terminal.unwrap().physical_closed);
        assert!(self.state.modem_actor.native_serial_session().is_none());
        self.physical.finish(&self.startup).await;
    }
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    Public,
    Private,
    Stopping,
}
async fn request(
    state: &crate::ServerState,
    entry: Entry,
    method: &str,
    path: &str,
    token: &str,
    body: &[u8],
) -> (StatusCode, Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let state = state.clone();
    let serving = tokio::spawn(async move {
        let result = match entry {
            Entry::Public => crate::serve_connection(server, state).await,
            Entry::Private => crate::serve_internal_connection(server, state).await,
            Entry::Stopping => {
                crate::serve_internal_shutdown_connection(server, state).await;
                Ok(())
            }
        };
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
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        sender.send_request(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost")
                .header("x-proletarius-app-token", "app-fixture")
                .header("x-switch-internal-token", token)
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap(),
        ),
    )
    .await
    .expect("native request cannot wait for Go")
    .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    serving.await.unwrap();
    (status, value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_public_status_health_are_cache_only_with_go_absent_or_trapped() {
    let f = Fixture::new().await;
    // Freeze autonomous metadata/reconnect work and await the async SMS initial scan
    // before measuring whether the two public GETs cause physical I/O.
    f.startup.begin_shutdown();
    f.state.modem_actor.finish_sms_receiver().await.unwrap();
    f.state.availability.mark_failed();
    assert!(!f.state.go_socket_path.exists());
    for trap_live in [false, true] {
        let trap = trap_live.then(|| UnixListener::bind(&f.state.go_socket_path).unwrap());
        let before = f.physical.command_count();
        let (status, health) =
            request(&f.state, Entry::Public, "GET", "/api/health", "", b"").await;
        assert_eq!(status, StatusCode::OK, "{health}");
        assert_eq!(health["port"], f.physical.port);
        assert_eq!(health["esim_available"], true);
        let (status, device) =
            request(&f.state, Entry::Public, "GET", "/api/status", "", b"").await;
        assert_eq!(status, StatusCode::OK, "{device}");
        assert_eq!(device["iccid"], SERIAL_TEST_ICCID);
        assert_eq!(device["sim_inserted"], true);
        assert_eq!(device["phone_number"], "+447700900123");
        assert_eq!(device["imei"], "123456789012345");
        assert_eq!(
            f.physical.command_count(),
            before,
            "public observation must not start another serial collector"
        );
        if let Some(trap) = trap {
            assert!(
                tokio::time::timeout(Duration::from_millis(40), trap.accept())
                    .await
                    .is_err(),
                "native public observation touched Go socket"
            );
        }
    }
    f.assert_retained();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_shutdown_retains_exact_session_until_radio_cleanup_finishes() {
    let f = Fixture::new().await;
    let before = f.physical.command_count();
    f.startup.begin_shutdown();
    f.state.module_radio.begin_shutdown();
    assert!(f.session.observation().terminal.is_none());
    assert_eq!(
        f.state
            .modem_actor
            .native_serial_session()
            .unwrap()
            .serial_session,
        f.session.serial_session
    );
    assert_eq!(
        f.state.sms_inbox.current_module_producer_id().as_deref(),
        Some(SERIAL_TEST_PRODUCER)
    );
    assert_eq!(f.physical.open_count(), 1);
    assert_eq!(f.physical.command_count(), before);
    f.state.module_radio.finish_shutdown().await.unwrap();
    assert!(f.session.observation().terminal.is_none());
    f.startup.shutdown().await.unwrap();
    assert!(f.session.observation().terminal.unwrap().physical_closed);
    assert!(f.state.modem_actor.native_serial_session().is_none());
    assert_eq!(f.physical.open_count(), 1);
    // Joining the retained worker again must preserve the same close receipt.
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_private_hardware_rpcs_reject_before_decode_io_on_all_three_entries() {
    let f = Fixture::new().await;
    let before = f.physical.command_count();
    for entry in [Entry::Public, Entry::Private, Entry::Stopping] {
        for path in [
            "/api/internal/modem/open",
            "/api/internal/modem/initialize",
            "/api/internal/modem/read",
            "/api/internal/modem/write",
            "/api/internal/modem/execute",
            "/api/internal/modem/close",
            "/api/internal/module-radio/configure",
            "/api/internal/usb-at/close",
            "/api/internal/usb-sms/configure",
            "/api/internal/call-mode/runtime/verify",
            crate::call_mode_adb::INTERNAL_ROUTE_STATUS_PATH,
            crate::call_mode_adb::INTERNAL_ROUTE_STOP_PATH,
        ] {
            for (token, expected, code) in [
                (
                    "wrong-token",
                    StatusCode::UNAUTHORIZED,
                    "internal_authentication_required",
                ),
                (
                    "internal-fixture",
                    StatusCode::FORBIDDEN,
                    "native_module_owned",
                ),
            ] {
                let (status, value) =
                    request(&f.state, entry, "POST", path, token, b"{invalid-json").await;
                let (expected, code) = if matches!(entry, Entry::Public) {
                    // No internal RPC is reachable through the public socket.
                    (StatusCode::NOT_FOUND, "route_not_found")
                } else {
                    (expected, code)
                };
                assert_eq!(status, expected, "{entry:?} {path}: {value}");
                assert_eq!(value["code"], code, "{entry:?} {path}");
            }
        }
    }
    assert_eq!(
        f.physical.command_count(),
        before,
        "denied RPC cannot reach physical I/O"
    );
    assert!(f.state.call_mode.get().is_none());
    assert!(f.state.module_call.get().is_none());
    f.assert_retained();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_note_phone_is_same_sim_display_only_and_close_revokes_all_old_identity() {
    let f = Fixture::with_missing_phone(true).await;
    let notes = crate::profile_notes::Store::new(f.state.multistandby_store.data_root());
    notes
        .remember_registered_phone(SERIAL_TEST_ICCID, "+447700900777", || true)
        .unwrap();
    f.state.availability.mark_failed();
    let (status, device) = request(&f.state, Entry::Public, "GET", "/api/status", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{device}");
    assert_eq!(device["iccid"], SERIAL_TEST_ICCID);
    assert_eq!(device["imsi"], "460001234567890");
    assert_eq!(device["phone_number"], "+447700900777");
    assert_eq!(
        f.state
            .modem_actor
            .native_serial_observation()
            .metadata
            .unwrap()
            .status["phone_number"],
        "",
        "saved display fallback must not become physical identity"
    );

    f.startup.begin_shutdown();
    f.state.module_radio.finish_shutdown().await.unwrap();
    f.state
        .modem_actor
        .close_native_serial(&f.session)
        .await
        .unwrap();
    let (status, device) = request(&f.state, Entry::Public, "GET", "/api/status", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{device}");
    for field in ["iccid", "imsi", "phone_number"] {
        assert!(
            device[field].as_str().unwrap_or_default().is_empty(),
            "old {field} survived actual physical close: {device}"
        );
    }
    let (status, health) = request(&f.state, Entry::Public, "GET", "/api/health", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["esim_available"], false);
    assert!(f.session.observation().terminal.unwrap().physical_closed);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_keeps_authenticated_pure_call_parser_without_any_hardware_io() {
    let f = Fixture::new().await;
    let before = f.physical.command_count();
    for entry in [Entry::Public, Entry::Private] {
        let (status, value) = request(
            &f.state,
            entry,
            "POST",
            crate::modem_actor::PROJECT_CALL_STATE_PATH,
            "wrong-token",
            br#"{"response":"OK\r\n"}"#,
        )
        .await;
        let (expected, code) = if matches!(entry, Entry::Public) {
            (StatusCode::NOT_FOUND, "route_not_found")
        } else {
            (StatusCode::UNAUTHORIZED, "internal_authentication_required")
        };
        assert_eq!(status, expected, "{entry:?}: {value}");
        assert_eq!(value["code"], code);
        let (status, value) = request(
            &f.state,
            entry,
            "POST",
            crate::modem_actor::PROJECT_CALL_STATE_PATH,
            "internal-fixture",
            br#"{"response":"OK\r\n"}"#,
        )
        .await;
        if matches!(entry, Entry::Public) {
            assert_eq!(status, StatusCode::NOT_FOUND, "{entry:?}: {value}");
            assert_eq!(value["code"], "route_not_found");
        } else {
            assert_eq!(status, StatusCode::OK, "{entry:?}: {value}");
            assert_eq!(value["state"], "idle");
            assert_eq!(value["active"], false);
            assert_eq!(value["incoming"], false);
        }
    }
    assert_eq!(
        f.physical.command_count(),
        before,
        "pure parser must not reach physical AT"
    );
    f.assert_retained();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_claim_before_radio_configuration_blocks_sms_go_fallback() {
    // The shared opener is PTY-only and records every open. Do not start the
    // lifecycle: this is precisely the claim-to-radio-configuration interval.
    let physical = SerialTestFixture::new();
    let mut state = crate::reader_pcsc_gateway_tests::state(&physical.root);
    state.modem_actor = physical.modem.clone();
    state.usb_at = physical.usb.clone();
    state.module_radio = physical.radio.clone();
    state.sms_inbox = physical.inbox.clone();
    state.modem_actor.claim_native_serial_management().unwrap();
    assert_eq!(state.module_radio.serial_transport_session(), None);
    assert_eq!(state.module_radio.configured_usb_transport(), None);
    state.availability.mark_ready();
    let trap = UnixListener::bind(&state.go_socket_path).unwrap();
    let (status, devices) =
        request(&state, Entry::Public, "GET", "/api/sms/devices", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{devices}");
    assert_eq!(devices["devices"][0]["ready"], false, "{devices}");
    // No actor session is a confirmed disconnected route (unknown=false),
    // not a failed observation. Automatic selection therefore reports unavailable.
    for device in ["", "dji"] {
        let body = serde_json::to_vec(&serde_json::json!({
            "client_request_id":format!("claim-gap-{device}"),
            "phone":"+15550102020", "message":"never sent", "device":device
        }))
        .unwrap();
        let (status, value) = request(
            &state,
            Entry::Public,
            "POST",
            "/api/sms/send-intents",
            "",
            &body,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
        assert_eq!(
            value["code"],
            if device.is_empty() {
                "sms_device_unavailable"
            } else {
                "sms_device_not_ready"
            }
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), trap.accept())
            .await
            .is_err(),
        "sticky native claim must not contact Go before configure"
    );
    assert_eq!(physical.open_count(), 0);
    assert_eq!(physical.command_count(), 0);
    state.module_radio.finish_shutdown().await.unwrap();
    state
        .modem_actor
        .shutdown()
        .expect("final modem close acknowledgement");
    state.usb_at.shutdown().await.unwrap();
}
