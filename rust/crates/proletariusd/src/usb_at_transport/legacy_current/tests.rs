use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Default)]
struct Trace { commands: Vec<String>, closes: usize, close_failed: bool }
struct Wire {
    target: PhysicalTarget,
    cfun: u8,
    trace: Arc<Mutex<Trace>>,
    incoming: VecDeque<u8>,
    time: Instant,
    truncate_cfun: bool,
    active_call: bool,
}
impl Io for Wire {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        let mut trace = self.trace.lock().unwrap();
        trace.closes += 1;
        if trace.close_failed { return Err(cleanup_error()); }
        Ok(CloseDisposition::ReleasedAndHandleDropped)
    }
    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> { Ok(self.target.clone()) }
    fn now(&self) -> Instant { self.time }
    fn read(&mut self, output: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
        if self.incoming.is_empty() { self.time += timeout; return Err(IoError::Timeout); }
        let count = self.incoming.len().min(output.len()).min(7);
        for byte in &mut output[..count] { *byte = self.incoming.pop_front().unwrap(); }
        Ok(count)
    }
    fn write(&mut self, bytes: &[u8], _: Duration) -> Result<usize, IoError> {
        let command = std::str::from_utf8(bytes).map_err(|_| IoError::Other)?
            .strip_suffix('\r').ok_or(IoError::Other)?;
        self.trace.lock().unwrap().commands.push(command.to_owned());
        let response = match command {
            "AT" => "OK\r\n".to_owned(),
            "AT+CGSN" => "123456789012345\r\nOK\r\n".to_owned(),
            "ATI" => "Quectel fixture\r\nOK\r\n".to_owned(),
            "AT+QCFG=\"USBCFG\"" => "+QCFG: \"USBCFG\",0x2c7c,0x0125,1,1,1,1,1,1,1\r\nOK\r\n".to_owned(),
            "AT+QCFG=\"ims\"" => "+QCFG: \"ims\",1,1\r\nOK\r\n".to_owned(),
            "AT+QCFG=\"volte_disable\"" => "+QCFG: \"volte_disable\",0\r\nOK\r\n".to_owned(),
            "AT+CFUN?" if self.truncate_cfun => format!("+CFUN: {}\r\nOK", self.cfun),
            "AT+CFUN?" => format!("+CFUN: {}\r\nOK\r\n", self.cfun),
            "AT+CLCC" if self.active_call => "+CLCC: 1,0,0,0,0\r\nOK\r\n".to_owned(),
            "AT+CLCC" => "OK\r\n".to_owned(),
            // Ordinary startup's best-effort eUICC probe only. Baseline reads
            // still use their separate fixed allowlist; arbitrary CCHO fails.
            value if proletarius_euicc::KNOWN_ISDR_APPLICATION_AIDS.iter().any(|aid| {
                let hex = aid.iter().map(|byte| format!("{byte:02X}")).collect::<String>();
                value == format!("AT+CCHO=\"{hex}\"")
            }) => "ERROR\r\n".to_owned(),
            "AT+CPMS?" => "+CPMS: \"ME\",0,100,\"ME\",0,100,\"ME\",0,100\r\nOK\r\n".to_owned(),
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n".to_owned(),
            _ => return Err(IoError::Other),
        };
        self.incoming.extend(response.as_bytes());
        Ok(bytes.len())
    }
}
fn port(target: PhysicalTarget, cfun: u8, truncated: bool, active_call: bool,
        trace: Arc<Mutex<Trace>>) -> NativeTransport {
    NativeTransport::new(Box::new(Wire { target, cfun, trace, incoming: VecDeque::new(),
        time: Instant::now(), truncate_cfun: truncated, active_call }),
        "software byte fixture; no physical device evidence".into(), None)
}
struct Scope { valid: Arc<AtomicBool>, deadline: Instant }
impl Scope {
    fn new() -> Self { Self { valid: Arc::new(AtomicBool::new(true)),
        deadline: Instant::now() + Duration::from_secs(60) } }
}
impl Authority for Scope {
    fn check(&self) -> Result<(), TransportError> {
        if self.valid.load(Ordering::Acquire) && Instant::now() < self.deadline { Ok(()) } else { Err(read_error()) }
    }
    fn deadline(&self) -> Instant { self.deadline }
    fn admission(&self) -> crate::module_radio::CallAdmission {
        let valid = self.valid.clone();
        Arc::new(move || valid.load(Ordering::Acquire))
    }
    fn expected_imei(&self) -> Option<&str> { Some("123456789012345") }
    fn accepts_usb(&self, usb: UsbComposition) -> bool {
        usb == UsbComposition { vendor_id: 0x2c7c, product_id: 0x0125, flags: [1; 7] }
    }
}
#[test]
fn stable_current_flight_modes_are_read_without_any_cfun_mutation() {
    for cfun in [0, 1, 4] {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut transport = port(PhysicalTarget::synthetic_for_tests(), cfun, false, false, trace.clone());
        let scope = Scope::new();
        let first = read_facts(&mut transport, &scope).unwrap();
        let second = read_facts(&mut transport, &scope).unwrap();
        assert_eq!(first.cfun, cfun);
        assert!(first.same(&second));
        assert!(transport.command_guard.is_none());
        checked_close(transport).unwrap();
        let trace = trace.lock().unwrap();
        assert_eq!(trace.closes, 1);
        assert_eq!(trace.commands.iter().filter(|s| *s == "AT+CFUN?").count(), 2);
        assert!(trace.commands.iter().all(|s| !s.starts_with("AT+CFUN=")));
    }
}
#[test]
fn incomplete_terminals_active_calls_unknown_cfun_and_revocation_refuse() {
    for (cfun, truncated, active) in [(1, true, false), (1, false, true), (2, false, false)] {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut transport = port(PhysicalTarget::synthetic_for_tests(), cfun, truncated, active, trace.clone());
        assert!(read_facts(&mut transport, &Scope::new()).is_err());
        checked_close(transport).unwrap();
        assert_eq!(trace.lock().unwrap().closes, 1);
    }
    let scope = Scope::new();
    scope.valid.store(false, Ordering::Release);
    let trace = Arc::new(Mutex::new(Trace::default()));
    let mut transport = port(PhysicalTarget::synthetic_for_tests(), 1, false, false, trace.clone());
    assert!(read_facts(&mut transport, &scope).is_err());
    assert!(trace.lock().unwrap().commands.is_empty());
    checked_close(transport).unwrap();
}
#[test]
fn no_mutating_action_or_command_can_cross_the_private_reader() {
    let scope = Scope::new();
    let trace = Arc::new(Mutex::new(Trace::default()));
    let mut transport = port(PhysicalTarget::synthetic_for_tests(), 1, false, false, trace.clone());
    for command in ["AT+CFUN=1", "AT+QCFG=\"ims\",1", "AT+QADBKEY=\"anything\"", "ATD123;"] {
        assert!(read_command(&mut transport, &scope, command, Duration::from_secs(1)).is_err());
    }
    assert!(read_plan(&mut transport, &scope, Action::Reboot).is_err());
    assert!(trace.lock().unwrap().commands.is_empty());
    checked_close(transport).unwrap();
}
#[test]
fn actual_close_failure_is_uncertain_and_never_turns_into_success() {
    let trace = Arc::new(Mutex::new(Trace { close_failed: true, ..Trace::default() }));
    let transport = port(PhysicalTarget::synthetic_for_tests(), 1, false, false, trace.clone());
    let error = checked_close(transport).unwrap_err();
    assert!(error.uncertain);
    assert_eq!(error.code, "legacy_current_cleanup_unconfirmed");
    assert_eq!(trace.lock().unwrap().closes, 1);
}

// Coordinator fixtures use the same Plans, wire decoder, physical-token binding,
// ADB packet decoder and checked retirement as inspect_port/reopen_port. The
// synthetic token is deliberately not capable of native hardware operations.
pub(crate) fn fixture_inspect(scope: &ReadScope, cfun: u8) -> Result<VerifiedDevice, TransportError> {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let transport = port(PhysicalTarget::synthetic_for_tests(), cfun, false, false, trace.clone());
    let result = inspect_port(transport, scope, crate::call_mode_adb::legacy_current::fixture_idle);
    assert_eq!(trace.lock().unwrap().closes, 1);
    result
}
pub(crate) fn fixture_reopen(scope: &ReadScope, verified: &VerifiedDevice)
    -> Result<Box<dyn Transport>, TransportError> {
    let transport = port(verified.target.clone(), verified.facts.cfun, false, false,
        Arc::new(Mutex::new(Trace::default())));
    reopen_port(transport, scope, verified, crate::call_mode_adb::legacy_current::fixture_idle)
}

#[test]
fn all_interface_claims_and_checked_release_share_the_native_ordering() {
    struct Claims { taken: Vec<u8>, fail: Option<u8>, wrong_alt: bool }
    impl InterfaceClaims for Claims {
        fn claim(&mut self, interface: u8) -> Result<(), TransportError> {
            if self.fail == Some(interface) { return Err(device_error()); }
            self.taken.push(interface);
            Ok(())
        }
        fn alternate(&mut self, _: u8, _: Duration) -> Result<u8, TransportError> {
            Ok(u8::from(self.wrong_alt))
        }
    }
    let expected = [2, 3].map(|interface| Candidate {
        interface, alternate: 0, endpoint_in: 0x81, endpoint_out: 1 });
    for (fail, wrong_alt, expected_claims) in [
        (None, false, vec![2,3]), (Some(3), false, vec![2]), (None, true, vec![2])
    ] {
        let mut io = Claims { taken: Vec::new(), fail, wrong_alt };
        let mut claimed = Vec::new();
        assert_eq!(claim_all(&Scope::new(), &expected, &mut io, &mut claimed).is_ok(),
            fail.is_none() && !wrong_alt);
        assert_eq!(claimed, expected_claims);
        assert_eq!(io.taken, expected_claims);
        let mut released = Vec::new();
        release_all(&mut claimed, |interface| {
            released.push(interface);
            Ok(CloseDisposition::ReleasedAndHandleDropped)
        }).unwrap();
        assert_eq!(released, expected_claims.into_iter().rev().collect::<Vec<_>>());
        assert!(claimed.is_empty());
    }
}
#[test]
fn release_failure_never_skips_other_claims_or_becomes_disconnection_success() {
    let mut claimed = vec![2,3,4,5];
    let mut released = Vec::new();
    let result = release_all(&mut claimed, |interface| {
        released.push(interface);
        if interface == 4 { Err(device_error()) }
        else { Ok(CloseDisposition::DisconnectedAndHandleDropped) }
    });
    let error = result.unwrap_err();
    assert!(error.uncertain);
    assert_eq!(released, [5,4,3,2]);
    assert!(claimed.is_empty());
}

#[test]
fn uncertain_observation_survives_definite_disconnect_and_failed_release_wins() {
    let error = settle::<()>(Err(cleanup_error()), Err(device_error())).unwrap_err();
    assert!(error.uncertain);
    let error = settle::<()>(Err(read_error()), Err(cleanup_error())).unwrap_err();
    assert!(error.uncertain);
    let error = settle::<()>(Ok(()), Err(device_error())).unwrap_err();
    assert!(!error.uncertain);
    assert_eq!(error.code, "legacy_current_device_unconfirmed");
}
#[test]
fn ordinary_startup_fixture_is_limited_to_known_aids_and_fixed_reads() {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let mut transport = port(PhysicalTarget::synthetic_for_tests(), 4, false, false, trace.clone());
    for aid in proletarius_euicc::KNOWN_ISDR_APPLICATION_AIDS {
        let hex = aid.iter().map(|byte| format!("{byte:02X}")).collect::<String>();
        let result = transport.execute_command(&format!("AT+CCHO=\"{hex}\""), None,
            Duration::from_secs(1), 4096, true).unwrap();
        assert!(result.contains("ERROR"));
    }
    for command in ["AT+CPMS?", "AT+CPIN?"] {
        assert!(transport.execute_command(command, None, Duration::from_secs(1), 4096, true).unwrap().contains("OK"));
    }
    for command in ["AT+CFUN=1", "AT+CCHO=\"A00000\""] {
        assert!(transport.execute_command(command, None, Duration::from_secs(1), 4096, true).is_err());
    }
    checked_close(transport).unwrap();
}
