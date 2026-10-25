use super::*;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use switch_voice_supervisor::Exit;

struct Root {
    path: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
}
impl Root {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/runtime")
            .join(format!("mc588-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let lease = crate::daemon_lifetime::Lease::acquire(&path).unwrap();
        Self {
            path,
            lease: Some(lease),
        }
    }
    fn control(&self) -> Arc<ManagedControl> {
        ManagedControl::create(
            &self.path,
            &self.lease.as_ref().unwrap().root_proof(),
            "producer-a",
            1,
        )
        .unwrap()
    }
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(self.path.join(STORE))
            .unwrap()
            .map(|entry| {
                let e = entry.unwrap();
                (
                    e.file_name().into_string().unwrap(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        drop(self.lease.take());
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn completion(control: &ManagedControl) -> Completion {
    let nonce = "42".repeat(32);
    Completion {
        schema_version: 1,
        session: control.binding.session.clone(),
        producer: "producer-a".into(),
        generation: 1,
        record_sha256: control.binding.record_sha256.clone(),
        release_nonce: nonce.clone(),
        signed_release: switch_voice_supervisor::encode_response(
            &control.binding.session,
            &nonce,
            ControlCommand::Release,
            ControlState::Exited,
            42,
            Exit::Code(0),
            &control.key[..],
        )
        .unwrap(),
    }
}
#[test]
fn actual_private_record_is_durable_bound_and_redacted() {
    let root = Root::new();
    let control = root.control();
    control.validate().unwrap();
    let files = root.files();
    assert_eq!(files.len(), 1);
    assert!(completed_store_files(&files).is_err());
    assert_eq!(control.record.metadata().unwrap().mode() & 0o7777, 0o600);
    assert_eq!(control.directory.metadata().unwrap().mode() & 0o7777, 0o700);
    let debug = format!("{control:?} {:?}", control.binding());
    assert!(!debug.contains(&hex(&control.key[..])));
    assert!(!debug.contains(&control.binding.session));
    let public = serde_json::to_string(&control.binding()).unwrap();
    assert!(!public.contains("key"));
}
#[test]
fn record_same_length_mutation_is_sticky_and_never_repaired() {
    let root = Root::new();
    let control = root.control();
    let original = control.payload.to_vec();
    let mut changed = original.clone();
    let last = changed.len() - 1;
    changed[last] = b' ';
    control.record.write_at(&changed, 0).unwrap();
    assert!(control.validate().is_err());
    control.record.write_at(&original, 0).unwrap();
    assert!(control.validate().is_err());
    assert_eq!(
        std::fs::read(
            root.path
                .join(STORE)
                .join(format!("{}.json", control.binding.session))
        )
        .unwrap(),
        original
    );
}
#[test]
fn dropped_lifetime_and_reacquired_same_path_cannot_revive_original_control() {
    let mut root = Root::new();
    let control = root.control();
    drop(root.lease.take());
    assert!(control.validate().is_err());
    root.lease = Some(crate::daemon_lifetime::Lease::acquire(&root.path).unwrap());
    assert!(control.validate().is_err());
}
#[test]
fn replaced_store_and_record_names_fail_without_touching_foreign_bytes() {
    let root = Root::new();
    let control = root.control();
    let original = root.path.join(STORE);
    let moved = root.path.join("retained-control");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(original.join("foreign"), b"preserve").unwrap();
    assert!(control.validate().is_err());
    assert_eq!(
        std::fs::read(original.join("foreign")).unwrap(),
        b"preserve"
    );
}
#[test]
fn pure_completion_verifier_requires_exact_pair_and_original_signed_release() {
    let root = Root::new();
    let control = root.control();
    let receipt = completion(&control);
    control.persist_completion(&receipt).unwrap();
    let files = root.files();
    completed_store_files(&files).unwrap();
    let mut extra = files.clone();
    extra.insert("unknown".into(), vec![]);
    assert!(completed_store_files(&extra).is_err());
    assert!(completed_store_files(&BTreeMap::new()).is_err());
    let mut wrong = files.clone();
    let name = format!("{}.complete.json", control.binding.session);
    let mut receipt = receipt;
    receipt.release_nonce = "43".repeat(32);
    wrong.insert(name.clone(), serde_json::to_vec(&receipt).unwrap());
    assert!(completed_store_files(&wrong).is_err());
    let mut wrong = files;
    let mut value: serde_json::Value = serde_json::from_slice(wrong.get(&name).unwrap()).unwrap();
    value["generation"] = serde_json::json!(2);
    wrong.insert(name, serde_json::to_vec(&value).unwrap());
    assert!(completed_store_files(&wrong).is_err());
}
#[test]
fn completion_is_no_overwrite_and_sync_retry_preserves_exact_bytes() {
    let root = Root::new();
    let control = root.control();
    let receipt = completion(&control);
    control.persist_completion(&receipt).unwrap();
    let before = root.files();
    control.persist_completion(&receipt).unwrap();
    assert_eq!(before, root.files());
    let mut other = receipt;
    other.generation = 2;
    assert!(control.persist_completion(&other).is_err());
    assert_eq!(before, root.files());
}
struct Script<'a> {
    control: &'a ManagedControl,
    commands: Vec<String>,
    stopping: bool,
    bad_nonce: bool,
    read_error: bool,
}
impl ModuleVoiceADB for Script<'_> {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        _: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        self.commands.push(command.into());
        if command.contains(" client ") {
            let nonce = command
                .split("--nonce '")
                .nth(1)
                .unwrap()
                .split('\'')
                .next()
                .unwrap();
            let (cmd, state) = if command.ends_with(" stop") {
                self.stopping = true;
                (ControlCommand::Stop, ControlState::Stopping)
            } else if command.ends_with(" status") && self.stopping {
                (ControlCommand::Status, ControlState::Exited)
            } else {
                (ControlCommand::Status, ControlState::Running)
            };
            let nonce = if self.bad_nonce {
                "00".repeat(32)
            } else {
                nonce.into()
            };
            let exit = if state == ControlState::Exited {
                Exit::Code(1)
            } else {
                Exit::None
            };
            let mut output = switch_voice_supervisor::encode_response(
                &self.control.binding.session,
                &nonce,
                cmd,
                state,
                42,
                exit,
                &self.control.key[..],
            )
            .unwrap()
            .into_bytes();
            output.extend_from_slice(b"\n__SWITCH_ADB_STATUS_fixture_0__\n");
            return parse_shell_result(&output, "fixture");
        }
        if command.contains("for p in") && self.read_error {
            return Ok((vec![], 2));
        }
        Ok((vec![], 0))
    }
}
#[test]
fn authenticated_stopping_requires_reaped_poll_then_real_cleanup_and_strict_readback() {
    let root = Root::new();
    let control = root.control();
    let mut adb = Script {
        control: &control,
        commands: vec![],
        stopping: false,
        bad_nonce: false,
        read_error: false,
    };
    assert!(!route(&mut adb, &control, VoiceRouteAction::Stop, &mut || Ok(())).unwrap());
    assert!(control.stopped.load(Ordering::Acquire));
    assert_eq!(
        adb.commands.iter().filter(|c| c.ends_with(" stop")).count(),
        1
    );
    assert_eq!(
        adb.commands
            .iter()
            .filter(|c| c.ends_with(" status"))
            .count(),
        1
    );
    assert!(adb.commands.iter().any(|c| c.contains("printf 'T")));
    assert!(adb.commands.last().unwrap().contains("for p in"));
    assert!(root.files().keys().all(|k| !k.ends_with(".complete.json")));
}
#[test]
fn valid_child_exit_cannot_turn_failed_dsp_read_into_completion() {
    let root = Root::new();
    let control = root.control();
    let mut adb = Script {
        control: &control,
        commands: vec![],
        stopping: false,
        bad_nonce: false,
        read_error: true,
    };
    assert!(route(&mut adb, &control, VoiceRouteAction::Stop, &mut || Ok(())).is_err());
    assert!(!control.stopped.load(Ordering::Acquire));
    assert_eq!(root.files().len(), 1);
}
#[test]
fn signed_wrong_nonce_is_sticky_and_prevents_any_dsp_mutation() {
    let root = Root::new();
    let control = root.control();
    let mut adb = Script {
        control: &control,
        commands: vec![],
        stopping: false,
        bad_nonce: true,
        read_error: false,
    };
    assert!(route(&mut adb, &control, VoiceRouteAction::Stop, &mut || Ok(())).is_err());
    assert_eq!(adb.commands.len(), 1);
    adb.bad_nonce = false;
    assert!(control.request(&mut adb, ControlCommand::Status).is_err());
    assert_eq!(root.files().len(), 1);
}
struct CloseTarget {
    events: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
}
struct CloseIo {
    events: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
}
impl Drop for CloseIo {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("drop");
    }
}
impl BulkIo for CloseIo {
    fn read_bulk(&mut self, _: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
        panic!("no protocol I/O in close test")
    }
    fn write_bulk(&mut self, _: &[u8], _: Duration) -> Result<usize, ProbeError> {
        panic!("no protocol I/O in close test")
    }
    fn close_confirmed(&mut self) -> Result<(), ProbeError> {
        self.events.lock().unwrap().push("release");
        if self.fail { Err(failure()) } else { Ok(()) }
    }
}
impl AdbTarget for CloseTarget {
    fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
        self.events.lock().unwrap().push("open");
        Ok(Box::new(CloseIo {
            events: self.events.clone(),
            fail: self.fail,
        }))
    }
}
#[test]
fn checked_lane_propagates_release_failure_and_drops_original_handle_before_return() {
    for fail in [false, true] {
        let events = Arc::new(Mutex::new(vec![]));
        let plan = AdbPlan::from_target(Box::new(CloseTarget {
            events: events.clone(),
            fail,
        }));
        let result = plan.with_usb_checked(|_| Ok(7));
        assert_eq!(result.is_err(), fail);
        assert_eq!(*events.lock().unwrap(), vec!["open", "release", "drop"]);
    }
}
#[test]
fn optional_public_control_binding_validates_session_scope_and_never_serializes_key() {
    let root = Root::new();
    let control = root.control();
    let mut value = serde_json::to_value(control.binding()).unwrap();
    assert!(serde_json::from_value::<DurableControlBinding>(value.clone()).is_ok());
    value["session"] = serde_json::json!("../foreign");
    assert!(serde_json::from_value::<DurableControlBinding>(value).is_err());
    assert!(valid_pid_path(&control.pid_path()));
    assert!(!valid_pid_path("/run/proletarius-voice-route.pid"));
}

#[test]
fn foreign_root_proof_refuses_before_creating_store() {
    let a = Root::new();
    let b = Root::new();
    let before = std::fs::metadata(&b.path).unwrap();
    assert!(
        ManagedControl::create(
            &b.path,
            &a.lease.as_ref().unwrap().root_proof(),
            "producer-a",
            1
        )
        .is_err()
    );
    assert!(!b.path.join(STORE).exists());
    let after = std::fs::metadata(&b.path).unwrap();
    assert_eq!(Stamp::of(&before) == Stamp::of(&after), true);
}
#[test]
fn partial_final_receipt_is_retained_unknown_and_never_repaired_or_retired() {
    let root = Root::new();
    let control = root.control();
    let receipt = completion(&control);
    let path = root
        .path
        .join(STORE)
        .join(format!("{}.complete.json", control.binding.session));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(b"{\"schema").unwrap();
    file.sync_all().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(control.persist_completion(&receipt).is_err());
    assert!(completed_store_files(&root.files()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
#[tokio::test]
async fn actual_normal_stop_entry_cannot_create_legacy_ownership_or_touch_any_transport() {
    let root = Root::new();
    let producer = format!("unowned-{}", crate::snapshot::new_instance_id());
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    let usb = crate::usb_at::Owner::with_factory(
        producer.clone(),
        Arc::new(|| panic!("unowned Stop must not open any USB transport")),
    )
    .with_recovery_gate_for_tests(gate.clone());
    let modem = crate::modem_actor::ModemActor::with_test_port_opener_and_module_recovery(
        |_, _| panic!("unowned Stop must not open serial"),
        gate.clone(),
    )
    .unwrap();
    let radio = crate::module_radio::Service::new(
        root.path.clone(),
        usb.clone(),
        modem,
        crate::availability::BackendAvailability::with_module_recovery(gate),
        producer.clone(),
    );
    let before = Stamp::of(&std::fs::metadata(&root.path).unwrap());
    let spawned = usb.spawned_io_for_tests();
    let error = execute_voice_route_action_with_admission(
        &root.path,
        crate::message_archive::InboxState::for_go_producer(producer.clone()),
        usb.clone(),
        radio,
        producer.clone(),
        VoiceRouteAction::Stop,
        StopPolicy::Always,
        None,
        VoiceAdmission::Immediate,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure.code, "module_voice_cleanup_owner_unknown");
    assert!(!error.stop_confirmed);
    assert!(retained_voice_obligation(&producer).unwrap().is_none());
    assert_eq!(usb.spawned_io_for_tests(), spawned);
    assert!(before == Stamp::of(&std::fs::metadata(&root.path).unwrap()));
    assert!(!root.path.join(STORE).exists());
}

struct ReleaseTarget {
    control: Arc<ManagedControl>,
    events: Arc<Mutex<Vec<&'static str>>>,
    fail_close: bool,
}
struct ReleaseWire {
    control: Arc<ManagedControl>,
    events: Arc<Mutex<Vec<&'static str>>>,
    out: Vec<u8>,
    input: std::collections::VecDeque<u8>,
    fail_close: bool,
}
impl AdbTarget for ReleaseTarget {
    fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
        self.events.lock().unwrap().push("open");
        Ok(Box::new(ReleaseWire {
            control: self.control.clone(),
            events: self.events.clone(),
            out: vec![],
            input: Default::default(),
            fail_close: self.fail_close,
        }))
    }
}
impl Drop for ReleaseWire {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("drop");
    }
}
fn wire_frame(tag: &[u8; 4], a: u32, b: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = encode_header(command(tag), a, b, payload).unwrap().to_vec();
    frame.extend_from_slice(payload);
    frame
}
impl BulkIo for ReleaseWire {
    fn close_confirmed(&mut self) -> Result<(), ProbeError> {
        self.events.lock().unwrap().push("close");
        if self.fail_close {
            Err(failure())
        } else {
            Ok(())
        }
    }
    fn read_bulk(&mut self, bytes: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
        assert!(!self.input.is_empty());
        let len = bytes.len().min(self.input.len()).min(7);
        for byte in &mut bytes[..len] {
            *byte = self.input.pop_front().unwrap();
        }
        Ok(len)
    }
    fn write_bulk(&mut self, bytes: &[u8], _: Duration) -> Result<usize, ProbeError> {
        self.out.extend_from_slice(bytes);
        while self.out.len() >= 24 {
            let (tag, local, _, size, _, _) = decode_header(&self.out[..24])?;
            let end = 24 + size as usize;
            if self.out.len() < end {
                break;
            }
            let payload = self.out[24..end].to_vec();
            self.out.drain(..end);
            if tag == ADB_CNXN {
                self.input.extend(wire_frame(
                    b"CNXN",
                    ADB_VERSION,
                    MAX_PAYLOAD as u32,
                    b"device::fixture\0",
                ));
            } else if tag == ADB_OPEN {
                let service = std::str::from_utf8(payload.strip_suffix(&[0]).unwrap()).unwrap();
                assert!(service.contains("--command release"));
                self.events.lock().unwrap().push("RELEASE");
                let nonce = service
                    .split("--nonce '")
                    .nth(1)
                    .unwrap()
                    .split('\'')
                    .next()
                    .unwrap();
                let token = service
                    .split("__SWITCH_ADB_STATUS_")
                    .nth(1)
                    .unwrap()
                    .split("_%u__")
                    .next()
                    .unwrap();
                let signed = switch_voice_supervisor::encode_response(
                    &self.control.binding.session,
                    nonce,
                    ControlCommand::Release,
                    ControlState::Exited,
                    42,
                    Exit::Code(0),
                    &self.control.key[..],
                )
                .unwrap();
                let response = format!("{signed}\n__SWITCH_ADB_STATUS_{token}_0__\n");
                self.input.extend(wire_frame(b"OKAY", 42, local, b""));
                self.input
                    .extend(wire_frame(b"WRTE", 42, local, response.as_bytes()));
                self.input.extend(wire_frame(b"CLSE", 42, local, b""));
            }
        }
        Ok(bytes.len())
    }
}
fn release_plan(
    control: &Arc<ManagedControl>,
    fail_close: bool,
) -> (AdbPlan, Arc<Mutex<Vec<&'static str>>>) {
    let events = Arc::new(Mutex::new(vec![]));
    (
        AdbPlan::from_target(Box::new(ReleaseTarget {
            control: control.clone(),
            events: events.clone(),
            fail_close,
        })),
        events,
    )
}
#[test]
fn actual_adb_framing_checked_close_precedes_completed_receipt_and_close_failure_never_seals() {
    for fail in [false, true] {
        let root = Root::new();
        let control = root.control();
        let mut adb = Script {
            control: &control,
            commands: vec![],
            stopping: false,
            bad_nonce: false,
            read_error: false,
        };
        route(&mut adb, &control, VoiceRouteAction::Stop, &mut || Ok(())).unwrap();
        let (plan, events) = release_plan(&control, fail);
        let result = control.release(&plan, &mut || Ok(()));
        assert_eq!(result.is_err(), fail);
        assert_eq!(
            *events.lock().unwrap(),
            vec!["open", "RELEASE", "close", "drop"]
        );
        assert_eq!(control.local_completion().unwrap().is_none(), fail);
        assert_eq!(root.files().len(), if fail { 1 } else { 2 });
        if !fail {
            completed_store_files(&root.files()).unwrap();
        }
    }
}
#[tokio::test]
async fn actual_closed_receipt_retry_bypasses_disconnected_device_and_retire_exact_original() {
    let root = Root::new();
    let producer = format!("closed-{}", crate::snapshot::new_instance_id());
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(root.lease.as_ref().unwrap());
    let control = ManagedControl::create(
        &root.path,
        &root.lease.as_ref().unwrap().root_proof(),
        &producer,
        1,
    )
    .unwrap();
    let mut adb = Script {
        control: &control,
        commands: vec![],
        stopping: false,
        bad_nonce: false,
        read_error: false,
    };
    route(&mut adb, &control, VoiceRouteAction::Stop, &mut || Ok(())).unwrap();
    let (plan, events) = release_plan(&control, false);
    control
        .completion_fail_before_sync
        .store(true, Ordering::Release);
    assert!(control.release(&plan, &mut || Ok(())).is_err());
    assert!(control.local_completion().unwrap().is_some());
    let after_close = events.lock().unwrap().clone();
    let original = VoiceCleanupObligation {
        producer: producer.clone(),
        generation: 1,
        target: VoiceBoundTarget {
            plan,
            capture: None,
            managed: Some(control.clone()),
        },
        authority: Arc::new(gate.admit_operation(&producer).unwrap()),
    };
    let admission = ADB_ADMISSION.acquire().await.unwrap();
    voice_route_ownership()
        .lock()
        .unwrap()
        .begin(original)
        .unwrap();
    let usb = crate::usb_at::Owner::with_factory(
        "replacement".into(),
        Arc::new(|| panic!("local completion cannot capture/open USB")),
    )
    .with_recovery_gate_for_tests(gate.clone());
    let modem = crate::modem_actor::ModemActor::with_test_port_opener_and_module_recovery(
        |_, _| panic!("local completion cannot open serial"),
        gate.clone(),
    )
    .unwrap();
    let radio = crate::module_radio::Service::new(
        root.path.clone(),
        usb.clone(),
        modem,
        crate::availability::BackendAvailability::with_module_recovery(gate),
        "replacement".into(),
    );
    let result = execute_voice_route_action_with_admission(
        &root.path,
        crate::message_archive::InboxState::for_go_producer("replacement".into()),
        usb.clone(),
        radio,
        producer.clone(),
        VoiceRouteAction::Stop,
        StopPolicy::Always,
        None,
        VoiceAdmission::Immediate,
        Some(admission),
    )
    .await
    .unwrap();
    assert!(!result);
    assert!(retained_voice_obligation(&producer).unwrap().is_none());
    assert_eq!(*events.lock().unwrap(), after_close);
    assert_eq!(usb.spawned_io_for_tests(), 0);
    completed_store_files(&root.files()).unwrap();
}
