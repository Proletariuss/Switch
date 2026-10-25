//! Production UDS dispatch and real PTY bytes; no hardware or carrier network.
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
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

mod download;

const ICCID: &str = "8901000000000000000";
const AID: &str = "A0000005591010FFFFFFFF8900000100";

#[derive(Default)]
struct Card {
    commands: Mutex<Vec<String>>,
    enabled: AtomicBool,
    deleted: AtomicBool,
    renamed: AtomicBool,
    block_close: AtomicBool,
    close_entered: AtomicBool,
    block_followup_open: AtomicBool,
    followup_open_entered: AtomicBool,
}
fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    proletarius_ccid::bertlv::encode(tag, value, &mut out).unwrap();
    out
}
fn cgla(mut data: Vec<u8>) -> String {
    data.extend([0x90, 0]);
    let hex = data.iter().map(|b| format!("{b:02X}")).collect::<String>();
    format!("+CGLA: {},\"{hex}\"\r\nOK\r\n", hex.len())
}
impl Card {
    fn response(&self, command: &str) -> String {
        self.commands.lock().unwrap().push(command.into());
        if command.starts_with("AT+CCHO=") {
            if command != format!("AT+CCHO=\"{AID}\"") {
                return "ERROR\r\n".into();
            }
            return "+CCHO: 1\r\nOK\r\n".into();
        }
        if command.starts_with("AT+CGLA=") {
            let hex = command.split('"').nth(1).unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>();
            let tag = bytes.windows(2).find(|v| v[0] == 0xbf).map(|v| v[1]);
            return match tag {
                Some(0x3e) => cgla(tlv(&[0xbf, 0x3e], &tlv(&[0x5a], &[0x12; 16]))),
                Some(0x2d) => {
                    let mut fields = Vec::new();
                    if !self.deleted.load(Ordering::Acquire) {
                        let mut profile =
                            tlv(&[0x5a], &proletarius_euicc::encode_iccid(ICCID).unwrap());
                        profile.extend(tlv(
                            &[0x9f, 0x70],
                            &[u8::from(self.enabled.load(Ordering::Acquire))],
                        ));
                        profile.extend(tlv(
                            &[0x90],
                            if self.renamed.load(Ordering::Acquire) {
                                b"Renamed"
                            } else {
                                b"Fixture"
                            },
                        ));
                        fields.extend(tlv(&[0xe3], &profile));
                    }
                    cgla(tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &fields)))
                }
                Some(tag @ (0x29 | 0x31 | 0x33)) => {
                    let encoded = proletarius_euicc::encode_iccid(ICCID).unwrap();
                    assert!(
                        bytes.windows(encoded.len()).any(|v| v == encoded),
                        "mutation must name the selected fixture ICCID"
                    );
                    match tag {
                        0x29 => self.renamed.store(true, Ordering::Release),
                        0x31 => self.enabled.store(true, Ordering::Release),
                        0x33 => self.deleted.store(true, Ordering::Release),
                        _ => unreachable!(),
                    }
                    cgla(tlv(&[0xbf, tag], &[0x80, 1, 0]))
                }
                Some(0x28) => cgla(tlv(&[0xbf, 0x28], &[0xa0, 0])),
                _ => "+CGLA: 4,\"6A81\"\r\nOK\r\n".into(),
            };
        }
        match command {
            "AT+QCCID" => format!("+QCCID: {ICCID}\r\nOK\r\n"),
            "AT+CIMI" => "001010000000001\r\nOK\r\n".into(),
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n".into(),
            "AT+CREG?" | "AT+CEREG?" => "+CREG: 0,1\r\nOK\r\n".into(),
            "AT+CSQ" => "+CSQ: 20,0\r\nOK\r\n".into(),
            "AT+COPS?" => "+COPS: 0,2,\"00101\",7\r\nOK\r\n".into(),
            "AT+QNWINFO" => "+QNWINFO: \"FDD LTE\",\"00101\",\"LTE BAND 3\",1300\r\nOK\r\n".into(),
            _ => "OK\r\n".into(),
        }
    }
}
struct Port {
    path: String,
    card: Arc<Card>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Port {
    fn new() -> Self {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let path = slave.name().unwrap();
        drop(slave);
        let stop = Arc::new(AtomicBool::new(false));
        let card = Arc::new(Card::default());
        let (stopped, trace) = (stop.clone(), card.clone());
        let worker = std::thread::spawn(move || {
            let mut line = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let mut byte = [0];
                match master.read(&mut byte) {
                    Ok(1) => {}
                    _ => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
                if byte[0] != b'\n' {
                    line.push(byte[0]);
                    continue;
                }
                let command = String::from_utf8(std::mem::take(&mut line))
                    .unwrap()
                    .trim()
                    .to_owned();
                if !command.is_empty() {
                    if command == format!("AT+CCHO=\"{AID}\"")
                        && trace.renamed.load(Ordering::Acquire)
                        && trace.block_followup_open.load(Ordering::Acquire)
                    {
                        trace.followup_open_entered.store(true, Ordering::Release);
                        while trace.block_followup_open.load(Ordering::Acquire)
                            && !stopped.load(Ordering::Acquire)
                        {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    if command.starts_with("AT+CCHC=") && trace.block_close.load(Ordering::Acquire)
                    {
                        trace.close_entered.store(true, Ordering::Release);
                        while trace.block_close.load(Ordering::Acquire)
                            && !stopped.load(Ordering::Acquire)
                        {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    let response = trace.response(&command);
                    let _ = master.write_all(response.as_bytes());
                }
            }
        });
        Self {
            path,
            card,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
struct Fixture {
    state: crate::ServerState,
    root: PathBuf,
    port: Port,
    finished: AtomicBool,
}
impl Fixture {
    async fn new() -> Self {
        let root = crate::tests::test_directory("serial-esim-native-gateway");
        let port = Port::new();
        let expected = port.path.clone();
        let mut state = crate::reader_pcsc_gateway_tests::state(&root);
        state.modem_actor =
            crate::modem_actor::ModemActor::with_test_port_opener(move |path, baud| {
                assert_eq!(path, expected, "never open a physical test card");
                assert_eq!(baud, 115200);
                serialport::new(path, 0)
                    .timeout(Duration::from_millis(20))
                    .open()
                    .map_err(|_| crate::modem_actor::ActorError::fixture_open_failed())
            })
            .unwrap();
        state.modem_actor.claim_native_esim_management();
        state.usb_at = crate::usb_at::Owner::unavailable_for_tests("serial507".into());
        state.module_radio = crate::module_radio::Service::new(
            root.clone(),
            state.usb_at.clone(),
            state.modem_actor.clone(),
            state.availability.clone(),
            "serial507".into(),
        );
        let f = Self {
            state,
            root,
            port,
            finished: AtomicBool::new(false),
        };
        f.open(1).await;
        f
    }
    async fn open(&self, sequence: u64) {
        let (status, value) = request(
            &self.state,
            "POST",
            "/api/internal/modem/open",
            json!({"port":self.port.path,"baud_rate":115200,"initialization_owned":true}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let (status,value)=request(&self.state,"POST","/api/internal/module-radio/configure",json!({"producer_id":"serial507","config_seq":sequence,"transport":"serial","session_id":""}),false).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        // Do not initialize SMS or inject CPIN READY. An eUICC with every
        // profile disabled remains manageable even without SMS readiness.
    }
    async fn close(&self) {
        let actor = self.state.modem_actor.clone();
        let session = tokio::task::spawn_blocking(move || actor.serial_sms_observation())
            .await
            .unwrap()
            .unwrap()
            .serial_session
            .unwrap();
        let (status, value) = request(
            &self.state,
            "POST",
            "/api/internal/modem/close",
            json!({"serial_session":session}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    async fn finish(self) {
        let actor = self.state.modem_actor.clone();
        tokio::task::spawn_blocking(move || actor.shutdown())
            .await
            .expect("final modem close task")
            .expect("final modem close acknowledgement");
        self.finished.store(true, Ordering::Release);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            self.port
                .card
                .block_followup_open
                .store(false, Ordering::Release);
            self.port.card.block_close.store(false, Ordering::Release);
            self.state.module_radio.begin_shutdown();
            let _ = self.state.modem_actor.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn request(
    state: &crate::ServerState,
    method: &str,
    path: &str,
    body: Value,
    public: bool,
) -> (StatusCode, Value) {
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let state = state.clone();
    let serving = tokio::spawn(async move {
        let result = if public {
            crate::serve_connection(server, state).await
        } else {
            crate::serve_internal_connection(server, state).await
        };
        if let Err(error) = result {
            assert!(
                crate::benign_connection_error_message(&error.to_string())
                    || error.to_string() == "connection closed before message completed",
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(crate::APP_TOKEN_HEADER, "app-fixture")
        .header("content-type", "application/json");
    if !public {
        req = req.header("X-Switch-Internal-Token", "internal-fixture");
    }
    let response = tokio::time::timeout(
        Duration::from_secs(20),
        sender.send_request(
            req.body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap(),
        ),
    )
    .await
    .expect("native request must not wait for Go")
    .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    let _ = connection.await.unwrap();
    serving.await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn serial_esim_health_reads_same_card_without_go_or_sms_initialization() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    f.port.card.enabled.store(true, Ordering::Release);
    let (status, value) = request(&f.state, "GET", "/api/esim/health", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    // The fixture retains the default cellular RF suppression policy. Card
    // identity is observable without inventing a cellular registration.
    assert_eq!(value["state"], "not_registered", "{value}");
    assert_eq!(value["ok"], false);
    assert_eq!(value["profile_matches_module"], true);
    assert_eq!(value["network_mode"], "飞行模式");
    assert_eq!(value["signal_dbm"], -999);
    assert_eq!(value["module_iccid"], ICCID);
    assert_eq!(value["imsi"], "001010000000001");
    assert!(
        !f.state.modem_actor.serial_sms_observation().unwrap().ready,
        "health observation must not manufacture SMS readiness"
    );
    let commands = f.port.card.commands.lock().unwrap().clone();
    assert!(commands.iter().any(|c| c == "AT+QCCID"));
    assert!(commands.iter().any(|c| c == "AT+CIMI"));
    assert!(!commands.iter().any(|c| c == "AT+CSQ" || c == "AT+COPS?"));
    assert!(
        !commands
            .iter()
            .any(|c| c.starts_with("AT+CPIN") || c.starts_with("AT+QSIMSTAT"))
    );
    f.finish().await;
}

#[tokio::test]
async fn serial_mutation_stays_active_until_final_overview_and_rf_release() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    struct Release(Arc<Card>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.block_followup_open.store(false, Ordering::Release);
        }
    }
    let release = Release(f.port.card.clone());
    f.port
        .card
        .block_followup_open
        .store(true, Ordering::Release);
    let state = f.state.clone();
    let waiter = tokio::spawn(async move {
        request(
            &state,
            "PATCH",
            "/api/esim/profile",
            json!({
                "client_request_id":"serial-active-rename",
                "iccid":ICCID,
                "aid":AID,
                "name":"Renamed"
            }),
            true,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !f.port.card.followup_open_entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.port.card.renamed.load(Ordering::Acquire));
    assert!(crate::reader_esim_download::has_active_operation());
    assert!(f.state.module_radio.sim_channel_busy());
    assert!(
        !waiter.is_finished(),
        "HTTP success must follow final physical cleanup"
    );
    let (status, value) = request(
        &f.state,
        "DELETE",
        "/api/esim/profile",
        json!({
            "client_request_id":"serial-active-delete-conflict",
            "iccid":ICCID,
            "aid":AID
        }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert!(!f.port.card.deleted.load(Ordering::Acquire));
    drop(release);
    let (status, value) = waiter.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(!crate::reader_esim_download::has_active_operation());
    assert!(!f.state.module_radio.sim_channel_busy());
    let (status, value) = request(
        &f.state,
        "DELETE",
        "/api/esim/profile",
        json!({
            "client_request_id":"serial-active-delete-after",
            "iccid":ICCID,
            "aid":AID
        }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    f.finish().await;
}

#[tokio::test]
async fn serial_esim_public_read_write_switch_and_delete_without_go_or_sms_ready() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    assert!(!f.state.go_socket_path.exists());
    let actor = f.state.modem_actor.clone();
    assert!(
        !tokio::task::spawn_blocking(move || actor.serial_sms_observation())
            .await
            .unwrap()
            .unwrap()
            .ready
    );
    let (status, value) = request(&f.state, "GET", "/api/esim", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["profiles"][0]["profiles"][0]["iccid"], ICCID);
    assert_eq!(value["profiles"][0]["profiles"][0]["state"], 0);
    assert_eq!(value["capabilities"]["can_switch"], true);
    let count = f.port.card.commands.lock().unwrap().len();
    assert_eq!(
        request(&f.state, "HEAD", "/api/esim", Value::Null, true)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.port.card.commands.lock().unwrap().len(),
        count,
        "cached HEAD must not submit card work"
    );
    let (status, value) = request(
        &f.state,
        "PATCH",
        "/api/esim/profile",
        json!({
            "client_request_id":"serial-public-rename",
            "iccid":ICCID,
            "aid":AID,
            "name":"Renamed"
        }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(f.port.card.renamed.load(Ordering::Acquire));
    let (status, value) = request(
        &f.state,
        "POST",
        "/api/esim/switch",
        json!({
            "client_request_id":"serial-public-switch",
            "iccid":ICCID,
            "aid":AID
        }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["switch_accepted"], true);
    assert!(f.port.card.enabled.load(Ordering::Acquire));
    tokio::time::timeout(Duration::from_secs(12), async {
        while crate::reader_esim_download::has_active_operation() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (status, value) = request(
        &f.state,
        "DELETE",
        "/api/esim/profile",
        json!({
            "client_request_id":"serial-public-delete",
            "iccid":ICCID,
            "aid":AID
        }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(f.port.card.deleted.load(Ordering::Acquire));
    let (status, value) = request(&f.state, "GET", "/api/esim/operation", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let commands = f.port.card.commands.lock().unwrap().clone();
    assert_eq!(
        commands
            .iter()
            .filter(|c| c.as_str() == format!("AT+CCHO=\"{AID}\""))
            .count(),
        commands
            .iter()
            .filter(|c| c.starts_with("AT+CCHC="))
            .count(),
        "all real logical channels closed"
    );
    f.finish().await;
}

#[tokio::test]
async fn serial_esim_cache_cannot_survive_reopen_and_claim_never_falls_back() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    let (_, value) = request(&f.state, "GET", "/api/esim", Value::Null, true).await;
    assert_eq!(value["profiles"][0]["profiles"][0]["iccid"], ICCID);
    let (key, _) = crate::module_esim_transport::cached_overview(&f.state, false).unwrap();
    f.close().await;
    assert!(!crate::module_esim_transport::snapshot_current(
        &f.state, &key
    ));
    assert!(crate::module_esim_transport::cached_overview(&f.state, true).is_none());
    // A listening Go socket makes any fallback observable even while closed.
    let listener = tokio::net::UnixListener::bind(&f.state.go_socket_path).unwrap();
    f.state.availability.mark_ready();
    let (status, value) = request(&f.state, "GET", "/api/esim", Value::Null, true).await;
    assert!(
        !status.is_success(),
        "closed native route must not produce an old success: {value}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "retired Go LPA was contacted"
    );
    f.open(2).await;
    let (status, value) = request(&f.state, "GET", "/api/esim", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(!crate::module_esim_transport::snapshot_current(
        &f.state, &key
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    drop(listener);
    f.finish().await;
}

#[tokio::test]
async fn cancelled_serial_rename_keeps_radio_until_actual_channel_cleanup() {
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    let (status, value) = request(&f.state, "GET", "/api/esim", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    struct Release(Arc<Card>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.block_close.store(false, Ordering::Release);
        }
    }
    let release = Release(f.port.card.clone());
    f.port.card.block_close.store(true, Ordering::Release);
    let state = f.state.clone();
    let waiter = tokio::spawn(async move {
        request(
            &state,
            "PATCH",
            "/api/esim/profile",
            json!({
                "client_request_id":"serial-cancelled-rename",
                "iccid":ICCID,
                "aid":AID,
                "name":"Renamed"
            }),
            true,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !f.port.card.close_entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    waiter.abort();
    let _ = waiter.await;
    assert!(f.state.module_radio.sim_channel_busy());
    assert!(f.state.module_radio.reserve_esim().await.is_err());
    assert!(
        f.port.card.renamed.load(Ordering::Acquire),
        "the APDU was accepted before cleanup blocked"
    );
    drop(release);
    tokio::time::timeout(Duration::from_secs(8), async {
        while crate::reader_esim_download::has_active_operation()
            || f.state.module_radio.sim_channel_busy()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, value) =
        request(&f.state, "GET", "/api/esim?refresh=true", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["profiles"][0]["profiles"][0]["name"], "Renamed");
    f.finish().await;
}
