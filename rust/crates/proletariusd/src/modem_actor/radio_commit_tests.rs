// Real owner admission, real actor mailbox and PTY writes. No physical modem.
use super::*;
use crate::availability::BackendAvailability;
use crate::module_radio::{CallModeReservation, Service};
use crate::module_radio_transport::VoiceConfiguration;
use serialport::SerialPort as _;
use std::path::PathBuf;

const PRODUCER: &str = "serial-commit-owner";
const WAIT: Duration = Duration::from_secs(5);
include!("radio_restore_tests.rs");

struct Fixture {
    root: PathBuf,
    lifetime: Option<crate::daemon_lifetime::Lease>,
    gate: crate::module_recovery::Gate,
    actor: ModemActor,
    peer: serialport::TTYPort,
    reopened: std::sync::mpsc::Receiver<serialport::TTYPort>,
    session: NativeSerialSession,
    service: Service,
    reservation: CallModeReservation,
}

fn line(peer: &mut serialport::TTYPort) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 4096);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes).unwrap().trim().into();
        }
    }
}

fn voice() -> Action {
    Action::CallModeVoice {
        target: VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        },
    }
}

impl Fixture {
    async fn new() -> Self {
        Self::new_with_reopen(false).await
    }
    async fn new_with_reopen(reopen: bool) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/r582-serial")
            .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&root).unwrap();
        let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        let (mut peer, slave) = serialport::TTYPort::pair().unwrap();
        peer.set_timeout(WAIT).unwrap();
        let path = slave.name().unwrap();
        let retry_path = path.clone();
        let (reopen_event, reopened) = std::sync::mpsc::channel();
        let port = std::sync::Mutex::new(Some(slave));
        let actor = ModemActor::with_test_port_opener_and_module_recovery(
            move |path, baud| {
                if let Some(port) = port.lock().unwrap().take() {
                    return Ok(Box::new(port) as Box<dyn serialport::SerialPort>);
                }
                assert!(reopen, "one PTY open");
                // A fresh safe PTY pair models the replacement at the existing
                // injected physical opener. macOS PTYs lack the builder's
                // hardware baud ioctl; this is not native path acceptance.
                assert_eq!(path, retry_path);
                assert_eq!(baud, 115200);
                let (mut peer, slave) = serialport::TTYPort::pair().map_err(|_| {
                    ActorError::new(
                        "serial_test_reopen_failed",
                        "private replacement PTY failed",
                    )
                })?;
                peer.set_timeout(WAIT).unwrap();
                reopen_event.send(peer).map_err(|_| {
                    ActorError::new(
                        "serial_test_reopen_failed",
                        "replacement PTY peer not retained",
                    )
                })?;
                Ok(Box::new(slave) as Box<dyn serialport::SerialPort>)
            },
            gate.clone(),
        )
        .unwrap();
        actor.claim_native_serial_management().unwrap();
        let session = actor.open_native_serial(path, 115_200).await.unwrap();
        let owner = actor.clone();
        let key = session.clone();
        let initialize = tokio::spawn(async move { owner.initialize_native_serial(&key).await });
        for expected in [
            "AT",
            "ATE0",
            "AT+CMGF=0",
            "AT+CNMI=2,1,0,0,0",
            "AT+CLIP=1",
            "AT+QPCMV=1,2",
        ] {
            assert_eq!(line(&mut peer), expected);
            peer.write_all(b"OK\r\n").unwrap();
        }
        let initialized = initialize.await.unwrap().unwrap();
        assert!(initialized.completed && initialized.probe_succeeded && initialized.sms_pdu_ready);
        let service = Service::new(
            root.clone(),
            crate::usb_at::Owner::unavailable_for_tests(PRODUCER.into())
                .with_recovery_gate_for_tests(gate.clone()),
            actor.clone(),
            BackendAvailability::with_module_recovery(gate.clone()),
            PRODUCER.into(),
        );
        service
            .configure_native_serial(session.serial_session)
            .await
            .unwrap();
        let reservation = service.reserve_call_mode_operation().await.unwrap();
        Self {
            root,
            lifetime: Some(lifetime),
            gate,
            actor,
            peer,
            reopened,
            session,
            service,
            reservation,
        }
    }

    fn begin_voice(
        &self,
    ) -> tokio::task::JoinHandle<Result<Outcome, crate::module_radio::Failure>> {
        let owner = self.service.clone();
        let reservation = self.reservation.clone();
        tokio::spawn(async move { owner.call_mode_action(&reservation, voice()).await })
    }

    fn answer(&mut self, expected: &str, reply: &[u8]) {
        assert_eq!(line(&mut self.peer), expected);
        self.peer.write_all(reply).unwrap();
    }

    fn voice_reads(&mut self) {
        self.answer("AT+CLCC", b"OK\r\n");
        self.answer(r#"AT+QCFG="ims""#, b"+QCFG: \"ims\",2,0\r\nOK\r\n");
        assert_eq!(line(&mut self.peer), r#"AT+QCFG="volte_disable""#);
    }

    fn replace_lock(&self) {
        let lock = self.root.join("call-mode-operation.lock");
        std::fs::rename(&lock, self.root.join("original-operation.lock")).unwrap();
        std::fs::write(&lock, b"").unwrap();
    }

    fn no_more_bytes(&mut self) {
        self.peer.set_timeout(Duration::from_millis(100)).unwrap();
        let mut byte = [0];
        assert!(
            matches!(self.peer.read(&mut byte), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
        );
        self.peer.set_timeout(WAIT).unwrap();
    }

    async fn finish(&self) {
        // Both acknowledgements must come from the original owners, while the
        // operation namespace can still be invalid and the outcome unknown.
        self.service
            .release_call_mode_operation(&self.reservation)
            .await
            .unwrap();
        self.actor.close_native_serial(&self.session).await.unwrap();
        assert!(self.session.observation().serial_session.is_none());
        self.service.finish_shutdown().await.unwrap();
        self.actor.shutdown().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        queued_radio_receipts()
            .lock()
            .unwrap()
            .remove(&(Arc::as_ptr(&self.actor.client) as usize));
        let _ = self.actor.shutdown();
        self.lifetime.take();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn outcome(
    task: tokio::task::JoinHandle<Result<Outcome, crate::module_radio::Failure>>,
) -> Outcome {
    tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_owner_proof_reaches_both_serial_configuration_writes() {
    let mut f = Fixture::new().await;
    let task = f.begin_voice();
    f.voice_reads();
    f.peer
        .write_all(b"+QCFG: \"volte_disable\",1\r\nOK\r\n")
        .unwrap();
    f.answer(r#"AT+QCFG="volte_disable",0"#, b"OK\r\n");
    f.answer(r#"AT+QCFG="ims",1"#, b"OK\r\n");
    f.answer(r#"AT+QCFG="ims""#, b"+QCFG: \"ims\",2,0\r\nOK\r\n");
    f.answer(
        r#"AT+QCFG="volte_disable""#,
        b"+QCFG: \"volte_disable\",0\r\nOK\r\n",
    );
    let actual = outcome(task).await;
    assert!(actual.complete && actual.verified && actual.submitted && !actual.uncertain);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_accepted_proof_is_rechecked_after_operation_lock_replacement() {
    let mut f = Fixture::new().await;
    let (entered, started) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    f.actor
        .client
        .sender
        .send(Command::LegacyGate(Box::new(move |_, _| {
            entered.send(()).unwrap();
            wait.recv_timeout(WAIT).unwrap();
        })))
        .unwrap();
    f.actor.client.wake.notify_one();
    started.recv_timeout(WAIT).unwrap();
    let (receipt, queued) = mpsc::sync_channel(1);
    let key = Arc::as_ptr(&f.actor.client) as usize;
    assert!(
        queued_radio_receipts()
            .lock()
            .unwrap()
            .insert(key, receipt)
            .is_none()
    );
    let task = f.begin_voice();
    queued.recv_timeout(WAIT).unwrap();
    // A valid proof was minted and its actual Command::Radio entered the
    // blocked actor's mailbox. Replacement occurs before that dequeue.
    f.service
        .pending_call_mode_commit_for_tests(&voice())
        .unwrap()
        .validate_for_binding(
            &voice(),
            &Binding::Serial {
                serial_session: f.session.serial_session,
            },
        )
        .unwrap();
    f.replace_lock();
    release.send(()).unwrap();
    f.voice_reads();
    f.peer
        .write_all(b"+QCFG: \"volte_disable\",1\r\nOK\r\n")
        .unwrap();
    let actual = outcome(task).await;
    assert!(!actual.complete && !actual.submitted && !actual.uncertain);
    assert_eq!(actual.error_code, "call_mode_operation_lock_unavailable");
    f.no_more_bytes();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_root_authority_is_rechecked_between_read_and_serial_write() {
    let mut f = Fixture::new().await;
    let task = f.begin_voice();
    f.voice_reads();
    let proof = f
        .service
        .pending_call_mode_commit_for_tests(&voice())
        .unwrap();
    proof
        .validate_for_binding(
            &voice(),
            &Binding::Serial {
                serial_session: f.session.serial_session,
            },
        )
        .unwrap();
    f.lifetime.take();
    f.peer
        .write_all(b"+QCFG: \"volte_disable\",1\r\nOK\r\n")
        .unwrap();
    let actual = outcome(task).await;
    assert!(!actual.complete && !actual.submitted && !actual.uncertain);
    assert_eq!(actual.error_code, "call_mode_operation_lock_unavailable");
    f.no_more_bytes();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prior_serial_write_remains_submitted_unknown_and_exact_cleanup_still_closes() {
    let mut f = Fixture::new().await;
    let task = f.begin_voice();
    f.voice_reads();
    f.peer
        .write_all(b"+QCFG: \"volte_disable\",1\r\nOK\r\n")
        .unwrap();
    assert_eq!(line(&mut f.peer), r#"AT+QCFG="volte_disable",0"#);
    f.replace_lock();
    f.peer.write_all(b"OK\r\n").unwrap();
    let actual = outcome(task).await;
    assert!(!actual.complete && actual.submitted && actual.uncertain);
    assert_eq!(actual.error_code, "call_mode_operation_lock_unavailable");
    f.no_more_bytes();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_rf_restore_uses_exact_finite_right_without_new_root_namespace() {
    let mut f = Fixture::new().await;
    let operation = f.gate.admit_operation(PRODUCER).unwrap();
    let right = operation.right();
    let owner = f.actor.clone();
    let session = f.session.serial_session;
    let task = tokio::spawn(async move {
        owner
            .radio_execute_with_right(
                session,
                None,
                Action::SetVerify { target: 1 },
                WAIT,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                right,
                PRODUCER.into(),
                None,
            )
            .await
    });
    assert_eq!(line(&mut f.peer), "AT+CFUN?");
    f.replace_lock();
    f.lifetime.take();
    f.gate.revoke_for_tests();
    f.peer.write_all(b"+CFUN: 4\r\nOK\r\n").unwrap();
    f.answer("AT+CLCC", b"OK\r\n");
    f.answer("AT+CFUN=1", b"OK\r\n");
    f.answer("AT+CFUN?", b"+CFUN: 1\r\nOK\r\n");
    let actual = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(actual.complete && actual.verified && actual.submitted && !actual.uncertain);
    f.finish().await;
    drop(operation);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_gate_and_serial_number_cannot_replay_proof_on_another_actual_actor() {
    let mut f = Fixture::new().await;
    let task = f.begin_voice();
    f.voice_reads();
    let proof = f
        .service
        .pending_call_mode_commit_for_tests(&voice())
        .unwrap();
    proof
        .validate_for_binding(
            &voice(),
            &Binding::Serial {
                serial_session: f.session.serial_session,
            },
        )
        .unwrap();
    let (mut peer, slave) = serialport::TTYPort::pair().unwrap();
    peer.set_timeout(Duration::from_millis(100)).unwrap();
    let path = slave.name().unwrap();
    let port = std::sync::Mutex::new(Some(slave));
    let other = ModemActor::with_test_port_opener_and_module_recovery(
        move |_, _| Ok(Box::new(port.lock().unwrap().take().unwrap())),
        f.gate.clone(),
    )
    .unwrap();
    other.claim_native_serial_management().unwrap();
    let session = other.open_native_serial(path, 115_200).await.unwrap();
    assert_eq!(session.serial_session, f.session.serial_session);
    assert!(!other.same_instance(&f.actor));
    // This is the private unscoped entry that could otherwise confuse two
    // real actors sharing one Gate and identical numeric session counters.
    let failure = other
        .radio_execute_tagged(
            session.serial_session,
            None,
            voice(),
            WAIT,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
            Some(proof),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, "radio_stale_generation");
    let mut byte = [0];
    assert!(
        matches!(peer.read(&mut byte), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
    );
    other.close_native_serial(&session).await.unwrap();
    other.shutdown().unwrap();
    // The helper observed the pending owner and minted another sealed proof;
    // rejection poisons that proof's clones, not the owner's separate proof.
    // The real original actor can still complete its accepted finite action.
    f.peer
        .write_all(b"+QCFG: \"volte_disable\",1\r\nOK\r\n")
        .unwrap();
    f.answer(r#"AT+QCFG="volte_disable",0"#, b"OK\r\n");
    f.answer(r#"AT+QCFG="ims",1"#, b"OK\r\n");
    f.answer(r#"AT+QCFG="ims""#, b"+QCFG: \"ims\",2,0\r\nOK\r\n");
    f.answer(
        r#"AT+QCFG="volte_disable""#,
        b"+QCFG: \"volte_disable\",0\r\nOK\r\n",
    );
    let actual = outcome(task).await;
    assert!(actual.complete && actual.verified && actual.submitted && !actual.uncertain);
    f.no_more_bytes();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_serial_configuration_without_owner_proof_stops_at_real_write_boundary() {
    let mut f = Fixture::new().await;
    let owner = f.actor.clone();
    let session = f.session.serial_session;
    let task = tokio::spawn(async move { owner.radio_execute(session, voice()).await });
    f.voice_reads();
    f.peer
        .write_all(b"+QCFG: \"volte_disable\",1\r\nOK\r\n")
        .unwrap();
    let actual = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!actual.complete && !actual.submitted && !actual.uncertain);
    assert_eq!(actual.error_code, "call_mode_operation_lock_unavailable");
    f.no_more_bytes();
    f.finish().await;
}
