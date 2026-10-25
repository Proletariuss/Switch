//! No device enumeration: real HTTP dispatch over anonymous socket pairs,
//! synthetic stores, and only projection/admission routes.
use super::*;
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;

fn fixture() -> (PathBuf, ServerState) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/p556-http")
        .join(snapshot::new_instance_id());
    fs::create_dir_all(&root).unwrap();
    let mut state = reader_pcsc_gateway_tests::state(&root);
    state.availability =
        BackendAvailability::with_module_recovery(module_recovery::Gate::quarantined_for_tests());
    state.availability.mark_ready();
    (root, state)
}

async fn request(
    state: ServerState,
    method: &str,
    path: &str,
    token: bool,
    private: bool,
    stopping: bool,
) -> (StatusCode, serde_json::Value, usize) {
    request_with_body(
        state,
        method,
        path,
        token,
        private,
        stopping,
        Bytes::from_static(b"{}"),
    )
    .await
}

async fn request_with_body(
    state: ServerState,
    method: &str,
    path: &str,
    token: bool,
    private: bool,
    stopping: bool,
    body: Bytes,
) -> (StatusCode, serde_json::Value, usize) {
    let (client, server) = UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        if stopping {
            serve_internal_shutdown_connection(server, state).await;
        } else if private {
            let _ = serve_internal_connection(server, state).await;
        } else {
            let _ = serve_connection(server, state).await;
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let mut request = Request::builder().method(method).uri(path);
    if token {
        request = request.header(
            if private {
                multistandby_store::INTERNAL_HEADER
            } else {
                "X-Proletarius-App-Token"
            },
            if private {
                "internal-fixture"
            } else {
                "app-fixture"
            },
        );
    }
    let response = sender
        .send_request(request.body(Full::new(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    let _ = connection.await;
    server.await.unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        body.len(),
    )
}

#[tokio::test]
async fn quarantine_sms_adopt_post_leaves_disk_and_live_preference_unchanged() {
    let (root, state) = fixture();
    let data = state.multistandby_store.data_root();
    fs::create_dir_all(data).unwrap();
    let path = data.join("sms-adopt.json");
    fs::write(&path, b"{\"enabled\":false}").unwrap();
    let before = fs::metadata(&path).unwrap();
    assert!(!state.usb_at.adoption_enabled().await.unwrap());
    let (status, body, _) = request_with_body(
        state.clone(),
        "POST",
        "/api/sms/adopt",
        true,
        false,
        false,
        Bytes::from_static(b"{\"enabled\":true}"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], module_recovery::CODE);
    assert!(!state.usb_at.adoption_enabled().await.unwrap());
    assert_eq!(fs::read(&path).unwrap(), b"{\"enabled\":false}");
    assert!(!data.join("sms-adopt.json.lock").exists());
    use std::os::unix::fs::MetadataExt;
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.ctime()),
        (after.ino(), after.mtime(), after.ctime())
    );
    assert_eq!(
        request(state.clone(), "GET", "/api/sms/adopt", true, false, false)
            .await
            .0,
        StatusCode::OK
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn quarantine_keeps_health_and_runtime_healthy_and_projects_module_unavailable() {
    let (root, state) = fixture();
    let (status, health, _) =
        request(state.clone(), "GET", "/api/health", true, false, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["ok"], true);
    assert_eq!(health["esim_available"], false);
    assert_eq!(health["discovery_error"], module_recovery::CODE);
    let (status, runtime, _) =
        request(state.clone(), "GET", "/api/v2/runtime", true, false, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime["ok"], true);
    assert_eq!(runtime["module_recovery"]["state"], module_recovery::CODE);
    assert_eq!(
        runtime["module_recovery"]["historical_recovery_confirmed"],
        false
    );
    let projected = device_status_projection::capture_typed(&state)
        .await
        .unwrap();
    assert_eq!(projected.sim_inserted, Some(false));
    assert!(state.module_call.get().is_none());
    assert!(state.call_mode.get().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn quarantine_getters_do_not_construct_adb_owner_or_write_cached_voice_evidence() {
    let (root, state) = fixture();
    let data = state.multistandby_store.data_root();
    fs::create_dir_all(data).unwrap();
    fs::write(data.join("voice-enabled.json"), b"{\"enabled\":true}").unwrap();
    for path in [
        "/api/call-mode/status",
        "/api/voice/enabled",
        "/api/sms/devices",
    ] {
        let (status, body, _) = request(state.clone(), "GET", path, true, false, false).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        if path == "/api/call-mode/status" {
            assert_eq!(body["state"], module_recovery::CODE);
            assert!(call_mode_status::valid_public_status(&body));
        }
        if path == "/api/voice/enabled" {
            assert_eq!(body["enabled"], true);
            assert_eq!(body["module_available"], false);
        }
        if path == "/api/sms/devices" {
            assert_eq!(body["devices"][0]["available"], false);
            assert_eq!(body["devices"][1]["id"], "reader");
        }
    }
    assert!(!data.join("voice-enabled.json.lock").exists());
    assert!(!data.join("call-mode-operation.lock").exists());
    assert!(!data.join(".module-adb-interface.lock").exists());
    assert!(!data.join(".call-mode-pristine-v1").exists());
    assert!(state.call_mode.get().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn quarantine_retains_auth_method_head_and_module_mutation_contracts() {
    let (root, state) = fixture();
    assert_eq!(
        request(state.clone(), "POST", "/api/at", false, false, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(state.clone(), "POST", "/api/health", true, false, false)
            .await
            .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    let (status, _, bytes) =
        request(state.clone(), "HEAD", "/api/health", true, false, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, 0);
    for path in [
        "/api/at",
        "/api/call-mode/enable",
        "/api/call-mode/backups/import",
        "/api/sms/refresh",
        "/api/vowifi",
    ] {
        let (status, body, _) = request(state.clone(), "POST", path, true, false, false).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {body}");
        assert_eq!(body["code"], module_recovery::CODE);
        assert_eq!(body["submitted"], false);
    }
    assert!(state.call_mode.get().is_none());
    assert!(state.module_call.get().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn quarantine_fences_both_private_entrypoints_without_stealing_pure_projection() {
    let (root, state) = fixture();
    for stopping in [false, true] {
        let path = "/api/internal/usb-at/open";
        assert_eq!(
            request(state.clone(), "POST", path, false, true, stopping)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body, _) = request(state.clone(), "POST", path, true, true, stopping).await;
        assert_eq!(status, StatusCode::CONFLICT, "stopping={stopping}: {body}");
        assert_eq!(body["code"], module_recovery::CODE);
    }
    assert!(managed_module_rpc_rejection(modem_actor::PROJECT_CALL_STATE_PATH, &state).is_none());
    assert!(managed_module_rpc_rejection(reader_reservation::LIFECYCLE_PATH, &state).is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn quarantine_module_route_classification_preserves_shared_reader_and_archive_paths() {
    for path in [
        "/api/reader/vowifi",
        "/api/reader/esim/download",
        "/api/multistandby/start",
        "/api/sms/send",
        "/api/sms/send-intents",
        "/api/v2/sms-archive/delete",
        "/api/v2/call/dtmf",
        "/api/esim/notes",
    ] {
        assert!(!module_recovery::owns_public(&Method::POST, path), "{path}");
    }
    for path in ["/api/call-mode/backups", "/api/call-mode/backups/export"] {
        assert!(!module_recovery::owns_public(&Method::GET, path));
    }
}

#[tokio::test]
async fn quarantine_direct_adb_probe_and_runtime_readiness_stop_before_worker_or_evidence() {
    let (root, state) = fixture();
    let nonexistent = root.join("must-not-be-created");
    let gate = state.availability.module_recovery();
    let error =
        call_mode_adb::probe_module_adb(gate, &nonexistent, "synthetic", state.sms_inbox.clone())
            .await
            .unwrap_err();
    assert_eq!(error.code, module_recovery::CODE);
    assert!(
        !call_mode_adb::is_module_voice_runtime_prepared(
            gate,
            &nonexistent,
            "synthetic",
            state.sms_inbox.clone()
        )
        .await
    );
    assert!(!nonexistent.exists());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn legacy_stopping_voice_status_is_sealed_on_both_private_http_entries() {
    let f = crate::module_radio::tests::tests::Fixture::new();
    let mut state = reader_pcsc_gateway_tests::state(&f.root);
    state.availability.mark_ready();
    state.module_radio = f.service.clone();
    state.usb_at = crate::usb_at::Owner::unavailable_for_tests("producer-1".into());
    state.sms_inbox = crate::message_archive::InboxState::for_go_producer("producer-1".into());
    f.service.begin_shutdown();
    for terminal in [false, true] {
        if terminal {
            assert!(
                f.service
                    .finish_shutdown_receipt()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        for stopping in [false, true] {
            let (status, body, _) = request_with_body(
                state.clone(),
                "POST",
                crate::call_mode_adb::INTERNAL_ROUTE_STATUS_PATH,
                true,
                true,
                stopping,
                Bytes::from_static(br#"{"producer_id":"producer-1"}"#),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "terminal={terminal}, stopping={stopping}: {body}"
            );
            assert_eq!(body["code"], "backend_stopping");
        }
    }
    assert!(!f.root.join(".module-adb-interface.lock").exists());
    state.modem_actor.shutdown().unwrap();
}
