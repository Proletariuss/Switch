use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

const BOOT: &str = "00112233-4455-6677-8899-aabbccddeeff";
fn boot_hex() -> String {
    format!("{BOOT}\n")
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn frame(tag: u32, a: u32, b: u32, payload: &[u8]) -> Vec<u8> {
    let mut wire = encode_header(tag, a, b, payload).unwrap().to_vec();
    wire.extend_from_slice(payload);
    wire
}
struct Inert<'a> {
    scope: Option<&'a ReadScope>,
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
    commands: usize,
    closed: bool,
    close_count: Arc<AtomicUsize>,
    invalid_dsp: bool,
    non_root: bool,
    different_boot: bool,
}
impl Inert<'_> {
    fn fence(&self) -> Result<(), ProbeError> {
        if self.closed {
            return Err(managed_control::failure());
        }
        if let Some(scope) = self.scope {
            scope.io_fence().map_err(|_| managed_control::failure())?;
        }
        Ok(())
    }
}
impl BulkIo for Inert<'_> {
    fn read_bulk(&mut self, output: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
        self.fence()?;
        if self.incoming.is_empty() {
            return Err(managed_control::failure());
        }
        let n = output.len().min(self.incoming.len()).min(7);
        for byte in &mut output[..n] {
            *byte = self.incoming.pop_front().unwrap();
        }
        Ok(n)
    }
    fn write_bulk(&mut self, bytes: &[u8], _: Duration) -> Result<usize, ProbeError> {
        self.fence()?;
        self.outgoing.extend_from_slice(bytes);
        while self.outgoing.len() >= 24 {
            let (tag, local, _, size, _, _) = decode_header(&self.outgoing[..24])?;
            let end = 24 + size as usize;
            if self.outgoing.len() < end {
                break;
            }
            let data = self.outgoing[24..end].to_vec();
            self.outgoing.drain(..end);
            if tag == ADB_CNXN {
                self.incoming.extend(frame(
                    ADB_CNXN,
                    ADB_VERSION,
                    MAX_PAYLOAD as u32,
                    b"device::inert\0",
                ));
            } else if tag == ADB_OPEN {
                let service = std::str::from_utf8(
                    data.strip_suffix(&[0])
                        .ok_or_else(managed_control::failure)?,
                )
                .map_err(|_| managed_control::failure())?;
                let token = service
                    .split("__SWITCH_ADB_STATUS_")
                    .nth(1)
                    .and_then(|s| s.split("_%u__").next())
                    .ok_or_else(managed_control::failure)?;
                let output = match self.commands {
                    0 if service.contains("{ id -u; }") => {
                        if self.non_root {
                            "2000\n".into()
                        } else {
                            "0\n".into()
                        }
                    }
                    1 if service.contains(&no_live_command()) => {
                        let hex = boot_hex();
                        format!("SWNOVOICE1\n{hex}\n1,2,\n{hex}\n")
                    }
                    2 if service.contains("SWDSP1") => {
                        if self.invalid_dsp {
                            "SWDSP1\n310a\n636c6f7365640a\n636c6f7365640a\n".into()
                        } else {
                            "SWDSP1\n300a\n636c6f7365640a\n636c6f7365640a\n".into()
                        }
                    }
                    3 if service.contains("cat /proc/sys/kernel/random/boot_id") => {
                        if self.different_boot {
                            "10112233-4455-6677-8899-aabbccddeeff\n".into()
                        } else {
                            format!("{BOOT}\n")
                        }
                    }
                    _ => return Err(managed_control::failure()),
                };
                self.commands += 1;
                let output = format!("{output}\n__SWITCH_ADB_STATUS_{token}_0__\n");
                self.incoming.extend(frame(ADB_OKAY, 42, local, &[]));
                self.incoming
                    .extend(frame(ADB_WRTE, 42, local, output.as_bytes()));
                self.incoming.extend(frame(ADB_CLSE, 42, local, &[]));
            } else if !matches!(tag, ADB_OKAY | ADB_CLSE) {
                return Err(managed_control::failure());
            }
        }
        Ok(bytes.len())
    }
}
impl IdleIo for Inert<'_> {
    fn checked_close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Err(cleanup());
        }
        self.closed = true;
        self.incoming.clear();
        self.outgoing.clear();
        self.close_count.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}
fn inert(scope: Option<&ReadScope>) -> Inert<'_> {
    Inert {
        scope,
        incoming: VecDeque::new(),
        outgoing: Vec::new(),
        commands: 0,
        closed: false,
        close_count: Arc::new(AtomicUsize::new(0)),
        invalid_dsp: false,
        non_root: false,
        different_boot: false,
    }
}
pub(crate) fn fixture_idle(
    scope: &ReadScope,
    _: &PhysicalTarget,
) -> Result<IdleEvidence, TransportError> {
    let io = inert(Some(scope));
    let closed = io.close_count.clone();
    let result = inspect_claimed(scope, io, |_| Ok(()));
    assert_eq!(closed.load(Ordering::Acquire), 1);
    result
}
#[test]
fn byte_peer_uses_the_real_adb_protocol_and_closed_idle_decoders() {
    for (bad_dsp, changed_boot, non_root) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut io = inert(None);
        io.invalid_dsp = bad_dsp;
        io.different_boot = changed_boot;
        io.non_root = non_root;
        let result = {
            let mut adb =
                AdbProbe::with_deadline(&mut io, Instant::now() + Duration::from_secs(60));
            adb.connect().unwrap();
            inspect_protocol(&mut adb)
        };
        assert_eq!(result.is_ok(), !bad_dsp && !changed_boot && !non_root);
        io.checked_close().unwrap();
        assert!(io.closed);
        assert_eq!(io.close_count.load(Ordering::Acquire), 1);
    }
}

// Execute the exact fixed shell policy against a software-only /proc/sys/run
// tree. Path replacement is private to this test; production accepts no path,
// command or caller-provided process inventory.
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "switch-legacy-proc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        for dir in [
            "proc/sys/kernel/random",
            "proc/1",
            "proc/2",
            "run",
            "tmp/proletarius-call",
        ] {
            std::fs::create_dir_all(path.join(dir)).unwrap();
        }
        std::fs::write(
            path.join("proc/sys/kernel/random/boot_id"),
            format!("{BOOT}\n"),
        )
        .unwrap();
        std::fs::write(path.join("proc/1/cmdline"), b"/sbin/init\0").unwrap();
        std::fs::write(path.join("proc/2/cmdline"), b"/usr/bin/alsaucm_test\0").unwrap();
        Self(path)
    }
    fn run(&self) -> std::process::Output {
        let root = self.0.to_str().unwrap();
        assert!(
            root.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        );
        let command = no_live_command()
            .replace("/proc/", &format!("{root}/proc/"))
            .replace("/run/", &format!("{root}/run/"))
            .replace(
                "/tmp/proletarius-call",
                &format!("{root}/tmp/proletarius-call"),
            );
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .unwrap()
    }
    fn write(&self, path: &str, value: impl AsRef<[u8]>) {
        std::fs::write(self.0.join(path), value).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn real_read_only_shell_accepts_calibration_and_stale_pid_without_removal() {
    let f = Fixture::new();
    let result = f.run();
    assert!(result.status.success(), "{:?}", result.status);
    assert!(startup_recovery::parse_no_live(&result.stdout).is_ok());
    f.write("run/proletarius-voice-route.pid", b"42 123\n");
    assert!(f.run().status.success());
    assert_eq!(
        std::fs::read(f.0.join("run/proletarius-voice-route.pid")).unwrap(),
        b"42 123\n"
    );
    std::fs::create_dir(f.0.join("proc/42")).unwrap();
    f.write("proc/42/cmdline", b"/usr/bin/unrelated\0");
    let fields = std::iter::once("S")
        .chain(std::iter::repeat_n("0", 18))
        .chain(std::iter::once("124"))
        .collect::<Vec<_>>()
        .join(" ");
    f.write("proc/42/stat", format!("42 (unrelated) {fields}\n"));
    assert!(f.run().status.success());
    f.write(
        "proc/42/stat",
        format!("42 (unrelated) {}\n", fields.replace("124", "123")),
    );
    assert!(!f.run().status.success());
}
#[test]
fn real_shell_rejects_live_bridge_supervisor_unknown_sessions_and_bad_pidfiles() {
    for helper in [
        MODULE_VOICE_HELPER,
        "/tmp/proletarius-call/switch-voice-supervisor.armv7",
        "/bin/sh\0-c\0./mavo-pcm-bridge.armv7 --voice-route-session",
        "/bin/sh\0-c\0sleep 1; /tmp/proletarius-call/switch-voice-supervisor.armv7",
    ] {
        let f = Fixture::new();
        f.write(
            "proc/2/cmdline",
            format!("{helper}\0--voice-route-session\0"),
        );
        assert!(!f.run().status.success());
    }
    let f = Fixture::new();
    std::fs::create_dir(f.0.join("tmp/proletarius-call/control-unknown")).unwrap();
    assert!(!f.run().status.success());
    for bytes in [
        b"42".as_slice(),
        b"42 123\n\n",
        b"042 123\n",
        b"42 0\n",
        b"42 /private\n",
        &[b'9'; 65],
    ] {
        let f = Fixture::new();
        f.write("run/proletarius-voice-route.pid", bytes);
        assert!(!f.run().status.success());
    }
    let command = no_live_command();
    assert!(command.len() + 200 < MAX_PAYLOAD);
    for mutation in ["kill ", "rm ", "pkill ", "audio_enable >"] {
        assert!(!command.contains(mutation));
    }
}

#[test]
fn uncertain_probe_survives_definite_disconnect_and_failed_release_wins() {
    assert!(
        settle::<()>(Err(cleanup()), Err(pending()))
            .unwrap_err()
            .uncertain
    );
    assert!(
        settle::<()>(Err(pending()), Err(cleanup()))
            .unwrap_err()
            .uncertain
    );
    assert!(!settle::<()>(Ok(()), Err(pending())).unwrap_err().uncertain);
}
