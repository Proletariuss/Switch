//! Two exact ignored selectors. The parent owns process launch, deadlines,
//! sandbox and reaping. Each child runs one fixed case against one actual root.
use super::super::{Coordinator, backup_lifecycle, ledger, storage};
use super::*;
use crate::daemon_lifetime::Lease;
use crate::module_recovery::Gate;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const EVENT: &str = "SWITCH_FINITE_IO_595_EVENT ";
const ROOT_ENV: &str = "SWITCH_FINITE_IO_595_ROOT";
const CASE_ENV: &str = "SWITCH_FINITE_IO_595_CASE";
const EVIDENCE_ENV: &str = "SWITCH_FINITE_IO_595_EVIDENCE";
fn emit(v: Value) {
    let mut out = io::stdout().lock();
    writeln!(out, "\n{EVENT}{v}").unwrap();
    out.flush().unwrap();
}
fn parameters() -> (CaseId, PathBuf, PathBuf) {
    let case = CaseId::parse(&std::env::var(CASE_ENV).unwrap()).unwrap();
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).unwrap());
    let evidence = PathBuf::from(std::env::var_os(EVIDENCE_ENV).unwrap());
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime")
        .canonicalize()
        .unwrap();
    assert!(root.is_absolute() && root.starts_with(&runtime) && root != runtime);
    assert!(
        evidence.is_absolute()
            && evidence.parent() == root.parent()
            && !evidence.starts_with(&root)
    );
    assert_eq!(root.canonicalize().unwrap(), root);
    let m = fs::symlink_metadata(&root).unwrap();
    assert!(m.is_dir() && !m.file_type().is_symlink());
    assert_eq!(m.mode() & 0o7777, 0o700);
    (case, root, evidence)
}
fn native_count() -> usize {
    crate::usb_at_transport::recovery::reconcile::reconciliation_native_entry_count()
}
fn locks_reacquired(root: &Path, proof: &RootProof) {
    if root.join("call-mode-operation.lock").exists()
        && root.join(".module-adb-interface.lock").exists()
    {
        let operation = super::super::acquire_bound_existing_recovery_lock(
            root,
            proof,
            super::super::RecoveryLockKind::Operation,
            false,
        )
        .unwrap();
        let adb = super::super::acquire_bound_existing_recovery_lock(
            root,
            proof,
            super::super::RecoveryLockKind::AdbInterface,
            false,
        )
        .unwrap();
        adb.release().unwrap();
        operation.release().unwrap();
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Entry {
    fingerprint: Fingerprint,
    sha256: String,
}
type Inventory = BTreeMap<String, Entry>;
fn inventory(root: &Path) -> Inventory {
    fn walk(root: &Path, path: &Path, out: &mut Inventory, budget: &mut usize) {
        for row in fs::read_dir(path).unwrap() {
            *budget += 1;
            assert!(*budget <= 16384, "finite evidence inventory exhausted");
            let path = row.unwrap().path();
            let m = fs::symlink_metadata(&path).unwrap();
            assert!(!m.file_type().is_symlink());
            if m.is_dir() {
                walk(root, &path, out, budget);
            } else {
                assert!(m.is_file() && m.len() <= 4 * 1024 * 1024 && m.nlink() == 1);
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
                    .open(&path)
                    .unwrap();
                let before = Fingerprint::of(&file).unwrap();
                assert_eq!((m.dev(), m.ino()), (before.dev, before.ino));
                let mut raw = Vec::new();
                (&file).take(before.size + 1).read_to_end(&mut raw).unwrap();
                assert_eq!(before, Fingerprint::of(&file).unwrap());
                let named = fs::symlink_metadata(&path).unwrap();
                assert_eq!((named.dev(), named.ino()), (before.dev, before.ino));
                assert_eq!(raw.len() as u64, before.size);
                out.insert(
                    path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                    Entry {
                        fingerprint: before,
                        sha256: storage::digest(&raw),
                    },
                );
            }
        }
    }
    let mut out = Inventory::new();
    walk(root, root, &mut out, &mut 0);
    out
}
fn same_retained(a: &Entry, b: &Entry) -> bool {
    a.sha256 == b.sha256
        && a.fingerprint.same_inode(&b.fingerprint)
        && a.fingerprint.nlink == b.fingerprint.nlink
        && a.fingerprint.size == b.fingerprint.size
}
fn original_retained(before: &Inventory, after: &Inventory) {
    for original in before.values() {
        assert_eq!(
            after
                .values()
                .filter(|now| same_retained(original, now))
                .count(),
            1,
            "original file inode/raw was lost or duplicated"
        );
    }
}
fn public_ids(root: &Path) -> Vec<String> {
    let path = root.join("module-backups");
    if !path.exists() {
        return Vec::new();
    }
    let mut out = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    out.sort();
    out
}
fn public_inventory(root: &Path) -> Inventory {
    inventory(root)
        .into_iter()
        .filter(|(n, _)| n.starts_with("module-backups/"))
        .collect()
}
fn payload() -> Vec<u8> {
    let mut raw=serde_json::to_vec_pretty(&json!({"schema_version":3,"kind":"proletarius-usb-composition-backup","reason":"before_call_mode","saved_at":"2026-10-20T10:11:12.123456789Z","module":{"imei":"123456789012345","firmware":"595-finite-io"},"usb":{"vendor_id":11388,"product_id":9216,"flags":[0,0,0,0,0,0,0]},"voice":{"ims":2,"volte_capability":1,"volte_disabled":1},"restore_command":"AT+QCFG=\"USBCFG\",0x2C7C,0x2400,0,0,0,0,0,0,0"})).unwrap();
    raw.push(b'\n');
    raw
}
fn request(path: &str, body: Vec<u8>) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}
async fn import(root: &Path, proof: RootProof) -> (u16, Value) {
    let r = crate::call_mode_backups::handle_public_with_proof(
        request("/api/call-mode/backups/import", payload()),
        root,
        Ok(proof),
    )
    .await;
    let status = r.status().as_u16();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
async fn delete(root: &Path, gate: &Gate, proof: RootProof, id: &str) -> (u16, Value) {
    let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
    let usb = crate::usb_at::Owner::unavailable_for_tests("595-finite-io".into())
        .with_recovery_gate_for_tests(gate.clone());
    let radio = crate::module_radio::Service::new(
        root.to_path_buf(),
        usb,
        modem.clone(),
        crate::availability::BackendAvailability::with_module_recovery(gate.clone()),
        "595-finite-io".into(),
    );
    let r = crate::call_mode_backups::handle_delete_public_with_proof(
        request(
            "/api/call-mode/backups/delete",
            serde_json::to_vec(&json!({"confirm":true,"backup_id":id})).unwrap(),
        ),
        root,
        radio,
        Ok(proof),
    )
    .await;
    let status = r.status().as_u16();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    let closed = modem.shutdown();
    drop(modem);
    assert!(
        closed.is_ok(),
        "finite delete actor Close ACK failed; no join claim"
    );
    (status, serde_json::from_slice(&body).unwrap())
}
fn receipts(root: &Path) -> Vec<(String, Entry)> {
    inventory(root)
        .into_iter()
        .filter(|(name, _)| {
            let path = root.join(name);
            fs::read(path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<ledger::Envelope>(&raw).ok())
                .is_some_and(|e| matches!(e.event, ledger::Event::CompletionReceipt { .. }))
        })
        .collect()
}
#[derive(Serialize, Deserialize)]
struct WorkerEvidence {
    case: CaseId,
    root: PathBuf,
    root_fingerprint: Fingerprint,
    pid: u32,
    before: Inventory,
    after: Inventory,
    trace: Trace,
    target_path: String,
    target_raw_sha256: String,
    target_json_complete: bool,
    http_status: Option<u16>,
    native_before: usize,
    native_after: usize,
    original_public_ids: Vec<String>,
    worker_completed: bool,
    original_retained: bool,
    genuine_some_joined_before_arm: bool,
}
fn save(path: &Path, e: &WorkerEvidence) {
    assert!(!path.exists() && !path.is_symlink());
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    let raw = serde_json::to_vec(e).unwrap();
    f.write_all(&raw).unwrap();
    f.sync_all().unwrap();
    File::open(path.parent().unwrap())
        .unwrap()
        .sync_all()
        .unwrap();
}
fn load(path: &Path) -> WorkerEvidence {
    let m = fs::symlink_metadata(path).unwrap();
    assert!(
        m.is_file() && !m.file_type().is_symlink() && m.nlink() == 1 && m.len() < 16 * 1024 * 1024
    );
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn target_entry<'a>(trace: &Trace, files: &'a Inventory) -> (&'a String, &'a Entry) {
    let fp = &trace.observations[0].target_after;
    files
        .iter()
        .find(|(_, e)| e.fingerprint.dev == fp.dev && e.fingerprint.ino == fp.ino)
        .expect("actual injected target file must remain")
}
fn is_late(c: CaseId) -> bool {
    matches!(
        c,
        CaseId::A08 | CaseId::A09 | CaseId::A10 | CaseId::B02 | CaseId::B03
    )
}
fn expected_pending(c: CaseId) -> bool {
    matches!(
        c,
        CaseId::A02
            | CaseId::A03
            | CaseId::A04
            | CaseId::A05
            | CaseId::A07
            | CaseId::A08
            | CaseId::A09
    )
}
fn assert_trace(t: &Trace) {
    let o = &t.observations[0];
    assert_eq!(t.hits, 1);
    assert_eq!(t.duplicates, 0);
    assert_eq!(t.bad_contexts, 0);
    assert_eq!(o.mode, Mode::Mutation);
    match t.case {
        CaseId::A01 => {
            assert!(o.delegated && o.success && o.short_calls >= 2);
            assert_eq!(o.bytes_written as u64, o.target_after.size);
        }
        CaseId::A02 | CaseId::A03 | CaseId::A04 | CaseId::A07 | CaseId::A08 | CaseId::A09 => {
            assert!(o.delegated && !o.success && o.bytes_written > 0);
            assert_eq!(o.bytes_written as u64, o.target_after.size);
        }
        _ => assert!(!o.delegated && !o.success),
    };
}

#[tokio::test]
#[ignore = "dedicated finite I/O worker; owned parent must sandbox and reap"]
async fn finite_io_worker() {
    let started = Instant::now();
    let (case, root, evidence) = parameters();
    assert!(!evidence.exists() && !evidence.is_symlink());
    assert_eq!(native_count(), 0);
    let mut lifecycle = None;
    let (lease, gate, settled) = if is_late(case) {
        let (l, g, s) = super::super::tests::prepare_publication_crash(&root).await;
        (l, g, Some(s))
    } else if case == CaseId::A07 {
        let (l, g) = super::super::tests::prepare_pending_for_finite_io(&root).await;
        (l, g, None)
    } else if matches!(case, CaseId::A01 | CaseId::A03) {
        let l = Lease::acquire(&root).unwrap();
        let g = Gate::inspect_with_lifetime(&l);
        lifecycle = Some(
            crate::call_mode_backups::lifecycle_tests::initialize_fresh_call_mode_lifecycle(
                &root, &g,
            ),
        );
        (l, g, None)
    } else {
        let (l, g) =
            super::super::tests::prepare_completed_backup_lifecycle(&root, case == CaseId::A04)
                .await;
        (l, g, None)
    };
    let proof = lease.root_proof();
    let before = inventory(&root);
    let original_public_ids = public_ids(&root);
    let native_before = native_count();
    if is_late(case) {
        assert!(
            native_before > 0,
            "late I/O case did not really join native/voice worker"
        );
    }
    if case == CaseId::A07 {
        assert_eq!(native_before, 0);
    }
    let root_fingerprint = proof.with_root(&root, Fingerprint::of).unwrap().unwrap();
    let arm = FaultArm::arm(case, &root, &proof).unwrap();
    let mut http_status = None;
    match case {
        CaseId::A01 | CaseId::A02 | CaseId::A03 | CaseId::A05 | CaseId::B01 => {
            let (status, body) = import(&root, proof.clone()).await;
            let expected = if case == CaseId::A01 { 201 } else { 400 };
            assert_eq!(
                status, expected,
                "finite import response status mismatch (error fields omitted)"
            );
            if case == CaseId::A01 {
                let id = body["backup"]["id"].as_str().unwrap();
                assert!(
                    fs::read(root.join("module-backups").join(id)).unwrap() == payload(),
                    "short write-all did not preserve the complete imported raw payload"
                );
            }
            http_status = Some(status);
        }
        CaseId::A04 | CaseId::A06 => {
            assert_eq!(original_public_ids.len(), 1);
            let (status, _) = delete(&root, &gate, proof.clone(), &original_public_ids[0]).await;
            assert_eq!(status, 500);
            http_status = Some(status);
            assert!(
                !root
                    .join("module-backups")
                    .join(&original_public_ids[0])
                    .exists()
            );
        }
        CaseId::A07 => {
            let result =
                Coordinator::reconcile_with_cancel(&gate, &root, Arc::new(AtomicBool::new(false)))
                    .await;
            assert!(
                result.is_err(),
                "partial Requested unexpectedly produced completion"
            );
            assert!(!result.err().unwrap().cleanup_unconfirmed());
        }
        _ => {
            let result = settled.unwrap().finalize();
            assert!(
                result.is_err(),
                "injected publication failure unexpectedly completed"
            );
            assert!(!result.err().unwrap().cleanup_unconfirmed());
        }
    }
    let trace = arm.finish().unwrap();
    assert_trace(&trace);
    assert_eq!(
        native_count(),
        native_before,
        "faulted metadata operation entered native reconciliation"
    );
    locks_reacquired(&root, &proof);
    let after = inventory(&root);
    original_retained(&before, &after);
    if matches!(case, CaseId::A02 | CaseId::A03 | CaseId::A05) {
        assert_eq!(
            public_inventory(&root),
            before
                .iter()
                .filter(|(n, _)| n.starts_with("module-backups/"))
                .map(|(n, e)| (n.clone(), e.clone()))
                .collect::<Inventory>()
        );
    }
    let observation = &trace.observations[0];
    let (target_path, target_raw_sha256, target_json_complete) = if observation.target_after.mode
        & (nix::libc::S_IFMT as u32)
        == nix::libc::S_IFREG as u32
    {
        let (n, e) = target_entry(&trace, &after);
        let raw = fs::read(root.join(n)).unwrap();
        (
            n.clone(),
            e.sha256.clone(),
            serde_json::from_slice::<Value>(&raw).is_ok(),
        )
    } else {
        (String::new(), String::new(), false)
    };
    if matches!(
        case,
        CaseId::A02 | CaseId::A03 | CaseId::A04 | CaseId::A07 | CaseId::A08 | CaseId::A09
    ) {
        assert!(
            !target_json_complete,
            "prefix write unexpectedly formed a complete record"
        );
    }
    if matches!(case, CaseId::B01 | CaseId::B02 | CaseId::B03) {
        if case != CaseId::B03 {
            assert!(
                target_json_complete,
                "complete sync-error record is malformed"
            );
        }
        if matches!(case, CaseId::B02 | CaseId::B03) {
            assert_eq!(receipts(&root).len(), 1);
        }
    }
    if let Some(l) = lifecycle {
        l.shutdown().await;
    }
    let result = WorkerEvidence {
        case,
        root: root.clone(),
        root_fingerprint,
        pid: std::process::id(),
        before,
        after,
        trace,
        target_path,
        target_raw_sha256,
        target_json_complete,
        http_status,
        native_before,
        native_after: native_count(),
        original_public_ids,
        worker_completed: true,
        original_retained: true,
        genuine_some_joined_before_arm: is_late(case),
    };
    save(&evidence, &result);
    drop(gate);
    drop(proof);
    drop(lease);
    assert!(
        started.elapsed() < Duration::from_secs(240),
        "finite worker exceeded owned child budget"
    );
    emit(
        json!({"kind":"worker_complete","case":case.name(),"pid":std::process::id(),"evidence":evidence,"hits":1,"native_delta_after_arm":0,"genuine_some_joined_before_arm":is_late(case),"io_error_is_not_power_loss":true}),
    );
}

fn exact_sync(rows: &[Observation], point: SyncPoint, record: Record, fp: &Fingerprint) -> bool {
    rows.iter().any(|o| {
        o.mode == Mode::Resync
            && o.sync_point == Some(point)
            && o.record == record
            && o.delegated
            && o.success
            && o.target_before.dev == fp.dev
            && o.target_before.ino == fp.ino
            && o.target_before == o.target_after
            && o.target_before == *fp
    })
}
fn root_sync(rows: &[Observation], root: &Fingerprint) -> bool {
    rows.iter().any(|o| {
        o.mode == Mode::Resync
            && o.sync_point == Some(SyncPoint::ResyncRoot)
            && o.delegated
            && o.success
            && o.target_before.same_inode(root)
            && o.target_before == o.target_after
    })
}
fn directory_ack(rows: &[Observation], points: &[SyncPoint], fp: &Fingerprint) -> bool {
    rows.iter().any(|o| {
        o.mode == Mode::Resync
            && o.sync_point.is_some_and(|p| points.contains(&p))
            && o.delegated
            && o.success
            && o.target_before.same_inode(fp)
            && o.target_before == o.target_after
    })
}
fn acknowledged_parents(
    rows: &[Observation],
    root: &Path,
    parent: &Fingerprint,
    backup: bool,
) -> bool {
    let outer = Fingerprint::of(&File::open(root.join("module-recovery-v1")).unwrap()).unwrap();
    let exact_parent = directory_ack(rows, &[SyncPoint::ResyncContainer], parent);
    let outer_ack = directory_ack(
        rows,
        &[
            SyncPoint::ResyncOuter,
            SyncPoint::ResyncNamespace,
            SyncPoint::ResyncContainer,
        ],
        &outer,
    );
    let nested_ack = if backup {
        let ns = Fingerprint::of(
            &File::open(root.join("module-recovery-v1/backup-lifecycle-v1")).unwrap(),
        )
        .unwrap();
        directory_ack(rows, &[SyncPoint::ResyncNamespace], &ns)
    } else {
        true
    };
    exact_parent && outer_ack && nested_ack
}
async fn ordinary_after_resume(root: &Path, gate: &Gate) {
    let proof = gate.root_proof_for(root).unwrap();
    let (status, body) = import(root, proof.clone()).await;
    assert_eq!(status, 201);
    let id = body["backup"]["id"].as_str().unwrap();
    let (status, _) = delete(root, gate, proof, id).await;
    assert_eq!(status, 200);
    assert!(!root.join("module-backups").join(id).exists());
}
#[tokio::test]
#[ignore = "dedicated fresh finite I/O observer; parent must reap worker first"]
async fn finite_io_observer() {
    let started = Instant::now();
    let (case, root, evidence) = parameters();
    let worker = load(&evidence);
    assert_eq!(case, worker.case);
    assert_eq!(root, worker.root);
    assert!(worker.worker_completed && worker.original_retained);
    assert_ne!(worker.pid, std::process::id());
    assert_trace(&worker.trace);
    assert_eq!(native_count(), 0);
    let before = inventory(&root);
    original_retained(&worker.before, &before);
    let lease = Lease::acquire(&root).unwrap();
    let proof = lease.root_proof();
    let root_fp = proof.with_root(&root, Fingerprint::of).unwrap().unwrap();
    assert!(root_fp.same_inode(&worker.root_fingerprint));
    if !worker.target_path.is_empty() {
        let actual = before.get(&worker.target_path).unwrap();
        assert_eq!(actual.sha256, worker.target_raw_sha256);
        assert_eq!(
            actual.fingerprint,
            worker.after.get(&worker.target_path).unwrap().fingerprint
        );
    }
    let watch = WatchGuard::start(&root, &proof).unwrap();
    let resumption = backup_lifecycle::resume(&root, &proof);
    let gate = Gate::inspect_with_lifetime(&lease);
    if expected_pending(case) {
        if matches!(case, CaseId::A02 | CaseId::A03 | CaseId::A04 | CaseId::A05) {
            assert!(
                resumption.is_err(),
                "unrecorded stage or partial record was adopted by resume"
            );
        }
        assert!(
            gate.is_quarantined(),
            "partial/unprepared immutable evidence was adopted"
        );
        assert!(
            receipts(&root).len()
                <= if matches!(case, CaseId::A02 | CaseId::A04 | CaseId::A05) {
                    1
                } else {
                    0
                }
        );
    } else {
        assert!(
            resumption.is_ok(),
            "strict one-sided or complete-chain metadata resync failed"
        );
        if gate.is_quarantined() {
            let completion =
                Coordinator::reconcile_with_cancel(&gate, &root, Arc::new(AtomicBool::new(false)))
                    .await
                    .unwrap()
                    .expect("metadata resume produced no completion");
            gate.consume_reconciliation(&completion).unwrap();
        }
        assert!(
            !gate.is_quarantined(),
            "resynced exact evidence did not cover startup"
        );
    }
    let observations = watch.finish().unwrap();
    assert_eq!(
        native_count(),
        0,
        "fresh observer crossed either instrumented reconciliation native entry"
    );
    locks_reacquired(&root, &proof);
    let after = inventory(&root);
    original_retained(&worker.before, &after);
    for (name, entry) in &before {
        if name.starts_with("module-recovery-v1/") {
            assert!(
                after.get(name) == Some(entry),
                "observer rewrote immutable existing record"
            );
        }
    }
    match case {
        CaseId::B01 | CaseId::B02 => {
            let fp = &worker.trace.observations[0].target_after;
            let record = if case == CaseId::B01 {
                Record::Commit
            } else {
                Record::CompletionReceipt
            };
            assert!(
                exact_sync(&observations, SyncPoint::ResyncRecord, record, fp),
                "missing actual successful sync of the exact previously failed record FD"
            );
            assert!(
                root_sync(&observations, &root_fp)
                    && acknowledged_parents(
                        &observations,
                        &root,
                        &worker.trace.observations[0].parent,
                        case == CaseId::B01
                    ),
                "missing actual exact parent/root resync ACK"
            );
        }
        CaseId::B03 => {
            assert!(
                root_sync(&observations, &worker.trace.observations[0].target_after),
                "missing actual successful retry of original root sync"
            );
            assert!(
                acknowledged_parents(
                    &observations,
                    &root,
                    &worker.trace.observations[0].parent,
                    false
                ),
                "missing actual exact parent resync ACK"
            );
        }
        _ => {}
    }
    if matches!(
        case,
        CaseId::A06 | CaseId::A10 | CaseId::B01 | CaseId::B02 | CaseId::B03
    ) {
        ordinary_after_resume(&root, &gate).await;
        assert_eq!(native_count(), 0);
    }
    emit(
        json!({"kind":"observer_complete","case":case.name(),"pid":std::process::id(),"worker_pid":worker.pid,"quarantined":gate.is_quarantined(),"expected_pending":expected_pending(case),"actual_sync_observations":observations,"original_inode_raw_retained":true,"instrumented_native_entry_count":native_count(),"native_counter_scope":"production reconcile and shared execute_session only","physical_acceptance":false,"power_loss_tested":false}),
    );
    drop(gate);
    drop(proof);
    drop(lease);
    assert!(
        started.elapsed() < Duration::from_secs(180),
        "finite observer exceeded owned child budget"
    );
}
