use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn quarantine_rejects_open_and_queries_before_the_mock_serial_factory() {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let actor = ModemActor::with_port_opener_and_recovery(
        move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Err(ActorError::new(
                "synthetic_serial_trap",
                "no physical serial",
            ))
        },
        crate::module_recovery::Gate::quarantined_for_tests(),
    )
    .unwrap();
    let open = actor.open("/dev/cu.p556-synthetic".into(), 115200);
    let query = actor.typed_query("imei");
    actor.shutdown().unwrap();
    assert_eq!(open.unwrap_err().code(), crate::module_recovery::CODE);
    assert_eq!(query.unwrap_err().code(), crate::module_recovery::CODE);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn compatibility_actor_cannot_bypass_startup_quarantine() {
    let mut actor = ModemActor::with_port_opener_and_recovery(
        |_, _| panic!("quarantined compatibility call opened serial"),
        crate::module_recovery::Gate::quarantined_for_tests(),
    )
    .unwrap();
    actor.compatibility_rpc = true;
    let query = actor.typed_query("imei");
    actor.shutdown().unwrap();
    assert_eq!(query.unwrap_err().code(), crate::module_recovery::CODE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_revoke_blocks_native_direct_senders_before_open_or_discovery() {
    let gate = crate::module_recovery::Gate::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let actor = ModemActor::with_port_opener_and_recovery(
        move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Err(ActorError::unavailable("no physical serial"))
        },
        gate.clone(),
    )
    .unwrap();
    actor.claim_native_serial_management().unwrap();
    gate.revoke_for_tests();
    assert_eq!(
        actor
            .open_native_serial("/dev/cu.revoked-fixture".into(), 115_200)
            .await
            .unwrap_err()
            .code(),
        crate::module_recovery::CODE
    );
    assert_eq!(
        actor.discover_at_port_confirmed().await.unwrap_err().code(),
        crate::module_recovery::CODE
    );
    assert_eq!(
        actor.typed_query("imei").unwrap_err().code(),
        crate::module_recovery::CODE
    );
    assert_eq!(
        actor
            .radio_execute_with_budget(
                1,
                crate::module_radio_transport::Action::Observe { cellular: false },
                Duration::from_secs(1),
                Arc::new(std::sync::atomic::AtomicBool::new(false))
            )
            .await
            .unwrap_err()
            .code,
        crate::module_recovery::CODE
    );
    actor.shutdown().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn live_revoke_is_checked_after_dequeue_not_only_before_submit() {
    let gate = crate::module_recovery::Gate::default();
    let actor = ModemActor::with_port_opener_and_recovery(
        |_, _| panic!("queued new open crossed revocation"),
        gate.clone(),
    )
    .unwrap();
    let (entered, entered_rx) = mpsc::sync_channel(1);
    let (resume, resume_rx) = mpsc::sync_channel(1);
    assert!(
        actor
            .client
            .sender
            .send(Command::LegacyGate(Box::new(move |_, _| {
                entered.send(()).unwrap();
                resume_rx.recv().unwrap();
            })))
            .is_ok()
    );
    entered_rx.recv().unwrap();
    let (reply, result) = mpsc::sync_channel(1);
    assert!(
        actor
            .client
            .sender
            .send(Command::Open {
                path: "/dev/cu.queued-revoked".into(),
                baud_rate: 115_200,
                initialization_required: false,
                reply,
            })
            .is_ok()
    );
    gate.revoke_for_tests();
    resume.send(()).unwrap();
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .code(),
        crate::module_recovery::CODE
    );
    actor.shutdown().unwrap();
}

#[test]
fn live_revoke_preserves_exact_card_operation_and_channel_cleanup() {
    use serialport::SerialPort as _;
    let (mut peer, port) = serialport::TTYPort::pair().unwrap();
    peer.set_timeout(Duration::from_secs(2)).unwrap();
    let port = std::sync::Mutex::new(Some(port));
    let gate = crate::module_recovery::Gate::default();
    let actor = ModemActor::with_port_opener_and_recovery(
        move |_, _| Ok(Box::new(port.lock().unwrap().take().unwrap())),
        gate.clone(),
    )
    .unwrap();
    actor
        .open("/dev/cu.card-revoke-fixture".into(), 115_200)
        .unwrap();
    actor.claim_native_esim_management();
    let binding = actor.capture_serial_card_binding().unwrap();
    let permit = actor
        .card_request(|reply| CardRequest::Begin {
            binding: binding.clone(),
            reply,
        })
        .unwrap();
    let owner = actor.clone();
    let opened = std::thread::spawn(move || {
        owner.card_request(|reply| CardRequest::Command {
            permit,
            command: "AT+CCHO=\"A0000005591010FFFFFFFF8900000100\"".into(),
            timeout: Duration::from_secs(1),
            maximum: 1024,
            reply,
        })
    });
    let read_line = |peer: &mut serialport::TTYPort| {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
            assert!(bytes.len() < 256);
        }
        String::from_utf8(bytes).unwrap()
    };
    assert!(read_line(&mut peer).starts_with("AT+CCHO="));
    gate.revoke_for_tests();
    peer.write_all(b"+CCHO: 1\r\nOK\r\n").unwrap();
    opened.join().unwrap().unwrap();
    // A different transaction cannot acquire the old session after revocation.
    assert_eq!(
        actor
            .card_request(|reply| CardRequest::Begin { binding, reply })
            .unwrap_err()
            .code(),
        crate::module_recovery::CODE
    );
    let owner = actor.clone();
    let ended =
        std::thread::spawn(move || owner.card_request(|reply| CardRequest::End { permit, reply }));
    assert_eq!(read_line(&mut peer), "AT+CCHC=1\r\n");
    peer.write_all(b"OK\r\n").unwrap();
    ended.join().unwrap().unwrap();
    // The old permit no longer grants another command after real cleanup.
    assert!(
        actor
            .card_request(|reply| CardRequest::Command {
                permit,
                command: "AT+QCCID".into(),
                timeout: Duration::from_secs(1),
                maximum: 1024,
                reply,
            })
            .is_err()
    );
    actor.close().unwrap();
    actor.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_native_initialization_finishes_and_exact_close_receipt_survives_revoke() {
    use serialport::SerialPort as _;
    let (mut peer, port) = serialport::TTYPort::pair().unwrap();
    peer.set_timeout(Duration::from_secs(2)).unwrap();
    let path = port.name().unwrap();
    let port = std::sync::Mutex::new(Some(port));
    let gate = crate::module_recovery::Gate::default();
    let actor = ModemActor::with_port_opener_and_recovery(
        move |_, _| Ok(Box::new(port.lock().unwrap().take().unwrap())),
        gate.clone(),
    )
    .unwrap();
    actor.claim_native_serial_management().unwrap();
    let session = actor
        .open_native_serial(path.clone(), 115_200)
        .await
        .unwrap();
    let owner = actor.clone();
    let key = session.clone();
    let pending = tokio::spawn(async move { owner.initialize_native_serial(&key).await });
    for (index, expected) in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ]
    .into_iter()
    .enumerate()
    {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
            assert!(bytes.len() < 256);
        }
        assert_eq!(String::from_utf8(bytes).unwrap().trim(), expected);
        if index == 0 {
            gate.revoke_for_tests();
        }
        peer.write_all(b"OK\r\n").unwrap();
    }
    assert!(pending.await.unwrap().unwrap().completed);
    assert_eq!(
        actor
            .initialize_native_serial(&session)
            .await
            .unwrap_err()
            .code(),
        crate::module_recovery::CODE
    );
    assert_eq!(
        actor
            .refresh_native_serial_metadata(&session)
            .await
            .unwrap_err()
            .code(),
        crate::module_recovery::CODE
    );
    actor.close_native_serial(&session).await.unwrap();
    let terminal = session.observation().terminal.unwrap();
    assert!(terminal.physical_closed);
    assert_eq!(terminal.owner_epoch, session.owner_epoch);
    assert_eq!(terminal.serial_session, session.serial_session);
    // Its historical exact close is idempotent; it cannot reopen the port.
    actor.close_native_serial(&session).await.unwrap();
    assert_eq!(
        actor
            .open_native_serial(path, 115_200)
            .await
            .unwrap_err()
            .code(),
        crate::module_recovery::CODE
    );
    actor.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_hold_is_not_new_operation_authority_after_revoke_but_release_settles() {
    use serialport::SerialPort as _;
    let (_peer, port) = serialport::TTYPort::pair().unwrap();
    let path = port.name().unwrap();
    let port = std::sync::Mutex::new(Some(port));
    let gate = crate::module_recovery::Gate::default();
    let actor = ModemActor::with_port_opener_and_recovery(
        move |_, _| Ok(Box::new(port.lock().unwrap().take().unwrap())),
        gate.clone(),
    )
    .unwrap();
    actor.claim_native_serial_management().unwrap();
    let session = actor.open_native_serial(path, 115_200).await.unwrap();
    let binding = actor.capture_serial_card_binding().unwrap();
    let hold = actor
        .retain_serial_module(&binding, "existing-hold-fixture")
        .await
        .unwrap();
    gate.revoke_for_tests();
    assert_eq!(
        actor
            .retain_serial_module(&binding, "new-hold-fixture")
            .await
            .err()
            .unwrap()
            .code,
        crate::module_recovery::CODE
    );
    assert_eq!(
        actor
            .sim_identity_transaction(&hold, |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        crate::module_recovery::CODE
    );
    assert!(actor.serial_module_hold_matches(session.serial_session, "existing-hold-fixture"));
    actor.release_serial_module(&hold).await.unwrap();
    assert!(!actor.serial_module_held());
    actor.close_native_serial(&session).await.unwrap();
    assert!(actor.serial_module_closed(&hold));
    actor.shutdown().unwrap();
}
