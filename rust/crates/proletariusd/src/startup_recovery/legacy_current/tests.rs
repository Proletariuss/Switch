use super::*;
use crate::startup_recovery::Coordinator;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};

const BACKUP: &str = "usb-before-call-mode-fixture.json";

struct Fixture { root: PathBuf, lease: Option<crate::daemon_lifetime::Lease> }
impl Fixture {
    fn new(schema: u64) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(4).unwrap()
            .join("build/runtime/legacy-current-baseline").join(crate::snapshot::new_instance_id());
        fs::create_dir_all(&root).unwrap();
        let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        fs::create_dir(root.join("module-backups")).unwrap();
        fs::set_permissions(root.join("module-backups"), fs::Permissions::from_mode(0o700)).unwrap();
        let value = Self { root, lease: Some(lease) };
        value.write_backup(schema);
        value
    }
    fn bytes(schema: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version":schema,"kind":"proletarius-usb-composition-backup",
            "reason":"before_call_mode","saved_at":"2026-01-01T00:00:00Z",
            "module":{"imei":"123456789012345","firmware":"fixture"},
            "usb":{"vendor_id":0x2c7c,"product_id":0x0125,"flags":[1,1,1,1,1,0,0]},
            "voice":{"ims":1,"volte_capability":1,"volte_disabled":0},
            "restore_command":"AT+QCFG=\"USBCFG\",0x2C7C,0x0125,1,1,1,1,1,0,0"
        })).unwrap()
    }
    fn write_backup(&self, schema: u64) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600)
            .open(self.root.join("module-backups").join(BACKUP)).unwrap();
        file.write_all(&Self::bytes(schema)).unwrap();
    }
    fn gate(&self) -> Gate { Gate::inspect_with_lifetime(self.lease.as_ref().unwrap()) }
}
impl Drop for Fixture {
    fn drop(&mut self) { self.lease.take(); let _ = fs::remove_dir_all(&self.root); }
}
fn fixed_read(scope: &ReadScope) -> Result<crate::usb_at_transport::legacy_current::VerifiedDevice, TransportError> {
    crate::usb_at_transport::legacy_current::fixture_inspect(scope, 1)
}
fn forbidden_read(_: &ReadScope) -> Result<crate::usb_at_transport::legacy_current::VerifiedDevice, TransportError> {
    panic!("ineligible evidence must not reach a physical read")
}
async fn observe(f: &Fixture, gate: &Gate) -> Completed {
    let mut completed = Coordinator::inspect_legacy_current_inner(gate, &f.root,
        Arc::new(AtomicBool::new(false)), fixed_read).await.unwrap().unwrap();
    completed.fixture_reopen = Some(crate::usb_at_transport::legacy_current::fixture_reopen);
    completed
}

#[tokio::test]
async fn schema_two_and_three_require_real_fixed_reads_then_same_device_first_open() {
    for schema in [2, 3] {
        let f = Fixture::new(schema);
        let gate = f.gate();
        assert!(gate.is_quarantined());
        assert!(gate.admit_operation("ordinary").is_err());
        let original = fs::read(f.root.join("module-backups").join(BACKUP)).unwrap();
        let completed = observe(&f, &gate).await;
        assert!(gate.is_quarantined(), "metadata and fixed reads alone grant no ordinary permission");
        let seed = gate.consume_legacy_current(completed).unwrap();
        assert!(!gate.is_quarantined());
        let transport = seed.open(&gate).unwrap();
        transport.checked_close().unwrap();
        assert_eq!(fs::read(f.root.join("module-backups").join(BACKUP)).unwrap(), original);
        assert!(gate.admit_operation("ordinary").is_ok());
        // Ordinary fallback cannot reopen another module after a disconnect.
        assert!(seed.open(&gate).is_err());
        assert!(gate.is_admission_revoked());
    }
}

#[tokio::test]
async fn original_bytes_and_root_lifetime_remain_part_of_the_sealed_completion() {
    let f = Fixture::new(3);
    let gate = f.gate();
    let completed = observe(&f, &gate).await;
    let foreign = f.gate();
    assert!(completed.verify(&foreign).is_err());
    let original = fs::read(f.root.join("module-backups").join(BACKUP)).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    changed["module"]["firmware"] = "changed".into();
    fs::write(f.root.join("module-backups").join(BACKUP), serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(gate.consume_legacy_current(completed).is_err());
    assert!(gate.is_quarantined());
}

#[tokio::test]
async fn normal_close_and_new_lifetime_require_another_actual_observation() {
    let mut f = Fixture::new(3);
    let gate = f.gate();
    let completed = observe(&f, &gate).await;
    let seed = gate.consume_legacy_current(completed).unwrap();
    let transport = seed.open(&gate).unwrap();
    transport.checked_close().unwrap();
    assert!(!f.root.join(crate::module_recovery::LEGACY_EXECUTION_PENDING).exists());
    drop(seed);
    f.lease.take();
    f.lease = Some(crate::daemon_lifetime::Lease::acquire(&f.root).unwrap());
    let next = f.gate();
    assert!(next.is_quarantined(), "there is no persisted current-baseline permit");
    let completed = observe(&f, &next).await;
    let seed = next.consume_legacy_current(completed).unwrap();
    seed.open(&next).unwrap().checked_close().unwrap();
    assert!(!next.is_admission_revoked());
}

#[tokio::test]
async fn known_pending_unknown_and_unsafe_sources_never_reach_the_device() {
    for name in [
        crate::module_recovery::LEGACY_EXECUTION_PENDING,
        crate::call_mode_restore_journal::ACTIVE_NAME,
        crate::call_mode_restore_journal::ARCHIVE_NAME,
        "voice-control", "module-recovery-v1", "module-legacy-locks-v1",
        "runtime-download-lineage-v1",
    ] {
        let f = Fixture::new(3);
        fs::write(f.root.join(name), b"untrusted").unwrap();
        let gate = f.gate();
        let result = Coordinator::inspect_legacy_current_inner(&gate, &f.root,
            Arc::new(AtomicBool::new(false)), forbidden_read).await;
        assert!(!matches!(result, Ok(Some(_))));
        assert!(gate.is_quarantined());
        assert_eq!(fs::read(f.root.join(name)).unwrap(), b"untrusted");
        assert!(!f.root.join("call-mode-operation.lock").exists());
    }
    let f = Fixture::new(3);
    fs::remove_file(f.root.join("module-backups").join(BACKUP)).unwrap();
    symlink("/does-not-exist", f.root.join("module-backups").join(BACKUP)).unwrap();
    let gate = f.gate();
    assert!(!matches!(Coordinator::inspect_legacy_current_inner(&gate, &f.root,
        Arc::new(AtomicBool::new(false)), forbidden_read).await, Ok(Some(_))));
}

#[tokio::test]
async fn cancelled_or_busy_inspection_does_not_dispatch_and_never_deletes_a_lock() {
    let f = Fixture::new(3);
    let gate = f.gate();
    assert!(matches!(Coordinator::inspect_legacy_current_inner(&gate, &f.root,
        Arc::new(AtomicBool::new(true)), forbidden_read).await, Err(Failure::Cancelled)));
    assert!(!f.root.join("call-mode-operation.lock").exists());
    let f = Fixture::new(3);
    let proof = f.lease.as_ref().unwrap().root_proof();
    let held = acquire_bound_recovery_lock(&f.root, &proof, RecoveryLockKind::Operation, false).unwrap();
    let original = fs::metadata(f.root.join("call-mode-operation.lock")).unwrap();
    let gate = f.gate();
    assert!(matches!(Coordinator::inspect_legacy_current_inner(&gate, &f.root,
        Arc::new(AtomicBool::new(false)), forbidden_read).await, Err(Failure::Busy)));
    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(f.root.join("call-mode-operation.lock")).unwrap().ino(), original.ino());
    held.release().unwrap();
}

#[test]
fn malformed_legacy_backup_or_partial_runtime_is_data_ineligible() {
    for shape in ["schema-zero", "extra-file", "mixed-identity"] {
        let f = Fixture::new(3);
        match shape {
            "schema-zero" => f.write_backup(0),
            "extra-file" => fs::write(f.root.join("module-backups/unknown.json"), b"{}").unwrap(),
            _ => {
                let mut value: serde_json::Value = serde_json::from_slice(&Fixture::bytes(3)).unwrap();
                value["module"]["imei"] = "987654321098765".into();
                fs::write(f.root.join("module-backups/usb-before-call-mode-second.json"),
                    serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let proof = f.lease.as_ref().unwrap().root_proof();
        assert!(proof.with_root(&f.root, Evidence::capture).unwrap().is_err(), "{shape}");
    }
}

#[test]
fn bounded_known_cache_may_be_empty_partial_or_corrupt_but_never_an_unknown_namespace() {
    for shape in ["empty", "version-only", "corrupt-file", "unknown"] {
        let f = Fixture::new(3);
        let runtime = f.root.join("voice-runtime");
        fs::create_dir(&runtime).unwrap();
        let manifest = crate::call_mode_runtime_manifest::manifest();
        if shape != "empty" {
            let version = runtime.join(&manifest.version);
            fs::create_dir(&version).unwrap();
            if shape == "corrupt-file" { fs::write(version.join(&manifest.files[0].name), b"partial invalid cache").unwrap(); }
            if shape == "unknown" { fs::write(version.join("unknown-executable"), b"unknown").unwrap(); }
        }
        let proof = f.lease.as_ref().unwrap().root_proof();
        let result = proof.with_root(&f.root, Evidence::capture).unwrap();
        assert_eq!(result.is_ok(), shape != "unknown", "{shape}");
    }
}

#[tokio::test]
async fn actual_revoked_lock_provenance_requires_fixed_device_check_without_removing_intent() {
    let f = Fixture::new(3);
    fs::remove_dir_all(f.root.join("module-backups")).unwrap();
    let proof = f.lease.as_ref().unwrap().root_proof();
    for kind in [RecoveryLockKind::Operation, RecoveryLockKind::AdbInterface] {
        acquire_bound_recovery_lock(&f.root, &proof, kind, false).unwrap().release().unwrap();
    }
    assert!(crate::module_recovery::legacy_locks::migrate(&f.root, &proof).unwrap());
    crate::call_mode_lifecycle::invalidate_pristine_marker(&f.root).unwrap();
    let tombstone = f.root.join("module-legacy-locks-v1/revoked");
    let gate = f.gate();
    assert!(gate.is_quarantined());
    let completed = observe(&f, &gate).await;
    let seed = gate.consume_legacy_current(completed).unwrap();
    seed.open(&gate).unwrap().checked_close().unwrap();
    assert!(tombstone.exists(), "the actual intent fence is preserved");
    assert!(!gate.is_admission_revoked());
}

#[tokio::test]
async fn actual_backup_transaction_survives_current_baseline_and_next_lifetime() {
    let mut f = Fixture::new(3);
    let gate = f.gate();
    let completed = observe(&f, &gate).await;
    let seed = gate.consume_legacy_current(completed).unwrap();
    seed.open(&gate).unwrap().checked_close().unwrap();
    let proof = f.lease.as_ref().unwrap().root_proof();
    let operation = gate.admit_operation("backup-business-fixture").unwrap();
    let id = crate::call_mode_backups::create_native_backup(
        &f.root,
        crate::module_radio_transport::CallModeModuleIdentity {
            imei: "123456789012345".into(), firmware: "fixture".into(),
        },
        crate::module_radio_transport::UsbComposition {
            vendor_id: 0x2c7c, product_id: 0x0125, flags: [1; 7],
        },
        crate::module_radio_transport::VoiceConfiguration {
            ims: 1, volte_capability: 1, volte_disabled: 0,
        },
        false, &proof,
    ).await.unwrap();
    drop(operation);
    assert!(f.root.join("module-backups").join(id).exists());
    assert!(proof.with_root(&f.root, |root|
        crate::startup_recovery::reconcile::backup_lifecycle::legacy_current_data_only(root))
        .unwrap().unwrap());
    assert!(!f.root.join(crate::module_recovery::LEGACY_EXECUTION_PENDING).exists());
    drop(seed);
    f.lease.take();
    f.lease = Some(crate::daemon_lifetime::Lease::acquire(&f.root).unwrap());
    let next = f.gate();
    assert!(next.is_quarantined());
    let completed = observe(&f, &next).await;
    let seed = next.consume_legacy_current(completed).unwrap();
    seed.open(&next).unwrap().checked_close().unwrap();
}

#[tokio::test]
async fn production_seed_owner_startup_business_shutdown_and_next_lifetime_stay_closed() {
    let mut f = Fixture::new(3);
    let gate = f.gate();
    let seed = gate.consume_legacy_current(observe(&f, &gate).await).unwrap();
    let producer = format!("legacy-current-{}", crate::snapshot::new_instance_id());
    let usb = crate::usb_at::Owner::with_legacy_current_seed(producer.clone(), false, gate.clone(), seed);
    let modem = crate::modem_actor::ModemActor::with_test_port_opener_and_module_recovery(
        |_, _| panic!("verified USB startup must never open a serial candidate"), gate.clone()).unwrap();
    let availability = crate::availability::BackendAvailability::with_module_recovery(gate.clone());
    let radio = crate::module_radio::Service::new(
        f.root.clone(), usb.clone(), modem.clone(), availability, producer.clone());
    radio.preserve_current_startup_cfun();
    let inbox = crate::message_archive::InboxState::for_go_producer("fixture-compatibility".into());
    let startup = crate::module_startup::Service::start_verified(
        modem.clone(), usb.clone(), radio.clone(), inbox.clone(), producer.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !usb.native_sms_ready() {
            assert!(startup.last_error().is_none(), "fixed production startup failed");
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let call_mode = crate::call_mode_lifecycle::Service::new(&f.root, radio.clone(), inbox, usb.clone());
    let proof = f.lease.as_ref().unwrap().root_proof();
    let permit = gate.admit_operation(&producer).unwrap();
    crate::call_mode_backups::create_native_backup(
        &f.root,
        crate::module_radio_transport::CallModeModuleIdentity { imei:"123456789012345".into(), firmware:"fixture".into() },
        crate::module_radio_transport::UsbComposition { vendor_id:0x2c7c, product_id:0x0125, flags:[1; 7] },
        crate::module_radio_transport::VoiceConfiguration { ims:1, volte_capability:1, volte_disabled:0 },
        false, &proof,
    ).await.unwrap();
    drop(permit);
    startup.begin_shutdown();
    tokio::time::timeout(Duration::from_secs(5), call_mode.finish_shutdown_receipt()).await.unwrap().unwrap();
    radio.begin_shutdown();
    radio.finish_shutdown().await.unwrap();
    startup.shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    modem.shutdown().unwrap();
    drop((call_mode, startup, radio, usb, modem));
    assert!(!f.root.join(crate::module_recovery::LEGACY_EXECUTION_PENDING).exists());
    f.lease.take();
    f.lease = Some(crate::daemon_lifetime::Lease::acquire(&f.root).unwrap());
    let next = f.gate();
    assert!(next.is_quarantined(), "previous current proof must not survive its real lifetime");
    let seed = next.consume_legacy_current(observe(&f, &next).await).unwrap();
    seed.open(&next).unwrap().checked_close().unwrap();
}

fn cancel_before_wire(scope: &ReadScope) -> Result<crate::usb_at_transport::legacy_current::VerifiedDevice, TransportError> {
    scope.0.cancelled.store(true, Ordering::Release);
    fixed_read(scope)
}
#[tokio::test]
async fn cancellation_inside_owned_worker_preserves_cause_and_releases_both_real_locks() {
    let f = Fixture::new(3);
    let gate = f.gate();
    let result = Coordinator::inspect_legacy_current_inner(&gate, &f.root,
        Arc::new(AtomicBool::new(false)), cancel_before_wire).await;
    assert!(matches!(result, Err(Failure::Cancelled)));
    assert!(gate.is_quarantined());
    let proof = f.lease.as_ref().unwrap().root_proof();
    for kind in [RecoveryLockKind::Operation, RecoveryLockKind::AdbInterface] {
        acquire_bound_recovery_lock(&f.root, &proof, kind, false).unwrap().release().unwrap();
    }
}
