//! macOS host composition: production owner/Child/UDS, protocol, shell wrapper,
//! fragmented ADB framing, STOP executor, checked close and durable completion.
//! The shell helper, DSP and USB lease are explicit host substitutes. This does
//! not execute the embedded ARM supervisor or exercise a physical USB device.
#![cfg(target_os = "macos")]

use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::AtomicU64;
use std::thread::JoinHandle;
use switch_voice_supervisor::{ClientConfig, ServeConfig, client, serve};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Close,
    LostRelease,
    MissingProtocolLf,
    ExtraProtocolLf,
}

struct Bridge {
    socket: PathBuf,
    key_file: PathBuf,
    exit_gate: PathBuf,
    completion: PathBuf,
    session: String,
    events: Vec<&'static str>,
    replies: Vec<(ControlCommand, String, String)>,
    terminal: bool,
    dsp_disabled: bool,
    usb_restored: bool,
    fault: Fault,
}
impl Bridge {
    fn request(
        &self,
        command: ControlCommand,
        nonce: &str,
    ) -> Result<String, switch_voice_supervisor::Error> {
        client(ClientConfig {
            socket: self.socket.clone(),
            key_file: self.key_file.clone(),
            session: self.session.clone(),
            nonce: nonce.into(),
            command,
        })
    }

    // Only generated, scope-checked supervisor commands are dispatched. No
    // shell command is executed by the ADB peer and no response MAC is forged.
    fn shell(&mut self, service: &str) -> Vec<u8> {
        let (command, wrapper) = service
            .strip_prefix("shell:{ ")
            .unwrap()
            .rsplit_once("; }; __switch_status=")
            .unwrap();
        let token = wrapper
            .split("__SWITCH_ADB_STATUS_")
            .nth(1)
            .unwrap()
            .split("_%u__")
            .next()
            .unwrap();
        let mut output = Vec::new();
        if command.starts_with(&format!("'{SUPERVISOR}' client ")) {
            let remote = format!("/tmp/proletarius-call/control-{}", self.session);
            let nonce = quoted(command, "--nonce '");
            let action = if command.ends_with("--command stop") {
                self.events.push("STOP");
                ControlCommand::Stop
            } else if command.ends_with("--command status") {
                self.events.push("STATUS");
                if self.events.contains(&"STOP") {
                    // A private helper-only gate ensures STOP cannot race all
                    // the way to EXITED before production requests STATUS.
                    fs::write(&self.exit_gate, b"exit permitted").unwrap();
                }
                ControlCommand::Status
            } else {
                assert!(command.ends_with("--command release"));
                assert!(self.terminal && self.dsp_disabled && self.usb_restored);
                assert!(!self.completion.exists());
                self.events.push("RELEASE");
                ControlCommand::Release
            };
            let word = match action {
                ControlCommand::Stop => "stop",
                ControlCommand::Status => "status",
                ControlCommand::Release => "release",
            };
            assert_eq!(
                command,
                format!(
                    "'{SUPERVISOR}' client --socket '{remote}/socket' --key-file '{remote}/key' --session '{}' --nonce '{nonce}' --command {word}",
                    self.session,
                )
            );
            let signed = self.request(action, nonce).unwrap();
            assert!(signed.ends_with('\n') && !signed.ends_with("\n\n"));
            // client() already authenticates the real owner's bytes and EOF.
            // The production managed-control request independently verifies
            // those same bytes after traversing the ADB wrapper and frames.
            let fields: Vec<_> = signed.split_ascii_whitespace().collect();
            assert_eq!(fields.len(), 9);
            if action == ControlCommand::Status || action == ControlCommand::Stop {
                // The state is observed again by the production verifier;
                // DSP substitution is allowed only after that terminal line.
                self.terminal = fields[5] == "EXITED";
            }
            self.replies.push((action, nonce.into(), signed.clone()));
            output = signed.into_bytes();
            if action == ControlCommand::Release {
                match self.fault {
                    Fault::LostRelease => output.clear(),
                    Fault::MissingProtocolLf => {
                        output.pop();
                    }
                    Fault::ExtraProtocolLf => output.push(b'\n'),
                    Fault::None | Fault::Close => {}
                }
            }
        } else if command.starts_with("echo 0 > /sys/class/android_usb/f_audio/audio_enable") {
            assert!(self.terminal);
            // Production's ordinary Rust string spells \\n for one literal
            // shell backslash+n. A raw fixture string spells that only once.
            // Keep the complete original T -> T -> B cleanup command exact.
            assert_eq!(
                command,
                r#"echo 0 > /sys/class/android_usb/f_audio/audio_enable || exit $?; if test -p /run/voc_svr; then printf 'T\n' > /run/voc_svr && printf 'T\n' > /run/voc_svr && printf 'B\n' > /run/voc_svr || exit $?; fi; test "$(cat /sys/class/android_usb/f_audio/audio_enable)" = 0"#,
                "DSP fixture must match the exact production cleanup command"
            );
            self.events.push("DSP disable");
            self.dsp_disabled = true;
        } else {
            assert!(
                command.starts_with("a=$(head -c 3 /sys/class/android_usb/f_audio/audio_enable)")
            );
            assert!(command.contains(
                "/proc/asound/card0/pcm4p/sub0/status /proc/asound/card0/pcm4c/sub0/status"
            ));
            assert!(command.contains("test \"$s\" = closed || exit 3"));
            assert!(self.terminal && self.dsp_disabled);
            self.events.push("DSP strict readback");
        }
        output.extend_from_slice(format!("\n__SWITCH_ADB_STATUS_{token}_0__\n").as_bytes());
        output
    }
}

fn quoted<'a>(command: &'a str, prefix: &str) -> &'a str {
    command
        .split(prefix)
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap()
}

struct Target(Arc<Mutex<Bridge>>);
struct Wire {
    bridge: Arc<Mutex<Bridge>>,
    outgoing: Vec<u8>,
    incoming: VecDeque<u8>,
    released: bool,
}
fn frame(tag: &[u8; 4], a: u32, b: u32, payload: &[u8]) -> Vec<u8> {
    let mut result = encode_header(command(tag), a, b, payload).unwrap().to_vec();
    result.extend_from_slice(payload);
    result
}
impl AdbTarget for Target {
    fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
        self.0.lock().unwrap().events.push("open");
        Ok(Box::new(Wire {
            bridge: self.0.clone(),
            outgoing: Vec::new(),
            incoming: VecDeque::new(),
            released: false,
        }))
    }
}
impl BulkIo for Wire {
    fn close_confirmed(&mut self) -> Result<(), ProbeError> {
        let mut bridge = self.bridge.lock().unwrap();
        assert!(!bridge.completion.exists());
        bridge.events.push(if self.released {
            "RELEASE close"
        } else {
            "close"
        });
        if self.released && bridge.fault == Fault::Close {
            return Err(failure());
        }
        Ok(())
    }
    fn read_bulk(&mut self, bytes: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
        assert!(
            !self.incoming.is_empty(),
            "bounded peer has no response bytes"
        );
        let count = bytes.len().min(self.incoming.len()).min(7);
        for byte in &mut bytes[..count] {
            *byte = self.incoming.pop_front().unwrap();
        }
        Ok(count)
    }
    fn write_bulk(&mut self, bytes: &[u8], _: Duration) -> Result<usize, ProbeError> {
        // Production deliberately rejects partial USB writes; fragment reads
        // and WRTE records while accepting each complete requested write.
        let count = bytes.len();
        self.outgoing.extend_from_slice(&bytes[..count]);
        while self.outgoing.len() >= 24 {
            let (tag, local, _, size, _, _) = decode_header(&self.outgoing[..24])?;
            let end = 24 + size as usize;
            if self.outgoing.len() < end {
                break;
            }
            let payload = self.outgoing[24..end].to_vec();
            self.outgoing.drain(..end);
            if tag == ADB_CNXN {
                self.incoming.extend(frame(
                    b"CNXN",
                    ADB_VERSION,
                    MAX_PAYLOAD as u32,
                    b"device::host-owner-fixture\0",
                ));
            } else if tag == ADB_OPEN {
                let service = std::str::from_utf8(payload.strip_suffix(&[0]).unwrap()).unwrap();
                self.released = service.contains("--command release");
                let output = self.bridge.lock().unwrap().shell(service);
                self.incoming.extend(frame(b"OKAY", 42, local, b""));
                // Distinct WRTE messages also split the signed record and LF.
                for part in output.chunks(19) {
                    self.incoming.extend(frame(b"WRTE", 42, local, part));
                }
                self.incoming.extend(frame(b"CLSE", 42, local, b""));
            } else {
                assert!(tag == ADB_OKAY || tag == ADB_CLSE);
            }
        }
        Ok(count)
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .events
            .push(if self.released {
                "RELEASE drop"
            } else {
                "drop"
            });
    }
}

struct Fixture {
    root: PathBuf,
    lease: Option<crate::daemon_lifetime::Lease>,
    control: Arc<ManagedControl>,
    bridge: Arc<Mutex<Bridge>>,
    plan: AdbPlan,
    owner: Option<JoinHandle<Result<(), switch_voice_supervisor::Error>>>,
}
impl Fixture {
    fn new(fault: Fault) -> Self {
        let root = std::env::temp_dir().join(format!(
            "i589c-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let control =
            ManagedControl::create(&root, &lease.root_proof(), "composition-589", 1).unwrap();
        let key_file = root.join("key");
        let helper = root.join("helper");
        let socket = root.join("socket");
        assert!(
            socket.as_os_str().len() < 100,
            "Darwin UDS path must be bounded"
        );
        let mut key = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_file)
            .unwrap();
        key.write_all(hex(&control.key[..]).as_bytes()).unwrap();
        key.sync_all().unwrap();
        // The shell is a host-only executable fixture. Its TERM trap waits
        // for an explicit private gate, so STOP must require real wait/poll.
        // macOS /bin/sh provides SECONDS; even a failed owner handshake cannot
        // leave an indefinitely live fixture child. Normal cleanup uses STOP.
        fs::write(&helper, b"#!/bin/sh\nSECONDS=0\ntrap 'while ! test -f \"$0.exit\"; do test \"$SECONDS\" -lt 30 || exit 70; /bin/sleep 0.01; done; exit 0' TERM\n: > \"$0.ready\"\nwhile test \"$SECONDS\" -lt 30; do :; done\nexit 70\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let bridge = Arc::new(Mutex::new(Bridge {
            socket: socket.clone(),
            key_file: key_file.clone(),
            exit_gate: helper.with_extension("exit"),
            completion: root
                .join(STORE)
                .join(format!("{}.complete.json", control.binding.session)),
            session: control.binding.session.clone(),
            events: Vec::new(),
            replies: Vec::new(),
            terminal: false,
            dsp_disabled: false,
            usb_restored: false,
            fault,
        }));
        let config = ServeConfig {
            socket,
            key_file,
            session: control.binding.session.clone(),
            helper: helper.clone(),
            child_pid_file: root.join("child.pid"),
        };
        let owner = std::thread::spawn(move || serve(config));
        let fixture = Self {
            root,
            lease: Some(lease),
            control,
            plan: AdbPlan::from_target(Box::new(Target(bridge.clone()))),
            bridge,
            owner: Some(owner),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !helper.with_extension("ready").exists() {
            assert!(
                !fixture.owner.as_ref().unwrap().is_finished(),
                "real host owner exited before helper readiness"
            );
            assert!(Instant::now() < deadline, "real helper readiness timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut adb = ModuleVoiceADBSession::checked(deadline, fixture.plan.clone());
        assert_eq!(
            fixture
                .control
                .request(&mut adb, ControlCommand::Status)
                .unwrap()
                .state(),
            ControlState::Running
        );
        fixture.bridge.lock().unwrap().events.clear();
        fixture
    }
    async fn stop(&self, fail_usb: bool) -> Result<bool, RouteExecutionFailure> {
        let plan = self.plan.clone();
        let control = self.control.clone();
        let bridge = self.bridge.clone();
        run_voice_route_pipeline(
            (),
            move || Ok(((), (plan, control), true)),
            || async { Ok(()) },
            move |(plan, control)| {
                let mut adb =
                    ModuleVoiceADBSession::checked(Instant::now() + Duration::from_secs(10), plan);
                route(&mut adb, &control, VoiceRouteAction::Stop, &mut || Ok(()))
                    .map_err(|error| RouteExecutionFailure::probe(error, Some(StopPhase::Stop)))
            },
            move |()| async move {
                let mut bridge = match bridge.lock() {
                    Ok(bridge) => bridge,
                    Err(poisoned) => {
                        // The production worker has already captured the
                        // original fixture panic. Preserve that diagnostic
                        // rather than replacing it with a second poison panic.
                        let mut bridge = poisoned.into_inner();
                        bridge.events.push("USB restore unconfirmed after fixture panic");
                        bridge.usb_restored = false;
                        return Err("composition fixture bridge poisoned by the preceding ADB worker assertion; USB restoration unconfirmed".into());
                    }
                };
                if !bridge.terminal || !bridge.dsp_disabled {
                    bridge.events.push("USB restore unconfirmed before DSP stop");
                    return Err("composition fixture did not confirm terminal child and DSP stop; USB restoration unconfirmed".into());
                }
                assert!(!bridge.completion.exists());
                bridge.events.push("USB restore");
                if fail_usb {
                    return Err("explicit USB lease substitute failed".into());
                }
                bridge.usb_restored = true;
                Ok(())
            },
            Some(StopPhase::Stop),
        )
        .await
    }
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(self.root.join(STORE))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }
    fn finish_owner(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.owner.as_ref().unwrap().is_finished() {
            assert!(
                Instant::now() < deadline,
                "owner must finish after actual terminal RELEASE"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        self.owner.take().unwrap().join().unwrap().unwrap();
    }
    fn assert_unsealed(&self) {
        assert!(self.control.local_completion().unwrap().is_none());
        assert_eq!(self.files().len(), 1);
        assert!(completed_store_files(&self.files()).is_err());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Failure cleanup still addresses only the original authenticated
        // owner/socket; no PID-file signal, external process search or kill.
        if let Some(owner) = self.owner.as_ref() {
            let deadline = Instant::now() + Duration::from_secs(3);
            let nonce = "ab".repeat(32);
            while !owner.is_finished() && Instant::now() < deadline {
                {
                    let bridge = self
                        .bridge
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let _ = fs::write(&bridge.exit_gate, b"failure cleanup permitted");
                    let _ = bridge.request(ControlCommand::Stop, &nonce);
                    let _ = bridge.request(ControlCommand::Release, &nonce);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !owner.is_finished() {
                return;
            } // Retain evidence if unknown.
            let _ = self.owner.take().unwrap().join();
        }
        drop(self.lease.take());
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn actual_host_owner_stop_dsp_usb_release_checked_close_then_completion() {
    let mut fixture = Fixture::new(Fault::None);
    assert!(!fixture.stop(false).await.unwrap());
    fixture.assert_unsealed();
    fixture
        .control
        .release(&fixture.plan, &mut || Ok(()))
        .unwrap();
    fixture.finish_owner();
    completed_store_files(&fixture.files()).unwrap();
    let bridge = fixture.bridge.lock().unwrap();
    let position = |event| {
        bridge
            .events
            .iter()
            .position(|value| *value == event)
            .unwrap()
    };
    assert!(position("STOP") < position("STATUS"));
    assert!(position("STATUS") < position("DSP disable"));
    assert!(position("DSP disable") < position("DSP strict readback"));
    assert!(position("DSP strict readback") < position("USB restore"));
    assert!(position("USB restore") < position("RELEASE"));
    assert!(position("RELEASE") < position("RELEASE close"));
    assert!(position("RELEASE close") < position("RELEASE drop"));
    let (_, nonce, actual) = bridge
        .replies
        .iter()
        .find(|(command, _, _)| *command == ControlCommand::Release)
        .unwrap();
    let receipt: Completion =
        serde_json::from_slice(&fs::read(&bridge.completion).unwrap()).unwrap();
    assert_eq!(&receipt.release_nonce, nonce);
    assert_eq!(&receipt.signed_release, actual);
    assert!(receipt.signed_release.ends_with('\n') && !receipt.signed_release.ends_with("\n\n"));
    let response = switch_voice_supervisor::verify_response(
        actual.as_bytes(),
        &bridge.session,
        nonce,
        ControlCommand::Release,
        &fixture.control.key[..],
    )
    .unwrap();
    assert_eq!(response.state(), ControlState::Exited);
    assert_ne!(response.exit(), switch_voice_supervisor::Exit::None);
    let pid = fs::read_to_string(fixture.root.join("child.pid")).unwrap();
    assert!(pid.starts_with(&format!("{} ", response.pid())));
}

#[tokio::test]
async fn actual_owner_exit_cannot_seal_lost_or_nonexact_lf_receipt_or_failed_close() {
    for fault in [
        Fault::LostRelease,
        Fault::MissingProtocolLf,
        Fault::ExtraProtocolLf,
        Fault::Close,
    ] {
        let mut fixture = Fixture::new(fault);
        assert!(!fixture.stop(false).await.unwrap());
        assert!(
            fixture
                .control
                .release(&fixture.plan, &mut || Ok(()))
                .is_err()
        );
        fixture.finish_owner(); // The actual owner exited, yet cleanup is unknown.
        fixture.assert_unsealed();
        let bridge = fixture.bridge.lock().unwrap();
        assert!(bridge.events.contains(&"RELEASE close"));
        assert_eq!(bridge.events.last(), Some(&"RELEASE drop"));
        assert_eq!(
            bridge
                .replies
                .iter()
                .filter(|(command, _, _)| *command == ControlCommand::Release)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn actual_owner_closed_receipt_sync_retry_needs_no_live_supervisor_or_transport() {
    let mut fixture = Fixture::new(Fault::None);
    fixture.stop(false).await.unwrap();
    fixture
        .control
        .completion_fail_before_sync
        .store(true, Ordering::Release);
    assert!(
        fixture
            .control
            .release(&fixture.plan, &mut || Ok(()))
            .is_err()
    );
    fixture.finish_owner();
    assert!(fixture.control.local_completion().unwrap().is_some());
    let before = fixture.files();
    let events = fixture.bridge.lock().unwrap().events.clone();
    // Replacing even the retained plan with an opener that panics proves the
    // sync retry consumes only the already verified local receipt.
    struct NoTransport;
    impl AdbTarget for NoTransport {
        fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
            panic!("local durable completion retry must not open a transport")
        }
    }
    fixture
        .control
        .release(&AdbPlan::from_target(Box::new(NoTransport)), &mut || Ok(()))
        .unwrap();
    assert_eq!(before, fixture.files());
    assert_eq!(events, fixture.bridge.lock().unwrap().events);
    completed_store_files(&fixture.files()).unwrap();
}

#[tokio::test]
async fn actual_release_followed_by_cancel_keeps_completion_unknown_after_checked_close() {
    let mut fixture = Fixture::new(Fault::None);
    fixture.stop(false).await.unwrap();
    let bridge = fixture.bridge.clone();
    let mut current = || {
        if bridge.lock().unwrap().events.contains(&"RELEASE") {
            return Err(ProbeError::new(
                "stale_backend_generation",
                "cancelled after actual host RELEASE",
            ));
        }
        Ok(())
    };
    assert!(
        fixture
            .control
            .release(&fixture.plan, &mut current)
            .is_err()
    );
    fixture.finish_owner();
    fixture.assert_unsealed();
    let bridge = fixture.bridge.lock().unwrap();
    assert!(bridge.events.contains(&"RELEASE close"));
    assert_eq!(bridge.events.last(), Some(&"RELEASE drop"));
}

#[tokio::test]
async fn actual_host_stop_usb_failure_or_cancel_does_not_send_release_or_seal() {
    for fail_usb in [false, true] {
        let mut fixture = Fixture::new(Fault::None);
        let result = fixture.stop(fail_usb).await;
        assert_eq!(result.is_err(), fail_usb);
        if !fail_usb {
            let mut cancelled = || {
                Err(ProbeError::new(
                    "stale_backend_generation",
                    "cancelled host composition operation",
                ))
            };
            assert!(
                fixture
                    .control
                    .release(&fixture.plan, &mut cancelled)
                    .is_err()
            );
        }
        fixture.assert_unsealed();
        assert!(!fixture.owner.as_ref().unwrap().is_finished());
        assert!(!fixture.bridge.lock().unwrap().events.contains(&"RELEASE"));
        // A new confirmed USB restoration and current fence can finish the
        // same original owner, never transfer ownership to a replacement.
        if fail_usb {
            fixture.stop(false).await.unwrap();
        }
        fixture
            .control
            .release(&fixture.plan, &mut || Ok(()))
            .unwrap();
        fixture.finish_owner();
        completed_store_files(&fixture.files()).unwrap();
    }
}
