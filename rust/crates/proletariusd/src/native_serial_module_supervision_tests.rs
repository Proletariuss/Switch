//! Independent serial module composition tests: real PTY and public UDS dispatch.
//! All SIM/AT responses are synthetic. No external network or hardware is used.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use serialport::SerialPort as _;
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};

const PRODUCER: &str = "serial-module-509-fixture";
const ICCID: &str = "8901234567890123456";
const IMSI: &str = "234330123456789";

#[derive(Default)]
pub(crate) struct Script {
    commands: Mutex<Vec<String>>,
    selected: Mutex<[u8; 2]>,
    gate_enter: AtomicBool,
    entered: AtomicBool,
    resume: AtomicBool,
    stop: AtomicBool,
    reject_pdu: AtomicBool,
    restore_fails: AtomicBool,
    changed: AtomicBool,
    pub(crate) cfun: AtomicUsize,
    pub(crate) auth: AtomicUsize,
}
impl Script {
    fn apdu(&self, c: &[u8]) -> Vec<u8> {
        let mut selected = self.selected.lock().unwrap();
        let mut response = match c[1] {
            0xa4 => {
                if c.len() >= 7 && c[2] != 4 {
                    *selected = [c[5], c[6]];
                }
                vec![]
            }
            0xb2 if *selected == [0x2f, 0] && c[2] == 1 => vec![
                0x61, 11, 0x4f, 9, 0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1, 0x61, 11, 0x4f, 9, 0xa0,
                0, 0, 0, 0x87, 0x10, 4, 0xff, 1,
            ],
            0xb2 => return vec![0x6a, 0x83],
            0xb0 => match *selected {
                [0x6f, 0xad] => vec![0, 0, 0, 2],
                [0x6f, 7] if c[4] == 1 => vec![8],
                [0x6f, 7] => vec![8, 0x29, 0x43, 0x33, 0x10, 0x32, 0x54, 0x76, 0x98],
                [0x2f, 0xe2] => vec![
                    0x98,
                    0x10,
                    0x32,
                    0x54,
                    0x76,
                    0x98,
                    0x10,
                    0x32,
                    0x54,
                    if self.changed.load(Ordering::Acquire) {
                        0xf7
                    } else {
                        0xf6
                    },
                ],
                _ => return vec![0x6a, 0x82],
            },
            0x88 => {
                self.auth.fetch_add(1, Ordering::SeqCst);
                let ike = c.windows(16).any(|v| v == [0x11; 16]);
                let mut value = vec![0xdb, 8];
                value.extend(if ike {
                    vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
                } else {
                    vec![1; 8]
                });
                value.push(16);
                value.extend([if ike { 0x31 } else { 2 }; 16]);
                value.push(16);
                value.extend([if ike { 0x42 } else { 3 }; 16]);
                value
            }
            _ => return vec![0x6a, 0x81],
        };
        response.extend([0x90, 0]);
        response
    }
    fn response(&self, command: &str) -> String {
        match command {
            "AT+CMGF=0" if self.reject_pdu.load(Ordering::Acquire) => "ERROR\r\n".into(),
            "AT+CGSN" => "123456789012345\r\nOK\r\n".into(),
            "AT+CGMR" => "SERIAL-MODULE-FIXTURE\r\nOK\r\n".into(),
            "AT+QSIMSTAT?" => "+QSIMSTAT: 1,1\r\nOK\r\n".into(),
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n".into(),
            "AT+QCCID" => format!("+QCCID: {ICCID}\r\nOK\r\n"),
            "AT+CIMI" => format!("{IMSI}\r\nOK\r\n"),
            "AT+CNUM" => "+CNUM: \"\",\"+447700900123\",145\r\nOK\r\n".into(),
            "AT+CSCA?" => "+CSCA: \"+447802002606\",145\r\nOK\r\n".into(),
            "AT+CFUN?" => format!("+CFUN: {}\r\nOK\r\n", self.cfun.load(Ordering::SeqCst)),
            "AT+CFUN=1" if self.restore_fails.load(Ordering::Acquire) => "ERROR\r\n".into(),
            c if c.starts_with("AT+CFUN=") => {
                self.cfun.store(
                    c.trim_start_matches("AT+CFUN=").parse().unwrap(),
                    Ordering::SeqCst,
                );
                "OK\r\n".into()
            }
            "AT+CREG?" | "AT+CEREG?" => "+CREG: 0,1\r\nOK\r\n".into(),
            "AT+CSQ" => "+CSQ: 20,0\r\nOK\r\n".into(),
            "AT+COPS?" => "+COPS: 0,2,\"23433\",7\r\nOK\r\n".into(),
            "AT+QNWINFO" => "+QNWINFO: \"FDD LTE\",\"23433\",\"LTE BAND 3\",1300\r\nOK\r\n".into(),
            "AT+CGDCONT?" => {
                "+CGDCONT: 1,\"IPV4V6\",\"fixture.apn\",\"0.0.0.0\",0,0\r\nOK\r\n".into()
            }
            "AT+QIMS?" => "+QIMS: 1\r\nOK\r\n".into(),
            "AT+QCFG=\"ims\"" => "+QCFG: \"ims\",1\r\nOK\r\n".into(),
            "AT+QCFG=\"usbnet\"" | "AT+QCFG=\"usbnet\"?" => "+QCFG: \"usbnet\",2\r\nOK\r\n".into(),
            "AT+CPMS?" => "+CPMS: \"SM\",0,50,\"SM\",0,50,\"SM\",0,50\r\nOK\r\n".into(),
            c if c.starts_with("AT+CSIM=") => {
                let hex = c.split('"').nth(1).expect("strict CSIM");
                let c = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect::<Vec<_>>();
                let bytes = self.apdu(&c);
                let hex = bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
                format!("+CSIM: {},\"{hex}\"\r\nOK\r\n", hex.len())
            }
            c if c.starts_with("AT+CCHO=") => "ERROR\r\n".into(),
            _ => "OK\r\n".into(),
        }
    }
}
pub(crate) struct Fixture {
    root: PathBuf,
    pub(crate) state: crate::ServerState,
    startup: crate::module_startup::Service,
    pub(crate) script: Arc<Script>,
    peer: Option<std::thread::JoinHandle<()>>,
    finished: AtomicBool,
}
impl Fixture {
    pub(crate) async fn new(reject_pdu: bool) -> Self {
        Self::new_inner(reject_pdu, false).await
    }
    async fn new_quiesced(reject_pdu: bool) -> Self {
        Self::new_inner(reject_pdu, true).await
    }
    async fn new_inner(reject_pdu: bool, quiesce_sms_receiver: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("s509-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        crate::module_radio_store::Store::new(root.clone())
            .save(crate::module_radio_store::Preferences {
                enabled: false,
                restore_cfun: 1,
                user_configured: true,
            })
            .unwrap();
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let port = slave.name().unwrap();
        drop(slave);
        let script = Arc::new(Script::default());
        script.cfun.store(1, Ordering::SeqCst);
        script.reject_pdu.store(reject_pdu, Ordering::Release);
        let peer_script = script.clone();
        let peer = std::thread::spawn(move || {
            let mut pending = Vec::new();
            while !peer_script.stop.load(Ordering::Acquire) {
                let mut byte = [0];
                if master.read(&mut byte).ok() != Some(1) {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                if byte[0] != b'\n' {
                    pending.push(byte[0]);
                    continue;
                }
                let command = String::from_utf8(std::mem::take(&mut pending))
                    .unwrap()
                    .trim()
                    .to_owned();
                if command.is_empty() {
                    continue;
                }
                assert!(!command.starts_with("AT+CMGS") && !command.starts_with("ATD"));
                peer_script.commands.lock().unwrap().push(command.clone());
                if command == "AT+CFUN=4" && peer_script.gate_enter.swap(false, Ordering::AcqRel) {
                    peer_script.entered.store(true, Ordering::Release);
                    while !peer_script.resume.load(Ordering::Acquire)
                        && !peer_script.stop.load(Ordering::Acquire)
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                let _ = master.write_all(peer_script.response(&command).as_bytes());
            }
        });
        let mut state = crate::reader_pcsc_gateway_tests::state(&root);
        let module_gate = state.availability.module_recovery().clone();
        let expected = port.clone();
        let actor = crate::modem_actor::ModemActor::with_test_port_opener_and_module_recovery(
            move |path, baud| {
                assert_eq!(path, expected, "fixture must never open hardware");
                assert_eq!(baud, 115200);
                serialport::new(path, 0)
                    .timeout(Duration::from_millis(20))
                    .open()
                    .map_err(|_| crate::modem_actor::ActorError::fixture_open_failed())
            },
            module_gate.clone(),
        )
        .unwrap();
        let usb = crate::usb_at::Owner::unavailable_for_tests(PRODUCER.into())
            .with_recovery_gate_for_tests(module_gate);
        state.modem_actor = actor;
        state.usb_at = usb;
        state.module_radio = crate::module_radio::Service::new(
            root.clone(),
            state.usb_at.clone(),
            state.modem_actor.clone(),
            state.availability.clone(),
            PRODUCER.into(),
        );
        state.availability.mark_failed();
        let startup = crate::module_startup::Service::start_serial_for_test(
            state.modem_actor.clone(),
            state.usb_at.clone(),
            state.module_radio.clone(),
            state.sms_inbox.clone(),
            PRODUCER.into(),
            proletarius_store::ArchiveStore::new(root.join("serial-sms.json")),
            port,
        );
        until(|| state.modem_actor.native_serial_observation().ready).await;
        // `ready` and the radio bootstrap receipt are published by different
        // retained owners. Wait for the public route predicate as well so a
        // fixture cannot expose a transient "unavailable" state.
        until(|| state.module_radio.native_vowifi_route_available()).await;
        if quiesce_sms_receiver {
            // This suite validates module lifecycle and card/RF ownership, not
            // stored-SMS ingest. Join that unrelated retained worker so its
            // empty-store scan cannot race the assertions below.
            state.modem_actor.finish_sms_receiver().await.unwrap();
        } else {
            // Shared production-composition fixtures keep the receiver alive.
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        Self {
            root,
            state,
            startup,
            script,
            peer: Some(peer),
            finished: AtomicBool::new(false),
        }
    }
    pub(crate) async fn finish(&self) {
        if let Some(service) = self.state.native_vowifi_lifecycle.retained_module_service() {
            service.begin_shutdown();
            service
                .stop_before(tokio::time::Instant::now() + Duration::from_secs(5))
                .await
                .unwrap();
        }
        self.startup.begin_shutdown();
        self.state.module_radio.finish_shutdown().await.unwrap();
        self.startup.shutdown().await.unwrap();
        self.state.usb_at.shutdown().await.unwrap();
        self.state
            .modem_actor
            .shutdown()
            .expect("final modem close acknowledgement");
        self.finished.store(true, Ordering::Release);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.script.resume.store(true, Ordering::Release);
        if !self.finished.swap(true, Ordering::AcqRel) {
            if let Some(service) = self.state.native_vowifi_lifecycle.retained_module_service() {
                service.begin_shutdown();
            }
            self.startup.begin_shutdown();
            self.state.module_radio.begin_shutdown();
            let _ = self.state.modem_actor.shutdown();
        }
        self.script.stop.store(true, Ordering::Release);
        if let Some(peer) = self.peer.take() {
            let _ = peer.join();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(12), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("serial fixture bounded progress");
}
async fn retain_serial_after_startup(
    f: &Fixture,
    intent: &crate::module_radio::NativeVoWiFiIntent,
) -> crate::modem_actor::SerialModuleHold {
    let binding = intent.serial_binding().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        // Always settle an admitted retain request. Cancelling this await could
        // lose the successful receipt while the actor keeps the physical hold.
        match f
            .state
            .modem_actor
            .retain_serial_module(&binding, intent.usb_inhibit())
            .await
        {
            Ok(hold) => return hold,
            Err(error) if error.code == "serial_card_busy" => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "serial fixture startup reached idle admission"
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Err(error) => panic!("serial fixture admission failed: {}", error.code),
        }
    }
}
async fn request(
    state: &crate::ServerState,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (client, server) = UnixStream::pair().unwrap();
    let state = state.clone();
    let serving = tokio::spawn(async move { crate::serve_connection(server, state).await });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        sender.send_request(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost")
                .header("x-proletarius-app-token", "app-fixture")
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let code = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(crate::benign_connection_error_message(&error.to_string()));
    }
    if let Err(error) = serving.await.unwrap() {
        assert!(crate::benign_connection_error_message(&error.to_string()));
    }
    (code, value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_serial_module_start_stop_retains_real_identity_and_rf_until_enter_settles() {
    let f = Fixture::new_quiesced(false).await;
    let trap = UnixListener::bind(&f.state.go_socket_path).unwrap();
    let (code, idle) = request(&f.state, "GET", "/api/vowifi", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{idle}");
    assert_eq!(
        idle["state"], "disabled",
        "configured native serial must be available"
    );
    assert_eq!(idle["cfun_mode"], 1);
    f.script.gate_enter.store(true, Ordering::Release);
    let (code, start) = request(
        &f.state,
        "POST",
        "/api/vowifi",
        json!({
            "enable": true,
            "observe_operation": true,
            "client_request_id": "serial-module-start-stop-start"
        }),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{start}");
    until(|| f.script.entered.load(Ordering::Acquire)).await;
    assert!(f.state.module_radio.vowifi_owns_sim_channel());
    let service = f
        .state
        .native_vowifi_lifecycle
        .retained_module_service()
        .unwrap();
    assert!(service.owns_session());
    let commands = f.script.commands.lock().unwrap().clone();
    assert!(
        commands.iter().any(|c| c.starts_with("AT+CSIM=")),
        "production start must read real SIM APDUs before RF enter"
    );
    assert!(
        !commands.iter().any(|c| c.starts_with("AT+CCHO=")),
        "basic CSIM must not allocate eUICC channels"
    );
    let (code, stop) = request(
        &f.state,
        "POST",
        "/api/vowifi",
        json!({
            "enable": false,
            "observe_operation": true,
            "client_request_id": "serial-module-start-stop-stop"
        }),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{stop}");
    assert_eq!(stop["cancelling"], true);
    assert!(
        service
            .stop_before(tokio::time::Instant::now() + Duration::from_millis(20))
            .await
            .is_err()
    );
    assert!(
        service.owns_session(),
        "caller timeout is not retained worker settlement"
    );
    assert!(f.state.module_radio.vowifi_owns_sim_channel());
    assert!(
        f.state
            .modem_actor
            .native_serial_session()
            .unwrap()
            .observation()
            .terminal
            .is_none()
    );
    let (_, result) = request(&f.state, "GET", "/api/vowifi/result", json!({})).await;
    assert_eq!(result["done"], false);
    f.script.resume.store(true, Ordering::Release);
    service
        .stop_before(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    let (_, result) = request(&f.state, "GET", "/api/vowifi/result", json!({})).await;
    assert_eq!(result["done"], true, "{result}");
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(f.script.cfun.load(Ordering::Acquire), 1);
    assert!(!service.owns_session());
    assert!(!f.state.module_radio.vowifi_owns_sim_channel());
    assert_eq!(
        f.script.auth.load(Ordering::Acquire),
        0,
        "cancelled preparation must not start a network AKA"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), trap.accept())
            .await
            .is_err(),
        "native module routes must not connect to Go"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn module_vowifi_http_request_id_replays_exact_operation_and_rejects_payload_drift() {
    let f = Fixture::new_quiesced(false).await;
    f.script.gate_enter.store(true, Ordering::Release);
    let body = json!({
        "enable": true,
        "observe_operation": true,
        "client_request_id": "swift-module-start-1"
    });

    let (code, first) = request(&f.state, "POST", "/api/vowifi", body.clone()).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{first}");
    assert_eq!(
        first["operation"]["client_request_id"],
        "swift-module-start-1"
    );
    let operation_id = first["operation"]["operation_id"].clone();
    let generation = first["operation"]["generation"].clone();

    let (code, replay) = request(&f.state, "POST", "/api/vowifi", body).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{replay}");
    assert_eq!(replay["operation"]["operation_id"], operation_id);
    assert_eq!(replay["operation"]["generation"], generation);
    assert_eq!(
        replay["operation"]["client_request_id"],
        "swift-module-start-1"
    );

    let (code, conflict) = request(
        &f.state,
        "POST",
        "/api/vowifi",
        json!({
            "enable": "changed",
            "observe_operation": true,
            "client_request_id": "swift-module-start-1"
        }),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "vowifi_request_id_conflict");

    let (code, recovered) = request(
        &f.state,
        "GET",
        "/api/vowifi/result?observe_operation=true&client_request_id=swift-module-start-1",
        json!({}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{recovered}");
    assert_eq!(recovered["operation"]["operation_id"], operation_id);
    assert_eq!(recovered["operation"]["generation"], generation);
    assert_eq!(
        recovered["operation"]["client_request_id"],
        "swift-module-start-1"
    );

    f.script.resume.store(true, Ordering::Release);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claimed_serial_before_radio_configuration_never_falls_back_to_live_usb_module() {
    let root =
        std::env::temp_dir().join(format!("s509-route-{}", crate::snapshot::new_instance_id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut state = crate::reader_pcsc_gateway_tests::state(&root);
    let module_gate = state.availability.module_recovery().clone();
    state.modem_actor =
        crate::modem_actor::ModemActor::new_with_module_recovery(module_gate.clone()).unwrap();
    // Claim native ownership before opening, exactly as production startup does.
    // This separate synthetic USB card is genuinely usable if routing regresses.
    struct UsbPort(Arc<Script>);
    impl crate::usb_at_transport::Transport for UsbPort {
        fn checked_close(
            self: Box<Self>,
        ) -> Result<
            crate::usb_at_transport::CloseDisposition,
            crate::usb_at_transport::TransportError,
        > {
            // Explicit fixture release; Drop barriers must finish before success.
            drop(self);
            Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
        }

        fn command(
            &mut self,
            command: &str,
            follow_up: Option<&[u8]>,
            _: Duration,
            _: usize,
        ) -> Result<String, crate::usb_at_transport::TransportError> {
            assert!(follow_up.is_none());
            self.0.commands.lock().unwrap().push(command.to_owned());
            Ok(self.0.response(command))
        }
        fn description(&self) -> String {
            "independent live USB selection fixture".into()
        }
    }
    let trace = Arc::new(Script::default());
    trace.cfun.store(1, Ordering::Release);
    let physical = trace.clone();
    state.usb_at = crate::usb_at::Owner::with_factory(
        "native-module-test".into(),
        Arc::new(move || Ok(Box::new(UsbPort(physical.clone())))),
    )
    .with_recovery_gate_for_tests(module_gate);
    state.usb_at.claim_native_management().unwrap();
    let session = state.usb_at.open_native_session().await.unwrap();
    state.module_radio = crate::module_radio::Service::new(
        root.clone(),
        state.usb_at.clone(),
        state.modem_actor.clone(),
        state.availability.clone(),
        "native-module-test".into(),
    );
    state
        .module_radio
        .configure_native_usb(&session)
        .await
        .unwrap();
    state.modem_actor.claim_native_serial_management().unwrap();
    state.availability.mark_failed();
    let before = trace.commands.lock().unwrap().len();
    let (code, idle) = request(&state, "GET", "/api/vowifi", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{idle}");
    assert_eq!(
        idle["state"], "unavailable",
        "serial selection remains authoritative without a configured serial session"
    );
    let (_, start) = request(
        &state,
        "POST",
        "/api/vowifi",
        json!({
            "enable": true,
            "observe_operation": true,
            "client_request_id": "serial-selection-mismatch-start"
        }),
    )
    .await;
    assert_eq!(start["accepted"], true);
    let service = state
        .native_vowifi_lifecycle
        .retained_module_service()
        .unwrap();
    until(|| !service.owns_session()).await;
    assert_eq!(
        trace.commands.lock().unwrap().len(),
        before,
        "selection mismatch must not send even USB RF/identity probes"
    );
    assert_eq!(trace.auth.load(Ordering::Acquire), 0);
    state.module_radio.finish_shutdown().await.unwrap();
    state.usb_at.shutdown().await.unwrap();
    state
        .modem_actor
        .shutdown()
        .expect("final modem close acknowledgement");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serial_module_full_card_aka_works_without_pdu_ready_and_failed_restore_keeps_hold() {
    use crate::native_vowifi_session::card::OnDemandCard;
    use crate::reader_session_lease::ReaderAkaApplication;
    let f = Fixture::new_quiesced(true).await;
    // Startup owns the same serial actor while its initial empty-SMS scan is
    // settling. Acquire the production hold as a deterministic admission
    // barrier; ModuleSessionCard recovers this exact binding/token below.
    let intent = f.state.module_radio.native_vowifi_intent().unwrap();
    let _startup_hold = retain_serial_after_startup(&f, &intent).await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let card = crate::module_vowifi_card::ModuleSessionCard::begin_prepare_with_serial_intent(
        f.state.usb_at.clone(),
        f.state.module_radio.clone(),
        f.state.modem_actor.clone(),
        intent,
        stopped,
    );
    card.wait_ready().await.unwrap();
    assert_eq!(card.identity().iccid.as_str(), ICCID);
    assert_eq!(card.identity().imsi.as_str(), IMSI);
    assert_eq!(card.status().cfun_mode, Some(4));
    assert!(matches!(
        card.physical_identity(),
        Some(crate::module_vowifi_card::ModulePhysicalIdentity::Serial { .. })
    ));
    card.validate_business().await.unwrap();
    for application in [ReaderAkaApplication::Usim, ReaderAkaApplication::Isim] {
        assert!(matches!(
            card.clone()
                .authenticate(application, [1; 16], [2; 16])
                .await
                .unwrap(),
            proletarius_vowifi::SimAkaResult::Success { .. }
        ));
    }
    assert_eq!(f.script.auth.load(Ordering::Acquire), 2);
    let session = f.state.modem_actor.native_serial_session().unwrap();
    let before = f.script.commands.lock().unwrap().len();
    assert!(
        f.state
            .modem_actor
            .refresh_native_serial_metadata(&session)
            .await
            .is_err()
    );
    assert_eq!(
        f.script.commands.lock().unwrap().len(),
        before,
        "metadata must not send QSIMSTAT/CPIN under module hold"
    );
    assert!(card.is_current());
    f.script.restore_fails.store(true, Ordering::Release);
    assert!(card.clone().release_with_confirmation().await.is_err());
    assert!(!card.status().cleanup_confirmed);
    assert!(f.state.modem_actor.serial_module_held());
    assert!(f.state.module_radio.vowifi_owns_sim_channel());
    assert!(
        f.state
            .modem_actor
            .close_native_serial(&session)
            .await
            .is_err()
    );
    f.script.restore_fails.store(false, Ordering::Release);
    card.clone().release_with_confirmation().await.unwrap();
    assert!(card.status().cleanup_confirmed);
    assert_eq!(f.script.cfun.load(Ordering::Acquire), 1);
    assert!(!f.state.modem_actor.serial_module_held());
    assert!(!f.state.module_radio.vowifi_owns_sim_channel());
    drop(stop);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serial_module_changed_sim_sends_zero_authenticate_and_still_restores_exact_radio() {
    use crate::native_vowifi_session::card::OnDemandCard;
    let f = Fixture::new_quiesced(false).await;
    let intent = f.state.module_radio.native_vowifi_intent().unwrap();
    let _startup_hold = retain_serial_after_startup(&f, &intent).await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let card = crate::module_vowifi_card::ModuleSessionCard::begin_prepare_with_serial_intent(
        f.state.usb_at.clone(),
        f.state.module_radio.clone(),
        f.state.modem_actor.clone(),
        intent,
        stopped,
    );
    card.wait_ready().await.unwrap();
    f.script.changed.store(true, Ordering::Release);
    assert!(
        card.clone()
            .authenticate(
                crate::reader_session_lease::ReaderAkaApplication::Usim,
                [1; 16],
                [2; 16]
            )
            .await
            .is_err()
    );
    assert_eq!(f.script.auth.load(Ordering::Acquire), 0);
    assert!(!card.is_current());
    card.clone().release_with_confirmation().await.unwrap();
    assert_eq!(f.script.cfun.load(Ordering::Acquire), 1);
    assert!(!f.state.modem_actor.serial_module_held());
    drop(stop);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn captured_serial_intent_retired_before_hold_releases_without_touching_replacement_rf() {
    let f = Fixture::new_quiesced(false).await;
    let intent = f.state.module_radio.native_vowifi_intent().unwrap();
    // Freeze the automatic bootstrap owner so it cannot configure another RF
    // epoch while the deliberately stale captured capability is exercised.
    f.startup.begin_shutdown();
    f.state
        .module_radio
        .retire_producer(PRODUCER.into())
        .await
        .unwrap();
    let before = f.script.commands.lock().unwrap().len();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let card = crate::module_vowifi_card::ModuleSessionCard::begin_prepare_with_serial_intent(
        f.state.usb_at.clone(),
        f.state.module_radio.clone(),
        f.state.modem_actor.clone(),
        intent,
        stopped,
    );
    assert!(card.wait_ready().await.is_err());
    card.clone().release_with_confirmation().await.unwrap();
    assert!(card.status().cleanup_confirmed);
    assert!(!f.state.modem_actor.serial_module_held());
    assert!(!f.state.module_radio.vowifi_owns_sim_channel());
    assert_eq!(
        f.script.commands.lock().unwrap().len(),
        before,
        "a settled rejected prepare must release its hold without SIM or RF commands"
    );
    assert_eq!(f.script.cfun.load(Ordering::Acquire), 1);
    drop(stop);
    f.finish().await;
}
