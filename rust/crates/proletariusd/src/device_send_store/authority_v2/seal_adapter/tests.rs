use super::*;
use crate::{daemon_lifetime::Lease, device_send_store::Store as V1Store};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
const AUTH: &str = "0123456789abcdef0123456789abcdef";
const GEN: &str = "abcdef0123456789abcdef0123456789";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    lease: Option<Lease>,
}
impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/tmp/seal-adapter");
        fs::create_dir_all(&parent).unwrap();
        let root = loop {
            let path = parent.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("{e}"),
            }
        };
        let lease = Lease::acquire(&root).unwrap();
        let store = V1Store::open(&root).unwrap();
        store.apply("prepare",json!({"client_request_id":"old","phone":"+12025550100","message":"fixture","device":"reader","binding":"original-route"})).unwrap();
        Self {
            root,
            lease: Some(lease),
        }
    }
    fn active(&self) -> PathBuf {
        self.root.join("device-send-ledger.json")
    }
    fn marker(&self) -> PathBuf {
        self.root.join(".device-send-ledger.json.presence-v1")
    }
    fn lock(&self) -> PathBuf {
        self.root.join("device-send-ledger.json.lock")
    }
    fn proof(&self) -> RootProof {
        self.lease.as_ref().unwrap().root_proof()
    }
    fn kernel(&self) -> Kernel {
        Kernel::existing(&self.root).unwrap()
    }
    fn seal(&self) {
        self.kernel().install(&identity(), valid).unwrap();
    }
    fn adapter(&self) -> Adapter {
        Adapter::open(&self.root, self.proof(), identity()).unwrap()
    }
    fn commit_v2_fixture(&self) {
        let legacy = fs::read(self.active()).unwrap();
        let index = CompleteLegacyIndex::from_snapshot(&legacy, &digest(&legacy).0).unwrap();
        let state = AuthoritySnapshot {
            schema_version: 2,
            family: Family::DeviceSms,
            authority_id: AuthorityId(AUTH.into()),
            epoch: Decimal(1),
            issued_high_water: Decimal(0),
            legacy_new_admission: false,
            legacy: LegacyReference {
                path: LegacyPath::FrozenV1,
                sha256: digest(&legacy),
            },
            live_slots: vec![],
            terminal_cache: vec![],
            legacy_overlay: index.fences.values().cloned().collect(),
        };
        let target = encode_checked(&state).unwrap();
        let journal = json!([{"schema_version":1,"family":"device_sms","authority_id":AUTH,"epoch":"1","legacy_sha256":digest(&legacy).0,"initial_v2_sha256":digest(&target).0}]);
        fs::write(self.root.join("device-send-ledger.legacy-v1.json"), &legacy).unwrap();
        fs::write(
            self.root.join("device-send-ledger.v2-prepared.json"),
            &target,
        )
        .unwrap();
        fs::write(
            self.root.join("device-send-ledger.v2-migration.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let installed = self.kernel().inspect(&identity(), valid).unwrap();
        self.kernel()
            .stage_v2_target(&installed, &target, valid)
            .unwrap();
        // TEST ONLY: no active commit/migration API exists in the adapter.
        fs::write(self.active(), target).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.lease.take();
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn identity() -> Identity {
    Identity::parse(AUTH, GEN).unwrap()
}
fn valid(bytes: &[u8]) -> Result<()> {
    if decode_snapshot_bytes(bytes).is_ok() {
        Ok(())
    } else {
        CompleteLegacyIndex::from_snapshot(bytes, &digest(bytes).0).map(|_| ())
    }
}
fn wait(path: &Path) {
    let end = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(Instant::now() < end, "wait {}", path.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn evidence(name: &str, value: Value) {
    if let Some(dir) = std::env::var_os("SWITCH_ADAPTER_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn sole_owner_clones_latch_and_drop_reopen_cannot_clear_health() {
    let f = Fixture::new();
    f.seal();
    let adapter = f.adapter();
    let clone = adapter.clone();
    assert_eq!(adapter.inspect().unwrap().legacy_pins.prepared, 1);
    let before = fs::read(f.marker()).unwrap();
    fs::write(f.marker(), b"truncated").unwrap();
    assert!(adapter.inspect().is_err());
    assert!(clone.restart_required());
    fs::write(f.marker(), before).unwrap();
    assert!(matches!(clone.inspect(), Err(Error::Unavailable)));
    drop(adapter);
    drop(clone);
    assert!(Adapter::open(&f.root, f.proof(), identity()).is_err());
}
#[test]
fn closed_or_foreign_owner_cannot_read_and_reacquisition_never_adopts_old_adapter() {
    let mut f = Fixture::new();
    f.seal();
    let a = f.adapter();
    a.inspect().unwrap();
    let other = Fixture::new();
    assert!(Adapter::open(&other.root, f.proof(), identity()).is_err());
    drop(f.lease.take());
    assert!(a.inspect().is_err());
    assert!(a.restart_required());
    f.lease = Some(Lease::acquire(&f.root).unwrap());
    let fresh = f.adapter();
    assert!(fresh.inspect().is_ok());
    assert!(matches!(a.inspect(), Err(Error::Unavailable)));
}
#[test]
fn all_mixed_missing_truncated_witness_and_family_faults_latch_without_writes() {
    for kind in [
        "marker-missing",
        "marker-short",
        "anchor-missing",
        "anchor-short",
        "anchor-old",
        "active-missing",
        "frozen-missing",
        "frozen-short",
        "journal-missing",
        "journal-short",
        "prepared-short",
    ] {
        let f = Fixture::new();
        f.seal();
        f.commit_v2_fixture();
        let a = f.adapter();
        a.inspect().unwrap();
        let (path, replacement) = match kind {
            "marker-missing" => (f.marker(), None),
            "marker-short" => (f.marker(), Some(b"seal".to_vec())),
            "anchor-missing" => (f.lock(), None),
            "anchor-short" => (f.lock(), Some(b"seal".to_vec())),
            "anchor-old" => (f.lock(), Some(b"switch-archive-presence-v1\n".to_vec())),
            "active-missing" => (f.active(), None),
            "frozen-missing" => (f.root.join("device-send-ledger.legacy-v1.json"), None),
            "frozen-short" => (
                f.root.join("device-send-ledger.legacy-v1.json"),
                Some(b"[".to_vec()),
            ),
            "journal-missing" => (f.root.join("device-send-ledger.v2-migration.json"), None),
            "journal-short" => (
                f.root.join("device-send-ledger.v2-migration.json"),
                Some(b"[]".to_vec()),
            ),
            _ => (
                f.root.join("device-send-ledger.v2-prepared.json"),
                Some(b"[".to_vec()),
            ),
        };
        let original = fs::read(&path).unwrap();
        match &replacement {
            Some(bytes) => fs::write(&path, bytes).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        };
        let active_before = fs::read(f.active()).ok();
        assert!(a.inspect().is_err(), "{kind}");
        assert!(a.restart_required(), "{kind}");
        assert_eq!(fs::read(f.active()).ok(), active_before);
        assert_eq!(fs::read(&path).ok(), replacement, "{kind} changed evidence");
        fs::write(&path, original).unwrap();
        assert!(matches!(a.inspect(), Err(Error::Unavailable)), "{kind}");
        evidence(
            kind,
            json!({"rejected":true,"sticky_after_external_restore":true,"adapter_writes":0}),
        );
    }
}
#[test]
fn immutable_frozen_replacement_with_identical_bytes_is_detected_across_calls() {
    let f = Fixture::new();
    f.seal();
    f.commit_v2_fixture();
    let a = f.adapter();
    a.inspect().unwrap();
    let frozen = f.root.join("device-send-ledger.legacy-v1.json");
    let old = fs::metadata(&frozen).unwrap().ino();
    let bytes = fs::read(&frozen).unwrap();
    let tmp = f.root.join("replacement-fixture");
    fs::write(&tmp, &bytes).unwrap();
    fs::rename(tmp, &frozen).unwrap();
    assert_ne!(fs::metadata(&frozen).unwrap().ino(), old);
    assert!(a.inspect().is_err());
    assert!(a.restart_required());
    assert_eq!(fs::read(frozen).unwrap(), bytes);
}
#[test]
fn marker_published_anchor_not_finished_and_original_lock_replacement_are_fenced() {
    let f = Fixture::new();
    let k = f.kernel().with_hook_for_test(Arc::new(|p| {
        p == proletarius_store::device_sms_seal::Point::MarkerDirectorySynced
    }));
    assert!(k.install(&identity(), valid).is_err());
    assert!(Adapter::open(&f.root, f.proof(), identity()).is_err());
    assert!(Adapter::open(&f.root, f.proof(), identity()).is_err());
    let f = Fixture::new();
    f.seal();
    let a = f.adapter();
    a.inspect().unwrap();
    let bytes = fs::read(f.lock()).unwrap();
    fs::rename(f.lock(), f.root.join("old-original-lock")).unwrap();
    fs::write(f.lock(), bytes).unwrap();
    // Lifetime root remains live; the kernel's per-operation original named lock
    // can be a replacement across calls. This conservative adapter must pin it.
    assert!(a.inspect().is_err());
    assert!(a.restart_required());
}
#[test]
fn v2_family_diagnosis_is_read_only_and_zero_pins_is_never_a_drain_permit() {
    let f = Fixture::new();
    f.seal();
    f.commit_v2_fixture();
    let a = f.adapter();
    let before: Vec<_> = fs::read_dir(&f.root)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            let m = fs::metadata(&p).unwrap();
            (
                p.file_name().unwrap().to_owned(),
                fs::read(&p).unwrap(),
                m.ino(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        })
        .collect();
    let d = a.inspect().unwrap();
    assert_eq!(d.family_step, family::RecoveryStep::V2Present);
    assert_eq!(d.seal.issued_floor(), 0);
    assert_eq!(d.legacy_pins.prepared, 1);
    for (name, bytes, ino, mt, mtn, ct, ctn) in before {
        let p = f.root.join(name);
        let m = fs::metadata(&p).unwrap();
        assert_eq!(fs::read(p).unwrap(), bytes);
        assert_eq!(
            (
                m.ino(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec()
            ),
            (ino, mt, mtn, ct, ctn)
        );
    }
}
struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn worker(f: &Fixture, mode: &str) -> Worker {
    Worker(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "device_send_store::authority_v2::seal_adapter::tests::real_process_helper",
                "--nocapture",
            ])
            .env("SWITCH_ADAPTER_ROOT", &f.root)
            .env("SWITCH_ADAPTER_MODE", mode)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
#[test]
#[ignore = "real subprocess helper invoked by parent tests"]
fn real_process_helper() {
    let root = PathBuf::from(std::env::var_os("SWITCH_ADAPTER_ROOT").unwrap());
    let mode = std::env::var("SWITCH_ADAPTER_MODE").unwrap();
    if mode == "participating-owner" {
        let busy = matches!(
            Lease::acquire(&root),
            Err(crate::daemon_lifetime::Error::Busy)
        );
        fs::write(
            root.join("participant-result"),
            if busy {
                b"busy".as_slice()
            } else {
                b"admitted".as_slice()
            },
        )
        .unwrap();
        assert!(busy);
        return;
    }
    let _lease = if mode == "old-owner-grant" {
        Some(Lease::acquire(&root).unwrap())
    } else {
        None
    };
    let store = V1Store::open(&root).unwrap();
    let grant = store
        .apply(
            "claim",
            json!({"client_request_id":"old","binding":"original-route","attempt":"old-attempt"}),
        )
        .unwrap();
    assert_eq!(grant["can_submit"], true);
    fs::write(
        root.join("old-grant-returned"),
        serde_json::to_vec(&grant).unwrap(),
    )
    .unwrap();
    wait(&root.join("allow-synthetic-dispatch"));
    // Pure synthetic counter demonstrates the old owner already possesses a
    // returned grant. NO SMS/device/network dispatcher is linked or called.
    fs::write(root.join("synthetic-dispatch-count"), b"1").unwrap();
    let finish=store.apply("finish",json!({"client_request_id":"old","binding":"original-route","attempt":"old-attempt","state":"accepted","segments":1,"archive_confirmed":false}));
    assert!(finish.is_err());
    fs::write(root.join("old-finish-fenced"), b"true").unwrap();
}
#[test]
fn real_process_owner_exclusion_and_old_returned_grant_is_not_revoked_or_drained() {
    let f = Fixture::new();
    let mut participating = worker(&f, "participating-owner");
    assert!(participating.0.wait().unwrap().success());
    assert_eq!(
        fs::read(f.root.join("participant-result")).unwrap(),
        b"busy"
    );
    let mut old = worker(&f, "old-grant");
    wait(&f.root.join("old-grant-returned"));
    f.seal();
    let before = fs::read(f.active()).unwrap();
    let a = f.adapter();
    assert_eq!(a.inspect().unwrap().legacy_pins.submitting, 1);
    assert!(!f.root.join("synthetic-dispatch-count").exists());
    fs::write(f.root.join("allow-synthetic-dispatch"), b"go").unwrap();
    assert!(old.0.wait().unwrap().success());
    assert_eq!(
        fs::read(f.root.join("synthetic-dispatch-count")).unwrap(),
        b"1"
    );
    assert!(f.root.join("old-finish-fenced").exists());
    assert_eq!(fs::read(f.active()).unwrap(), before);
    assert_eq!(a.inspect().unwrap().legacy_pins.submitting, 1);
    evidence(
        "real-old-grant",
        json!({"participating_daemon":"busy","actual_v1_claim_can_submit":true,"old_synthetic_dispatch_after_seal":1,"physical_sms_calls":0,"old_finish_rejected":true,"submitting_pin":1,"drain_proven":false,"active_sha256":digest(&before).0}),
    );
}
#[test]
fn real_old_grant_process_kill_keeps_obligation_across_new_daemon_lifetime() {
    let mut f = Fixture::new();
    drop(f.lease.take());
    let mut old = worker(&f, "old-owner-grant");
    wait(&f.root.join("old-grant-returned"));
    assert!(matches!(
        Lease::acquire(&f.root),
        Err(crate::daemon_lifetime::Error::Busy)
    ));
    old.0.kill().unwrap();
    assert!(!old.0.wait().unwrap().success());
    assert!(!f.root.join("synthetic-dispatch-count").exists());
    f.lease = Some(Lease::acquire(&f.root).unwrap());
    f.seal();
    let a = f.adapter();
    assert_eq!(a.inspect().unwrap().legacy_pins.submitting, 1);
    evidence(
        "real-old-grant-killed",
        json!({"live_old_participating_owner_blocks_new_lease":true,"child_sigkill":true,"new_lifetime_submitting_pin":1,"automatic_recovery":false,"physical_sms_calls":0,"drain_proven":false}),
    );
}
#[test]
fn all_legacy_phases_are_pinned_without_converting_unknown_or_accepted_to_prepared() {
    let f = Fixture::new();
    let mut rows: Value = serde_json::from_slice(&fs::read(f.active()).unwrap()).unwrap();
    let original = rows[0]["records"]["old"].clone();
    rows[0]["records"] = json!({});
    for (id, state, archive) in [
        ("p", "prepared", false),
        ("s", "submitting", false),
        ("u", "unknown", false),
        ("a", "accepted", false),
        ("c", "accepted", true),
    ] {
        let mut row = original.clone();
        row["client_request_id"] = json!(id);
        row["state"] = json!(state);
        row["archive_confirmed"] = json!(archive);
        if state != "prepared" {
            row["attempt"] = json!("original-attempt");
        }
        if state == "accepted" {
            row["segments"] = json!(1);
        }
        rows[0]["records"][id] = row;
    }
    fs::write(f.active(), serde_json::to_vec(&rows).unwrap()).unwrap();
    f.seal();
    let a = f.adapter();
    let p = a.inspect().unwrap().legacy_pins;
    assert_eq!(
        p,
        LegacyPins {
            prepared: 1,
            submitting: 1,
            unknown: 1,
            accepted_unarchived: 1,
            accepted_archived: 1
        }
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(f.active()).unwrap()).unwrap(),
        rows
    );
}

#[test]
fn live_owner_detects_consistent_witness_and_active_rollback_after_higher_observation() {
    let f = Fixture::new();
    f.seal();
    f.commit_v2_fixture();
    let a = f.adapter();
    let first = a.inspect().unwrap();
    let original: Vec<_> = [f.active(), f.marker(), f.lock()]
        .into_iter()
        .map(|p| {
            let b = fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    let mut next: Value = serde_json::from_slice(&original[0].1).unwrap();
    next[0]["issued_high_water"] = json!("1");
    let target = serde_json::to_vec(&next).unwrap();
    let next_view = f
        .kernel()
        .stage_v2_target(&first.seal, &target, valid)
        .unwrap();
    fs::write(f.active(), &target).unwrap();
    assert_eq!(a.inspect().unwrap().seal.issued_floor(), 1);
    for (p, b) in original {
        fs::write(p, b).unwrap();
    }
    // A fresh standalone kernel cannot detect a consistent historical restore.
    assert_eq!(
        f.kernel()
            .inspect(&identity(), valid)
            .unwrap()
            .issued_floor(),
        0
    );
    assert_eq!(next_view.issued_floor(), 1);
    assert!(a.inspect().is_err());
    assert!(a.restart_required());
}
#[test]
fn concurrent_clones_and_poisoned_worker_never_clear_health_or_run_fresh_inspections() {
    let f = Fixture::new();
    f.seal();
    let a = f.adapter();
    a.inspect().unwrap();
    let marker = fs::read(f.marker()).unwrap();
    fs::write(f.marker(), b"broken").unwrap();
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let a = a.clone();
            std::thread::spawn(move || a.inspect().is_err())
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap());
    }
    fs::write(f.marker(), marker).unwrap();
    assert!(matches!(a.inspect(), Err(Error::Unavailable)));
    let f = Fixture::new();
    f.seal();
    let a = f.adapter();
    let copy = a.clone();
    assert!(
        std::thread::spawn(move || {
            let _state = copy.0.state.lock().unwrap();
            panic!("controlled sealed adapter worker panic");
        })
        .join()
        .is_err()
    );
    assert!(a.restart_required());
    assert!(matches!(a.inspect(), Err(Error::Unavailable)));
    drop(a);
    assert!(Adapter::open(&f.root, f.proof(), identity()).is_err());
}

#[test]
fn every_install_fault_prefix_is_diagnostic_only_and_never_repaired_by_adapter() {
    use proletarius_store::device_sms_seal::Point;
    for point in Point::ALL {
        let f = Fixture::new();
        let k = f
            .kernel()
            .with_hook_for_test(Arc::new(move |at| at == point));
        assert!(k.install(&identity(), valid).is_err());
        let members = [f.active(), f.marker(), f.lock()];
        let before: Vec<_> = members.iter().map(|p| fs::read(p).ok()).collect();
        let opened = Adapter::open(&f.root, f.proof(), identity());
        let coherent_visible = matches!(point, Point::AnchorWritten | Point::AnchorSynced);
        assert_eq!(opened.is_ok(), coherent_visible, "{point:?}");
        let after: Vec<_> = members.iter().map(|p| fs::read(p).ok()).collect();
        assert_eq!(before, after);
        let snapshots:Vec<_>=members.iter().zip(after).map(|(p,b)|json!({"member":p.file_name().unwrap().to_string_lossy(),"sha256":b.as_ref().map(|v|digest(v).0),"bytes_hex":b.map(|v|v.iter().map(|x|format!("{x:02x}")).collect::<String>())})).collect();
        evidence(
            &format!("adapter-install-prefix-{point:?}"),
            json!({"injected_point":format!("{point:?}"),"diagnosis_only":true,"opened":coherent_visible,"members":snapshots,"adapter_writes":0,"dispatch_calls":0,"durable_commit_or_drain_proven":false}),
        );
    }
}

#[test]
fn foreign_authority_journal_at_every_v1_preparation_step_is_rejected_and_latched() {
    const FOREIGN: &str = "22222222222222222222222222222222";
    for stage in 0..3 {
        let f = Fixture::new();
        f.seal();
        let a = f.adapter();
        let legacy = fs::read(f.active()).unwrap();
        let index = CompleteLegacyIndex::from_snapshot(&legacy, &digest(&legacy).0).unwrap();
        let state = AuthoritySnapshot {
            schema_version: 2,
            family: Family::DeviceSms,
            authority_id: AuthorityId(FOREIGN.into()),
            epoch: Decimal(1),
            issued_high_water: Decimal(0),
            legacy_new_admission: false,
            legacy: LegacyReference {
                path: LegacyPath::FrozenV1,
                sha256: digest(&legacy),
            },
            live_slots: vec![],
            terminal_cache: vec![],
            legacy_overlay: index.fences.values().cloned().collect(),
        };
        let prepared = encode_checked(&state).unwrap();
        let journal = json!([{"schema_version":1,"family":"device_sms","authority_id":FOREIGN,"epoch":"1","legacy_sha256":digest(&legacy).0,"initial_v2_sha256":digest(&prepared).0}]);
        fs::write(
            f.root.join("device-send-ledger.v2-migration.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        if stage >= 1 {
            fs::write(f.root.join("device-send-ledger.legacy-v1.json"), &legacy).unwrap();
        }
        if stage >= 2 {
            fs::write(
                f.root.join("device-send-ledger.v2-prepared.json"),
                &prepared,
            )
            .unwrap();
        }
        let outcome = a.inspect();
        assert!(
            outcome.is_err(),
            "seal authority A accepted foreign journal/prepared B at stage {stage}"
        );
        assert!(a.restart_required());
        assert_eq!(fs::read(f.active()).unwrap(), legacy);
        fs::remove_file(f.root.join("device-send-ledger.v2-migration.json")).unwrap();
        assert!(matches!(a.inspect(), Err(Error::Unavailable)));
        evidence(
            &format!("cross-authority-stage-{stage}"),
            json!({"seal_authority":AUTH,"foreign_authority":FOREIGN,"stage":stage,"rejected":true,"sticky_after_journal_removal":true,"active_unchanged":true,"dispatch_calls":0}),
        );
    }
}


fn startup(f: &Fixture) -> super::super::super::StartupMigration {
    super::super::super::StartupMigration { proof: f.proof() }
}

#[test]
fn production_startup_migrates_full_v1_without_dropping_tombstones_or_raw_bytes() {
    let f = Fixture::new();
    let mut v: Value = serde_json::from_slice(&fs::read(f.active()).unwrap()).unwrap();
    let template = v[0]["records"]["old"].clone();
    let mut records = serde_json::Map::new();
    for n in 0..10_000 {
        let id = format!("legacy-full-{n}");
        let mut record = template.clone();
        record["client_request_id"] = json!(id);
        records.insert(id, record);
    }
    v[0]["records"] = Value::Object(records);
    let original = serde_json::to_vec_pretty(&v).unwrap();
    fs::write(f.active(), &original).unwrap();
    let adapter = Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).unwrap();
    adapter.transact(|bytes, frozen| {
        assert_eq!(frozen, original);
        let snapshot = decode_snapshot_bytes(bytes)?;
        assert_eq!(snapshot.legacy_overlay.len(), 10_000);
        assert_eq!(snapshot.issued_high_water.0, 0);
        Ok((None, ()))
    }).unwrap();
    assert_eq!(fs::read(f.root.join("device-send-ledger.legacy-v1.json")).unwrap(), original);
    let legacy_writer = proletarius_store::ArchiveStore::with_limits(f.active(), SNAPSHOT_BYTES as u64, 1)
        .unwrap().track_durable_presence();
    assert!(legacy_writer.mutate(|_| Ok(())).is_err());
    let active_items: Vec<Value> = serde_json::from_slice(&fs::read(f.active()).unwrap()).unwrap();
    assert!(super::super::super::read(&active_items, true).is_err());
    assert!(!adapter.restart_required());
}

#[test]
fn migration_startup_requires_the_same_real_unpublished_owner_proof() {
    let f = Fixture::new();
    let other = Fixture::new();
    assert!(Adapter::open_or_migrate(&f.root, f.proof(), startup(&other)).is_err());
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(f.active()).unwrap()).unwrap()[0]["schema_version"], 1);
    let adapter = Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).unwrap();
    assert!(!adapter.restart_required());
}

#[test]
fn live_business_refusal_does_not_latch_but_missing_prepared_provenance_does() {
    let f = Fixture::new();
    let adapter = Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).unwrap();
    adapter.0.state.lock().unwrap().last = None;
    assert!(matches!(adapter.transact::<()>(|_, _| Err(CodecError::AuthorityMismatch)),
        Err(Error::Rejected(CodecError::AuthorityMismatch))));
    assert!(adapter.0.state.lock().unwrap().last.is_some());
    assert!(!adapter.restart_required());
    let prepared = f.root.join("device-send-ledger.v2-prepared.json");
    let bytes = fs::read(&prepared).unwrap();
    fs::remove_file(&prepared).unwrap();
    assert!(adapter.transact(|_, _| Ok((None, ()))).is_err());
    assert!(adapter.restart_required());
    fs::write(prepared, bytes).unwrap();
    assert!(adapter.clone().transact(|_, _| Ok((None, ()))).is_err());
    assert!(Adapter::open(&f.root, f.proof(), adapter.0.identity.clone()).is_err());
}

#[test]
fn failed_sealed_write_is_sticky_until_a_new_real_lifetime_recovers_the_exact_target() {
    use proletarius_store::device_sms_seal::Point;
    for point in Point::ALL.into_iter().chain([
        Point::WalCommitted, Point::ActiveCommitted, Point::WalRemoved, Point::WalDirectorySynced,
    ]) {
        let mut f = Fixture::new();
        let healthy = Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).unwrap();
        let faulty = Adapter(Arc::new(Inner {
            root: f.root.clone(), proof: f.proof(), identity: healthy.0.identity.clone(),
            kernel: f.kernel().with_hook_for_test(Arc::new(move |at| at == point)),
            state: Mutex::new(State::default()),
        }));
        faulty.inspect().unwrap();
        let result = faulty.transact(|bytes, _| {
            let mut snapshot = decode_snapshot_bytes(bytes)?;
            snapshot.issued_high_water = Decimal(7);
            Ok((Some(encode_checked(&snapshot)?), ()))
        });
        assert!(result.is_err(), "{point:?}");
        assert!(faulty.restart_required(), "{point:?}");
        assert!(faulty.clone().transact(|_, _| Ok((None, ()))).is_err());
        drop(healthy);
        drop(faulty);
        f.lease.take();
        f.lease = Some(Lease::acquire(&f.root).unwrap());
        let recovered = Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).unwrap();
        recovered.transact(|bytes, _| {
            assert_eq!(decode_snapshot_bytes(bytes)?.issued_high_water.0, 7);
            Ok((None, ()))
        }).unwrap();
        assert!(!recovered.restart_required());
    }
}


#[test]
fn historical_v1_enrolls_only_after_full_schema_and_never_recreates_a_missing_witness_lock() {
    let f = Fixture::new();
    let original = fs::read(f.active()).unwrap();
    fs::remove_file(f.marker()).unwrap();
    fs::remove_file(f.lock()).unwrap();
    let migrated = Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).unwrap();
    migrated.transact(|_, frozen| { assert_eq!(frozen, original); Ok((None, ())) }).unwrap();

    let f = Fixture::new();
    fs::remove_file(f.marker()).unwrap();
    fs::remove_file(f.lock()).unwrap();
    fs::write(f.active(), br#"[{"schema_version":1,"records":{"corrupt":{}}}]"#).unwrap();
    assert!(Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).is_err());
    assert!(!f.marker().exists(), "invalid legacy schema enrolled a presence witness");

    let f = Fixture::new();
    fs::remove_file(f.lock()).unwrap();
    assert!(Adapter::open_or_migrate(&f.root, f.proof(), startup(&f)).is_err());
    assert!(!f.lock().exists(), "existing witness silently recreated its original lock");
}
