use super::*;
use crate::reader_pcsc::{LifecycleBinding, Physical};
use proletarius_sim::ApduTransport;
use std::sync::{atomic::AtomicUsize, mpsc};

const READER: &str = "Native N-wait fixture";
const PRODUCER: &str = "native-nwait-fixture";
#[derive(Default)]
struct Trace {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    unblock: Mutex<Option<mpsc::Receiver<()>>>,
    close_entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    close_unblock: Mutex<Option<mpsc::Receiver<()>>>,
    close_fails: AtomicBool,
    ended: AtomicUsize,
    closed: AtomicUsize,
    opened: AtomicUsize,
}
struct Card(Arc<Trace>);
impl ApduTransport for Card {
    type Error = Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Failure> {
        Ok(vec![0x90, 0])
    }
}
impl Physical for Card {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = Failure>),
    ) -> Result<(), Failure> {
        if let Some(entered) = self.0.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        if let Some(unblock) = self.0.unblock.lock().unwrap().take() {
            unblock.recv().unwrap();
        }
        body(self);
        self.0.ended.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn close(&mut self) -> Result<(), Failure> {
        if let Some(entered) = self.0.close_entered.lock().unwrap().take() {
            entered.send(()).unwrap();
        }
        if let Some(unblock) = self.0.close_unblock.lock().unwrap().take() {
            unblock.recv().unwrap();
        }
        if self.0.close_fails.load(Ordering::SeqCst) {
            return Err(failure("fixture_close_failed"));
        }
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Fixture {
    gate: Arc<Gate>,
    owner: Owner,
    session: LifecycleBinding,
    trace: Arc<Trace>,
}
impl Fixture {
    fn new() -> Self {
        let gate = Arc::new(Gate::default());
        gate.initialize_native_reader_producer(PRODUCER);
        gate.initialize_native_lifecycle(PRODUCER).unwrap();
        gate.set_ordinary_admission(Arc::new(crate::ordinary_vowifi_admission::Gate::default()))
            .unwrap();
        let trace = Arc::new(Trace::default());
        let factory = trace.clone();
        let owner = Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move |_| {
                factory.opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Card(factory.clone())))
            }),
            Arc::new(|| Ok(vec![READER.into()])),
            Arc::new(|_, _| panic!("no reset")),
        );
        let session = owner.native_open(READER).unwrap();
        let transition = gate
            .prepare_native_transition(
                PRODUCER,
                &SelectionSnapshot::default(),
                NativeTransitionKind::Connect,
                READER,
                NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(&transition, Some(&session), NativeReaderFacts::default())
            .unwrap();
        Self {
            gate,
            owner,
            session,
            trace,
        }
    }
    fn open_physical(&self) {
        self.owner
            .native_work_on_session(&self.session, |card| card.transmit(&[0, 0x84, 0, 0, 0]))
            .unwrap()
            .unwrap();
        assert_eq!(self.trace.opened.load(Ordering::SeqCst), 1);
    }
    fn loss(&self) -> NativeTransition {
        self.gate
            .prepare_native_transition(
                PRODUCER,
                &self.gate.native_reader_snapshot().0,
                NativeTransitionKind::Loss,
                READER,
                NativeAuthority::Loss(self.session.clone()),
            )
            .unwrap()
    }
    async fn close(self) {
        self.owner.native_close(&self.session).unwrap();
        self.owner.shutdown().await.unwrap();
        assert_eq!(
            self.trace.closed.load(Ordering::SeqCst),
            self.trace.opened.load(Ordering::SeqCst)
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_parent_and_physical_work_survive_go_exit_without_go_principal() {
    let f = Fixture::new();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    let op = f
        .gate
        .begin_native_multi_operation(f.owner.clone(), &parent)
        .unwrap();
    let CardTransaction::Operation { binding } = &op.transaction else {
        unreachable!()
    };
    assert_eq!(
        f.gate
            .test_operation_command("acquire", serde_json::to_value(binding).unwrap())
            .err(),
        Some("stale_producer")
    );
    f.gate
        .acquire_native_multi_pcsc(f.owner.clone(), &op, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    f.gate.test_confirm_go_exit("new-go-producer");
    assert!(f.gate.native_multi_current(&parent));
    assert!(
        f.gate
            .validate_card_workflow(&op.transaction, READER, 1)
            .is_ok()
    );
    assert_eq!(f.gate.release_native_multi(&parent), Err("card_busy"));
    assert!(f.gate.acquire_native_multi(READER, 1).is_err());
    let mut forged = binding.clone();
    forged.operation_sequence += 1;
    assert!(
        f.gate
            .track_card_io(
                &CardTransaction::Operation { binding: forged },
                PRODUCER,
                READER
            )
            .is_err()
    );
    f.gate.finish_native_multi_operation(&op).await.unwrap();
    assert!(
        f.gate
            .validate_card_workflow(&op.transaction, READER, 1)
            .is_err()
    );
    f.gate.release_native_multi(&parent).unwrap();
    f.gate.release_native_multi(&parent).unwrap();
    assert!(!f.gate.native_multi_current(&parent));
    f.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandoned_acquire_keeps_fifo_and_parent_until_physical_begin_returns() {
    let f = Fixture::new();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    let op = f
        .gate
        .begin_native_multi_operation(f.owner.clone(), &parent)
        .unwrap();
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (unblock, blocked) = mpsc::channel();
    *f.trace.entered.lock().unwrap() = Some(entered);
    *f.trace.unblock.lock().unwrap() = Some(blocked);
    let g = f.gate.clone();
    let o = f.owner.clone();
    let operation = op.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let acquire =
        tokio::spawn(async move { g.acquire_native_multi_pcsc(o, &operation, flag).await });
    tokio::time::timeout(Duration::from_secs(3), wait)
        .await
        .unwrap()
        .unwrap();
    acquire.abort();
    let _ = acquire.await;
    cancelled.store(true, Ordering::Release);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            f.gate.finish_native_multi_operation(&op)
        )
        .await
        .is_err()
    );
    f.gate.test_confirm_go_exit("replacement-go");
    assert_eq!(f.gate.release_native_multi(&parent), Err("card_busy"));
    assert!(
        f.gate
            .begin_native_multi_operation(f.owner.clone(), &parent)
            .is_err()
    );
    assert_eq!(f.trace.ended.load(Ordering::SeqCst), 0);
    unblock.send(()).unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        f.gate.finish_native_multi_operation(&op),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(f.trace.ended.load(Ordering::SeqCst), 1);
    f.gate.release_native_multi(&parent).unwrap();
    f.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_operation_expiry_rejects_new_work_but_waits_actual_io_guard() {
    let f = Fixture::new();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    let op = f
        .gate
        .begin_native_multi_operation(f.owner.clone(), &parent)
        .unwrap();
    let permit = f
        .gate
        .track_card_io(&op.transaction, PRODUCER, READER)
        .unwrap();
    let CardTransaction::Operation { binding } = &op.transaction else {
        unreachable!()
    };
    f.gate
        .0
        .lock()
        .unwrap()
        .operations
        .entries
        .get_mut(&binding.operation_token)
        .unwrap()
        .deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        f.gate.renew_native_multi_operation(&op),
        Err("owner_expired")
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            f.gate.finish_native_multi_operation(&op)
        )
        .await
        .is_err()
    );
    assert_eq!(f.gate.release_native_multi(&parent), Err("card_busy"));
    drop(permit);
    tokio::time::timeout(
        Duration::from_secs(3),
        f.gate.finish_native_multi_operation(&op),
    )
    .await
    .unwrap()
    .unwrap();
    f.gate.release_native_multi(&parent).unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss_retirement_waits_real_close_and_receipt_survives_ack_and_replacement() {
    let f = Fixture::new();
    f.open_physical();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (unblock, blocked) = mpsc::channel();
    *f.trace.close_entered.lock().unwrap() = Some(entered);
    *f.trace.close_unblock.lock().unwrap() = Some(blocked);
    let owner = f.owner.clone();
    let binding = f.session.clone();
    let closing = tokio::task::spawn_blocking(move || owner.test_close_for_presence(&binding));
    tokio::time::timeout(Duration::from_secs(3), wait)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 0);
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    let premature = f.loss();
    assert_eq!(
        f.gate
            .commit_confirmed_native_presence_loss(&f.owner, &premature)
            .unwrap_err(),
        "physical_loss_unconfirmed"
    );
    f.gate.abort_native_transition(&premature).unwrap();
    assert!(f.owner.native_open(READER).is_err());
    unblock.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), closing)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 1);
    let loss = f.loss();
    f.gate
        .commit_confirmed_native_presence_loss(&f.owner, &loss)
        .unwrap();
    f.owner.test_ack_presence_loss(&f.session);
    assert!(
        f.owner
            .with_confirmed_presence_loss(&f.session, || ())
            .is_err()
    );
    // A generic receipt ring eviction must not erase the parent-pinned proof.
    lifecycle::clear_test_native_receipts(&f.gate);
    assert_eq!(
        f.gate.release_native_multi(&parent),
        Err("stale_reader_generation")
    );
    assert!(
        f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    let replacement = f.owner.native_open(READER).unwrap();
    assert_ne!(replacement, f.session);
    let connect = f
        .gate
        .prepare_native_transition(
            PRODUCER,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Connect,
            READER,
            NativeAuthority::Idle,
        )
        .unwrap();
    f.gate
        .commit_native_transition(&connect, Some(&replacement), NativeReaderFacts::default())
        .unwrap();
    let next = f.gate.acquire_native_multi(READER, 3).unwrap();
    assert!(
        f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&next)
            .unwrap()
    );
    assert!(f.gate.native_multi_current(&next));
    assert!(!f.owner.test_close_for_presence(&f.session).unwrap());
    f.gate.release_native_multi(&next).unwrap();
    f.owner.native_close(&replacement).unwrap();
    f.owner.shutdown().await.unwrap();
    assert_eq!(
        f.trace.closed.load(Ordering::SeqCst),
        f.trace.opened.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_physical_close_and_tombstone_never_grant_parent_retirement() {
    let f = Fixture::new();
    f.open_physical();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    f.trace.close_fails.store(true, Ordering::SeqCst);
    let owner = f.owner.clone();
    let binding = f.session.clone();
    let error = tokio::task::spawn_blocking(move || owner.test_close_for_presence(&binding))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "fixture_close_failed");
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 0);
    let loss = f.loss();
    assert_eq!(
        f.gate
            .commit_confirmed_native_presence_loss(&f.owner, &loss)
            .unwrap_err(),
        "physical_loss_unconfirmed"
    );
    f.gate.abort_native_transition(&loss).unwrap();
    f.owner.test_ack_presence_loss(&f.session);
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    assert!(f.gate.native_multi_current(&parent));
    assert!(f.owner.native_open(READER).is_err());
    assert!(f.owner.shutdown().await.is_err());
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    // Real later quarantine cleanup is possible, but it is not a presence
    // receipt and must not retrospectively certify original-profile recovery.
    f.trace.close_fails.store(false, Ordering::SeqCst);
    f.owner.shutdown().await.unwrap();
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 1);
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logical_loss_receipt_without_owner_proof_does_not_release_parent() {
    let f = Fixture::new();
    f.open_physical();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    let loss = f.loss();
    f.gate
        .commit_native_transition(&loss, None, NativeReaderFacts::default())
        .unwrap();
    assert!(!f.gate.native_multi_current(&parent));
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    assert_eq!(
        f.gate.release_native_multi(&parent),
        Err("stale_reader_generation")
    );
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 0);
    let owner = f.owner.clone();
    let binding = f.session.clone();
    assert!(
        tokio::task::spawn_blocking(move || owner.test_close_for_presence(&binding))
            .await
            .unwrap()
            .unwrap()
    );
    // Exact committed receipt may be replayed only under real Owner proof.
    f.gate
        .commit_confirmed_native_presence_loss(&f.owner, &loss)
        .unwrap();
    f.owner.test_ack_presence_loss(&f.session);
    assert!(
        f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    f.owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn physical_close_does_not_replace_actual_operation_join() {
    let f = Fixture::new();
    f.open_physical();
    let parent = f.gate.acquire_native_multi(READER, 1).unwrap();
    let op = f
        .gate
        .begin_native_multi_operation(f.owner.clone(), &parent)
        .unwrap();
    let permit = f
        .gate
        .track_card_io(&op.transaction, PRODUCER, READER)
        .unwrap();
    let owner = f.owner.clone();
    let binding = f.session.clone();
    assert!(
        tokio::task::spawn_blocking(move || owner.test_close_for_presence(&binding))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 1);
    assert!(
        f.gate
            .prepare_native_transition(
                PRODUCER,
                &f.gate.native_reader_snapshot().0,
                NativeTransitionKind::Loss,
                READER,
                NativeAuthority::Loss(f.session.clone())
            )
            .is_err()
    );
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    drop(permit);
    f.gate.finish_native_multi_operation(&op).await.unwrap();
    let loss = f.loss();
    f.gate
        .commit_confirmed_native_presence_loss(&f.owner, &loss)
        .unwrap();
    f.owner.test_ack_presence_loss(&f.session);
    assert!(
        f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    f.owner.shutdown().await.unwrap();
}

#[test]
fn ble_logical_loss_has_no_exact_physical_close_proof() {
    let gate = Gate::default();
    gate.initialize_native_reader_producer(PRODUCER);
    gate.initialize_native_lifecycle(PRODUCER).unwrap();
    gate.set_ordinary_admission(Arc::new(crate::ordinary_vowifi_admission::Gate::default()))
        .unwrap();
    let reader = "BLE:fixture";
    let connect = gate
        .prepare_native_transition(
            PRODUCER,
            &SelectionSnapshot::default(),
            NativeTransitionKind::Connect,
            reader,
            NativeAuthority::Idle,
        )
        .unwrap();
    gate.commit_native_transition(&connect, None, NativeReaderFacts::default())
        .unwrap();
    let parent = gate.acquire_native_multi(reader, 1).unwrap();
    assert!(
        !gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    lifecycle::seed_test_selection(&gate, "", 2);
    assert!(
        !gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    assert_eq!(
        gate.release_native_multi(&parent),
        Err("stale_reader_generation")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ble_pool_shutdown_cannot_infer_close_from_logical_loss() {
    let f = Fixture::new();
    let reader = "BLE:loss-without-close-proof";
    let connect = f
        .gate
        .prepare_native_transition(
            PRODUCER,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Connect,
            reader,
            NativeAuthority::Idle,
        )
        .unwrap();
    f.gate
        .commit_native_transition(&connect, None, NativeReaderFacts::default())
        .unwrap();
    let service =
        crate::multistandby_card::Service::new(f.owner.clone(), f.gate.clone(), None, None);
    let pool = crate::multistandby_native_card::Pool::acquire(
        f.owner.clone(),
        f.gate.clone(),
        service,
        reader.into(),
        2,
    )
    .unwrap();
    lifecycle::seed_test_selection(&f.gate, "", 3);
    assert!(pool.shutdown_receipt().await.is_err());
    assert!(pool.retry_shutdown_receipt().await.is_err());
    assert!(f.gate.0.lock().unwrap().native_multi.active());
    assert_eq!(f.trace.opened.load(Ordering::SeqCst), 0);
    assert_eq!(f.trace.closed.load(Ordering::SeqCst), 0);
    drop(pool);
    f.close().await;
}

fn ble_binding(epoch: &str) -> crate::reader_ble_apdu::ConnectionBinding {
    crate::reader_ble_apdu::ConnectionBinding {
        reader: "BLE:12345678-1234-1234-1234-123456789abc".into(),
        bridge_id: "fixture-bridge".into(),
        connection_id: format!("connection-{epoch}"),
        close_operation_id: format!("close-{epoch}"),
    }
}
fn select_ble(f: &Fixture, binding: &crate::reader_ble_apdu::ConnectionBinding) {
    let connect = f
        .gate
        .prepare_native_transition(
            PRODUCER,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Connect,
            &binding.reader,
            NativeAuthority::Idle,
        )
        .unwrap();
    f.gate
        .commit_native_transition(
            &connect,
            None,
            NativeReaderFacts {
                ble_connection: Some(binding.clone()),
                ..Default::default()
            },
        )
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_ble_close_waits_rust_apdu_join_and_old_receipt_cannot_release_reconnection() {
    let f = Fixture::new();
    let binding = ble_binding("first");
    select_ble(&f, &binding);
    let parent = f.gate.acquire_native_multi(&binding.reader, 2).unwrap();
    let operation = f
        .gate
        .begin_native_multi_operation(f.owner.clone(), &parent)
        .unwrap();
    let metadata_worker = f
        .gate
        .track_card_io(&operation.transaction, PRODUCER, &binding.reader)
        .unwrap();
    f.gate
        .publish_multistandby_metadata(
            &operation.transaction,
            &binding.reader,
            2,
            NativeReaderFacts {
                metadata: Some(serde_json::json!({"imsi":"001010123456789"})),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        f.gate.ble_connection_binding(&binding.reader),
        Some(binding.clone()),
        "profile metadata cannot erase or replace the captured connection"
    );
    drop(metadata_worker);
    let physical = f.gate.track_ble_io(&binding.reader).unwrap();
    let proof =
        crate::reader_ble_apdu::close_tests::proof(&binding, serde_json::json!({})).unwrap();
    assert!(
        f.gate
            .prepare_native_transition(
                PRODUCER,
                &f.gate.native_reader_snapshot().0,
                NativeTransitionKind::Loss,
                &binding.reader,
                NativeAuthority::BleLoss(binding.clone())
            )
            .is_err()
    );
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            f.gate.finish_native_multi_operation(&operation)
        )
        .await
        .is_err()
    );
    drop(physical);
    f.gate
        .finish_native_multi_operation(&operation)
        .await
        .unwrap();
    let loss = f
        .gate
        .prepare_native_transition(
            PRODUCER,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Loss,
            &binding.reader,
            NativeAuthority::BleLoss(binding.clone()),
        )
        .unwrap();
    f.gate
        .commit_native_transition(&loss, None, NativeReaderFacts::default())
        .unwrap();
    assert!(
        !f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap(),
        "logical loss is not physical proof"
    );
    f.gate
        .commit_confirmed_ble_presence_loss(&proof, &loss)
        .unwrap();
    assert!(
        f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    let replacement = ble_binding("replacement");
    select_ble(&f, &replacement);
    let next = f.gate.acquire_native_multi(&replacement.reader, 4).unwrap();
    let next_loss = f
        .gate
        .prepare_native_transition(
            PRODUCER,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Loss,
            &replacement.reader,
            NativeAuthority::BleLoss(replacement.clone()),
        )
        .unwrap();
    assert!(
        f.gate
            .commit_confirmed_ble_presence_loss(&proof, &next_loss)
            .is_err()
    );
    f.gate.abort_native_transition(&next_loss).unwrap();
    assert!(
        f.gate
            .release_native_multi_after_confirmed_loss(&parent)
            .unwrap()
    );
    assert!(f.gate.native_multi_current(&next));
    f.gate.release_native_multi(&next).unwrap();
    f.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_ble_pool_can_retry_shutdown_after_delayed_exact_close_receipt() {
    let f = Fixture::new();
    let binding = ble_binding("pool");
    select_ble(&f, &binding);
    let service =
        crate::multistandby_card::Service::new(f.owner.clone(), f.gate.clone(), None, None);
    let pool = crate::multistandby_native_card::Pool::acquire(
        f.owner.clone(),
        f.gate.clone(),
        service,
        binding.reader.clone(),
        2,
    )
    .unwrap();
    let loss = f
        .gate
        .prepare_native_transition(
            PRODUCER,
            &f.gate.native_reader_snapshot().0,
            NativeTransitionKind::Loss,
            &binding.reader,
            NativeAuthority::BleLoss(binding.clone()),
        )
        .unwrap();
    f.gate
        .commit_native_transition(&loss, None, NativeReaderFacts::default())
        .unwrap();
    assert!(pool.shutdown_receipt().await.is_err());
    assert!(f.gate.0.lock().unwrap().native_multi.active());
    assert!(
        crate::reader_ble_apdu::close_tests::proof(&binding, serde_json::json!({"closed":false}))
            .is_none()
    );
    assert!(pool.retry_shutdown_receipt().await.is_err());
    let proof =
        crate::reader_ble_apdu::close_tests::proof(&binding, serde_json::json!({})).unwrap();
    f.gate
        .commit_confirmed_ble_presence_loss(&proof, &loss)
        .unwrap();
    assert_eq!(
        pool.retry_shutdown_receipt().await.unwrap(),
        crate::multistandby_native_card::CleanupReceipt::ConfirmedReaderLost
    );
    assert!(!f.gate.0.lock().unwrap().native_multi.active());
    f.close().await;
}
