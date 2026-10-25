use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

#[test]
fn explicit_rejection_never_becomes_acknowledged_or_retry_authority() {
    assert_eq!(
        terminal_outcome(&Reply::response("ERROR\r\n".into())),
        SubmissionOutcome::KnownRejected
    );
    assert_eq!(
        terminal_outcome(&Reply::response("+CME ERROR: 3\r\n".into())),
        SubmissionOutcome::KnownRejected
    );
    assert_eq!(
        terminal_outcome(&Reply::response("OK\r\n".into())),
        SubmissionOutcome::Acknowledged
    );
    assert_eq!(
        terminal_outcome(&Reply::response("partial bytes".into())),
        SubmissionOutcome::SubmittedUnknown
    );
    assert_eq!(
        terminal_outcome(&Reply {
            text: "OK\r\n".into(),
            ok: true,
            uncertain: true,
            fatal: false,
            code: String::new()
        }),
        SubmissionOutcome::SubmittedUnknown
    );
}

#[test]
fn final_voice_target_compares_writable_fields_without_claiming_capability_write() {
    let target = VoiceConfiguration {
        ims: 2,
        volte_capability: 0,
        volte_disabled: 1,
    };
    assert!(same_voice(
        VoiceConfiguration {
            volte_capability: 1,
            ..target
        },
        target
    ));
    assert!(!same_voice(VoiceConfiguration { ims: 1, ..target }, target));
    assert!(!same_voice(
        VoiceConfiguration {
            volte_disabled: 0,
            ..target
        },
        target
    ));
}

fn descriptor(candidate: Candidate) -> InterfaceObservation {
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
fn successor_candidates_prefer_exact_tuple_and_reject_ambiguous_or_ep0_descriptors() {
    let stored = Candidate {
        interface: 3,
        alternate: 1,
        endpoint_in: 0x83,
        endpoint_out: 3,
    };
    let other = Candidate {
        interface: 2,
        alternate: 0,
        endpoint_in: 0x82,
        endpoint_out: 2,
    };
    assert_eq!(
        candidate_set(
            DEVICE_IDS[0],
            stored,
            vec![descriptor(other), descriptor(stored)]
        )
        .unwrap(),
        vec![stored, other]
    );
    assert!(
        candidate_set(
            DEVICE_IDS[0],
            stored,
            vec![descriptor(stored), descriptor(stored)]
        )
        .is_err()
    );
    assert!(
        candidate_set(
            DEVICE_IDS[0],
            stored,
            vec![descriptor(Candidate {
                endpoint_out: 0,
                ..stored
            })]
        )
        .is_err()
    );
    let shifted = Candidate {
        interface: 4,
        endpoint_in: 0x86,
        endpoint_out: 0x04,
        ..stored
    };
    assert_eq!(
        candidate_set(DEVICE_IDS[0], stored, vec![descriptor(shifted)]).unwrap(),
        vec![shifted]
    );
    assert!(
        candidate_set(
            DEVICE_IDS[0],
            stored,
            vec![descriptor(Candidate {
                endpoint_in: 0x83,
                endpoint_out: 3,
                ..shifted
            })]
        )
        .is_err()
    );
    assert!(
        candidate_set(
            DEVICE_IDS[0],
            stored,
            vec![(3, 1, 0xff, 0x42, 1, vec![(0x83, true), (3, true)])]
        )
        .is_err()
    );
}

#[test]
fn successor_candidates_accept_only_dji_vendor_serial_rndis_pair_and_prioritize_it() {
    let dji = DEVICE_IDS[0];
    let previous = Candidate {
        interface: 2,
        alternate: 0,
        endpoint_in: 0x84,
        endpoint_out: 0x03,
    };
    let rndis_at0 = Candidate {
        interface: 4,
        alternate: 0,
        endpoint_in: 0x86,
        endpoint_out: 0x04,
    };
    let rndis_at1 = Candidate {
        interface: 5,
        alternate: 0,
        endpoint_in: 0x88,
        endpoint_out: 0x05,
    };
    let mut ecm_network = descriptor(Candidate {
        interface: 4,
        ..rndis_at0
    });
    ecm_network.2 = 0x02;
    ecm_network.3 = 0x06;
    let mut mbim_network = descriptor(Candidate {
        interface: 5,
        ..rndis_at1
    });
    mbim_network.2 = 0x0a;
    mbim_network.4 = 2;
    assert_eq!(
        candidate_set(
            dji,
            previous,
            vec![descriptor(previous), ecm_network, mbim_network],
        )
        .unwrap(),
        vec![previous]
    );
    let selected = candidate_set(
        dji,
        previous,
        vec![
            descriptor(previous),
            descriptor(rndis_at1),
            descriptor(rndis_at0),
        ],
    )
    .unwrap();
    assert_eq!(selected, vec![rndis_at0, rndis_at1, previous]);
    assert!(candidate_set(DEVICE_IDS[1], previous, vec![descriptor(rndis_at0)]).is_err());
}
#[test]
fn successor_requires_complete_original_retirement_and_counts_all_topology_occupants() {
    let ports = [1, 2];
    let binding = BindingView {
        model: (0x2ca3, 0x4006),
        bus: 1,
        ports: &ports,
        language: 0x409,
        serial: "inert",
        candidate: Candidate {
            interface: 2,
            alternate: 0,
            endpoint_in: 0x82,
            endpoint_out: 2,
        },
    };
    let observation = |device, model| Observation {
        device,
        model,
        bus: binding.bus,
        ports: ports.to_vec(),
    };
    let mut retired = false;
    assert_eq!(
        retired_snapshot(
            vec![observation(1, binding.model)],
            &binding,
            &mut retired,
            |d| *d == 1
        )
        .unwrap(),
        None
    );
    assert!(!retired);
    assert_eq!(
        retired_snapshot(
            Vec::<Observation<u64>>::new(),
            &binding,
            &mut retired,
            |d| *d == 1
        )
        .unwrap(),
        None
    );
    assert!(retired);
    assert!(
        retired_snapshot(
            vec![observation(1, binding.model)],
            &binding,
            &mut retired,
            |d| *d == 1
        )
        .is_err()
    );
    assert!(
        retired_snapshot(
            vec![
                observation(2, binding.model),
                observation(3, (0xffff, 0xffff))
            ],
            &binding,
            &mut retired,
            |d| *d == 1
        )
        .is_err()
    );
    assert_eq!(
        retired_snapshot(
            vec![observation(2, binding.model)],
            &binding,
            &mut retired,
            |d| *d == 1
        )
        .unwrap(),
        Some(2)
    );
    assert!(
        retired_snapshot(
            (0..257).map(|_| observation(1, binding.model)).collect(),
            &binding,
            &mut false,
            |d| *d == 1
        )
        .is_err()
    );
}

struct ClosingPort {
    calls: Arc<Mutex<usize>>,
    panic: bool,
    failure: bool,
}
impl Transport for ClosingPort {
    fn command(
        &mut self,
        _: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        unreachable!()
    }
    fn description(&self) -> String {
        "inert close fixture".into()
    }
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        *self.calls.lock().unwrap() += 1;
        if self.panic {
            panic!("inert close panic");
        }
        if self.failure {
            Err(close_unconfirmed())
        } else {
            Ok(CloseDisposition::ReleasedAndHandleDropped)
        }
    }
}
#[test]
fn consuming_close_panic_and_failure_leave_no_second_close_opportunity() {
    for (panic, failure) in [(true, false), (false, true), (false, false)] {
        let calls = Arc::new(Mutex::new(0));
        let mut port: Option<Box<dyn Transport>> = Some(Box::new(ClosingPort {
            calls: calls.clone(),
            panic,
            failure,
        }));
        let result = checked_retire(&mut port);
        assert_eq!(result.is_err(), panic || failure);
        if let Err(error) = result {
            assert_eq!(error.code, "usb_at_close_unconfirmed");
        }
        assert!(port.is_none());
        assert_eq!(checked_retire(&mut port).unwrap(), None);
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}

/// These fixed cases enter the real owner's scope and the production session,
/// Plan, per-write ledger, framing and checked-close flow. Only enumeration
/// tokens and physical byte transfers are inert. No native Device or physical
/// target is fabricated, and callers cannot construct a NativeCompletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReconciliationFixtureScenario {
    Success,
    AlreadyTarget,
    RebootAckUnknown,
    ConfigWriteUnknown,
    FinalCloseError,
    CallbackPanic,
    SuccessorWrongImei,
    SuccessorProbeFallback,
    FailedCandidateCloseError,
    SuccessorVoiceUnconfirmed,
}
/// Test observations only. This object cannot mint native or ledger rights,
/// construct a completion, or supply transport admission. Call assert_reached
/// after the blocking worker joins, outside its callback catch_unwind.
#[derive(Clone, Default)]
pub(crate) struct ReconciliationArrivalProbe(Arc<Mutex<ArrivalRecord>>);
#[derive(Default)]
struct ArrivalRecord {
    reports: usize,
    observation: Option<ArrivalObservation>,
}
#[derive(Debug)]
struct ArrivalObservation {
    scenario: ReconciliationFixtureScenario,
    with_hook: bool,
    owned_voice_scenario: Option<voice_cleanup::ManagedVoiceScenario>,
    expected_config_writes: usize,
    original_closed: bool,
    reboot_submitted: bool,
    successor_opens: usize,
    checked_closes: usize,
    config_writes: usize,
    reboot_writes: usize,
    successor_probe_writes: usize,
    successor_identity_reads: usize,
    successor_usb_reads: usize,
    successor_voice_checks: usize,
    prepared_write_hook_boundaries: usize,
    success: bool,
    error_code: Option<&'static str>,
}
impl ReconciliationArrivalProbe {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn assert_reached(&self) {
        let observed = self.0.lock().unwrap();
        assert_eq!(
            observed.reports, 1,
            "exactly one native fixture must report arrival"
        );
        let state = observed
            .observation
            .as_ref()
            .expect("native fixture returned without arrival observations");
        let scenario = state.scenario;
        let with_hook = state.with_hook;
        let expected_config_writes = state.expected_config_writes;
        assert!(
            state.original_closed,
            "original byte transport escaped explicit close"
        );
        assert!(state.checked_closes >= 1);
        if let Some(voice_scenario) = state.owned_voice_scenario {
            use voice_cleanup::ManagedVoiceScenario::{InitiallyExited, Running};
            if !matches!(voice_scenario, Running | InitiallyExited) {
                assert!(
                    !state.success,
                    "unsettled original voice yielded completion"
                );
                assert_eq!(state.checked_closes, 1);
                assert_eq!(state.config_writes, 0);
                assert_eq!(state.reboot_writes, 0);
                assert!(!state.reboot_submitted);
                assert_eq!(state.successor_opens, 0);
                assert_eq!(state.successor_voice_checks, 0);
                return;
            }
        }
        if with_hook && scenario == ReconciliationFixtureScenario::Success {
            assert!(
                state.prepared_write_hook_boundaries >= 1,
                "hook scenario ended before its actual prepared-write boundary"
            );
            if !state.success {
                assert_eq!(state.prepared_write_hook_boundaries, 1);
                assert_eq!(
                    state.config_writes, 0,
                    "revoked first-write guard submitted bytes"
                );
                assert!(!state.reboot_submitted);
                assert_eq!(state.reboot_writes, 0);
                assert_eq!(state.successor_opens, 0);
                assert_eq!(state.checked_closes, 1);
                assert_eq!(state.successor_voice_checks, 0);
            }
        }
        // A late negative case must arrive at its intended stage; an unrelated
        // early voice Pending cannot masquerade as that branch's successful test.
        let expected_late = match scenario {
            ReconciliationFixtureScenario::SuccessorWrongImei => Some((1, 2, 1, 1, 0, 0)),
            ReconciliationFixtureScenario::FailedCandidateCloseError => Some((1, 2, 1, 0, 0, 0)),
            ReconciliationFixtureScenario::SuccessorVoiceUnconfirmed => Some((1, 2, 1, 1, 1, 1)),
            ReconciliationFixtureScenario::FinalCloseError => Some((1, 2, 2, 2, 2, 1)),
            ReconciliationFixtureScenario::SuccessorProbeFallback => Some((2, 3, 3, 2, 2, 1)),
            ReconciliationFixtureScenario::Success
            | ReconciliationFixtureScenario::AlreadyTarget
            | ReconciliationFixtureScenario::RebootAckUnknown
                if !with_hook =>
            {
                Some((1, 2, 2, 2, 2, 1))
            }
            _ => None,
        };
        if let Some((opens, closes, probes, identity_reads, usb_reads, voice_checks)) =
            expected_late
        {
            assert!(
                state.reboot_submitted,
                "late fixture never submitted its own reboot: {state:?}"
            );
            assert_eq!(
                state.reboot_writes, 1,
                "late fixture must submit exactly one new reboot"
            );
            assert_eq!(
                state.successor_opens, opens,
                "wrong number of actual successor opens"
            );
            assert_eq!(
                state.checked_closes, closes,
                "actual checked close did not cover each opened port"
            );
            assert_eq!(
                state.successor_probe_writes, probes,
                "successor probe stage was not reached"
            );
            assert_eq!(
                state.successor_identity_reads, identity_reads,
                "successor identity stage was not reached"
            );
            assert_eq!(
                state.successor_usb_reads, usb_reads,
                "successor target read stage was not reached"
            );
            assert_eq!(
                state.successor_voice_checks, voice_checks,
                "successor voice stage was not reached"
            );
            assert_eq!(
                state.config_writes,
                if scenario == ReconciliationFixtureScenario::AlreadyTarget {
                    0
                } else {
                    expected_config_writes
                },
                "fixed fresh diff writes were skipped or replayed"
            );
        }
        match scenario {
            ReconciliationFixtureScenario::ConfigWriteUnknown
            | ReconciliationFixtureScenario::CallbackPanic => {
                assert_eq!(
                    state.config_writes, 1,
                    "first prepared configuration write was not reached"
                );
                assert!(!state.reboot_submitted);
                assert_eq!(state.reboot_writes, 0);
                assert_eq!(state.successor_opens, 0);
                assert_eq!(state.checked_closes, 1);
                assert_eq!(state.successor_voice_checks, 0);
                assert!(!state.success);
            }
            ReconciliationFixtureScenario::SuccessorWrongImei
            | ReconciliationFixtureScenario::SuccessorVoiceUnconfirmed => assert!(!state.success),
            ReconciliationFixtureScenario::FinalCloseError
            | ReconciliationFixtureScenario::FailedCandidateCloseError => {
                assert_eq!(state.error_code, Some("usb_at_close_unconfirmed"));
            }
            ReconciliationFixtureScenario::SuccessorProbeFallback => assert!(
                state.success,
                "fallback must actually finish strict identity/voice, close and sealed completion"
            ),
            _ => {}
        }
        if state.success {
            assert_eq!(state.reboot_writes, 1);
            assert!(state.checked_closes >= 2);
        }
    }
}

struct ScriptState {
    usb: UsbComposition,
    voice: VoiceConfiguration,
    target_usb: UsbComposition,
    target_voice: VoiceConfiguration,
    imei: String,
    reboot_submitted: bool,
    original_closed: bool,
    successor_opens: usize,
    successor_probe_writes: usize,
    successor_identity_reads: usize,
    successor_usb_reads: usize,
    successor_voice_checks: usize,
    prepared_write_hook_boundaries: usize,
    checked_closes: usize,
    config_writes: usize,
    reboot_writes: usize,
}
struct ByteIo {
    state: Arc<Mutex<ScriptState>>,
    incoming: VecDeque<u8>,
    successor: bool,
    candidate_index: usize,
    scenario: ReconciliationFixtureScenario,
}
fn composition_text(usb: UsbComposition) -> String {
    format!(
        "+QCFG: \"usbcfg\",0x{:04X},0x{:04X},{}\r\nOK\r\n",
        usb.vendor_id,
        usb.product_id,
        usb.flags
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn composition_write(usb: UsbComposition) -> String {
    format!(
        "AT+QCFG=\"USBCFG\",0x{:04X},0x{:04X},{}",
        usb.vendor_id,
        usb.product_id,
        usb.flags
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}
impl Io for ByteIo {
    fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
        assert!(timeout >= MIN_TIMEOUT);
        if self.incoming.is_empty() {
            return Err(IoError::Timeout);
        }
        let length = bytes.len().min(self.incoming.len());
        for byte in &mut bytes[..length] {
            *byte = self.incoming.pop_front().unwrap();
        }
        Ok(length)
    }
    fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, IoError> {
        assert!(timeout >= MIN_TIMEOUT);
        let command = std::str::from_utf8(bytes.strip_suffix(b"\r").ok_or(IoError::Other)?)
            .map_err(|_| IoError::Other)?;
        let mut state = self.state.lock().unwrap();
        if self.successor {
            match command {
                "AT" => state.successor_probe_writes += 1,
                "AT+CGSN" => state.successor_identity_reads += 1,
                "AT+QCFG=\"USBCFG\"" => state.successor_usb_reads += 1,
                _ => {}
            }
        }
        let response = match command {
            "AT" if self.successor
                && self.candidate_index == 0
                && matches!(
                    self.scenario,
                    ReconciliationFixtureScenario::SuccessorProbeFallback
                        | ReconciliationFixtureScenario::FailedCandidateCloseError
                ) =>
            {
                "ERROR\r\n".into()
            }
            "AT" => "OK\r\n".into(),
            "AT+CGSN" => {
                let mut imei = state.imei.clone();
                if self.successor
                    && self.scenario == ReconciliationFixtureScenario::SuccessorWrongImei
                {
                    imei.replace_range(0..1, if imei.starts_with('0') { "1" } else { "0" });
                }
                format!("{imei}\r\nOK\r\n")
            }
            "ATI" => "fixture firmware\r\nOK\r\n".into(),
            "AT+CLCC" => "OK\r\n".into(),
            "AT+QCFG=\"USBCFG\"" => composition_text(state.usb),
            "AT+QCFG=\"ims\"" => format!(
                "+QCFG: \"ims\",{},{}\r\nOK\r\n",
                state.voice.ims, state.voice.volte_capability
            ),
            "AT+QCFG=\"volte_disable\"" => format!(
                "+QCFG: \"volte/disable\",{}\r\nOK\r\n",
                state.voice.volte_disabled
            ),
            "AT+CFUN=1,1" => {
                state.reboot_writes += 1;
                assert_eq!(state.reboot_writes, 1, "reboot was replayed");
                state.reboot_submitted = true;
                state.usb = state.target_usb;
                state.voice = state.target_voice;
                if self.scenario == ReconciliationFixtureScenario::RebootAckUnknown {
                    return Err(IoError::Disconnected);
                }
                "OK\r\n".into()
            }
            other => {
                assert!(
                    !self.successor,
                    "configuration write preceded successor identity"
                );
                let allowed = other == composition_write(state.target_usb)
                    || other == format!("AT+QCFG=\"ims\",{}", state.target_voice.ims)
                    || other
                        == format!(
                            "AT+QCFG=\"volte_disable\",{}",
                            state.target_voice.volte_disabled
                        );
                assert!(
                    allowed,
                    "unexpected command in sealed reconciliation fixture"
                );
                state.config_writes += 1;
                if self.scenario == ReconciliationFixtureScenario::CallbackPanic {
                    drop(state);
                    panic!("inert post-prepare callback panic");
                }
                if self.scenario == ReconciliationFixtureScenario::ConfigWriteUnknown {
                    return Err(IoError::Disconnected);
                }
                // Return the genuine ACK but keep old values until reboot,
                // exercising the normal partial pre-reboot readback policy.
                "OK\r\n".into()
            }
        };
        self.incoming.extend(response.bytes());
        Ok(bytes.len())
    }
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        let mut state = self.state.lock().unwrap();
        state.checked_closes += 1;
        if !self.successor {
            state.original_closed = true;
        }
        if self.successor
            && (self.scenario == ReconciliationFixtureScenario::FinalCloseError
                || (self.candidate_index == 0
                    && self.scenario == ReconciliationFixtureScenario::FailedCandidateCloseError))
        {
            return Err(close_unconfirmed());
        }
        Ok(CloseDisposition::ReleasedAndHandleDropped)
    }
}
struct TokenSession {
    state: Arc<Mutex<ScriptState>>,
    scenario: ReconciliationFixtureScenario,
    hook: Option<CommandHook>,
    owned_voice: Option<OwnedVoicePlan>,
}
struct OwnedVoicePlan {
    fixture: Arc<voice_cleanup::OriginalManagedVoiceFixture>,
    scenario: voice_cleanup::ManagedVoiceScenario,
    probe: voice_cleanup::ManagedVoiceArrivalProbe,
    hook: Option<Box<dyn FnMut() + Send>>,
}
type CommandHook = Arc<Mutex<Box<dyn FnMut(&str) + Send>>>;
struct HookedPort {
    inner: Box<dyn Transport>,
    hook: CommandHook,
    state: Arc<Mutex<ScriptState>>,
}
impl Transport for HookedPort {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        self.inner.checked_close()
    }
    fn command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<String, TransportError> {
        self.inner.command(command, follow_up, timeout, max_bytes)
    }
    fn command_with_guard(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<String, TransportError> {
        if command.starts_with("AT+QCFG=\"USBCFG\",")
            || command.starts_with("AT+QCFG=\"ims\",")
            || command.starts_with("AT+QCFG=\"volte_disable\",")
            || command == "AT+CFUN=1,1"
        {
            self.state.lock().unwrap().prepared_write_hook_boundaries += 1;
        }
        (self.hook.lock().unwrap())(command);
        // Cancellation/evidence changes in the hook are observed by the real
        // NativeTransport guard immediately before its byte write.
        self.inner
            .command_with_guard(command, follow_up, timeout, max_bytes, guard)
    }
    fn description(&self) -> String {
        "inert byte I/O boundary hook".into()
    }
}
impl SessionBackend for TokenSession {
    type Device = u64;
    fn initial(&mut self, binding: &BindingView<'_>) -> Result<u64, TransportError> {
        select_exact(
            vec![Observation {
                device: 1,
                model: binding.model,
                bus: binding.bus,
                ports: binding.ports.to_vec(),
            }],
            binding,
        )
    }
    fn open(
        &mut self,
        device: &u64,
        _: &BindingView<'_>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        assert!(matches!(*device, 1 | 2));
        let mut state = self.state.lock().unwrap();
        let index = state.successor_opens;
        if *device == 2 {
            assert!(state.reboot_submitted && state.original_closed);
            state.successor_opens += 1;
        }
        drop(state);
        let port: Box<dyn Transport> = Box::new(NativeTransport::new(
            Box::new(ByteIo {
                state: self.state.clone(),
                incoming: VecDeque::new(),
                successor: *device == 2,
                candidate_index: index,
                scenario: self.scenario,
            }),
            "inert reconciliation byte I/O".into(),
            None,
        ));
        Ok(match &self.hook {
            Some(hook) => Box::new(HookedPort {
                inner: port,
                hook: hook.clone(),
                state: self.state.clone(),
            }),
            None => port,
        })
    }
    fn settle_voice(
        &mut self,
        scope: &ReconciliationScope<'_>,
        _: &mut dyn Transport,
    ) -> Result<VoiceSettledProof, TransportError> {
        match self.owned_voice.as_mut() {
            Some(owned) => {
                let hook = owned
                    .hook
                    .as_mut()
                    .map(|hook| &mut **hook as &mut dyn FnMut());
                voice_cleanup::scripted_owned_settle_for_native(
                    scope,
                    &owned.fixture,
                    owned.scenario,
                    &owned.probe,
                    hook,
                )
            }
            None => voice_cleanup::scripted_settle_for_native(scope),
        }
    }
    fn verify_successor_voice(
        &mut self,
        scope: &ReconciliationScope<'_>,
        _: &mut dyn Transport,
        prior: VoiceSettledProof,
    ) -> Result<VoiceSettledProof, TransportError> {
        self.state.lock().unwrap().successor_voice_checks += 1;
        if self.scenario == ReconciliationFixtureScenario::SuccessorVoiceUnconfirmed {
            return Err(pending());
        }
        voice_cleanup::scripted_verify_successor_voice(scope, prior)
    }
    fn successor(
        &mut self,
        original: &u64,
        binding: &BindingView<'_>,
        scope: &ReconciliationScope<'_>,
    ) -> Result<u64, TransportError> {
        scope.check()?;
        assert_eq!(*original, 1);
        let state = self.state.lock().unwrap();
        assert!(state.reboot_submitted && state.original_closed);
        drop(state);
        let mut retired = false;
        assert!(
            retired_snapshot(
                vec![Observation {
                    device: 1,
                    model: binding.model,
                    bus: binding.bus,
                    ports: binding.ports.to_vec()
                }],
                binding,
                &mut retired,
                |token| *token == 1
            )?
            .is_none()
        );
        let successor = retired_snapshot(
            vec![Observation {
                device: 2,
                model: binding.model,
                bus: binding.bus,
                ports: binding.ports.to_vec(),
            }],
            binding,
            &mut retired,
            |token| *token == 1,
        )?
        .ok_or_else(pending)?;
        assert!(retired);
        Ok(successor)
    }
    fn candidates(
        &mut self,
        device: &u64,
        binding: &BindingView<'_>,
    ) -> Result<Vec<Candidate>, TransportError> {
        assert_eq!(*device, 2);
        let mut candidates = vec![binding.candidate];
        if matches!(
            self.scenario,
            ReconciliationFixtureScenario::SuccessorProbeFallback
                | ReconciliationFixtureScenario::FailedCandidateCloseError
        ) {
            candidates.push(Candidate {
                interface: if binding.candidate.interface == 2 {
                    3
                } else {
                    2
                },
                ..binding.candidate
            });
        }
        Ok(candidates)
    }
}

pub(crate) fn scripted_reconcile_observed(
    scope: &ReconciliationScope<'_>,
    scenario: ReconciliationFixtureScenario,
    hook: Option<Box<dyn FnMut(&str) + Send>>,
    probe: &ReconciliationArrivalProbe,
) -> Result<NativeCompletion, TransportError> {
    run_observed(scope, scenario, hook, probe, None)
}

pub(crate) fn scripted_reconcile_owned_observed(
    scope: &ReconciliationScope<'_>,
    fixture: Arc<voice_cleanup::OriginalManagedVoiceFixture>,
    voice_scenario: voice_cleanup::ManagedVoiceScenario,
    voice_probe: &voice_cleanup::ManagedVoiceArrivalProbe,
    hook: Option<Box<dyn FnMut() + Send>>,
    native_probe: &ReconciliationArrivalProbe,
) -> Result<NativeCompletion, TransportError> {
    run_observed(
        scope,
        ReconciliationFixtureScenario::Success,
        None,
        native_probe,
        Some(OwnedVoicePlan {
            fixture,
            scenario: voice_scenario,
            probe: voice_probe.clone(),
            hook,
        }),
    )
}

fn run_observed(
    scope: &ReconciliationScope<'_>,
    scenario: ReconciliationFixtureScenario,
    hook: Option<Box<dyn FnMut(&str) + Send>>,
    probe: &ReconciliationArrivalProbe,
    owned_voice: Option<OwnedVoicePlan>,
) -> Result<NativeCompletion, TransportError> {
    scope.check()?;
    let target_usb = scope.target_usb();
    let target_voice = scope.target_voice().unwrap_or(VoiceConfiguration {
        ims: 2,
        volte_capability: 1,
        volte_disabled: 1,
    });
    let mut usb = target_usb;
    let mut voice = target_voice;
    if scenario != ReconciliationFixtureScenario::AlreadyTarget {
        usb.flags[0] ^= 1;
        if scope.target_voice().is_some() {
            voice.ims = (voice.ims + 1) % 3;
            voice.volte_disabled ^= 1;
        }
    }
    let state = Arc::new(Mutex::new(ScriptState {
        usb,
        voice,
        target_usb,
        target_voice,
        imei: scope.expected_imei().into(),
        reboot_submitted: false,
        original_closed: false,
        successor_opens: 0,
        successor_probe_writes: 0,
        successor_identity_reads: 0,
        successor_usb_reads: 0,
        successor_voice_checks: 0,
        prepared_write_hook_boundaries: 0,
        checked_closes: 0,
        config_writes: 0,
        reboot_writes: 0,
    }));
    let with_hook = hook.is_some();
    let expected_config_writes = if scope.target_voice().is_some() { 3 } else { 1 };
    let owned_voice_scenario = owned_voice.as_ref().map(|owned| owned.scenario);
    let mut backend = TokenSession {
        state: state.clone(),
        scenario,
        hook: hook.map(|hook| Arc::new(Mutex::new(hook))),
        owned_voice,
    };
    let result = execute_session(scope, &mut backend);
    let state = state.lock().unwrap();
    let observation = ArrivalObservation {
        scenario,
        with_hook,
        owned_voice_scenario,
        expected_config_writes,
        original_closed: state.original_closed,
        reboot_submitted: state.reboot_submitted,
        successor_opens: state.successor_opens,
        checked_closes: state.checked_closes,
        config_writes: state.config_writes,
        reboot_writes: state.reboot_writes,
        successor_probe_writes: state.successor_probe_writes,
        successor_identity_reads: state.successor_identity_reads,
        successor_usb_reads: state.successor_usb_reads,
        successor_voice_checks: state.successor_voice_checks,
        prepared_write_hook_boundaries: state.prepared_write_hook_boundaries,
        success: result.is_ok(),
        error_code: result.as_ref().err().map(|error| error.code),
    };
    drop(state);
    let mut observed = probe.0.lock().unwrap();
    observed.reports += 1;
    observed.observation = Some(observation);
    drop(observed);
    result
}
