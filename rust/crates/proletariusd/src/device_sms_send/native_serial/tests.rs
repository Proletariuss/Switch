use super::*;
use serialport::SerialPort as _;
use std::{
    io::{Read, Write},
    sync::{Mutex, atomic::AtomicUsize},
};

struct Port {
    path: String,
    stop: Arc<AtomicBool>,
    sends: Arc<AtomicUsize>,
    commands: Arc<Mutex<Vec<String>>>,
    inject: std::sync::mpsc::Sender<Vec<u8>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Port {
    fn new() -> Self {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let path = slave.name().unwrap();
        drop(slave);
        let stop = Arc::new(AtomicBool::new(false));
        let sends = Arc::new(AtomicUsize::new(0));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (inject, receive) = std::sync::mpsc::channel::<Vec<u8>>();
        let (stopped, sent, trace) = (stop.clone(), sends.clone(), commands.clone());
        let worker = std::thread::spawn(move || {
            let mut line = Vec::new();
            let mut payload = false;
            while !stopped.load(Ordering::Acquire) {
                while let Ok(bytes) = receive.try_recv() {
                    master.write_all(&bytes).unwrap();
                }
                let mut byte = [0];
                match master.read(&mut byte) {
                    Ok(1) => {}
                    _ => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
                if payload {
                    if byte[0] == 0x1a {
                        payload = false;
                        sent.fetch_add(1, Ordering::SeqCst);
                        master.write_all(b"\r\n+CMGS: 17\r\nOK\r\n").unwrap();
                    }
                    continue;
                }
                if byte[0] != b'\n' {
                    line.push(byte[0]);
                    continue;
                }
                let command = String::from_utf8(std::mem::take(&mut line))
                    .unwrap()
                    .trim()
                    .to_owned();
                if command.is_empty() {
                    continue;
                }
                trace.lock().unwrap().push(command.clone());
                let response = match command.as_str() {
                    "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
                    "AT+CNUM" => "+CNUM: \"\",\"+447700900123\",145\r\nOK\r\n",
                    "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n",
                    c if c.starts_with("AT+CMGS=") => {
                        payload = true;
                        "> "
                    }
                    _ => "OK\r\n",
                };
                master.write_all(response.as_bytes()).unwrap();
            }
        });
        Self {
            path,
            stop,
            sends,
            commands,
            inject,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.sends.load(Ordering::SeqCst)
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
pub(in crate::device_sms_send) struct Fixture {
    root: PathBuf,
    pub(in crate::device_sms_send) state: crate::ServerState,
    port: Port,
}
impl Fixture {
    pub(in crate::device_sms_send) async fn new(initialize: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("s506-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let port = Port::new();
        let expected_path = port.path.clone();
        let mut state = crate::reader_pcsc_gateway_tests::state(&root);
        // macOS IOSSIOSPEED is a hardware-serial ioctl and rejects PTYs with
        // ENOTTY. Only the test factory skips that ioctl; HTTP validation,
        // actual PTY open/close, actor sessions and all byte I/O stay real.
        state.modem_actor =
            crate::modem_actor::ModemActor::with_test_port_opener(move |path, baud| {
                assert_eq!(
                    path, expected_path,
                    "fixture must never open a hardware device"
                );
                assert_eq!(baud, 115_200, "public open keeps the real baud contract");
                Ok(serialport::new(path, 0)
                    .timeout(Duration::from_millis(20))
                    .open()
                    .expect("open the fixture PTY without a hardware baud ioctl"))
            })
            .unwrap();
        state.usb_at = crate::usb_at::Owner::unavailable_for_tests("serial-fixture".into());
        state.module_radio = crate::module_radio::Service::new(
            root.clone(),
            state.usb_at.clone(),
            state.modem_actor.clone(),
            state.availability.clone(),
            "serial-fixture".into(),
        );
        let f = Self { root, state, port };
        f.open().await;
        f.configure(1).await;
        if initialize {
            f.initialize().await;
        }
        f
    }
    async fn internal(&self, path: &str, body: Value) -> (StatusCode, Value) {
        request(&self.state, "POST", path, body, false).await
    }
    async fn open(&self) {
        let (status, value) = self
            .internal(
                "/api/internal/modem/open",
                json!({
                    "port":self.port.path,"baud_rate":115200,"initialization_owned":true
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    async fn configure(&self, sequence: u64) {
        let (status, value) = self.internal("/api/internal/module-radio/configure", json!({
            "producer_id":"serial-fixture","config_seq":sequence,"transport":"serial","session_id":""
        })).await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    async fn initialize(&self) {
        let (status, value) = self
            .internal("/api/internal/modem/initialize", json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["sms_pdu_ready"], true);
        self.port.inject.send(b"+CPIN: READY\r\n".to_vec()).unwrap();
        let actor = self.state.modem_actor.clone();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let actor = actor.clone();
                if tokio::task::spawn_blocking(move || actor.capture_serial_sms_binding())
                    .await
                    .unwrap()
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn reopen(&self) {
        self.close().await;
        self.open().await;
        self.initialize().await;
    }
    async fn prepare(&self, id: &str, device: &str) -> Value {
        let (status, value) = request(
            &self.state,
            "POST",
            "/api/sms/send-intents",
            body(id, device),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["can_submit"], true, "{value}");
        self.record(id)
    }
    fn record(&self, id: &str) -> Value {
        self.state
            .device_send_store
            .apply("get", json!({"client_request_id":id}))
            .unwrap()
    }
    pub(in crate::device_sms_send) async fn close(&self) {
        // The radio may intentionally retain the previous session in rebind
        // tests. Close the exact physical actor session, never that old binding.
        let actor = self.state.modem_actor.clone();
        let serial_session = tokio::task::spawn_blocking(move || actor.serial_sms_observation())
            .await
            .unwrap()
            .unwrap()
            .serial_session
            .expect("fixture actor is open");
        let (status, value) = self
            .internal(
                "/api/internal/modem/close",
                json!({"serial_session":serial_session}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn body(id: &str, device: &str) -> Value {
    json!({"client_request_id":id,"phone":"+15550102020","message":"serial fixture","device":device})
}
async fn request(
    state: &crate::ServerState,
    method: &str,
    path: &str,
    body: Value,
    public: bool,
) -> (StatusCode, Value) {
    use hyper_util::rt::TokioIo;
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
                .method(method)
                .uri(path)
                .header(crate::APP_TOKEN_HEADER, "app-fixture")
                .header("X-Switch-Internal-Token", "internal-fixture")
                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap(),
        )
        .await
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
fn compatibility_facts() -> Value {
    json!({"module": {"modem_present":true,"usb_at_present":false,"usb_device_present":false,
        "session_active":false,"session_info_present":false,"ims_registered":false,"messaging_ready":false,
        "smsc":"","target":"","phone":"old Go phone","session_phone":"","subscription_id":""},
        "reader": {"ready":true}})
}

#[tokio::test]
async fn real_serial_public_http_without_go_prepares_sends_once_and_projects_actor_facts() {
    let f = Fixture::new(true).await;
    assert!(!f.state.go_socket_path.exists());
    assert_ne!(
        f.state.availability.snapshot().phase,
        crate::BackendPhase::Ready
    );
    let first = f.prepare("auto", "").await;
    let exact = serial_session_binding(first["binding"].as_str().unwrap()).unwrap();
    assert_eq!(
        Some(exact.serial_session),
        f.state.module_radio.serial_transport_session()
    );
    let (status, sent) = request(&f.state, "POST", "/api/sms/send", body("auto", ""), true).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent["sent"], true);
    assert_eq!(f.port.count(), 1);
    let (status, _) = request(&f.state, "POST", "/api/sms/send", body("auto", ""), true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(f.port.count(), 1);
    let listener = tokio::net::UnixListener::bind(&f.state.go_socket_path).unwrap();
    f.state.availability.mark_ready();
    let _ = f.prepare("explicit", "dji").await;
    let (status, sent) = request(
        &f.state,
        "POST",
        "/api/sms/send",
        body("explicit", "dji"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(f.port.count(), 2);
    let mut wrong_subscription = body("mismatch", "dji");
    wrong_subscription["expected_subscription_id"] = json!("wrong-subscription");
    let (status, value) = request(
        &f.state,
        "POST",
        "/api/sms/send-intents",
        wrong_subscription,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["code"], "sms_intent_conflict");
    let (status, _) = f
        .internal("/api/internal/modem/typed-query", json!({"query":"msisdn"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let commands = f.port.commands.lock().unwrap().len();
    let (status, devices) = request(&f.state, "GET", "/api/sms/devices", json!({}), true).await;
    assert_eq!(status, StatusCode::OK, "{devices}");
    assert_eq!(devices["devices"][0]["ready"], true);
    assert_eq!(devices["devices"][0]["phone"], "+447700900123");
    assert_eq!(devices["devices"][1]["ready"], false);
    let (status, selected) = f
        .internal(
            crate::sms_device_service::SELECT_PATH,
            json!({"requested":"","facts":compatibility_facts()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(selected["selected"], "dji");
    assert_eq!(
        f.port.commands.lock().unwrap().len(),
        commands,
        "facts/selection cannot issue AT"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    f.port.inject.send(b"+QSIMSTAT: 1,0\r\n".to_vec()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let actor = f.state.modem_actor.clone();
            if tokio::task::spawn_blocking(move || actor.serial_sms_observation())
                .await
                .unwrap()
                .unwrap()
                .sim_present
                == Some(false)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (_, devices) = request(&f.state, "GET", "/api/sms/devices", json!({}), true).await;
    assert_eq!(devices["devices"][0]["ready"], false);
    assert!(
        devices["devices"][0].get("phone").is_none(),
        "old SIM phone must be revoked"
    );
    f.close().await;
}

#[tokio::test]
async fn configured_serial_unknown_and_uninitialized_never_fall_back_to_live_go() {
    let f = Fixture::new(false).await;
    let listener = tokio::net::UnixListener::bind(&f.state.go_socket_path).unwrap();
    f.state.availability.mark_ready();
    for device in ["", "dji"] {
        let (status, value) = request(
            &f.state,
            "POST",
            "/api/sms/send-intents",
            body("blocked", device),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{value}");
        assert_eq!(
            value["code"],
            if device.is_empty() {
                "sms_device_selection_unavailable"
            } else {
                "sms_device_not_ready"
            }
        );
    }
    let (status, devices) = request(&f.state, "GET", "/api/sms/devices", json!({}), true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(devices["devices"][0]["available"], true);
    assert_eq!(devices["devices"][0]["ready"], false);
    assert!(devices["devices"][0].get("phone").is_none());
    let (_, selected) = f
        .internal(
            crate::sms_device_service::SELECT_PATH,
            json!({"requested":"","facts":compatibility_facts()}),
        )
        .await;
    assert_eq!(
        selected["error"]["code"],
        "sms_device_selection_unavailable"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "Go facts/capture must not be requested"
    );
    assert_eq!(f.port.count(), 0);
    f.close().await;
}

#[tokio::test]
async fn serial_reopen_same_sim_generation_expires_intent_until_radio_rebind() {
    let f = Fixture::new(true).await;
    let old = f.prepare("old", "dji").await;
    let first = serial_session_binding(old["binding"].as_str().unwrap()).unwrap();
    f.reopen().await;
    let actor = f.state.modem_actor.clone();
    let second = tokio::task::spawn_blocking(move || actor.capture_serial_sms_binding())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.sim_generation, second.sim_generation);
    assert_ne!(first.serial_session, second.serial_session);
    let (status, value) =
        request(&f.state, "POST", "/api/sms/send", body("old", "dji"), true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["code"], "sms_intent_binding_expired");
    let (status, _) = request(
        &f.state,
        "POST",
        "/api/sms/send-intents",
        body("fresh", "dji"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(f.port.count(), 0);
    let (_, devices) = request(&f.state, "GET", "/api/sms/devices", json!({}), true).await;
    assert_eq!(devices["devices"][0]["ready"], false);
    assert_eq!(devices["devices"][0]["detail"], "DJI 模块状态暂不可读取");
    f.configure(2).await;
    let fresh = f.prepare("fresh", "dji").await;
    assert_eq!(
        serial_session_binding(fresh["binding"].as_str().unwrap()).unwrap(),
        second
    );
    let (status, value) = request(
        &f.state,
        "POST",
        "/api/sms/send",
        body("fresh", "dji"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(f.port.count(), 1);
    f.close().await;
}

#[tokio::test]
async fn waiting_serial_send_rechecks_capability_after_reservation_releases() {
    let f = Fixture::new(true).await;
    f.prepare("waiting", "dji").await;
    let reservation = f.state.module_radio.reserve_esim().await.unwrap();
    let state = f.state.clone();
    let send = tokio::spawn(async move {
        request(
            &state,
            "POST",
            "/api/sms/send",
            body("waiting", "dji"),
            true,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.record("waiting")["state"] != "submitting" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.reopen().await;
    f.state
        .module_radio
        .release_esim(reservation)
        .await
        .unwrap();
    let (status, value) = send.await.unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
    assert_eq!(value["code"], "sms_send_outcome_unknown");
    assert_eq!(f.record("waiting")["state"], "unknown");
    assert_eq!(f.port.count(), 0);
    let (status, _) = request(
        &f.state,
        "POST",
        "/api/sms/send",
        body("waiting", "dji"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(f.port.count(), 0);
    assert!(!f.state.module_radio.sim_channel_busy());
    f.close().await;
}

#[tokio::test]
async fn old_generation_only_durable_intent_requires_new_prepare_without_go() {
    let f = Fixture::new(true).await;
    for (id, binding) in [
        ("legacy", "rust_modem_sms_generation:2"),
        ("malformed", "rust_modem_session_v2:bad:0:2"),
    ] {
        let mut record = body(id, "dji");
        record["binding"] = json!(binding);
        record["subscription_id"] = json!("");
        f.state.device_send_store.apply("prepare", record).unwrap();
        let (status, projected) = request(
            &f.state,
            "GET",
            &format!("/api/sms/send-intents?client_request_id={id}"),
            json!({}),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{projected}");
        assert_eq!(projected["can_submit"], false);
        let (status, value) =
            request(&f.state, "POST", "/api/sms/send", body(id, "dji"), true).await;
        assert_eq!(status, StatusCode::CONFLICT, "{value}");
        assert_eq!(value["code"], "sms_intent_binding_expired");
        assert_eq!(f.record(id)["state"], "prepared");
        assert_eq!(f.port.count(), 0);
    }
    f.prepare("new-intent", "dji").await;
    f.close().await;
}
