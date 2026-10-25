//! Genuine root authority and production HTTP/native writers; no physical I/O.
use super::*;
use crate::daemon_lifetime::{Lease, RootProof};
use crate::module_radio_transport::{
    CallModeModuleIdentity, UsbComposition as NativeUsb, VoiceConfiguration as NativeVoice,
};
use crate::module_recovery::Gate;
use serde_json::Value;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU64, Ordering};

fn root(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/iteration-594-backup-handlers")
        .join(format!(
            "{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
}

/// Retains the actual fresh read-only lifecycle owners. This carries no
/// receipt or device authority and never submits a modem/USB action.
pub(crate) struct FreshCallModeLifecycle {
    service: Option<crate::call_mode_lifecycle::Service>,
    modem: Option<crate::modem_actor::ModemActor>,
    factory_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl FreshCallModeLifecycle {
    pub(crate) async fn shutdown(mut self) {
        let service = self.service.take().unwrap();
        service.begin_shutdown();
        let service_result = service.finish_shutdown().await;
        let modem = self.modem.take().unwrap();
        let modem_result = modem.shutdown();
        drop(service);
        drop(modem);
        assert!(
            service_result.is_ok(),
            "fresh lifecycle actual shutdown unconfirmed: {service_result:?}"
        );
        assert!(
            modem_result.is_ok(),
            "fresh modem actual close acknowledgement unconfirmed"
        );
        assert_eq!(
            self.factory_calls.load(Ordering::SeqCst),
            0,
            "fresh lifecycle unexpectedly invoked its inert USB factory"
        );
    }
}
impl Drop for FreshCallModeLifecycle {
    fn drop(&mut self) {
        // Normal test exits explicitly await shutdown above. During unwinding,
        // still consume the actor's real close acknowledgement before owners
        // disappear; this fallback never asserts an async service receipt.
        if let Some(modem) = self.modem.take() {
            let result = modem.shutdown();
            drop(self.service.take());
            drop(modem);
            if result.is_err() {
                if std::thread::panicking() {
                    crate::diagnostics::write_line(format_args!(
                        "fresh fixture modem close unconfirmed during unwind"
                    ));
                } else {
                    panic!("fresh fixture modem close unconfirmed");
                }
            }
        }
    }
}

pub(crate) fn initialize_fresh_call_mode_lifecycle(
    root: &Path,
    gate: &Gate,
) -> FreshCallModeLifecycle {
    gate.root_proof_for(root)
        .expect("fresh fixture requires its actual lifetime RootProof");
    assert!(
        !gate.is_quarantined(),
        "fresh fixture root was already quarantined"
    );
    for name in [
        crate::module_recovery::LEGACY_EXECUTION_PENDING,
        ".call-mode-pristine-v1",
        "call-mode-operation.lock",
        ".module-adb-interface.lock",
        "module-backups",
        "voice-runtime",
        "call-mode-restore.active",
        "voice-control",
    ] {
        assert!(
            matches!(fs::symlink_metadata(root.join(name)),
            Err(error) if error.kind()==std::io::ErrorKind::NotFound),
            "fresh fixture must initialize production lifecycle before any evidence slot exists"
        );
    }
    let factory_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = factory_calls.clone();
    let usb = crate::usb_at::Owner::with_factory(
        "594-fresh-lifecycle-fixture".into(),
        std::sync::Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Err(crate::usb_at_transport::TransportError {
                code: "usb_at_not_found",
                message: "fixed inert fresh lifecycle fixture".into(),
                response: String::new(),
                uncertain: false,
            })
        }),
    )
    .with_recovery_gate_for_tests(gate.clone());
    let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
    let radio = crate::module_radio::Service::new(
        root.to_path_buf(),
        usb.clone(),
        modem.clone(),
        crate::availability::BackendAvailability::with_module_recovery(gate.clone()),
        "594-fresh-lifecycle-fixture".into(),
    );
    // Match actual main's owned call producer rather than bypassing the
    // production shutdown decision with an inbox that has no owner.
    let inbox =
        crate::message_archive::InboxState::for_go_producer("594-fresh-lifecycle-fixture".into());
    assert_eq!(
        inbox.current_module_producer_id().as_deref(),
        Some("594-fresh-lifecycle-fixture")
    );
    let service = crate::call_mode_lifecycle::Service::new(root, radio, inbox, usb);
    assert!(
        crate::call_mode_lifecycle::pristine_marker(root).unwrap(),
        "actual production lifecycle constructor did not establish pristine lineage"
    );
    FreshCallModeLifecycle {
        service: Some(service),
        modem: Some(modem),
        factory_calls,
    }
}

fn payload(schema: i64) -> Vec<u8> {
    let value = json!({
        "schema_version":schema, "kind":BACKUP_KIND, "reason":"before_call_mode",
        "saved_at":"2026-10-20T10:11:12.123456789Z",
        "module":{"imei":"123456789012345", "firmware":"594-handler-fixture"},
        "usb":{"vendor_id":11388, "product_id":9216, "flags":[0,0,0,0,0,0,0]},
        "voice":{"ims":2,"volte_capability":1,"volte_disabled":1},
        "restore_command":"AT+QCFG=\"USBCFG\",0x2C7C,0x2400,0,0,0,0,0,0,0"
    });
    let mut raw = serde_json::to_vec_pretty(&value).unwrap();
    raw.push(b'\n');
    raw
}
fn request(path: &str, raw: Vec<u8>) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .body(Full::new(Bytes::from(raw)))
        .unwrap()
}
async fn json_body(response: Response<Full<Bytes>>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// Audit process workers invoke the actual public handler after arming a hook.
pub(crate) async fn process_import(root: &Path, proof: RootProof) -> String {
    let response = handle_public_with_proof(
        request("/api/call-mode/backups/import", payload(CURRENT_SCHEMA)),
        root,
        Ok(proof),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
pub(crate) async fn process_delete(root: &Path, proof: RootProof, id: &str) {
    let response = handle_delete_public_with_proof(
        request(
            "/api/call-mode/backups/delete",
            serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
        ),
        root,
        crate::test_module_radio(root),
        Ok(proof),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!backup_directory(root).join(id).exists());
}

fn files(root: &Path) -> Vec<(PathBuf, u64, u64, Vec<u8>)> {
    fn visit(root: &Path, output: &mut Vec<(PathBuf, u64, u64, Vec<u8>)>) {
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                visit(&path, output);
            } else {
                output.push((
                    path.clone(),
                    metadata.dev(),
                    metadata.ino(),
                    fs::read(&path).unwrap(),
                ));
            }
            assert!(output.len() < 8192);
        }
    }
    let mut output = Vec::new();
    visit(root, &mut output);
    output.sort_by(|left, right| left.0.cmp(&right.0));
    output
}
fn protected_originals(root: &Path) -> Vec<(PathBuf, u64, u64, Vec<u8>)> {
    files(root)
        .into_iter()
        .filter(|(path, _, _, _)| {
            path.components()
                .any(|part| part.as_os_str() == "recovered-original")
        })
        .collect()
}
fn assert_originals_unchanged(before: &[(PathBuf, u64, u64, Vec<u8>)]) {
    assert!(!before.is_empty());
    for (path, dev, ino, raw) in before {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!((metadata.dev(), metadata.ino()), (*dev, *ino));
        assert!(
            fs::read(path).unwrap() == *raw,
            "protected original raw bytes changed"
        );
    }
}

async fn all_actual_writers(root: &Path, gate: &Gate) {
    let proof = gate.root_proof_for(root).unwrap();
    let raw = payload(CURRENT_SCHEMA);
    let response = handle_internal_with_proof(
        request(INTERNAL_CREATE_PATH, raw.clone()),
        root,
        Ok(proof.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let id = json_body(response).await["backup_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        read_export(root, &id).unwrap().0 == raw,
        "internal create did not preserve raw payload"
    );
    let response = handle_internal_with_proof(
        request(INTERNAL_CREATE_PATH, raw.clone()),
        root,
        Ok(proof.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        read_export(root, &id).unwrap().0 == raw,
        "collision replaced original payload"
    );
    let native_operation = try_call_mode_operation_file_lock(root).unwrap();
    let native = create_native_backup(
        root,
        CallModeModuleIdentity {
            imei: "123456789012345".into(),
            firmware: "594-native-fixture".into(),
        },
        NativeUsb {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [0; 7],
        },
        NativeVoice {
            ims: 2,
            volte_capability: 1,
            volte_disabled: 1,
        },
        true,
        &proof,
    )
    .await
    .unwrap();
    drop(native_operation);
    assert!(read_native_backup(root, &native).await.is_ok());
    let schema2 = payload(PREVIOUS_SCHEMA);
    let response = handle_public_with_proof(
        request("/api/call-mode/backups/import", schema2.clone()),
        root,
        Ok(proof.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let imported = json_body(response).await["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        read_export(root, &imported).unwrap().0 == schema2,
        "schema2 raw extra voice lost"
    );
    assert!(
        read_native_backup(root, &imported)
            .await
            .unwrap()
            .voice
            .is_none()
    );
    let fixed = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let first = write_imported_backup_with_proof(root, Ok(proof.clone()), &raw, fixed).unwrap();
    let second = write_imported_backup_with_proof(root, Ok(proof.clone()), &raw, fixed).unwrap();
    assert_ne!(first.id, second.id);
    assert!(second.id.ends_with("-2.json"));
    // Legacy export-only imports remain raw files and never gain restore authority.
    let mut legacy: Value = serde_json::from_slice(&raw).unwrap();
    legacy["schema_version"] = json!(0);
    legacy["kind"] = Value::Null;
    legacy.as_object_mut().unwrap().remove("module");
    legacy.as_object_mut().unwrap().remove("voice");
    let legacy = serde_json::to_vec(&legacy).unwrap();
    let response = handle_public_with_proof(
        request("/api/call-mode/backups/import", legacy.clone()),
        root,
        Ok(proof.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["backup"]["restorable"], false);
    assert!(
        read_export(root, body["backup"]["id"].as_str().unwrap())
            .unwrap()
            .0
            == legacy
    );
    let old = fs::metadata(backup_directory(root).join(&id)).unwrap();
    process_delete(root, proof, &id).await;
    let archived: Vec<_> = files(root)
        .into_iter()
        .filter(|(_, dev, ino, _)| (*dev, *ino) == (old.dev(), old.ino()))
        .collect();
    assert_eq!(
        archived.len(),
        1,
        "delete did not keep the sole original inode in a private archive"
    );
    assert!(archived[0].3 == raw, "delete archive bytes changed");
    assert!(
        !archived[0].0.starts_with(backup_directory(root)),
        "delete only logically hid public file"
    );
    assert!(!gate.is_quarantined());
}

#[tokio::test]
async fn genuine_fresh_handlers_native_import_collision_delete_restart() {
    let root = root("fresh");
    let lease = Lease::acquire(&root).unwrap();
    let gate = Gate::inspect_with_lifetime(&lease);
    assert!(!gate.is_quarantined());
    let lifecycle = initialize_fresh_call_mode_lifecycle(&root, &gate);
    all_actual_writers(&root, &gate).await;
    lifecycle.shutdown().await;
    drop(gate);
    drop(lease);
    let lease = Lease::acquire(&root).unwrap();
    backup_lifecycle::resume(&root, &lease.root_proof()).unwrap();
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
}

#[tokio::test]
async fn actual_handlers_preserve_body_read_busy_operation_order_before_authority() {
    let absent = root("invalid-body");
    for path in [INTERNAL_CREATE_PATH, "/api/call-mode/backups/import"] {
        let response = if path == INTERNAL_CREATE_PATH {
            handle_internal_with_proof(request(path, b"{".to_vec()), &absent, Err(())).await
        } else {
            handle_public_with_proof(request(path, b"{".to_vec()), &absent, Err(())).await
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!absent.exists());
    }
    for path in [INTERNAL_CREATE_PATH, "/api/call-mode/backups/import"] {
        let response = if path == INTERNAL_CREATE_PATH {
            handle_internal_with_proof(request(path, payload(CURRENT_SCHEMA)), &absent, Err(()))
                .await
        } else {
            handle_public_with_proof(request(path, payload(CURRENT_SCHEMA)), &absent, Err(())).await
        };
        assert_eq!(
            response.status(),
            if path == INTERNAL_CREATE_PATH {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::BAD_REQUEST
            }
        );
        assert!(
            !absent.exists(),
            "missing authority caused production filesystem creation"
        );
    }
    let root = root("error-order");
    let lease = Lease::acquire(&root).unwrap();
    let gate = Gate::inspect_with_lifetime(&lease);
    let id = process_import(&root, gate.root_proof_for(&root).unwrap()).await;
    let service = crate::test_module_radio(&root);
    let guard = service.try_call_mode_backup_delete().unwrap();
    for (id, status) in [
        ("usb-imported-absent.json", StatusCode::NOT_FOUND),
        (id.as_str(), StatusCode::CONFLICT),
    ] {
        let response = handle_delete_public_with_proof(
            request(
                "/api/call-mode/backups/delete",
                serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
            ),
            &root,
            service.clone(),
            Err(()),
        )
        .await;
        assert_eq!(response.status(), status);
    }
    drop(guard);
    let operation = try_call_mode_operation_file_lock(&root).unwrap();
    let response = handle_delete_public_with_proof(
        request(
            "/api/call-mode/backups/delete",
            serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
        ),
        &root,
        service.clone(),
        Err(()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    drop(operation);
    let response = handle_delete_public_with_proof(
        request(
            "/api/call-mode/backups/delete",
            serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
        ),
        &root,
        service,
        Err(()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(backup_directory(&root).join(id).exists());
}

#[tokio::test]
async fn actual_writer_rejects_foreign_expired_or_replaced_root_proof_without_writing() {
    let root = root("proof");
    let lease = Lease::acquire(&root).unwrap();
    let foreign = self::root("foreign");
    let foreign_lease = Lease::acquire(&foreign).unwrap();
    let response = handle_internal_with_proof(
        request(INTERNAL_CREATE_PATH, payload(CURRENT_SCHEMA)),
        &root,
        Ok(foreign_lease.root_proof()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!backup_directory(&root).exists());
    let expired = lease.root_proof();
    drop(lease);
    let response = handle_public_with_proof(
        request("/api/call-mode/backups/import", payload(CURRENT_SCHEMA)),
        &root,
        Ok(expired),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!backup_directory(&root).exists());
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let moved = root.with_extension("original");
    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    let response = handle_internal_with_proof(
        request(INTERNAL_CREATE_PATH, payload(CURRENT_SCHEMA)),
        &root,
        Ok(proof),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!backup_directory(&root).exists());
    assert!(!backup_directory(&moved).exists());
}

async fn post_recovery(owned: bool) {
    let root = root(if owned { "post-owned" } else { "post-none" });
    let (lease, gate) =
        crate::startup_recovery::reconcile::tests::prepare_completed_backup_lifecycle(&root, owned)
            .await;
    let originals = protected_originals(&root);
    let original_backups = list_backups(&root).unwrap();
    assert_eq!(original_backups.len(), 1);
    let original_id = original_backups[0].id.clone();
    let original_path = backup_directory(&root).join(&original_id);
    let original_metadata = fs::metadata(&original_path).unwrap();
    let original_raw = fs::read(&original_path).unwrap();
    all_actual_writers(&root, &gate).await;
    process_delete(&root, gate.root_proof_for(&root).unwrap(), &original_id).await;
    assert!(!original_path.exists());
    let retained: Vec<_> = files(&root)
        .into_iter()
        .filter(|(_, dev, ino, _)| {
            (*dev, *ino) == (original_metadata.dev(), original_metadata.ino())
        })
        .collect();
    assert_eq!(
        retained.len(),
        1,
        "protected original backup inode not retained exactly once"
    );
    assert!(
        retained[0].3 == original_raw,
        "protected original backup raw bytes changed"
    );
    assert!(!retained[0].0.starts_with(backup_directory(&root)));
    assert_originals_unchanged(&originals);
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
    drop(gate);
    drop(lease);
    let lease = Lease::acquire(&root).unwrap();
    backup_lifecycle::resume(&root, &lease.root_proof()).unwrap();
    let fresh = Gate::inspect_with_lifetime(&lease);
    assert!(!fresh.is_quarantined());
    assert_originals_unchanged(&originals);
    // A genuinely new lifetime can perform another public import and delete.
    let next = process_import(&root, fresh.root_proof_for(&root).unwrap()).await;
    process_delete(&root, fresh.root_proof_for(&root).unwrap(), &next).await;
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
    assert_originals_unchanged(&originals);
}
#[tokio::test]
async fn genuine_none_recovery_actual_all_writers_protected_delete_then_fresh_gate() {
    post_recovery(false).await;
}
#[tokio::test]
async fn genuine_some_recovery_actual_all_writers_protected_delete_then_fresh_gate() {
    post_recovery(true).await;
}

// Observation/fault injection is test-only and carries no authority or success token.
type DeleteHook = Box<dyn FnOnce() + Send>;
fn delete_hooks() -> &'static std::sync::Mutex<Vec<(PathBuf, DeleteHook)>> {
    static HOOKS: std::sync::OnceLock<std::sync::Mutex<Vec<(PathBuf, DeleteHook)>>> =
        std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}
pub(super) fn after_delete_selection(root: &Path) {
    let hook = {
        let mut hooks = delete_hooks().lock().unwrap();
        hooks
            .iter()
            .position(|(path, _)| path == root)
            .map(|index| hooks.remove(index).1)
    };
    if let Some(hook) = hook {
        hook();
    }
}

#[tokio::test]
async fn public_delete_keeps_initial_selected_inode_and_rejects_replacement_before_intent() {
    let root = root("selected-race");
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let id = process_import(&root, proof.clone()).await;
    let path = backup_directory(&root).join(&id);
    let metadata = fs::metadata(&path).unwrap();
    let raw = fs::read(&path).unwrap();
    let retained = root.join("selected-original.raw");
    let replacement = b"replacement must remain selected by nobody".to_vec();
    let hook_path = path.clone();
    let hook_retained = retained.clone();
    let hook_replacement = replacement.clone();
    let arrived = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_arrived = arrived.clone();
    delete_hooks().lock().unwrap().push((
        root.clone(),
        Box::new(move || {
            fs::rename(&hook_path, &hook_retained).unwrap();
            fs::write(&hook_path, hook_replacement).unwrap();
            hook_arrived.store(true, Ordering::SeqCst);
        }),
    ));
    let ledger = root.join("module-recovery-v1/backup-lifecycle-v1");
    let before: Vec<_> = files(&ledger)
        .into_iter()
        .map(|(path, _, _, raw)| (path, raw))
        .collect();
    let response = handle_delete_public_with_proof(
        request(
            "/api/call-mode/backups/delete",
            serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
        ),
        &root,
        crate::test_module_radio(&root),
        Ok(proof),
    )
    .await;
    assert!(
        arrived.load(Ordering::SeqCst),
        "selection replacement branch never reached"
    );
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(fs::read(&path).unwrap() == replacement);
    assert!(fs::read(&retained).unwrap() == raw);
    let after = fs::metadata(&retained).unwrap();
    assert_eq!((after.dev(), after.ino()), (metadata.dev(), metadata.ino()));
    let after: Vec<_> = files(&ledger)
        .into_iter()
        .map(|(path, _, _, raw)| (path, raw))
        .collect();
    assert!(
        before == after,
        "selection mismatch wrote an Intent or changed prior records"
    );
}

#[tokio::test]
async fn actual_handlers_inventory_lock_failure_keeps_public_files_and_ledger_unchanged() {
    let root = root("inventory-lock");
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let id = process_import(&root, proof.clone()).await;
    let before: Vec<_> = files(&root)
        .into_iter()
        .map(|(path, _, _, raw)| (path, raw))
        .collect();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("module-recovery-v1/backup-lifecycle-v1/inventory.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let response = handle_public_with_proof(
        request("/api/call-mode/backups/import", payload(CURRENT_SCHEMA)),
        &root,
        Ok(proof.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = handle_delete_public_with_proof(
        request(
            "/api/call-mode/backups/delete",
            serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
        ),
        &root,
        crate::test_module_radio(&root),
        Ok(proof),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let after: Vec<_> = files(&root)
        .into_iter()
        .filter(|(path, _, _, _)| path.file_name().unwrap() != "call-mode-operation.lock")
        .map(|(path, _, _, raw)| (path, raw))
        .collect();
    let before: Vec<_> = before
        .into_iter()
        .filter(|(path, _)| path.file_name().unwrap() != "call-mode-operation.lock")
        .collect();
    assert!(
        before == after,
        "failed inventory lock mutated public backups or durable records"
    );
}

#[tokio::test]
async fn actual_concurrent_imports_both_commit_without_new_physical_busy_requirement() {
    let root = root("concurrent-import");
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let gate = Gate::inspect_with_lifetime(&lease);
    let lifecycle = initialize_fresh_call_mode_lifecycle(&root, &gate);
    let service = crate::test_module_radio(&root);
    let _busy = service.try_call_mode_backup_delete().unwrap();
    let _operation = try_call_mode_operation_file_lock(&root).unwrap();
    let first = handle_public_with_proof(
        request("/api/call-mode/backups/import", payload(CURRENT_SCHEMA)),
        &root,
        Ok(proof.clone()),
    );
    let second = handle_public_with_proof(
        request("/api/call-mode/backups/import", payload(CURRENT_SCHEMA)),
        &root,
        Ok(proof),
    );
    let (first, second) = tokio::join!(first, second);
    let first_status = first.status();
    let second_status = second.status();
    let first = json_body(first).await;
    let second = json_body(second).await;
    assert_eq!(
        first_status,
        StatusCode::CREATED,
        "first actual import response: {first}"
    );
    assert_eq!(
        second_status,
        StatusCode::CREATED,
        "second actual import response: {second}"
    );
    let first = first["backup"]["id"].as_str().unwrap();
    let second = second["backup"]["id"].as_str().unwrap();
    assert_ne!(first, second);
    let raw = payload(CURRENT_SCHEMA);
    assert!(read_export(&root, first).unwrap().0 == raw);
    assert!(read_export(&root, second).unwrap().0 == raw);
    assert_eq!(list_backups(&root).unwrap().len(), 2);
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
    lifecycle.shutdown().await;
}

#[test]
fn real_import_keeps_hundredth_collision_suffix_and_exhaustion_is_nonmutating() {
    use std::io::Write;
    let root = root("hundred-collisions");
    let lease = Lease::acquire(&root).unwrap();
    let raw = payload(CURRENT_SCHEMA);
    let parsed = validate_import_payload(&raw).unwrap();
    let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    fs::create_dir(backup_directory(&root)).unwrap();
    for index in 1..100 {
        let path = backup_directory(&root).join(imported_backup_id(&parsed, now, index));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&raw).unwrap();
        file.sync_all().unwrap();
    }
    let imported =
        write_imported_backup_with_proof(&root, Ok(lease.root_proof()), &raw, now).unwrap();
    assert_eq!(imported.id, imported_backup_id(&parsed, now, 100));
    assert!(read_export(&root, &imported.id).unwrap().0 == raw);
    let before = files(&root);
    assert!(
        matches!(write_imported_backup_with_proof(&root,Ok(lease.root_proof()),&raw,now),Err(ImportFailure::BadRequest(message)) if message=="无法为导入备份分配唯一文件名")
    );
    assert!(
        before == files(&root),
        "exhausted name allocation mutated files or records"
    );
}

#[tokio::test]
async fn actual_main_owned_fresh_backup_shutdown_is_inert_and_checked() {
    let root = root("main-owned-shutdown");
    let lease = Lease::acquire(&root).unwrap();
    let gate = Gate::inspect_with_lifetime(&lease);
    let lifecycle = initialize_fresh_call_mode_lifecycle(&root, &gate);
    let id = process_import(&root, gate.root_proof_for(&root).unwrap()).await;
    let path = backup_directory(&root).join(&id);
    let before = fs::metadata(&path).unwrap();
    let raw = fs::read(&path).unwrap();
    // No physical operation, probe or runtime startup was admitted. An owned
    // main producer must still complete actual shutdown without touching USB.
    lifecycle.shutdown().await;
    let after = fs::metadata(&path).unwrap();
    assert_eq!(
        (after.dev(), after.ino(), after.mode(), after.len()),
        (before.dev(), before.ino(), before.mode(), before.len())
    );
    assert!(
        fs::read(&path).unwrap() == raw,
        "idle shutdown changed actual backup bytes"
    );
    drop(gate);
    drop(lease);
    let lease = Lease::acquire(&root).unwrap();
    backup_lifecycle::resume(&root, &lease.root_proof()).unwrap();
    assert!(!Gate::inspect_with_lifetime(&lease).is_quarantined());
}
