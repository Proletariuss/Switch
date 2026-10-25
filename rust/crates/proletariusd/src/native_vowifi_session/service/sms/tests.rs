use super::*;
use crate::native_vowifi_session::ordinary::sms_binding_test_support::Facts;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type CaptureHook = Box<dyn FnOnce() + Send>;
struct Identity {
    value: Mutex<intent::ExpectedIdentity>,
    calls: AtomicUsize,
    hook: Mutex<Option<(usize, CaptureHook)>>,
}
impl IdentitySource for Identity {
    fn capture(&self) -> Result<intent::ExpectedIdentity, String> {
        let value = self.value.lock().unwrap().clone();
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = {
            let mut pending = self.hook.lock().unwrap();
            if pending.as_ref().is_some_and(|(at, _)| *at == call) {
                pending.take().map(|(_, hook)| hook)
            } else {
                None
            }
        };
        if let Some(hook) = hook {
            hook();
        }
        Ok(value)
    }
}
struct RetainedTask;
impl intent::Driver for RetainedTask {
    fn check_identity(&self, _: &intent::ExpectedIdentity) -> Result<(), String> {
        Ok(())
    }
    fn run(
        self: Arc<Self>,
        _: intent::Attempt,
        mut stop: watch::Receiver<bool>,
        _: watch::Sender<intent::AttemptProgress>,
    ) -> intent::DriverFuture<intent::AttemptExit> {
        Box::pin(async move {
            while !*stop.borrow_and_update() {
                if stop.changed().await.is_err() {
                    break;
                }
            }
            intent::AttemptExit {
                reason: intent::EndReason::Stopped,
                cleanup_confirmed: true,
            }
        })
    }
    fn await_cleanup(
        self: Arc<Self>,
        _: intent::Attempt,
    ) -> intent::DriverFuture<Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// This exercises production Record/LiveDriver/Handle/identity checks with
/// seeded registered facts. It intentionally does not register or send SMS.
struct Fixture {
    service: Service,
    identity: Arc<Identity>,
    module: Arc<AtomicBool>,
    facts: Vec<Facts>,
    records: Vec<Arc<Record>>,
    progress: Vec<watch::Sender<intent::Snapshot>>,
}
fn generation(n: u64) -> SessionGeneration {
    SessionGeneration::from_value(n).unwrap()
}
impl Fixture {
    fn new() -> Self {
        let identity = Arc::new(Identity {
            value: Mutex::new(intent::ExpectedIdentity {
                reader: "sms-binding-reader".into(),
                reader_generation: 91,
                iccid: "894433000000000001".into(),
                imsi: "234330000000001".into(),
            }),
            calls: AtomicUsize::new(0),
            hook: Mutex::new(None),
        });
        let module = Arc::new(AtomicBool::new(false));
        let claimed = module.clone();
        let pcsc = crate::reader_pcsc::Owner::with_factory(
            "sms-binding-native-producer".into(),
            Arc::new(|_| panic!("SMS capture issued physical open")),
            Arc::new(|| panic!("SMS capture enumerated hardware")),
            Arc::new(|_, _| panic!("SMS capture reset a card")),
        );
        let data_root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "tmp/iteration-485-sms-capability-{}",
            new_epoch().unwrap()
        ));
        let resources = Resources {
            legacy_module_claim: Arc::new(move || claimed.load(Ordering::SeqCst)),
            ordinary_admission: Arc::new(crate::ordinary_vowifi_admission::Gate::default()),
            identity: identity.clone(),
            lifecycle: Weak::new(),
            gate: Arc::new(crate::reader_reservation::Gate::default()),
            pcsc,
            // An attempted reverse-Go request would not find a server here.
            socket: data_root.join("no-go-server.sock"),
            token: None,
            data_root: data_root.clone(),
            transactions: Default::default(),
            calls: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(data_root.join("archive.json")),
                "sms-binding-test".into(),
            ),
        };
        let service = Service(Arc::new(Inner {
            sms_epoch: new_epoch().unwrap(),
            resources,
            state: Mutex::new(State::default()),
            mutation: tokio::sync::Mutex::new(()),
        }));
        let mut fixture = Self {
            service,
            identity,
            module,
            facts: vec![],
            records: vec![],
            progress: vec![],
        };
        let record = fixture.make_record(1, generation(7));
        fixture.service.0.state.lock().unwrap().record = Some(record);
        fixture
    }
    fn make_record(&mut self, id: u64, native_generation: SessionGeneration) -> Arc<Record> {
        let expected = self.identity.value.lock().unwrap().clone();
        let facts = Facts::registered(
            &expected.reader,
            expected.reader_generation,
            native_generation,
        );
        let driver = Arc::new(LiveDriver {
            ordinary_claim: None,
            resources: self.service.0.resources.clone(),
            current: Mutex::new(AttemptState {
                handle: Some(facts.handle.clone()),
                ..Default::default()
            }),
        });
        let task = intent::spawn(
            Arc::new(RetainedTask),
            id,
            expected.clone(),
            intent::FrozenRequest {
                proxy: "never-used".into(),
                epdg: "never-resolved.invalid".into(),
                epdg_port: 500,
            },
            intent::Policy::default(),
        )
        .unwrap();
        let (progress, snapshot) = watch::channel(intent::Snapshot {
            intent_id: id,
            attempt: 0,
            generation: Some(native_generation),
            phase: intent::Phase::Registered,
            last_error: None,
            last_end: None,
            retry_at: None,
            cleanup_confirmed: false,
        });
        let record = Arc::new(Record {
            id,
            expected,
            driver,
            stop: task.stop_sender(),
            progress: snapshot,
            task: tokio::sync::Mutex::new(task),
            claim: self
                .service
                .0
                .resources
                .ordinary_admission
                .acquire_native(&self.service.0.sms_epoch, id)
                .unwrap(),
            observer: Mutex::new(None),
            operation: Mutex::new(crate::native_vowifi_session::operation::Clock::start(
                tokio::time::Instant::now(),
                std::time::Duration::from_secs(90),
            )),
        });
        self.facts.push(facts);
        self.progress.push(progress);
        self.records.push(record.clone());
        record
    }
    fn current(&self) -> Arc<Record> {
        self.service.record().unwrap().unwrap()
    }
    fn token(&self) -> String {
        self.service
            .sms_binding()
            .ok()
            .expect("registered capability")
            .binding
    }
    fn assert_rejected(&mut self, token: &str) {
        assert!(self.service.sms_binding().is_err());
        assert!(self.service.resolve_sms_binding(token).is_err());
        assert!(
            self.service.has_intent(),
            "a stale retained intent must not become a Go fallback"
        );
        self.assert_quiet();
    }
    fn assert_quiet(&mut self) {
        for facts in &mut self.facts {
            facts.assert_no_command();
        }
        assert!(!self.service.0.resources.gate.native_reader_session_active());
        assert!(
            !self
                .service
                .0
                .resources
                .data_root
                .join("archive.json")
                .exists()
        );
    }
    async fn finish(mut self) {
        self.assert_quiet();
        for record in &self.records {
            record.request_stop();
        }
        for record in &self.records {
            record
                .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
                .await
                .unwrap();
            assert!(record.task.lock().await.is_finished());
        }
    }
}

#[test]
fn capability_changes_across_restart_intent_and_ims_rebuild_without_identity_disclosure() {
    let first = new_epoch().unwrap();
    let restart = new_epoch().unwrap();
    assert_eq!(first.len(), 32);
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    let original = capability(&first, 1, generation(7));
    assert_ne!(original, capability(&restart, 1, generation(7)));
    assert_ne!(original, capability(&first, 2, generation(7)));
    assert_ne!(original, capability(&first, 1, generation(8)));
    let largest = capability(&first, u64::MAX, generation(u64::MAX));
    assert_eq!(largest.len(), 74);
    assert!(largest.len() <= 96);
    assert!(
        largest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b':')
    );
}

#[tokio::test]
async fn registered_record_captures_stable_private_token_subscription_and_exact_control_channel_without_sending()
 {
    let mut f = Fixture::new();
    let binding = f.service.sms_binding().ok().unwrap();
    assert_eq!(binding.generation, generation(7));
    assert_eq!(
        binding.subscription_id,
        native_device_sms_subscription_id("894433000000000001", "234330000000001").unwrap()
    );
    assert!(binding.subscription_id.starts_with("sub1_"));
    assert!(binding.controls.same_channel(&f.facts[0].handle.controls));
    assert!(!binding.binding.contains("sms-binding-reader"));
    assert!(!binding.binding.contains("894433000000000001"));
    assert!(!binding.binding.contains("234330000000001"));
    let resolved = f
        .service
        .resolve_sms_binding(&binding.binding)
        .ok()
        .unwrap();
    assert_eq!(resolved.binding, binding.binding);
    assert_eq!(resolved.subscription_id, binding.subscription_id);
    assert!(resolved.controls.same_channel(&binding.controls));
    f.assert_quiet();
    f.finish().await;
}

#[tokio::test]
async fn current_reader_physical_generation_and_sim_identity_changes_expire_capture_and_resolution()
{
    let mut f = Fixture::new();
    let token = f.token();
    let original = f.identity.value.lock().unwrap().clone();
    for kind in 0..4 {
        let mut changed = original.clone();
        match kind {
            0 => changed.reader.push_str("-replacement"),
            1 => changed.reader_generation += 1,
            2 => changed.iccid = "894433000000000002".into(),
            _ => changed.imsi = "234330000000002".into(),
        }
        *f.identity.value.lock().unwrap() = changed;
        f.assert_rejected(&token);
    }
    *f.identity.value.lock().unwrap() = original;
    assert!(f.service.resolve_sms_binding(&token).is_ok());
    f.finish().await;
}

#[tokio::test]
async fn renewal_not_ready_registered_loss_and_drain_never_admit_sms_work() {
    let mut f = Fixture::new();
    let token = f.token();
    let original = f.facts[0].handle.snapshot();
    for kind in 0..4 {
        f.facts[0].update(|s| {
            *s = original.clone();
            match kind {
                0 => s.renewal_due = true,
                1 => s.sms_ready = false,
                2 => s.registered = false,
                _ => s.phase = ordinary::Phase::Draining,
            }
        });
        f.assert_rejected(&token);
    }
    f.finish().await;
}

#[tokio::test]
async fn snapshot_reader_physical_or_native_generation_disagreement_is_rejected() {
    let mut f = Fixture::new();
    let token = f.token();
    for kind in 0..3 {
        let (reader, physical, native) = match kind {
            0 => ("different-reader", 91, generation(7)),
            1 => ("sms-binding-reader", 92, generation(7)),
            _ => ("sms-binding-reader", 91, generation(8)),
        };
        let mut facts = Facts::registered(reader, physical, native);
        // For the third case only the snapshot has the wrong generation.
        facts.handle.generation = generation(7);
        f.current().driver.current.lock().unwrap().handle = Some(facts.handle.clone());
        f.facts.push(facts);
        f.assert_rejected(&token);
    }
    f.finish().await;
}

#[tokio::test]
async fn stop_intent_phase_and_module_resource_claim_fail_closed_without_go_fallback() {
    let mut f = Fixture::new();
    let token = f.token();
    f.module.store(true, Ordering::SeqCst);
    f.assert_rejected(&token);
    f.module.store(false, Ordering::SeqCst);
    let original = f.progress[0].borrow().clone();
    for phase in [
        intent::Phase::Starting,
        intent::Phase::Backoff,
        intent::Phase::Draining,
        intent::Phase::CleanupUnconfirmed,
        intent::Phase::Failed,
        intent::Phase::Stopped,
        intent::Phase::ReaderChanged,
    ] {
        f.progress[0].send_modify(|s| s.phase = phase);
        f.assert_rejected(&token);
    }
    f.progress[0].send_replace(original);
    f.current().request_stop();
    f.assert_rejected(&token);
    f.finish().await;
}

#[tokio::test]
async fn intent_native_generation_and_new_service_epoch_cannot_reuse_an_old_ledger_capability() {
    let mut f = Fixture::new();
    let first = f.token();
    f.current().request_stop();
    f.current()
        .finish_before(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    let replacement = f.make_record(2, generation(7));
    f.service.0.state.lock().unwrap().record = Some(replacement);
    let second = f.token();
    assert_ne!(first, second);
    assert!(f.service.resolve_sms_binding(&first).is_err());
    let replacement = f.make_record(2, generation(8));
    f.service.0.state.lock().unwrap().record = Some(replacement);
    let third = f.token();
    assert_ne!(second, third);
    assert!(f.service.resolve_sms_binding(&second).is_err());
    let restarted = Service(Arc::new(Inner {
        sms_epoch: new_epoch().unwrap(),
        resources: f.service.0.resources.clone(),
        state: Mutex::new(State {
            record: Some(f.current()),
            ..Default::default()
        }),
        mutation: tokio::sync::Mutex::new(()),
    }));
    assert!(restarted.resolve_sms_binding(&third).is_err());
    assert_ne!(restarted.sms_binding().ok().unwrap().binding, third);
    f.assert_quiet();
    f.finish().await;
}

#[tokio::test]
async fn retained_record_replaced_during_identity_capture_cannot_publish_its_old_capability() {
    let mut f = Fixture::new();
    let original = f.token();
    // A rebuilt IMS generation remains inside the same intent claim. A new
    // intent would first require the old task's confirmed release.
    let replacement = f.make_record(1, generation(8));
    let inner = Arc::downgrade(&f.service.0);
    let at = f.identity.calls.load(Ordering::SeqCst) + 2;
    *f.identity.hook.lock().unwrap() = Some((
        at,
        Box::new(move || {
            inner.upgrade().unwrap().state.lock().unwrap().record = Some(replacement);
        }),
    ));
    assert!(f.service.sms_binding().is_err());
    assert!(f.service.resolve_sms_binding(&original).is_err());
    assert!(f.service.has_intent());
    f.finish().await;
}

#[tokio::test]
async fn unavailable_and_foreign_or_malformed_tokens_never_create_an_owner() {
    let mut f = Fixture::new();
    let valid = f.token();
    for token in [
        "",
        "go:legacy",
        "reader:91",
        "arbitrary",
        &format!("{valid}:trailing"),
    ] {
        assert!(f.service.resolve_sms_binding(token).is_err());
    }
    f.service.0.state.lock().unwrap().record = None;
    assert_eq!(
        f.service.sms_binding().err(),
        Some("native_sms_session_unavailable")
    );
    assert!(f.service.resolve_sms_binding(&valid).is_err());
    assert!(!f.service.has_intent());
    f.finish().await;
}

#[tokio::test]
async fn reader_observation_keeps_subscription_while_renewing_but_not_sms_authority() {
    let mut f = Fixture::new();
    let initial = f.service.reader_observation().unwrap();
    assert!(initial.identity_valid && initial.session_active && initial.sms_ready);
    let subscription = initial.subscription_id.unwrap();
    f.facts[0].update(|snapshot| {
        snapshot.renewal_due = true;
        snapshot.sms_ready = false;
    });
    let renewing = f.service.reader_observation().unwrap();
    assert!(renewing.identity_valid && renewing.session_active);
    assert!(!renewing.sms_ready);
    assert_eq!(renewing.status["sms_ready"], false);
    assert_eq!(
        renewing.subscription_id.as_deref(),
        Some(subscription.as_str())
    );
    assert!(f.service.sms_binding().is_err());
    f.progress[0].send_modify(|snapshot| snapshot.generation = Some(generation(8)));
    let inconsistent = f.service.reader_observation().unwrap();
    assert!(!inconsistent.sms_ready);
    assert_eq!(
        inconsistent.subscription_id.as_deref(),
        Some(subscription.as_str())
    );
    f.current().request_stop();
    let stopping = f.service.reader_observation().unwrap();
    assert!(!stopping.sms_ready);
    f.assert_quiet();
    f.finish().await;
}

#[tokio::test]
async fn observation_rechecks_exact_record_after_binding_resolves_replacement_during_status_capture()
 {
    let mut f = Fixture::new();
    let old = f.current();
    old.claim.release_confirmed().unwrap();
    let replacement = f.make_record(2, generation(8));
    let inner = Arc::downgrade(&f.service.0);
    let call = f.identity.calls.load(Ordering::SeqCst) + 1;
    *f.identity.hook.lock().unwrap() = Some((
        call,
        Box::new(move || {
            inner.upgrade().unwrap().state.lock().unwrap().record = Some(replacement);
        }),
    ));
    // Status began with the old record, but SMS binding can now resolve the
    // new generation. The combined observation must publish neither one.
    let observation = f.service.reader_observation().unwrap();
    assert_eq!(observation.status["available"], false);
    assert!(!observation.sms_ready && !observation.identity_valid);
    assert!(observation.subscription_id.is_none());
    assert!(observation.sim_id.is_empty());
    f.assert_quiet();
    f.finish().await;
}

#[tokio::test]
async fn imsi_changed_after_status_capture_clears_registered_status_even_with_same_iccid() {
    let mut f = Fixture::new();
    let identity = f.identity.clone();
    let call = identity.calls.load(Ordering::SeqCst) + 1;
    *f.identity.hook.lock().unwrap() = Some((
        call,
        Box::new(move || {
            identity.value.lock().unwrap().imsi = "234330000000002".into();
        }),
    ));
    let observation = f.service.reader_observation().unwrap();
    assert!(!observation.identity_valid && !observation.sms_ready);
    assert_eq!(observation.status["available"], false);
    assert_eq!(observation.status["tunnel_state"], "disconnected");
    assert_eq!(observation.status["ims_state"], "idle");
    assert_eq!(observation.status["call_ready"], false);
    assert!(observation.subscription_id.is_none());
    f.assert_quiet();
    f.finish().await;
}

#[tokio::test]
async fn poisoned_admission_keeps_retirement_unknown_but_rejects_ready_calls_and_sms_capabilities()
{
    let mut f = Fixture::new();
    let token = f.token();
    let record = f.current();
    assert!(
        f.service
            .current_binding(record.expected.reader_generation)
            .is_ok()
    );
    let gate = f.service.0.resources.ordinary_admission.clone();
    gate.poison_for_test();
    // Retirement is conservative, while positive authority must be proven.
    assert!(record.claim.is_current());
    assert!(record.claim.try_is_current().is_err());
    let calls_rejected = f
        .service
        .current_binding(record.expected.reader_generation)
        .is_err();
    let sms_rejected = f.service.sms_binding().is_err();
    let token_rejected = f.service.resolve_sms_binding(&token).is_err();
    let observation = f.service.reader_observation().unwrap();
    gate.clear_test_poison();
    assert!(calls_rejected && sms_rejected && token_rejected);
    assert_eq!(observation.status["available"], false);
    assert!(!observation.sms_ready && !observation.identity_valid);
    assert!(
        f.service.resolve_sms_binding(&token).is_ok(),
        "test-only poison repair must not change owner/capability"
    );
    f.assert_quiet();
    f.finish().await;
}
