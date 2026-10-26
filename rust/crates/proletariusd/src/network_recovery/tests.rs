use super::*;
use std::collections::VecDeque;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Fake(Arc<Facts>);

struct Facts {
    epoch_ms: u64,
    elapsed_ms: AtomicU64,
    sleeps: Mutex<Vec<Duration>>,
    observations: Mutex<VecDeque<Observation>>,
    samples: AtomicUsize,
    reboots: AtomicUsize,
    direct_reboots: AtomicUsize,
    reboot_error: Mutex<Option<(String, bool)>>,
    reboot_gate: Option<Arc<Semaphore>>,
    identity: Mutex<String>,
    generation: AtomicU64,
    change_binding_on_sample: AtomicBool,
    change_binding_on_reboot: AtomicBool,
}

impl Fake {
    fn new(observations: Vec<Observation>, blocked: bool) -> Self {
        Self(Arc::new(Facts {
            epoch_ms: wall_clock_ms(),
            elapsed_ms: AtomicU64::new(0),
            sleeps: Mutex::new(vec![]),
            observations: Mutex::new(observations.into()),
            samples: AtomicUsize::new(0),
            reboots: AtomicUsize::new(0),
            direct_reboots: AtomicUsize::new(0),
            reboot_error: Mutex::new(None),
            reboot_gate: blocked.then(|| Arc::new(Semaphore::new(0))),
            identity: Mutex::new("a".repeat(64)),
            generation: AtomicU64::new(7),
            change_binding_on_sample: AtomicBool::new(false),
            change_binding_on_reboot: AtomicBool::new(false),
        }))
    }

    fn advance_to(&self, elapsed_ms: u64) {
        self.0.elapsed_ms.store(elapsed_ms, Ordering::SeqCst);
    }

    fn change_owner(&self) {
        *self.0.identity.lock().unwrap() = "b".repeat(64);
        self.0.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl Runtime for Fake {
    fn now_ms(&self) -> u64 {
        self.0
            .epoch_ms
            .saturating_add(self.0.elapsed_ms.load(Ordering::SeqCst))
    }

    fn binding(&self) -> OwnerBinding {
        let identity = self.0.identity.lock().unwrap().clone();
        OwnerBinding {
            identity: identity.clone(),
            generation: self.0.generation.load(Ordering::SeqCst),
            radio_binding: identity,
        }
    }

    async fn sleep(&self, delay: Duration) {
        self.0.sleeps.lock().unwrap().push(delay);
        self.0
            .elapsed_ms
            .fetch_add(delay.as_millis() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }

    async fn sample(&self) -> Observation {
        self.0.samples.fetch_add(1, Ordering::SeqCst);
        if self
            .0
            .change_binding_on_sample
            .swap(false, Ordering::SeqCst)
        {
            self.change_owner();
        }
        self.0
            .observations
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Observation::Disconnected)
    }

    async fn reboot(&self, direct: bool, binding: &OwnerBinding) -> Result<(), RebootFailure> {
        if self.0.change_binding_on_reboot.swap(false, Ordering::SeqCst) {
            self.change_owner();
        }
        if binding.radio_binding != self.binding().radio_binding {
            return Err(RebootFailure {
                reason: "radio_stale_request".into(),
                submitted: false,
            });
        }
        self.0.reboots.fetch_add(1, Ordering::SeqCst);
        if direct {
            self.0.direct_reboots.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(gate) = &self.0.reboot_gate {
            gate.acquire().await.unwrap().forget();
        }
        match self.0.reboot_error.lock().unwrap().clone() {
            Some((code, submitted)) => Err(RebootFailure {
                reason: code,
                submitted,
            }),
            None => Ok(()),
        }
    }
}

async fn finished(owner: &Service, id: &str) -> Operation {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let operation = owner.snapshot(id).unwrap().unwrap();
            if operation.done() {
                return operation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn at_reboot(fake: &Fake) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while fake.0.reboots.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn inactive(owner: &Service, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let active = owner
                .runtime
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active_operation_id
                .clone();
            if active.as_deref() != Some(id) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn root(label: &str) -> std::path::PathBuf {
    let base = std::path::PathBuf::from(
        std::env::var_os("SWITCH_BUILD_DIR").expect("workspace build directory"),
    )
    .join("tmp/network-recovery-tests");
    fs::create_dir_all(&base).unwrap();
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random).unwrap();
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let root = base.join(format!("{label}-{suffix}"));
    fs::create_dir(&root).unwrap();
    root
}

#[tokio::test]
async fn wake_delay_and_wall_clock_cooldown_survive_normal_completion() {
    let owner = Service::default();
    let fake = Fake::new(
        vec![Observation::Disconnected, Observation::Connected],
        false,
    );
    let first = owner
        .start("wake-1", Trigger::Wake, fake.clone())
        .await
        .unwrap();
    assert_eq!(first.phase, "waiting");
    assert_eq!(
        finished(&owner, &first.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("recovered")
    );
    assert_eq!(
        *fake.0.sleeps.lock().unwrap(),
        vec![WAKE_DELAY, READBACK_DELAY]
    );
    let cooldown = owner
        .start("wake-2", Trigger::Wake, fake.clone())
        .await
        .unwrap();
    assert_eq!(cooldown.outcome.as_deref(), Some("cooldown"));
    assert!(cooldown.cooldown_until_ms.unwrap() > fake.0.epoch_ms);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);

    // The absolute cooldown began at the physical arm after the wake delay.
    fake.advance_to(611_999);
    assert_eq!(
        owner
            .start("wake-3", Trigger::Wake, fake.clone())
            .await
            .unwrap()
            .outcome
            .as_deref(),
        Some("cooldown")
    );
    fake.advance_to(612_000);
    fake.0
        .observations
        .lock()
        .unwrap()
        .extend([Observation::Disconnected, Observation::Connected]);
    let next = owner
        .start("wake-4", Trigger::Wake, fake.clone())
        .await
        .unwrap();
    assert_eq!(
        finished(&owner, &next.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("recovered")
    );
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn direct_reboot_and_recovery_share_one_cooldown() {
    let owner = Service::default();
    let fake = Fake::new(vec![Observation::Connected], false);
    let direct = owner
        .start("direct-1", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    assert_eq!(
        finished(&owner, &direct.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("recovered")
    );
    assert_eq!(fake.0.direct_reboots.load(Ordering::SeqCst), 1);
    let manual = owner
        .start("manual-after-direct", Trigger::Manual, fake.clone())
        .await
        .unwrap();
    assert_eq!(manual.outcome.as_deref(), Some("cooldown"));
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unrecoverable_and_connected_observations_never_reboot() {
    for (index, observation, outcome) in [
        (1, Observation::Unrecoverable, "not_recoverable"),
        (2, Observation::Connected, "already_connected"),
    ] {
        let owner = Service::default();
        let fake = Fake::new(vec![observation], false);
        let operation = owner
            .start(&format!("observe-{index}"), Trigger::Manual, fake.clone())
            .await
            .unwrap();
        assert_eq!(
            finished(&owner, &operation.operation_id)
                .await
                .outcome
                .as_deref(),
            Some(outcome)
        );
        assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn readback_is_exactly_thirty_two_second_intervals() {
    let owner = Service::default();
    let fake = Fake::new(vec![], false);
    let operation = owner
        .start("readback", Trigger::Manual, fake.clone())
        .await
        .unwrap();
    let result = finished(&owner, &operation.operation_id).await;
    assert_eq!(result.outcome.as_deref(), Some("timed_out"));
    assert_eq!(result.sample_count, 30);
    assert_eq!(fake.0.samples.load(Ordering::SeqCst), 31);
    assert_eq!(*fake.0.sleeps.lock().unwrap(), vec![READBACK_DELAY; 30]);
}

#[tokio::test]
async fn same_id_replay_after_lost_response_never_dispatches_twice() {
    let owner = Service::default();
    let fake = Fake::new(vec![Observation::Connected], true);
    let first = owner
        .start("lost-http-response", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    at_reboot(&fake).await;
    let replay = owner
        .start("lost-http-response", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    assert_eq!(replay.operation_id, first.operation_id);
    assert_eq!(replay.submission, Submission::Unknown);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
    fake.0.reboot_gate.as_ref().unwrap().add_permits(1);
    assert_eq!(
        finished(&owner, &first.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("recovered")
    );
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn submitted_crash_reopen_blocks_second_wake_manual_and_changed_owner() {
    let root = root("submitted-reopen");
    let first_owner = Service::new(&root);
    let fake = Fake::new(vec![Observation::Connected], true);
    let first = first_owner
        .start("durable-reboot", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    at_reboot(&fake).await;

    // A new daemon instance recovers the arm record before any new admission.
    let reopened = Service::new(&root);
    let recovered = reopened.snapshot(&first.operation_id).unwrap().unwrap();
    assert_eq!(recovered.outcome.as_deref(), Some("unknown"));
    assert_eq!(recovered.submission, Submission::Unknown);
    assert!(recovered.cooldown_until_ms.unwrap() > fake.now_ms());

    fake.change_owner();
    let replay = reopened
        .start("durable-reboot", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    assert_eq!(replay.operation_id, first.operation_id);
    assert_eq!(replay.owner_generation, 7);
    for (request, trigger) in [
        ("second-wake", Trigger::Wake),
        ("second-manual", Trigger::Manual),
    ] {
        let blocked = reopened
            .start(request, trigger, fake.clone())
            .await
            .unwrap();
        assert_eq!(blocked.outcome.as_deref(), Some("cooldown"));
    }
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
    let recovered_bytes = fs::read(root.join(STORE_FILE)).unwrap();

    // Let the old in-process fixture unwind. Its stale completion cannot
    // replace the recovered unknown receipt or dispatch again.
    fake.0.reboot_gate.as_ref().unwrap().add_permits(1);
    inactive(&first_owner, &first.operation_id).await;
    assert_eq!(
        reopened
            .snapshot(&first.operation_id)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("unknown")
    );
    assert_eq!(fs::read(root.join(STORE_FILE)).unwrap(), recovered_bytes);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn expired_reopened_cooldown_allows_one_new_dispatch() {
    let root = root("expired-cooldown");
    let owner = Service::new(&root);
    let fake = Fake::new(vec![Observation::Connected], false);
    let first = owner
        .start("first", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    finished(&owner, &first.operation_id).await;
    let reopened = Service::new(&root);
    fake.advance_to(REBOOT_COOLDOWN.as_millis() as u64);
    fake.0
        .observations
        .lock()
        .unwrap()
        .push_back(Observation::Connected);
    let second = reopened
        .start("after-expiry", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    assert_eq!(
        finished(&reopened, &second.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("recovered")
    );
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 2);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn same_id_payload_drift_conflicts_before_dispatch() {
    let owner = Service::default();
    let fake = Fake::new(vec![Observation::Connected], false);
    let first = owner
        .start("same-id", Trigger::Manual, fake.clone())
        .await
        .unwrap();
    finished(&owner, &first.operation_id).await;
    let error = owner
        .start("same-id", Trigger::Reboot, fake.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::Configuration(code) if code == "network_request_id_conflict"
    ));
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stale_successor_completion_cannot_replace_current_operation() {
    let owner = Service::default();
    let first_fake = Fake::new(vec![Observation::Connected], false);
    let first = owner
        .start("old", Trigger::Manual, first_fake)
        .await
        .unwrap();
    finished(&owner, &first.operation_id).await;

    let fake = Fake::new(vec![Observation::Connected], true);
    let next = owner
        .start("successor", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    at_reboot(&fake).await;
    owner
        .finish(&first.operation_id, "failed", Some("late_result".into()))
        .await;
    let current = owner.snapshot(&next.operation_id).unwrap().unwrap();
    assert_eq!(current.phase, "submitting");
    assert!(current.outcome.is_none());
    fake.0.reboot_gate.as_ref().unwrap().add_permits(1);
    assert_eq!(
        finished(&owner, &next.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("recovered")
    );
}

#[tokio::test]
async fn owner_identity_change_before_arm_never_touches_dispatcher() {
    let owner = Service::default();
    let fake = Fake::new(vec![Observation::Disconnected], false);
    fake.0
        .change_binding_on_sample
        .store(true, Ordering::SeqCst);
    let operation = owner
        .start("owner-change", Trigger::Manual, fake.clone())
        .await
        .unwrap();
    let result = finished(&owner, &operation.operation_id).await;
    assert_eq!(result.outcome.as_deref(), Some("stale_owner"));
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn owner_replacement_after_durable_arm_cannot_reboot_replacement() {
    for trigger in [Trigger::Manual, Trigger::Reboot] {
        let owner = Service::default();
        let fake = Fake::new(vec![Observation::Disconnected], false);
        fake.0.change_binding_on_reboot.store(true, Ordering::SeqCst);
        let operation = owner.start("replace-after-arm", trigger, fake.clone()).await.unwrap();
        let result = finished(&owner, &operation.operation_id).await;
        assert_eq!(result.outcome.as_deref(), Some("blocked"));
        assert_eq!(result.submission, Submission::NotSubmitted);
        assert!(result.cooldown_until_ms.is_none());
        assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn rejected_submission_only_keeps_cooldown_when_commit_is_unknown() {
    for submitted in [false, true] {
        let owner = Service::default();
        let fake = Fake::new(vec![Observation::Disconnected], false);
        *fake.0.reboot_error.lock().unwrap() = Some(("radio_call_busy".into(), submitted));
        let first = owner
            .start("first", Trigger::Manual, fake.clone())
            .await
            .unwrap();
        let result = finished(&owner, &first.operation_id).await;
        assert_eq!(
            result.outcome.as_deref(),
            Some(if submitted { "unknown" } else { "blocked" })
        );
        let next = owner
            .start("next", Trigger::Manual, fake.clone())
            .await
            .unwrap();
        assert_eq!(next.outcome.as_deref() == Some("cooldown"), submitted);
        if !submitted {
            finished(&owner, &next.operation_id).await;
        }
    }
}

#[test]
fn request_decoder_requires_stable_id_and_explicit_trigger() {
    for bytes in [
        br#"{"client_request_id":"id","trigger":"other"}"#.as_slice(),
        br#"{"client_request_id":"id","trigger":"wake","reboot":true}"#,
        br#"{"trigger":"wake"}"#,
        br#"{"client_request_id":"id"}"#,
    ] {
        assert!(serde_json::from_slice::<StartRequest>(bytes).is_err());
    }
    let request: StartRequest =
        serde_json::from_slice(br#"{"client_request_id":"request-1","trigger":"reboot"}"#).unwrap();
    assert_eq!(request.trigger, Trigger::Reboot);
}

// Wiring-candidate tests. Require the actual network module/full daemon harness;
// they are not covered by the standalone coordinator component test result.
#[tokio::test]
async fn coordinator_default_reserves_before_new_runtime_and_exposes_full_and_close() {
    let service = Service::default();
    let a = service.try_reserve_start("reserved-a").unwrap();
    let b = service.try_reserve_start("reserved-b").unwrap();
    assert!(
        matches!(service.try_reserve_start("reserved-c"), Err(StoreError::Configuration(code)) if code == "network_recovery_admission_full")
    );
    let status = service.owner_status();
    assert_eq!((status.maximum, status.pending, status.peak), (2, 2, 2));
    service.begin_shutdown();
    assert!(
        matches!(service.try_reserve_start("reserved-d"), Err(StoreError::Configuration(code)) if code == "network_recovery_stopping")
    );
    drop(a);
    drop(b);
    service.finish_pending().await.unwrap();
    assert_eq!(service.owner_status().pending, 0);
}

#[tokio::test]
async fn unpolled_response_future_cannot_untrack_accepted_single_runner() {
    let service = Service::default();
    let fake = Fake::new(vec![Observation::Connected], true);
    let ticket = service
        .try_reserve_start("unpolled-response-owner")
        .unwrap();
    let response = service.start_reserved(
        ticket,
        "unpolled-response-owner",
        Trigger::Reboot,
        fake.clone(),
    );
    // start_reserved builds/spawns the named owner synchronously, not on response poll.
    at_reboot(&fake).await;
    drop(response);
    service.begin_shutdown();
    let mut finish = Box::pin(service.finish_pending());
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(finish.as_mut().poll(cx).is_pending()))
            .await
    );
    assert_eq!(service.owner_status().pending, 1);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
    fake.0.reboot_gate.as_ref().unwrap().add_permits(1);
    finish.await.unwrap();
    assert_eq!(service.owner_status().pending, 0);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn coordinator_post_submit_journal_failure_is_not_clean_or_replay_permission() {
    let service = Service::default();
    let fake = Fake::new(vec![Observation::Connected], true);
    let accepted = service
        .start("finish-failure-owner", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    at_reboot(&fake).await;
    let Journal::Memory(items) = &service.journal else {
        panic!("private memory journal expected")
    };
    let poison = items.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = poison.lock().unwrap();
            panic!("controlled private journal fault");
        })
        .join()
        .is_err()
    );
    service.begin_shutdown();
    fake.0.reboot_gate.as_ref().unwrap().add_permits(1);
    assert!(
        service
            .finish_pending()
            .await
            .unwrap_err()
            .contains("unconfirmed")
    );
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
    assert_eq!(
        service
            .runtime
            .lock()
            .unwrap()
            .active_operation_id
            .as_deref(),
        Some(accepted.operation_id.as_str())
    );
}

#[tokio::test]
async fn runtime_conflict_before_commit_preserves_old_owner_without_orphan() {
    let service = Service::default();
    service.runtime.lock().unwrap().active_operation_id = Some("older-runtime-owner".into());
    let fake = Fake::new(vec![Observation::Connected], false);
    for request in ["runtime-conflict", "replacement-for-conflict"] {
        let error = service
            .start(request, Trigger::Manual, fake.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::Configuration(code) if code == "network_operation_in_progress")
        );
        let snapshot = service.journal.read(read_snapshot).unwrap();
        assert!(snapshot.operations.is_empty());
        assert!(snapshot.requests.is_empty());
        assert_eq!(snapshot.sequence, 0);
        assert_eq!(
            service
                .runtime
                .lock()
                .unwrap()
                .active_operation_id
                .as_deref(),
            Some("older-runtime-owner")
        );
        assert_eq!(fake.0.samples.load(Ordering::SeqCst), 0);
        assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 0);
    }
    service.begin_shutdown();
    service.finish_pending().await.unwrap();
}

#[tokio::test]
async fn precommit_busy_stays_clean_without_mutating_second_request() {
    let service = Service::default();
    let fake = Fake::new(vec![Observation::Connected], true);
    let accepted = service
        .start("actual-running-owner", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    at_reboot(&fake).await;
    let replay = service
        .start("actual-running-owner", Trigger::Reboot, fake.clone())
        .await
        .unwrap();
    assert_eq!(replay.operation_id, accepted.operation_id);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.owner_status().pending != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let error = service
        .start("pure-precommit-busy", Trigger::Manual, fake.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Configuration(code) if code == "network_operation_in_progress")
    );
    assert_eq!(service.owner_status().faults, 0);
    let snapshot = service.journal.read(read_snapshot).unwrap();
    assert_eq!(snapshot.operations.len(), 1);
    assert!(!snapshot.requests.contains_key("pure-precommit-busy"));
    assert_eq!(
        service
            .runtime
            .lock()
            .unwrap()
            .active_operation_id
            .as_deref(),
        Some(accepted.operation_id.as_str())
    );
    service.begin_shutdown();
    fake.0.reboot_gate.as_ref().unwrap().add_permits(1);
    service.finish_pending().await.unwrap();
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 1);
}

fn polluted_request_key_items(key: &str) -> Vec<Value> {
    let source = Service::default();
    source
        .admit(
            "valid-persisted-request",
            Trigger::Manual,
            &OwnerBinding {
                identity: "a".repeat(64),
                generation: 1,
                radio_binding: "fixture-radio".into(),
            },
            1,
        )
        .unwrap();
    let mut snapshot = source.journal.read(read_snapshot).unwrap();
    let index = snapshot.requests.remove("valid-persisted-request").unwrap();
    snapshot
        .operations
        .get_mut(&index.operation_id)
        .unwrap()
        .client_request_id = key.into();
    snapshot.requests.insert(key.into(), index);
    vec![serde_json::to_value(snapshot).unwrap()]
}

#[test]
fn persisted_request_keys_are_schema_errors_while_live_input_stays_invalid() {
    for key in [
        "".to_owned(),
        " trailing ".into(),
        "control\nkey".into(),
        "x".repeat(129),
    ] {
        let items = polluted_request_key_items(&key);
        assert!(
            matches!(read_snapshot(&items), Err(StoreError::Configuration(code)) if code == "invalid_store_schema")
        );
        assert!(
            matches!(validate_request_id(&key), Err(StoreError::Configuration(code)) if code == "invalid_network_request_id")
        );
    }
}

#[tokio::test]
async fn guarded_persisted_key_corruption_latches_503_and_uncertain_owner() {
    let root = root("persisted-key-alias");
    let path = root.join(STORE_FILE);
    let raw = ArchiveStore::new(&path);
    raw.replace(polluted_request_key_items("bad\nkey")).unwrap();
    let before = fs::read(&path).unwrap();
    let owner = Service::new(&root);
    let Journal::Durable(store) = &owner.journal else {
        panic!("durable store")
    };
    assert_eq!(store.status().state, "unavailable");
    assert_eq!(store.status().reason, Some("store_corrupt"));
    assert!(store.status().restart_required);
    assert_eq!(fs::read(&path).unwrap(), before);
    let fake = Fake::new(vec![Observation::Connected], false);
    let error = owner
        .start("new-valid-request", Trigger::Reboot, fake.clone())
        .await
        .unwrap_err();
    assert!(matches!(&error, StoreError::Configuration(code) if code == "sms_store_unavailable"));
    let response = store_error(error);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], "sms_store_unavailable");
    assert_eq!(fake.0.samples.load(Ordering::SeqCst), 0);
    assert_eq!(fake.0.reboots.load(Ordering::SeqCst), 0);
    assert!(owner.runtime.lock().unwrap().active_operation_id.is_none());
    owner.begin_shutdown();
    assert!(
        owner
            .finish_pending()
            .await
            .unwrap_err()
            .contains("unconfirmed")
    );
    assert_eq!(owner.owner_status().pending, 0);
    // External file repair never clears the live guard's sticky corruption latch.
    raw.replace(vec![serde_json::to_value(Snapshot::default()).unwrap()])
        .unwrap();
    assert!(
        matches!(owner.snapshot("missing"), Err(StoreError::Configuration(code)) if code == "sms_store_unavailable")
    );
    assert_eq!(store.status().state, "unavailable");
    // A fresh Service after repair has a fresh health guard; no physical dispatch.
    let reopened = Service::new(&root);
    let Journal::Durable(fresh) = &reopened.journal else {
        panic!("durable store")
    };
    assert_eq!(fresh.status().state, "ready");
    assert!(reopened.snapshot("missing").unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_live_request_id_stays_400_without_owner_or_health_fault() {
    let root = root("live-input-not-corruption");
    let owner = Service::new(&root);
    let error = match owner.try_reserve_start("bad\nkey") {
        Ok(_) => panic!("invalid live input admitted"),
        Err(error) => error,
    };
    assert_eq!(store_error(error).status(), StatusCode::BAD_REQUEST);
    let Journal::Durable(store) = &owner.journal else {
        panic!("durable store")
    };
    assert_eq!(store.status().state, "ready");
    assert_eq!(owner.owner_status().pending, 0);
    assert_eq!(owner.owner_status().faults, 0);
    assert_eq!(owner.owner_status().accepted, 0);
    owner.begin_shutdown();
    owner.finish_pending().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

// A per-Service, one-shot checkpoint AFTER the real archive commit and BEFORE
// runtime retirement. No sleep creates the production race; the gate holds it.
pub(super) struct TerminalGate {
    entered: AtomicBool,
    notify: tokio::sync::Notify,
    released: Mutex<bool>,
    cv: std::sync::Condvar,
}
impl TerminalGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: AtomicBool::new(false),
            notify: Default::default(),
            released: Mutex::new(false),
            cv: Default::default(),
        })
    }
    pub(super) fn pause(&self) {
        self.entered.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        let guard = self.released.lock().unwrap();
        let (_guard, timed) = self
            .cv
            .wait_timeout_while(guard, Duration::from_secs(10), |released| !*released)
            .unwrap();
        assert!(!timed.timed_out(), "terminal test gate was not released");
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.entered.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

async fn terminal_commit_handoff(label: &str, submitted: Option<bool>) {
    let path = root(label);
    let owner = Service::new(&path);
    let gate = TerminalGate::new();
    *owner.terminal_gate.lock().unwrap() = Some(gate.clone());
    let first_runtime = Fake::new(vec![Observation::Connected], false);
    if let Some(submitted) = submitted {
        *first_runtime.0.reboot_error.lock().unwrap() = Some(("radio_call_busy".into(), submitted));
    }
    let trigger = if submitted.is_some() {
        Trigger::Reboot
    } else {
        Trigger::Manual
    };
    let first = owner
        .start("first", trigger, first_runtime.clone())
        .await
        .unwrap();
    gate.entered().await;
    // snapshot is a fresh read through the real GuardedArchive / ArchiveStore.
    let terminal = owner.snapshot(&first.operation_id).unwrap().unwrap();
    assert!(terminal.done());
    assert_eq!(
        terminal.outcome.as_deref(),
        Some(match submitted {
            None => "already_connected",
            Some(false) => "blocked",
            Some(true) => "unknown",
        })
    );
    let successor_runtime = Fake::new(vec![Observation::Connected], false);
    if submitted == Some(true) {
        successor_runtime.advance_to(REBOOT_COOLDOWN.as_millis() as u64 + 1);
        assert!(terminal.cooldown_until_ms.unwrap() > first_runtime.now_ms());
    }
    successor_runtime.change_owner();
    let task = {
        let owner = owner.clone();
        let runtime = successor_runtime.clone();
        tokio::spawn(async move { owner.start("successor", Trigger::Manual, runtime).await })
    };
    // Baseline returns an erroneous refusal while held; fixed code serializes
    // admission until the atomic terminal/retirement transaction is complete.
    let mut task = task;
    let early_result = tokio::time::timeout(Duration::from_millis(100), &mut task)
        .await
        .ok();
    gate.release();
    let result = match early_result {
        Some(result) => result.unwrap(),
        None => task.await.unwrap(),
    };
    assert!(
        result.is_ok(),
        "durable terminal followed by erroneous admission refusal: {result:?}"
    );
    let successor = result.unwrap();
    assert_ne!(successor.operation_id, first.operation_id);
    assert_eq!(successor.owner_generation, 8);
    assert_eq!(
        finished(&owner, &successor.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("already_connected")
    );
    assert_eq!(
        owner
            .snapshot(&first.operation_id)
            .unwrap()
            .unwrap()
            .outcome,
        terminal.outcome
    );
    if submitted == Some(true) {
        let early = owner
            .admit(
                "cooldown-probe",
                Trigger::Manual,
                &first_runtime.binding(),
                first_runtime.now_ms(),
            )
            .unwrap();
        assert_eq!(early.operation.outcome.as_deref(), Some("cooldown"));
        assert!(!early.dispatch);
    }
    owner.begin_shutdown();
    owner.finish_pending().await.unwrap();
    // Late predecessor retirement may never clear the successor's slot.
    {
        owner.runtime.lock().unwrap().active_operation_id = Some(successor.operation_id.clone());
    }
    owner.release_runtime(&first.operation_id);
    assert_eq!(
        owner.runtime.lock().unwrap().active_operation_id.as_deref(),
        Some(successor.operation_id.as_str())
    );
    owner.release_runtime(&successor.operation_id);
    assert_eq!(successor_runtime.0.reboots.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_commit_normal_finish_allows_safe_successor() {
    terminal_commit_handoff("terminal-normal", None).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_commit_reboot_blocked_allows_safe_successor() {
    terminal_commit_handoff("terminal-blocked", Some(false)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_commit_reboot_unknown_keeps_cooldown_then_allows_successor() {
    terminal_commit_handoff("terminal-unknown", Some(true)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_commit_stale_generation_never_dispatches_old_owner() {
    let owner = Service::new(&root("terminal-stale"));
    let gate = TerminalGate::new();
    *owner.terminal_gate.lock().unwrap() = Some(gate.clone());
    let old = Fake::new(vec![Observation::Disconnected], false);
    old.0.change_binding_on_sample.store(true, Ordering::SeqCst);
    let first = owner
        .start("stale-first", Trigger::Manual, old.clone())
        .await
        .unwrap();
    gate.entered().await;
    assert_eq!(
        owner
            .snapshot(&first.operation_id)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("stale_owner")
    );
    let next_runtime = Fake::new(vec![Observation::Connected], false);
    next_runtime.change_owner();
    let mut next = {
        let owner = owner.clone();
        let runtime = next_runtime.clone();
        tokio::spawn(async move {
            owner
                .start("stale-successor", Trigger::Manual, runtime)
                .await
        })
    };
    let early = tokio::time::timeout(Duration::from_millis(100), &mut next)
        .await
        .ok();
    gate.release();
    let result = match early {
        Some(result) => result.unwrap(),
        None => next.await.unwrap(),
    };
    let next = result.expect("committed stale owner must not refuse a safe successor");
    assert_eq!(next.owner_generation, 8);
    assert_eq!(
        finished(&owner, &next.operation_id)
            .await
            .outcome
            .as_deref(),
        Some("already_connected")
    );
    assert_eq!(old.0.reboots.load(Ordering::SeqCst), 0);
    owner.begin_shutdown();
    owner.finish_pending().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_commit_true_runner_abort_still_retires_in_started_archive_worker() {
    let owner = Service::new(&root("terminal-abort"));
    let runtime = Fake::new(vec![Observation::Connected], false);
    let first = owner
        .admit(
            "abort-first",
            Trigger::Manual,
            &runtime.binding(),
            runtime.now_ms(),
        )
        .unwrap()
        .operation;
    let gate = TerminalGate::new();
    *owner.terminal_gate.lock().unwrap() = Some(gate.clone());
    let ticket = owner.try_reserve_start("abort-first").unwrap();
    let runner = {
        let service = owner.clone();
        let id = first.operation_id.clone();
        tokio::spawn(ticket.into_runner(async move {
            let binding = runtime.binding();
            match service.run(id, Trigger::Manual, runtime, binding).await {
                Ok(()) => OwnerFinish::Clean,
                Err(_) => OwnerFinish::Uncertain,
            }
        }))
    };
    gate.entered().await;
    assert!(owner.snapshot(&first.operation_id).unwrap().unwrap().done());
    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    gate.release();
    inactive(&owner, &first.operation_id).await;
    let next_runtime = Fake::new(vec![Observation::Connected], false);
    let next = owner
        .start("abort-successor", Trigger::Manual, next_runtime)
        .await
        .unwrap();
    finished(&owner, &next.operation_id).await;
    owner.begin_shutdown();
    assert!(
        owner.finish_pending().await.is_err(),
        "actual owner abort remains an uncertain shutdown receipt"
    );
}

#[tokio::test]
async fn true_runner_abort_before_terminal_keeps_unknown_runtime_fence() {
    let path = root("preterminal-abort");
    let owner = Service::new(&path);
    let runtime = Fake::new(vec![], true);
    let first = owner
        .admit(
            "preterminal-first",
            Trigger::Reboot,
            &runtime.binding(),
            runtime.now_ms(),
        )
        .unwrap()
        .operation;
    let ticket = owner.try_reserve_start("preterminal-first").unwrap();
    let runner = {
        let owner = owner.clone();
        let runtime = runtime.clone();
        let id = first.operation_id.clone();
        tokio::spawn(ticket.into_runner(async move {
            let binding = runtime.binding();
            match owner.run(id, Trigger::Reboot, runtime, binding).await {
                Ok(()) => OwnerFinish::Clean,
                Err(_) => OwnerFinish::Uncertain,
            }
        }))
    };
    at_reboot(&runtime).await;
    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    let before = owner.snapshot(&first.operation_id).unwrap().unwrap();
    assert!(!before.done());
    assert_eq!(before.submission, Submission::Unknown);
    assert_eq!(
        owner.runtime.lock().unwrap().active_operation_id.as_deref(),
        Some(first.operation_id.as_str())
    );
    runtime.advance_to(REBOOT_COOLDOWN.as_millis() as u64 + 1);
    assert!(
        matches!(owner.admit("unsafe-next", Trigger::Manual, &runtime.binding(), runtime.now_ms()), Err(StoreError::Configuration(code)) if code == "network_operation_in_progress")
    );
    let reopened = Service::new(&path);
    assert_eq!(
        reopened
            .snapshot(&first.operation_id)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("unknown")
    );
    // Reopening does not touch the still-existing old Service's runtime fence.
    assert_eq!(
        owner.runtime.lock().unwrap().active_operation_id.as_deref(),
        Some(first.operation_id.as_str())
    );
    owner.begin_shutdown();
    assert!(owner.finish_pending().await.is_err());
}

#[tokio::test]
async fn terminal_commit_actual_archive_faults_preserve_pin_and_uncertain_health() {
    for point in 1..=5 {
        let path = root(&format!("terminal-commit-fault-{point}"));
        let store = ArchiveStore::new(path.join(STORE_FILE));
        let owner = Service {
            journal: Journal::Durable(store.clone().into()),
            ..Service::default()
        };
        owner.recover().unwrap();
        let runtime = Fake::new(vec![], true);
        *runtime.0.reboot_error.lock().unwrap() = Some(("radio_call_busy".into(), false));
        let first = owner
            .start("fault-first", Trigger::Reboot, runtime.clone())
            .await
            .unwrap();
        at_reboot(&runtime).await;
        store.fail_commit_at_for_test(point);
        runtime.0.reboot_gate.as_ref().unwrap().add_permits(1);
        owner.begin_shutdown();
        assert!(owner.finish_pending().await.is_err());
        assert_eq!(
            owner.runtime.lock().unwrap().active_operation_id.as_deref(),
            Some(first.operation_id.as_str())
        );
        let snapshot = read_snapshot(&store.load().unwrap()).unwrap();
        assert_eq!(snapshot.operations[&first.operation_id].done(), point >= 4);
        assert!(
            owner.snapshot(&first.operation_id).is_err(),
            "failed guarded commit latches health even when rename made terminal visible"
        );
        let before = std::fs::read(store.path()).unwrap();
        runtime.advance_to(REBOOT_COOLDOWN.as_millis() as u64 + 1);
        assert!(
            owner
                .admit(
                    "fault-successor",
                    Trigger::Manual,
                    &runtime.binding(),
                    runtime.now_ms()
                )
                .is_err()
        );
        assert_eq!(before, std::fs::read(store.path()).unwrap());
        assert_eq!(runtime.0.reboots.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn real_actor_reply_loss_keeps_durable_network_cooldown() {
    let lost = crate::module_radio::tests::tests::real_lost_reboot_receipt();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let path = root("real-lost-actor-receipt");
        let owner = Service::new(&path);
        let adapter = Fake::new(vec![], false);
        *adapter.0.reboot_error.lock().unwrap() =
            Some((lost.code.to_owned(), lost.command_submitted()));
        let operation = owner
            .start("actual-lost-reply", Trigger::Reboot, adapter.clone())
            .await
            .unwrap();
        let done = finished(&owner, &operation.operation_id).await;
        assert_eq!(done.outcome.as_deref(), Some("unknown"));
        assert_eq!(done.submission, Submission::Unknown);
        assert!(done.cooldown_until_ms.unwrap() > adapter.now_ms());
        owner.begin_shutdown();
        owner.finish_pending().await.unwrap();
        let reopened = Service::new(&path);
        let next_runtime = Fake::new(vec![], false);
        let next = reopened
            .start("after-reopen", Trigger::Reboot, next_runtime.clone())
            .await
            .unwrap();
        let next_done = finished(&reopened, &next.operation_id).await;
        assert_eq!(next_done.outcome.as_deref(), Some("cooldown"));
        assert_eq!(next_runtime.0.reboots.load(Ordering::SeqCst), 0);
        reopened.begin_shutdown();
        reopened.finish_pending().await.unwrap();
    });
}
