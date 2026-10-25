use super::*;

impl Fixture {
    async fn configure(&self, sequence: u64, transport: &str, session: &str) {
        let body = serde_json::to_vec(&json!({"producer_id":"native-module-test",
            "config_seq":sequence,"transport":transport,"session_id":session}))
        .unwrap();
        let (code, value) = crate::reader_pcsc_gateway_tests::request_with_body(
            self.state.clone(),
            "/api/internal/module-radio/configure",
            false,
            false,
            Some(("X-Switch-Internal-Token", "internal-fixture")),
            &body,
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{value}");
        assert_eq!(value["configured"], true);
    }
}

#[tokio::test]
async fn unconfigured_health_and_status_are_native_and_ignore_a_live_retired_peer() {
    let f = Fixture::new().await;
    let session = f.state.module_radio.usb_transport_session().unwrap();
    f.configure(2, "none", "").await;
    let listener = tokio::net::UnixListener::bind(&f.state.go_socket_path).unwrap();
    for path in ["/api/status", "/api/health"] {
        let (code, value) = f.get(path).await;
        assert_eq!(code, StatusCode::OK, "{path}: {value}");
        if path == "/api/status" {
            assert_eq!(value["operator"], "未连接");
            assert_eq!(value["sim_inserted"], false);
            assert!(value.get("iccid").is_none());
        } else {
            assert_eq!(value["ok"], true);
            assert_eq!(value["esim_available"], false);
            assert_eq!(value["port"], "未发现 AT 串口");
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    f.configure(3, "usb", &session).await;
    let (code, value) = f.get("/api/status").await;
    assert_eq!(code, StatusCode::OK, "{value}");
    assert_eq!(value["iccid"], "8988212345678901234");
}

#[tokio::test]
async fn descriptor_finishing_after_actual_transport_deselect_cannot_publish_old_usb_status() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new().await;
    f.state
        .usb_at
        .replace_producer("native-module-test".into())
        .await
        .unwrap();
    let program = f.root.join("blocked-ioreg-fixture");
    std::fs::write(&program, "#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\nprintf 'entered' > entered\nwhile [ ! -f release ]; do /bin/sleep 0.01; done\n/bin/cat discovery-output\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        f.root.join("discovery-output"),
        "  \"idVendor\" = 11427\n  \"USB Product Name\" = \"OLD USB\"\n",
    )
    .unwrap();
    f.state.usb_at = f.state.usb_at.clone().with_hardware_observer(
        crate::module_discovery::Observer::with_program_for_tests(program),
    );
    let f = Arc::new(f);
    let request_fixture = f.clone();
    let request = tokio::spawn(async move { request_fixture.get("/api/status").await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f.root.join("entered").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.configure(2, "none", "").await;
    assert!(!crate::module_discovery::uses_native_usb(
        &f.state.module_radio,
        &f.state.usb_at
    ));
    std::fs::write(f.root.join("release"), "").unwrap();
    let (code, value) = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert!(!code.is_success(), "{value}");
    assert!(value.get("usb_device").is_none(), "{value}");
    assert!(value.get("iccid").is_none(), "{value}");
}
