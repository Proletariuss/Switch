use super::*;
use crate::device_send_store::Store as V1Store;
use proletarius_store::device_sms_seal::{Error as SealError, Identity, Kernel, Point};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
const AUTHORITY: &str = "0123456789abcdef0123456789abcdef";
const GENERATION: &str = "abcdef0123456789abcdef0123456789";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/tmp/device-sms-generation-seal");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let root = Self(path);
        let store = V1Store::open(&root.0).unwrap();
        store.apply("prepare",json!({"client_request_id":"old","phone":"+12025550100","message":"fixture","device":"reader","binding":"original-route"})).unwrap();
        root
    }
    fn active(&self) -> PathBuf {
        self.0.join("device-send-ledger.json")
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn identity() -> Identity {
    Identity::parse(AUTHORITY, GENERATION).unwrap()
}
fn valid(bytes: &[u8]) -> std::result::Result<(), CodecError> {
    if decode_snapshot_bytes(bytes).is_ok() {
        return Ok(());
    }
    CompleteLegacyIndex::from_snapshot(bytes, &digest(bytes).0).map(|_| ())
}
fn wait(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(Instant::now() < deadline, "waiting {}", path.display());
        std::thread::sleep(Duration::from_millis(5));
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
fn worker(root: &Root, label: &str) -> Worker {
    Worker(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "device_send_store::authority_v2::seal_tests::old_v1_process_worker",
                "--nocapture",
            ])
            .env("SWITCH_OLD_V1_ROOT", &root.0)
            .env("SWITCH_OLD_V1_LABEL", label)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn result(root: &Root, label: &str) -> Value {
    let path = root.0.join(format!("{label}.result.json"));
    wait(&path);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn claim() -> Value {
    json!({"client_request_id":"old","binding":"original-route","attempt":"old-attempt"})
}

#[test]
#[ignore = "real current-v1 subprocess helper invoked by generation-seal tests"]
fn old_v1_process_worker() {
    let root = PathBuf::from(std::env::var_os("SWITCH_OLD_V1_ROOT").unwrap());
    let label = std::env::var("SWITCH_OLD_V1_LABEL").unwrap();
    let store = V1Store::open(&root).unwrap();
    fs::write(
        root.join(format!("{label}.attempted")),
        b"attempting current Store::apply claim",
    )
    .unwrap();
    let claimed = store.apply("claim", claim());
    let (can_submit, error) = match claimed {
        Ok(value) => (value["can_submit"].as_bool().unwrap(), None),
        Err(error) => (false, Some(format!("{error:?}"))),
    };
    fs::write(root.join(format!("{label}.result.json")),serde_json::to_vec(&json!({"can_submit":can_submit,"error":error,"restart_required":store.status().restart_required,"physical_dispatch_calls":0})).unwrap()).unwrap();
}

#[test]
fn actual_old_v1_waiter_opened_original_lock_before_new_marker_and_is_fenced_after_unlock() {
    let root = Root::new();
    let before = fs::read(root.active()).unwrap();
    let scope = identity();
    let path = root.0.clone();
    let hook_root = path.clone();
    let kernel = Kernel::existing(&path)
        .unwrap()
        .with_hook_for_test(Arc::new(move |point| {
            let gate = match point {
                Point::MarkerSynced => Some(("kernel-before-marker", "allow-marker")),
                Point::MarkerDirectorySynced => Some(("marker-durable", "allow-anchor")),
                _ => None,
            };
            if let Some((hit, release)) = gate {
                fs::write(hook_root.join(hit), b"hit").unwrap();
                wait(&hook_root.join(release));
            }
            false
        }));
    let thread = std::thread::spawn(move || kernel.install(&scope, valid));
    wait(&root.0.join("kernel-before-marker"));
    let mut old = worker(&root, "queued");
    wait(&root.0.join("queued.attempted"));
    // Observe the descriptor opened by the REAL ArchiveStore::acquire_lock. The
    // child has no other code opening this name. It is held behind the kernel's
    // original lock, rather than an artificial pre-claim barrier or mocked parser.
    let lock_name = root
        .0
        .join("device-send-ledger.json.lock")
        .to_string_lossy()
        .into_owned();
    let deadline = Instant::now() + Duration::from_secs(15);
    let observed = loop {
        let output = Command::new("lsof")
            .args(["-p", &old.0.id().to_string(), "-Fn"])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&output.stdout).contains(&lock_name) {
            break String::from_utf8_lossy(&output.stdout).into_owned();
        }
        assert!(
            Instant::now() < deadline,
            "old process never opened original lock"
        );
        assert!(old.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(!root.0.join("queued.result.json").exists());
    fs::write(root.0.join("allow-marker"), b"go").unwrap();
    wait(&root.0.join("marker-durable"));
    assert!(!root.0.join("queued.result.json").exists());
    assert_eq!(fs::read(root.active()).unwrap(), before);
    assert_eq!(
        serde_json::from_slice::<Value>(&before).unwrap()[0]["schema_version"],
        1
    );
    fs::write(root.0.join("allow-anchor"), b"go").unwrap();
    thread.join().unwrap().unwrap();
    assert!(old.0.wait().unwrap().success());
    let queued = result(&root, "queued");
    assert_eq!(queued["can_submit"], false);
    assert_eq!(queued["restart_required"], true);
    assert!(queued["error"].is_string());
    let mut after = worker(&root, "after-seal");
    assert!(after.0.wait().unwrap().success());
    let after_result = result(&root, "after-seal");
    assert_eq!(after_result["can_submit"], false);
    assert_eq!(after_result["restart_required"], true);
    assert_eq!(fs::read(root.active()).unwrap(), before);
    if let Some(dir) = std::env::var_os("SWITCH_SEAL_EVIDENCE_DIR") {
        let dir = Path::new(&dir);
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("actual-old-v1-waiter.json"),serde_json::to_vec_pretty(&json!({"queued":queued,"after_seal":after_result,"active_schema":1,"active_sha256_before":digest(&before).0,"active_sha256_after":digest(&fs::read(root.active()).unwrap()).0,"observed_original_lock_open":true,"lsof":observed})).unwrap()).unwrap();
    }
}

#[test]
fn actual_old_v1_claim_winning_before_kernel_is_preserved_and_never_claimed_again() {
    let root = Root::new();
    let mut first = worker(&root, "before-seal");
    assert!(first.0.wait().unwrap().success());
    let first_result = result(&root, "before-seal");
    assert_eq!(first_result["can_submit"], true);
    let before = fs::read(root.active()).unwrap();
    let index = CompleteLegacyIndex::from_snapshot(&before, &digest(&before).0).unwrap();
    assert!(index.fences["old"].original_state == LegacyPhase::Submitting);
    assert_eq!(index.fences["old"].original_attempt, "old-attempt");
    Kernel::existing(&root.0)
        .unwrap()
        .install(&identity(), valid)
        .unwrap();
    let mut after = worker(&root, "after-prior-claim");
    assert!(after.0.wait().unwrap().success());
    let after_result = result(&root, "after-prior-claim");
    assert_eq!(after_result["can_submit"], false);
    assert_eq!(after_result["restart_required"], true);
    assert_eq!(fs::read(root.active()).unwrap(), before);
    if let Some(dir) = std::env::var_os("SWITCH_SEAL_EVIDENCE_DIR") {
        fs::write(Path::new(&dir).join("actual-old-v1-before-lock.json"),serde_json::to_vec_pretty(&json!({"before_seal":first_result,"after_seal":after_result,"preserved_phase":"submitting","preserved_attempt":"old-attempt","active_sha256":digest(&before).0,"prior_grant_revoked":false,"physical_dispatch_calls":0})).unwrap()).unwrap();
    }
}

#[test]
fn actual_current_v1_all_actions_reject_versioned_witness_while_active_remains_v1() {
    for action in [
        "recover",
        "recover_go_owned",
        "prepare",
        "claim",
        "finish",
        "get",
        "list",
    ] {
        let root = Root::new();
        let before = fs::read(root.active()).unwrap();
        Kernel::existing(&root.0)
            .unwrap()
            .install(&identity(), valid)
            .unwrap();
        let old = V1Store::open(&root.0).unwrap();
        let request = json!({"client_request_id":"old","phone":"+12025550100","message":"fixture","device":"reader","binding":"original-route","attempt":"old-attempt","state":"accepted","segments":1,"archive_confirmed":true});
        let result = match action {
            "recover" => old.recover().map(|_| Value::Null),
            "recover_go_owned" => old.recover_go_owned().map(|_| Value::Null),
            other => old.apply(other, request),
        };
        assert!(result.is_err(), "{action}");
        assert!(old.status().restart_required);
        assert_eq!(fs::read(root.active()).unwrap(), before);
    }
}

#[test]
fn every_install_fault_prefix_is_checked_by_actual_current_v1_claim() {
    for point in Point::ALL {
        let root = Root::new();
        let before = fs::read(root.active()).unwrap();
        let kernel = Kernel::existing(&root.0)
            .unwrap()
            .with_hook_for_test(Arc::new(move |at| at == point));
        assert!(
            matches!(kernel.install(&identity(),valid),Err(SealError::Injected(at)) if at==point)
        );
        assert_eq!(fs::read(root.active()).unwrap(), before);
        let old = V1Store::open(&root.0).unwrap();
        let result = old.apply("claim", claim());
        if matches!(
            point,
            Point::MarkerCreated
                | Point::MarkerPartial
                | Point::MarkerWritten
                | Point::MarkerSynced
        ) {
            assert_eq!(
                result.unwrap()["can_submit"],
                true,
                "v1 may continue before seal publication, no v2 ID exists"
            );
        } else {
            assert!(result.is_err(), "new marker prefix {point:?}");
            assert!(old.status().restart_required);
            assert_eq!(fs::read(root.active()).unwrap(), before);
        }
    }
}

#[test]
fn full_codec_target_and_reserve_are_validated_without_becoming_issuance_or_active_writes() {
    let root = Root::new();
    let legacy = fs::read(root.active()).unwrap();
    let index = CompleteLegacyIndex::from_snapshot(&legacy, &digest(&legacy).0).unwrap();
    let initial=serde_json::to_vec(&json!([{"schema_version":2,"family":"device_sms","authority_id":AUTHORITY,"epoch":"1","issued_high_water":"0","legacy_new_admission":false,"legacy":{"path":"device-send-ledger.legacy-v1.json","sha256":digest(&legacy).0},"live_slots":[],"terminal_cache":[],"legacy_overlay":index.fences.values().cloned().collect::<Vec<_>>()}])).unwrap();
    let initial_state = decode_snapshot_bytes(&initial).unwrap();
    let kernel = Kernel::existing(&root.0).unwrap();
    let installed = kernel.install(&identity(), valid).unwrap();
    let first = kernel.stage_v2_target(&installed, &initial, valid).unwrap();
    assert_eq!(first.issued_floor(), 0);
    assert_eq!(fs::read(root.active()).unwrap(), legacy);
    assert!(kernel.inspect(&identity(), valid).is_err());
    fs::write(root.active(), &initial).unwrap();
    assert_eq!(kernel.inspect(&identity(), valid).unwrap(), first);
    let id = next_identity(&initial_state).unwrap();
    let request=decode_reserve_bytes(&serde_json::to_vec(&json!({"identity_version":2,"family":"device_sms","authority_id":AUTHORITY,"epoch":"1","sequence":"1","client_request_id":id.wire(),"intent_nonce":GENERATION,"purpose":"device_sms_send","device":"reader","payload":{"phone":"+12025550100","message":"fixture","expected_subscription_id":""}})).unwrap()).unwrap();
    let reserve = plan_reserve(&initial_state, &request, &index)
        .unwrap()
        .encoded()
        .unwrap();
    let staged = kernel.stage_v2_target(&first, &reserve, valid).unwrap();
    assert_eq!(staged.issued_floor(), 1);
    assert_eq!(fs::read(root.active()).unwrap(), initial);
    assert!(kernel.inspect(&identity(), valid).is_err());
    fs::write(root.active(), &reserve).unwrap();
    assert_eq!(kernel.inspect(&identity(), valid).unwrap(), staged);
    let value: Value = serde_json::from_slice(&reserve).unwrap();
    assert_eq!(value[0]["live_slots"][0]["dispatch_count"], 0);
    assert_eq!(value[0]["live_slots"][0]["phase"], "reserved_unbound");
    fs::write(root.active(), &initial).unwrap();
    assert!(
        kernel.inspect(&identity(), valid).is_err(),
        "active-only lower-floor rollback fenced"
    );
}
