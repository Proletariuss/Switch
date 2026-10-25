use super::*;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

struct Plan {
    reason: EndReason,
    confirmed: bool,
    wait: Option<Arc<Notify>>,
    cleanup: Option<Arc<Notify>>,
    wait_stop: bool,
    panic: bool,
}
impl Plan {
    fn done(reason: EndReason) -> Self {
        Self {
            reason,
            confirmed: true,
            wait: None,
            cleanup: None,
            wait_stop: false,
            panic: false,
        }
    }
}
struct Fake {
    identity: Mutex<ExpectedIdentity>,
    plans: Mutex<VecDeque<Plan>>,
    attempts: Mutex<Vec<Attempt>>,
    live: Mutex<Option<u32>>,
    cleanup: Mutex<HashMap<u32, Arc<Notify>>>,
    cleanup_calls: AtomicUsize,
    cleanup_errors: AtomicUsize,
    cleanup_panics: AtomicUsize,
    cancel_seen: AtomicBool,
    identity_panics: AtomicBool,
}
impl Fake {
    fn new(plans: Vec<Plan>) -> Arc<Self> {
        Arc::new(Self {
            identity: Mutex::new(expected()),
            plans: Mutex::new(plans.into()),
            attempts: Mutex::new(vec![]),
            live: Mutex::new(None),
            cleanup: Mutex::new(HashMap::new()),
            cleanup_calls: AtomicUsize::new(0),
            cleanup_errors: AtomicUsize::new(0),
            cleanup_panics: AtomicUsize::new(0),
            cancel_seen: AtomicBool::new(false),
            identity_panics: AtomicBool::new(false),
        })
    }
    fn count(&self) -> usize {
        self.attempts.lock().unwrap().len()
    }
}
impl Driver for Fake {
    fn check_identity(&self, identity: &ExpectedIdentity) -> Result<(), String> {
        assert!(
            !self.identity_panics.load(Ordering::SeqCst),
            "identity observer panic"
        );
        if *self.identity.lock().unwrap() == *identity {
            Ok(())
        } else {
            Err("reader_identity_changed".into())
        }
    }
    fn run(
        self: Arc<Self>,
        attempt: Attempt,
        mut stop: watch::Receiver<bool>,
        progress: watch::Sender<AttemptProgress>,
    ) -> DriverFuture<AttemptExit> {
        Box::pin(async move {
            let plan = self
                .plans
                .lock()
                .unwrap()
                .pop_front()
                .expect("no unexpected retry");
            assert!(
                self.live.lock().unwrap().replace(attempt.number).is_none(),
                "new owner before actual release"
            );
            self.attempts.lock().unwrap().push(attempt.clone());
            if let Some(cleanup) = plan.cleanup {
                self.cleanup.lock().unwrap().insert(attempt.number, cleanup);
            }
            progress.send_replace(AttemptProgress::Registered(
                SessionGeneration::from_value(attempt.number as u64 + 1).unwrap(),
            ));
            if plan.panic {
                panic!("submitted owner panic");
            }
            let mut saw_stop = false;
            if let Some(wait) = plan.wait {
                loop {
                    tokio::select! {
                        _=wait.notified()=>break,
                        _=stop.changed(),if !saw_stop=>{saw_stop=stopped(&stop);if saw_stop {self.cancel_seen.store(true,Ordering::SeqCst);}},
                    }
                }
            } else if plan.wait_stop {
                while !stopped(&stop) {
                    let _ = stop.changed().await;
                }
                self.cancel_seen.store(true, Ordering::SeqCst);
            }
            if plan.confirmed {
                assert_eq!(self.live.lock().unwrap().take(), Some(attempt.number));
            }
            AttemptExit {
                reason: plan.reason,
                cleanup_confirmed: plan.confirmed,
            }
        })
    }
    fn await_cleanup(self: Arc<Self>, attempt: Attempt) -> DriverFuture<Result<(), String>> {
        Box::pin(async move {
            self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
            if self
                .cleanup_panics
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
                .is_ok()
            {
                panic!("cleanup observer panic");
            }
            if self
                .cleanup_errors
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
                .is_ok()
            {
                return Err("release_not_confirmed".into());
            }
            let gate = self
                .cleanup
                .lock()
                .unwrap()
                .get(&attempt.number)
                .cloned()
                .expect("captured cleanup owner");
            gate.notified().await;
            assert_eq!(self.live.lock().unwrap().take(), Some(attempt.number));
            Ok(())
        })
    }
}
fn expected() -> ExpectedIdentity {
    ExpectedIdentity {
        reader: "private-reader".into(),
        reader_generation: 7,
        iccid: "8901234567890123456".into(),
        imsi: "234100000000001".into(),
    }
}
fn request() -> FrozenRequest {
    FrozenRequest {
        proxy: "socks5h://secret-user:secret-password@127.0.0.1:1080".into(),
        epdg: "custom.epdg.invalid".into(),
        epdg_port: 5443,
    }
}
fn policy() -> Policy {
    Policy {
        initial_delay: Duration::from_millis(10),
        maximum_delay: Duration::from_millis(40),
        jitter: Duration::ZERO,
        identity_poll: Duration::from_millis(2),
        cleanup_retry: Duration::from_millis(2),
    }
}
fn start(fake: Arc<Fake>) -> Handle {
    spawn(fake, 31, expected(), request(), policy()).unwrap()
}
async fn phase(handle: &Handle, wanted: Phase) {
    let mut observed = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if observed.borrow().phase == wanted {
                break;
            }
            observed.changed().await.expect("retained intent status");
        }
    })
    .await
    .unwrap();
}
async fn finish(handle: &mut Handle) -> Exit {
    tokio::time::timeout(Duration::from_secs(2), handle.finish_cancel_safe())
        .await
        .unwrap()
        .unwrap()
}
fn lost() -> EndReason {
    EndReason::RuntimeLost {
        error: "tunnel_lost".into(),
        retryable: true,
    }
}

#[test]
fn default_backoff_matches_go_and_optional_jitter_is_bounded() {
    let p = Policy::default();
    for n in [0, 1, 2, 3, 4, 5, 100, u32::MAX] {
        assert_eq!(
            p.delay(n).unwrap(),
            Duration::from_secs(proletarius_vowifi::reconnect_delay_seconds(n))
        );
    }
    let p = Policy {
        jitter: Duration::from_millis(500),
        ..p
    };
    for n in 1..20 {
        let base = Duration::from_secs(proletarius_vowifi::reconnect_delay_seconds(n));
        let actual = p.delay(n).unwrap();
        assert!(actual <= base && actual >= base - Duration::from_millis(500));
    }
}
#[test]
fn debug_never_contains_sim_reader_or_proxy_credentials() {
    let text = format!(
        "{:?} {:?} {:?}",
        expected(),
        request(),
        Attempt {
            intent_id: 31,
            number: 2,
            expected: expected(),
            request: request()
        }
    );
    for secret in [
        "private-reader",
        "8901234567890123456",
        "234100000000001",
        "secret-user",
        "secret-password",
        "custom.epdg.invalid",
    ] {
        assert!(!text.contains(secret));
    }
}
#[tokio::test]
async fn failed_session_reopens_only_after_exact_release_and_keeps_requested_configuration() {
    let gate = Arc::new(Notify::new());
    let mut first = Plan::done(lost());
    first.confirmed = false;
    first.cleanup = Some(gate.clone());
    let fake = Fake::new(vec![first, Plan::done(EndReason::Stopped)]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::CleanupUnconfirmed).await;
    assert_eq!(fake.count(), 1);
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    assert!(!handle.snapshot().cleanup_confirmed);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), handle.finish_cancel_safe())
            .await
            .is_err()
    );
    assert_eq!(fake.count(), 1);
    gate.notify_one();
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::Stopped);
    assert!(exit.snapshot.cleanup_confirmed);
    let attempts = fake.attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].number, 0);
    assert_eq!(attempts[1].number, 1);
    for a in attempts.iter() {
        assert_eq!(a.intent_id, 31);
        assert_eq!(a.expected, expected());
        assert_eq!(a.request, request());
    }
}
#[tokio::test]
async fn cancellation_of_finish_waiter_does_not_cancel_or_drop_live_worker() {
    let gate = Arc::new(Notify::new());
    let mut plan = Plan::done(EndReason::Stopped);
    plan.wait = Some(gate.clone());
    let fake = Fake::new(vec![plan]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::Registered).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), handle.finish_cancel_safe())
            .await
            .is_err()
    );
    assert!(!fake.cancel_seen.load(Ordering::SeqCst));
    assert!(!handle.is_finished());
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    gate.notify_one();
    finish(&mut handle).await;
    assert_eq!(fake.count(), 1);
}
#[tokio::test]
async fn user_stop_waits_actual_worker_exit_and_cannot_rebuild_planned_reauth() {
    let gate = Arc::new(Notify::new());
    let mut plan = Plan::done(EndReason::PlannedReauthentication);
    plan.wait = Some(gate.clone());
    let fake = Fake::new(vec![plan]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::Registered).await;
    handle.stop_sender().send_replace(true);
    phase(&handle, Phase::Draining).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), handle.finish_cancel_safe())
            .await
            .is_err()
    );
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    assert!(!handle.is_finished());
    gate.notify_one();
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::Stopped);
    assert_eq!(fake.count(), 1);
    assert_eq!(
        exit.snapshot.last_end,
        Some(EndReason::PlannedReauthentication)
    );
}
#[tokio::test]
async fn stop_during_unknown_cleanup_retains_exact_cleanup_future_until_ack() {
    let gate = Arc::new(Notify::new());
    let mut plan = Plan::done(lost());
    plan.confirmed = false;
    plan.cleanup = Some(gate.clone());
    let fake = Fake::new(vec![plan]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::CleanupUnconfirmed).await;
    handle.request_stop();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), handle.finish_cancel_safe())
            .await
            .is_err()
    );
    assert_eq!(fake.cleanup_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    gate.notify_one();
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::Stopped);
    assert!(exit.snapshot.cleanup_confirmed);
    assert_eq!(fake.count(), 1);
}
#[tokio::test]
async fn changing_card_or_reader_generation_cancels_and_drains_without_replacement() {
    for change in 0..3 {
        let gate = Arc::new(Notify::new());
        let mut plan = Plan::done(lost());
        plan.wait = Some(gate.clone());
        let fake = Fake::new(vec![plan]);
        let mut handle = start(fake.clone());
        phase(&handle, Phase::Registered).await;
        {
            let mut i = fake.identity.lock().unwrap();
            match change {
                0 => i.reader_generation += 1,
                1 => i.iccid = "8909999999999999999".into(),
                _ => i.imsi = "234109999999999".into(),
            };
        }
        phase(&handle, Phase::Draining).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), handle.finish_cancel_safe())
                .await
                .is_err()
        );
        gate.notify_one();
        let exit = finish(&mut handle).await;
        assert_eq!(exit.snapshot.phase, Phase::ReaderChanged);
        assert_eq!(fake.count(), 1);
        assert!(fake.live.lock().unwrap().is_none());
    }
}
#[tokio::test]
async fn changing_identity_during_backoff_never_starts_the_replacement_sim() {
    let fake = Fake::new(vec![Plan::done(lost())]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::Backoff).await;
    fake.identity.lock().unwrap().reader_generation += 1;
    assert_eq!(
        finish(&mut handle).await.snapshot.phase,
        Phase::ReaderChanged
    );
    assert_eq!(fake.count(), 1);
}
#[tokio::test]
async fn renewal_and_planned_reauth_are_distinct_and_each_reopens_after_release() {
    let fake = Fake::new(vec![
        Plan::done(EndReason::RegistrationRenewal),
        Plan::done(EndReason::PlannedReauthentication),
        Plan::done(EndReason::Stopped),
    ]);
    let mut handle = start(fake.clone());
    let mut observed = handle.subscribe();
    let mut seen = vec![];
    tokio::time::timeout(Duration::from_secs(2), async {
        while !handle.is_finished() {
            if let Some(reason) = observed.borrow().last_end.clone() {
                if !seen.contains(&reason) {
                    seen.push(reason);
                }
            }
            let _ = observed.changed().await;
        }
    })
    .await
    .unwrap();
    finish(&mut handle).await;
    assert!(seen.contains(&EndReason::RegistrationRenewal));
    assert!(seen.contains(&EndReason::PlannedReauthentication));
    assert_eq!(fake.count(), 3);
}
#[tokio::test]
async fn startup_failure_is_terminal_and_remains_observable_without_retry() {
    let fake = Fake::new(vec![Plan::done(EndReason::StartupFailed {
        error: "isim_unavailable".into(),
    })]);
    let mut handle = start(fake.clone());
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::Failed);
    assert_eq!(
        exit.snapshot.last_error.as_deref(),
        Some("isim_unavailable")
    );
    assert!(exit.snapshot.cleanup_confirmed);
    assert_eq!(fake.count(), 1);
    assert_eq!(
        handle.finish_cancel_safe().await.unwrap().snapshot.phase,
        Phase::Failed
    );
}
#[tokio::test]
async fn permanent_runtime_failure_does_not_repeat_authentication_or_commands() {
    let fake = Fake::new(vec![Plan::done(EndReason::RuntimeLost {
        error: "configuration_invalid".into(),
        retryable: false,
    })]);
    let mut handle = start(fake.clone());
    assert_eq!(finish(&mut handle).await.snapshot.phase, Phase::Failed);
    assert_eq!(fake.count(), 1);
}
#[tokio::test]
async fn stop_racing_backoff_deadline_prevents_any_later_reconnect() {
    for _ in 0..20 {
        let fake = Fake::new(vec![Plan::done(EndReason::RegistrationRenewal)]);
        let mut handle = start(fake.clone());
        phase(&handle, Phase::Backoff).await;
        handle.request_stop();
        let exit = finish(&mut handle).await;
        assert_eq!(exit.snapshot.phase, Phase::Stopped);
        assert_eq!(fake.count(), 1);
    }
}
#[tokio::test]
async fn dropping_control_owner_only_signals_stop_and_detaches_actual_drain() {
    let gate = Arc::new(Notify::new());
    let mut plan = Plan::done(lost());
    plan.wait = Some(gate.clone());
    let fake = Fake::new(vec![plan]);
    let handle = start(fake.clone());
    let mut state = handle.subscribe();
    phase(&handle, Phase::Registered).await;
    drop(handle);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.borrow().phase == Phase::Draining {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.borrow().phase == Phase::Stopped {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(fake.live.lock().unwrap().is_none());
    assert_eq!(fake.count(), 1);
}
#[tokio::test]
async fn panic_after_submission_and_cleanup_observer_panic_never_forge_release() {
    let gate = Arc::new(Notify::new());
    let mut plan = Plan::done(lost());
    plan.panic = true;
    plan.confirmed = false;
    plan.cleanup = Some(gate.clone());
    let fake = Fake::new(vec![plan]);
    fake.cleanup_panics.store(1, Ordering::SeqCst);
    fake.cleanup_errors.store(1, Ordering::SeqCst);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::CleanupUnconfirmed).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), handle.finish_cancel_safe())
            .await
            .is_err()
    );
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    assert_eq!(fake.count(), 1);
    assert!(!handle.snapshot().cleanup_confirmed);
    assert!(fake.cleanup_calls.load(Ordering::SeqCst) >= 3);
    gate.notify_one();
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::Failed);
    assert!(exit.snapshot.cleanup_confirmed);
}

#[tokio::test]
async fn identity_observer_panic_while_registered_still_waits_actual_worker_drain() {
    let gate = Arc::new(Notify::new());
    let mut plan = Plan::done(lost());
    plan.wait = Some(gate.clone());
    let fake = Fake::new(vec![plan]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::Registered).await;
    fake.identity_panics.store(true, Ordering::SeqCst);
    phase(&handle, Phase::Draining).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), handle.finish_cancel_safe())
            .await
            .is_err()
    );
    assert_eq!(*fake.live.lock().unwrap(), Some(0));
    gate.notify_one();
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::ReaderChanged);
    assert!(exit.snapshot.cleanup_confirmed);
    assert_eq!(fake.count(), 1);
}

#[tokio::test]
async fn transient_rebuild_startup_preserves_identity_and_retries_after_cleanup_only() {
    let gate = Arc::new(Notify::new());
    let mut interrupted = Plan::done(EndReason::StartupTransient {
        error: "transport_timeout".into(),
    });
    interrupted.confirmed = false;
    interrupted.cleanup = Some(gate.clone());
    let fake = Fake::new(vec![
        Plan::done(lost()),
        interrupted,
        Plan::done(EndReason::Stopped),
    ]);
    let mut handle = start(fake.clone());
    phase(&handle, Phase::CleanupUnconfirmed).await;
    assert_eq!(fake.count(), 2);
    assert_eq!(*fake.live.lock().unwrap(), Some(1));
    // A pending cleanup observer has not granted the next generation even
    // though the transport outcome is explicitly transient.
    assert!(!handle.is_finished());
    gate.notify_one();
    let exit = finish(&mut handle).await;
    assert_eq!(exit.snapshot.phase, Phase::Stopped);
    assert!(exit.snapshot.cleanup_confirmed);
    let attempts = fake.attempts.lock().unwrap();
    assert_eq!(attempts.len(), 3);
    for (index, attempt) in attempts.iter().enumerate() {
        assert_eq!(attempt.number as usize, index);
        assert_eq!(attempt.intent_id, 31);
        assert_eq!(attempt.expected, expected());
        assert_eq!(attempt.request, request());
    }
    // The Driver only rebuilds a session; it has no API for replaying any SMS,
    // USSD or call command. Its immutable request contains only proxy/ePDG.
}

#[tokio::test]
async fn transient_startup_stop_or_identity_change_during_cleanup_prevents_rebuild() {
    for change_identity in [false, true] {
        let gate = Arc::new(Notify::new());
        let mut interrupted = Plan::done(EndReason::StartupTransient {
            error: "transport_timeout".into(),
        });
        interrupted.confirmed = false;
        interrupted.cleanup = Some(gate.clone());
        let fake = Fake::new(vec![interrupted]);
        let mut handle = start(fake.clone());
        phase(&handle, Phase::CleanupUnconfirmed).await;
        if change_identity {
            fake.identity.lock().unwrap().iccid = "replacement".into();
        } else {
            handle.request_stop();
        }
        assert_eq!(*fake.live.lock().unwrap(), Some(0));
        gate.notify_one();
        let exit = finish(&mut handle).await;
        assert_eq!(
            exit.snapshot.phase,
            if change_identity {
                Phase::ReaderChanged
            } else {
                Phase::Stopped
            }
        );
        assert_eq!(fake.count(), 1);
        assert!(fake.live.lock().unwrap().is_none());
    }
}

#[tokio::test]
async fn transient_startup_stop_or_identity_change_during_backoff_prevents_rebuild() {
    for change_identity in [false, true] {
        let fake = Fake::new(vec![Plan::done(EndReason::StartupTransient {
            error: "transport_timeout".into(),
        })]);
        let mut handle = start(fake.clone());
        phase(&handle, Phase::Backoff).await;
        if change_identity {
            fake.identity.lock().unwrap().reader_generation += 1;
        } else {
            handle.request_stop();
        }
        let exit = finish(&mut handle).await;
        assert_eq!(
            exit.snapshot.phase,
            if change_identity {
                Phase::ReaderChanged
            } else {
                Phase::Stopped
            }
        );
        assert_eq!(fake.count(), 1);
    }
}
