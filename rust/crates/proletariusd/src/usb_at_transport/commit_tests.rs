// Scripted bytes through the production owner, actor, Plan and NativeTransport.
// Synthetic targets/anchors below represent actor ownership only. No native
// USB Device or native physical pairing/successor evidence is manufactured.
use super::*;
use crate::module_radio_transport::{Action as RadioAction, VoiceConfiguration};
use std::path::{Path, PathBuf};

const PRODUCER: &str = "usb-commit-boundary";
enum Wire {
    Quiet,
    Reply(Vec<u8>),
    Write(Vec<u8>, Option<usize>),
    Change(Box<dyn FnOnce() + Send>),
}
#[derive(Default)]
struct Trace {
    script: VecDeque<Wire>,
    writes: Vec<Vec<u8>>,
    accepted: Vec<Vec<u8>>,
    opens: usize,
    drops: usize,
    target: Option<PhysicalTarget>,
    anchor_available: bool,
}
struct ScriptIo(Arc<Mutex<Trace>>);
impl ScriptIo {
    fn next(&self) -> Wire {
        loop {
            let event = self
                .0
                .lock()
                .unwrap()
                .script
                .pop_front()
                .expect("unexpected physical I/O");
            match event {
                Wire::Change(change) => change(),
                event => return event,
            }
        }
    }
}
impl Io for ScriptIo {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
        self.0
            .lock()
            .unwrap()
            .target
            .clone()
            .ok_or_else(target_unconfirmed)
    }
    fn capture_reenumeration_anchor(
        &mut self,
        deadline: Instant,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<ReenumerationAnchor, TransportError> {
        reconnect::check_finite(deadline, &guard)?;
        let trace = self.0.lock().unwrap();
        if !trace.anchor_available {
            return Err(target_unconfirmed());
        }
        Ok(ReenumerationAnchor::synthetic_for_tests(
            trace.target.clone().ok_or_else(target_unconfirmed)?,
            (0x2c7c, 0x0125),
        ))
    }
    fn read(&mut self, buffer: &mut [u8], _: Duration) -> Result<usize, IoError> {
        match self.next() {
            Wire::Quiet => Err(IoError::Timeout),
            Wire::Reply(bytes) => {
                assert!(bytes.len() <= buffer.len());
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            _ => panic!("unexpected read at physical write boundary"),
        }
    }
    fn write(&mut self, bytes: &[u8], _: Duration) -> Result<usize, IoError> {
        match self.next() {
            Wire::Write(expected, accepted) => {
                assert_eq!(bytes, expected);
                let accepted = accepted.unwrap_or(bytes.len());
                assert!(accepted <= bytes.len());
                let mut trace = self.0.lock().unwrap();
                trace.writes.push(bytes.to_vec());
                trace.accepted.push(bytes[..accepted].to_vec());
                Ok(accepted)
            }
            _ => panic!("unexpected physical write"),
        }
    }
}
impl Drop for ScriptIo {
    fn drop(&mut self) {
        self.0.lock().unwrap().drops += 1;
    }
}
fn transaction(command: &str, response: &str) -> Vec<Wire> {
    vec![
        Wire::Quiet,
        Wire::Write(format!("{command}\r").into_bytes(), None),
        Wire::Reply(response.as_bytes().to_vec()),
    ]
}
fn voice_reads() -> Vec<Wire> {
    transaction("AT+CLCC", "\r\nOK\r\n")
        .into_iter()
        .chain(transaction(
            "AT+QCFG=\"ims\"",
            "+QCFG: \"ims\",0,1\r\nOK\r\n",
        ))
        .chain(transaction(
            "AT+QCFG=\"volte_disable\"",
            "+QCFG: \"volte_disable\",1\r\nOK\r\n",
        ))
        .collect()
}
fn preflight_identity_reads() -> Vec<Wire> {
    transaction("AT+CGSN", "123456789012345\r\nOK\r\n")
        .into_iter()
        .chain(transaction("ATI", "fixture-fw\r\nOK\r\n"))
        .collect()
}
fn owned_voice_reads() -> Vec<Wire> {
    preflight_identity_reads()
        .into_iter()
        .chain(voice_reads())
        .collect()
}
fn voice_target() -> RadioAction {
    RadioAction::CallModeVoice {
        target: VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        },
    }
}

struct Root {
    path: PathBuf,
    retained: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
}
impl Root {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/usb-commit-boundary-tests")
            .join(crate::snapshot::new_instance_id());
        let lease = crate::daemon_lifetime::Lease::acquire(&path).unwrap();
        let retained = path.with_extension("retained");
        Self {
            path,
            retained,
            lease: Some(lease),
        }
    }
    fn replacement(&self, replace_root: bool) -> Wire {
        let root = self.path.clone();
        let retained = self.retained.clone();
        Wire::Change(Box::new(move || {
            if replace_root {
                std::fs::rename(&root, retained).unwrap();
                std::fs::create_dir(&root).unwrap();
            } else {
                let lock = root.join("call-mode-operation.lock");
                std::fs::rename(&lock, root.join("held-operation.lock")).unwrap();
                std::fs::write(lock, b"replacement lock namespace").unwrap();
            }
        }))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        drop(self.lease.take());
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_dir_all(&self.retained);
    }
}

struct Actor {
    root: Root,
    usb: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    modem: crate::modem_actor::ModemActor,
    reservation: crate::module_radio::CallModeReservation,
    session: String,
    trace: Arc<Mutex<Trace>>,
}
impl Actor {
    async fn new() -> Self {
        let root = Root::new();
        let gate =
            crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
        let trace = Arc::new(Mutex::new(Trace {
            target: Some(PhysicalTarget::synthetic_for_tests()),
            anchor_available: true,
            ..Trace::default()
        }));
        let opened = trace.clone();
        let usb = crate::usb_at::Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move || {
                opened.lock().unwrap().opens += 1;
                Ok(Box::new(NativeTransport::new(
                    Box::new(ScriptIo(opened.clone())),
                    "scripted physical USB I/O".into(),
                    None,
                )))
            }),
        )
        .with_recovery_gate_for_tests(gate.clone());
        usb.claim_native_management().unwrap();
        let session = usb.open_native_session().await.unwrap();
        let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
        let radio = crate::module_radio::Service::new(
            root.path.clone(),
            usb.clone(),
            modem.clone(),
            crate::availability::BackendAvailability::with_module_recovery(gate),
            PRODUCER.into(),
        );
        radio.configure_native_usb(&session).await.unwrap();
        let reservation = radio.reserve_call_mode_operation().await.unwrap();
        Self {
            root,
            usb,
            radio,
            modem,
            reservation,
            session,
            trace,
        }
    }
    fn script(&self, events: Vec<Wire>) {
        self.trace.lock().unwrap().script.extend(events);
    }
    async fn voice(&self) -> crate::module_radio_transport::Outcome {
        self.radio
            .call_mode_action(&self.reservation, voice_target())
            .await
            .unwrap()
    }
    async fn close(&self) {
        self.radio
            .release_call_mode_operation(&self.reservation)
            .await
            .unwrap();
        self.usb.shutdown().await.unwrap();
        self.modem.shutdown().unwrap();
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        if let Err(error) = self.modem.shutdown() {
            if !std::thread::panicking() {
                panic!("controlled modem shutdown: {error:?}");
            }
        }
    }
}

#[tokio::test]
async fn direct_usb_config_without_owner_commit_proof_cannot_write() {
    let actor = Actor::new().await;
    actor.script(voice_reads());
    let outcome = actor
        .usb
        .radio_execute(PRODUCER.into(), actor.session.clone(), voice_target(), None)
        .await
        .unwrap();
    assert!(!outcome.complete && !outcome.submitted && !outcome.uncertain);
    assert_eq!(outcome.error_code, "call_mode_operation_lock_unavailable");
    assert_eq!(actor.trace.lock().unwrap().writes.len(), 3);
    assert_eq!(actor.trace.lock().unwrap().opens, 1);
    actor.close().await;
}

#[tokio::test]
async fn actual_owner_preflight_without_target_or_anchor_sends_no_configuration_or_identity() {
    for missing_target in [false, true] {
        let actor = Actor::new().await;
        {
            let mut trace = actor.trace.lock().unwrap();
            if missing_target {
                trace.target = None;
            } else {
                trace.anchor_available = false;
            }
        }
        let error = actor
            .radio
            .call_mode_action(&actor.reservation, voice_target())
            .await
            .err()
            .unwrap();
        assert_eq!(error.public_code(), "usb_at_target_unconfirmed");
        assert!(!error.public_submitted() && !error.public_uncertain());
        assert!(actor.trace.lock().unwrap().writes.is_empty());
        assert_eq!(actor.trace.lock().unwrap().opens, 1);
        actor.close().await;
    }
}

#[test]
fn accepted_usb_worker_queue_rechecks_minted_proof_after_namespace_replacement() {
    // A second operation on the same USB owner is fail-fast busy. Exercise
    // the real Owner::run queue instead: one accepted read on another owner
    // occupies the single blocking executor while this owner accepts its job.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let actor = Actor::new().await;
            let trace = Arc::new(Mutex::new(Trace::default()));
            let opened = trace.clone();
            let blocker = crate::usb_at::Owner::with_factory(
                "usb-queue-read-blocker".into(),
                Arc::new(move || {
                    opened.lock().unwrap().opens += 1;
                    Ok(Box::new(NativeTransport::new(
                        Box::new(ScriptIo(opened.clone())),
                        "queued read blocker".into(),
                        None,
                    )))
                }),
            );
            blocker.claim_native_management().unwrap();
            let session = blocker.open_native_session().await.unwrap();
            let (entered, entry) = tokio::sync::oneshot::channel();
            let (resume, wait) = std::sync::mpsc::sync_channel(1);
            trace.lock().unwrap().script.extend([
                Wire::Quiet,
                Wire::Write(b"AT+CFUN?\r".to_vec(), None),
                Wire::Change(Box::new(move || {
                    entered.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                })),
                Wire::Reply(b"+CFUN: 4\r\nOK\r\n".to_vec()),
            ]);
            let reader = blocker.clone();
            let read = tokio::spawn(async move {
                reader
                    .radio_execute(
                        "usb-queue-read-blocker".into(),
                        session,
                        RadioAction::Observe { cellular: false },
                        None,
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), entry)
                .await
                .expect("the accepted read did not enter the blocking executor")
                .unwrap();
            actor.script(owned_voice_reads());
            let spawned_before = actor.usb.spawned_io_for_tests();
            let radio = actor.radio.clone();
            let reservation = actor.reservation.clone();
            let work =
                tokio::spawn(
                    async move { radio.call_mode_action(&reservation, voice_target()).await },
                );
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if actor.usb.spawned_io_for_tests() > spawned_before
                        && actor.radio.pending_call_mode_rebind_for_tests().is_ok()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("configuration job was not actually spawned behind the blocked worker");
            assert!(!work.is_finished());
            assert!(actor.trace.lock().unwrap().writes.is_empty());
            if let Wire::Change(change) = actor.root.replacement(false) {
                change();
            }
            resume.send(()).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(5), read)
                    .await
                    .expect("the accepted read worker did not join")
                    .unwrap()
                    .unwrap()
                    .complete
            );
            let error = tokio::time::timeout(Duration::from_secs(5), work)
                .await
                .expect("the queued configuration worker did not join")
                .unwrap()
                .err()
                .unwrap();
            assert_eq!(error.public_code(), "call_mode_reconnect_unconfirmed");
            assert!(!error.public_submitted() && !error.public_uncertain());
            assert_eq!(
                actor.trace.lock().unwrap().writes.len(),
                0,
                "the actual queued preflight rejects namespace loss before CGSN or configuration"
            );
            assert_eq!(actor.trace.lock().unwrap().opens, 1);
            blocker.shutdown().await.unwrap();
            actor.close().await;
        });
}

#[tokio::test]
async fn actual_usb_producer_revocation_during_drain_blocks_the_next_configuration_write() {
    let actor = Actor::new().await;
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (resume, wait) = std::sync::mpsc::sync_channel(1);
    let mut events = owned_voice_reads();
    events.extend([
        Wire::Change(Box::new(move || {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
        })),
        Wire::Quiet,
    ]);
    actor.script(events);
    let radio = actor.radio.clone();
    let reservation = actor.reservation.clone();
    let work =
        tokio::spawn(async move { radio.call_mode_action(&reservation, voice_target()).await });
    tokio::time::timeout(Duration::from_secs(5), entry)
        .await
        .expect("the actual NativeTransport never entered its configuration drain")
        .unwrap();
    let mut revoke = Box::pin(
        actor
            .usb
            .replace_producer("replacement-usb-producer".into()),
    );
    // Poll the real API through its synchronous epoch/producer/session revoke.
    // Its pending cleanup must wait for this original, still-held I/O worker.
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(revoke.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(!work.is_finished());
    assert_eq!(actor.trace.lock().unwrap().writes.len(), 5);
    resume.send(()).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .expect("the original I/O worker did not settle")
        .unwrap()
        .unwrap();
    assert!(!outcome.complete && !outcome.submitted && !outcome.uncertain);
    tokio::time::timeout(Duration::from_secs(5), revoke)
        .await
        .expect("producer retirement did not join the accepted I/O")
        .unwrap();
    {
        let trace = actor.trace.lock().unwrap();
        assert_eq!(
            trace.writes.len(),
            5,
            "two preflight and three config reads, no configuration bytes"
        );
        assert_eq!(trace.opens, 1, "no replacement transport was opened");
        assert_eq!(trace.drops, 1, "the original transport was actually closed");
        assert!(trace.script.is_empty());
    }
    actor.close().await;
}

#[tokio::test]
async fn owner_minted_usb_commit_rechecks_after_read_before_any_write_drain() {
    let actor = Actor::new().await;
    let mut events = owned_voice_reads();
    let reply = events.pop().unwrap();
    events.extend([actor.root.replacement(false), reply]);
    actor.script(events);
    let outcome = actor.voice().await;
    assert!(!outcome.complete && !outcome.submitted && !outcome.uncertain);
    assert_eq!(outcome.error_code, "call_mode_operation_lock_unavailable");
    assert_eq!(actor.trace.lock().unwrap().writes.len(), 5);
    assert!(
        actor.trace.lock().unwrap().script.is_empty(),
        "no next-step drain or write was attempted"
    );
    actor.close().await;
}

#[tokio::test]
async fn owner_minted_usb_commit_rejects_root_or_lock_replacement_during_first_write_drain() {
    for replace_root in [false, true] {
        let actor = Actor::new().await;
        let mut events = owned_voice_reads();
        events.extend([actor.root.replacement(replace_root), Wire::Quiet]);
        actor.script(events);
        let outcome = actor.voice().await;
        assert!(!outcome.complete && !outcome.submitted && !outcome.uncertain);
        assert_eq!(actor.trace.lock().unwrap().writes.len(), 5);
        assert_eq!(actor.trace.lock().unwrap().opens, 1);
        // Namespace revocation does not strand the accepted operation owner.
        actor.close().await;
        assert_eq!(actor.trace.lock().unwrap().drops, 1);
    }
}

#[tokio::test]
async fn owner_minted_qadb_challenge_never_authorizes_after_namespace_loss() {
    for during_drain in [false, true] {
        let actor = Actor::new().await;
        let mut events = preflight_identity_reads();
        events.extend(transaction("AT+CLCC", "\r\nOK\r\n"));
        events.extend(transaction("AT+QADBKEY?", "+QADBKEY: 12345678\r\nOK\r\n"));
        if during_drain {
            events.extend([actor.root.replacement(false), Wire::Quiet]);
        } else {
            let response = events.pop().unwrap();
            events.extend([actor.root.replacement(false), response]);
        }
        actor.script(events);
        let outcome = actor
            .radio
            .call_mode_action(&actor.reservation, RadioAction::CallModeAdbAuthorize)
            .await
            .unwrap();
        assert!(!outcome.complete && !outcome.submitted && !outcome.uncertain);
        assert!(!outcome.response.contains("12345678"));
        let writes = actor.trace.lock().unwrap().writes.clone();
        assert_eq!(
            writes,
            [
                b"AT+CGSN\r".to_vec(),
                b"ATI\r".to_vec(),
                b"AT+CLCC\r".to_vec(),
                b"AT+QADBKEY?\r".to_vec()
            ]
        );
        assert!(actor.trace.lock().unwrap().script.is_empty());
        actor.close().await;
        assert_eq!(actor.trace.lock().unwrap().opens, 1);
    }
}

#[tokio::test]
async fn owner_minted_usb_commit_preserves_prior_write_unknown_when_later_payload_is_denied() {
    let actor = Actor::new().await;
    let mut events = owned_voice_reads();
    events.extend(transaction("AT+QCFG=\"volte_disable\",0", "\r\nOK\r\n"));
    events.extend([actor.root.replacement(false), Wire::Quiet]);
    actor.script(events);
    let outcome = actor.voice().await;
    assert!(!outcome.complete && outcome.submitted && outcome.uncertain);
    {
        let trace = actor.trace.lock().unwrap();
        assert_eq!(trace.writes.len(), 6);
        assert_eq!(
            trace.writes.last().unwrap(),
            b"AT+QCFG=\"volte_disable\",0\r"
        );
    }
    actor.close().await;
    assert_eq!(actor.trace.lock().unwrap().opens, 1);
}

#[tokio::test]
async fn owner_minted_usb_partial_first_commit_is_unknown_and_never_replayed_or_reopened() {
    let actor = Actor::new().await;
    let mut events = owned_voice_reads();
    events.extend([
        Wire::Quiet,
        Wire::Write(b"AT+QCFG=\"volte_disable\",0\r".to_vec(), Some(3)),
    ]);
    actor.script(events);
    let outcome = actor.voice().await;
    assert!(!outcome.complete && outcome.submitted && outcome.uncertain);
    {
        let trace = actor.trace.lock().unwrap();
        assert_eq!(trace.writes.len(), 6);
        assert_eq!(trace.accepted.last().unwrap(), b"AT+");
        assert_eq!(trace.opens, 1);
        assert!(trace.script.is_empty());
    }
    actor.close().await;
    assert_eq!(actor.trace.lock().unwrap().drops, 1);
}

#[test]
fn namespace_guard_denial_before_first_editor_command_has_no_payload_or_escape() {
    let root = Root::new();
    let held =
        Arc::new(crate::call_mode_backups::try_call_mode_operation_file_lock(&root.path).unwrap());
    let trace = Arc::new(Mutex::new(Trace::default()));
    trace
        .lock()
        .unwrap()
        .script
        .extend([root.replacement(false), Wire::Quiet]);
    let mut transport = NativeTransport::new(
        Box::new(ScriptIo(trace.clone())),
        "editor guard fixture".into(),
        None,
    );
    let error = transport
        .command_with_guard(
            "AT+CMGS=1",
            Some(b"body\x1a"),
            Duration::from_secs(1),
            4096,
            Arc::new(move || held.validate().is_ok()),
        )
        .unwrap_err();
    assert_eq!(error.code, "module_call_admission_changed");
    assert!(!error.uncertain);
    assert!(trace.lock().unwrap().writes.is_empty());
}

#[test]
fn namespace_revoked_at_prompt_blocks_followup_but_permits_one_real_escape_cleanup() {
    let root = Root::new();
    let held =
        Arc::new(crate::call_mode_backups::try_call_mode_operation_file_lock(&root.path).unwrap());
    let trace = Arc::new(Mutex::new(Trace::default()));
    trace.lock().unwrap().script.extend([
        Wire::Quiet,
        Wire::Write(b"AT+CMGS=1\r".to_vec(), None),
        root.replacement(false),
        Wire::Reply(b"\r\n> ".to_vec()),
        Wire::Write(vec![0x1b], None),
    ]);
    let mut transport = NativeTransport::new(
        Box::new(ScriptIo(trace.clone())),
        "editor guard fixture".into(),
        None,
    );
    let error = transport
        .command_with_guard(
            "AT+CMGS=1",
            Some(b"body\x1a"),
            Duration::from_secs(1),
            4096,
            Arc::new(move || held.validate().is_ok()),
        )
        .unwrap_err();
    assert_eq!(error.code, "module_call_admission_changed");
    assert!(error.uncertain, "the editor command was actually submitted");
    assert_eq!(
        trace.lock().unwrap().writes,
        [b"AT+CMGS=1\r".to_vec(), vec![0x1b]]
    );
    assert!(trace.lock().unwrap().script.is_empty());
}

#[test]
fn partial_editor_command_keeps_unknown_and_escape_is_not_namespace_gated() {
    let root = Root::new();
    let held =
        Arc::new(crate::call_mode_backups::try_call_mode_operation_file_lock(&root.path).unwrap());
    let trace = Arc::new(Mutex::new(Trace::default()));
    trace.lock().unwrap().script.extend([
        Wire::Quiet,
        Wire::Write(b"AT+CMGS=1\r".to_vec(), Some(3)),
        root.replacement(false),
        Wire::Write(vec![0x1b], None),
    ]);
    let mut transport = NativeTransport::new(
        Box::new(ScriptIo(trace.clone())),
        "editor guard fixture".into(),
        None,
    );
    let error = transport
        .command_with_guard(
            "AT+CMGS=1",
            Some(b"body\x1a"),
            Duration::from_secs(1),
            4096,
            Arc::new(move || held.validate().is_ok()),
        )
        .unwrap_err();
    assert_eq!(error.code, "usb_at_short_write");
    assert!(error.uncertain);
    assert_eq!(
        trace.lock().unwrap().accepted,
        [b"AT+".to_vec(), vec![0x1b]]
    );
    assert_eq!(trace.lock().unwrap().writes.len(), 2);
}
