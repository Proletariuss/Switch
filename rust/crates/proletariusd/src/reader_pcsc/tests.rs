use super::presence::PresenceMonitor;
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[derive(Default)]
struct Trace {
    opens: AtomicUsize,
    closes: AtomicUsize,
    transactions: AtomicUsize,
    calls: Mutex<Vec<Vec<u8>>>,
    resets: Mutex<Vec<bool>>,
    close_fails: AtomicBool,
    close_panics: AtomicBool,
    close_gate: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
    io_gate: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
    io_error: AtomicBool,
    end_error: AtomicBool,
}
struct Fake(Arc<Trace>);
impl Physical for Fake {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = Failure>),
    ) -> Result<(), Failure> {
        self.0.transactions.fetch_add(1, Ordering::SeqCst);
        body(self);
        if self.0.end_error.load(Ordering::SeqCst) {
            Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "fixture_end_failed",
                "Fixture transaction end failed",
            ))
        } else {
            Ok(())
        }
    }
    fn close(&mut self) -> Result<(), Failure> {
        self.0.closes.fetch_add(1, Ordering::SeqCst);
        if let Some((started, wait)) = self.0.close_gate.lock().unwrap().take() {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(
            !self.0.close_panics.load(Ordering::SeqCst),
            "fixture close panic"
        );
        if self.0.close_fails.load(Ordering::SeqCst) {
            Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "fixture_close_failed",
                "Fixture close failed",
            ))
        } else {
            Ok(())
        }
    }
}
impl ApduTransport for Fake {
    type Error = Failure;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Failure> {
        self.0.calls.lock().unwrap().push(command.to_vec());
        if let Some((started, wait)) = self.0.io_gate.lock().unwrap().take() {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.0.io_error.load(Ordering::SeqCst) {
            Err(Failure::unknown())
        } else {
            Ok(vec![0x90, 0])
        }
    }
}
struct Fixture {
    owner: Owner,
    trace: Arc<Trace>,
    reader: String,
}
impl Fixture {
    fn new(reader: &str) -> Self {
        let trace = Arc::new(Trace::default());
        let open_trace = trace.clone();
        let reset_trace = trace.clone();
        let name = reader.to_owned();
        let owner = Owner::with_factory(
            "producer".into(),
            Arc::new(move |_| {
                open_trace.opens.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Fake(open_trace.clone())))
            }),
            Arc::new(move || Ok(vec![name.clone()])),
            Arc::new(move |_, power| {
                reset_trace.resets.lock().unwrap().push(power);
                Ok(Box::new(Fake(reset_trace.clone())))
            }),
        );
        Self {
            owner,
            trace,
            reader: reader.into(),
        }
    }
    fn binding(&self, sequence: u64) -> Binding {
        Binding {
            producer_id: "producer".into(),
            reader: self.reader.clone(),
            session_id: "session".into(),
            lease_id: format!("lease-{sequence}"),
            lease_seq: sequence,
        }
    }
    fn open(&self) {
        self.owner
            .open("producer", &self.reader, "session", 1)
            .unwrap();
    }
    fn acquire(&self, sequence: u64) -> Binding {
        let binding = self.binding(sequence);
        self.owner
            .acquire(&binding, Duration::from_secs(120))
            .unwrap();
        binding
    }
    fn block_io(&self) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (started, ready) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        *self.trace.io_gate.lock().unwrap() = Some((started, wait));
        (ready, release)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.trace.close_fails.store(false, Ordering::SeqCst);
        let _ = self.owner.retire_blocking(None, None, true);
    }
}

#[test]
fn revive_cooldown_reports_remaining_wait_without_relabeling_submitted_io() {
    let now = Instant::now();
    let recent = now - Duration::from_secs(2);
    let remaining = revival_cooldown_remaining(recent, now).unwrap();
    assert!(remaining > Duration::from_secs(2));
    assert!(remaining <= Duration::from_secs(3));
    assert_eq!(revival_cooldown_remaining(now - REVIVE_COOLDOWN, now), None);

    let failure = Failure::recovery_cooldown(Duration::from_millis(1_001));
    assert_eq!(failure.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failure.code, "reader_pcsc_recovery_cooldown");
    assert_eq!(failure.message, "读卡器复位冷却中，请约 2 秒后重试");
    assert!(!failure.submitted);
    assert!(!failure.uncertain);
}

#[test]
fn repeated_unresponsive_open_does_not_reset_again_during_cooldown() {
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let reset_calls = Arc::new(AtomicUsize::new(0));
    let factory_count = factory_calls.clone();
    let reset_count = reset_calls.clone();
    let owner = Owner::with_factory(
        "producer".into(),
        Arc::new(move |_| {
            factory_count.fetch_add(1, Ordering::SeqCst);
            Err(Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_pcsc_unresponsive_card",
                "The selected card is unresponsive",
            ))
        }),
        Arc::new(|| Ok(vec!["Generic reader".into()])),
        Arc::new(move |_, _| {
            reset_count.fetch_add(1, Ordering::SeqCst);
            Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "fixture_reset_failed",
                "Fixture reset failed",
            ))
        }),
    );
    owner
        .open("producer", "Generic reader", "session", 1)
        .unwrap();

    let binding = |sequence| Binding {
        producer_id: "producer".into(),
        reader: "Generic reader".into(),
        session_id: "session".into(),
        lease_id: format!("lease-{sequence}"),
        lease_seq: sequence,
    };
    let first = owner
        .acquire(&binding(1), Duration::from_secs(120))
        .unwrap_err();
    assert_eq!(first.code, "reader_pcsc_unresponsive_card");

    let worker = owner.worker("producer", "Generic reader").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while worker.state.lock().unwrap().lease.is_some() {
        assert!(
            Instant::now() < deadline,
            "first failed lease did not drain"
        );
        thread::yield_now();
    }

    let second = owner
        .acquire(&binding(2), Duration::from_secs(120))
        .unwrap_err();
    assert_eq!(second.code, "reader_pcsc_recovery_cooldown");
    assert!(second.message.contains("读卡器复位冷却中"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 2);
    assert_eq!(reset_calls.load(Ordering::SeqCst), 1);
    owner.retire_blocking(None, None, true).unwrap();
}

#[tokio::test]
async fn presence_requires_two_successful_absences_and_resets_after_scan_failure() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    let mut monitor = PresenceMonitor::new(fixture.owner.clone());
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    monitor.poll_with(|| async { Err(()) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    monitor
        .poll_with(|| async { Ok(vec!["Generic reader".into()]) })
        .await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .owner
            .acquire(&binding, Duration::from_secs(120))
            .is_err()
    );
}

async fn wait_for_presence_handoffs(owner: &Owner, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if owner.confirmed_presence_handoffs().len() == count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("physical close did not publish its handoff");
}

fn lifecycle_binding(session: &presence::Session) -> LifecycleBinding {
    LifecycleBinding {
        producer: session.producer.clone(),
        reader: session.reader.clone(),
        session_id: session.id.clone(),
        session_sequence: session.sequence,
    }
}

#[test]
fn lifecycle_live_binding_uses_current_rust_session_and_keeps_publication_atomic() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let current = fixture
        .owner
        .with_live_lifecycle_binding("producer", &fixture.reader, |binding| {
            assert!(fixture.owner.inner.state.try_lock().is_err());
            binding.clone()
        })
        .unwrap();
    assert_eq!(current.session_id, "session");
    assert_eq!(current.session_sequence, 1);
    // Opening a session is deliberately lazy; no physical connect is needed
    // to publish its identity to the lifecycle actor.
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 0);
    for (producer, reader) in [("old-producer", "Generic reader"), ("producer", "other")] {
        assert!(
            fixture
                .owner
                .with_live_lifecycle_binding(producer, reader, |_| panic!("invalid proof consumed"))
                .is_err()
        );
    }
    fixture
        .owner
        .open("producer", &fixture.reader, "replacement", 2)
        .unwrap();
    let replacement = fixture
        .owner
        .with_live_lifecycle_binding("producer", &fixture.reader, Clone::clone)
        .unwrap();
    assert_ne!(current, replacement);
    assert_eq!(replacement.session_id, "replacement");
    assert_eq!(replacement.session_sequence, 2);
}

#[tokio::test]
async fn lifecycle_presence_proof_requires_exact_confirmed_handoff_and_excludes_ack_reopen() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let lease = fixture.acquire(1);
    fixture.owner.release(&lease).unwrap();
    fixture.owner.enable_presence_handoff();
    let before = fixture
        .owner
        .with_live_lifecycle_binding("producer", &fixture.reader, Clone::clone)
        .unwrap();
    assert!(
        fixture
            .owner
            .with_confirmed_presence_loss(&before, || panic!("live session was treated as lost"))
            .is_err()
    );
    let mut monitor = PresenceMonitor::new(fixture.owner.clone());
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    wait_for_presence_handoffs(&fixture.owner, 1).await;
    let session = fixture.owner.confirmed_presence_handoffs().remove(0);
    let lost = lifecycle_binding(&session);
    assert_eq!(before, lost);
    assert!(
        fixture
            .owner
            .with_live_lifecycle_binding("producer", &fixture.reader, |_| panic!(
                "lost binding published as live"
            ))
            .is_err()
    );
    fixture
        .owner
        .with_confirmed_presence_loss(&lost, || {
            // Both ACK and open must acquire this mutex, so neither can revoke
            // the proof between validation and the lifecycle Gate transition.
            assert!(fixture.owner.inner.state.try_lock().is_err());
        })
        .unwrap();
    let mut mismatches = Vec::new();
    let mut other = lost.clone();
    other.producer = "old-producer".into();
    mismatches.push(other);
    let mut other = lost.clone();
    other.reader = "other".into();
    mismatches.push(other);
    let mut other = lost.clone();
    other.session_id = "other-session".into();
    mismatches.push(other);
    let mut other = lost.clone();
    other.session_sequence += 1;
    mismatches.push(other);
    for mismatch in mismatches {
        assert!(
            fixture
                .owner
                .with_confirmed_presence_loss(&mismatch, || panic!("mismatched proof consumed"))
                .is_err()
        );
    }
    fixture.owner.acknowledge_presence_handoff(&session);
    assert!(
        fixture
            .owner
            .with_confirmed_presence_loss(&lost, || panic!("acknowledged proof replayed"))
            .is_err()
    );
    fixture
        .owner
        .open("producer", &fixture.reader, "replacement", 2)
        .unwrap();
    assert!(
        fixture
            .owner
            .with_confirmed_presence_loss(&lost, || panic!("old proof applied to replacement"))
            .is_err()
    );
    let replacement = fixture
        .owner
        .with_live_lifecycle_binding("producer", &fixture.reader, Clone::clone)
        .unwrap();
    assert_ne!(lost, replacement);
}

#[tokio::test]
async fn lifecycle_close_tombstone_and_failed_close_are_not_loss_proofs() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let lease = fixture.acquire(1);
    fixture.owner.release(&lease).unwrap();
    let binding = fixture
        .owner
        .with_live_lifecycle_binding("producer", &fixture.reader, Clone::clone)
        .unwrap();
    fixture.owner.enable_presence_handoff();
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    let mut monitor = PresenceMonitor::new(fixture.owner.clone());
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(
        fixture
            .owner
            .inner
            .state
            .lock()
            .unwrap()
            .closed_sessions
            .get(&fixture.reader),
        Some(&1)
    );
    assert!(
        fixture
            .owner
            .with_confirmed_presence_loss(&binding, || panic!("failed close granted proof"))
            .is_err()
    );
    // Even an exact tombstone without a pending handoff must fail closed.
    fixture
        .owner
        .inner
        .state
        .lock()
        .unwrap()
        .pending_presence_handoffs
        .clear();
    assert!(
        fixture
            .owner
            .with_confirmed_presence_loss(&binding, || panic!("tombstone granted proof"))
            .is_err()
    );
}

#[tokio::test]
async fn presence_handoff_fences_replacement_until_exact_business_ack() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let lease = fixture.acquire(1);
    fixture.owner.release(&lease).unwrap();
    let mut monitor = PresenceMonitor::with_native_handoff(
        fixture.owner.clone(),
        Arc::new(|_| Box::pin(async { Err("not acknowledged".into()) })),
    );
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    wait_for_presence_handoffs(&fixture.owner, 1).await;
    let old = fixture.owner.confirmed_presence_handoffs();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].sequence, 1);
    assert!(
        fixture
            .owner
            .open("producer", "Generic reader", "replacement", 2)
            .is_err()
    );
    fixture.owner.acknowledge_presence_handoff(&old[0]);
    fixture
        .owner
        .open("producer", "Generic reader", "replacement", 2)
        .unwrap();
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    wait_for_presence_handoffs(&fixture.owner, 1).await;
    let current = fixture.owner.confirmed_presence_handoffs();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].sequence, 2);
    fixture.owner.acknowledge_presence_handoff(&old[0]);
    assert_eq!(fixture.owner.confirmed_presence_handoffs().len(), 1);
    assert!(
        fixture
            .owner
            .open("producer", "Generic reader", "future", 3)
            .is_err()
    );
}

#[tokio::test]
async fn presence_handoff_does_not_ack_a_failed_physical_close() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let lease = fixture.acquire(1);
    fixture.owner.release(&lease).unwrap();
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    let mut monitor = PresenceMonitor::with_native_handoff(
        fixture.owner.clone(),
        Arc::new(|_| Box::pin(async { Err("not acknowledged".into()) })),
    );
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.trace.closes.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(fixture.owner.confirmed_presence_handoffs().is_empty());
    assert!(
        fixture
            .owner
            .open("producer", "Generic reader", "replacement", 2)
            .is_err()
    );
}

#[tokio::test]
async fn presence_handoff_retries_failed_private_callback_before_reuse() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let lease = fixture.acquire(1);
    fixture.owner.release(&lease).unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let mut monitor = PresenceMonitor::with_native_handoff(
        fixture.owner.clone(),
        Arc::new(move |binding| {
            assert_eq!(binding.reader, "Generic reader");
            assert_eq!(binding.producer, "producer");
            assert_eq!(binding.session_id, "session");
            assert_eq!(binding.session_sequence, 1);
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                if first {
                    Err("cleanup pending".into())
                } else {
                    Ok(())
                }
            })
        }),
    );
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    wait_for_presence_handoffs(&fixture.owner, 1).await;
    monitor.flush_confirmed_handoffs().await;
    assert_eq!(fixture.owner.confirmed_presence_handoffs().len(), 1);
    assert!(
        fixture
            .owner
            .open("producer", "Generic reader", "replacement", 2)
            .is_err()
    );
    monitor.flush_confirmed_handoffs().await;
    assert!(fixture.owner.confirmed_presence_handoffs().is_empty());
    fixture
        .owner
        .open("producer", "Generic reader", "replacement", 2)
        .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn presence_does_not_scan_or_close_a_borrowed_transaction() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let mut monitor = PresenceMonitor::new(fixture.owner.clone());
    let scans = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let scans = scans.clone();
        monitor
            .poll_with(move || async move {
                scans.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            })
            .await;
    }
    assert_eq!(scans.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    fixture.owner.release(&binding).unwrap();
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_cannot_revoke_a_blocked_apdu() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let (started, release) = fixture.block_io();
    let owner = fixture.owner.clone();
    let request = binding.clone();
    let submitted = std::thread::spawn(move || {
        owner.transmit(
            &request,
            None,
            vec![0, 0xa4, 0, 0],
            Some((1, "blocked-apdu".into())),
        )
    });
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut monitor = PresenceMonitor::new(fixture.owner.clone());
    let scans = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let scans = scans.clone();
        monitor
            .poll_with(move || async move {
                scans.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            })
            .await;
    }
    assert_eq!(scans.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    assert_eq!(submitted.join().unwrap().unwrap(), vec![0x90, 0]);
    fixture.owner.release(&binding).unwrap();
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn presence_discards_inventory_started_before_session_replacement() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let owner = fixture.owner.clone();
    let mut monitor = PresenceMonitor::new(owner.clone());
    monitor
        .poll_with(|| async {
            owner
                .open("producer", "Generic reader", "replacement", 2)
                .unwrap();
            Ok(vec![])
        })
        .await;
    assert_eq!(fixture.owner.idle_pcsc_sessions()[0].id, "replacement");
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.owner.idle_pcsc_sessions()[0].id, "replacement");
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert!(fixture.owner.idle_pcsc_sessions().is_empty());
}

#[tokio::test]
async fn presence_cannot_apply_an_old_producers_scan_to_the_replacement() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let owner = fixture.owner.clone();
    let mut monitor = PresenceMonitor::new(owner.clone());
    monitor
        .poll_with(|| async {
            owner
                .replace_producer("replacement-producer".into())
                .await
                .unwrap();
            owner
                .open("replacement-producer", "Generic reader", "session", 1)
                .unwrap();
            Ok(vec![])
        })
        .await;
    assert_eq!(
        fixture.owner.idle_pcsc_sessions()[0].producer,
        "replacement-producer"
    );
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert_eq!(fixture.owner.idle_pcsc_sessions().len(), 1);
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    assert!(fixture.owner.idle_pcsc_sessions().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_waits_for_physical_close_before_new_session_can_open() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    let (started, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    *fixture.trace.close_gate.lock().unwrap() = Some((started, wait));
    let mut monitor = PresenceMonitor::new(fixture.owner.clone());
    monitor.poll_with(|| async { Ok(vec![]) }).await;
    let closing = tokio::spawn(async move {
        monitor.poll_with(|| async { Ok(vec![]) }).await;
    });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    assert!(!closing.is_finished());
    assert!(fixture.owner.idle_pcsc_sessions().is_empty());
    assert!(
        fixture
            .owner
            .acquire(&binding, Duration::from_secs(120))
            .is_err()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), closing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
    fixture
        .owner
        .open("producer", "Generic reader", "replacement", 2)
        .unwrap();
}

#[test]
fn selection_is_enumeration_only_and_generic_handle_is_persistent() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 0);
    for sequence in 1..=2 {
        let binding = fixture.acquire(sequence);
        assert_eq!(
            fixture
                .owner
                .transmit(
                    &binding,
                    None,
                    vec![0, 0xa4, 0, 0],
                    Some((1, "operation".into()))
                )
                .unwrap(),
            [0x90, 0]
        );
        fixture.owner.release(&binding).unwrap();
    }
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
}

#[test]
fn released_request_cannot_late_acquire_or_replay_evicted_apdu() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.binding(1);
    fixture.owner.release(&binding).unwrap();
    assert_eq!(
        fixture
            .owner
            .acquire(&binding, Duration::from_secs(120))
            .unwrap_err()
            .code,
        "reader_pcsc_stale_session"
    );
    let binding = fixture.acquire(2);
    for sequence in 1..=40 {
        fixture
            .owner
            .transmit(
                &binding,
                None,
                vec![0, 0xa4, 0, 0],
                Some((sequence, format!("op-{sequence}"))),
            )
            .unwrap();
    }
    fixture
        .owner
        .transmit(
            &binding,
            None,
            vec![0, 0xa4, 0, 0],
            Some((40, "op-40".into())),
        )
        .unwrap();
    assert_eq!(fixture.trace.calls.lock().unwrap().len(), 40);
    assert_eq!(
        fixture
            .owner
            .transmit(
                &binding,
                None,
                vec![0, 0xa4, 0, 0],
                Some((1, "op-1".into()))
            )
            .unwrap_err()
            .code,
        "reader_pcsc_stale_session"
    );
    assert_eq!(fixture.trace.calls.lock().unwrap().len(), 40);
    fixture.owner.release(&binding).unwrap();
}

#[test]
fn close_before_enumeration_finishes_prevents_late_open() {
    let fixture = Fixture::new("Generic reader");
    fixture
        .owner
        .close("producer", &fixture.reader, "session", 1, None)
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .open("producer", &fixture.reader, "session", 1)
            .unwrap_err()
            .code,
        "reader_pcsc_stale_session"
    );
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 0);
    fixture
        .owner
        .open("producer", &fixture.reader, "new-session", 2)
        .unwrap();
}

#[test]
fn native_borrow_uses_existing_transaction_and_does_not_advance_wire_sequence() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture
        .owner
        .transmit(
            &binding,
            None,
            vec![0, 0xa4, 0, 0],
            Some((1, "go-1".into())),
        )
        .unwrap();
    fixture
        .owner
        .with_transaction(Some(&binding), &fixture.reader, |transport| {
            transport.transmit(&[0, 0xb0, 0, 0]).unwrap();
            assert_eq!(
                fixture
                    .owner
                    .transmit(
                        &binding,
                        None,
                        vec![0, 0xa4, 0, 0],
                        Some((2, "go-2".into()))
                    )
                    .unwrap_err()
                    .code,
                "reader_pcsc_busy"
            );
        })
        .unwrap();
    fixture
        .owner
        .transmit(
            &binding,
            None,
            vec![0, 0xa4, 0, 0],
            Some((2, "go-2".into())),
        )
        .unwrap();
    assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.trace.calls.lock().unwrap().len(), 3);
    fixture.owner.release(&binding).unwrap();
}

#[test]
fn native_short_leases_do_not_poison_go_sequence_space() {
    let fixture = Fixture::new("Generic reader");
    fixture
        .owner
        .with_transaction(None, &fixture.reader, |transport| {
            transport.transmit(&[0, 0xa4, 0, 0])
        })
        .unwrap()
        .unwrap();
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
}

#[test]
fn release_waits_for_actual_io_and_native_closure_before_terminal_status() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let (ready, unblock) = fixture.block_io();
    let owner = fixture.owner.clone();
    let native_binding = binding.clone();
    let reader = fixture.reader.clone();
    let (closure_ready, closure_seen) = mpsc::sync_channel(1);
    let (leave, leave_wait) = mpsc::sync_channel(1);
    let native = thread::spawn(move || {
        owner.with_transaction(Some(&native_binding), &reader, |transport| {
            let result = transport.transmit(&[0, 0xa4, 0, 0]);
            closure_ready.send(()).unwrap();
            leave_wait.recv_timeout(Duration::from_secs(5)).unwrap();
            result
        })
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let owner = fixture.owner.clone();
    let released_binding = binding.clone();
    let (done, done_wait) = mpsc::sync_channel(1);
    let release = thread::spawn(move || {
        let value = owner.release(&released_binding);
        done.send(()).unwrap();
        value
    });
    let worker = fixture.owner.worker("producer", &fixture.reader).unwrap();
    {
        let mut state = worker.state.lock().unwrap();
        while state.lease.as_ref().unwrap().ending.is_none() {
            drop(state);
            thread::yield_now();
            state = worker.state.lock().unwrap();
        }
    }
    assert_eq!(fixture.owner.status(&binding).unwrap()["state"], "closing");
    assert!(done_wait.try_recv().is_err());
    unblock.send(()).unwrap();
    closure_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(done_wait.try_recv().is_err());
    leave.send(()).unwrap();
    assert!(native.join().unwrap().is_err());
    release.join().unwrap().unwrap();
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], true);
}

#[test]
fn expiry_rejects_renewal_and_closes_handle_after_real_completion() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let worker = fixture.owner.worker("producer", &fixture.reader).unwrap();
    worker
        .state
        .lock()
        .unwrap()
        .lease
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(
        fixture
            .owner
            .renew(&binding, Duration::from_secs(120))
            .unwrap_err()
            .code,
        "reader_pcsc_lease_expired"
    );
    fixture.owner.release(&binding).unwrap();
    assert_eq!(fixture.owner.status(&binding).unwrap()["state"], "expired");
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_admission_keeps_live_cleanup_available() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.begin_shutdown();
    assert_eq!(fixture.owner.status(&binding).unwrap()["state"], "live");
    assert_eq!(
        fixture
            .owner
            .transmit(&binding, None, vec![0, 0xa4, 0, 0], Some((1, "op".into())))
            .unwrap_err()
            .code,
        "reader_pcsc_stopping"
    );
    assert_eq!(
        fixture
            .owner
            .renew(&binding, Duration::from_secs(120))
            .unwrap_err()
            .code,
        "reader_pcsc_stopping"
    );
    fixture.owner.release(&binding).unwrap();
    fixture
        .owner
        .close("producer", &fixture.reader, "session", 1, None)
        .unwrap();
}

#[test]
fn estk_release_closes_but_generic_reset_is_rejected_without_io() {
    let generic = Fixture::new("Generic reader");
    generic.open();
    let binding = generic.acquire(1);
    assert_eq!(
        generic
            .owner
            .reset(&binding, ResetAction::Reset, 1, "reset".into())
            .unwrap_err()
            .code,
        "reader_pcsc_reset_unsupported"
    );
    assert!(generic.trace.resets.lock().unwrap().is_empty());
    generic.owner.release(&binding).unwrap();
    let estk = Fixture::new("ESTKme-RED");
    estk.open();
    let binding = estk.acquire(1);
    estk.owner
        .reset(&binding, ResetAction::Reset, 1, "reset".into())
        .unwrap();
    estk.owner
        .reset(&binding, ResetAction::Reset, 1, "reset".into())
        .unwrap();
    assert_eq!(*estk.trace.resets.lock().unwrap(), vec![true]);
    estk.owner.release(&binding).unwrap();
    assert!(estk.trace.closes.load(Ordering::SeqCst) >= 3);
}

#[test]
fn physical_close_failure_quarantines_handle_and_does_not_confirm_release() {
    let fixture = Fixture::new("ESTKme-RED");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.release(&binding).unwrap_err().code,
        "fixture_close_failed"
    );
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    assert_eq!(
        fixture
            .owner
            .open("producer", &fixture.reader, "next", 2)
            .unwrap_err()
            .code,
        "fixture_close_failed"
    );
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 1);
}

#[test]
fn producer_replacement_fences_every_old_capability() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    fixture
        .owner
        .retire_blocking(None, Some("replacement".into()), false)
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .open("producer", &fixture.reader, "old", 2)
            .unwrap_err()
            .code,
        "reader_pcsc_stale_session"
    );
    assert_eq!(fixture.owner.status(&binding).unwrap()["state"], "stale");
    fixture
        .owner
        .open("replacement", &fixture.reader, "new", 1)
        .unwrap();
}

#[test]
fn extended_apdu_boundary_and_error_diagnostics_do_not_expose_payload() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture
        .owner
        .transmit(&binding, None, vec![0xaa; 65544], Some((1, "large".into())))
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .transmit(
                &binding,
                None,
                vec![0xaa; 65545],
                Some((2, "too-large".into()))
            )
            .unwrap_err()
            .code,
        "reader_pcsc_invalid_request"
    );
    let error = Failure::unknown();
    assert!(!format!("{error:?}").contains("AAAA"));
    fixture.owner.release(&binding).unwrap();
}

#[test]
fn failed_transaction_end_requires_confirmed_close_before_release() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.trace.end_error.store(true, Ordering::SeqCst);
    fixture.owner.release(&binding).unwrap();
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], true);

    fixture.trace.end_error.store(false, Ordering::SeqCst);
    let binding = fixture.acquire(2);
    fixture.trace.end_error.store(true, Ordering::SeqCst);
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.release(&binding).unwrap_err().code,
        "fixture_close_failed"
    );
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
}

#[test]
fn unconfirmed_acquire_status_is_not_a_terminal_ack() {
    let fixture = Fixture::new("Generic reader");
    let binding = fixture.binding(1);
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    fixture.open();
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    fixture.owner.release(&binding).unwrap();
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], true);
    assert!(
        fixture
            .owner
            .acquire(&binding, Duration::from_secs(120))
            .is_err()
    );
    let active = fixture.acquire(2);
    let future = fixture.binding(3);
    assert_eq!(fixture.owner.status(&future).unwrap()["terminal"], false);
    fixture.owner.release(&future).unwrap();
    assert_eq!(fixture.owner.status(&future).unwrap()["terminal"], true);
    assert_eq!(fixture.owner.status(&active).unwrap()["terminal"], false);
    fixture.owner.release(&active).unwrap();
}

#[test]
fn retirement_close_failure_retains_physical_owner_and_blocks_replacement() {
    let fixture = Fixture::new("Generic reader");
    assert!(!fixture.owner.producer_physically_idle("producer"));
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture
            .owner
            .retire_blocking(None, Some("next".into()), false)
            .unwrap_err()
            .code,
        "fixture_close_failed"
    );
    let state = fixture.owner.inner.state.lock().unwrap();
    assert_eq!(state.producer, "producer");
    assert!(!state.active);
    let worker = state.readers.get(&fixture.reader).unwrap().clone();
    drop(state);
    assert!(worker.quarantined.lock().unwrap().is_some());
    assert!(!fixture.owner.producer_physically_idle("producer"));
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 1);
    fixture.trace.close_fails.store(false, Ordering::SeqCst);
    fixture
        .owner
        .retire_blocking(None, Some("next".into()), false)
        .unwrap();
    assert!(worker.quarantined.lock().unwrap().is_none());
    assert!(fixture.owner.producer_physically_idle("producer"));
    assert!(!fixture.owner.producer_physically_idle("next"));
}

#[test]
fn native_cleanup_without_a_binding_waits_for_actual_producer_retirement() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let (started, unblock) = fixture.block_io();
    let owner = fixture.owner.clone();
    let io = std::thread::spawn(move || owner.transmit(&binding, None, vec![0, 0xa4, 0, 0], None));
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    let owner = fixture.owner.clone();
    let worker = fixture.owner.inner.state.lock().unwrap().readers[&fixture.reader].clone();
    let retirement = std::thread::spawn(move || owner.retire_blocking(None, None, true));
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !worker.state.lock().unwrap().closing {
        assert!(
            std::time::Instant::now() < deadline,
            "retirement did not begin"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!fixture.owner.producer_physically_idle("producer"));
    unblock.send(()).unwrap();
    // Retirement invalidates the business outcome even though the physical
    // operation finishes. Only the joined/closed worker proves cleanup.
    assert_eq!(
        io.join().unwrap().unwrap_err().code,
        "reader_pcsc_outcome_unknown"
    );
    retirement.join().unwrap().unwrap();
    assert!(fixture.owner.producer_physically_idle("producer"));
}

#[test]
fn delayed_enumeration_cannot_resurrect_a_closed_session_token() {
    let trace = Arc::new(Trace::default());
    let factory_trace = trace.clone();
    let (started, ready) = mpsc::sync_channel(1);
    let (unblock, wait) = mpsc::sync_channel(1);
    let gate = Mutex::new(Some((started, wait)));
    let owner = Owner::with_factory(
        "producer".into(),
        Arc::new(move |_| {
            factory_trace.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Fake(factory_trace.clone())))
        }),
        Arc::new(move || {
            if let Some((started, wait)) = gate.lock().unwrap().take() {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(vec!["reader".into()])
        }),
        Arc::new(|_, _| panic!("no reset expected")),
    );
    let opening = owner.clone();
    let task = thread::spawn(move || opening.open("producer", "reader", "session", 1));
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    owner
        .close("producer", "reader", "session", 1, None)
        .unwrap();
    unblock.send(()).unwrap();
    assert_eq!(
        task.join().unwrap().unwrap_err().code,
        "reader_pcsc_stale_session"
    );
    assert_eq!(trace.opens.load(Ordering::SeqCst), 0);
    owner.retire_blocking(None, None, true).unwrap();
}

#[test]
fn close_during_persistent_handle_drain_prevents_late_session_publication() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    let (started, ready) = mpsc::sync_channel(1);
    let (unblock, wait) = mpsc::sync_channel(1);
    *fixture.trace.close_gate.lock().unwrap() = Some((started, wait));
    let opening = fixture.owner.clone();
    let reader = fixture.reader.clone();
    let task = thread::spawn(move || opening.open("producer", &reader, "replacement", 2));
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    fixture
        .owner
        .close("producer", &fixture.reader, "replacement", 2, None)
        .unwrap();
    unblock.send(()).unwrap();
    assert_eq!(
        task.join().unwrap().unwrap_err().code,
        "reader_pcsc_stale_session"
    );
    let worker = fixture.owner.worker("producer", &fixture.reader).unwrap();
    assert_ne!(worker.state.lock().unwrap().session, "replacement");
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 1);
}

#[test]
fn final_session_publication_rechecks_a_transaction_started_while_waiting() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let worker = fixture.owner.worker("producer", &fixture.reader).unwrap();
    assert_eq!(
        fixture
            .owner
            .publish_session(
                &worker,
                "producer",
                &fixture.reader,
                "replacement",
                2,
                false
            )
            .unwrap_err()
            .code,
        "reader_pcsc_busy"
    );
    assert_eq!(worker.state.lock().unwrap().session, "session");
    fixture
        .owner
        .transmit(
            &binding,
            None,
            vec![0, 0xa4, 0, 0],
            Some((1, "old-live".into())),
        )
        .unwrap();
    fixture.owner.release(&binding).unwrap();
    // The new lease may already have ended by the time the open waiter runs.
    // Its still-open physical handle must likewise never be silently rebound.
    assert_eq!(
        fixture
            .owner
            .publish_session(
                &worker,
                "producer",
                &fixture.reader,
                "replacement",
                2,
                false
            )
            .unwrap_err()
            .code,
        "reader_pcsc_busy"
    );
}

#[test]
fn old_close_completion_does_not_wait_for_or_clear_a_replacement_session() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    fixture
        .owner
        .open("producer", &fixture.reader, "replacement", 2)
        .unwrap();
    let mut binding = fixture.binding(2);
    binding.session_id = "replacement".into();
    fixture
        .owner
        .acquire(&binding, Duration::from_secs(120))
        .unwrap();
    let worker = fixture.owner.worker("producer", &fixture.reader).unwrap();
    {
        let mut state = worker.state.lock().unwrap();
        // Resume an old close waiter after the replacement won publication.
        assert!(!closing_session_pending(&state, "session", 1));
        finish_session_close(&mut state, "session", 1).unwrap();
        assert_eq!(state.session, "replacement");
        assert_eq!(state.lease.as_ref().unwrap().binding, binding);
    }
    fixture
        .owner
        .transmit(
            &binding,
            None,
            vec![0, 0xa4, 0, 0],
            Some((1, "replacement-live".into())),
        )
        .unwrap();
    fixture.owner.release(&binding).unwrap();
}

#[test]
fn old_producer_close_cannot_write_a_replacement_producers_tombstone() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    // The old request passed validation before retirement. Resume its commit
    // after the new producer has been published, without probabilistic races.
    fixture.owner.check_producer("producer").unwrap();
    fixture
        .owner
        .retire_blocking(None, Some("replacement-producer".into()), false)
        .unwrap();
    assert_eq!(
        fixture
            .owner
            .record_session_close("producer", &fixture.reader, 500)
            .err()
            .expect("retired producer must not mutate tombstones")
            .code,
        "reader_pcsc_stale_session"
    );
    assert!(
        fixture
            .owner
            .inner
            .state
            .lock()
            .unwrap()
            .closed_sessions
            .is_empty()
    );
    // Sequence numbers restart in the replacement Go process.
    fixture
        .owner
        .open("replacement-producer", &fixture.reader, "new-session", 1)
        .unwrap();
}

#[tokio::test]
async fn cancelled_waiter_does_not_release_or_replay_submitted_apdu() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let (ready, unblock) = fixture.block_io();
    let owner = fixture.owner.clone();
    let request = binding.clone();
    let pool = owner.inner.normal_pool.clone();
    let task = tokio::spawn(async move {
        pool.run(move || {
            owner.transmit(
                &request,
                None,
                vec![0, 0xa4, 0, 0],
                Some((1, "once".into())),
            )
        })
        .await
    });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(fixture.owner.status(&binding).unwrap()["state"], "live");
    assert_eq!(
        fixture
            .owner
            .transmit(
                &binding,
                None,
                vec![0, 0xa4, 0, 0],
                Some((1, "once".into()))
            )
            .unwrap_err()
            .code,
        "reader_pcsc_busy"
    );
    unblock.send(()).unwrap();
    let worker = fixture.owner.worker("producer", &fixture.reader).unwrap();
    loop {
        let state = worker.state.lock().unwrap();
        if state
            .wire_cache
            .front()
            .is_some_and(|entry| entry.result.is_some())
        {
            break;
        }
        drop(state);
        tokio::task::yield_now().await;
    }
    fixture
        .owner
        .transmit(
            &binding,
            None,
            vec![0, 0xa4, 0, 0],
            Some((1, "once".into())),
        )
        .unwrap();
    assert_eq!(fixture.trace.calls.lock().unwrap().len(), 1);
    fixture.owner.release(&binding).unwrap();
}

#[test]
fn retained_operation_retirement_fences_acquire_before_worker_exists() {
    let fixture = Fixture::new("retirement fixture");
    let binding = fixture.binding(7);
    assert!(fixture.owner.retire_operation_binding(&binding).unwrap());
    fixture.open();
    assert_eq!(
        fixture
            .owner
            .acquire(&binding, Duration::from_secs(120))
            .unwrap_err()
            .code,
        "reader_pcsc_stale_session"
    );
    assert_eq!(fixture.trace.transactions.load(Ordering::SeqCst), 0);
    let later = fixture.acquire(8);
    fixture.owner.release(&later).unwrap();
}

#[test]
fn retained_operation_retirement_cannot_terminate_actual_apdu() {
    let fixture = Fixture::new("retirement fixture");
    fixture.open();
    let binding = fixture.acquire(1);
    let (ready, unblock) = fixture.block_io();
    let owner = fixture.owner.clone();
    let request = binding.clone();
    let task = thread::spawn(move || {
        owner.transmit(
            &request,
            None,
            vec![0, 0xa4, 0, 0],
            Some((1, "once".into())),
        )
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    unblock.send(()).unwrap();
    // Retirement marks the result uncertain while the submitted command is
    // still executing; only its actual completion permits lease cleanup.
    let _ = task.join().unwrap();
    fixture.owner.release(&binding).unwrap();
    assert!(fixture.owner.retire_operation_binding(&binding).unwrap());
    assert_eq!(fixture.trace.calls.lock().unwrap().len(), 1);
    assert!(
        fixture
            .owner
            .acquire(&binding, Duration::from_secs(120))
            .is_err()
    );
}

#[test]
fn retained_operation_retirement_rejects_false_completion_after_close_failure() {
    let fixture = Fixture::new("retirement fixture");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.trace.end_error.store(true, Ordering::SeqCst);
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    let _ = fixture.owner.release(&binding);
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
}

#[test]
fn dropping_last_owner_breaks_idle_worker_cycle_and_exits_bounded() {
    let owner = Owner::with_factory(
        "producer".into(),
        Arc::new(|_| panic!("idle drop regression must not open a physical reader")),
        Arc::new(|| Ok(vec!["Drop reader".into()])),
        Arc::new(|_, _| panic!("idle drop regression must not reset a physical reader")),
    );
    owner.open("producer", "Drop reader", "session", 1).unwrap();
    let worker = owner.worker("producer", "Drop reader").unwrap();
    let inner = Arc::downgrade(&owner.inner);
    let handle = worker
        .thread
        .lock()
        .unwrap()
        .take()
        .expect("worker thread handle");
    drop(owner);

    let (joined, completion) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = joined.send(handle.join());
    });
    let result = match completion.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(_) => {
            let mut state = worker.state.lock().unwrap();
            state.stopping = true;
            state.closing = true;
            drop(state);
            let _ = worker.sender.try_send(Work::Wake);
            let _ = completion.recv_timeout(Duration::from_secs(2));
            panic!("idle PC/SC worker retained the dropped Owner");
        }
    };
    assert!(result.is_ok(), "idle PC/SC worker panicked during drop");
    assert!(inner.upgrade().is_none(), "Inner survived the last Owner");
    assert!(
        worker.sender.try_send(Work::Wake).is_err(),
        "worker receiver survived its thread"
    );
}

#[tokio::test]
async fn final_shutdown_joins_idle_worker_before_success() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let worker = fixture.owner.worker("producer", "Generic reader").unwrap();
    tokio::time::timeout(Duration::from_secs(2), fixture.owner.shutdown())
        .await
        .expect("final shutdown must be bounded")
        .unwrap();
    assert!(worker.thread.lock().unwrap().is_none());
    assert!(worker.sender.try_send(Work::Wake).is_err());
    assert!(fixture.owner.inner.state.lock().unwrap().readers.is_empty());
}

#[path = "card_workflow_tests.rs"]
mod card_workflow_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_shutdown_waits_for_physical_close_before_confirming_idle() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    let (started, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    *fixture.trace.close_gate.lock().unwrap() = Some((started, wait));
    let owner = fixture.owner.clone();
    let closing = tokio::spawn(async move { owner.shutdown().await });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    assert!(!closing.is_finished());
    assert!(!fixture.owner.producer_physically_idle("producer"));
    assert_eq!(fixture.trace.opens.load(Ordering::SeqCst), 1);
    closing.abort();
    assert!(closing.await.unwrap_err().is_cancelled());
    {
        let receipt = fixture.owner.inner.final_retirement.lock().await;
        assert!(!receipt.confirmed);
        assert!(!receipt.task.as_ref().unwrap().is_finished());
    }
    let owner = fixture.owner.clone();
    let retry = tokio::spawn(async move { owner.shutdown().await });
    assert!(!retry.is_finished());
    release.send(()).unwrap();
    retry.await.unwrap().unwrap();
    fixture.owner.shutdown().await.unwrap();
    assert!(fixture.owner.producer_physically_idle("producer"));
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .owner
            .replace_producer("replacement".into())
            .await
            .is_err()
    );
    assert_eq!(
        fixture.owner.inner.state.lock().unwrap().producer,
        "producer"
    );
}

#[tokio::test]
async fn final_shutdown_failure_retains_owner_until_physical_close_is_confirmed() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    fixture.trace.close_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.shutdown().await.unwrap_err().code,
        "fixture_close_failed"
    );
    assert!(!fixture.owner.producer_physically_idle("producer"));
    assert!(!fixture.owner.inner.state.lock().unwrap().readers.is_empty());
    fixture.trace.close_fails.store(false, Ordering::SeqCst);
    fixture.owner.shutdown().await.unwrap();
    assert!(fixture.owner.producer_physically_idle("producer"));
    assert!(fixture.owner.inner.state.lock().unwrap().readers.is_empty());
}

#[tokio::test]
async fn final_shutdown_worker_panic_never_confirms_physical_retirement() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    fixture.owner.release(&binding).unwrap();
    fixture.trace.close_panics.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.owner.shutdown().await.unwrap_err().code,
        "reader_pcsc_worker_failed"
    );
    assert!(!fixture.owner.producer_physically_idle("producer"));
    fixture.trace.close_panics.store(false, Ordering::SeqCst);
    // A dead worker without a confirmed physical close is not retry success.
    assert_eq!(
        fixture.owner.shutdown().await.unwrap_err().code,
        "reader_pcsc_worker_failed"
    );
    assert!(!fixture.owner.producer_physically_idle("producer"));
}
