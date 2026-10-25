//! Actual PTY input/output and actor thread tests; no card or carrier is used.
use super::*;
use serialport::SerialPort as _;
use std::sync::Mutex;

struct Fixture {
    actor: ModemActor,
    master: serialport::TTYPort,
}
fn pair() -> (serialport::TTYPort, serialport::TTYPort) {
    let (mut master, slave) = serialport::TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(3)).unwrap();
    (master, slave)
}
fn actor_for(ports: Vec<serialport::TTYPort>) -> ModemActor {
    let ports = Mutex::new(VecDeque::from(ports));
    ModemActor::with_port_opener(move |_, _| {
        ports
            .lock()
            .unwrap()
            .pop_front()
            .map(|port| Box::new(port) as Box<dyn serialport::SerialPort>)
            .ok_or_else(|| ActorError::unavailable("PTY fixture exhausted"))
    })
    .unwrap()
}
fn command(master: &mut serialport::TTYPort) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        assert_eq!(master.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() <= 1024);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes).unwrap().trim().into();
        }
    }
}
fn payload(master: &mut serialport::TTYPort) {
    let mut count = 0;
    loop {
        let mut byte = [0];
        assert_eq!(master.read(&mut byte).unwrap(), 1);
        count += 1;
        assert!(count < 1024);
        if byte[0] == 0x1a {
            assert!(count > 2);
            return;
        }
        assert!(byte[0].is_ascii_hexdigit());
    }
}
fn no_write(master: &mut serialport::TTYPort) {
    master.set_timeout(Duration::from_millis(40)).unwrap();
    let mut bytes = [0; 32];
    match master.read(&mut bytes) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::UnexpectedEof
            ) || error.raw_os_error() == Some(5) => {} // PTY peer closed: EIO.
        other => panic!("unexpected serial write after completed rejection: {other:?}"),
    }
    master.set_timeout(Duration::from_secs(3)).unwrap();
}
fn initialize(actor: &ModemActor, master: &mut serialport::TTYPort) {
    let owner = actor.clone();
    let task = thread::spawn(move || owner.initialize());
    for expected in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ] {
        assert_eq!(command(master), expected);
        master.write_all(b"OK\r\n").unwrap();
    }
    let result = task.join().unwrap().unwrap();
    assert!(result.completed && result.probe_succeeded && result.sms_pdu_ready);
}
fn event(actor: &ModemActor, master: &mut serialport::TTYPort, line: &[u8]) {
    let before = actor.current_sms_generation().unwrap();
    master.write_all(line).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let frame = actor.read(MAX_READ_BYTES, 250).unwrap();
        if frame.sim_event.is_some() && frame.sms_generation > before {
            return;
        }
        assert!(Instant::now() < deadline, "SIM input was not observed");
    }
}
impl Fixture {
    fn new() -> Self {
        let (master, slave) = pair();
        let path = slave.name().unwrap();
        let actor = actor_for(vec![slave]);
        actor.open_with_initialization(path, 115_200, true).unwrap();
        Self { actor, master }
    }
    fn ready() -> Self {
        let mut fixture = Self::new();
        initialize(&fixture.actor, &mut fixture.master);
        event(&fixture.actor, &mut fixture.master, b"+CPIN: READY\r\n");
        fixture
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.actor.close().unwrap();
    }
}
fn send(
    actor: &ModemActor,
    binding: SerialSmsBinding,
    text: &str,
) -> thread::JoinHandle<Result<SmsSubmitResult, ActorError>> {
    let actor = actor.clone();
    let text = text.to_owned();
    thread::spawn(move || {
        actor.submit_sms_bound("+447700900123".into(), text, "auto".into(), binding)
    })
}
fn queue_bound(
    actor: &ModemActor,
    binding: SerialSmsBinding,
) -> Receiver<Result<SmsSubmitResult, ActorError>> {
    let tpdus = proletarius_sms::submit::prepare_with_policy(
        "+447700900123",
        "queued",
        1,
        proletarius_sms::submit::EncodingPolicy::Auto,
        false,
    )
    .unwrap();
    let (reply, result) = mpsc::sync_channel(1);
    actor
        .client
        .sender
        .send(Command::SubmitSms {
            tpdus,
            expected_sms_generation: Some(binding.sim_generation),
            binding: Some(binding),
            reply,
        })
        .unwrap();
    // This read-only actor command is a FIFO admission barrier, not a sleep.
    actor.serial_sms_observation().unwrap();
    result
}

#[test]
fn strict_binding_rejects_reopen_at_same_sim_generation_and_actor_reconstruction() {
    let (mut first, first_slave) = pair();
    let (mut second, second_slave) = pair();
    let first_path = first_slave.name().unwrap();
    let second_path = second_slave.name().unwrap();
    let actor = actor_for(vec![first_slave, second_slave]);
    actor
        .open_with_initialization(first_path, 115_200, true)
        .unwrap();
    initialize(&actor, &mut first);
    event(&actor, &mut first, b"+CPIN: READY\r\n");
    let old = actor.capture_serial_sms_binding().unwrap();
    actor.close().unwrap();
    actor
        .open_with_initialization(second_path, 115_200, true)
        .unwrap();
    // Even a caller copying the current numbers cannot bypass initialization.
    let mut uninitialized = old.clone();
    uninitialized.serial_session = actor
        .serial_sms_observation()
        .unwrap()
        .serial_session
        .unwrap();
    uninitialized.sim_generation = 0;
    assert!(
        send(&actor, uninitialized, "not initialized")
            .join()
            .unwrap()
            .is_err()
    );
    no_write(&mut second);
    initialize(&actor, &mut second);
    event(&actor, &mut second, b"+CPIN: READY\r\n");
    let replacement = actor.capture_serial_sms_binding().unwrap();
    assert_eq!(old.sim_generation, replacement.sim_generation);
    assert_ne!(old.serial_session, replacement.serial_session);
    assert!(!actor.serial_sms_binding_current(&old).unwrap());
    assert_eq!(
        send(&actor, old.clone(), "old")
            .join()
            .unwrap()
            .unwrap_err()
            .code(),
        "serial_sms_binding_changed"
    );
    no_write(&mut second);
    actor.close().unwrap();
    let mut rebuilt = Fixture::ready();
    let fresh = rebuilt.actor.capture_serial_sms_binding().unwrap();
    assert_eq!(old.serial_session, fresh.serial_session);
    assert_eq!(old.sim_generation, fresh.sim_generation);
    assert_ne!(old.owner_epoch, fresh.owner_epoch);
    assert!(!rebuilt.actor.serial_sms_binding_current(&old).unwrap());
    assert!(
        send(&rebuilt.actor, old, "old actor")
            .join()
            .unwrap()
            .is_err()
    );
    no_write(&mut rebuilt.master);
}

#[test]
fn uninitialized_unknown_or_pin_locked_sim_never_writes_cmgs_and_qsimstat_remains_supported() {
    let mut f = Fixture::new();
    assert!(f.actor.serial_sms_observation().unwrap().unknown);
    assert!(f.actor.capture_serial_sms_binding().is_err());
    event(&f.actor, &mut f.master, b"+QSIMSTAT: 1,1\r\n");
    assert!(f.actor.capture_serial_sms_binding().is_err());
    no_write(&mut f.master);
    initialize(&f.actor, &mut f.master);
    let known = f.actor.capture_serial_sms_binding().unwrap();
    assert!(f.actor.serial_sms_observation().unwrap().ready);
    // A real normal query response must not permanently disable serial SMS,
    // even when CPIN READY is never emitted by the physical peer.
    let owner = f.actor.clone();
    let query = thread::spawn(move || {
        owner.execute_scheduled("AT+QSIMSTAT?".into(), 3_000, false, None, None)
    });
    assert_eq!(command(&mut f.master), "AT+QSIMSTAT?");
    f.master.write_all(b"+QSIMSTAT: 1,1\r\nOK\r\n").unwrap();
    assert_eq!(query.join().unwrap().unwrap().result, "ok");
    assert!(f.actor.capture_serial_sms_binding().is_ok());
    assert!(!f.actor.serial_sms_binding_current(&known).unwrap());
    for line in [
        b"RDY\r\n".as_slice(),
        b"+CPIN: SIM PIN\r\n",
        b"+QSIMSTAT: 1,1\r\n",
        b"+QSIMSTAT: 1,0\r\n",
    ] {
        event(&f.actor, &mut f.master, line);
        assert!(f.actor.capture_serial_sms_binding().is_err());
        let mut forged = known.clone();
        forged.sim_generation = f.actor.current_sms_generation().unwrap();
        assert!(send(&f.actor, forged, "not ready").join().unwrap().is_err());
        no_write(&mut f.master);
    }
}

#[test]
fn queued_bound_sms_revalidates_sim_before_any_pdu_mode_or_cmgs_write() {
    let mut f = Fixture::ready();
    let binding = f.actor.capture_serial_sms_binding().unwrap();
    let owner = f.actor.clone();
    let held =
        thread::spawn(move || owner.execute_scheduled("AT+CGMR".into(), 3_000, false, None, None));
    assert_eq!(command(&mut f.master), "AT+CGMR");
    let result = queue_bound(&f.actor, binding);
    f.master.write_all(b"+CPIN: READY\r\nOK\r\n").unwrap();
    assert_eq!(held.join().unwrap().unwrap().result, "ok");
    assert!(
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_err()
    );
    no_write(&mut f.master);
}

#[test]
fn invalidation_after_cmgs_quarantines_exact_session_without_payload_or_queued_at() {
    let mut f = Fixture::ready();
    let binding = f.actor.capture_serial_sms_binding().unwrap();
    let result = send(&f.actor, binding, "must not be submitted");
    assert_eq!(command(&mut f.master), "AT+CMGF=0");
    f.master.write_all(b"OK\r\n").unwrap();
    assert!(command(&mut f.master).starts_with("AT+CMGS="));
    let (reply, queued) = mpsc::sync_channel(1);
    f.actor
        .client
        .sender
        .send(Command::Execute {
            command: "AT+CGMR".into(),
            timeout: Duration::from_secs(3),
            high_priority: false,
            follow_up: None,
            expected_sms_generation: None,
            reply,
        })
        .unwrap();
    f.actor.serial_sms_observation().unwrap();
    f.master.write_all(b"+CPIN: READY\r\n> ").unwrap();
    let result = result.join().unwrap().unwrap();
    assert!(!result.complete && result.accepted_segments == 0);
    assert!(
        queued
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_err()
    );
    assert!(!f.actor.serial_sms_observation().unwrap().present);
    no_write(&mut f.master);
}

#[test]
fn multipart_sms_retains_accepted_count_but_does_not_write_next_segment_after_sim_change() {
    let mut f = Fixture::ready();
    let binding = f.actor.capture_serial_sms_binding().unwrap();
    let result = send(&f.actor, binding, &"a".repeat(200));
    assert_eq!(command(&mut f.master), "AT+CMGF=0");
    f.master.write_all(b"OK\r\n").unwrap();
    assert!(command(&mut f.master).starts_with("AT+CMGS="));
    f.master.write_all(b"> ").unwrap();
    payload(&mut f.master);
    f.master
        .write_all(b"+CMGS: 1\r\nOK\r\n+CPIN: READY\r\n")
        .unwrap();
    let result = result.join().unwrap().unwrap();
    assert_eq!(result.accepted_segments, 1);
    assert!(!result.complete);
    no_write(&mut f.master);
}

#[test]
fn last_segment_ack_after_sim_change_is_observed_but_not_confirmed_complete() {
    let mut f = Fixture::ready();
    let binding = f.actor.capture_serial_sms_binding().unwrap();
    let result = send(&f.actor, binding, "last segment");
    assert_eq!(command(&mut f.master), "AT+CMGF=0");
    f.master.write_all(b"OK\r\n").unwrap();
    assert!(command(&mut f.master).starts_with("AT+CMGS="));
    f.master.write_all(b"> ").unwrap();
    payload(&mut f.master);
    // The TPDU was physically written. This new identity precedes the late
    // positive ACK; do not promote that ACK to a current-intent completion.
    f.master
        .write_all(b"+CPIN: READY\r\n+CMGS: 1\r\nOK\r\n")
        .unwrap();
    let result = result.join().unwrap().unwrap();
    assert_eq!(result.accepted_segments, 1);
    assert!(!result.complete);
    no_write(&mut f.master);
}

#[test]
fn successful_bound_sms_and_phone_cache_use_only_current_serial_sim() {
    let mut f = Fixture::ready();
    let binding = f.actor.capture_serial_sms_binding().unwrap();
    let owner = f.actor.clone();
    let query = thread::spawn(move || owner.typed_query("msisdn"));
    assert_eq!(command(&mut f.master), "AT+CNUM");
    f.master
        .write_all(b"+CNUM: \"\",\"+447700900123\",145\r\nOK\r\n")
        .unwrap();
    query.join().unwrap().unwrap();
    assert_eq!(
        f.actor.serial_sms_observation().unwrap().phone,
        "+447700900123"
    );
    let result = send(&f.actor, binding.clone(), "hello");
    assert_eq!(command(&mut f.master), "AT+CMGF=0");
    f.master.write_all(b"OK\r\n").unwrap();
    assert!(command(&mut f.master).starts_with("AT+CMGS="));
    f.master.write_all(b"> ").unwrap();
    payload(&mut f.master);
    f.master.write_all(b"+CMGS: 1\r\nOK\r\n").unwrap();
    assert!(result.join().unwrap().unwrap().complete);
    let owner = f.actor.clone();
    let late = thread::spawn(move || owner.typed_query("msisdn"));
    assert_eq!(command(&mut f.master), "AT+CNUM");
    f.master
        .write_all(b"+CPIN: READY\r\n+CNUM: \"\",\"+447700900999\",145\r\nOK\r\n")
        .unwrap();
    late.join().unwrap().unwrap();
    let observation = f.actor.serial_sms_observation().unwrap();
    assert!(observation.ready);
    assert!(observation.phone.is_empty());
    assert!(!f.actor.serial_sms_binding_current(&binding).unwrap());
    no_write(&mut f.master);
}

#[test]
fn native_generation_exhaustion_never_revalidates_a_wrapped_legacy_token() {
    let (_master, slave) = pair();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 1;
    state.serial_sms.owner_epoch = serial_sms::new_owner_epoch().unwrap();
    state.initialization.cached = Some(Ok(InitializationResult {
        completed: true,
        probe_succeeded: true,
        sms_pdu_ready: true,
        steps: Vec::new(),
    }));
    state.frame_input(b"+CPIN: READY\r\n").unwrap();
    assert!(
        state.serial_sms_observation().ready,
        "fixture must initially admit a strict binding"
    );
    state.sms_generation = u64::MAX;
    state.frame_input(b"+CPIN: READY\r\n").unwrap();
    assert_eq!(
        state.sms_generation, 1,
        "legacy counter semantics remain unchanged"
    );
    assert!(!state.serial_sms_observation().ready);
    state.frame_input(b"+QSIMSTAT: 1,1\r\n").unwrap();
    assert!(!state.serial_sms_observation().ready);
}
