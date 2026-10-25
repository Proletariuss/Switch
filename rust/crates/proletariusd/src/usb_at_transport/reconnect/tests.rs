// Tests the actual production successor state machine with inert Device tokens.
// No token is a rusb Device, native PhysicalTarget, or native identity proof.
use super::*;
use std::collections::VecDeque;

const MODEL: (u16, u16) = DEVICE_IDS[0];
fn slot() -> Topology {
    Topology {
        bus: 0,
        ports: vec![1, 4],
    }
}
fn observation(device: u64, model: (u16, u16), topology: Topology) -> Observation<u64> {
    Observation {
        device,
        model,
        topology,
    }
}
fn anchor() -> CoreAnchor<u64> {
    CoreAnchor {
        original: 1,
        model: MODEL,
        topology: slot(),
        serial: Zeroizing::new("unit-完整".into()),
        transition: Mutex::new(Transition {
            detached: false,
            selected: None,
            failure: None,
        }),
    }
}
struct OpenedToken {
    device: u64,
    events: Arc<Mutex<Vec<String>>>,
}
impl Drop for OpenedToken {
    fn drop(&mut self) {
        self.events
            .lock()
            .unwrap()
            .push(format!("drop:{}", self.device));
    }
}
struct FixtureBackend {
    scans: VecDeque<Result<Vec<Observation<u64>>, TransportError>>,
    events: Arc<Mutex<Vec<String>>>,
    active: Arc<AtomicBool>,
    serial: String,
    serial_descriptor: Option<Vec<u8>>,
    revoke_on_serial: bool,
    fail_open: Option<TransportError>,
    deadline: Instant,
}
impl FixtureBackend {
    fn new(scans: Vec<Result<Vec<Observation<u64>>, TransportError>>) -> Self {
        Self {
            scans: scans.into(),
            events: Arc::new(Mutex::new(Vec::new())),
            active: Arc::new(AtomicBool::new(true)),
            serial: "unit-完整".into(),
            serial_descriptor: None,
            revoke_on_serial: false,
            fail_open: None,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}
impl Backend for FixtureBackend {
    type Device = u64;
    type Opened = OpenedToken;
    type Output = u64;
    fn fence(&mut self) -> Result<(), TransportError> {
        let active = self.active.clone();
        let guard: crate::module_radio::CallAdmission =
            Arc::new(move || active.load(Ordering::Acquire));
        check_finite(self.deadline, &guard).map(|_| ())
    }
    fn same_device(&self, a: &u64, b: &u64) -> bool {
        a == b
    }
    fn snapshot(&mut self) -> Result<Vec<Observation<u64>>, TransportError> {
        self.events.lock().unwrap().push("scan".into());
        self.scans
            .pop_front()
            .expect("unexpected enumeration after target selection")
    }
    fn open(&mut self, device: &u64) -> Result<OpenedToken, TransportError> {
        self.events.lock().unwrap().push(format!("open:{device}"));
        if let Some(error) = self.fail_open.take() {
            return Err(error);
        }
        Ok(OpenedToken {
            device: *device,
            events: self.events.clone(),
        })
    }
    fn serial(&mut self, opened: &mut OpenedToken) -> Result<Zeroizing<String>, TransportError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("serial:{}", opened.device));
        if self.revoke_on_serial {
            self.active.store(false, Ordering::Release);
        }
        let bytes = self
            .serial_descriptor
            .clone()
            .unwrap_or_else(|| descriptor(&self.serial.encode_utf16().collect::<Vec<_>>()));
        serial_value(&bytes)
    }
    fn retire_output(&mut self, output: u64) -> Result<(), TransportError> {
        self.events.lock().unwrap().push(format!("retire:{output}"));
        Ok(()) // inert output token, never a physical close receipt
    }
    fn claim_probe(&mut self, opened: OpenedToken) -> Result<u64, TransportError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("claim/probe:{}", opened.device));
        Ok(opened.device)
    }
}

#[test]
fn original_presence_never_opens_any_successor() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![Ok(vec![
        observation(1, MODEL, slot()),
        observation(2, MODEL, slot()),
    ])]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        SUCCESSOR_PENDING
    );
    assert_eq!(backend.events(), ["scan"]);
    assert!(!anchor.transition.lock().unwrap().detached);
}

#[test]
fn observed_detach_then_same_model_unique_slot_serial_precedes_claim_and_probe() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![
        Ok(vec![]),
        Ok(vec![
            observation(
                99,
                MODEL,
                Topology {
                    bus: 0,
                    ports: vec![2, 1],
                },
            ),
            observation(2, MODEL, slot()),
        ]),
    ]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        SUCCESSOR_PENDING
    );
    assert!(anchor.transition.lock().unwrap().detached);
    assert_eq!(execute_successor(&anchor, &mut backend).unwrap(), 2);
    assert_eq!(
        backend.events(),
        [
            "scan",
            "scan",
            "open:2",
            "serial:2",
            "claim/probe:2",
            "drop:2"
        ]
    );
    // A retry reopens only the retained selected token, never enumerates anew.
    assert_eq!(execute_successor(&anchor, &mut backend).unwrap(), 2);
    assert_eq!(
        backend
            .events()
            .iter()
            .filter(|event| event.as_str() == "scan")
            .count(),
        2
    );
}

#[test]
fn unsupported_slot_occupant_counts_as_ambiguity_before_any_open() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![Ok(vec![
        observation(2, MODEL, slot()),
        observation(3, (0xffff, 1), slot()),
    ])]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_successor_ambiguous"
    );
    assert_eq!(backend.events(), ["scan"]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_successor_ambiguous"
    );
    assert_eq!(
        backend.events(),
        ["scan"],
        "ambiguous anchor cannot later select a different device"
    );
}

#[test]
fn changed_vid_pid_is_rejected_before_open_even_if_other_model_is_supported() {
    for model in [DEVICE_IDS[1], (0xffff, 1)] {
        let anchor = anchor();
        let mut backend = FixtureBackend::new(vec![Ok(vec![observation(2, model, slot())])]);
        assert_eq!(
            execute_successor(&anchor, &mut backend).unwrap_err().code,
            "usb_at_successor_unconfirmed"
        );
        assert_eq!(backend.events(), ["scan"]);
    }
}

#[test]
fn incomplete_snapshot_never_proves_detach_or_opens_a_candidate() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![Err(unconfirmed())]);
    assert!(execute_successor(&anchor, &mut backend).is_err());
    assert!(!anchor.transition.lock().unwrap().detached);
    assert_eq!(backend.events(), ["scan"]);
}

#[test]
fn wrong_topology_and_excessive_scan_cannot_pick_an_alternative() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![Ok(vec![observation(
        2,
        MODEL,
        Topology {
            bus: 1,
            ports: vec![1, 4],
        },
    )])]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        SUCCESSOR_PENDING
    );
    assert_eq!(backend.events(), ["scan"]);
    let mut backend = FixtureBackend::new(vec![Ok((0..MAX_SCAN_DEVICES + 1)
        .map(|i| observation(i as u64 + 2, MODEL, slot()))
        .collect())]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_successor_unconfirmed"
    );
    assert_eq!(backend.events(), ["scan"]);
}

#[test]
fn restored_old_connection_object_after_detach_does_not_revive_the_anchor() {
    let anchor = anchor();
    let mut backend =
        FixtureBackend::new(vec![Ok(vec![]), Ok(vec![observation(1, MODEL, slot())])]);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        SUCCESSOR_PENDING
    );
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_successor_unconfirmed"
    );
    assert_eq!(backend.events(), ["scan", "scan"]);
}

#[test]
fn strict_serial_mismatch_closes_unclaimed_candidate_and_never_selects_another() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![Ok(vec![observation(2, MODEL, slot())])]);
    backend.serial = "unit-別物".into();
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_successor_unconfirmed"
    );
    assert_eq!(backend.events(), ["scan", "open:2", "serial:2", "drop:2"]);
    backend.serial = "unit-完整".into();
    assert!(execute_successor(&anchor, &mut backend).is_err());
    assert_eq!(backend.events(), ["scan", "open:2", "serial:2", "drop:2"]);
}

#[test]
fn empty_and_malformed_serial_from_the_production_decoder_never_claim_or_probe() {
    for bytes in [
        descriptor(&[]),
        vec![6, 3, b'a', 0],   // incomplete descriptor, not a complete identity
        descriptor(&[0xd800]), // malformed UTF-16 must not become replacement text
    ] {
        let anchor = anchor();
        let mut backend = FixtureBackend::new(vec![Ok(vec![observation(2, MODEL, slot())])]);
        backend.serial_descriptor = Some(bytes);
        assert_eq!(
            execute_successor(&anchor, &mut backend).unwrap_err().code,
            "usb_at_successor_unconfirmed"
        );
        assert_eq!(backend.events(), ["scan", "open:2", "serial:2", "drop:2"]);
        assert!(execute_successor(&anchor, &mut backend).is_err());
        assert_eq!(backend.events(), ["scan", "open:2", "serial:2", "drop:2"]);
    }
}

#[test]
fn real_finite_guard_after_serial_prevents_claim_and_closes_the_opened_candidate() {
    let anchor = anchor();
    let mut backend = FixtureBackend::new(vec![Ok(vec![observation(2, MODEL, slot())])]);
    backend.revoke_on_serial = true;
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_reopen_revoked"
    );
    assert_eq!(backend.events(), ["scan", "open:2", "serial:2", "drop:2"]);
    backend.active.store(true, Ordering::Release);
    assert_eq!(
        execute_successor(&anchor, &mut backend).unwrap_err().code,
        "usb_at_reopen_revoked"
    );
}

#[test]
fn expired_or_revoked_admission_does_not_enumerate_and_original_open_error_never_falls_back() {
    for expired in [false, true] {
        let anchor = anchor();
        let mut backend = FixtureBackend::new(vec![]);
        if expired {
            backend.deadline = Instant::now();
        } else {
            backend.active.store(false, Ordering::Release);
        }
        assert!(execute_successor(&anchor, &mut backend).is_err());
        assert!(backend.events().is_empty());
    }
    for code in ["usb_at_target_disconnected", "usb_at_open_failed"] {
        let anchor = anchor();
        let mut backend = FixtureBackend::new(vec![Ok(vec![observation(2, MODEL, slot())])]);
        backend.fail_open = Some(failed(code, "retained open failed"));
        assert_eq!(
            execute_successor(&anchor, &mut backend).unwrap_err().code,
            code
        );
        assert_eq!(
            execute_successor(&anchor, &mut backend).unwrap_err().code,
            code
        );
        assert_eq!(backend.events(), ["scan", "open:2"]);
    }
}

fn descriptor(units: &[u16]) -> Vec<u8> {
    let mut bytes = vec![(units.len() * 2 + 2) as u8, 3];
    bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    bytes
}
#[test]
fn strict_standard_descriptor_decoders_preserve_full_identity_and_fail_closed() {
    let one = serial_value(&descriptor(&[0x4e01])).unwrap();
    let two = serial_value(&descriptor(&[0x4e02])).unwrap();
    assert_ne!(
        one.as_str(),
        two.as_str(),
        "non-ASCII serials must not collapse to '?' aliases"
    );
    let exact = " Serial-丁 ";
    assert_eq!(
        serial_value(&descriptor(&exact.encode_utf16().collect::<Vec<_>>()))
            .unwrap()
            .as_str(),
        exact
    );
    assert_eq!(
        languages(&descriptor(&[0x0409, 0x0411])).unwrap(),
        [0x0409, 0x0411]
    );
    for bytes in [
        vec![],
        vec![2, 3],
        vec![3, 3, 1],
        vec![4, 2, 1, 0],
        vec![6, 3, 1, 0],
        descriptor(&[0xd800]),
        descriptor(&[0]),
        descriptor(&[0x20, 0x20]),
    ] {
        assert!(
            serial_value(&bytes).is_err(),
            "accepted invalid or incomplete serial descriptor"
        );
    }
    for values in [&[][..], &[0][..], &[0x0409, 0x0409][..]] {
        assert!(languages(&descriptor(values)).is_err());
    }
    assert_eq!(
        serial_value(&descriptor(&vec![u16::from(b'a'); 126]))
            .unwrap()
            .len(),
        126
    );
    assert!(serial_value(&vec![0; 256]).is_err());
    assert!(
        !Topology {
            bus: 0,
            ports: vec![]
        }
        .usable()
    );
    assert!(
        !Topology {
            bus: 0,
            ports: vec![1, 0]
        }
        .usable()
    );
    assert!(
        slot().usable(),
        "bus zero is a valid bus identifier, not absence of topology"
    );
}

struct ProbeIo {
    active: Arc<AtomicBool>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    revoke_in_drain: bool,
}
impl Io for ProbeIo {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn read(&mut self, buffer: &mut [u8], _: Duration) -> Result<usize, IoError> {
        if self.writes.lock().unwrap().is_empty() {
            if self.revoke_in_drain {
                self.active.store(false, Ordering::Release);
            }
            return Err(IoError::Timeout);
        }
        let response = b"\r\nOK\r\n";
        buffer[..response.len()].copy_from_slice(response);
        Ok(response.len())
    }
    fn write(&mut self, data: &[u8], _: Duration) -> Result<usize, IoError> {
        self.writes.lock().unwrap().push(data.to_vec());
        Ok(data.len())
    }
}
#[test]
fn actual_native_probe_guard_runs_after_drain_before_its_first_at_payload() {
    for revoke in [false, true] {
        let active = Arc::new(AtomicBool::new(true));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let io = ProbeIo {
            active: active.clone(),
            writes: writes.clone(),
            revoke_in_drain: revoke,
        };
        let guard: crate::module_radio::CallAdmission =
            Arc::new(move || active.load(Ordering::Acquire));
        let result = probe_guarded(
            NativeTransport::new(Box::new(io), "targeted probe fixture".into(), None),
            Instant::now() + Duration::from_secs(1),
            guard,
        );
        if revoke {
            assert_eq!(result.err().unwrap().code, "usb_at_reopen_revoked");
            assert!(writes.lock().unwrap().is_empty());
        } else {
            assert!(result.is_ok());
            assert_eq!(*writes.lock().unwrap(), [b"AT\r".to_vec()]);
        }
    }
}

#[test]
fn synthetic_actor_anchor_has_explicit_model_but_cannot_open_native_usb() {
    let source = PhysicalTarget::synthetic_for_tests();
    let anchor = ReenumerationAnchor::synthetic_for_tests(source.clone(), MODEL);
    assert!(anchor.same_instance(&anchor.clone()));
    assert!(anchor.same_source(&source));
    assert!(!anchor.same_source(&PhysicalTarget::synthetic_for_tests()));
    assert!(
        matches!(anchor.inner.as_ref(), AnchorConnection::Synthetic { descriptor, .. } if *descriptor == MODEL)
    );
    let guard: crate::module_radio::CallAdmission = Arc::new(|| true);
    assert_eq!(
        open_exact(
            &source,
            Instant::now() + Duration::from_secs(1),
            guard.clone()
        )
        .err()
        .unwrap()
        .code,
        "usb_at_target_unconfirmed"
    );
    assert_eq!(
        open_successor(&anchor, Instant::now() + Duration::from_secs(1), guard)
            .err()
            .unwrap()
            .code,
        "usb_at_target_unconfirmed"
    );
    assert!(!format!("{anchor:?}").contains("Synthetic"));
    assert_eq!(anchor.durable_binding().unwrap(), None);
}

fn durable_observation() -> DurableUsbBinding {
    DurableUsbBinding::from_observation(
        MODEL,
        &Topology {
            bus: 19,
            ports: vec![2, 1, 4, 3, 6, 5, 7],
        },
        0x0409,
        "unit-完整-🙂",
        Candidate {
            interface: 3,
            alternate: 1,
            endpoint_in: 0x83,
            endpoint_out: 0x02,
        },
    )
    .unwrap()
}

#[test]
fn durable_binding_preserves_the_complete_observed_tuple_and_redacts_debug() {
    // Same production constructor used by the native anchor export. This is
    // data validation, not a synthetic native Device or reopen permission.
    let observation = durable_observation();
    let wire = serde_json::to_value(&observation).unwrap();
    assert_eq!(wire["schema_version"], 1);
    assert_eq!(wire["vendor_id"], MODEL.0);
    assert_eq!(wire["product_id"], MODEL.1);
    assert_eq!(wire["bus_number"], 19);
    assert_eq!(
        wire["port_numbers"],
        serde_json::json!([2, 1, 4, 3, 6, 5, 7])
    );
    assert_eq!(wire["serial_language"], 0x0409);
    assert_eq!(wire["serial_number"], "unit-完整-🙂");
    assert_eq!(wire["interface_number"], 3);
    assert_eq!(wire["alternate_setting"], 1);
    assert_eq!(wire["endpoint_in"], 0x83);
    assert_eq!(wire["endpoint_out"], 0x02);
    let decoded: DurableUsbBinding = serde_json::from_value(wire).unwrap();
    assert_eq!(observation, decoded);
    assert_eq!(
        format!("{decoded:?}"),
        "DurableUsbBinding { schema_version: 1, .. }"
    );
}

#[test]
fn durable_binding_accepts_both_dji_rndis_at_interfaces_only_for_dji() {
    let topology = Topology {
        bus: 19,
        ports: vec![2, 1, 4],
    };
    for (interface, endpoint_in, endpoint_out) in [(4, 0x86, 0x04), (5, 0x88, 0x05)] {
        let observation = DurableUsbBinding::from_observation(
            DJI_QDC507,
            &topology,
            0x0409,
            "dji-rndis-at",
            Candidate {
                interface,
                alternate: 0,
                endpoint_in,
                endpoint_out,
            },
        )
        .unwrap();
        let wire = serde_json::to_value(&observation).unwrap();
        assert_eq!(wire["interface_number"], interface);
        assert_eq!(wire["endpoint_in"], endpoint_in);
        assert_eq!(wire["endpoint_out"], endpoint_out);
        assert_eq!(
            serde_json::from_value::<DurableUsbBinding>(wire).unwrap(),
            observation
        );

        assert!(
            DurableUsbBinding::from_observation(
                DEVICE_IDS[1],
                &topology,
                0x0409,
                "other-modem",
                Candidate {
                    interface,
                    alternate: 0,
                    endpoint_in,
                    endpoint_out,
                },
            )
            .is_err()
        );
    }
    assert!(
        DurableUsbBinding::from_observation(
            DJI_QDC507,
            &topology,
            0x0409,
            "dji-rndis-at",
            Candidate {
                interface: 4,
                alternate: 0,
                endpoint_in: 0x88,
                endpoint_out: 0x05,
            },
        )
        .is_err()
    );
}

#[test]
fn durable_binding_deserialization_rejects_invalid_or_ambiguous_data() {
    let valid = serde_json::to_value(durable_observation()).unwrap();
    let invalid = [
        ("schema_version", serde_json::json!(0)),
        ("schema_version", serde_json::json!(2)),
        ("vendor_id", serde_json::json!(0)),
        ("product_id", serde_json::json!(0)),
        ("bus_number", serde_json::json!(256)),
        ("port_numbers", serde_json::json!([])),
        ("port_numbers", serde_json::json!([1, 0])),
        ("port_numbers", serde_json::json!([1, 2, 3, 4, 5, 6, 7, 8])),
        ("serial_language", serde_json::json!(0)),
        ("serial_number", serde_json::json!("")),
        ("serial_number", serde_json::json!(" 　")),
        ("serial_number", serde_json::json!("unit\u{0}tail")),
        ("serial_number", serde_json::json!("unit\n")),
        ("serial_number", serde_json::json!("🙂".repeat(64))),
        ("interface_number", serde_json::json!(1)),
        ("endpoint_in", serde_json::json!(0x80)),
        ("endpoint_in", serde_json::json!(0x93)),
        ("endpoint_out", serde_json::json!(0x82)),
        ("endpoint_out", serde_json::json!(0)),
        ("reopen_authorized", serde_json::json!(true)),
    ];
    for (key, value) in invalid {
        let mut input = valid.clone();
        input[key] = value;
        assert!(
            serde_json::from_value::<DurableUsbBinding>(input).is_err(),
            "{key}"
        );
    }
    let mut missing = valid;
    missing.as_object_mut().unwrap().remove("serial_language");
    assert!(serde_json::from_value::<DurableUsbBinding>(missing).is_err());
}

#[test]
fn durable_binding_usb_string_limit_counts_utf16_units_without_normalizing() {
    let mut wire = serde_json::to_value(durable_observation()).unwrap();
    let exact = "🙂".repeat(63);
    wire["serial_number"] = serde_json::json!(exact);
    let decoded: DurableUsbBinding = serde_json::from_value(wire).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["serial_number"],
        exact
    );
}
