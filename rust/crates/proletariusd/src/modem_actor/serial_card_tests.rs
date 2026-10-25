//! Real PTY actor I/O, with simulated modem/card replies. No physical card,
//! carrier, ES9 server, or user profile is accessed by these fixtures.
use super::*;
use serialport::SerialPort as _;

fn pair() -> (serialport::TTYPort, serialport::TTYPort) {
    let (mut master, slave) = serialport::TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(3)).unwrap();
    (master, slave)
}
fn actor_for(ports: Vec<serialport::TTYPort>) -> ModemActor {
    let ports = Mutex::new(VecDeque::from(ports));
    let actor = ModemActor::with_port_opener(move |_, _| {
        ports
            .lock()
            .unwrap()
            .pop_front()
            .map(|port| Box::new(port) as Box<dyn serialport::SerialPort>)
            .ok_or_else(|| ActorError::unavailable("PTY exhausted"))
    })
    .unwrap();
    actor.claim_native_esim_management();
    actor
}
fn command(master: &mut serialport::TTYPort) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        assert_eq!(master.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 4096);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes).unwrap().trim().into();
        }
    }
}
fn no_write(master: &mut serialport::TTYPort) {
    master.set_timeout(Duration::from_millis(40)).unwrap();
    let mut bytes = [0; 64];
    match master.read(&mut bytes) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::UnexpectedEof
            ) || error.raw_os_error() == Some(5) => {}
        other => panic!("unexpected PTY write after completed rejection: {other:?}"),
    }
    master.set_timeout(Duration::from_secs(3)).unwrap();
}
fn open_actor() -> (ModemActor, serialport::TTYPort, SerialCardBinding) {
    let (master, slave) = pair();
    let path = slave.name().unwrap();
    let actor = actor_for(vec![slave]);
    actor.open_with_initialization(path, 115_200, true).unwrap();
    let binding = actor.capture_serial_card_binding().unwrap();
    (actor, master, binding)
}
fn open_channel(transport: &mut dyn AtCommandTransport<Error = Failure>) -> Result<(), Failure> {
    transport.command(
        "AT+CCHO=\"A0000005591010FFFFFFFF8900000100\"",
        Duration::from_secs(1),
        1024,
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn card_binding_without_sms_readiness_and_forced_channel_cleanup() {
    let (actor, mut master, binding) = open_actor();
    assert!(!actor.serial_sms_observation().unwrap().ready);
    assert_eq!(binding.sim_generation, 0);
    let owner = actor.clone();
    let task = tokio::spawn(async move {
        owner
            .esim_transaction(binding, false, |transport| {
                open_channel(transport)?;
                transport.command("AT+CGLA=1,10,\"00A4000000\"", Duration::from_secs(1), 1024)?;
                // Deliberately omit close: the actor owns and verifies forced CCHC.
                Ok(())
            })
            .await
    });
    assert!(command(&mut master).starts_with("AT+CCHO="));
    master.write_all(b"+CCHO: 1\r\nOK\r\n").unwrap();
    assert_eq!(command(&mut master), "AT+CGLA=1,10,\"00A4000000\"");
    master.write_all(b"+CGLA: 4,\"9000\"\r\nOK\r\n").unwrap();
    assert_eq!(command(&mut master), "AT+CCHC=1");
    assert!(
        !task.is_finished(),
        "result cannot precede physical cleanup"
    );
    master.write_all(b"OK\r\n").unwrap();
    task.await.unwrap().unwrap();
    assert!(actor.native_esim_managed());
    actor.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn card_binding_rejects_same_generation_reopen_and_actor_reconstruction() {
    let (mut first, slave1) = pair();
    let (mut second, slave2) = pair();
    let path1 = slave1.name().unwrap();
    let path2 = slave2.name().unwrap();
    let actor = actor_for(vec![slave1, slave2]);
    actor.open(path1, 115_200).unwrap();
    let old = actor.capture_serial_card_binding().unwrap();
    actor.close().unwrap();
    actor.open(path2, 115_200).unwrap();
    let current = actor.capture_serial_card_binding().unwrap();
    assert_eq!(old.sim_generation, current.sim_generation);
    assert_ne!(old.serial_session, current.serial_session);
    assert!(!actor.serial_card_binding_current(&old));
    let mut stale_commit_ran = false;
    assert!(
        actor
            .with_current_serial_card_binding(&old, || stale_commit_ran = true)
            .is_none()
    );
    assert!(
        !stale_commit_ran,
        "card A cannot commit after physical reopen to B"
    );
    assert_eq!(
        actor.with_current_serial_card_binding(&current, || 7),
        Some(7)
    );
    assert_eq!(
        actor
            .esim_transaction(old.clone(), false, open_channel)
            .await
            .unwrap_err()
            .code,
        "serial_card_binding_changed"
    );
    no_write(&mut first);
    no_write(&mut second);
    let (rebuilt, mut third, rebuilt_binding) = open_actor();
    assert_ne!(old.owner_epoch, rebuilt_binding.owner_epoch);
    let forged = SerialCardBinding {
        serial_session: rebuilt_binding.serial_session,
        sim_generation: rebuilt_binding.sim_generation,
        ..old
    };
    assert_eq!(
        rebuilt
            .esim_transaction(forged, false, open_channel)
            .await
            .unwrap_err()
            .code,
        "serial_card_binding_changed"
    );
    no_write(&mut third);
    actor.close().unwrap();
    rebuilt.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_card_transaction_rejects_writers_but_keeps_legacy_drain_live() {
    let (actor, mut master, binding) = open_actor();
    let (opened, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let owner = actor.clone();
    let task = tokio::spawn(async move {
        owner
            .esim_transaction(binding, false, move |transport| {
                open_channel(transport)?;
                opened.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
            .await
    });
    assert!(command(&mut master).starts_with("AT+CCHO="));
    master.write_all(b"+CCHO: 1\r\nOK\r\n").unwrap();
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        actor.write(b"AT\r\n".to_vec()).unwrap_err().code,
        "serial_card_busy"
    );
    assert_eq!(actor.initialize().unwrap_err().code, "serial_card_busy");
    assert!(
        actor
            .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
            .unwrap()
            .is_none()
    );
    // Drain more than the legacy queue's 64 KiB limit while the transaction
    // holds its channel and waits for external work; each read is a receipt.
    let event = format!("+TEST: {}\r\n", "x".repeat(1000));
    for _ in 0..70 {
        master.write_all(event.as_bytes()).unwrap();
        let frame = actor.read(MAX_READ_BYTES, 250).unwrap();
        assert_eq!(frame.data, event.as_bytes());
    }
    no_write(&mut master);
    release.send(()).unwrap();
    assert_eq!(command(&mut master), "AT+CCHC=1");
    master.write_all(b"OK\r\n").unwrap();
    task.await.unwrap().unwrap();
    actor.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_observer_retains_channel_and_defers_close_until_cleanup_receipt() {
    let (actor, mut master, binding) = open_actor();
    let (opened, ready) = mpsc::sync_channel(1);
    let (release, wait) = mpsc::sync_channel(1);
    let owner = actor.clone();
    let task = tokio::spawn(async move {
        owner
            .esim_transaction(binding, true, move |transport| {
                open_channel(transport)?;
                opened.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
            .await
    });
    assert!(command(&mut master).starts_with("AT+CCHO="));
    master.write_all(b"+CCHO: 1\r\nOK\r\n").unwrap();
    ready.recv_timeout(Duration::from_secs(3)).unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        actor.write(b"AT\r\n".to_vec()).unwrap_err().code,
        "serial_card_busy"
    );
    let owner = actor.clone();
    let closing = thread::spawn(move || owner.close());
    actor.serial_sms_observation().unwrap();
    assert!(!closing.is_finished());
    release.send(()).unwrap();
    assert_eq!(command(&mut master), "AT+CCHC=1");
    assert!(!closing.is_finished());
    master.write_all(b"OK\r\n").unwrap();
    closing.join().unwrap().unwrap();
    assert!(actor.capture_serial_card_binding().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_cchc_quarantines_session_and_cannot_close_replacement() {
    let (mut master, slave1) = pair();
    let (mut replacement, slave2) = pair();
    let path1 = slave1.name().unwrap();
    let path2 = slave2.name().unwrap();
    let actor = actor_for(vec![slave1, slave2]);
    actor.open(path1, 115_200).unwrap();
    let binding = actor.capture_serial_card_binding().unwrap();
    let owner = actor.clone();
    let task =
        tokio::spawn(async move { owner.esim_transaction(binding, true, open_channel).await });
    assert!(command(&mut master).starts_with("AT+CCHO="));
    master.write_all(b"+CCHO: 1\r\nOK\r\n").unwrap();
    assert_eq!(command(&mut master), "AT+CCHC=1");
    master.write_all(b"ERROR\r\n").unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(error.uncertain);
    assert!(actor.capture_serial_card_binding().is_err());
    actor.open(path2, 115_200).unwrap();
    let current = actor.capture_serial_card_binding().unwrap();
    assert!(actor.serial_card_binding_current(&current));
    no_write(&mut replacement);
    actor.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sim_change_during_open_never_submits_apdu_or_cleanup_to_new_sim() {
    let (actor, mut master, binding) = open_actor();
    let owner = actor.clone();
    let task = tokio::spawn(async move {
        owner
            .esim_transaction(binding, true, |transport| {
                open_channel(transport)?;
                transport.command("AT+CGLA=1,10,\"00A4000000\"", Duration::from_secs(1), 1024)?;
                Ok(())
            })
            .await
    });
    assert!(command(&mut master).starts_with("AT+CCHO="));
    master
        .write_all(b"+CPIN: READY\r\n+CCHO: 1\r\nOK\r\n")
        .unwrap();
    assert!(task.await.unwrap().unwrap_err().uncertain);
    assert!(actor.capture_serial_card_binding().is_err());
    no_write(&mut master);
    actor.close().unwrap();
}

#[test]
fn managed_card_capability_rejects_legacy_escape_hatches_but_preserves_metadata_reads() {
    for command in [
        "AT+CCHO=\"A000\"",
        "at + cgla=1,2,\"00\"",
        "AT+CCHC=1",
        "AT+CSIM=4,\"00A4\"",
        "AT+CRSM=214,28486,0,0,17",
        "AT+CRSM=176,28486,0,0,17;AT+CRSM=214,28486,0,0,17",
        "AT+CRSM=176,28486,0,0,17\r\nAT+CSQ",
        "AT+CRSM=178,1,1,4,1,\"00\"",
        "AT+CRSM=176,1,0,0,256",
    ] {
        assert!(legacy_card_command(command), "must reject {command}");
    }
    for command in [
        "AT+CRSM=176,28486,0,0,17\r\n",
        "AT+CRSM=178,28480,1,4,20",
        "AT+QCCID",
        "AT+CIMI",
        "AT+CSQ",
    ] {
        assert!(!legacy_card_command(command), "must preserve {command}");
    }
    let (actor, mut master, _) = open_actor();
    assert_eq!(
        actor
            .write(b"AT+CCHO=\"A000\"\r\n".to_vec())
            .unwrap_err()
            .code,
        "serial_esim_native_owner"
    );
    assert!(actor.write(b"AT+CC".to_vec()).is_err());
    assert!(actor.write(b"HO=\"A000\"\r\n".to_vec()).is_err());
    no_write(&mut master);
    actor.close().unwrap();
}

#[test]
fn queued_card_command_rechecks_identity_before_wire_and_wrap_is_fail_closed() {
    let (mut master, slave) = pair();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 1;
    state.serial_sms.owner_epoch = "fixture-epoch".into();
    state.card_shared.managed.store(true, Ordering::Release);
    let binding = state.local_card_binding().unwrap();
    let (reply, receive) = mpsc::sync_channel(1);
    state.process_card_request(CardRequest::Begin { binding, reply });
    let permit = receive.recv().unwrap().unwrap();
    let (reply, result) = mpsc::sync_channel(1);
    state.process_card_request(CardRequest::Command {
        permit,
        command: "AT+CCHO=\"A000\"".into(),
        timeout: Duration::from_secs(1),
        maximum: 1024,
        reply,
    });
    state.observe_sim_event(Some(&SimEvent::Ready));
    state.start_next_command();
    assert!(result.recv().unwrap().is_err());
    no_write(&mut master);
    assert!(state.port.is_none());
    let (_, slave) = pair();
    state.port = Some(Box::new(slave));
    state.serial_session = 2;
    state.serial_sms.reset_sim();
    state.sms_generation = u64::MAX;
    state.observe_sim_event(Some(&SimEvent::Ready));
    assert_eq!(state.sms_generation, 0);
    assert!(
        state.local_card_binding().is_none(),
        "wrapped generation must never mint an old capability"
    );
}

#[test]
fn stale_card_cleanup_does_not_close_or_write_replacement_physical_session() {
    let (mut master, slave) = pair();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 2;
    state.serial_sms.owner_epoch = "fixture-epoch".into();
    let current = state.local_card_binding().unwrap();
    state.card_lease = Some(CardLease {
        module: None,
        permit: 1,
        binding: SerialCardBinding {
            serial_session: 1,
            ..current.clone()
        },
        channel: ChannelState::Open(1),
        poisoned: false,
    });
    let (reply, result) = mpsc::sync_channel(1);
    state.process_card_request(CardRequest::End { permit: 1, reply });
    assert!(result.recv().unwrap().is_err());
    assert_eq!(state.local_card_binding().as_ref(), Some(&current));
    no_write(&mut master);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_ccho_never_claims_known_channel_cleanup() {
    for response in [
        "+CCHO: 1\r\n+CCHO: 99\r\nOK\r\n",
        "+CCHO: broken\r\nERROR\r\n",
        "+CCHO: 1\r\n+CCHO: 2\r\nOK\r\n",
    ] {
        let (actor, mut master, binding) = open_actor();
        let owner = actor.clone();
        let task =
            tokio::spawn(async move { owner.esim_transaction(binding, false, open_channel).await });
        assert!(command(&mut master).starts_with("AT+CCHO="));
        master.write_all(response.as_bytes()).unwrap();
        assert!(task.await.unwrap().unwrap_err().uncertain);
        assert!(actor.capture_serial_card_binding().is_err());
        no_write(&mut master);
        actor.close().unwrap();
    }
}

#[test]
fn serial_esim_health_uses_existing_signal_and_suppression_sentinels() {
    let health = parse_serial_esim_health_responses(
        "+QCCID: 8912345678901234567",
        "123456789012345",
        "",
        "",
        "",
        "",
        false,
        true,
    );
    assert_eq!(health["signal_dbm"], -999);
    let health = parse_serial_esim_health_responses(
        "",
        "",
        "+CSQ: 17,99",
        "+CREG: 2,1",
        "+COPS: 0,0,\"Test\",7",
        "+QNWINFO: \"FDD LTE\",\"00101\",\"LTE BAND 1\",1",
        true,
        false,
    );
    assert_eq!(health["signal_dbm"], -999);
    assert_eq!(health["network_mode"], "飞行模式");
    assert_eq!(health["operator"], "");
}

#[test]
fn deferred_legacy_close_is_fenced_before_transaction_ends_and_port_reopens() {
    let (_, slave) = pair();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 1;
    state.serial_sms.owner_epoch = "fixture-epoch".into();
    let binding = state.local_card_binding().unwrap();
    state.card_lease = Some(CardLease {
        module: None,
        permit: 1,
        binding,
        channel: ChannelState::Unknown,
        poisoned: true,
    });
    let (reply, closed) = mpsc::sync_channel(1);
    assert!(
        state
            .admit_card_boundary(Command::Close { reply })
            .is_none()
    );
    let (reply, ended) = mpsc::sync_channel(1);
    state.process_card_request(CardRequest::End { permit: 1, reply });
    assert!(ended.recv().unwrap().is_err());
    let (mut replacement, slave) = pair();
    state.port = Some(Box::new(slave));
    state.serial_session = 2;
    let current = state.local_card_binding().unwrap();
    let deferred = state.card_deferred_closes.pop().unwrap();
    assert!(matches!(
        &deferred,
        Command::FencedClose {
            serial_session: 1,
            ..
        }
    ));
    let opener: PortOpener = Box::new(|_, _| panic!("no serial reopen expected"));
    state.process(deferred, &opener);
    closed.recv().unwrap().unwrap();
    assert_eq!(state.local_card_binding().as_ref(), Some(&current));
    no_write(&mut replacement);
}

async fn open_module_actor() -> (
    ModemActor,
    serialport::TTYPort,
    NativeSerialSession,
    SerialModuleHold,
) {
    let (master, slave) = pair();
    let path = slave.name().unwrap();
    let actor = actor_for(vec![slave]);
    actor.claim_native_serial_management().unwrap();
    let session = actor.open_native_serial(path, 115_200).await.unwrap();
    let binding = actor.capture_serial_card_binding().unwrap();
    let hold = actor
        .retain_serial_module(&binding, "module-fixture-owner")
        .await
        .unwrap();
    (actor, master, session, hold)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_hold_blocks_metadata_esim_raw_close_and_foreign_radio_without_bytes() {
    let (actor, mut master, session, hold) = open_module_actor().await;
    assert!(actor.serial_module_held());
    assert!(actor.serial_module_current(&hold));
    assert!(actor.serial_module_hold_matches(session.serial_session, "module-fixture-owner"));
    assert!(!actor.serial_module_hold_matches(session.serial_session, "foreign"));
    assert_eq!(
        actor
            .refresh_native_serial_metadata(&session)
            .await
            .unwrap_err()
            .code,
        "serial_command_busy"
    );
    assert_eq!(
        actor
            .esim_transaction(hold.binding(), false, |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        "serial_card_busy"
    );
    assert!(actor.execute("AT+CNUM".into(), 1000).is_err());
    assert_eq!(
        actor.close_native_serial(&session).await.unwrap_err().code,
        "serial_card_busy"
    );
    assert_eq!(
        actor
            .radio_execute(
                session.serial_session,
                crate::module_radio_transport::Action::Observe { cellular: false }
            )
            .await
            .unwrap_err()
            .code,
        "serial_card_busy"
    );
    assert_eq!(
        actor
            .radio_execute_module_with_budget(
                session.serial_session,
                "foreign",
                crate::module_radio_transport::Action::Observe { cellular: false },
                Duration::from_secs(1),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .unwrap_err()
            .code,
        "serial_card_binding_changed"
    );
    no_write(&mut master);
    assert_eq!(hold.binding(), actor.capture_serial_card_binding().unwrap());
    actor.release_serial_module(&hold).await.unwrap();
    assert!(!actor.serial_module_current(&hold));
    actor.close_native_serial(&session).await.unwrap();
    assert!(actor.serial_module_closed(&hold));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_identity_cancellation_retains_whole_transaction_and_release_barrier() {
    let (actor, mut master, session, hold) = open_module_actor().await;
    let owner = actor.clone();
    let retained = hold.clone();
    let (entered, arrived) = mpsc::sync_channel(1);
    let (resume, wait) = mpsc::sync_channel(1);
    let task = tokio::spawn(async move {
        owner
            .sim_identity_transaction(&retained, move |transport| {
                transport.command(
                    "AT+CSIM=10,\"00A4000000\"",
                    Duration::from_secs(1),
                    1024 * 1024,
                )?;
                entered.send(()).unwrap();
                wait.recv().unwrap();
                transport.command("AT+CNUM", Duration::from_secs(1), 1024)?;
                Ok(())
            })
            .await
    });
    assert_eq!(command(&mut master), "AT+CSIM=10,\"00A4000000\"");
    master.write_all(b"+CSIM: 4,\"9000\"\r\nOK\r\n").unwrap();
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    task.abort();
    let _ = task.await;
    assert_eq!(
        actor.release_serial_module(&hold).await.unwrap_err().code,
        "serial_card_busy"
    );
    assert_eq!(
        actor
            .sim_identity_transaction(&hold, |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        "serial_card_busy"
    );
    assert_eq!(
        actor.close_native_serial(&session).await.unwrap_err().code,
        "serial_card_busy"
    );
    resume.send(()).unwrap();
    assert_eq!(command(&mut master), "AT+CNUM");
    master.write_all(b"OK\r\n").unwrap();
    for _ in 0..100 {
        if actor.release_serial_module(&hold).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!actor.serial_module_held());
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_identity_strict_commands_malformed_csim_quarantine_exact_session() {
    let (actor, mut master, _, hold) = open_module_actor().await;
    let denied = actor
        .sim_identity_transaction(&hold, |transport| {
            transport.command("AT+CCHO=\"A0000000871002\"", Duration::from_secs(1), 1024)
        })
        .await;
    assert!(denied.is_err());
    no_write(&mut master);
    assert!(actor.serial_module_current(&hold));
    let owner = actor.clone();
    let retained = hold.clone();
    let task = tokio::spawn(async move {
        owner
            .sim_identity_transaction(&retained, |transport| {
                transport.command("AT+CSIM=10,\"00A4000000\"", Duration::from_secs(1), 1024)
            })
            .await
    });
    assert_eq!(command(&mut master), "AT+CSIM=10,\"00A4000000\"");
    master.write_all(b"+CSIM: 8,\"9000\"\r\nOK\r\n").unwrap();
    assert!(task.await.unwrap().unwrap_err().uncertain);
    assert!(!actor.serial_module_current(&hold));
    assert!(actor.serial_module_closed(&hold));
    assert!(actor.serial_module_held());
    actor.release_serial_module(&hold).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_unsolicited_sim_change_revokes_identity_without_becoming_close_proof() {
    let (actor, mut master, session, hold) = open_module_actor().await;
    master.write_all(b"+QSIMSTAT: 1,0\r\n").unwrap();
    for _ in 0..100 {
        if !actor.serial_module_current(&hold) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!actor.serial_module_current(&hold));
    assert!(!actor.serial_module_closed(&hold));
    assert!(actor.serial_module_hold_matches(session.serial_session, "module-fixture-owner"));
    assert_eq!(
        actor
            .sim_identity_transaction(&hold, |_| Ok(()))
            .await
            .unwrap_err()
            .code,
        "serial_card_binding_changed"
    );
    no_write(&mut master);
    actor.release_serial_module(&hold).await.unwrap();
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_matching_rf_survives_waiter_cancel_with_pdu_failure_and_stale_sim_restore() {
    let (actor, mut master, session, initial_hold) = open_module_actor().await;
    actor.release_serial_module(&initial_hold).await.unwrap();
    let owner = actor.clone();
    let key = session.clone();
    let initialization = tokio::spawn(async move { owner.initialize_native_serial(&key).await });
    for expected in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ] {
        assert_eq!(command(&mut master), expected);
        master
            .write_all(if expected == "AT+CMGF=0" {
                b"ERROR\r\n"
            } else {
                b"OK\r\n"
            })
            .unwrap();
    }
    let initialized = initialization.await.unwrap().unwrap();
    assert!(initialized.completed && initialized.probe_succeeded && !initialized.sms_pdu_ready);
    let binding = actor.capture_serial_card_binding().unwrap();
    let hold = actor
        .retain_serial_module(&binding, "module-fixture-owner")
        .await
        .unwrap();
    let owner = actor.clone();
    let number = session.serial_session;
    let worker = tokio::spawn(async move {
        owner
            .radio_execute_module_with_budget(
                number,
                "module-fixture-owner",
                crate::module_radio_transport::Action::SetVerify { target: 4 },
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(false)),
            )
            .await
    });
    assert_eq!(command(&mut master), "AT+CFUN?");
    worker.abort();
    let _ = worker.await;
    assert_eq!(
        actor.release_serial_module(&hold).await.unwrap_err().code,
        "serial_card_busy"
    );
    master.write_all(b"+CFUN: 1\r\nOK\r\n").unwrap();
    for (expected, reply) in [
        ("AT+CLCC", "OK\r\n"),
        ("AT+CFUN=4", "OK\r\n"),
        ("AT+CFUN?", "+CFUN: 4\r\nOK\r\n"),
    ] {
        assert_eq!(command(&mut master), expected);
        master.write_all(reply.as_bytes()).unwrap();
    }
    // Synchronize command completion by awaiting a new matching read plan.
    let owner = actor.clone();
    let read = tokio::spawn(async move {
        for _ in 0..100 {
            match owner
                .radio_execute_module_with_budget(
                    number,
                    "module-fixture-owner",
                    crate::module_radio_transport::Action::Observe { cellular: false },
                    Duration::from_secs(3),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
            {
                Err(error) if error.code == "serial_command_busy" => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                result => return result,
            }
        }
        panic!("cancelled retained RF did not settle")
    });
    assert_eq!(command(&mut master), "AT+CFUN?");
    master
        .write_all(b"+QSIMSTAT: 1,0\r\n+CFUN: 4\r\nOK\r\n")
        .unwrap();
    assert!(read.await.unwrap().unwrap().verified);
    assert!(!actor.serial_module_current(&hold));
    assert!(!actor.serial_module_closed(&hold));
    let owner = actor.clone();
    let restore = tokio::spawn(async move {
        owner
            .radio_execute_module_with_budget(
                number,
                "module-fixture-owner",
                crate::module_radio_transport::Action::SetVerify { target: 1 },
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(false)),
            )
            .await
    });
    for (expected, reply) in [
        ("AT+CFUN?", "+CFUN: 4\r\nOK\r\n"),
        ("AT+CLCC", "OK\r\n"),
        ("AT+CFUN=1", "OK\r\n"),
        ("AT+CFUN?", "+CFUN: 1\r\nOK\r\n"),
    ] {
        assert_eq!(command(&mut master), expected);
        master.write_all(reply.as_bytes()).unwrap();
    }
    assert!(restore.await.unwrap().unwrap().verified);
    actor.release_serial_module(&hold).await.unwrap();
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_hold_stale_epoch_session_never_releases_or_writes_replacement() {
    let (mut first, slave1) = pair();
    let (mut second, slave2) = pair();
    let first_path = slave1.name().unwrap();
    let second_path = slave2.name().unwrap();
    let actor = actor_for(vec![slave1, slave2]);
    actor.claim_native_serial_management().unwrap();
    let first_session = actor.open_native_serial(first_path, 115_200).await.unwrap();
    let original = actor.capture_serial_card_binding().unwrap();
    let old = actor
        .retain_serial_module(&original, "old-owner")
        .await
        .unwrap();
    let recovered = actor
        .retain_serial_module(&original, "old-owner")
        .await
        .unwrap();
    assert_eq!(old.binding(), recovered.binding());
    assert!(
        actor
            .open_native_serial(second_path.clone(), 115_200)
            .await
            .is_err()
    );
    actor.release_serial_module(&old).await.unwrap();
    actor.close_native_serial(&first_session).await.unwrap();
    let second_session = actor
        .open_native_serial(second_path, 115_200)
        .await
        .unwrap();
    let replacement = actor.capture_serial_card_binding().unwrap();
    assert_eq!(original.sim_generation, replacement.sim_generation);
    assert_ne!(original.serial_session, replacement.serial_session);
    let current = actor
        .retain_serial_module(&replacement, "new-owner")
        .await
        .unwrap();
    assert!(actor.serial_module_closed(&old));
    assert!(!actor.serial_module_current(&old));
    actor.release_serial_module(&old).await.unwrap();
    assert!(actor.serial_module_current(&current));
    assert!(
        actor
            .sim_identity_transaction(&old, |_| Ok(()))
            .await
            .is_err()
    );
    assert!(
        actor
            .retain_serial_module(&original, "old-owner")
            .await
            .is_err()
    );
    no_write(&mut first);
    no_write(&mut second);
    actor.release_serial_module(&current).await.unwrap();
    actor.close_native_serial(&second_session).await.unwrap();
}
