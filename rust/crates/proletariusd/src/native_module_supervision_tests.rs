//! Native shutdown ownership and real HTTP routing with an injected USB transport.
//! All card replies are synthetic; no USB discovery, SMS submission, or call occurs.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI32, AtomicUsize, Ordering},
};
use std::time::Duration;

use crate::usb_at_transport::{Transport, TransportError};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};

const NATIVE: &str = "supervision-native-usb";
const GO: &str = "supervision-go-before";
const ICCID: &str = "8901000000000000000";

#[derive(Default)]
struct Trace {
    commands: Mutex<Vec<String>>,
    esim_replies: Mutex<VecDeque<String>>,
    opens: AtomicUsize,
    drops: AtomicUsize,
    cfun: AtomicI32,
}
impl Trace {
    fn overview(&self) {
        use proletarius_ccid::bertlv;
        let wrap = |tag: &[u8], value: &[u8]| {
            let mut result = vec![];
            bertlv::encode(tag, value, &mut result).unwrap();
            result
        };
        let mut eid = wrap(&[0xbf, 0x3e], &wrap(&[0x5a], &[0x12; 16]));
        eid.extend([0x90, 0]);
        let mut fields = wrap(&[0x5a], &proletarius_euicc::encode_iccid(ICCID).unwrap());
        fields.extend(wrap(&[0x9f, 0x70], &[1]));
        fields.extend(wrap(&[0x90], b"Supervisor fixture"));
        let mut profiles = wrap(&[0xbf, 0x2d], &wrap(&[0xa0], &wrap(&[0xe3], &fields)));
        profiles.extend([0x90, 0]);
        let cgla = |bytes: &[u8]| {
            let hex = bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
            format!("+CGLA: {},\"{}\"\r\nOK\r\n", hex.len(), hex)
        };
        self.esim_replies.lock().unwrap().extend([
            "+CCHO: 1\r\nOK\r\n".into(),
            cgla(&eid),
            cgla(&[0x6a, 0x81]),
            cgla(&[0x6a, 0x81]),
            cgla(&[0x6a, 0x81]),
            cgla(&profiles),
            "OK\r\n".into(),
            "ERROR\r\n".into(),
            "ERROR\r\n".into(),
        ]);
    }
    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}
struct Port(Arc<Trace>);
impl Drop for Port {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn description(&self) -> String {
        "supervision simulated USB".into()
    }
    fn command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        assert!(
            follow_up.is_none(),
            "fixture must never submit an SMS payload"
        );
        assert!(!command.starts_with("AT+CMGS") && !command.starts_with("ATD"));
        self.0.commands.lock().unwrap().push(command.into());
        if command == "AT+CFUN?" {
            return Ok(format!(
                "+CFUN: {}\r\nOK\r\n",
                self.0.cfun.load(Ordering::SeqCst)
            ));
        }
        if let Some(mode) = command.strip_prefix("AT+CFUN=") {
            self.0
                .cfun
                .store(mode.parse().expect("fixture CFUN mode"), Ordering::SeqCst);
            return Ok("OK\r\n".into());
        }
        if command.starts_with("AT+CCHO=")
            || command.starts_with("AT+CGLA=")
            || command.starts_with("AT+CCHC=")
        {
            return Ok(self
                .0
                .esim_replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected eUICC transaction"));
        }
        Ok(match command {
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
            "AT+CEREG?" => "+CEREG: 0,1\r\nOK\r\n",
            "AT+CREG?" => "+CREG: 0,1\r\nOK\r\n",
            "AT+QCCID" => "+QCCID: 8901000000000000000\r\nOK\r\n",
            "AT+CIMI" => "001010000000001\r\nOK\r\n",
            "AT+COPS?" => "+COPS: 0,2,\"00101\",7\r\nOK\r\n",
            "AT+CPMS?" => "+CPMS: \"SM\",0,50,\"SM\",0,50,\"SM\",0,50\r\nOK\r\n",
            _ => "OK\r\n",
        }
        .into())
    }
}
struct Fixture {
    root: PathBuf,
    state: crate::ServerState,
    trace: Arc<Trace>,
    session: String,
}
impl Fixture {
    async fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "s503-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let trace = Arc::new(Trace::default());
        trace.cfun.store(1, Ordering::SeqCst);
        let mut state = crate::reader_pcsc_gateway_tests::state(&root);
        let module_gate = state.availability.module_recovery().clone();
        let physical = trace.clone();
        let usb = crate::usb_at::Owner::with_factory(
            NATIVE.into(),
            Arc::new(move || {
                physical.opens.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Port(physical.clone())))
            }),
        )
        .with_recovery_gate_for_tests(module_gate.clone());
        state.modem_actor =
            crate::modem_actor::ModemActor::new_with_module_recovery(module_gate).unwrap();
        state.usb_at = usb;
        state.sms_inbox = crate::message_archive::InboxState::for_go_producer(GO.into());
        state
            .native_vowifi_lifecycle
            .ordinary_admission()
            .set_go_producer(GO)
            .unwrap();
        state.usb_sms_receive =
            crate::test_usb_sms_receiver(state.sms_inbox.clone(), root.join("sms-archive.json"));
        state.call_facts = crate::call_facts::CallFacts::new(state.call_archive.clone(), GO.into());
        state.module_radio = crate::module_radio::Service::new(
            root.clone(),
            state.usb_at.clone(),
            state.modem_actor.clone(),
            state.availability.clone(),
            NATIVE.into(),
        );
        state.usb_at.claim_native_management().unwrap();
        state
            .sms_inbox
            .bind_native_module_producer(NATIVE.into())
            .unwrap();
        let session = state.usb_at.open_native_session().await.unwrap();
        state
            .module_radio
            .configure_native_usb(&session)
            .await
            .unwrap();
        assert!(!state.module_radio.usb_esim_route_available());
        state.module_radio.bootstrap_native_usb().await.unwrap();
        state.usb_at.commit_native_sms_ready(&session).unwrap();
        assert!(trace.commands().iter().any(|command| command == "AT+CFUN?"));
        Self {
            root,
            state,
            trace,
            session,
        }
    }
    fn assert_native_retained(&self) {
        self.state
            .usb_at
            .validate_radio_binding(NATIVE, &self.session)
            .unwrap();
        assert_eq!(
            self.state.module_radio.usb_transport_session().as_deref(),
            Some(self.session.as_str())
        );
        assert!(self.state.module_radio.usb_esim_route_available());
        assert!(self.state.usb_at.native_sms_ready());
        assert_eq!(self.trace.opens.load(Ordering::SeqCst), 1);
        assert_eq!(self.trace.drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            self.state.sms_inbox.current_module_producer_id().as_deref(),
            Some(NATIVE)
        );
    }
    async fn finish(self) {
        self.state.usb_at.shutdown().await.unwrap();
        assert_eq!(self.trace.drops.load(Ordering::SeqCst), 1);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
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
    internal_token: &str,
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
                .header("x-switch-internal-token", internal_token)
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap(),
        ),
    )
    .await
    .expect("native request must not wait for compatibility child")
    .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
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

#[tokio::test]
async fn native_shutdown_keeps_usb_cleanup_owner_until_physical_close() {
    let f = Fixture::new().await;
    let before = f.trace.commands();
    f.state.module_radio.begin_shutdown();
    // Admission closes before physical ownership. Radio restoration still
    // needs the exact USB session until the retained cleanup has completed.
    f.state
        .usb_at
        .validate_radio_binding(NATIVE, &f.session)
        .unwrap();
    assert_eq!(f.trace.drops.load(Ordering::SeqCst), 0);
    assert_eq!(f.trace.opens.load(Ordering::SeqCst), 1);
    assert_eq!(f.trace.commands(), before);
    assert_eq!(
        f.state.sms_inbox.current_module_producer_id().as_deref(),
        Some(NATIVE)
    );
    f.state.module_radio.finish_shutdown().await.unwrap();
    assert_eq!(f.trace.drops.load(Ordering::SeqCst), 0);
    let session = f.session.clone();
    f.state.usb_at.shutdown().await.unwrap();
    assert_eq!(f.trace.drops.load(Ordering::SeqCst), 1);
    assert!(
        f.state
            .usb_at
            .validate_radio_binding(NATIVE, &session)
            .is_err()
    );
    // Repeated finalization cannot close a replacement or double-drop a port.
    f.finish().await;
}

#[tokio::test]
async fn managed_private_module_rpcs_reject_before_body_and_io_on_every_entrypoint() {
    let f = Fixture::new().await;
    let before = f.trace.commands();
    for entry in [Entry::Public, Entry::Private, Entry::Stopping] {
        for path in [
            "/api/internal/usb-at/close",
            "/api/internal/module-radio/configure",
            "/api/internal/usb-sms/configure",
            "/api/internal/modem/close",
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
                    // Private RPCs never enter the public router, even with
                    // the valid internal credential.
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
        f.trace.commands(),
        before,
        "denied RPC must not reach physical I/O"
    );
    assert!(
        f.state.call_mode.get().is_none(),
        "denied RPC must not initialize call-mode service"
    );
    assert!(f.state.module_call.get().is_none());
    f.assert_native_retained();
    f.finish().await;
}

#[tokio::test]
async fn failed_go_does_not_gate_native_public_status_or_esim_or_connect_to_go() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    // Public eSIM handlers consult the process-wide operation journal, just as
    // daemon startup does. Other eSIM fixtures may already have initialized it.
    assert!(!crate::reader_esim_download::has_active_operation());
    f.state.availability.mark_failed();
    let trap = UnixListener::bind(&f.state.go_socket_path).unwrap();
    let before = f.trace.commands();
    let (status, health) = request(&f.state, Entry::Public, "GET", "/api/health", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["esim_available"], true);
    assert_eq!(health["port"], "supervision simulated USB");
    assert_eq!(
        f.trace.commands(),
        before,
        "health must use the completed bootstrap receipt"
    );
    let (status, device) = request(&f.state, Entry::Public, "GET", "/api/status", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{device}");
    assert_eq!(device["iccid"], ICCID);
    assert_eq!(device["sim_inserted"], true);
    f.trace.overview();
    let (status, overview) = request(&f.state, Entry::Public, "GET", "/api/esim", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["profiles"][0]["profiles"][0]["iccid"], ICCID);
    let (status, health) =
        request(&f.state, Entry::Public, "GET", "/api/esim/health", "", b"").await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["module_iccid"], ICCID);
    assert_eq!(health["profile_matches_module"], true);
    assert!(
        f.trace.commands().iter().any(|c| c.starts_with("AT+CGLA=")),
        "eSIM result must come from actual native APDU workflow"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(40), trap.accept())
            .await
            .is_err(),
        "native public routes must never connect to the compatibility socket"
    );
    f.assert_native_retained();
    drop(trap);
    f.finish().await;
}
