//! Real PTY transport with simulated modem replies; no physical modem or SIM.
use super::*;
use serialport::SerialPort as _;

fn pair() -> (serialport::TTYPort, serialport::TTYPort) {
    let (mut master, slave) = serialport::TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(5)).unwrap();
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
            .ok_or_else(|| ActorError::unavailable("PTY fixture exhausted"))
    })
    .unwrap();
    actor.claim_native_serial_management().unwrap();
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
async fn opened() -> (ModemActor, serialport::TTYPort, NativeSerialSession) {
    let (master, slave) = pair();
    let path = slave.name().unwrap();
    let actor = actor_for(vec![slave]);
    let session = actor.open_native_serial(path, 115_200).await.unwrap();
    (actor, master, session)
}
async fn initialized(
    actor: &ModemActor,
    master: &mut serialport::TTYPort,
    session: &NativeSerialSession,
    pdu: bool,
) {
    let owner = actor.clone();
    let key = session.clone();
    let task = tokio::spawn(async move { owner.initialize_native_serial(&key).await });
    for expected in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ] {
        assert_eq!(command(master), expected);
        master
            .write_all(if !pdu && expected == "AT+CMGF=0" {
                b"ERROR\r\n"
            } else {
                b"OK\r\n"
            })
            .unwrap();
    }
    let value = task.await.unwrap().unwrap();
    assert!(value.completed && value.probe_succeeded);
    assert_eq!(value.sms_pdu_ready, pdu);
}
fn metadata_reply(master: &mut serialport::TTYPort, command_name: &str, reply: &[u8]) {
    assert_eq!(command(master), command_name);
    master.write_all(reply).unwrap();
}
fn metadata_tail(master: &mut serialport::TTYPort) {
    for (command_name, reply) in [
        ("AT+CGSN", "123456789012345\r\nOK\r\n"),
        ("AT+CGMR", "build-508\r\nOK\r\n"),
        ("AT+CIMI", "234150123456789\r\nOK\r\n"),
        ("AT+CNUM", "+CNUM: ,\"+441234567890\",145\r\nOK\r\n"),
        ("AT+QCCID", "+QCCID: 8901234567890123456\r\nOK\r\n"),
        ("AT+CGDCONT?", "+CGDCONT: 1,\"IP\",\"internet\"\r\nOK\r\n"),
        ("AT+QCFG=\"usbnet\"?", "+QCFG: \"usbnet\",0\r\nOK\r\n"),
    ] {
        metadata_reply(master, command_name, reply.as_bytes());
    }
}
async fn refreshed(
    actor: &ModemActor,
    master: &mut serialport::TTYPort,
    session: &NativeSerialSession,
) -> NativeSerialMetadata {
    let owner = actor.clone();
    let key = session.clone();
    let task = tokio::spawn(async move { owner.refresh_native_serial_metadata(&key).await });
    metadata_reply(master, "AT+QSIMSTAT?", b"+QSIMSTAT: 1,1\r\nOK\r\n");
    metadata_tail(master);
    task.await.unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_metadata_probe_settles_without_self_refresh_and_pdu_failure_keeps_card_capability()
{
    let (actor, mut master, session) = opened().await;
    initialized(&actor, &mut master, &session, false).await;
    let metadata = refreshed(&actor, &mut master, &session).await;
    actor.commit_native_serial_ready(&session).await.unwrap();
    let facts = actor.native_serial_observation();
    assert!(facts.initialized && facts.ready && !facts.refresh_needed);
    assert_eq!(facts.metadata.as_ref(), Some(&metadata));
    assert_eq!(metadata.status["imei"], "123456789012345");
    assert_eq!(metadata.status["phone_number"], "+441234567890");
    assert_eq!(metadata.status["cellular_suppressed"], true);
    assert!(actor.serial_card_binding_current(&metadata.binding));
    assert_eq!(actor.cached_serial_card_binding(), Some(metadata.binding));
    assert!(!actor.serial_sms_observation().unwrap().ready);
    assert!(actor.capture_serial_sms_binding().is_err());
    let next = actor
        .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
        .unwrap();
    assert!(
        next.is_none(),
        "PDU failure must not issue stored-SMS reads"
    );
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_idle_input_exceeds_legacy_capacity_without_queue_or_duplicate_call_capture() {
    let (actor, mut master, session) = opened().await;
    let (captured, complete) = mpsc::sync_channel(1);
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    actor.set_call_capture(Arc::new(move |binding, line| {
        assert!(matches!(
            binding,
            crate::module_radio_transport::Binding::Serial { .. }
        ));
        if line.trim() == "RING" && seen.fetch_add(1, Ordering::AcqRel) + 1 == 12_000 {
            let _ = captured.send(());
        }
        true
    }));
    // 72 KiB across complete input frames, with no compatibility Read caller.
    master.write_all(&b"RING\r\n".repeat(12_000)).unwrap();
    complete.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(count.load(Ordering::Acquire), 12_000);
    assert_eq!(
        actor.native_serial_observation().serial_session,
        Some(session.serial_session)
    );
    assert!(actor.native_serial_observation().terminal.is_none());
    let owner = actor.clone();
    let work = thread::spawn(move || owner.execute("AT+CSQ".into(), 1000));
    // CSQ is policy gated; ordinary non-cellular AT remains available.
    assert_eq!(
        work.join().unwrap().unwrap_err().code,
        "module_flight_mode_blocks_cellular_probe"
    );
    let owner = actor.clone();
    let work = thread::spawn(move || owner.execute("AT".into(), 1000));
    metadata_reply(&mut master, "AT", b"OK\r\n");
    assert_eq!(work.join().unwrap().unwrap().result, "ok");
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_terminal_is_sticky_and_old_close_cannot_retire_replacement() {
    let (mut master1, slave1) = pair();
    let (_master2, slave2) = pair();
    let path1 = slave1.name().unwrap();
    let path2 = slave2.name().unwrap();
    let actor = actor_for(vec![slave1, slave2]);
    let old = actor
        .open_native_serial(path1.clone(), 115_200)
        .await
        .unwrap();
    master1.write_all(&vec![b'X'; MAX_READ_BYTES + 1]).unwrap();
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let facts = old.observation();
            if let Some(value) = facts.terminal {
                break value;
            }
            actor.wait_native_serial_change(facts.revision).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(terminal.code, "serial_input_overflow");
    assert!(terminal.physical_closed);
    let current = actor.open_native_serial(path2, 115_200).await.unwrap();
    assert_ne!(old.serial_session, current.serial_session);
    assert_eq!(old.observation().port, path1);
    assert_eq!(old.observation().terminal, Some(terminal));
    actor.close_native_serial(&old).await.unwrap();
    assert_eq!(
        actor.native_serial_session().unwrap().serial_session,
        current.serial_session
    );
    actor.close_native_serial(&current).await.unwrap();
    assert_eq!(
        current.observation().terminal.unwrap().code,
        "serial_closed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_duplicate_sim_probe_aborts_metadata_instead_of_rebinding() {
    let (actor, mut master, session) = opened().await;
    initialized(&actor, &mut master, &session, true).await;
    let owner = actor.clone();
    let key = session.clone();
    let task = tokio::spawn(async move { owner.refresh_native_serial_metadata(&key).await });
    metadata_reply(
        &mut master,
        "AT+QSIMSTAT?",
        b"+QSIMSTAT: 1,1\r\n+QSIMSTAT: 1,0\r\nOK\r\n",
    );
    assert_eq!(
        task.await.unwrap().unwrap_err().code,
        "serial_session_changed"
    );
    let facts = actor.native_serial_observation();
    assert!(facts.metadata.is_none() && facts.refresh_needed && !facts.ready);
    assert_eq!(facts.sim_present, Some(false));
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_policy_aba_invalidates_metadata_and_notifies_without_any_at() {
    let (actor, mut master, session) = opened().await;
    initialized(&actor, &mut master, &session, true).await;
    let old = refreshed(&actor, &mut master, &session).await;
    actor.commit_native_serial_ready(&session).await.unwrap();
    let revision = actor.native_serial_observation().revision;
    actor.set_cellular_suppressed(false);
    actor.set_cellular_suppressed(true);
    assert_ne!(old.suppression_revision, actor.cellular_policy_revision());
    let facts = tokio::time::timeout(
        Duration::from_secs(5),
        actor.wait_native_serial_change(revision),
    )
    .await
    .unwrap();
    assert!(facts.refresh_needed && facts.metadata.is_none());
    assert!(actor.commit_native_serial_ready(&session).await.is_err());
    actor.close_native_serial(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_legacy_gate_denies_control_but_rust_typed_commands_remain_available() {
    let (actor, mut master, session) = opened().await;
    let mut legacy = actor.clone();
    legacy.compatibility_rpc = true;
    assert_eq!(legacy.close().unwrap_err().code, "serial_native_owner");
    assert_eq!(
        legacy.execute("AT".into(), 1000).unwrap_err().code,
        "serial_native_owner"
    );
    assert_eq!(
        legacy.read(1024, 1).unwrap_err().code,
        "serial_native_owner"
    );
    assert_eq!(
        actor.native_serial_session().unwrap().serial_session,
        session.serial_session
    );
    let owner = actor.clone();
    let task = thread::spawn(move || owner.execute("AT".into(), 1000));
    metadata_reply(&mut master, "AT", b"OK\r\n");
    assert_eq!(task.join().unwrap().unwrap().result, "ok");
    actor.close_native_serial(&session).await.unwrap();
}
