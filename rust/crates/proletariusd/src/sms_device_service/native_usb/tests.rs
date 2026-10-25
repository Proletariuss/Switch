use super::*;
mod transport_switch;
use crate::usb_at_transport::{Transport, TransportError};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
struct Trace {
    commands: Mutex<Vec<String>>,
    no_sim: AtomicBool,
}
struct Port(Arc<Trace>);
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
        "USB SMS facts fixture".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        Ok(match command {
            "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n",
            "AT+CPIN?" if self.0.no_sim.load(Ordering::SeqCst) => "+CME ERROR: 10\r\n",
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
            "AT+QCCID" => "+QCCID: 8988212345678901234\r\nOK\r\n",
            "AT+CIMI" => "234150123456789\r\nOK\r\n",
            "AT+CNUM" => "ERROR\r\n",
            _ => "OK\r\n",
        }
        .into())
    }
}
struct Fixture {
    root: std::path::PathBuf,
    state: crate::ServerState,
    trace: Arc<Trace>,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("sms502-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let trace = Arc::new(Trace::default());
        let physical = trace.clone();
        let (owner, session) = crate::usb_at::Owner::native_module_fixture(Arc::new(move || {
            Ok(Box::new(Port(physical.clone())))
        }))
        .await;
        let mut state = crate::reader_pcsc_gateway_tests::state(&root);
        let notes = crate::profile_notes::Store::new(state.multistandby_store.data_root());
        notes
            .remember_registered_phone("8988212345678901234", "+441234567890", || true)
            .unwrap();
        state.usb_at = owner.with_profile_notes(notes);
        state.module_radio = crate::module_radio::Service::native_vowifi_fixture(
            state.usb_at.clone(),
            session,
            root.clone(),
        )
        .await;
        assert_eq!(
            state.module_radio.native_vowifi_idle_mode().await.unwrap(),
            1
        );
        Self { root, state, trace }
    }
    fn count(&self) -> usize {
        self.trace.commands.lock().unwrap().len()
    }
    async fn select(&self, stale_facts: DeviceFacts) -> Value {
        let body = serde_json::to_vec(&json!({"requested":"dji", "facts": stale_facts})).unwrap();
        let (code, result) = crate::reader_pcsc_gateway_tests::request_with_body(
            self.state.clone(),
            SELECT_PATH,
            false,
            false,
            Some(("X-Switch-Internal-Token", "internal-fixture")),
            &body,
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{result}");
        result
    }
    async fn get(&self, path: &str) -> (StatusCode, Value) {
        use hyper_util::rt::TokioIo;
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let state = self.state.clone();
        let serving = tokio::spawn(async move {
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
                    .uri(path)
                    .header(crate::APP_TOKEN_HEADER, "app-fixture")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn main_http_usb_devices_uses_exact_status_cache_without_go_or_extra_at() {
    let f = Fixture::new().await;
    assert!(!f.state.go_socket_path.exists());
    assert_ne!(
        f.state.availability.snapshot().phase,
        crate::availability::BackendPhase::Ready
    );
    let initial = f.count();
    let (code, empty) = f.get("/api/sms/devices").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(empty["devices"][0]["ready"], true);
    assert!(empty["devices"][0].get("phone").is_none());
    assert!(empty["devices"][0].get("subscription_id").is_none());
    assert_eq!(f.count(), initial);
    assert_eq!(f.select(DeviceFacts::default()).await["selected"], "dji");
    assert_eq!(f.count(), initial);
    let (code, status) = f.get("/api/status").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["iccid"], "8988212345678901234");
    assert_eq!(status["phone_number"], "+441234567890");
    assert!(f.count() > initial);
    let observed = f.count();
    for _ in 0..3 {
        let (code, devices) = f.get("/api/sms/devices").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(devices["devices"][0]["phone"], "+441234567890");
        assert_eq!(devices["devices"][0]["name"], "+441234567890_DJI");
        assert!(devices["devices"][0].get("subscription_id").is_none());
        assert_eq!(f.count(), observed);
    }
    let facts = capture(&f.state).await.unwrap();
    let devices = project(&DeviceFacts {
        module: facts,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(select(&devices.devices, "").unwrap(), "dji");
    f.state.usb_at.set_cellular_suppressed(true);
    let (_, invalidated) = f.get("/api/sms/devices").await;
    assert!(invalidated["devices"][0].get("phone").is_none());
    assert_eq!(f.count(), observed);
    f.state
        .usb_at
        .replace_producer("replacement-producer".into())
        .await
        .unwrap();
    let (_, retired) = f.get("/api/sms/devices").await;
    assert_eq!(retired["devices"][0]["available"], false);
    assert_eq!(retired["devices"][0]["ready"], false);
    assert!(retired["devices"][0].get("phone").is_none());
    assert!(retired["devices"][0].get("subscription_id").is_none());
    let stale = DeviceFacts {
        module: ModuleFacts {
            modem_present: true,
            usb_at_present: true,
            phone: "old phone".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let denied = f.select(stale).await;
    assert!(denied.get("selected").is_none());
    assert_eq!(denied["error"]["code"], "sms_device_not_ready");
}

#[tokio::test]
async fn main_http_no_sim_does_not_publish_remembered_phone_or_subscription() {
    let f = Fixture::new().await;
    let _ = f.get("/api/status").await;
    f.trace.no_sim.store(true, Ordering::SeqCst);
    let (code, status) = f.get("/api/status").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["sim_inserted"], false);
    let observed = f.count();
    let (code, devices) = f.get("/api/sms/devices").await;
    assert_eq!(code, StatusCode::OK);
    assert!(devices["devices"][0].get("phone").is_none());
    assert!(devices["devices"][0].get("subscription_id").is_none());
    assert_eq!(f.count(), observed);
}

#[tokio::test]
async fn main_http_closed_usb_descriptor_is_available_but_never_sms_ready() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new().await;
    f.state
        .usb_at
        .replace_producer("closed-descriptor".into())
        .await
        .unwrap();
    let program = f.root.join("ioreg-fixture");
    std::fs::write(&program, "#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\nprintf 'called\\n' >> invocations\n/bin/cat discovery-output\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(f.root.join("discovery-output"),
        "  \"idVendor\" = 11427\n  \"idProduct\" = 16390\n  \"USB Product Name\" = \"DJI SMS discovery fixture\"\n  \"bInterfaceNumber\" = 2\n  \"bInterfaceClass\" = 255\n").unwrap();
    f.state.usb_at = f.state.usb_at.clone().with_hardware_observer(
        crate::module_discovery::Observer::with_program_for_tests(program),
    );
    let before = f.count();
    for _ in 0..2 {
        let (code, devices) = f.get("/api/sms/devices").await;
        assert_eq!(code, StatusCode::OK, "{devices}");
        assert_eq!(devices["devices"][0]["available"], true);
        assert_eq!(devices["devices"][0]["ready"], false);
        assert_eq!(devices["devices"][0]["detail"], "DJI 模块短信通道正在连接");
        assert!(devices["devices"][0].get("phone").is_none());
        assert!(devices["devices"][0].get("subscription_id").is_none());
    }
    let selection = f
        .select(DeviceFacts {
            module: ModuleFacts {
                modem_present: true,
                usb_at_present: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
    assert_eq!(selection["error"]["code"], "sms_device_not_ready");
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations")).unwrap(),
        "called\n"
    );
    assert_eq!(
        f.count(),
        before,
        "descriptor discovery must not open USB or send AT"
    );
}

#[tokio::test]
async fn native_usb_module_keeps_passive_reader_local_without_ims_or_go() {
    for reader in ["Reader A", "BLE:SMS fixture"] {
        let f = Fixture::new().await;
        crate::sms_device_service::reader_authority_tests::publish_reader(
            &f.state,
            reader,
            "8988212345678901235",
            "+447700900444",
        );
        assert!(
            f.state
                .native_vowifi_lifecycle
                .reader_observation()
                .is_none()
        );
        let count = f.count();
        let (code, value) = f.get("/api/sms/devices").await;
        assert_eq!(code, StatusCode::OK, "{value}");
        assert_eq!(value["devices"][0]["ready"], true);
        assert_eq!(value["devices"][1]["available"], true);
        assert_eq!(value["devices"][1]["ready"], false);
        assert_eq!(value["devices"][1]["phone"], "+447700900444");
        assert!(value["devices"][1].get("subscription_id").is_none());
        assert_eq!(
            f.count(),
            count,
            "facts must not issue physical AT commands"
        );
        assert!(!f.state.go_socket_path.exists());
    }
}
