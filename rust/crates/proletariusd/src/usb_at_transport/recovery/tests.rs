//! Inert token snapshots exercise the actual selector/read/retirement flow.
//! Tokens never become a native Device, PhysicalTarget, or reconnect anchor.
use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

fn binding() -> BindingView<'static> {
    BindingView {
        model: DEVICE_IDS[0],
        bus: 0,
        ports: &[1, 4],
        language: 0x409,
        serial: "unit-完整",
        candidate: Candidate {
            interface: 2,
            alternate: 1,
            endpoint_in: 0x82,
            endpoint_out: 0x02,
        },
    }
}
fn observation(device: u64, model: (u16, u16), ports: &[u8]) -> Observation<u64> {
    Observation {
        device,
        model,
        bus: 0,
        ports: ports.to_vec(),
    }
}
struct FixtureAuthority {
    active: Arc<AtomicBool>,
    deadline: Instant,
}
impl FixtureAuthority {
    fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
}
impl Authority for FixtureAuthority {
    fn check(&self) -> Result<(), TransportError> {
        if self.active.load(Ordering::Acquire) && Instant::now() < self.deadline {
            Ok(())
        } else {
            Err(failed("fixture_revoked", "Inert fixture authority ended"))
        }
    }
    fn deadline(&self) -> Instant {
        self.deadline
    }
    fn admission(&self) -> crate::module_radio::CallAdmission {
        let active = self.active.clone();
        let deadline = self.deadline;
        Arc::new(move || active.load(Ordering::Acquire) && Instant::now() < deadline)
    }
    fn expected_imei(&self) -> &str {
        "123456789012345"
    }
}
struct FixtureTransport {
    replies: VecDeque<(&'static str, &'static str)>,
    events: Arc<Mutex<Vec<String>>>,
    active: Arc<AtomicBool>,
    close: Result<CloseDisposition, TransportError>,
    index: usize,
    fail_at: Option<usize>,
    panic_at: Option<usize>,
    revoke_at: Option<usize>,
    panic_close: bool,
    revoke_on_close: bool,
    on_command: Option<Box<dyn FnMut(usize) + Send>>,
}
impl Transport for FixtureTransport {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        self.events.lock().unwrap().push("checked-close".into());
        if self.revoke_on_close {
            self.active.store(false, Ordering::Release);
        }
        if self.panic_close {
            panic!("inert checked close panic");
        }
        self.close.clone()
    }
    fn command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        assert!(follow_up.is_none());
        assert!(timeout >= MIN_TIMEOUT && timeout < Duration::from_secs(6));
        let index = self.index;
        self.index += 1;
        self.events.lock().unwrap().push(command.to_owned());
        if let Some(on_command) = &mut self.on_command {
            on_command(index);
        }
        if self.panic_at == Some(index) {
            panic!("inert command panic");
        }
        if self.revoke_at == Some(index) {
            self.active.store(false, Ordering::Release);
        }
        if self.fail_at == Some(index) {
            return Err(TransportError::new(
                "fixture_read_error",
                "Inert read failed",
                b"private-response",
                true,
            ));
        }
        let (expected, response) = self
            .replies
            .pop_front()
            .expect("unexpected extra startup command");
        assert_eq!(command, expected);
        Ok(response.into())
    }
    fn description(&self) -> String {
        "inert startup fixture".into()
    }
}
struct FixtureBackend {
    snapshot: Option<Result<Vec<Observation<u64>>, TransportError>>,
    transport: Option<FixtureTransport>,
    events: Arc<Mutex<Vec<String>>>,
    fail_open: bool,
}
impl FixtureBackend {
    fn new(authority: &FixtureAuthority) -> Self {
        Self::with_active(authority.active.clone())
    }
    fn with_active(active: Arc<AtomicBool>) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let transport = FixtureTransport {
            replies: VecDeque::from([
                ("AT", "OK\r\n"),
                ("AT+CGSN", "123456789012345\r\nOK\r\n"),
                ("ATI", "Quectel\r\nRevision: fixture\r\nOK\r\n"),
                (
                    r#"AT+QCFG="USBCFG""#,
                    "+QCFG: \"usbcfg\",0x2CA3,0x4006,1,1,1,1,1,0,1\r\nOK\r\n",
                ),
                (r#"AT+QCFG="ims""#, "+QCFG: \"ims\",2,0\r\nOK\r\n"),
                (
                    r#"AT+QCFG="volte_disable""#,
                    "+QCFG: \"volte/disable\",1\r\nOK\r\n",
                ),
            ]),
            events: events.clone(),
            active,
            close: Ok(CloseDisposition::ReleasedAndHandleDropped),
            index: 0,
            fail_at: None,
            panic_at: None,
            revoke_at: None,
            panic_close: false,
            revoke_on_close: false,
            on_command: None,
        };
        Self {
            snapshot: Some(Ok(vec![observation(7, DEVICE_IDS[0], &[1, 4])])),
            transport: Some(transport),
            events,
            fail_open: false,
        }
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}
impl Backend for FixtureBackend {
    type Device = u64;
    fn snapshot(&mut self) -> Result<Vec<Observation<u64>>, TransportError> {
        self.events.lock().unwrap().push("snapshot".into());
        self.snapshot
            .take()
            .expect("fresh enumeration retried after target selection")
    }
    fn open_selected(
        &mut self,
        selected: &u64,
        _: &BindingView<'_>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        self.events.lock().unwrap().push(format!("open:{selected}"));
        if self.fail_open {
            return Err(failed("fixture_open_error", "Inert open failed"));
        }
        Ok(Box::new(
            self.transport
                .take()
                .expect("a second selected device was opened"),
        ))
    }
}

/// A fixed software script for the real coordinator's sealed scope tests.
/// It can neither construct authority nor return a caller-fabricated readback.
pub(crate) enum OwnerFixtureScenario {
    Success,
    CloseError,
    Disconnected,
}
struct FixtureByteIo {
    script: FixtureTransport,
    incoming: VecDeque<u8>,
}
impl Io for FixtureByteIo {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        let Self { script, .. } = *self;
        Box::new(script).checked_close()
    }
    fn read(&mut self, output: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
        assert!(timeout >= MIN_TIMEOUT);
        if self.incoming.is_empty() {
            return Err(IoError::Timeout);
        }
        let length = output.len().min(self.incoming.len());
        for byte in &mut output[..length] {
            *byte = self.incoming.pop_front().unwrap();
        }
        Ok(length)
    }
    fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, IoError> {
        let command = bytes.strip_suffix(b"\r").ok_or(IoError::Other)?;
        let command = std::str::from_utf8(command).map_err(|_| IoError::Other)?;
        let response = self
            .script
            .command(command, None, timeout, 4096)
            .map_err(|_| IoError::Other)?;
        self.incoming.extend(response.bytes());
        Ok(bytes.len())
    }
}
struct OwnerByteBackend(FixtureBackend);
impl Backend for OwnerByteBackend {
    type Device = u64;
    fn snapshot(&mut self) -> Result<Vec<Observation<u64>>, TransportError> {
        self.0.snapshot()
    }
    fn open_selected(
        &mut self,
        selected: &u64,
        _: &BindingView<'_>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        self.0
            .events
            .lock()
            .unwrap()
            .push(format!("open:{selected}"));
        let script = self
            .0
            .transport
            .take()
            .expect("owner byte fixture reopened selected token");
        // Real production framing, draining, byte-write admission and response
        // collection execute here. Only physical byte transfers are scripted.
        Ok(Box::new(NativeTransport::new(
            Box::new(FixtureByteIo {
                script,
                incoming: VecDeque::new(),
            }),
            "inert startup byte I/O".into(),
            None,
        )))
    }
}
pub(crate) fn scripted_read_for_owner(
    scope: &NativeIdentityScope<'_>,
    scenario: OwnerFixtureScenario,
    on_command: Option<Box<dyn FnMut(usize) + Send>>,
) -> Result<NativeReadback, TransportError> {
    scope.check()?;
    let view = BindingView::new(scope.binding());
    // This fixture's active bit is not passed to execute. Every actual scope
    // and transfer fence below uses the genuine owner and its original locks.
    let mut backend = FixtureBackend::with_active(Arc::new(AtomicBool::new(false)));
    backend.snapshot = Some(Ok(vec![Observation {
        device: 7,
        model: view.model,
        bus: view.bus,
        ports: view.ports.to_vec(),
    }]));
    let transport = backend.transport.as_mut().unwrap();
    transport.on_command = on_command;
    transport.close = match scenario {
        OwnerFixtureScenario::Success => Ok(CloseDisposition::ReleasedAndHandleDropped),
        OwnerFixtureScenario::CloseError => Err(close_unconfirmed()),
        OwnerFixtureScenario::Disconnected => Ok(CloseDisposition::DisconnectedAndHandleDropped),
    };
    execute(scope, &view, &mut OwnerByteBackend(backend))
}

#[test]
fn exact_topology_counts_unsupported_and_wrong_serial_candidates_before_any_open() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.snapshot = Some(Ok(vec![
        observation(7, DEVICE_IDS[0], &[1, 4]),
        observation(8, (0xffff, 0xffff), &[1, 4]),
    ]));
    assert_eq!(
        execute(&authority, &binding(), &mut backend)
            .unwrap_err()
            .code,
        "startup_native_target_ambiguous"
    );
    assert_eq!(backend.events(), ["snapshot"]);
}

#[test]
fn absence_model_mismatch_and_incomplete_snapshot_never_open_or_probe() {
    for snapshot in [
        Ok(vec![]),
        Ok(vec![observation(7, (0xffff, 0xffff), &[1, 4])]),
        Err(unconfirmed()),
    ] {
        let authority = FixtureAuthority::new();
        let mut backend = FixtureBackend::new(&authority);
        backend.snapshot = Some(snapshot);
        assert!(execute(&authority, &binding(), &mut backend).is_err());
        assert_eq!(backend.events(), ["snapshot"]);
    }
}

#[test]
fn bounded_complete_snapshot_rejects_overflow_before_open() {
    let observations = (0..257)
        .map(|device| observation(device, DEVICE_IDS[0], &[2]))
        .collect();
    assert!(select_exact(observations, &binding()).is_err());
}

fn descriptor(candidate: Candidate) -> (u8, u8, u8, u8, u8, Vec<(u8, bool)>) {
    (
        candidate.interface,
        candidate.alternate,
        0xff,
        0,
        0,
        vec![
            (candidate.endpoint_in, true),
            (candidate.endpoint_out, true),
        ],
    )
}

#[test]
fn strict_tuple_rejects_duplicates_extra_bulk_wrong_class_and_different_alternate() {
    let candidate = binding().candidate;
    assert_eq!(
        strict_candidate(binding().model, candidate, [descriptor(candidate)]).unwrap(),
        candidate
    );
    assert!(
        strict_candidate(
            binding().model,
            candidate,
            [descriptor(candidate), descriptor(candidate)]
        )
        .is_err()
    );
    let mut extra = descriptor(candidate);
    extra.5.push((0x83, true));
    assert!(strict_candidate(binding().model, candidate, [extra]).is_err());
    let mut class = descriptor(candidate);
    class.2 = 0x02;
    assert!(strict_candidate(binding().model, candidate, [class]).is_err());
    let mut other = descriptor(candidate);
    other.1 = 0;
    assert!(strict_candidate(binding().model, candidate, [other]).is_err());
    let mut duplicate_endpoint = descriptor(candidate);
    duplicate_endpoint.5 = vec![(candidate.endpoint_in, true), (candidate.endpoint_in, true)];
    assert!(strict_candidate(binding().model, candidate, [duplicate_endpoint]).is_err());
}

#[test]
fn strict_candidate_allows_dji_rndis_at_but_not_other_modem_interfaces() {
    let dji = DEVICE_IDS[0];
    let rndis = Candidate {
        interface: 4,
        alternate: 0,
        endpoint_in: 0x86,
        endpoint_out: 0x04,
    };
    assert_eq!(
        strict_candidate(dji, rndis, [descriptor(rndis)]).unwrap(),
        rndis
    );
    assert!(strict_candidate(DEVICE_IDS[1], rndis, [descriptor(rndis)]).is_err());
    let rndis_at1 = Candidate {
        interface: 5,
        endpoint_in: 0x88,
        endpoint_out: 0x05,
        ..rndis
    };
    assert_eq!(
        strict_candidate(dji, rndis_at1, [descriptor(rndis_at1)]).unwrap(),
        rndis_at1
    );
    assert!(
        strict_candidate(
            dji,
            Candidate {
                interface: 5,
                ..rndis
            },
            [descriptor(Candidate {
                interface: 5,
                ..rndis
            })]
        )
        .is_err()
    );
}

#[test]
fn actual_three_read_plans_produce_facts_only_after_explicit_close() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    let result = execute(&authority, &binding(), &mut backend).unwrap();
    assert_eq!(result.identity().imei, authority.expected_imei());
    assert_eq!(result.usb_composition().flags, [1, 1, 1, 1, 1, 0, 1]);
    assert_eq!(result.voice().ims, 2);
    assert_eq!(
        result.close_disposition(),
        CloseDisposition::ReleasedAndHandleDropped
    );
    assert_eq!(
        backend.events(),
        [
            "snapshot",
            "open:7",
            "AT",
            "AT+CGSN",
            "ATI",
            r#"AT+QCFG="USBCFG""#,
            r#"AT+QCFG="ims""#,
            r#"AT+QCFG="volte_disable""#,
            "checked-close"
        ]
    );
    assert!(!format!("{result:?}").contains(authority.expected_imei()));
}

#[test]
fn imei_mismatch_closes_before_any_configuration_query() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.transport.as_mut().unwrap().replies[1].1 = "999999999999999\r\nOK\r\n";
    assert!(execute(&authority, &binding(), &mut backend).is_err());
    assert_eq!(
        backend.events(),
        [
            "snapshot",
            "open:7",
            "AT",
            "AT+CGSN",
            "ATI",
            "checked-close"
        ]
    );
}

#[test]
fn every_failed_read_and_revocation_explicitly_closes_without_next_query() {
    for index in 0..6 {
        for revoked in [false, true] {
            let authority = FixtureAuthority::new();
            let mut backend = FixtureBackend::new(&authority);
            let transport = backend.transport.as_mut().unwrap();
            if revoked {
                transport.revoke_at = Some(index);
            } else {
                transport.fail_at = Some(index);
            }
            let error = execute(&authority, &binding(), &mut backend).unwrap_err();
            let events = backend.events();
            assert_eq!(events.len(), index + 4);
            assert_eq!(events.last().unwrap(), "checked-close");
            assert!(error.response.is_empty());
            assert!(!format!("{error:?}").contains("private-response"));
        }
    }
}

#[test]
fn command_panic_is_retired_through_checked_close() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.transport.as_mut().unwrap().panic_at = Some(2);
    assert_eq!(
        execute(&authority, &binding(), &mut backend)
            .unwrap_err()
            .code,
        "startup_native_callback_panicked"
    );
    assert_eq!(backend.events().last().unwrap(), "checked-close");
}

#[test]
fn callback_panic_debt_survives_concurrent_authority_revocation() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    let active = authority.active.clone();
    backend.transport.as_mut().unwrap().on_command = Some(Box::new(move |_| {
        active.store(false, Ordering::Release);
        panic!("inert callback panic after revocation");
    }));
    assert_eq!(
        execute(&authority, &binding(), &mut backend)
            .unwrap_err()
            .code,
        "startup_native_callback_panicked"
    );
    assert_eq!(backend.events().last().unwrap(), "checked-close");
}

#[test]
fn incomplete_advisory_firmware_stream_is_retired_before_configuration_reads() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.transport.as_mut().unwrap().replies[2].1 = "Quectel\r\n";
    assert!(execute(&authority, &binding(), &mut backend).is_err());
    assert_eq!(
        backend.events(),
        [
            "snapshot",
            "open:7",
            "AT",
            "AT+CGSN",
            "ATI",
            "checked-close"
        ]
    );
}

#[test]
fn malformed_identity_usb_and_voice_responses_are_not_live_read_success() {
    for (index, response) in [
        (0, "ERROR\r\n"),
        (1, "1234567890123456\r\nOK\r\n"),
        (3, "+QCFG: \"usbcfg\",0x2CA3,0x4006,1,1\r\nOK\r\n"),
        (4, "+QCFG: \"ims\",9,0\r\nOK\r\n"),
        (5, "+QCFG: \"volte/disable\",9\r\nOK\r\n"),
    ] {
        let authority = FixtureAuthority::new();
        let mut backend = FixtureBackend::new(&authority);
        backend.transport.as_mut().unwrap().replies[index].1 = response;
        assert!(execute(&authority, &binding(), &mut backend).is_err());
        assert_eq!(backend.events().last().unwrap(), "checked-close");
    }
}

#[test]
fn close_failure_overrides_success_and_original_read_error() {
    for fail_read in [false, true] {
        let authority = FixtureAuthority::new();
        let mut backend = FixtureBackend::new(&authority);
        let transport = backend.transport.as_mut().unwrap();
        transport.close = Err(close_unconfirmed());
        if fail_read {
            transport.fail_at = Some(0);
        }
        assert_eq!(
            execute(&authority, &binding(), &mut backend)
                .unwrap_err()
                .code,
            "usb_at_close_unconfirmed"
        );
        assert_eq!(backend.events().last().unwrap(), "checked-close");
    }
}

#[test]
fn consuming_close_panic_becomes_close_failure_after_success_or_read_failure() {
    for fail_read in [false, true] {
        let authority = FixtureAuthority::new();
        let mut backend = FixtureBackend::new(&authority);
        let transport = backend.transport.as_mut().unwrap();
        transport.panic_close = true;
        if fail_read {
            transport.fail_at = Some(0);
        }
        assert_eq!(
            execute(&authority, &binding(), &mut backend)
                .unwrap_err()
                .code,
            "usb_at_close_unconfirmed"
        );
        assert_eq!(backend.events().last().unwrap(), "checked-close");
    }
}

#[test]
fn authority_change_during_close_rejects_live_read_success_after_resource_retirement() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.transport.as_mut().unwrap().revoke_on_close = true;
    assert_eq!(
        execute(&authority, &binding(), &mut backend)
            .unwrap_err()
            .code,
        "fixture_revoked"
    );
    assert_eq!(backend.events().last().unwrap(), "checked-close");
}

#[test]
fn disconnected_close_remains_typed_resource_retirement_not_device_recovery() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.transport.as_mut().unwrap().close = Ok(CloseDisposition::DisconnectedAndHandleDropped);
    let result = execute(&authority, &binding(), &mut backend).unwrap();
    assert_eq!(
        result.close_disposition(),
        CloseDisposition::DisconnectedAndHandleDropped
    );
    assert_eq!(backend.events().last().unwrap(), "checked-close");
}

#[test]
fn expired_authority_never_enumerates_or_opens() {
    let mut authority = FixtureAuthority::new();
    authority.deadline = Instant::now();
    let mut backend = FixtureBackend::new(&authority);
    assert!(execute(&authority, &binding(), &mut backend).is_err());
    assert!(backend.events().is_empty());
}

#[test]
fn selected_open_error_is_terminal_without_fallback_or_probe() {
    let authority = FixtureAuthority::new();
    let mut backend = FixtureBackend::new(&authority);
    backend.fail_open = true;
    assert!(execute(&authority, &binding(), &mut backend).is_err());
    assert_eq!(backend.events(), ["snapshot", "open:7"]);
}
