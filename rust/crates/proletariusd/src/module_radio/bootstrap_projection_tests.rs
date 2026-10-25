use super::*;
use crate::usb_at_transport::{Transport, TransportError};
use std::sync::Condvar;

#[derive(Default)]
struct Trace {
    fail: AtomicBool,
    block: AtomicBool,
    entered: Notify,
    release: (Mutex<bool>, Condvar),
    calls: AtomicU64,
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
        "native health USB fixture".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        assert_eq!(command, "AT+CFUN?");
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if self.0.block.load(Ordering::SeqCst) {
            self.0.entered.notify_one();
            let released = self.0.release.0.lock().unwrap();
            let (released, wait) = self
                .0
                .release
                .1
                .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                .unwrap();
            assert!(
                !wait.timed_out() && *released,
                "bootstrap fixture gate timed out"
            );
        }
        Ok(if self.0.fail.load(Ordering::SeqCst) {
            "ERROR\r\n"
        } else {
            "+CFUN: 1\r\nOK\r\n"
        }
        .into())
    }
}
struct Fixture {
    root: PathBuf,
    state: crate::ServerState,
    trace: Arc<Trace>,
}
impl Fixture {
    async fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime")
            .join(format!("health502-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let trace = Arc::new(Trace::default());
        let physical = trace.clone();
        let (usb, session) = crate::usb_at::Owner::native_module_fixture(Arc::new(move || {
            Ok(Box::new(Port(physical.clone())))
        }))
        .await;
        let radio = Service::native_vowifi_fixture(usb.clone(), session, root.clone()).await;
        let mut state = crate::reader_pcsc_gateway_tests::state(&root);
        state.usb_at = usb;
        state.module_radio = radio;
        state.app_token = Some("app-fixture".into());
        Self { root, state, trace }
    }
    async fn wait_until_entered(&self) {
        tokio::time::timeout(Duration::from_secs(3), self.trace.entered.notified())
            .await
            .expect("bootstrap fixture must enter its bounded command gate");
    }
    async fn bootstrap(&self) -> Result<Value, Failure> {
        self.state
            .module_radio
            .submit(Job::Bootstrap(Producer {
                producer_id: "native-module-test".into(),
            }))
            .await
    }
    async fn health(&self) -> (StatusCode, Value) {
        use http_body_util::BodyExt;
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let state = self.state.clone();
        let serving = tokio::spawn(async move { crate::serve_connection(server, state).await });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            sender.send_request(
                Request::builder()
                    .method("GET")
                    .uri("/api/health")
                    .header(crate::APP_TOKEN_HEADER, "app-fixture")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let status = response.status();
        let value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        drop(sender);
        for result in [
            connection.await.unwrap().map_err(|e| e.to_string()),
            serving.await.unwrap().map_err(|e| e.to_string()),
        ] {
            if let Err(error) = result {
                assert!(crate::benign_connection_error_message(&error), "{error}");
            }
        }
        (status, value)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        *self
            .trace
            .release
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.trace.release.1.notify_all();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn public_health_requires_actual_usb_bootstrap_but_not_go_ready() {
    let f = Fixture::new().await;
    assert_ne!(
        f.state.availability.snapshot().phase,
        crate::BackendPhase::Ready
    );
    assert!(!f.state.go_socket_path.exists());
    assert_eq!(f.state.module_radio.configured_usb_transport(), Some(true));
    let (code, health) = f.health().await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(health["ok"], true);
    assert_eq!(health["esim_available"], false);
    assert_eq!(health["port"], "native health USB fixture");
    assert_eq!(health["usb_device"]["interfaces"], Value::Null);
    assert_eq!(
        f.trace.calls.load(Ordering::SeqCst),
        0,
        "health must not issue AT"
    );
    f.trace.fail.store(true, Ordering::SeqCst);
    assert!(f.bootstrap().await.is_err());
    assert_eq!(f.health().await.1["esim_available"], false);
    f.trace.fail.store(false, Ordering::SeqCst);
    f.bootstrap().await.unwrap();
    assert_eq!(f.health().await.1["esim_available"], true);
    let calls = f.trace.calls.load(Ordering::SeqCst);
    assert_eq!(f.health().await.1["esim_available"], true);
    assert_eq!(f.trace.calls.load(Ordering::SeqCst), calls);
    f.state
        .usb_at
        .replace_producer("health-replacement".into())
        .await
        .unwrap();
    let (_, health) = f.health().await;
    assert_eq!(health["esim_available"], false);
    assert_eq!(health["usb_device"], Value::Null);
    assert!(!health["discovery_error"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_bootstrap_waiter_does_not_publish_before_actual_completion() {
    let f = Fixture::new().await;
    f.trace.block.store(true, Ordering::SeqCst);
    let service = f.state.module_radio.clone();
    let waiter = tokio::spawn(async move {
        service
            .submit(Job::Bootstrap(Producer {
                producer_id: "native-module-test".into(),
            }))
            .await
    });
    f.wait_until_entered().await;
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(f.health().await.1["esim_available"], false);
    *f.trace.release.0.lock().unwrap() = true;
    f.trace.release.1.notify_all();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f.state.module_radio.usb_esim_route_available() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.health().await.1["esim_available"], true);
    f.state
        .module_radio
        .retire_producer("health-new-producer".into())
        .await
        .unwrap();
    assert!(!f.state.module_radio.usb_esim_route_available());
    assert_eq!(f.state.module_radio.configured_usb_transport(), None);
}
