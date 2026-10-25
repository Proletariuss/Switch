//! Inert byte peer for the actual original-owned startup session executor.
//! All authority, HMAC verification, ledger writes and record pins are real.
use super::*;
use managed_control::startup::owned_fixture::{OriginalSigningPeer, SignedFault};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use switch_voice_supervisor::{Command, Exit, State};

const BOOT: &str = "00112233-4455-6677-8899-aabbccddeeff";
const OTHER_BOOT: &str = "11112233-4455-6677-8899-aabbccddeeff";
const PID: u32 = 321;
const START: u64 = 98765;
const ADB_TUPLE: (u8, u8, u8, u8) = (4, 1, 0x85, 5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedVoiceScenario {
    Running,
    InitiallyExited,
    BadMac,
    BadNonce,
    WrongPid,
    WrongBoot,
    StopAckLost,
    ReleaseAckLost,
    DspActive,
    PlaybackOpen,
    CaptureOpen,
    CloseError,
    CancelBeforeStop,
    LedgerFailure,
}

pub(crate) struct OriginalManagedVoiceFixture {
    peer: OriginalSigningPeer,
    binding: DurableVoiceBinding,
}
impl OriginalManagedVoiceFixture {
    pub(crate) fn create(
        root: &Path,
        proof: &crate::daemon_lifetime::RootProof,
        producer: &str,
        generation: u64,
    ) -> Result<Self, TransportError> {
        let peer = OriginalSigningPeer::create(root, proof, producer, generation).map_err(probe)?;
        // Parse the exact byte peer's original helper observation with the real
        // strict production parser. Durable data alone never mints a scope.
        let helper =
            recovery_binding::parse_startup_helper(helper_wire(BOOT).as_bytes(), ADB_TUPLE)
                .map_err(probe)?;
        let binding = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "producer": producer,
            "owner_instance": "a".repeat(64), "registry_revision": 1,
            "observation": {"kind": "owned", "runtime_generation": generation,
                            "helper": helper},
            "managed_control": peer.binding(),
        }))
        .map_err(|_| pending())?;
        Ok(Self { peer, binding })
    }
    pub(crate) fn binding(&self) -> DurableVoiceBinding {
        self.binding.clone()
    }
}

#[derive(Default)]
struct Arrivals {
    status: AtomicUsize,
    stop: AtomicUsize,
    release: AtomicUsize,
    identity: AtomicUsize,
    dsp_cleanup: AtomicUsize,
    dsp_read: AtomicUsize,
    boot_read: AtomicUsize,
    adb_close: AtomicUsize,
    pin_release: AtomicUsize,
    hook: AtomicUsize,
}
#[derive(Clone, Default)]
pub(crate) struct ManagedVoiceArrivalProbe(Arc<Arrivals>);
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ManagedVoiceArrivalSnapshot {
    pub(crate) status: usize,
    pub(crate) stop: usize,
    pub(crate) release: usize,
    pub(crate) identity: usize,
    pub(crate) dsp_cleanup: usize,
    pub(crate) dsp_read: usize,
    pub(crate) boot_read: usize,
    pub(crate) adb_close: usize,
    pub(crate) pin_release: usize,
    pub(crate) hook: usize,
}
impl ManagedVoiceArrivalProbe {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn snapshot(&self) -> ManagedVoiceArrivalSnapshot {
        let a = &self.0;
        ManagedVoiceArrivalSnapshot {
            status: a.status.load(Ordering::Relaxed),
            stop: a.stop.load(Ordering::Relaxed),
            release: a.release.load(Ordering::Relaxed),
            identity: a.identity.load(Ordering::Relaxed),
            dsp_cleanup: a.dsp_cleanup.load(Ordering::Relaxed),
            dsp_read: a.dsp_read.load(Ordering::Relaxed),
            boot_read: a.boot_read.load(Ordering::Relaxed),
            adb_close: a.adb_close.load(Ordering::Relaxed),
            pin_release: a.pin_release.load(Ordering::Relaxed),
            hook: a.hook.load(Ordering::Relaxed),
        }
    }
    pub(super) fn pin_released(&self) {
        self.0.pin_release.fetch_add(1, Ordering::Relaxed);
    }
    /// Must run after the worker join, outside all production catch_unwind.
    pub(crate) fn assert_reached(&self, scenario: ManagedVoiceScenario) {
        use ManagedVoiceScenario::*;
        let (status, stop, release, identity, dsp_cleanup, dsp_read, boot_read) = match scenario {
            BadMac | BadNonce | WrongPid => (1, 0, 0, 0, 0, 0, 0),
            WrongBoot => (1, 0, 0, 1, 0, 0, 0),
            StopAckLost => (1, 1, 0, 1, 0, 0, 0),
            DspActive | PlaybackOpen | CaptureOpen => (2, 1, 0, 1, 1, 1, 0),
            ReleaseAckLost => (2, 1, 1, 1, 1, 1, 0),
            InitiallyExited => (1, 1, 1, 1, 1, 1, 1),
            Running | CloseError | CancelBeforeStop | LedgerFailure => (2, 1, 1, 1, 1, 1, 1),
        };
        assert_eq!(
            self.snapshot(),
            ManagedVoiceArrivalSnapshot {
                status,
                stop,
                release,
                identity,
                dsp_cleanup,
                dsp_read,
                boot_read,
                adb_close: 1,
                pin_release: 1,
                hook: usize::from(matches!(scenario, CancelBeforeStop | LedgerFailure)),
            },
            "original-owned voice scenario did not reach its actual byte/close/pin boundaries"
        );
    }
}
fn encoded(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn helper_wire(boot: &str) -> String {
    let mut stat = format!("{PID} (helper ) with spaces) S");
    for _ in 0..18 {
        stat.push_str(" 0");
    }
    stat.push_str(&format!(" {START} 0\n"));
    let argv = format!("{MODULE_VOICE_HELPER}\0--voice-route-session\0--verbose\0");
    let boot = encoded(format!("{boot}\n").as_bytes());
    let pid = encoded(format!("{PID} {START}\n").as_bytes());
    let stat = encoded(stat.as_bytes());
    let argv = encoded(argv.as_bytes());
    format!(
        "SWVOICE1\n{boot}\n{pid}\n{stat}\n{argv}\n310a\n1\n1\n1\n{stat}\n{argv}\n{pid}\n{boot}\n"
    )
}
fn frame(tag: u32, a: u32, b: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [
        tag,
        a,
        b,
        payload.len() as u32,
        payload.iter().map(|v| *v as u32).sum(),
        tag ^ u32::MAX,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(payload);
    out
}
struct OwnedByteIo<'a, 'scope, 'hook> {
    scope: &'a ReconciliationScope<'scope>,
    fixture: &'a OriginalManagedVoiceFixture,
    scenario: ManagedVoiceScenario,
    probe: ManagedVoiceArrivalProbe,
    cleanup: Option<crate::module_radio::CallAdmission>,
    hook: Option<&'hook mut dyn FnMut()>,
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
    stopped: bool,
    closed: bool,
}
impl OwnedByteIo<'_, '_, '_> {
    fn check(&self) -> Result<(), ProbeError> {
        match &self.cleanup {
            Some(admission) if admission() => {}
            Some(_) => return Err(managed_control::failure()),
            None => self.scope.check().map_err(|_| managed_control::failure())?,
        }
        if Instant::now() >= self.scope.deadline() {
            return Err(managed_control::failure());
        }
        Ok(())
    }
    fn output(&mut self, command: &str) -> Result<String, ProbeError> {
        use ManagedVoiceScenario::*;
        let a = &self.probe.0;
        if command.contains(" client --socket ") {
            let nonce = command
                .split(" --nonce '")
                .nth(1)
                .and_then(|s| s.split('\'').next())
                .ok_or_else(managed_control::failure)?;
            let word = command
                .rsplit(" --command ")
                .next()
                .ok_or_else(managed_control::failure)?;
            if !self.fixture.peer.matches_command(command, nonce, word) {
                return Err(managed_control::failure());
            }
            let (operation, state) = match word {
                "status" => {
                    a.status.fetch_add(1, Ordering::Relaxed);
                    (
                        Command::Status,
                        if self.stopped || self.scenario == InitiallyExited {
                            State::Exited
                        } else {
                            State::Running
                        },
                    )
                }
                "stop" => {
                    a.stop.fetch_add(1, Ordering::Relaxed);
                    if matches!(self.scenario, CancelBeforeStop | LedgerFailure) {
                        a.hook.fetch_add(1, Ordering::Relaxed);
                        let hook = self.hook.as_mut().ok_or_else(managed_control::failure)?;
                        hook();
                    }
                    self.stopped = true;
                    if self.scenario == StopAckLost {
                        return Err(managed_control::failure());
                    }
                    (
                        Command::Stop,
                        if self.scenario == InitiallyExited {
                            State::Exited
                        } else {
                            State::Stopping
                        },
                    )
                }
                "release" => {
                    a.release.fetch_add(1, Ordering::Relaxed);
                    if self.scenario == ReleaseAckLost {
                        return Err(managed_control::failure());
                    }
                    (Command::Release, State::Exited)
                }
                _ => return Err(managed_control::failure()),
            };
            let pid = if self.scenario == WrongPid {
                PID + 1
            } else {
                PID
            };
            let exit = if state == State::Exited {
                Exit::Code(0)
            } else {
                Exit::None
            };
            let fault = match self.scenario {
                BadMac => SignedFault::BadMac,
                BadNonce => SignedFault::BadNonce,
                _ => SignedFault::None,
            };
            return self
                .fixture
                .peer
                .response(operation, nonce, state, pid, exit, fault);
        }
        if command == recovery_binding::startup_read_command(&self.fixture.peer.pid_path())? {
            a.identity.fetch_add(1, Ordering::Relaxed);
            return Ok(helper_wire(if self.scenario == WrongBoot {
                OTHER_BOOT
            } else {
                BOOT
            }));
        }
        if command == exited_identity_command(&self.fixture.peer.pid_path(), PID)? {
            a.identity.fetch_add(1, Ordering::Relaxed);
            let boot = encoded(format!("{BOOT}\n").as_bytes());
            let pid = encoded(format!("{PID} {START}\n").as_bytes());
            return Ok(format!("SWVOICEEXIT1\n{boot}\n{pid}\n"));
        }
        // Match the actual fixed T/T/B+UAC cleanup command in full, not a
        // substring that could accept a different write command.
        let cleanup = "echo 0 > /sys/class/android_usb/f_audio/audio_enable || exit $?; if test -p /run/voc_svr; then printf 'T\\n' > /run/voc_svr && printf 'T\\n' > /run/voc_svr && printf 'B\\n' > /run/voc_svr || exit $?; fi; test \"$(cat /sys/class/android_usb/f_audio/audio_enable)\" = 0";
        if command == cleanup {
            a.dsp_cleanup.fetch_add(1, Ordering::Relaxed);
            return Ok(String::new());
        }
        if command == READ_STOPPED_DSP {
            a.dsp_read.fetch_add(1, Ordering::Relaxed);
            let audio = if self.scenario == DspActive {
                "310a"
            } else {
                "300a"
            };
            let playback = if self.scenario == PlaybackOpen {
                "73746174653a2052554e4e494e470a"
            } else {
                "636c6f7365640a"
            };
            let capture = if self.scenario == CaptureOpen {
                "73746174653a2052554e4e494e470a"
            } else {
                "636c6f7365640a"
            };
            return Ok(format!("SWDSP1\n{audio}\n{playback}\n{capture}\n"));
        }
        if command == "cat /proc/sys/kernel/random/boot_id" {
            a.boot_read.fetch_add(1, Ordering::Relaxed);
            return Ok(format!("{BOOT}\n"));
        }
        Err(managed_control::failure())
    }
}
impl BulkIo for OwnedByteIo<'_, '_, '_> {
    fn read_bulk(&mut self, bytes: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
        self.check()?;
        if self.incoming.is_empty() {
            return Err(managed_control::failure());
        }
        // This fixture proves the complete owned-voice recovery and durable
        // publication path. Dedicated ADB composition/parser fixtures retain
        // the 7-byte and smaller fragmented-read coverage; imposing it here
        // multiplies full evidence-tree validation for every frame fragment
        // and can exhaust the finite reconciliation budget under parallel
        // process-matrix load before the first managed cleanup is prepared.
        let n = bytes.len().min(self.incoming.len());
        for b in &mut bytes[..n] {
            *b = self.incoming.pop_front().unwrap();
        }
        self.check()?;
        Ok(n)
    }
    fn write_bulk(&mut self, bytes: &[u8], _: Duration) -> Result<usize, ProbeError> {
        self.check()?;
        self.outgoing.extend_from_slice(bytes);
        while self.outgoing.len() >= 24 {
            let (tag, local, _, size, _, _) = decode_header(&self.outgoing[..24])?;
            let end = 24 + size as usize;
            if self.outgoing.len() < end {
                break;
            }
            let payload = self.outgoing[24..end].to_vec();
            self.outgoing.drain(..end);
            match tag {
                ADB_CNXN => self.incoming.extend(frame(
                    ADB_CNXN,
                    ADB_VERSION,
                    MAX_PAYLOAD as u32,
                    b"device::owned-inert\0",
                )),
                ADB_OPEN => {
                    let service = std::str::from_utf8(
                        payload
                            .strip_suffix(&[0])
                            .ok_or_else(managed_control::failure)?,
                    )
                    .map_err(|_| managed_control::failure())?;
                    let token = service
                        .split("__SWITCH_ADB_STATUS_")
                        .nth(1)
                        .and_then(|s| s.split("_%u__").next())
                        .ok_or_else(managed_control::failure)?;
                    let tail = format!(
                        "; }}; __switch_status=$?; printf '\\n__SWITCH_ADB_STATUS_{token}_%u__\\n' \"$__switch_status\""
                    );
                    let command = service
                        .strip_prefix("shell:{ ")
                        .and_then(|s| s.strip_suffix(&tail))
                        .ok_or_else(managed_control::failure)?;
                    let output = self.output(command)?;
                    let output = format!("{output}\n__SWITCH_ADB_STATUS_{token}_0__\n");
                    self.incoming.extend(frame(ADB_OKAY, 42, local, &[]));
                    self.incoming
                        .extend(frame(ADB_WRTE, 42, local, output.as_bytes()));
                    self.incoming.extend(frame(ADB_CLSE, 42, local, &[]));
                }
                ADB_OKAY | ADB_CLSE => {}
                _ => return Err(managed_control::failure()),
            }
        }
        self.check()?;
        Ok(bytes.len())
    }
}
impl SettlementIo for OwnedByteIo<'_, '_, '_> {
    fn set_cleanup(&mut self, admission: Option<crate::module_radio::CallAdmission>) {
        self.cleanup = admission;
    }
    fn checked_close(mut self) -> Result<(), TransportError> {
        if self.closed {
            return Err(cleanup_failed());
        }
        self.closed = true;
        self.probe.0.adb_close.fetch_add(1, Ordering::Relaxed);
        self.incoming.clear();
        self.outgoing.clear();
        if self.scenario == ManagedVoiceScenario::CloseError {
            Err(cleanup_failed())
        } else {
            Ok(())
        }
    }
    fn arrival(&self) -> Option<ManagedVoiceArrivalProbe> {
        Some(self.probe.clone())
    }
}
pub(crate) fn scripted_owned_settle_for_native(
    scope: &ReconciliationScope<'_>,
    fixture: &OriginalManagedVoiceFixture,
    scenario: ManagedVoiceScenario,
    arrivals: &ManagedVoiceArrivalProbe,
    hook: Option<&mut dyn FnMut()>,
) -> Result<VoiceSettledProof, TransportError> {
    scope.check()?;
    if scope.original_voice_binding() != Some(&fixture.binding) {
        return Err(pending());
    }
    let control = capture_original_control(scope)?;
    let io = OwnedByteIo {
        scope,
        fixture,
        scenario,
        probe: arrivals.clone(),
        cleanup: None,
        hook,
        incoming: VecDeque::new(),
        outgoing: Vec::new(),
        stopped: false,
        closed: false,
    };
    settle_claimed(scope, io, control, |_| scope.check())
}
