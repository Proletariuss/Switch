//! Real local child/lock receipts with per-owner OS-return fault seams.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

const CHILD_ROOT: &str = "SWITCH_ADB_LIFETIME_FIXTURE_ROOT";
#[test]
#[ignore = "subprocess entry point; invoked only by exact local child tests"]
fn real_child_fixture() {
    let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("fixture root"));
    let _lock = acquire_interface_lock(&root).expect("real private interface flock");
    std::fs::write(root.join("child-ready"), std::process::id().to_string()).unwrap();
    while !root.join("child-stop").exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Clone)]
pub(crate) struct HeldProbe {
    pub(crate) root: PathBuf,
    pub(crate) blocked: Arc<AtomicBool>,
    pub(crate) pid: Arc<AtomicU32>,
    pub(crate) spawned: Arc<AtomicUsize>,
    pub(crate) timeout: Duration,
    pub(crate) supervision_gate: Option<Arc<AtomicBool>>,
    // A fixture owns an admission domain; clones still compete for the exact
    // same permit. Intentional cross-owner contention tests share this Arc.
    admission: Arc<Semaphore>,
}
impl HeldProbe {
    pub(crate) fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        let root = std::env::temp_dir().join(format!(
            "adb-child-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            blocked: Arc::new(AtomicBool::new(true)),
            pid: Arc::new(AtomicU32::new(0)),
            spawned: Arc::new(AtomicUsize::new(0)),
            timeout: Duration::from_millis(30),
            supervision_gate: None,
            admission: Arc::new(Semaphore::new(1)),
        }
    }
    fn owner(&self, observation: Option<Arc<ProbeObservation>>) -> ChildOwner {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "call_mode_adb::child_tests::real_child_fixture",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut owner = ChildOwner::new(command.spawn().unwrap(), observation);
        self.spawned.fetch_add(1, Ordering::AcqRel);
        owner.nonblocking_stderr();
        self.pid.store(owner.child.id(), Ordering::Release);
        owner.faults.kill_blocked = Some(self.blocked.clone());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.root.join("child-ready").exists() {
            if Instant::now() >= deadline {
                self.blocked.store(false, Ordering::Release);
                panic!("fixture child did not acquire its real interface flock");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        owner
    }
    pub(crate) fn release(&self) {
        self.blocked.store(false, Ordering::Release);
    }
    pub(crate) fn alive(&self) -> bool {
        let pid = self.pid.load(Ordering::Acquire);
        pid != 0 && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
    }
    pub(crate) fn lock_busy(&self) -> bool {
        acquire_interface_lock(&self.root).is_err()
    }
    fn natural_exit(&self) {
        std::fs::write(self.root.join("child-stop"), []).unwrap();
    }
    fn assert_settled(&self) {
        assert!(!self.alive(), "exact child PID remains alive after result");
        drop(acquire_interface_lock(&self.root).expect("child's real flock released"));
        assert_eq!(
            nix::sys::wait::waitpid(
                nix::unistd::Pid::from_raw(self.pid.load(Ordering::Acquire) as i32),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG)
            ),
            Err(nix::errno::Errno::ECHILD),
            "child must already be reaped"
        );
    }
}
// Every worker gets a bounded external escape for controlled seams. Teardown
// merely allows the original production settlement path to kill/reap; it never
// fabricates the result or supplies a wait receipt for the tested assertions.
struct Escape(Arc<AtomicBool>);
impl Drop for Escape {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn watchdog(flag: Arc<AtomicBool>) -> std::sync::mpsc::Sender<()> {
    let (done, stop) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if stop.recv_timeout(Duration::from_secs(5)).is_err() {
            flag.store(false, Ordering::Release);
        }
    });
    done
}

pub(crate) async fn run_command_probe(probe: HeldProbe) -> Result<(), String> {
    run_probe_for(probe, "physical").await
}
pub(crate) async fn run_probe_for(probe: HeldProbe, producer: &str) -> Result<(), String> {
    let permit = probe
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| "busy".to_owned())?;
    let observation = ProbeObservation::begin(&probe.root, producer);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut owner = probe.owner(Some(observation));
        let _escape = Escape(probe.blocked.clone());
        let watchdog = watchdog(probe.blocked.clone());
        while probe
            .supervision_gate
            .as_ref()
            .is_some_and(|gate| !gate.load(Ordering::Acquire))
            && probe.blocked.load(Ordering::Acquire)
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        let result = owner.supervise(probe.timeout, Duration::ZERO);
        let _ = watchdog.send(());
        probe.assert_settled();
        result.map_err(|e| e.message)
    })
    .await
    .unwrap()
}

#[test]
fn real_child_success_wait_receipt_releases_original_owner() {
    let probe = HeldProbe::new("success");
    let mut owner = probe.owner(None);
    assert!(probe.alive() && probe.lock_busy());
    probe.natural_exit();
    owner
        .supervise(Duration::from_secs(2), Duration::ZERO)
        .unwrap();
    assert!(owner.settled);
    probe.assert_settled();
    std::fs::remove_dir_all(&probe.root).unwrap();
}

#[test]
fn failed_kill_retains_exact_child_permit_and_latched_timeout_until_real_reap() {
    let probe = HeldProbe::new("failed-kill");
    let mut owner = probe.owner(None);
    let admission = Arc::new(Semaphore::new(1));
    let permit = admission.clone().try_acquire_owned().unwrap();
    let escape = Escape(probe.blocked.clone());
    let watchdog = watchdog(probe.blocked.clone());
    let (sender, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _permit = permit;
        sender
            .send(owner.supervise(Duration::from_millis(30), Duration::ZERO))
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(result.try_recv().is_err());
    assert!(probe.alive() && probe.lock_busy());
    assert!(
        admission.try_acquire().is_err(),
        "original parent owner must remain busy"
    );
    probe.release();
    let error = result
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "module_adb_probe_timeout");
    worker.join().unwrap();
    assert!(admission.try_acquire().is_ok());
    probe.assert_settled();
    let _ = watchdog.send(());
    drop(escape);
    std::fs::remove_dir_all(&probe.root).unwrap();
}

#[test]
fn poll_and_wait_faults_never_turn_original_failure_into_success() {
    let probe = HeldProbe::new("poll-wait");
    let mut owner = probe.owner(None);
    owner.faults.poll_failures = 1;
    owner.faults.kill_failures = 1;
    owner.faults.wait_failures = 1;
    probe.release();
    let error = owner
        .supervise(Duration::from_secs(2), Duration::ZERO)
        .unwrap_err();
    assert_eq!(error.code, "module_adb_probe_unconfirmed");
    assert!(owner.settled);
    probe.assert_settled();
    std::fs::remove_dir_all(&probe.root).unwrap();
}

#[test]
fn late_zero_exit_after_poll_failure_preserves_failure_and_original_identity() {
    let probe = HeldProbe::new("late-zero");
    let mut owner = probe.owner(None);
    owner.faults.poll_failures = 1;
    let escape = Escape(probe.blocked.clone());
    let watchdog = watchdog(probe.blocked.clone());
    let worker =
        std::thread::spawn(move || owner.supervise(Duration::from_secs(2), Duration::ZERO));
    std::thread::sleep(Duration::from_millis(80));
    assert!(probe.alive() && probe.lock_busy());
    probe.natural_exit();
    assert_eq!(
        worker.join().unwrap().unwrap_err().code,
        "module_adb_probe_unconfirmed"
    );
    probe.assert_settled();
    let _ = watchdog.send(());
    drop(escape);
    std::fs::remove_dir_all(&probe.root).unwrap();
}

#[test]
fn panic_and_unconfirmed_wait_retain_owner_until_actual_wait_receipt() {
    let probe = HeldProbe::new("panic-unconfirmed");
    let mut owner = probe.owner(None);
    let unconfirmed = Arc::new(AtomicBool::new(true));
    owner.faults.panic_once = true;
    owner.faults.unconfirmed = Some(unconfirmed.clone());
    let admission = Arc::new(Semaphore::new(1));
    let permit = admission.clone().try_acquire_owned().unwrap();
    probe.release();
    let escape = Escape(unconfirmed.clone());
    let watchdog = watchdog(unconfirmed.clone());
    let (sender, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _permit = permit;
        sender
            .send(owner.supervise(Duration::from_secs(2), Duration::ZERO))
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(result.try_recv().is_err());
    assert!(admission.try_acquire().is_err());
    unconfirmed.store(false, Ordering::Release);
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .code,
        "module_adb_probe_unconfirmed"
    );
    worker.join().unwrap();
    probe.assert_settled();
    let _ = watchdog.send(());
    drop(escape);
    std::fs::remove_dir_all(&probe.root).unwrap();
}

#[test]
fn echild_seam_suppresses_further_pid_signals_but_never_claims_settlement() {
    let probe = HeldProbe::new("echild");
    let mut owner = probe.owner(None);
    owner.wait_error(std::io::Error::from_raw_os_error(nix::libc::ECHILD));
    assert!(!owner.signal_allowed && !owner.settled);
    // Allowing the kill seam cannot authorize signalling after ECHILD.
    probe.release();
    let later = probe.clone();
    let natural = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        assert!(later.alive() && later.lock_busy());
        later.natural_exit();
    });
    assert_eq!(
        owner
            .supervise(Duration::ZERO, Duration::ZERO)
            .unwrap_err()
            .code,
        "module_adb_probe_unconfirmed"
    );
    natural.join().unwrap();
    probe.assert_settled();
    std::fs::remove_dir_all(&probe.root).unwrap();
}

#[test]
fn settled_child_stderr_collection_never_waits_for_descendant_eof() {
    // An inherited descriptor is an IO fixture only, never a cleanup receipt.
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "sleep 1 & printf module_adb_authentication_required >&2; exit 1",
    ]);
    let started = Instant::now();
    let error = supervise_child(command, Duration::from_secs(2), Duration::ZERO).unwrap_err();
    assert_eq!(error.code, "module_adb_authentication_required");
    assert!(started.elapsed() < Duration::from_millis(700));
}

#[test]
fn stderr_flood_is_bounded_and_does_not_starve_timeout_polling() {
    // yes writes stdout; duplicate stdout to stderr in this IO-only fixture.
    let mut command = {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exec /usr/bin/yes diagnostic >&2"]);
        cmd
    };
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut owner = ChildOwner::new(child, None);
    let started = Instant::now();
    assert_eq!(
        owner
            .supervise(Duration::from_millis(80), Duration::ZERO)
            .unwrap_err()
            .code,
        "module_adb_probe_timeout"
    );
    assert!(owner.diagnostic.len() <= MAX_HELPER_ERROR_BYTES);
    assert!(started.elapsed() < Duration::from_secs(2));
}

// Isolating independent fixtures must preserve real contention within a
// deliberately shared admission domain, including cancellation of its waiter.
#[tokio::test]
async fn shared_fixture_admission_stays_busy_until_cancelled_observers_child_is_reaped() {
    let original = HeldProbe::new("shared-admission-original");
    let mut competing = HeldProbe::new("shared-admission-competing");
    competing.admission = original.admission.clone();
    let observer = tokio::spawn(run_command_probe(original.clone()));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !original.root.join("child-ready").exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the original fixture must acquire its real child lock");
    assert!(original.alive() && original.lock_busy());
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(
        run_command_probe(competing.clone()).await.unwrap_err(),
        "busy"
    );
    assert_eq!(competing.spawned.load(Ordering::Acquire), 0);
    assert_eq!(original.admission.available_permits(), 0);
    let independent = HeldProbe::new("independent-admission");
    independent.release();
    assert_ne!(
        run_command_probe(independent.clone()).await.unwrap_err(),
        "busy"
    );
    assert_eq!(independent.spawned.load(Ordering::Acquire), 1);
    independent.assert_settled();
    assert_eq!(original.admission.available_permits(), 0);
    std::fs::remove_dir_all(&independent.root).unwrap();
    original.release();
    tokio::time::timeout(Duration::from_secs(3), async {
        while original.admission.available_permits() != 1 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("only the real child retirement can return its permit");
    original.assert_settled();
    competing.release();
    // A fresh child is now admitted and actually retired. Its intentional
    // deadline failure is not an admission refusal or a successful probe.
    let error = run_command_probe(competing.clone()).await.unwrap_err();
    assert_ne!(error, "busy");
    assert_eq!(competing.spawned.load(Ordering::Acquire), 1);
    competing.assert_settled();
    assert_eq!(competing.admission.available_permits(), 1);
    std::fs::remove_dir_all(&original.root).unwrap();
    std::fs::remove_dir_all(&competing.root).unwrap();
}

// Deterministic registry tests: no child, timeout or scheduler ordering is needed.
fn observation_registry_namespace(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    PathBuf::from(format!(
        "fixture-observation-{}-{}-{label}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn parallel_observation_roots_keep_both_pending_identities_until_real_retirement() {
    let root_a = observation_registry_namespace("root-a");
    let root_b = observation_registry_namespace("root-b");
    let a = ProbeObservation::begin(&root_a, "producer-1");
    a.publish_failure(&ProbeError::new("controlled-a", "failure-a"));
    let b = ProbeObservation::begin(&root_b, "producer-1");
    b.publish_failure(&ProbeError::new("controlled-b", "failure-b"));
    assert!(
        pending_probe(&root_a, "producer-1")
            .unwrap()
            .message
            .contains("failure-a")
    );
    assert!(
        pending_probe(&root_b, "producer-1")
            .unwrap()
            .message
            .contains("failure-b")
    );
    assert!(pending_probe(&root_a, "different-producer").is_none());
    drop(b);
    assert!(pending_probe(&root_b, "producer-1").is_none());
    assert!(
        pending_probe(&root_a, "producer-1")
            .unwrap()
            .message
            .contains("failure-a")
    );
    drop(a);
    assert!(pending_probe(&root_a, "producer-1").is_none());
}

#[test]
fn parallel_observation_producers_keep_both_pending_identities_in_one_root() {
    let root = observation_registry_namespace("one-root");
    let a = ProbeObservation::begin(&root, "producer-a");
    a.publish_failure(&ProbeError::new("controlled-a", "failure-a"));
    let b = ProbeObservation::begin(&root, "producer-b");
    b.publish_failure(&ProbeError::new("controlled-b", "failure-b"));
    assert!(
        pending_probe(&root, "producer-a")
            .unwrap()
            .message
            .contains("failure-a")
    );
    assert!(
        pending_probe(&root, "producer-b")
            .unwrap()
            .message
            .contains("failure-b")
    );
    drop(b);
    assert!(pending_probe(&root, "producer-b").is_none());
    assert!(
        pending_probe(&root, "producer-a")
            .unwrap()
            .message
            .contains("failure-a")
    );
    drop(a);
    assert!(pending_probe(&root, "producer-a").is_none());
}

#[test]
fn observation_test_registry_prunes_retired_weak_namespaces() {
    let root = observation_registry_namespace("prune");
    let a = ProbeObservation::begin(&root, "producer-1");
    let weak = Arc::downgrade(&a);
    a.publish_failure(&ProbeError::new("controlled-a", "failure-a"));
    drop(a);
    assert!(
        weak.upgrade().is_none(),
        "the registry must never own the observation"
    );
    assert!(pending_probe(&root, "producer-1").is_none());
    assert!(
        !TEST_PROBE_OBSERVATIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key(&(root, "producer-1".to_owned()))
    );
}
