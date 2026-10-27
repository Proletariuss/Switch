//! Fixed read-only helper/DSP observations on the actual AT-owned connection.
//! No caller supplied shell, ADB authorization, helper execution, signal or
//! stale pidfile removal is exposed by this entry point.
use super::*;
use crate::startup_recovery::legacy_current::ReadScope;
use crate::usb_at_transport::{PhysicalTarget, TransportError};

#[cfg(test)]
mod tests;

pub(crate) struct IdleEvidence {
    boot: String,
}
impl IdleEvidence {
    pub(crate) fn same_boot(&self, other: &Self) -> bool {
        self.boot == other.boot
    }
}
fn pending() -> TransportError {
    TransportError {
        code: "legacy_current_helper_unconfirmed",
        message: "Current module helper state is unconfirmed".into(),
        response: String::new(),
        uncertain: false,
    }
}
fn cleanup() -> TransportError {
    TransportError {
        code: "legacy_current_cleanup_unconfirmed",
        message: "Current module native release is unconfirmed".into(),
        response: String::new(),
        uncertain: true,
    }
}
fn probe(_: ProbeError) -> TransportError {
    pending()
}

// A canonical old route PID/start pair may refer to an absent or reused PID.
// Read it twice, never remove it. alsaucm_test is calibration, not a live route;
// its PID/log/FIFO remain untouched. Two bounded /proc passes independently
// reject every known bridge or supervisor regardless of the old pidfile.
fn no_live_command() -> String {
    let helper = b"mavo-pcm-bridge.armv7"
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let supervisor = b"switch-voice-supervisor.armv7"
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        r#"set -f; LC_ALL=C; export LC_ALL; h() {{ v=$(od -An -v -tx1 -N "$2" "$1" 2>/dev/null) || return 81; printf '%s' "$v" | tr -d ' \n'; }}; stale() {{ f=/run/proletarius-voice-route.pid; if test ! -e "$f" && test ! -L "$f"; then printf 'absent'; return; fi; test -f "$f" && test ! -L "$f" || return 61; raw_hex=$(h "$f" 65) || return 61; test "${{#raw_hex}}" -le 128 || return 61; raw=$(dd if="$f" bs=1 count=65 2>/dev/null) || return 61; test "${{#raw}}" -le 63 || return 61; normalized=$(printf '%s' "$raw" | od -An -v -tx1 | tr -d ' \n'); case "$raw_hex" in "$normalized"|"$normalized"0a) ;; *) return 61;; esac; set -- $raw; test "$#" -eq 2 || return 61; p=$1; t=$2; case "$p:$t" in *[!0-9:]*|:*|*:|0*|*:0*) return 61;; esac; test "${{#p}}" -le 10 && test "${{#t}}" -le 20 && test "$p" -gt 1 && test "$t" != 0 || return 61; test "$raw" = "$p $t" || return 61; if test ! -e "/proc/$p" && test ! -L "/proc/$p"; then h "$f" 65; return; fi; test -d "/proc/$p" && test ! -L "/proc/$p" || return 61; s=$(dd if="/proc/$p/stat" bs=1 count=1537 2>/dev/null) || return 61; test "${{#s}}" -le 1536 || return 61; q=$(dd if="/proc/$p/stat" bs=1 count=1537 2>/dev/null) || return 61; test "$s" = "$q" || return 61; r=${{s##*) }}; test "$r" != "$s" || return 61; set -- $r; test "$#" -ge 20 || return 61; i=1; while test "$i" -lt 20; do shift; i=$((i+1)); done; start=$1; case "$start" in ''|*[!0-9]*) return 61;; esac; test "$start" != "$t" || return 61; a=$(h "/proc/$p/cmdline" 1025) || return 61; test "${{#a}}" -le 2048 || return 61; z=$(h "/proc/$p/cmdline" 1025) || return 61; test "$a" = "$z" || return 61; h "$f" 65; }}; b=$(h /proc/sys/kernel/random/boot_id 38) || exit 64; test "${{#b}}" -eq 74 || exit 64; old_pid=$(stale) || exit 61; test ! -L /tmp/proletarius-call || exit 62; nc=0; set +f; for d in /tmp/proletarius-call/control-*; do nc=$((nc+1)); test "$nc" -le 256 || exit 63; test ! -e "$d" && test ! -L "$d" || exit 63; done; set -f; scan() {{ ids=; n=0; set +f; for d in /proc/[0-9]*; do set -f; test -d "$d" && test ! -L "$d" || exit 65; p=${{d#/proc/}}; case "$p" in ''|*[!0-9]*) exit 66;; esac; test "${{#p}}" -le 10 || exit 67; n=$((n+1)); test "$n" -le 256 || exit 68; a=$(h "$d/cmdline" 1025) || exit 70; test "${{#a}}" -le 2048 || exit 70; c=$(h "$d/cmdline" 1025) || exit 71; test "$a" = "$c" || exit 71; case "$a" in *{helper}*|*{supervisor}*) exit 72;; esac; ids="$ids$p,"; done; set -f; test "$n" -gt 0 || exit 73; true; }}; scan || exit $?; first_ids=$ids; scan || exit $?; test "$first_ids" = "$ids" || exit 74; new_pid=$(stale) || exit 61; test "$old_pid" = "$new_pid" || exit 61; e=$(h /proc/sys/kernel/random/boot_id 38) || exit 75; test "$b" = "$e" || exit 75; printf 'SWNOVOICE1\n%s\n%s\n%s\n' "$b" "$ids" "$e""#
    )
}
fn inspect_protocol(adb: &mut impl ModuleVoiceADB) -> Result<IdleEvidence, TransportError> {
    let (root, status) = adb
        .shell_checked_timeout("id -u", Duration::from_secs(8))
        .map_err(probe)?;
    if status != 0 || startup_recovery::checked_shell_protocol(&root).map_err(probe)? != b"0\n" {
        return Err(pending());
    }
    let (output, status) = adb
        .shell_checked_timeout(&no_live_command(), Duration::from_secs(30))
        .map_err(probe)?;
    if status != 0 {
        return Err(pending());
    }
    let protocol = startup_recovery::checked_shell_protocol(&output).map_err(probe)?;
    let boot = startup_recovery::parse_no_live(protocol).map_err(probe)?;
    startup_recovery::read_stopped_dsp(adb).map_err(probe)?;
    if read_module_boot_id(adb).map_err(probe)? != boot {
        return Err(pending());
    }
    Ok(IdleEvidence { boot })
}
trait IdleIo: BulkIo {
    fn checked_close(&mut self) -> Result<(), TransportError>;
}
fn settle<T>(
    result: Result<T, TransportError>,
    closed: Result<(), TransportError>,
) -> Result<T, TransportError> {
    if closed.as_ref().err().is_some_and(|error| error.uncertain) {
        return Err(closed.err().expect("uncertain close"));
    }
    if result.as_ref().err().is_some_and(|error| error.uncertain) {
        return result;
    }
    closed?;
    result
}
fn inspect_claimed<I: IdleIo>(
    scope: &ReadScope,
    mut io: I,
    prepare: impl FnOnce(&mut I) -> Result<(), TransportError>,
) -> Result<IdleEvidence, TransportError> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scope.check()?;
        prepare(&mut io)?;
        let mut adb = AdbProbe::with_deadline(&mut io, scope.deadline());
        adb.connect().map_err(probe)?;
        let observed = inspect_protocol(&mut adb)?;
        scope.check()?;
        Ok(observed)
    }))
    .unwrap_or_else(|_| Err(cleanup()));
    let closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| io.checked_close()))
        .unwrap_or_else(|_| Err(cleanup()));
    let observed = settle(result, closed)?;
    scope.check()?;
    Ok(observed)
}
#[cfg(test)]
pub(crate) use tests::fixture_idle;

pub(crate) fn inspect(
    scope: &ReadScope,
    target: &PhysicalTarget,
) -> Result<IdleEvidence, TransportError> {
    scope.check()?;
    #[cfg(target_os = "macos")]
    {
        native::inspect(scope, target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = target;
        Err(pending())
    }
}
#[cfg(target_os = "macos")]
mod native {
    use super::*;
    struct Io<'a> {
        handle: Option<rusb::DeviceHandle<rusb::Context>>,
        candidate: AdbInterface,
        claimed: bool,
        scope: &'a ReadScope,
    }
    impl Io<'_> {
        fn timeout(&self, wanted: Duration) -> Result<Duration, ProbeError> {
            self.scope
                .io_fence()
                .map_err(|_| managed_control::failure())?;
            let left = self
                .scope
                .deadline()
                .saturating_duration_since(Instant::now());
            if left < Duration::from_millis(1) || wanted < Duration::from_millis(1) {
                return Err(managed_control::failure());
            }
            Ok(wanted.min(left))
        }
        fn close(&mut self) -> Result<(), TransportError> {
            let outcome = if self.claimed {
                self.handle
                    .as_ref()
                    .ok_or_else(cleanup)?
                    .release_interface(self.candidate.interface)
            } else {
                Ok(())
            };
            self.claimed = false;
            drop(self.handle.take());
            match outcome {
                Ok(()) => Ok(()),
                Err(rusb::Error::NoDevice) => Err(pending()),
                Err(_) => Err(cleanup()),
            }
        }
    }
    impl Drop for Io<'_> {
        fn drop(&mut self) {
            if self.handle.is_some() {
                let _ = self.close();
            }
        }
    }
    impl IdleIo for Io<'_> {
        fn checked_close(&mut self) -> Result<(), TransportError> {
            self.close()
        }
    }
    impl BulkIo for Io<'_> {
        fn read_bulk(&mut self, bytes: &mut [u8], timeout: Duration) -> Result<usize, ProbeError> {
            let timeout = self.timeout(timeout)?;
            let result = self
                .handle
                .as_ref()
                .ok_or_else(managed_control::failure)?
                .read_bulk(self.candidate.endpoint_in, bytes, timeout)
                .map_err(|e| {
                    native_adb_error(e, "legacy_current_adb_read", "Current ADB read failed")
                });
            self.scope
                .io_fence()
                .map_err(|_| managed_control::failure())?;
            result
        }
        fn write_bulk(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, ProbeError> {
            let timeout = self.timeout(timeout)?;
            let result = self
                .handle
                .as_ref()
                .ok_or_else(managed_control::failure)?
                .write_bulk(self.candidate.endpoint_out, bytes, timeout)
                .map_err(|e| {
                    native_adb_error(e, "legacy_current_adb_write", "Current ADB request failed")
                });
            self.scope
                .io_fence()
                .map_err(|_| managed_control::failure())?;
            result
        }
    }
    fn tuple(device: &rusb::Device<rusb::Context>) -> Result<AdbInterface, TransportError> {
        let desc = device.device_descriptor().map_err(|_| pending())?;
        if !matches!(
            (desc.vendor_id(), desc.product_id()),
            (0x2ca3, 0x4006) | (0x2c7c, 0x0125)
        ) {
            return Err(pending());
        }
        let config = device.active_config_descriptor().map_err(|_| pending())?;
        let mut found = Vec::new();
        for interface in config.interfaces() {
            for alt in interface.descriptors() {
                if (alt.class_code(), alt.sub_class_code(), alt.protocol_code()) != (0xff, 0x42, 1)
                {
                    continue;
                }
                let eps = alt.endpoint_descriptors().collect::<Vec<_>>();
                if eps.len() != 2
                    || eps.iter().any(|ep| {
                        ep.transfer_type() != rusb::TransferType::Bulk
                            || ep.address() & 15 == 0
                            || ep.address() & 0x70 != 0
                    })
                {
                    return Err(pending());
                }
                let value = adb_interface(
                    alt.interface_number(),
                    alt.setting_number(),
                    alt.class_code(),
                    alt.sub_class_code(),
                    alt.protocol_code(),
                    eps.iter().map(|ep| (ep.address(), true)),
                )
                .ok_or_else(pending)?;
                if value.endpoint_in & 0x80 == 0 || value.endpoint_out & 0x80 != 0 {
                    return Err(pending());
                }
                found.push(value);
            }
        }
        if found.len() != 1 {
            return Err(pending());
        }
        Ok(found[0])
    }
    pub(super) fn inspect(
        scope: &ReadScope,
        target: &PhysicalTarget,
    ) -> Result<IdleEvidence, TransportError> {
        let device = target.native_device().map_err(|_| pending())?;
        scope.check()?;
        let expected = tuple(&device)?;
        scope.check()?;
        let handle = device.open().map_err(|_| pending())?;
        if handle.device().as_raw() != device.as_raw() {
            return Err(pending());
        }
        let io = Io {
            handle: Some(handle),
            candidate: expected,
            claimed: false,
            scope,
        };
        inspect_claimed(scope, io, |io| {
            scope.check()?;
            let handle = io.handle.as_ref().ok_or_else(pending)?;
            handle
                .claim_interface(expected.interface)
                .map_err(|_| pending())?;
            io.claimed = true;
            let mut alt = [0_u8; 1];
            let n = handle
                .read_control(
                    0x81,
                    10,
                    0,
                    expected.interface.into(),
                    &mut alt,
                    io.timeout(Duration::from_secs(1)).map_err(probe)?,
                )
                .map_err(|_| pending())?;
            if n != 1 || alt[0] != expected.alternate || tuple(&device)? != expected {
                return Err(pending());
            }
            scope.check()
        })
    }
}
