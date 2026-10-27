//! Finite startup settlement on the exact Device retained by the fresh AT owner.
//! New observations do not rewrite an original unknown STOP or RELEASE result.
use super::*;
use crate::startup_recovery::reconcile::{
    ReconciliationMutation, ReconciliationScope, ReconciliationSeal, SubmissionOutcome,
};
use crate::usb_at_transport::{PhysicalTarget, TransportError};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VoiceSettlementEvidence {
    policy: String,
    kind: String,
    original_manifest_sha256: String,
    original_record_sha256: Option<String>,
    fresh_boot_sha256: String,
    child_pid: Option<u32>,
    child_start_ticks: Option<u64>,
    authenticated_responses: Vec<managed_control::startup::AuthenticatedControlEvidence>,
    actual_child_exit: bool,
    audio_disabled: bool,
    playback_closed: bool,
    capture_closed: bool,
    actual_adb_closed: bool,
    original_record_pin_released: Option<bool>,
    bounded_proc_scan: bool,
    dsp_wire: String,
    no_live_wire: Option<String>,
    successor: Option<Box<VoiceSettlementEvidence>>,
}
pub(crate) struct VoiceSettledProof {
    seal: ReconciliationSeal,
    evidence: VoiceSettlementEvidence,
}
impl VoiceSettledProof {
    pub(crate) fn seal(&self) -> &ReconciliationSeal {
        &self.seal
    }
    pub(crate) fn validate_for(
        &self,
        scope: &ReconciliationScope<'_>,
    ) -> Result<(), TransportError> {
        if !self.seal.matches(scope)
            || !self.evidence.actual_adb_closed
            || self.evidence.original_record_pin_released == Some(false)
        {
            return Err(pending());
        }
        if self.evidence.successor.as_ref().is_some_and(|e| {
            !e.actual_adb_closed
                || e.kind != "current_no_live_observation"
                || e.successor.is_some()
                || e.original_manifest_sha256 != self.evidence.original_manifest_sha256
                || !e.audio_disabled
                || !e.playback_closed
                || !e.capture_closed
        }) {
            return Err(pending());
        }
        scope.check()
    }
    pub(crate) fn evidence(&self) -> &VoiceSettlementEvidence {
        &self.evidence
    }
    pub(crate) fn has_successor_observation(&self) -> bool {
        self.evidence.successor.is_some()
    }
}
fn pending() -> TransportError {
    TransportError {
        code: "startup_voice_settlement_pending",
        message: "Fresh exact module voice settlement is unconfirmed".into(),
        response: String::new(),
        uncertain: false,
    }
}
fn cleanup_failed() -> TransportError {
    TransportError {
        code: "startup_voice_cleanup_unconfirmed",
        message: "Original startup voice resources have not confirmed close".into(),
        response: String::new(),
        uncertain: true,
    }
}
fn probe(_: ProbeError) -> TransportError {
    pending()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
const READ_STOPPED_DSP: &str = r#"set -f; h() { v=$(od -An -v -tx1 -N "$2" "$1" 2>/dev/null) || return 81; printf '%s' "$v" | tr -d ' \n'; }; a=$(h /sys/class/android_usb/f_audio/audio_enable 3) || exit 81; p=$(h /proc/asound/card0/pcm4p/sub0/status 8) || exit 82; c=$(h /proc/asound/card0/pcm4c/sub0/status 8) || exit 83; printf 'SWDSP1\n%s\n%s\n%s\n' "$a" "$p" "$c""#;
/// AdbProbe has already verified the random status marker and decoded
/// its exit code. Its output deliberately retains the wrapper's separator LF.
/// Consume exactly that LF; protocol terminators remain strict decoder input.
pub(super) fn checked_shell_protocol(output: &[u8]) -> Result<&[u8], ProbeError> {
    output
        .strip_suffix(b"\n")
        .ok_or_else(managed_control::failure)
}
pub(super) fn read_stopped_dsp(adb: &mut impl ModuleVoiceADB) -> Result<String, ProbeError> {
    let (output, status) = adb.shell_checked_timeout(READ_STOPPED_DSP, Duration::from_secs(8))?;
    if status != 0 {
        return Err(managed_control::failure());
    }
    let protocol = checked_shell_protocol(&output)?;
    if !parse_stopped_dsp(protocol) {
        return Err(managed_control::failure());
    }
    String::from_utf8(protocol.to_vec()).map_err(|_| managed_control::failure())
}
fn parse_stopped_dsp(output: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(output) else {
        return false;
    };
    let Some(text) = text.strip_suffix('\n') else {
        return false;
    };
    let lines = text.split('\n').collect::<Vec<_>>();
    lines.len() == 4
        && lines[0] == "SWDSP1"
        && matches!(lines[1], "30" | "300a")
        && matches!(lines[2], "636c6f736564" | "636c6f7365640a")
        && matches!(lines[3], "636c6f736564" | "636c6f7365640a")
}

fn inspect_no_live(
    scope: &ReconciliationScope<'_>,
    adb: &mut impl ModuleVoiceADB,
    allowed_sessions: &[String],
) -> Result<VoiceSettlementEvidence, TransportError> {
    scope.check()?;
    let (output, status) = adb
        .shell_checked_timeout(&no_live_command(allowed_sessions), Duration::from_secs(30))
        .map_err(probe)?;
    if status != 0 {
        return Err(pending());
    }
    let protocol = checked_shell_protocol(&output).map_err(probe)?;
    let boot = parse_no_live(protocol).map_err(probe)?;
    let dsp_wire = read_stopped_dsp(adb).map_err(probe)?;
    if read_module_boot_id(adb).map_err(probe)? != boot {
        return Err(pending());
    }
    scope.check()?;
    Ok(VoiceSettlementEvidence {
        policy: "startup-voice-v1".into(),
        kind: "current_no_live_observation".into(),
        original_manifest_sha256: scope.original_manifest_sha256().into(),
        original_record_sha256: None,
        fresh_boot_sha256: digest(boot.as_bytes()),
        child_pid: None,
        child_start_ticks: None,
        authenticated_responses: Vec::new(),
        actual_child_exit: false,
        audio_disabled: true,
        playback_closed: true,
        capture_closed: true,
        actual_adb_closed: false,
        original_record_pin_released: None,
        bounded_proc_scan: true,
        dsp_wire,
        no_live_wire: Some(String::from_utf8(protocol.to_vec()).map_err(|_| pending())?),
        successor: None,
    })
}

pub(crate) fn settle_original_voice(
    scope: &ReconciliationScope<'_>,
    target: &PhysicalTarget,
) -> Result<VoiceSettledProof, TransportError> {
    scope.check()?;
    #[cfg(target_os = "macos")]
    {
        native::settle(scope, target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = target;
        Err(pending())
    }
}

pub(crate) fn verify_successor_voice(
    scope: &ReconciliationScope<'_>,
    target: &PhysicalTarget,
    mut prior: VoiceSettledProof,
) -> Result<VoiceSettledProof, TransportError> {
    prior.validate_for(scope)?;
    if prior.has_successor_observation() {
        return Err(pending());
    }
    let binding = if prior.evidence.kind == "original_managed_settled" {
        scope
            .original_voice_binding()
            .and_then(|b| b.startup_owned())
            .map(|(_, c)| c)
    } else {
        None
    };
    let allowed = managed_control::startup::sessions_for_settlement(
        scope.root(),
        scope.root_proof(),
        binding,
    )
    .map_err(probe)?;
    #[cfg(target_os = "macos")]
    {
        let successor = native::inspect_successor(scope, target, &allowed)?;
        prior.evidence.successor = Some(Box::new(successor));
        prior.validate_for(scope)?;
        Ok(prior)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (target, allowed);
        Err(pending())
    }
}

// Two complete bounded passes over numeric /proc entries. Every cmdline is
// read twice with limit+1, every PID set is stable, and boot is read twice.
// A known helper OR supervisor anywhere in the scan is pending. This never
// sends STOP, kills a PID, authorizes ADB or treats a stale pidfile as clean.
fn no_live_command(allowed_sessions: &[String]) -> String {
    let allowed = if allowed_sessions.is_empty() {
        String::new()
    } else {
        let sessions = allowed_sessions.join(" ");
        format!(
            "case ' {sessions} ' in *\" ${{d##*/control-}} \"*) test -d \"$d\" && test ! -L \"$d\" || exit 63; continue;; esac;"
        )
    };
    let helper = MODULE_VOICE_HELPER
        .as_bytes()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    let supervisor = b"/tmp/proletarius-call/switch-voice-supervisor.armv7"
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    format!(
        r#"set -f; LC_ALL=C; export LC_ALL; h() {{ v=$(od -An -v -tx1 -N "$2" "$1" 2>/dev/null) || return 81; printf '%s' "$v" | tr -d ' \n'; }}; test ! -e /run/proletarius-voice-route.pid && test ! -L /run/proletarius-voice-route.pid || exit 61; test ! -L /tmp/proletarius-call || exit 62; nc=0; set +f; for d in /tmp/proletarius-call/control-*; do nc=$((nc+1)); test "$nc" -le 256 || exit 63; {allowed} test ! -e "$d" && test ! -L "$d" || exit 63; done; set -f; b=$(h /proc/sys/kernel/random/boot_id 38) || exit 64; test "${{#b}}" -eq 74 || exit 64; scan() {{ ids=; n=0; set +f; for d in /proc/[0-9]*; do set -f; test -d "$d" && test ! -L "$d" || exit 65; p=${{d#/proc/}}; case "$p" in ''|*[!0-9]*) exit 66;; esac; test "${{#p}}" -le 10 || exit 67; n=$((n+1)); test "$n" -le 256 || exit 68; test -r "$d/cmdline" || exit 69; a=$(h "$d/cmdline" 1025) || exit 70; test "${{#a}}" -le 2048 || exit 70; c=$(h "$d/cmdline" 1025) || exit 71; test "$a" = "$c" || exit 71; case "$a" in {helper}00*|{supervisor}00*) exit 72;; esac; ids="$ids$p,"; done; set -f; test "$n" -gt 0 || exit 73; true; }}; scan || exit $?; first_ids=$ids; scan || exit $?; test "$first_ids" = "$ids" || exit 74; e=$(h /proc/sys/kernel/random/boot_id 38) || exit 75; test "$b" = "$e" || exit 75; printf 'SWNOVOICE1\n%s\n%s\n%s\n' "$b" "$ids" "$e""#
    )
}
pub(super) fn parse_no_live(output: &[u8]) -> Result<String, ProbeError> {
    let bad = || {
        ProbeError::new(
            "startup_voice_no_live_unconfirmed",
            "Module process inventory is incomplete",
        )
    };
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(bad());
    }
    let text = std::str::from_utf8(output).map_err(|_| bad())?;
    let lines: Vec<_> = text
        .strip_suffix('\n')
        .ok_or_else(bad)?
        .split('\n')
        .collect();
    if lines.len() != 4 || lines[0] != "SWNOVOICE1" || lines[1] != lines[3] || lines[1].len() != 74
    {
        return Err(bad());
    }
    let bytes = lines[1]
        .as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let s = std::str::from_utf8(p).map_err(|_| bad())?;
            u8::from_str_radix(s, 16).map_err(|_| bad())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let boot = std::str::from_utf8(&bytes)
        .map_err(|_| bad())?
        .strip_suffix('\n')
        .ok_or_else(bad)?;
    if !valid_module_boot_id(boot) {
        return Err(bad());
    }
    let pids = lines[2]
        .strip_suffix(',')
        .ok_or_else(bad)?
        .split(',')
        .collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    if pids.is_empty() || pids.len() > 256 {
        return Err(bad());
    }
    for pid in pids {
        let value = pid.parse::<u32>().map_err(|_| bad())?;
        if value == 0
            || pid.len() > 10
            || !pid.bytes().all(|b| b.is_ascii_digit())
            || !seen.insert(value)
        {
            return Err(bad());
        }
    }
    Ok(boot.to_owned())
}

fn exited_identity_command(pid_path: &str, pid: u32) -> Result<String, ProbeError> {
    if !managed_control::valid_pid_path(pid_path) || pid < 2 {
        return Err(managed_control::failure());
    }
    Ok(format!(
        r#"set -f; h() {{ v=$(od -An -v -tx1 -N "$2" "$1" 2>/dev/null) || return 81; printf '%s' "$v" | tr -d ' \n'; }}; b=$(h /proc/sys/kernel/random/boot_id 38) || exit 81; p=$(h '{pid_path}' 65) || exit 82; test ! -e '/proc/{pid}' && test ! -L '/proc/{pid}' || exit 83; c=$(h /proc/sys/kernel/random/boot_id 38) || exit 84; q=$(h '{pid_path}' 65) || exit 85; test "$b" = "$c" && test "$p" = "$q" || exit 86; test ! -e '/proc/{pid}' && test ! -L '/proc/{pid}' || exit 87; printf 'SWVOICEEXIT1\n%s\n%s\n' "$b" "$p""#
    ))
}
fn verify_exited_identity(
    output: &[u8],
    original: &recovery_binding::NativeHelperObservation,
) -> Result<(), ProbeError> {
    let text = std::str::from_utf8(output).map_err(|_| managed_control::failure())?;
    let encode = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let boot = encode(format!("{}\n", original.startup_boot()).as_bytes());
    let pidfile = encode(
        format!(
            "{} {}\n",
            original.startup_pid(),
            original.startup_start_ticks()
        )
        .as_bytes(),
    );
    let expected = format!("SWVOICEEXIT1\n{boot}\n{pidfile}\n");
    if output.len() > 256 || text != expected {
        return Err(managed_control::failure());
    }
    Ok(())
}

/// Durable transcript checking only. The coordinator separately verifies the
/// exact retired-placement FD/inode/raw manifest and original journal chain.
/// This function cannot create a live scope or a new VoiceSettledProof.
pub(crate) fn verify_settlement_evidence(
    value: &serde_json::Value,
    archived_control: Option<&std::fs::File>,
    original_voice: Option<&DurableVoiceBinding>,
) -> std::io::Result<()> {
    let invalid = || std::io::Error::other("startup voice settlement evidence is invalid");
    let e: VoiceSettlementEvidence =
        serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    if e.policy != "startup-voice-v1"
        || !e.actual_adb_closed
        || !e.audio_disabled
        || !e.playback_closed
        || !e.capture_closed
        || e.original_manifest_sha256.len() != 64
        || !parse_stopped_dsp(e.dsp_wire.as_bytes())
    {
        return Err(invalid());
    }
    if let Some(successor) = &e.successor {
        if successor.kind != "current_no_live_observation"
            || successor.successor.is_some()
            || successor.original_manifest_sha256 != e.original_manifest_sha256
        {
            return Err(invalid());
        }
        verify_settlement_evidence(
            &serde_json::to_value(successor).map_err(|_| invalid())?,
            None,
            None,
        )?;
    }
    match e.kind.as_str() {
        "original_managed_settled" => {
            let (helper, binding) = original_voice
                .and_then(|b| b.startup_owned())
                .ok_or_else(invalid)?;
            if e.child_pid != Some(helper.startup_pid())
                || e.child_start_ticks != Some(helper.startup_start_ticks())
                || e.fresh_boot_sha256 != digest(helper.startup_boot().as_bytes())
                || !e.actual_child_exit
                || e.bounded_proc_scan
                || e.no_live_wire.is_some()
                || e.original_record_pin_released != Some(true)
            {
                return Err(invalid());
            }
            let dir = archived_control.ok_or_else(invalid)?;
            if e.original_record_sha256.as_deref() != Some(binding.startup_record_sha256()) {
                return Err(invalid());
            }
            managed_control::startup::verify_archived_transcript(
                dir,
                binding,
                helper.startup_pid(),
                &e.authenticated_responses,
            )
            .map_err(|_| invalid())
        }
        "current_no_live_observation" => {
            if e.original_record_sha256.is_some()
                || e.child_pid.is_some()
                || e.child_start_ticks.is_some()
                || !e.authenticated_responses.is_empty()
                || e.actual_child_exit
                || !e.bounded_proc_scan
                || e.original_record_pin_released.is_some()
            {
                return Err(invalid());
            }
            let wire = e.no_live_wire.as_ref().ok_or_else(invalid)?;
            if wire.len() > MAX_OUTPUT_BYTES {
                return Err(invalid());
            }
            let boot = parse_no_live(wire.as_bytes()).map_err(|_| invalid())?;
            if e.fresh_boot_sha256 != digest(boot.as_bytes()) {
                return Err(invalid());
            }
            Ok(())
        }
        _ => Err(invalid()),
    }
}

// No public backend selector: only the actual claimed native handle and the
// private cfg(test) byte peer implement this consuming resource boundary.
trait SettlementIo: BulkIo + Sized {
    fn set_cleanup(&mut self, admission: Option<crate::module_radio::CallAdmission>);
    fn checked_close(self) -> Result<(), TransportError>;
    #[cfg(test)]
    fn arrival(&self) -> Option<ManagedVoiceArrivalProbe> {
        None
    }
}
fn capture_original_control(
    scope: &ReconciliationScope<'_>,
) -> Result<Option<managed_control::startup::StartupControl>, TransportError> {
    let owned = scope
        .original_voice_binding()
        .and_then(|b| b.startup_owned());
    let control = owned
        .map(|(_, binding)| {
            managed_control::startup::StartupControl::capture(
                scope.root(),
                scope.root_proof(),
                binding,
            )
        })
        .transpose()
        .map_err(probe)?;
    if owned.is_none() {
        managed_control::startup::require_closed_inventory(scope.root(), scope.root_proof())
            .map_err(probe)?;
    }
    Ok(control)
}
fn settle_claimed<I: SettlementIo>(
    scope: &ReconciliationScope<'_>,
    mut io: I,
    control: Option<managed_control::startup::StartupControl>,
    configure_exact_interface: impl FnOnce(&mut I) -> Result<(), TransportError>,
) -> Result<VoiceSettledProof, TransportError> {
    use switch_voice_supervisor::{Command as ControlCommand, Exit, State as ControlState};
    let owned = scope
        .original_voice_binding()
        .and_then(|b| b.startup_owned());
    #[cfg(test)]
    let observer = io.arrival();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<VoiceSettlementEvidence, TransportError> {
            configure_exact_interface(&mut io)?;
            if let (Some((original, _)), Some(control)) = (owned, control.as_ref()) {
                let mut adb = AdbProbe::with_deadline(&mut io, scope.deadline());
                adb.connect().map_err(probe)?;
                let (mut status, status_hash) = control
                    .request(&mut adb, ControlCommand::Status)
                    .map_err(probe)?;
                let mut prior_responses = vec![status_hash];
                let wait_end =
                    (Instant::now() + MODULE_VOICE_ROUTE_STOP_TIMEOUT).min(scope.deadline());
                while status.state() == ControlState::Stopping {
                    if status.pid() != original.startup_pid() || Instant::now() >= wait_end {
                        return Err(pending());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    let (next, hash) = control
                        .request(&mut adb, ControlCommand::Status)
                        .map_err(probe)?;
                    status = next;
                    prior_responses[0] = hash;
                }
                if status.pid() != original.startup_pid() {
                    return Err(pending());
                }
                if status.state() == ControlState::Running {
                    let cmd = recovery_binding::startup_read_command(&control.pid_path())
                        .map_err(probe)?;
                    let (output, rc) = adb
                        .shell_checked_timeout(&cmd, Duration::from_secs(15))
                        .map_err(probe)?;
                    if rc != 0
                        || !original.same_startup_identity(
                            &recovery_binding::parse_startup_helper(
                                checked_shell_protocol(&output).map_err(probe)?,
                                original.startup_tuple(),
                            )
                            .map_err(probe)?,
                        )
                    {
                        return Err(pending());
                    }
                } else if status.state() == ControlState::Exited
                    && !matches!(status.exit(), Exit::None)
                {
                    // A signed original wait result plus stable original boot,
                    // exact diagnostic PID/start and absence of that process.
                    // Absence alone never substitutes for the authenticated ACK.
                    let (output, rc) = adb
                        .shell_checked_timeout(
                            &exited_identity_command(&control.pid_path(), original.startup_pid())
                                .map_err(probe)?,
                            Duration::from_secs(8),
                        )
                        .map_err(probe)?;
                    if rc != 0 {
                        return Err(pending());
                    }
                    verify_exited_identity(
                        checked_shell_protocol(&output).map_err(probe)?,
                        original,
                    )
                    .map_err(probe)?;
                } else {
                    return Err(pending());
                }
                drop(adb);
                let step = scope.prepare(ReconciliationMutation::ManagedVoiceCleanup)?;
                step.check()?;
                // The minted fixed cleanup chain may finish after cancellation
                // or ledger failure. It cannot authorize configuration writes.
                io.set_cleanup(Some(step.cleanup_admission()));
                let mut stop_may_have_submitted = false;
                let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> Result<VoiceSettlementEvidence, TransportError> {
                        let mut adb = AdbProbe::with_deadline(&mut io, scope.deadline());
                        adb.connect().map_err(probe)?;
                        stop_may_have_submitted = true;
                        let (mut response, stop_hash) = control
                            .request(&mut adb, ControlCommand::Stop)
                            .map_err(probe)?;
                        let mut responses = prior_responses;
                        responses.push(stop_hash);
                        let end = (Instant::now() + MODULE_VOICE_ROUTE_STOP_TIMEOUT)
                            .min(scope.deadline());
                        while response.state() != ControlState::Exited {
                            if Instant::now() >= end {
                                return Err(pending());
                            }
                            std::thread::sleep(Duration::from_millis(100));
                            let (next, hash) = control
                                .request(&mut adb, ControlCommand::Status)
                                .map_err(probe)?;
                            response = next;
                            responses.truncate(2);
                            responses.push(hash);
                        }
                        if response.pid() != original.startup_pid()
                            || matches!(response.exit(), Exit::None)
                        {
                            return Err(pending());
                        }
                        cleanup_module_voice_audio(
                            &mut adb,
                            5,
                            Duration::from_millis(200),
                            &mut || step.cleanup_check().map_err(|_| managed_control::failure()),
                        )
                        .map_err(probe)?;
                        let dsp_wire = read_stopped_dsp(&mut adb).map_err(probe)?;
                        let (released, hash) = control
                            .request(&mut adb, ControlCommand::Release)
                            .map_err(probe)?;
                        if released.state() != ControlState::Exited
                            || released.pid() != original.startup_pid()
                            || released.exit() != response.exit()
                        {
                            return Err(pending());
                        }
                        responses.push(hash);
                        if read_module_boot_id(&mut adb).map_err(probe)? != original.startup_boot()
                        {
                            return Err(pending());
                        }
                        Ok(VoiceSettlementEvidence {
                            policy: "startup-voice-v1".into(),
                            kind: "original_managed_settled".into(),
                            original_manifest_sha256: scope.original_manifest_sha256().into(),
                            original_record_sha256: Some(control.record_sha256().into()),
                            fresh_boot_sha256: digest(original.startup_boot().as_bytes()),
                            child_pid: Some(original.startup_pid()),
                            child_start_ticks: Some(original.startup_start_ticks()),
                            authenticated_responses: responses,
                            actual_child_exit: true,
                            audio_disabled: true,
                            playback_closed: true,
                            capture_closed: true,
                            actual_adb_closed: false,
                            original_record_pin_released: Some(false),
                            bounded_proc_scan: false,
                            dsp_wire,
                            no_live_wire: None,
                            successor: None,
                        })
                    },
                ))
                .unwrap_or_else(|_| Err(cleanup_failed()));
                io.set_cleanup(None);
                let outcome = if cleanup.is_ok() {
                    SubmissionOutcome::Acknowledged
                } else if stop_may_have_submitted {
                    SubmissionOutcome::SubmittedUnknown
                } else {
                    SubmissionOutcome::NotSubmitted
                };
                // Recording failure never skips close of an already claimed handle.
                let recorded = scope.record_outcome(&step, outcome);
                let value = cleanup?;
                recorded?;
                Ok(value)
            } else {
                let mut adb = AdbProbe::with_deadline(&mut io, scope.deadline());
                adb.connect().map_err(probe)?;
                inspect_no_live(
                    scope,
                    &mut adb,
                    &managed_control::startup::sessions_for_settlement(
                        scope.root(),
                        scope.root_proof(),
                        None,
                    )
                    .map_err(probe)?,
                )
            }
        },
    ))
    .unwrap_or_else(|_| Err(cleanup_failed()));
    let closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| io.checked_close()))
        .unwrap_or_else(|_| Err(cleanup_failed()));
    let unpinned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        control.map(|c| c.checked_release()).transpose()
    }))
    .unwrap_or_else(|_| Err(managed_control::failure()))
    .map_err(|_| cleanup_failed());
    #[cfg(test)]
    if unpinned.is_ok() && owned.is_some() {
        if let Some(probe) = observer {
            probe.pin_released();
        }
    }
    closed?;
    unpinned?;
    let mut evidence = result?;
    evidence.actual_adb_closed = true;
    evidence.original_record_pin_released = if owned.is_some() { Some(true) } else { None };
    scope.check()?;
    let proof = VoiceSettledProof {
        seal: scope.seal(),
        evidence,
    };
    proof.validate_for(scope)?;
    Ok(proof)
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    struct Io<'a> {
        handle: Option<rusb::DeviceHandle<rusb::Context>>,
        interface: u8,
        input: u8,
        output: u8,
        claimed: bool,
        scope: &'a ReconciliationScope<'a>,
        cleanup: Option<crate::module_radio::CallAdmission>,
    }
    impl Io<'_> {
        fn timeout(&self, requested: Duration) -> Result<Duration, ProbeError> {
            self.check()?;
            let left = self
                .scope
                .deadline()
                .saturating_duration_since(Instant::now());
            if left < Duration::from_millis(1) || requested < Duration::from_millis(1) {
                return Err(managed_control::failure());
            }
            Ok(requested.min(left))
        }
        fn check(&self) -> Result<(), ProbeError> {
            let result = match &self.cleanup {
                Some(admission) if admission() => Ok(()),
                Some(_) => Err(pending()),
                None => self.scope.check(),
            };
            result.map_err(|_| {
                ProbeError::new(
                    "startup_voice_scope_revoked",
                    "Startup voice scope is unavailable",
                )
            })?;
            if Instant::now() >= self.scope.deadline() {
                return Err(ProbeError::new(
                    "startup_voice_deadline",
                    "Startup voice deadline elapsed",
                ));
            }
            Ok(())
        }
        fn close(mut self) -> Result<(), TransportError> {
            let result = self
                .handle
                .as_ref()
                .ok_or_else(cleanup_failed)?
                .release_interface(self.interface);
            self.claimed = false;
            drop(self.handle.take());
            match result {
                Ok(()) => Ok(()),
                Err(rusb::Error::NoDevice) => Err(pending()),
                Err(_) => Err(cleanup_failed()),
            }
        }
    }
    impl Drop for Io<'_> {
        fn drop(&mut self) {
            if self.claimed {
                if let Some(h) = &self.handle {
                    let _ = h.release_interface(self.interface);
                }
            }
            drop(self.handle.take());
        }
    }
    impl BulkIo for Io<'_> {
        fn read_bulk(&mut self, bytes: &mut [u8], timeout: Duration) -> Result<usize, ProbeError> {
            self.check()?;
            let timeout = self.timeout(timeout)?;
            let result = self
                .handle
                .as_ref()
                .ok_or_else(managed_control::failure)?
                .read_bulk(self.input, bytes, timeout)
                .map_err(|e| {
                    native_adb_error(e, "startup_voice_usb_read", "Startup ADB read failed")
                });
            self.check()?;
            result
        }
        fn write_bulk(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, ProbeError> {
            self.check()?;
            let timeout = self.timeout(timeout)?;
            let result = self
                .handle
                .as_ref()
                .ok_or_else(managed_control::failure)?
                .write_bulk(self.output, bytes, timeout)
                .map_err(|e| {
                    native_adb_error(e, "startup_voice_usb_write", "Startup ADB write failed")
                });
            self.check()?;
            result
        }
    }
    impl SettlementIo for Io<'_> {
        fn set_cleanup(&mut self, admission: Option<crate::module_radio::CallAdmission>) {
            self.cleanup = admission;
        }
        fn checked_close(self) -> Result<(), TransportError> {
            self.close()
        }
    }
    fn tuple(
        device: &rusb::Device<rusb::Context>,
        stored: Option<(u8, u8, u8, u8)>,
    ) -> Result<AdbInterface, TransportError> {
        let config = device.active_config_descriptor().map_err(|_| pending())?;
        let mut found = Vec::new();
        for iface in config.interfaces() {
            for alt in iface.descriptors() {
                if (alt.class_code(), alt.sub_class_code(), alt.protocol_code()) != (0xff, 0x42, 1)
                {
                    continue;
                }
                let eps = alt.endpoint_descriptors().collect::<Vec<_>>();
                if eps.len() != 2
                    || eps.iter().any(|e| {
                        e.transfer_type() != rusb::TransferType::Bulk
                            || e.address() & 15 == 0
                            || e.address() & 0x70 != 0
                    })
                {
                    return Err(pending());
                }
                let candidate = adb_interface(
                    alt.interface_number(),
                    alt.setting_number(),
                    alt.class_code(),
                    alt.sub_class_code(),
                    alt.protocol_code(),
                    eps.iter().map(|e| (e.address(), true)),
                )
                .ok_or_else(pending)?;
                if candidate.endpoint_in & 0x80 == 0 || candidate.endpoint_out & 0x80 != 0 {
                    return Err(pending());
                }
                found.push(candidate);
            }
        }
        if found.len() != 1 {
            return Err(pending());
        }
        let one = found[0];
        if stored.is_some_and(|s| {
            s != (
                one.interface,
                one.alternate,
                one.endpoint_in,
                one.endpoint_out,
            )
        }) {
            return Err(pending());
        }
        Ok(one)
    }
    pub(super) fn inspect_successor(
        scope: &ReconciliationScope<'_>,
        target: &PhysicalTarget,
        allowed: &[String],
    ) -> Result<VoiceSettlementEvidence, TransportError> {
        scope.check()?;
        let device = target.native_device()?;
        let desc = device.device_descriptor().map_err(|_| pending())?;
        if !matches!(
            (desc.vendor_id(), desc.product_id()),
            (0x2ca3, 0x4006) | (0x2c7c, 0x0125)
        ) {
            return Err(pending());
        }
        let expected = tuple(&device, None)?;
        scope.check()?;
        let handle = device.open().map_err(|_| pending())?;
        if handle.device().as_raw() != device.as_raw()
            || tuple(
                &device,
                Some((
                    expected.interface,
                    expected.alternate,
                    expected.endpoint_in,
                    expected.endpoint_out,
                )),
            )? != expected
        {
            return Err(pending());
        }
        scope.check()?;
        handle
            .claim_interface(expected.interface)
            .map_err(|_| pending())?;
        let mut io = Io {
            handle: Some(handle),
            interface: expected.interface,
            input: expected.endpoint_in,
            output: expected.endpoint_out,
            claimed: true,
            scope,
            cleanup: None,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let timeout = io.timeout(Duration::from_secs(1)).map_err(probe)?;
            let h = io.handle.as_ref().ok_or_else(pending)?;
            h.set_alternate_setting(expected.interface, expected.alternate)
                .map_err(|_| pending())?;
            let mut actual = [0; 1];
            let n = h
                .read_control(0x81, 10, 0, expected.interface as u16, &mut actual, timeout)
                .map_err(|_| pending())?;
            if n != 1 || actual[0] != expected.alternate {
                return Err(pending());
            }
            let mut adb = AdbProbe::with_deadline(&mut io, scope.deadline());
            adb.connect().map_err(probe)?;
            inspect_no_live(scope, &mut adb, allowed)
        }))
        .unwrap_or_else(|_| Err(cleanup_failed()));
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| io.close()))
            .unwrap_or_else(|_| Err(cleanup_failed()))?;
        let mut e = result?;
        e.actual_adb_closed = true;
        scope.check()?;
        Ok(e)
    }
    pub(super) fn settle(
        scope: &ReconciliationScope<'_>,
        target: &PhysicalTarget,
    ) -> Result<VoiceSettledProof, TransportError> {
        let owned = scope
            .original_voice_binding()
            .and_then(|b| b.startup_owned());
        let control = capture_original_control(scope)?;
        let device = target.native_device()?;
        let desc = device.device_descriptor().map_err(|_| pending())?;
        if !matches!(
            (desc.vendor_id(), desc.product_id()),
            (0x2ca3, 0x4006) | (0x2c7c, 0x0125)
        ) {
            return Err(pending());
        }
        let expected = tuple(&device, owned.map(|(h, _)| h.startup_tuple()))?;
        scope.check()?;
        let handle = device.open().map_err(|_| pending())?;
        if handle.device().as_raw() != device.as_raw()
            || tuple(
                &device,
                Some((
                    expected.interface,
                    expected.alternate,
                    expected.endpoint_in,
                    expected.endpoint_out,
                )),
            )? != expected
        {
            return Err(pending());
        }
        scope.check()?;
        handle
            .claim_interface(expected.interface)
            .map_err(|_| pending())?;
        let io = Io {
            handle: Some(handle),
            interface: expected.interface,
            input: expected.endpoint_in,
            output: expected.endpoint_out,
            claimed: true,
            scope,
            cleanup: None,
        };
        settle_claimed(scope, io, control, |io| {
            io.check().map_err(probe)?;
            let alt_timeout = io.timeout(Duration::from_secs(1)).map_err(probe)?;
            let h = io.handle.as_ref().ok_or_else(pending)?;
            h.set_alternate_setting(expected.interface, expected.alternate)
                .map_err(|_| pending())?;
            let mut alt = [0; 1];
            let len = h
                .read_control(
                    0x81,
                    10,
                    0,
                    expected.interface as u16,
                    &mut alt,
                    alt_timeout,
                )
                .map_err(|_| pending())?;
            if len != 1 || alt[0] != expected.alternate {
                return Err(pending());
            }
            io.check().map_err(probe)?;
            Ok(())
        })
    }
}

#[cfg(test)]
pub(crate) fn scripted_settle_for_native(
    scope: &ReconciliationScope<'_>,
) -> Result<VoiceSettledProof, TransportError> {
    managed_control::startup::require_closed_inventory(scope.root(), scope.root_proof())
        .map_err(probe)?;
    scripted_no_live(
        scope,
        &managed_control::startup::sessions_for_settlement(scope.root(), scope.root_proof(), None)
            .map_err(probe)?,
    )
}
#[cfg(test)]
pub(crate) fn scripted_verify_successor_voice(
    scope: &ReconciliationScope<'_>,
    mut prior: VoiceSettledProof,
) -> Result<VoiceSettledProof, TransportError> {
    prior.validate_for(scope)?;
    if prior.has_successor_observation() {
        return Err(pending());
    }
    let binding = if prior.evidence.kind == "original_managed_settled" {
        scope
            .original_voice_binding()
            .and_then(|b| b.startup_owned())
            .map(|(_, c)| c)
    } else {
        None
    };
    let allowed = managed_control::startup::sessions_for_settlement(
        scope.root(),
        scope.root_proof(),
        binding,
    )
    .map_err(probe)?;
    let observed = scripted_no_live(scope, &allowed)?;
    prior.evidence.successor = Some(Box::new(observed.evidence));
    prior.validate_for(scope)?;
    Ok(prior)
}
#[cfg(test)]
fn scripted_no_live(
    scope: &ReconciliationScope<'_>,
    allowed_sessions: &[String],
) -> Result<VoiceSettledProof, TransportError> {
    use std::collections::VecDeque;
    struct Inert<'a> {
        scope: &'a ReconciliationScope<'a>,
        allowed_sessions: Vec<String>,
        incoming: VecDeque<u8>,
        outgoing: Vec<u8>,
        commands: usize,
        closed: bool,
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
    impl BulkIo for Inert<'_> {
        fn read_bulk(&mut self, bytes: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
            self.scope.check().map_err(|_| managed_control::failure())?;
            if self.incoming.is_empty() {
                return Err(managed_control::failure());
            }
            let n = bytes.len().min(self.incoming.len()).min(7);
            for b in &mut bytes[..n] {
                *b = self.incoming.pop_front().unwrap();
            }
            Ok(n)
        }
        fn write_bulk(&mut self, bytes: &[u8], _: Duration) -> Result<usize, ProbeError> {
            self.scope.check().map_err(|_| managed_control::failure())?;
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
                    let boot = "00112233-4455-6677-8899-aabbccddeeff";
                    let output = match self.commands {
                        0 if service.contains(&no_live_command(&self.allowed_sessions)) => {
                            let hex = boot
                                .as_bytes()
                                .iter()
                                .chain(std::iter::once(&b'\n'))
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>();
                            format!("SWNOVOICE1\n{hex}\n1,2,\n{hex}\n")
                        }
                        1 if service.contains(READ_STOPPED_DSP) => {
                            "SWDSP1\n300a\n636c6f7365640a\n636c6f7365640a\n".into()
                        }
                        2 if service.contains("/proc/sys/kernel/random/boot_id") => {
                            format!("{boot}\n")
                        }
                        _ => return Err(managed_control::failure()),
                    };
                    self.commands += 1;
                    let output = format!("{output}\n__SWITCH_ADB_STATUS_{token}_0__\n");
                    self.incoming.extend(frame(ADB_OKAY, 42, local, &[]));
                    self.incoming
                        .extend(frame(ADB_WRTE, 42, local, output.as_bytes()));
                    self.incoming.extend(frame(ADB_CLSE, 42, local, &[]));
                }
            }
            Ok(bytes.len())
        }
        fn close_confirmed(&mut self) -> Result<(), ProbeError> {
            if self.closed {
                return Err(managed_control::failure());
            }
            self.closed = true;
            self.incoming.clear();
            self.outgoing.clear();
            Ok(())
        }
    }
    scope.check()?;
    let mut io = Inert {
        scope,
        allowed_sessions: allowed_sessions.to_vec(),
        incoming: VecDeque::new(),
        outgoing: Vec::new(),
        commands: 0,
        closed: false,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut adb = AdbProbe::with_deadline(&mut io, scope.deadline());
        adb.connect().map_err(probe)?;
        inspect_no_live(scope, &mut adb, allowed_sessions)
    }))
    .unwrap_or_else(|_| Err(cleanup_failed()));
    io.close_confirmed().map_err(|_| cleanup_failed())?;
    drop(io);
    let mut evidence = result?;
    evidence.actual_adb_closed = true;
    let proof = VoiceSettledProof {
        seal: scope.seal(),
        evidence,
    };
    proof.validate_for(scope)?;
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_shell_status_separator_is_removed_once_without_weakening_protocol() {
        let golden = b"SWDSP1\n300a\n636c6f7365640a\n636c6f7365640a\n";
        let mut framed = golden.to_vec();
        framed.push(b'\n');
        let token = "0123456789abcdef0123456789abcdef";
        let mut wire = framed.clone();
        wire.extend_from_slice(format!("__SWITCH_ADB_STATUS_{token}_0__\n").as_bytes());
        let (before, status) = parse_shell_result(&wire, token).unwrap();
        assert_eq!(status, 0);
        assert_eq!(before, framed);
        assert!(!parse_stopped_dsp(&before));
        let protocol = checked_shell_protocol(&before).unwrap();
        assert_eq!(protocol, golden);
        assert!(parse_stopped_dsp(protocol));
        assert!(!parse_stopped_dsp(
            checked_shell_protocol(protocol).unwrap()
        ));
        let mut malformed = framed;
        malformed.push(b'\n');
        assert!(!parse_stopped_dsp(
            checked_shell_protocol(&malformed).unwrap()
        ));
        assert!(checked_shell_protocol(b"SWDSP1").is_err());
    }
    #[test]
    fn exited_identity_command_and_protocol_golden_keep_shell_quotes_and_newlines() {
        let path = "/tmp/proletarius-call/control-00000000000000000000000000000000/child.pid";
        let command = exited_identity_command(path, 42).unwrap();
        assert!(command.contains(r#"-N "$2" "$1""#));
        assert!(command.contains(r#"printf 'SWVOICEEXIT1\n%s\n%s\n' "$b" "$p""#));
        assert!(!command.contains(r#"\""#));
        assert!(!command.contains(r#"\\n"#));
        assert!(exited_identity_command("/tmp/foreign';anything", 42).is_err());
        let original:recovery_binding::NativeHelperObservation=serde_json::from_value(serde_json::json!({"boot_id":"00112233-4455-6677-8899-aabbccddeeff","pid":42,"start_ticks":123,"exact_argv":[MODULE_VOICE_HELPER,"--voice-route-session","--verbose"],"route_kind":"d4_uac","route":{"audio_enabled":false,"playback_running":false,"capture_running":false,"log_reports_active":false},"adb_interface":{"interface_number":2,"alternate_setting":0,"endpoint_in":129,"endpoint_out":1}})).unwrap();
        let golden=b"SWVOICEEXIT1\n30303131323233332d343435352d363637372d383839392d6161626263636464656566660a\n3432203132330a\n";
        assert!(verify_exited_identity(golden, &original).is_ok());
        let changed = String::from_utf8(golden.to_vec())
            .unwrap()
            .replace("3432203132330a", "3432203132340a");
        assert!(verify_exited_identity(changed.as_bytes(), &original).is_err());
    }
    fn boot_hex() -> String {
        b"00112233-4455-6677-8899-aabbccddeeff\n"
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    #[test]
    fn current_no_live_observation_requires_two_stable_boot_reads_and_unique_bounded_pids() {
        let b = boot_hex();
        assert!(parse_no_live(format!("SWNOVOICE1\n{b}\n1,2,\n{b}\n").as_bytes()).is_ok());
        assert!(parse_no_live(format!("SWNOVOICE1\n{b}\n1,1,\n{b}\n").as_bytes()).is_err());
        assert!(parse_no_live(format!("SWNOVOICE1\n{b}\n1,\n00\n").as_bytes()).is_err());
        assert!(parse_no_live(b"SWNOVOICE1\n").is_err());
    }
    #[test]
    fn current_no_live_decoder_rejects_truncated_boot_and_unbounded_process_inventory() {
        let b = boot_hex();
        let ids = (1..=257).map(|n| format!("{n},")).collect::<String>();
        assert!(parse_no_live(format!("SWNOVOICE1\n{b}\n{ids}\n{b}\n").as_bytes()).is_err());
        assert!(parse_no_live(format!("SWNOVOICE1\n{b}\n1,\n{b}").as_bytes()).is_err());
        let bad = b.replacen('0', "x", 1);
        assert!(parse_no_live(format!("SWNOVOICE1\n{bad}\n1,\n{bad}\n").as_bytes()).is_err());
    }
    #[test]
    fn dsp_closed_decoder_preserves_binary_boundaries_and_rejects_truncation() {
        assert!(parse_stopped_dsp(
            b"SWDSP1\n300a\n636c6f7365640a\n636c6f7365640a\n"
        ));
        for wire in [
            b"SWDSP1\n300a0a\n636c6f7365640a\n636c6f7365640a\n".as_slice(),
            b"SWDSP1\n300a\n636c6f7365640a0a\n636c6f7365640a\n",
            b"SWDSP1\n300a\n636c6f7365640a\n",
        ] {
            assert!(!parse_stopped_dsp(wire));
        }
    }
}

#[cfg(test)]
mod owned_fixture;
#[cfg(test)]
pub(crate) use owned_fixture::{
    ManagedVoiceArrivalProbe, ManagedVoiceScenario, OriginalManagedVoiceFixture,
    scripted_owned_settle_for_native,
};
