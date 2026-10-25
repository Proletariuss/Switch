//! Data captured by a live original owner, never startup execution authority.
use super::*;

#[cfg(test)]
mod tests;

const CAPTURE_BUDGET: Duration = Duration::from_secs(15);
const MAX_STAT: usize = 512;
const MAX_ARGV: usize = 256;
static CAPTURE_INSTANCE: std::sync::OnceLock<Result<String, ProbeError>> =
    std::sync::OnceLock::new();

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WireBinding")]
pub(crate) struct DurableVoiceBinding {
    schema_version: u8,
    producer: String,
    owner_instance: String,
    registry_revision: u64,
    observation: Observation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_control: Option<managed_control::DurableControlBinding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBinding {
    schema_version: u8,
    producer: String,
    owner_instance: String,
    registry_revision: u64,
    observation: Observation,
    #[serde(default)]
    managed_control: Option<managed_control::DurableControlBinding>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Observation {
    /// This process has no recorded route history. Never historical clean.
    CurrentProcessEmpty,
    /// This process observed its obligations retire after confirmed cleanup.
    /// This remains a current-owner observation, not a global historical claim.
    OriginalOwnerNoOwnedCleanup,
    Unknown {
        reason: UnknownReason,
    },
    Owned {
        runtime_generation: u64,
        helper: NativeHelperObservation,
    },
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnknownReason {
    MissingCapture,
    UnboundConnection,
    ReadUnavailable,
    MissingManagedControl,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeHelperObservation {
    boot_id: String,
    pid: u32,
    start_ticks: u64,
    exact_argv: Vec<String>,
    route_kind: RouteKind,
    route: RouteObservation,
    /// This tuple belongs to the actual selected ADB Device that was compared
    /// with the original and current captured AT Device, never a bus lookup.
    adb_interface: InterfaceObservation,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RouteKind {
    D4Uac,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfaceObservation {
    interface_number: u8,
    alternate_setting: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteObservation {
    audio_enabled: bool,
    playback_running: bool,
    capture_running: bool,
    log_reports_active: bool,
}
impl std::fmt::Debug for DurableVoiceBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableVoiceBinding")
            .finish_non_exhaustive()
    }
}
impl TryFrom<WireBinding> for DurableVoiceBinding {
    type Error = &'static str;
    fn try_from(wire: WireBinding) -> Result<Self, Self::Error> {
        let valid = wire.schema_version == 1
            && valid_producer(&wire.producer)
            && wire.owner_instance.len() == 64
            && wire
                .owner_instance
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && wire.registry_revision != u64::MAX
            && match &wire.observation {
                Observation::CurrentProcessEmpty => wire.registry_revision == 0,
                Observation::OriginalOwnerNoOwnedCleanup => wire.registry_revision > 0,
                Observation::Unknown { .. } => true,
                Observation::Owned {
                    runtime_generation,
                    helper,
                } => *runtime_generation > 0 && wire.registry_revision > 0 && helper.valid(),
            };
        if !valid {
            return Err("invalid durable voice observation");
        }
        if let Some(control) = &wire.managed_control {
            if !control.matches_producer(&wire.producer) {
                return Err("foreign managed voice control");
            }
            if let Observation::Owned {
                runtime_generation, ..
            } = &wire.observation
            {
                if !control.matches(&wire.producer, *runtime_generation) {
                    return Err("foreign managed voice generation");
                }
            }
        }
        Ok(Self {
            schema_version: wire.schema_version,
            producer: wire.producer,
            owner_instance: wire.owner_instance,
            registry_revision: wire.registry_revision,
            observation: wire.observation,
            managed_control: wire.managed_control,
        })
    }
}
impl NativeHelperObservation {
    fn valid(&self) -> bool {
        valid_module_boot_id(&self.boot_id)
            && self.boot_id.bytes().all(|b| !b.is_ascii_uppercase())
            && self.pid > 1
            && self.start_ticks > 0
            && self.exact_argv == [MODULE_VOICE_HELPER, "--voice-route-session", "--verbose"]
            && (0x81..=0x8f).contains(&self.adb_interface.endpoint_in)
            && (1..=15).contains(&self.adb_interface.endpoint_out)
    }
}
impl DurableVoiceBinding {
    pub(crate) fn matches_producer(&self, producer: &str) -> bool {
        self.producer == producer
    }
    pub(super) fn startup_owned(
        &self,
    ) -> Option<(
        &NativeHelperObservation,
        &managed_control::DurableControlBinding,
    )> {
        match (&self.observation, &self.managed_control) {
            (Observation::Owned { helper, .. }, Some(control)) => Some((helper, control)),
            _ => None,
        }
    }
}

impl NativeHelperObservation {
    pub(super) fn startup_tuple(&self) -> (u8, u8, u8, u8) {
        (
            self.adb_interface.interface_number,
            self.adb_interface.alternate_setting,
            self.adb_interface.endpoint_in,
            self.adb_interface.endpoint_out,
        )
    }
    pub(super) fn startup_pid(&self) -> u32 {
        self.pid
    }
    pub(super) fn startup_start_ticks(&self) -> u64 {
        self.start_ticks
    }
    pub(super) fn startup_boot(&self) -> &str {
        &self.boot_id
    }
    pub(super) fn same_startup_identity(&self, other: &Self) -> bool {
        self.boot_id == other.boot_id
            && self.pid == other.pid
            && self.start_ticks == other.start_ticks
            && self.exact_argv == other.exact_argv
            && self.route_kind == other.route_kind
            && self.adb_interface == other.adb_interface
    }
}

/// NonClone publication pin. Drop this only after Intent and its admission
/// fence are durable/attached, and before entering the original Stop helper.
pub(crate) struct CapturedRestoreVoiceBinding {
    binding: DurableVoiceBinding,
    _permit: tokio::sync::SemaphorePermit<'static>,
    adb_namespace: Option<crate::call_mode_backups::OperationNamespaceWitness>,
    scope: Option<PublicationScope>,
}
impl CapturedRestoreVoiceBinding {
    pub(crate) fn binding(&self) -> &DurableVoiceBinding {
        &self.binding
    }
    pub(crate) fn validate_for_publication(&self) -> Result<(), ProbeError> {
        if let Some(scope) = &self.scope {
            scope.validate()?;
        }
        if let Some(witness) = &self.adb_namespace {
            witness.validate().map_err(|_| structural())?;
        }
        Ok(())
    }
}
struct PublicationScope {
    root: std::path::PathBuf,
    proof: crate::daemon_lifetime::RootProof,
    radio: crate::module_radio::Service,
    inbox: crate::message_archive::InboxState,
    lease: crate::module_radio::CallModeReservation,
    revision: u64,
    obligation: Option<(
        u64,
        std::sync::Weak<Mutex<SelectedAdbPlan>>,
        std::sync::Weak<crate::module_recovery::OperationGuard>,
    )>,
}
impl PublicationScope {
    fn validate(&self) -> Result<(), ProbeError> {
        self.proof
            .with_root(&self.root, |_| ())
            .map_err(|_| structural())?;
        ensure_current_producer(&self.inbox, &self.lease.producer_id)?;
        self.radio
            .validate_call_mode_operation_lock(&self.lease)
            .map_err(|_| structural())?;
        let state = voice_route_ownership().lock().map_err(|_| structural())?;
        let now = snapshot(&state, &self.lease.producer_id)?;
        let matches = match (&now.obligation, &self.obligation) {
            (None, None) => true,
            (Some(now), Some((generation, plan, right))) => {
                now.generation == *generation
                    && plan.ptr_eq(&Arc::downgrade(&now.target.plan.0))
                    && right.ptr_eq(&Arc::downgrade(&now.authority))
            }
            _ => false,
        };
        if now.revision != self.revision || !matches {
            return Err(structural());
        }
        Ok(())
    }
}
impl std::fmt::Debug for CapturedRestoreVoiceBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedRestoreVoiceBinding")
            .finish_non_exhaustive()
    }
}

fn structural() -> ProbeError {
    ProbeError::new(
        "module_voice_capture_owner_changed",
        "原语音观察所属操作或目录归属未确认",
    )
}
fn unavailable() -> ProbeError {
    ProbeError::new(
        "module_voice_capture_read_unavailable",
        "原语音助手只读身份未确认",
    )
}
fn acquire_capture_interface_lock(
    root: &Path,
    proof: &crate::daemon_lifetime::RootProof,
) -> Result<crate::call_mode_backups::OperationFileLock, ProbeError> {
    crate::call_mode_backups::acquire_bound_recovery_lock(
        root,
        proof,
        crate::call_mode_backups::RecoveryLockKind::AdbInterface,
        false,
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            unavailable()
        } else {
            structural()
        }
    })
}
fn process_instance() -> Result<String, ProbeError> {
    CAPTURE_INSTANCE
        .get_or_init(|| crate::multistandby_store::random_token().map_err(|_| structural()))
        .clone()
}
struct RegistrySnapshot {
    revision: u64,
    obligation: Option<VoiceCleanupObligation>,
}
fn snapshot(
    ownership: &VoiceRouteOwnership,
    producer: &str,
) -> Result<RegistrySnapshot, ProbeError> {
    if ownership.observation_revision_exhausted || ownership.observation_revision == u64::MAX {
        return Err(structural());
    }
    let obligation = restore_voice_cleanup_obligation(ownership, producer)?;
    Ok(RegistrySnapshot {
        revision: ownership.observation_revision,
        obligation,
    })
}
fn still_current(
    ownership: &VoiceRouteOwnership,
    producer: &str,
    original: &RegistrySnapshot,
) -> Result<(), ProbeError> {
    let now = snapshot(ownership, producer)?;
    let same = match (&now.obligation, &original.obligation) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.generation == b.generation
                && Arc::ptr_eq(&a.target.plan.0, &b.target.plan.0)
                && Arc::ptr_eq(&a.authority, &b.authority)
        }
        _ => false,
    };
    if now.revision != original.revision || !same {
        return Err(structural());
    }
    Ok(())
}

pub(crate) async fn capture_restore_voice_binding(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    reservation: &crate::module_radio::CallModeReservation,
) -> Result<CapturedRestoreVoiceBinding, ProbeError> {
    let root = data_root.to_path_buf();
    let lease = reservation.clone();
    let deadline = Instant::now() + CAPTURE_BUDGET;
    // Cancellation of the public waiter must not cancel an accepted read or
    // release ADB admission while its real handle remains open.
    retain_capture(capture_inner(root, inbox, usb_at, radio, lease, deadline)).await
}
async fn retain_capture(
    work: impl std::future::Future<Output = Result<CapturedRestoreVoiceBinding, ProbeError>>
    + Send
    + 'static,
) -> Result<CapturedRestoreVoiceBinding, ProbeError> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = work.await;
        let _ = send.send(result);
    });
    receive.await.map_err(|_| structural())?
}

async fn capture_inner(
    root: std::path::PathBuf,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    lease: crate::module_radio::CallModeReservation,
    deadline: Instant,
) -> Result<CapturedRestoreVoiceBinding, ProbeError> {
    let permit = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        ADB_ADMISSION.acquire(),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| structural())?;
    ensure_current_producer(&inbox, &lease.producer_id)?;
    radio
        .validate_call_mode_operation_lock(&lease)
        .map_err(|_| structural())?;
    let original = {
        let ownership = voice_route_ownership().lock().map_err(|_| structural())?;
        snapshot(&ownership, &lease.producer_id)?
    };
    let proof = radio
        .module_recovery()
        .root_proof_for(&root)
        .map_err(|_| structural())?;
    let scope = PublicationScope {
        root: root.clone(),
        proof: proof.clone(),
        radio: radio.clone(),
        inbox: inbox.clone(),
        lease: lease.clone(),
        revision: original.revision,
        obligation: original.obligation.as_ref().map(|entry| {
            (
                entry.generation,
                Arc::downgrade(&entry.target.plan.0),
                Arc::downgrade(&entry.authority),
            )
        }),
    };
    let mut binding = DurableVoiceBinding {
        schema_version: 1,
        producer: lease.producer_id.clone(),
        owner_instance: process_instance()?,
        registry_revision: original.revision,
        observation: if original.revision == 0 {
            Observation::CurrentProcessEmpty
        } else {
            Observation::OriginalOwnerNoOwnedCleanup
        },
        managed_control: None,
    };
    let Some(obligation) = original.obligation.clone() else {
        // No interface lock, discovery, AT capture, or ADB command on empty.
        return Ok(CapturedRestoreVoiceBinding {
            binding,
            _permit: permit,
            adb_namespace: None,
            scope: Some(scope),
        });
    };
    let gate = radio.module_recovery().clone();
    let right = obligation.authority.right();
    if !right.is_active_for(&gate, &lease.producer_id) {
        return Err(structural());
    }
    let Some(managed) = obligation.target.managed.clone() else {
        binding.observation = Observation::Unknown {
            reason: UnknownReason::MissingManagedControl,
        };
        return Ok(CapturedRestoreVoiceBinding {
            binding,
            _permit: permit,
            adb_namespace: None,
            scope: Some(scope),
        });
    };
    managed.validate()?;
    binding.managed_control = Some(managed.binding());
    let Some(original_capture) = obligation.target.capture.as_ref() else {
        binding.observation = Observation::Unknown {
            reason: UnknownReason::MissingCapture,
        };
        return Ok(CapturedRestoreVoiceBinding {
            binding,
            _permit: permit,
            adb_namespace: None,
            scope: Some(scope),
        });
    };
    let current_capture = match usb_at
        .validate_voice_cleanup_capture(&lease.producer_id, original_capture)
        .await
    {
        Ok(capture) => capture,
        Err(error) if matches!(error.code, "usb_at_target_unconfirmed" | "usb_at_busy") => {
            binding.observation = Observation::Unknown {
                reason: UnknownReason::ReadUnavailable,
            };
            return Ok(CapturedRestoreVoiceBinding {
                binding,
                _permit: permit,
                adb_namespace: None,
                scope: Some(scope),
            });
        }
        Err(_) => return Err(structural()),
    };
    let Some(target) = current_capture.physical_target().cloned() else {
        binding.observation = Observation::Unknown {
            reason: UnknownReason::UnboundConnection,
        };
        return Ok(CapturedRestoreVoiceBinding {
            binding,
            _permit: permit,
            adb_namespace: None,
            scope: Some(scope),
        });
    };
    let namespace = radio
        .capture_call_mode_operation_namespace(&lease)
        .map_err(|_| structural())?;
    let voice_pin = radio
        .borrow_call_mode_operation_voice_settlement(&lease, &right)
        .map_err(|_| structural())?;
    let after_usb = usb_at.clone();
    let after_producer = lease.producer_id.clone();
    let mut captured = run_capture_worker(permit, move || {
        let current = || {
            if !right.is_active_for(&gate, &lease.producer_id)
                || !inbox.owns_module_producer(&lease.producer_id)
                || namespace.validate().is_err()
                || proof.with_root(&root, |_| ()).is_err()
            {
                return Err(structural());
            }
            Ok(())
        };
        current()?;
        let mut adb_namespace = None;
        let result = {
            let _interface = acquire_capture_interface_lock(&root, &proof);
            match _interface {
                Ok(_interface) => {
                    let witness = _interface.namespace_witness().map_err(|_| structural())?;
                    let mut admitted_current = || {
                        current()?;
                        _interface.validate().map_err(|_| structural())
                    };
                    admitted_current()?;
                    let mut control_session =
                        ModuleVoiceADBSession::checked(deadline, obligation.target.plan.clone());
                    let mut auth_adb = FencedModuleVoiceADB {
                        adb: &mut control_session,
                        current: &mut admitted_current,
                    };
                    let status =
                        managed.request(&mut auth_adb, switch_voice_supervisor::Command::Status)?;
                    if status.state() != switch_voice_supervisor::State::Running {
                        return Err(unavailable());
                    }
                    let expected_pid = status.pid();
                    drop(auth_adb);
                    let mut selected = obligation.target.plan.0.lock().map_err(|_| structural())?;
                    let result = match &selected.disconnected {
                        Some(_) => Err(unavailable()),
                        None => selected.target.capture_managed_restore_helper(
                            &target,
                            deadline,
                            &managed.pid_path(),
                            &mut admitted_current,
                        ),
                    };
                    drop(selected);
                    let mut auth_adb = FencedModuleVoiceADB {
                        adb: &mut control_session,
                        current: &mut admitted_current,
                    };
                    let status =
                        managed.request(&mut auth_adb, switch_voice_supervisor::Command::Status)?;
                    if status.state() != switch_voice_supervisor::State::Running
                        || status.pid() != expected_pid
                        || result
                            .as_ref()
                            .is_ok_and(|helper| helper.pid != expected_pid)
                    {
                        return Err(unavailable());
                    }
                    admitted_current()?;
                    adb_namespace = Some(witness);
                    result
                }
                Err(error) => Err(error),
            }
        };
        current()?;
        if let Some(witness) = &adb_namespace {
            witness.validate().map_err(|_| structural())?;
        }
        // All actual read handles have closed before the operation pin drops.
        drop(voice_pin);
        radio
            .validate_call_mode_operation_lock(&lease)
            .map_err(|_| structural())?;
        {
            let ownership = voice_route_ownership().lock().map_err(|_| structural())?;
            still_current(&ownership, &lease.producer_id, &original)?;
        }
        binding.observation = match result {
            Ok(helper) if obligation.generation > 0 && helper.valid() => Observation::Owned {
                runtime_generation: obligation.generation,
                helper,
            },
            Err(error)
                if error.code == "module_voice_capture_owner_changed"
                    || error.code == "module_voice_capture_foreign_target" =>
            {
                return Err(error);
            }
            Err(error) if error.code == "module_voice_capture_unbound" => Observation::Unknown {
                reason: UnknownReason::UnboundConnection,
            },
            _ => Observation::Unknown {
                reason: UnknownReason::ReadUnavailable,
            },
        };
        Ok((binding, adb_namespace))
    })
    .await?;
    captured.scope = Some(scope);
    // Recheck the original actual AT relation after the read handle really
    // closed. The publication pin still prevents another ADB route operation.
    if matches!(&captured.binding.observation, Observation::Owned { .. }) {
        let now = after_usb
            .validate_voice_cleanup_capture(&after_producer, &current_capture)
            .await
            .map_err(|_| structural())?;
        if now.physical_target().is_none() {
            return Err(structural());
        }
    }
    captured.validate_for_publication()?;
    Ok(captured)
}
async fn run_capture_worker(
    permit: tokio::sync::SemaphorePermit<'static>,
    work: impl FnOnce() -> Result<
        (
            DurableVoiceBinding,
            Option<crate::call_mode_backups::OperationNamespaceWitness>,
        ),
        ProbeError,
    > + Send
    + 'static,
) -> Result<CapturedRestoreVoiceBinding, ProbeError> {
    tokio::task::spawn_blocking(move || {
        // Both normal completion and a dropped JoinHandle keep this permit
        // through the worker's actual I/O and close, including error cleanup.
        let (binding, adb_namespace) = work()?;
        Ok(CapturedRestoreVoiceBinding {
            binding,
            _permit: permit,
            adb_namespace,
            scope: None,
        })
    })
    .await
    .map_err(|_| structural())?
}

// Fixed, read-only command. Hex keeps binary argv/stat boundaries intact;
// limit+1 reads let the decoder reject truncation instead of accepting prefixes.
const READ_HELPER: &str = r#"set -f; LC_ALL=C; export LC_ALL; h() { head -c "$2" "$1" 2>/dev/null | od -An -v -tx1 | tr -d ' \n'; }; b() { if test ! -r "$1"; then printf 2; else head -c 8192 "$1" 2>/dev/null | grep -q "$2"; s=$?; case "$s" in 0) printf 1;; 1) printf 0;; *) printf 2;; esac; fi; }; p=$(head -c 65 /run/proletarius-voice-route.pid 2>/dev/null) || exit 71; set -- $p; test "$#" -eq 2 || exit 72; pid=$1; start=$2; case "$pid:$start" in :*|*:|*[!0-9:]*) exit 73;; esac; test "${#pid}" -le 10 && test "${#start}" -le 20 || exit 74; printf 'SWVOICE1\n'; h /proc/sys/kernel/random/boot_id 38; printf '\n'; h /run/proletarius-voice-route.pid 65; printf '\n'; h "/proc/$pid/stat" 513; printf '\n'; h "/proc/$pid/cmdline" 257; printf '\n'; h /sys/class/android_usb/f_audio/audio_enable 3; printf '\n'; b /proc/asound/card0/pcm4p/sub0/status '^state: RUNNING'; printf '\n'; b /proc/asound/card0/pcm4c/sub0/status '^state: RUNNING'; printf '\n'; b /run/proletarius-voice-route.log 'VoLTE route session active on hw:0,4'; printf '\n'; h "/proc/$pid/stat" 513; printf '\n'; h "/proc/$pid/cmdline" 257; printf '\n'; h /run/proletarius-voice-route.pid 65; printf '\n'; h /proc/sys/kernel/random/boot_id 38; printf '\n'"#;

fn hex(line: &str, limit: usize) -> Result<Vec<u8>, ProbeError> {
    if line.is_empty() || line.len() > limit * 2 || line.len() % 2 != 0 {
        return Err(unavailable());
    }
    line.as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            };
            Ok(digit(p[0]).ok_or_else(unavailable)? * 16 + digit(p[1]).ok_or_else(unavailable)?)
        })
        .collect()
}
fn decimal<T: std::str::FromStr>(value: &str) -> Result<T, ProbeError> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(unavailable());
    }
    value.parse().map_err(|_| unavailable())
}
fn stat_identity(bytes: &[u8]) -> Result<(u32, u64), ProbeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| unavailable())?;
    let (pid, _) = text.split_once(" (").ok_or_else(unavailable)?;
    let end = text.rfind(") ").ok_or_else(unavailable)?;
    let fields: Vec<_> = text[end + 2..].split_ascii_whitespace().collect();
    if fields.len() < 20 || fields[0].len() != 1 {
        return Err(unavailable());
    }
    Ok((decimal(pid)?, decimal(fields[19])?))
}
fn parse_helper(
    output: &[u8],
    interface: InterfaceObservation,
) -> Result<NativeHelperObservation, ProbeError> {
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(unavailable());
    }
    let text = std::str::from_utf8(output).map_err(|_| unavailable())?;
    let lines: Vec<_> = text
        .strip_suffix('\n')
        .ok_or_else(unavailable)?
        .split('\n')
        .collect();
    if lines.len() != 13 || lines[0] != "SWVOICE1" {
        return Err(unavailable());
    }
    let boot = hex(lines[1], 37)?;
    if boot != hex(lines[12], 37)? {
        return Err(unavailable());
    }
    let boot_id = std::str::from_utf8(&boot)
        .map_err(|_| unavailable())?
        .trim_end_matches('\n')
        .to_owned();
    let pidfile = hex(lines[2], 64)?;
    if pidfile != hex(lines[11], 64)? {
        return Err(unavailable());
    }
    let fields: Vec<_> = std::str::from_utf8(&pidfile)
        .map_err(|_| unavailable())?
        .split_ascii_whitespace()
        .collect();
    if fields.len() != 2 {
        return Err(unavailable());
    }
    let pid: u32 = decimal(fields[0])?;
    let start_ticks: u64 = decimal(fields[1])?;
    if stat_identity(&hex(lines[3], MAX_STAT)?)? != (pid, start_ticks)
        || stat_identity(&hex(lines[9], MAX_STAT)?)? != (pid, start_ticks)
    {
        return Err(unavailable());
    }
    let argv = hex(lines[4], MAX_ARGV)?;
    if argv != hex(lines[10], MAX_ARGV)? || argv.last() != Some(&0) {
        return Err(unavailable());
    }
    let exact_argv = argv[..argv.len() - 1]
        .split(|b| *b == 0)
        .map(|arg| {
            std::str::from_utf8(arg)
                .map(str::to_owned)
                .map_err(|_| unavailable())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let flag = |line: &str| match line {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(unavailable()),
    };
    let audio = hex(lines[5], 2)?;
    let audio_enabled = match audio.as_slice() {
        b"0\n" | b"0" => false,
        b"1\n" | b"1" => true,
        _ => return Err(unavailable()),
    };
    let result = NativeHelperObservation {
        boot_id,
        pid,
        start_ticks,
        exact_argv,
        route_kind: RouteKind::D4Uac,
        route: RouteObservation {
            audio_enabled,
            playback_running: flag(lines[6])?,
            capture_running: flag(lines[7])?,
            log_reports_active: flag(lines[8])?,
        },
        adb_interface: interface,
    };
    if !result.valid() {
        return Err(unavailable());
    }
    Ok(result)
}

pub(super) fn startup_read_command(pid_path: &str) -> Result<String, ProbeError> {
    if !managed_control::valid_pid_path(pid_path) {
        return Err(structural());
    }
    Ok(READ_HELPER.replace("/run/proletarius-voice-route.pid", pid_path))
}
fn parse_checked_helper(
    output: &[u8],
    interface: InterfaceObservation,
) -> Result<NativeHelperObservation, ProbeError> {
    // ADB checked shell retains precisely one status-wrapper separator LF.
    // READ_HELPER's own terminator remains required by the strict parser.
    parse_helper(
        output.strip_suffix(b"\n").ok_or_else(unavailable)?,
        interface,
    )
}
pub(super) fn parse_startup_helper(
    output: &[u8],
    tuple: (u8, u8, u8, u8),
) -> Result<NativeHelperObservation, ProbeError> {
    parse_helper(
        output,
        InterfaceObservation {
            interface_number: tuple.0,
            alternate_setting: tuple.1,
            endpoint_in: tuple.2,
            endpoint_out: tuple.3,
        },
    )
}

#[cfg(target_os = "macos")]
struct CaptureIo<'a> {
    handle: Option<rusb::DeviceHandle<rusb::Context>>,
    interface: u8,
    endpoint_in: u8,
    endpoint_out: u8,
    claimed: bool,
    deadline: Instant,
    current: &'a mut dyn FnMut() -> Result<(), ProbeError>,
}
#[cfg(target_os = "macos")]
impl CaptureIo<'_> {
    fn check(&mut self) -> Result<(), ProbeError> {
        (self.current)()?;
        if Instant::now() >= self.deadline {
            return Err(unavailable());
        }
        Ok(())
    }
    fn close(mut self) -> Result<(), ProbeError> {
        // Original cleanup remains unconditional even after a failed fence.
        let released = self
            .handle
            .as_ref()
            .ok_or_else(unavailable)?
            .release_interface(self.interface);
        self.claimed = false;
        drop(self.handle.take()); // real libusb_close before reporting success
        released.map_err(|_| unavailable())
    }
}
#[cfg(target_os = "macos")]
impl Drop for CaptureIo<'_> {
    fn drop(&mut self) {
        if self.claimed {
            if let Some(handle) = &self.handle {
                let _ = handle.release_interface(self.interface);
            }
        }
        drop(self.handle.take());
    }
}
#[cfg(target_os = "macos")]
impl BulkIo for CaptureIo<'_> {
    fn read_bulk(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, ProbeError> {
        self.check()?;
        let result = self.handle.as_ref().ok_or_else(unavailable)?.read_bulk(
            self.endpoint_in,
            buffer,
            timeout.min(self.deadline.saturating_duration_since(Instant::now())),
        );
        self.check()?;
        result.map_err(|_| unavailable())
    }
    fn write_bulk(&mut self, payload: &[u8], timeout: Duration) -> Result<usize, ProbeError> {
        self.check()?;
        let result = self.handle.as_ref().ok_or_else(unavailable)?.write_bulk(
            self.endpoint_out,
            payload,
            timeout.min(self.deadline.saturating_duration_since(Instant::now())),
        );
        self.check()?;
        result.map_err(|_| unavailable())
    }
}
#[cfg(target_os = "macos")]
pub(super) fn capture_native(
    selected: &NativeAdbTarget,
    original: &crate::usb_at_transport::PhysicalTarget,
    deadline: Instant,
    current: &mut dyn FnMut() -> Result<(), ProbeError>,
) -> Result<NativeHelperObservation, ProbeError> {
    capture_native_with_pid(
        selected,
        original,
        deadline,
        "/run/proletarius-voice-route.pid",
        current,
    )
}
#[cfg(target_os = "macos")]
pub(super) fn capture_native_with_pid(
    selected: &NativeAdbTarget,
    original: &crate::usb_at_transport::PhysicalTarget,
    deadline: Instant,
    pid_path: &str,
    current: &mut dyn FnMut() -> Result<(), ProbeError>,
) -> Result<NativeHelperObservation, ProbeError> {
    if pid_path != "/run/proletarius-voice-route.pid" && !managed_control::valid_pid_path(pid_path)
    {
        return Err(structural());
    }
    current()?;
    let original = original
        .native_device()
        .map_err(|_| ProbeError::new("module_voice_capture_unbound", "没有原生AT物理观察"))?;
    if original.as_raw() != selected.device.as_raw() {
        return Err(ProbeError::new(
            "module_voice_capture_foreign_target",
            "ADB计划不是原AT连接",
        ));
    }
    let descriptor = selected
        .device
        .device_descriptor()
        .map_err(|_| unavailable())?;
    if !matches!(
        (descriptor.vendor_id(), descriptor.product_id()),
        (0x2ca3, 0x4006) | (0x2c7c, 0x0125)
    ) {
        return Err(unavailable());
    }
    let configuration = selected
        .device
        .active_config_descriptor()
        .map_err(|_| unavailable())?;
    let expected = AdbInterface {
        interface: selected.interface,
        alternate: selected.alternate,
        endpoint_in: selected.endpoint_in,
        endpoint_out: selected.endpoint_out,
    };
    let mut present = false;
    for interface in configuration.interfaces() {
        for alt in interface.descriptors() {
            present |= adb_interface(
                alt.interface_number(),
                alt.setting_number(),
                alt.class_code(),
                alt.sub_class_code(),
                alt.protocol_code(),
                alt.endpoint_descriptors()
                    .map(|ep| (ep.address(), ep.transfer_type() == rusb::TransferType::Bulk)),
            ) == Some(expected);
        }
    }
    if !present {
        return Err(unavailable());
    }
    current()?;
    if Instant::now() >= deadline {
        return Err(unavailable());
    }
    let handle = selected.device.open().map_err(|_| unavailable())?;
    if handle.device().as_raw() != selected.device.as_raw() {
        return Err(structural());
    }
    current()?;
    if Instant::now() >= deadline {
        return Err(unavailable());
    }
    handle
        .claim_interface(selected.interface)
        .map_err(|_| unavailable())?;
    let mut io = CaptureIo {
        handle: Some(handle),
        interface: selected.interface,
        endpoint_in: selected.endpoint_in,
        endpoint_out: selected.endpoint_out,
        claimed: true,
        deadline,
        current,
    };
    let result = (|| {
        io.check()?;
        if selected.alternate != 0 {
            io.handle
                .as_ref()
                .ok_or_else(unavailable)?
                .set_alternate_setting(selected.interface, selected.alternate)
                .map_err(|_| unavailable())?;
        }
        io.check()?;
        let mut adb = AdbProbe::with_deadline(&mut io, deadline);
        adb.connect()?;
        let (output, status) = adb.shell_checked_timeout(
            &READ_HELPER.replace("/run/proletarius-voice-route.pid", pid_path),
            CAPTURE_BUDGET,
        )?;
        if status != 0 {
            return Err(unavailable());
        }
        parse_checked_helper(
            &output,
            InterfaceObservation {
                interface_number: selected.interface,
                alternate_setting: selected.alternate,
                endpoint_in: selected.endpoint_in,
                endpoint_out: selected.endpoint_out,
            },
        )
    })();
    let result = finish_read(result, || io.close());
    current()?;
    result
}
fn finish_read<T>(
    result: Result<T, ProbeError>,
    close: impl FnOnce() -> Result<(), ProbeError>,
) -> Result<T, ProbeError> {
    let closed = close();
    match (result, closed) {
        (Ok(data), Ok(())) => Ok(data),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error),
    }
}
