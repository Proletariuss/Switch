//! Isolated module ADB probe and verified voice-runtime deployment.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

mod managed_control;
pub(crate) use managed_control::ControlStoreEvidence;
pub(crate) use managed_control::startup::verify_normal_origin_inventory;
pub(crate) use managed_control::startup::verify_normal_origin_registration;
pub(crate) use managed_control::startup::verify_normal_origin_writer_inventory;
#[cfg(test)]
pub(crate) fn completed_voice_control_store_files(
    files: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    managed_control::completed_store_files(files)
}
#[cfg(test)]
pub(crate) fn completed_voice_control_store_fixture_for_tests()
-> std::collections::BTreeMap<String, Vec<u8>> {
    managed_control::completed_files_fixture_for_tests()
}
mod recovery_binding;
pub(crate) use recovery_binding::{DurableVoiceBinding, capture_restore_voice_binding};
pub(crate) mod startup_recovery;

pub(crate) const INTERNAL_PROBE_PATH: &str = "/api/internal/call-mode/adb/probe";
pub(crate) const INTERNAL_DEPLOY_PATH: &str = "/api/internal/call-mode/adb/deploy-runtime";
pub(crate) const INTERNAL_ROUTE_START_PATH: &str = "/api/internal/call-mode/adb/voice-route/start";
pub(crate) const INTERNAL_ROUTE_STOP_PATH: &str = "/api/internal/call-mode/adb/voice-route/stop";
pub(crate) const INTERNAL_ROUTE_STATUS_PATH: &str =
    "/api/internal/call-mode/adb/voice-route/status";
pub(crate) const INTERNAL_RUNTIME_STATUS_PATH: &str =
    "/api/internal/call-mode/adb/voice-runtime/status";
pub(crate) const INTERNAL_RUNTIME_GENERATION_PATH: &str =
    "/api/internal/call-mode/adb/voice-runtime/generation";
pub(crate) const INTERNAL_PROBE_ARGUMENT: &str = "--internal-adb-probe";

const MAX_REQUEST_BYTES: usize = 4096;
const MAX_HELPER_ERROR_BYTES: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 4096;
const MAX_PAYLOAD: usize = 4096;
const PROBE_TIMEOUT: Duration = Duration::from_secs(12);
const USB_RELEASE_COOLDOWN: Duration = Duration::from_secs(2);
const MODULE_VOICE_REMOTE_DIRECTORY: &str = "/tmp/proletarius-call";
const MODULE_VOICE_HELPER: &str = "/tmp/proletarius-call/mavo-pcm-bridge.armv7";
const MODULE_VOICE_CALIBRATION_LOG: &str = "/run/proletarius-alsaucm.log";
const MODULE_VOICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(180);
const MODULE_VOICE_ROUTE_START_TIMEOUT: Duration = Duration::from_secs(60);
const MODULE_VOICE_ROUTE_STOP_TIMEOUT: Duration = Duration::from_secs(45);
const MODULE_VOICE_ROUTE_STATUS_TIMEOUT: Duration = Duration::from_secs(15);
const ADB_STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const ADB_STREAM_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const ADB_SYNC_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
const ADB_STREAM_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const ADB_SYNC_RESPONSE_LIMIT: usize = 4096;
static ADB_ADMISSION: Semaphore = Semaphore::const_new(1);
static VOICE_ROUTE_OWNERSHIP: std::sync::OnceLock<Mutex<VoiceRouteOwnership>> =
    std::sync::OnceLock::new();
static VOICE_RUNTIME_READINESS: std::sync::OnceLock<std::sync::Mutex<VoiceRuntimeReadiness>> =
    std::sync::OnceLock::new();

const ADB_VERSION: u32 = 0x0100_0001;
const ADB_CNXN: u32 = command(b"CNXN");
const ADB_AUTH: u32 = command(b"AUTH");
const ADB_OPEN: u32 = command(b"OPEN");
const ADB_OKAY: u32 = command(b"OKAY");
const ADB_CLSE: u32 = command(b"CLSE");
const ADB_WRTE: u32 = command(b"WRTE");

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProbeError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ProbeError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ProbeError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeRequest {
    producer_id: String,
    #[serde(default, deserialize_with = "deserialize_stop_policy")]
    stop_policy: Option<StopPolicy>,
    #[serde(default)]
    runtime_generation: Option<u64>,
    #[serde(default)]
    call_mode_token: Option<String>,
    #[serde(default)]
    call_mode_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StopPolicy {
    #[default]
    Always,
    Hangup,
    Shutdown,
    Restore,
}

fn deserialize_stop_policy<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<StopPolicy>, D::Error> {
    // Missing is compatible with older callers; explicit null is invalid.
    StopPolicy::deserialize(deserializer).map(Some)
}

fn validate_stop_policy(
    input: &ProbeRequest,
    action: Option<VoiceRouteAction>,
) -> Result<StopPolicy, ProbeError> {
    if input.stop_policy.is_some() && !matches!(action, Some(VoiceRouteAction::Stop)) {
        return Err(ProbeError::new(
            "invalid_request",
            "仅语音路由停止请求接受 stop_policy",
        ));
    }
    Ok(input.stop_policy.unwrap_or_default())
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StopPhase {
    Status,
    Stop,
}

#[derive(Debug)]
struct RouteExecutionFailure {
    // True only after actual stop/audio cleanup; USB cleanup can revoke it.
    stop_confirmed: bool,
    failure: VoiceRouteFailure,
    stop_phase: Option<StopPhase>,
}

impl RouteExecutionFailure {
    fn probe(error: ProbeError, phase: Option<StopPhase>) -> Self {
        let status = match error.code {
            "stale_backend_generation" => StatusCode::CONFLICT,
            "module_adb_probe_timeout" | "module_voice_route_timeout" => {
                StatusCode::GATEWAY_TIMEOUT
            }
            _ => StatusCode::FAILED_DEPENDENCY,
        };
        Self {
            stop_confirmed: false,
            failure: VoiceRouteFailure::from_probe(status, error),
            stop_phase: phase,
        }
    }
    fn from_stop(error: StopFailure) -> Self {
        let mut failure = Self::probe(error.error, Some(error.phase));
        failure.stop_confirmed = error.stop_confirmed;
        failure
    }
}

#[derive(Debug)]
struct StopFailure {
    stop_confirmed: bool,
    error: ProbeError,
    phase: StopPhase,
}

/// Actual admission resources, retained through ADB completion and USB cleanup.
/// A setup self-test borrows its existing Rust-owned exclusive operation;
/// normal audio startup instead holds the shared operation-file guard.
pub(crate) struct VoiceRouteGuard {
    _shared: Option<crate::call_mode_backups::OperationFileLock>,
    _call_mode: Option<crate::module_radio::CallModeVoiceGuard>,
    _radio_shared: Option<crate::module_radio::SharedVoiceGuard>,
}

impl VoiceRouteGuard {
    pub(crate) fn shared(lock: crate::call_mode_backups::OperationFileLock) -> Self {
        Self {
            _shared: Some(lock),
            _call_mode: None,
            _radio_shared: None,
        }
    }

    pub(crate) fn call_mode(guard: crate::module_radio::CallModeVoiceGuard) -> Self {
        Self {
            _shared: None,
            _call_mode: Some(guard),
            _radio_shared: None,
        }
    }
}

fn call_mode_credentials(
    input: &ProbeRequest,
    start: bool,
) -> Result<Option<(&str, u64)>, ProbeError> {
    match (&input.call_mode_token, input.call_mode_sequence, start) {
        (Some(token), Some(sequence), true) if !token.is_empty() && sequence > 0 => {
            Ok(Some((token, sequence)))
        }
        (None, None, false) => Ok(None),
        _ => Err(ProbeError::new(
            "module_voice_operation_invalid",
            "路由自检启动需要有效的通话模式操作凭据，其他 ADB 操作不得携带该凭据",
        )),
    }
}

#[derive(Serialize)]
struct ProbeResponse {
    ready: bool,
    code: &'static str,
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prepared: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_phase: Option<PreparationPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_phase: Option<StopPhase>,
}

#[derive(Serialize)]
struct RuntimeGenerationResponse {
    ready: bool,
    code: &'static str,
    error: String,
    runtime_generation: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum VoiceRouteAction {
    Start,
    Stop,
    Status,
}

#[derive(Debug)]
pub(crate) struct VoiceRouteFailure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl VoiceRouteFailure {
    fn from_probe(status: StatusCode, error: ProbeError) -> Self {
        Self {
            status,
            code: error.code,
            message: error.to_string(),
        }
    }
}

enum WorkerResponse {
    Ready(&'static str),
    RuntimeStatus { boot_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreparationPhase {
    Prepare,
    SelfTest,
}

#[derive(Debug)]
struct PreparationFailure {
    status: StatusCode,
    error: ProbeError,
    phase: PreparationPhase,
}

impl PreparationFailure {
    fn at(phase: PreparationPhase, error: ProbeError) -> Self {
        let status = match error.code {
            "stale_backend_generation"
            | "stale_module_voice_runtime_generation"
            | "unissued_module_voice_runtime_generation" => StatusCode::CONFLICT,
            "module_adb_probe_timeout" | "module_voice_route_timeout" => {
                StatusCode::GATEWAY_TIMEOUT
            }
            "module_voice_runtime_state_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
            "module_adb_worker_failed" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::FAILED_DEPENDENCY,
        };
        Self {
            status,
            error,
            phase,
        }
    }
}

trait RuntimePreparationIO {
    fn deploy(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<String, ProbeError>;
    fn route(
        &mut self,
        action: VoiceRouteAction,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<bool, ProbeError>;
    fn rollback(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<(), ProbeError>;
    fn boot_id(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<String, ProbeError>;
}

struct NativeRuntimePreparation<'a> {
    root: &'a Path,
    plan: Option<AdbPlan>,
    obligation: Option<VoiceCleanupObligation>,
    owner: Option<(
        String,
        u64,
        crate::usb_at::CapturedVoiceTarget,
        Arc<crate::module_recovery::OperationGuard>,
    )>,
    gate: Option<crate::module_recovery::Gate>,
}
impl<'a> NativeRuntimePreparation<'a> {
    fn new(
        root: &'a Path,
        target: Option<&crate::usb_at_transport::PhysicalTarget>,
    ) -> Result<Self, ProbeError> {
        Ok(Self {
            root,
            // A native AT connection must bind deployment and all subsequent
            // stages. No-AT operation retains its existing deferred selection.
            plan: target.map(AdbPlan::from_physical_target).transpose()?,
            obligation: None,
            owner: None,
            gate: None,
        })
    }
    fn selected_plan(&self) -> Result<&AdbPlan, ProbeError> {
        self.plan.as_ref().ok_or_else(|| {
            ProbeError::new(
                "module_adb_target_unconfirmed",
                "ADB 准备计划尚未选择原设备",
            )
        })
    }
}
impl RuntimePreparationIO for NativeRuntimePreparation<'_> {
    fn deploy(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<String, ProbeError> {
        let boot_id = deploy_module_voice_runtime(self.root, &mut self.plan, &mut || current())?;
        if let Some((producer, generation, capture, authority)) = &self.owner {
            self.obligation = Some(VoiceCleanupObligation {
                producer: producer.clone(),
                generation: *generation,
                authority: authority.clone(),
                target: VoiceBoundTarget {
                    plan: self.selected_plan()?.clone(),
                    capture: Some(capture.clone()),
                    managed: None,
                },
            });
        }
        Ok(boot_id)
    }
    fn route(
        &mut self,
        action: VoiceRouteAction,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<bool, ProbeError> {
        if matches!(action, VoiceRouteAction::Start) {
            if let Some(gate) = &self.gate {
                ensure_voice_admission(gate)?;
            }
            let proof = self
                .gate
                .as_ref()
                .ok_or_else(ownership_unavailable)?
                .root_proof_for(self.root)
                .map_err(|_| ownership_unavailable())?;
            let obligation = self.obligation.as_mut().ok_or_else(ownership_unavailable)?;
            if obligation.target.managed.is_none() {
                obligation.target.managed = Some(managed_control::ManagedControl::create(
                    self.root,
                    &proof,
                    &obligation.producer,
                    obligation.generation,
                )?);
            }
            voice_route_ownership()
                .lock()
                .map_err(|_| ownership_unavailable())?
                .begin(obligation.clone())?;
        }
        if let Some(control) = self
            .obligation
            .as_ref()
            .and_then(|entry| entry.target.managed.as_ref())
        {
            let plan = self.selected_plan()?.clone();
            let _lock = control.acquire_lock()?;
            current()?;
            if matches!(action, VoiceRouteAction::Start) && !control.has_submitted() {
                control.submit(&plan, current)?;
            }
            let mut session = ModuleVoiceADBSession::checked(
                Instant::now() + MODULE_VOICE_ROUTE_STOP_TIMEOUT,
                plan,
            );
            let mut adb = FencedModuleVoiceADB {
                adb: &mut session,
                current,
            };
            module_voice_route_root(&mut adb)?;
            return managed_control::route(&mut adb, control, action, &mut || Ok(()));
        }
        module_voice_route_action_fenced(self.root, self.selected_plan()?, action, &mut || {
            current()?;
            if matches!(action, VoiceRouteAction::Start) {
                if let Some(gate) = &self.gate {
                    ensure_voice_admission(gate)?;
                }
            }
            Ok(())
        })
    }
    fn rollback(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<(), ProbeError> {
        current()?;
        let _lock = acquire_interface_lock(self.root)?;
        current()?;
        let mut session = ModuleVoiceADBSession::new(
            Instant::now() + MODULE_VOICE_ROUTE_STOP_TIMEOUT,
            self.selected_plan()?.clone(),
        );
        let mut adb = FencedModuleVoiceADB {
            adb: &mut session,
            current,
        };
        let stopped = match self
            .obligation
            .as_ref()
            .and_then(|entry| entry.target.managed.as_ref())
        {
            Some(control) => !managed_control::route(
                &mut adb,
                control,
                VoiceRouteAction::Status,
                &mut || Ok(()),
            )?,
            None => module_voice_route_stopped(&mut adb)?,
        };
        if !stopped {
            return Err(ProbeError::new(
                "module_voice_route_stop_unconfirmed",
                "D4 helper 退出尚未确认，停止音频回滚",
            ));
        }
        let result =
            cleanup_module_voice_audio(&mut adb, 5, Duration::from_millis(200), &mut || Ok(()));
        (adb.current)()?;
        result
    }
    fn boot_id(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<String, ProbeError> {
        module_voice_runtime_boot_id_fenced(self.root, self.selected_plan()?, &mut || current())
    }
}

fn prepare_runtime_pipeline(
    io: &mut impl RuntimePreparationIO,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<String, PreparationFailure> {
    current().map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
    let boot_id = io
        .deploy(current)
        .map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
    current().map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))?;
    reconcile_prepare_route(io, VoiceRouteAction::Start, true, current)
        .map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))?;
    current().map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))?;
    let reconciled_stop = reconcile_prepare_route(io, VoiceRouteAction::Stop, false, current)
        .map_err(|mut error| {
            error.message = format!("语音路由已启动，但安全回滚未通过：{}", error.message);
            PreparationFailure::at(PreparationPhase::SelfTest, error)
        })?;
    if reconciled_stop {
        // A missing helper alone does not prove audio_enable or T/T/B rollback.
        current()
            .and_then(|()| io.rollback(current))
            .map_err(|mut error| {
                error.message = format!("语音路由已启动，但安全回滚未通过：{}", error.message);
                PreparationFailure::at(PreparationPhase::SelfTest, error)
            })?;
    }
    current().map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))?;
    let final_boot = io
        .boot_id(current)
        .map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))?;
    if final_boot != boot_id {
        return Err(PreparationFailure::at(
            PreparationPhase::SelfTest,
            ProbeError::new(
                "module_voice_boot_id_changed",
                "模块在语音自检期间重新启动，运行时准备未确认",
            ),
        ));
    }
    current().map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))?;
    Ok(boot_id)
}

fn reconcile_prepare_route(
    io: &mut impl RuntimePreparationIO,
    action: VoiceRouteAction,
    expected: bool,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<bool, ProbeError> {
    current()?;
    let error = match io.route(action, current) {
        Ok(active) if active == expected => return Ok(false),
        Ok(_) => ProbeError::new(
            "module_voice_route_state_unconfirmed",
            "模块语音路由返回了不一致的状态",
        ),
        Err(error) => error,
    };
    current()?;
    match io.route(VoiceRouteAction::Status, current) {
        Ok(active) if active == expected => Ok(true),
        Ok(_) => Err(error),
        Err(status_error) => Err(ProbeError::new(
            error.code,
            format!(
                "{}；路由状态也未确认：{}",
                error.message, status_error.message,
            ),
        )),
    }
}

fn ensure_current_preparation(
    inbox: &crate::message_archive::InboxState,
    producer: &str,
    generation: u64,
) -> Result<(), ProbeError> {
    ensure_current_producer(inbox, producer)?;
    voice_runtime_readiness()
        .lock()
        .map_err(|_| {
            ProbeError::new(
                "module_voice_runtime_state_unavailable",
                "模块语音运行时状态暂不可用",
            )
        })?
        .validate_token(producer, generation)
}

async fn finish_runtime_preparation<C, F>(
    result: Result<String, PreparationFailure>,
    cleanup: C,
    commit: F,
) -> Result<(), PreparationFailure>
where
    C: std::future::Future<Output = Result<(), String>>,
    F: FnOnce(String) -> Result<(), ProbeError>,
{
    let cleanup = cleanup.await;
    let boot_id = match (result, cleanup) {
        (Ok(boot_id), Ok(())) => boot_id,
        (Err(error), Ok(())) => return Err(error),
        (Err(mut error), Err(cleanup)) => {
            error
                .error
                .message
                .push_str(&format!("；USB AT 租约释放未确认：{cleanup}"));
            return Err(error);
        }
        (Ok(_), Err(cleanup)) => {
            return Err(PreparationFailure::at(
                PreparationPhase::SelfTest,
                ProbeError::new(
                    "module_voice_route_cleanup_unconfirmed",
                    format!("USB AT 租约释放未确认：{cleanup}"),
                ),
            ));
        }
    };
    commit(boot_id).map_err(|error| PreparationFailure::at(PreparationPhase::SelfTest, error))
}

async fn execute_runtime_preparation(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    producer: String,
    generation: u64,
    guard: VoiceRouteGuard,
    gate: crate::module_recovery::Gate,
) -> Result<(), PreparationFailure> {
    let authority = Arc::new(gate.admit_operation(&producer).map_err(|_| {
        PreparationFailure::at(
            PreparationPhase::Prepare,
            ProbeError::new(
                crate::module_recovery::CODE,
                crate::module_recovery::MESSAGE,
            ),
        )
    })?);
    let permit = ADB_ADMISSION
        .try_acquire()
        .map_err(|_| PreparationFailure {
            status: StatusCode::CONFLICT,
            phase: PreparationPhase::Prepare,
            error: ProbeError::new("adb_probe_busy", "模块 USB ADB 正由其他操作使用"),
        })?;
    let root = data_root.to_path_buf();
    let task = spawn_guarded_voice_task(Some(guard), async move {
        let _permit = permit;
        ensure_voice_admission(&gate)
            .map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
        if !voice_route_ownership()
            .lock()
            .map_err(|_| {
                PreparationFailure::at(PreparationPhase::Prepare, ownership_unavailable())
            })?
            .obligations
            .is_empty()
        {
            return Err(PreparationFailure::at(
                PreparationPhase::Prepare,
                ProbeError::new(
                    "module_voice_cleanup_pending",
                    "原设备语音清理尚未确认，拒绝新的运行时部署",
                ),
            ));
        }
        ensure_current_preparation(&inbox, &producer, generation)
            .map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
        {
            let mut readiness = voice_runtime_readiness().lock().map_err(|_| {
                PreparationFailure::at(
                    PreparationPhase::Prepare,
                    ProbeError::new(
                        "module_voice_runtime_state_unavailable",
                        "模块语音运行时状态暂不可用",
                    ),
                )
            })?;
            readiness
                .invalidate_prepared(&producer, generation)
                .map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
        }
        let captured = usb_at
            .capture_voice_target(&producer)
            .await
            .map_err(|error| PreparationFailure {
                status: error.public_status(),
                phase: PreparationPhase::Prepare,
                error: ProbeError::new(error.public_code(), error.to_string()),
            })?;
        let physical_target = captured.physical_target().cloned();
        let retained_capture = captured.clone();
        let lease = usb_at
            .acquire_voice_transition_captured_with_right(&producer, captured, &authority.right())
            .await
            .map_err(|error| PreparationFailure {
                status: error.public_status(),
                phase: PreparationPhase::Prepare,
                error: ProbeError::new(error.public_code(), error.to_string()),
            })?;
        let worker_inbox = inbox.clone();
        let worker_producer = producer.clone();
        let worker_gate = gate.clone();
        let result = tokio::task::spawn_blocking(move || {
            ensure_current_preparation(&worker_inbox, &worker_producer, generation)
                .map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
            let mut preparation = NativeRuntimePreparation::new(&root, physical_target.as_ref())
                .map_err(|error| PreparationFailure::at(PreparationPhase::Prepare, error))?;
            preparation.gate = Some(worker_gate.clone());
            preparation.owner = Some((
                worker_producer.clone(),
                generation,
                retained_capture,
                authority.clone(),
            ));
            let result = prepare_runtime_pipeline(&mut preparation, &mut || {
                if !authority
                    .right()
                    .is_active_for(&worker_gate, &worker_producer)
                {
                    return Err(ownership_unavailable());
                }
                ensure_current_preparation(&worker_inbox, &worker_producer, generation)
            });
            Ok((result, preparation.obligation))
        })
        .await
        .unwrap_or_else(|_| {
            Err(PreparationFailure::at(
                PreparationPhase::Prepare,
                ProbeError::new("module_adb_worker_failed", "模块 ADB 工作线程未能确认结果"),
            ))
        });
        let (result, obligation) = match result {
            Ok((result, obligation)) => (result, obligation),
            Err(error) => (Err(error), None),
        };
        let self_test_completed = result.is_ok();
        let release_obligation = obligation.clone();
        finish_runtime_preparation(
            result,
            async {
                lease
                    .release_confirmed()
                    .await
                    .map_err(|error| error.to_string())?;
                if self_test_completed {
                    if let Some(obligation) = release_obligation {
                        release_managed_obligation(obligation, gate.clone(), inbox.clone())
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                }
                Ok(())
            },
            |boot_id| {
                // This callback runs only after the actual self-test stop,
                // final boot check and confirmed USB lease release.
                let obligation = obligation.ok_or_else(ownership_unavailable)?;
                voice_route_ownership()
                    .lock()
                    .map_err(|_| ownership_unavailable())?
                    .retire(&obligation, true);
                ensure_voice_admission(&gate)?;
                ensure_current_preparation(&inbox, &producer, generation)?;
                let mut readiness = voice_runtime_readiness().lock().map_err(|_| {
                    ProbeError::new(
                        "module_voice_runtime_state_unavailable",
                        "模块语音运行时部署完成，但状态提交暂不可用",
                    )
                })?;
                ensure_current_producer(&inbox, &producer)?;
                readiness.mark_prepared(&producer, generation, boot_id)?;
                let mut prepared = obligation.target;
                prepared.managed = None;
                readiness.prepared_target = Some(prepared);
                Ok(())
            },
        )
        .await
    });
    task.await.unwrap_or_else(|_| {
        Err(PreparationFailure::at(
            PreparationPhase::Prepare,
            ProbeError::new("module_adb_worker_failed", "模块 ADB 工作线程未能确认结果"),
        ))
    })
}

#[derive(Default)]
struct VoiceRuntimeReadiness {
    producer_id: String,
    generation: u64,
    boot_id: Option<String>,
    prepared_target: Option<VoiceBoundTarget>,
}

impl VoiceRuntimeReadiness {
    fn advance_next(&mut self, producer_id: &str) -> Result<u64, ProbeError> {
        let generation = if self.producer_id != producer_id {
            1
        } else if self.generation == 0 {
            1
        } else {
            self.generation.checked_add(1).ok_or_else(|| {
                ProbeError::new(
                    "module_voice_runtime_generation_exhausted",
                    "模块语音运行时代次已耗尽",
                )
            })?
        };
        self.producer_id = producer_id.to_owned();
        self.generation = generation;
        self.boot_id = None;
        self.prepared_target = None;
        Ok(generation)
    }

    fn validate_token(&self, producer_id: &str, generation: u64) -> Result<(), ProbeError> {
        if generation == 0 {
            return Err(ProbeError::new(
                "module_voice_runtime_generation_invalid",
                "模块语音运行时代次无效",
            ));
        }
        if self.producer_id != producer_id || generation < self.generation {
            return Err(ProbeError::new(
                "stale_module_voice_runtime_generation",
                "模块语音运行时请求代次已过期",
            ));
        }
        if generation > self.generation {
            return Err(ProbeError::new(
                "unissued_module_voice_runtime_generation",
                "模块语音运行时代次不是 Rust 分配的当前令牌",
            ));
        }
        Ok(())
    }

    fn invalidate_prepared(
        &mut self,
        producer_id: &str,
        generation: u64,
    ) -> Result<(), ProbeError> {
        self.validate_token(producer_id, generation)?;
        self.boot_id = None;
        self.prepared_target = None;
        Ok(())
    }

    fn mark_prepared(
        &mut self,
        producer_id: &str,
        generation: u64,
        boot_id: String,
    ) -> Result<(), ProbeError> {
        if self.producer_id != producer_id || self.generation != generation {
            return Err(ProbeError::new(
                "stale_module_voice_runtime_generation",
                "运行时部署完成时模块语音代次已变化",
            ));
        }
        self.boot_id = Some(boot_id);
        Ok(())
    }

    fn is_prepared(&mut self, producer_id: &str, generation: u64, boot_id: &str) -> bool {
        if self.producer_id != producer_id || self.generation != generation {
            return false;
        }
        if self.boot_id.as_deref() != Some(boot_id) {
            self.boot_id = None;
            self.prepared_target = None;
            return false;
        }
        true
    }
}

// A prepared same-connection target is readiness evidence. An outstanding
// cleanup target is a distinct obligation: invalidating readiness, cancellation
// or observing a new AT connection must never discard or replace it.
#[derive(Clone)]
struct VoiceBoundTarget {
    plan: AdbPlan,
    capture: Option<crate::usb_at::CapturedVoiceTarget>,
    managed: Option<Arc<managed_control::ManagedControl>>,
}
#[derive(Clone)]
struct VoiceCleanupObligation {
    producer: String,
    generation: u64,
    target: VoiceBoundTarget,
    authority: Arc<crate::module_recovery::OperationGuard>,
}

async fn release_managed_obligation(
    obligation: VoiceCleanupObligation,
    gate: crate::module_recovery::Gate,
    inbox: crate::message_archive::InboxState,
) -> Result<(), ProbeError> {
    let Some(control) = obligation.target.managed.clone() else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        let current = || {
            ensure_current_producer(&inbox, &obligation.producer)?;
            if !obligation
                .authority
                .right()
                .is_active_for(&gate, &obligation.producer)
            {
                return Err(ownership_unavailable());
            }
            control.validate()
        };
        current()?;
        let _lock = control.acquire_lock()?;
        current()?;
        let mut locked_current = || {
            current()?;
            _lock.validate().map_err(|_| ownership_unavailable())
        };
        control.release(&obligation.target.plan, &mut locked_current)
    })
    .await
    .map_err(|_| ownership_unavailable())?
}
#[derive(Default)]
struct VoiceRouteOwnership {
    obligations: Vec<VoiceCleanupObligation>,
    observation_revision: u64,
    observation_revision_exhausted: bool,
}
impl VoiceRouteOwnership {
    fn retained(&self, producer: &str) -> Result<Option<VoiceCleanupObligation>, ProbeError> {
        let mut matches = self
            .obligations
            .iter()
            .filter(|entry| entry.producer == producer);
        let first = matches.next().cloned();
        if matches.next().is_some() {
            return Err(ProbeError::new(
                "module_voice_cleanup_ambiguous",
                "尚有多个未确认的原设备清理义务",
            ));
        }
        Ok(first)
    }
    fn final_settlement(&self, producer: &str) -> Result<FinalVoiceSettlement, ProbeError> {
        if self.obligations.is_empty() {
            return Ok(FinalVoiceSettlement::NoOwnedCleanup);
        }
        if self
            .obligations
            .iter()
            .any(|entry| entry.producer != producer)
        {
            return Err(ProbeError::new(
                "module_voice_cleanup_pending",
                "其他后端仍有未确认的原设备语音清理义务",
            ));
        }
        self.retained(producer)?.ok_or_else(ownership_unavailable)?;
        Ok(FinalVoiceSettlement::OwnedCleanup)
    }
    fn begin(&mut self, obligation: VoiceCleanupObligation) -> Result<(), ProbeError> {
        if let Some(existing) = self.retained(&obligation.producer)? {
            if existing.generation == obligation.generation
                && Arc::ptr_eq(&existing.target.plan.0, &obligation.target.plan.0)
            {
                return Ok(());
            }
            return Err(ProbeError::new(
                "module_voice_cleanup_pending",
                "原设备语音清理尚未确认，拒绝替换其设备计划",
            ));
        }
        // Another producer cannot acquire a fresh target while an old route's
        // physical effects remain unknown, even after its readiness was revoked.
        if !self.obligations.is_empty() {
            return Err(ProbeError::new(
                "module_voice_cleanup_pending",
                "旧后端仍有未确认的原设备语音清理义务",
            ));
        }
        self.obligations.push(obligation);
        self.note_observation_change();
        Ok(())
    }
    fn retire(&mut self, obligation: &VoiceCleanupObligation, stopped_and_released: bool) {
        if stopped_and_released {
            let before = self.obligations.len();
            self.obligations.retain(|entry| {
                !(entry.producer == obligation.producer
                    && entry.generation == obligation.generation
                    && Arc::ptr_eq(&entry.target.plan.0, &obligation.target.plan.0))
            });
            if self.obligations.len() != before {
                self.note_observation_change();
            }
        }
    }
    fn note_observation_change(&mut self) {
        if let Some(next) = self.observation_revision.checked_add(1) {
            self.observation_revision = next;
        } else {
            self.observation_revision_exhausted = true;
        }
    }
}
fn voice_route_ownership() -> &'static Mutex<VoiceRouteOwnership> {
    VOICE_ROUTE_OWNERSHIP.get_or_init(|| Mutex::new(VoiceRouteOwnership::default()))
}
fn ownership_unavailable() -> ProbeError {
    ProbeError::new(
        "module_voice_route_obligation_unavailable",
        "无法确认原设备语音清理义务",
    )
}
fn retained_voice_obligation(producer: &str) -> Result<Option<VoiceCleanupObligation>, ProbeError> {
    voice_route_ownership()
        .lock()
        .map_err(|_| ownership_unavailable())?
        .retained(producer)
}
/// Metadata only; the durable HTTP receipt binds to this exact native runtime
/// generation and boot observation as well as the physical radio session.
pub(crate) fn public_runtime_binding() -> String {
    let state = voice_runtime_readiness()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    format!(
        "{}:{}:{:?}",
        state.producer_id, state.generation, state.boot_id
    )
}
fn prepared_voice_target(producer: &str) -> Result<(u64, Option<VoiceBoundTarget>), ProbeError> {
    let state = voice_runtime_readiness()
        .lock()
        .map_err(|_| ownership_unavailable())?;
    if state.producer_id != producer {
        return Ok((0, None));
    }
    Ok((state.generation, state.prepared_target.clone()))
}
fn ensure_voice_admission(gate: &crate::module_recovery::Gate) -> Result<(), ProbeError> {
    if gate.is_admission_revoked() {
        Err(ProbeError::new(
            crate::module_recovery::CODE,
            crate::module_recovery::MESSAGE,
        ))
    } else {
        Ok(())
    }
}

fn voice_runtime_readiness() -> &'static std::sync::Mutex<VoiceRuntimeReadiness> {
    VOICE_RUNTIME_READINESS.get_or_init(|| std::sync::Mutex::new(VoiceRuntimeReadiness::default()))
}

/// Runs the isolated ADB readiness probe directly for Rust-owned public
/// routes. The permit follows the blocking child supervisor, including when
/// the awaiting request is cancelled.
pub(crate) async fn probe_module_adb(
    module_recovery: &crate::module_recovery::Gate,
    data_root: &Path,
    producer_id: &str,
    inbox: crate::message_archive::InboxState,
) -> Result<(), ProbeError> {
    if module_recovery.is_admission_revoked() {
        return Err(ProbeError::new(
            crate::module_recovery::CODE,
            crate::module_recovery::MESSAGE,
        ));
    }
    ensure_current_producer(&inbox, producer_id)?;
    let authority = module_recovery.admit_operation(producer_id).map_err(|_| {
        ProbeError::new(
            crate::module_recovery::CODE,
            crate::module_recovery::MESSAGE,
        )
    })?;
    let gate = module_recovery.clone();
    let permit = ADB_ADMISSION
        .try_acquire()
        .map_err(|_| ProbeError::new("adb_probe_busy", "模块 USB ADB 正由其他操作使用"))?;
    let executable = std::env::current_exe()
        .map_err(|_| ProbeError::new("adb_probe_unavailable", "无法定位隔离 ADB 探测程序"))?;
    let worker_root = data_root.to_path_buf();
    let observation = ProbeObservation::begin(data_root, producer_id);
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _authority = authority;
        ensure_voice_admission(&gate)?;
        supervise_probe_child(&executable, &worker_root, observation)
    })
    .await
    .map_err(|_| ProbeError::new("module_adb_worker_failed", "模块 ADB 工作线程未能确认结果"))?;
    result?;
    ensure_voice_admission(module_recovery)?;
    ensure_current_producer(&inbox, producer_id)
}

/// Native lifecycle entry points reuse the same fenced ADB workers as the
/// private compatibility RPC. Their caller owns the radio reservation until
/// these workers and their confirmed USB cleanup have actually completed.
pub(crate) fn invalidate_native_runtime(
    inbox: &crate::message_archive::InboxState,
    producer: &str,
) -> Result<u64, ProbeError> {
    ensure_current_producer(inbox, producer)?;
    voice_runtime_readiness()
        .lock()
        .map_err(|_| {
            ProbeError::new(
                "module_voice_runtime_state_unavailable",
                "模块语音运行时状态暂不可用",
            )
        })?
        .advance_next(producer)
}

pub(crate) async fn prepare_native_runtime(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: &crate::module_radio::Service,
    reservation: &crate::module_radio::CallModeReservation,
) -> Result<(), (String, String)> {
    if radio.module_recovery().is_admission_revoked() {
        return Err((
            crate::module_recovery::CODE.into(),
            crate::module_recovery::MESSAGE.into(),
        ));
    }
    let generation = invalidate_native_runtime(&inbox, &reservation.producer_id)
        .map_err(|error| ("模块侧语音运行时准备失败".into(), error.to_string()))?;
    let guard = radio
        .borrow_call_mode_operation_voice(reservation)
        .map_err(|error| {
            (
                "模块侧语音运行时准备失败".into(),
                error.public_message().to_owned(),
            )
        })?;
    execute_runtime_preparation(
        data_root,
        inbox,
        usb_at,
        reservation.producer_id.clone(),
        generation,
        VoiceRouteGuard::call_mode(guard),
        radio.module_recovery().clone(),
    )
    .await
    .map_err(|error| {
        let summary = match error.phase {
            PreparationPhase::Prepare => "模块侧语音运行时准备失败",
            PreparationPhase::SelfTest => "模块语音路由自检失败",
        };
        (summary.into(), error.error.to_string())
    })
}

pub(crate) async fn stop_native_hangup(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    producer: String,
) -> Result<(), String> {
    execute_voice_route_action_with_policy(
        data_root,
        inbox,
        usb_at,
        radio,
        producer,
        VoiceRouteAction::Stop,
        StopPolicy::Hangup,
        None,
    )
    .await
    .map(|_| ())
    .map_err(|error| error.failure.message)
}

pub(crate) async fn stop_native_runtime(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    producer: String,
    restore: Option<&crate::module_radio::CallModeReservation>,
) -> Result<(), (String, String)> {
    let (policy, guard) = match restore {
        Some(reservation) => {
            let retained = retained_voice_obligation(&producer)
                .map_err(|error| ("无法停止当前通话音频路由".into(), error.to_string()))?;
            let guard = match retained {
                // Only a real outstanding exact-target obligation supplies
                // this narrow unknown-outcome settlement authority.
                Some(obligation) => radio.borrow_call_mode_operation_voice_settlement(
                    reservation,
                    &obligation.authority.right(),
                ),
                None => radio.borrow_call_mode_operation_voice(reservation),
            }
            .map_err(|error| {
                (
                    "无法停止当前通话音频路由".into(),
                    error.public_message().to_owned(),
                )
            })?;
            (StopPolicy::Restore, Some(VoiceRouteGuard::call_mode(guard)))
        }
        None => (StopPolicy::Shutdown, None),
    };
    execute_voice_route_action_with_policy(
        data_root,
        inbox,
        usb_at,
        radio,
        producer,
        VoiceRouteAction::Stop,
        policy,
        guard,
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        let summary = if error.stop_phase == Some(StopPhase::Status) {
            "无法确认当前通话音频路由状态"
        } else {
            "无法停止当前通话音频路由"
        };
        (summary.into(), error.failure.message)
    })
}

/// A no-change restore still owes this process's accepted voice cleanup. An
/// empty registry needs no ADB/USB discovery, including a prepared-only plan.
/// Retain this owner across caller cancellation until exact settlement ends.
pub(crate) async fn settle_owned_native_runtime_for_restore(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    producer: String,
    restore: &crate::module_radio::CallModeReservation,
) -> Result<(), (String, String)> {
    let root = data_root.to_path_buf();
    let restore = restore.clone();
    retain_restore_voice_cleanup(async move {
        if restore.producer_id != producer {
            return Err((
                "无法停止当前通话音频路由".into(),
                "还原预约与原设备清理义务所属后端不一致".into(),
            ));
        }
        let inspect_producer = producer.clone();
        finish_restore_voice_cleanup(
            &ADB_ADMISSION,
            move || {
                let ownership = voice_route_ownership()
                    .lock()
                    .map_err(|_| ownership_unavailable())?;
                restore_voice_cleanup_obligation(&ownership, &inspect_producer)
            },
            |permit, obligation| async move {
                let guard = radio
                    .borrow_call_mode_operation_voice_settlement(
                        &restore,
                        &obligation.authority.right(),
                    )
                    .map_err(|error| RouteExecutionFailure {
                        stop_confirmed: false,
                        failure: VoiceRouteFailure {
                            status: error.status,
                            code: error.code,
                            message: error.message,
                        },
                        stop_phase: Some(StopPhase::Status),
                    })?;
                execute_voice_route_action_with_admission(
                    &root,
                    inbox,
                    usb_at,
                    radio,
                    producer,
                    VoiceRouteAction::Stop,
                    StopPolicy::Restore,
                    Some(VoiceRouteGuard::call_mode(guard)),
                    VoiceAdmission::Immediate,
                    Some(permit),
                )
                .await
            },
        )
        .await
    })
    .await
}

async fn retain_restore_voice_cleanup(
    work: impl std::future::Future<Output = Result<(), (String, String)>> + Send + 'static,
) -> Result<(), (String, String)> {
    tokio::spawn(work).await.unwrap_or_else(|_| {
        Err((
            "无法停止当前通话音频路由".into(),
            "模块 ADB 工作线程未能确认结果".into(),
        ))
    })
}

fn restore_voice_cleanup_obligation(
    ownership: &VoiceRouteOwnership,
    producer: &str,
) -> Result<Option<VoiceCleanupObligation>, ProbeError> {
    match ownership.final_settlement(producer)? {
        FinalVoiceSettlement::NoOwnedCleanup => Ok(None),
        FinalVoiceSettlement::OwnedCleanup => ownership
            .retained(producer)?
            .map(Some)
            .ok_or_else(ownership_unavailable),
    }
}

async fn finish_restore_voice_cleanup<'a, F, R>(
    semaphore: &'a Semaphore,
    inspect: impl FnOnce() -> Result<Option<VoiceCleanupObligation>, ProbeError>,
    settle: F,
) -> Result<(), (String, String)>
where
    F: FnOnce(tokio::sync::SemaphorePermit<'a>, VoiceCleanupObligation) -> R,
    R: std::future::Future<Output = Result<bool, RouteExecutionFailure>>,
{
    // Accepted workers can publish an obligation immediately before dropping
    // admission. Inspect the complete registry only after that publication.
    let permit = VoiceAdmission::FinalShutdown
        .acquire_adb(semaphore)
        .await
        .map_err(|()| {
            (
                "无法停止当前通话音频路由".into(),
                "无法排空已接纳的模块 ADB 工作".into(),
            )
        })?;
    let obligation =
        inspect().map_err(|error| ("无法停止当前通话音频路由".into(), error.to_string()))?;
    let Some(obligation) = obligation else {
        return Ok(());
    };
    match settle(permit, obligation).await {
        Ok(false) => Ok(()),
        Ok(true) => Err((
            "无法停止当前通话音频路由".into(),
            "模块语音运行时仍处于活动状态".into(),
        )),
        Err(error) => {
            let summary = if error.stop_phase == Some(StopPhase::Status) {
                "无法确认当前通话音频路由状态"
            } else {
                "无法停止当前通话音频路由"
            };
            Err((summary.into(), error.failure.message))
        }
    }
}

/// Final process cleanup only: reporting errors may survive a confirmed stop.
/// Unknown stop, generation, or USB cleanup never authorizes transport close.
pub(crate) async fn stop_native_runtime_shutdown_receipt(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    radio: crate::module_radio::Service,
    producer: String,
) -> Result<Vec<String>, String> {
    let operation_guard = final_voice_route_guard(data_root).await?;
    // Every accepted ADB worker owns admission before it can publish a route
    // obligation. Drain them before deciding that this process owes no route
    // cleanup; keep admission throughout the resulting exact settlement.
    let inspect_producer = producer.clone();
    finish_final_voice_cleanup(
        &ADB_ADMISSION,
        move || {
            voice_route_ownership()
                .lock()
                .map_err(|_| ownership_unavailable().to_string())?
                .final_settlement(&inspect_producer)
                .map_err(|error| error.to_string())
        },
        |permit| async move {
            shutdown_route_receipt(
                execute_voice_route_action_with_admission(
                    data_root,
                    inbox,
                    usb_at,
                    radio,
                    producer,
                    VoiceRouteAction::Stop,
                    StopPolicy::Shutdown,
                    Some(operation_guard),
                    VoiceAdmission::FinalShutdown,
                    Some(permit),
                )
                .await,
            )
        },
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalVoiceSettlement {
    NoOwnedCleanup,
    OwnedCleanup,
}

async fn drain_final_voice_cleanup<'a>(
    semaphore: &'a Semaphore,
    inspect: impl FnOnce() -> Result<FinalVoiceSettlement, String>,
) -> Result<(tokio::sync::SemaphorePermit<'a>, FinalVoiceSettlement), String> {
    let permit = VoiceAdmission::FinalShutdown
        .acquire_adb(semaphore)
        .await
        .map_err(|()| "module voice ADB admission unavailable during shutdown".to_owned())?;
    let settlement = inspect()?;
    Ok((permit, settlement))
}

async fn finish_final_voice_cleanup<'a, F, R>(
    semaphore: &'a Semaphore,
    inspect: impl FnOnce() -> Result<FinalVoiceSettlement, String>,
    settle: F,
) -> Result<Vec<String>, String>
where
    F: FnOnce(tokio::sync::SemaphorePermit<'a>) -> R,
    R: std::future::Future<Output = Result<Vec<String>, String>>,
{
    let (permit, settlement) = drain_final_voice_cleanup(semaphore, inspect).await?;
    match settlement {
        // A prepared target already passed its self-test Stop and USB release.
        // External/legacy evidence is not this process's mutation obligation.
        FinalVoiceSettlement::NoOwnedCleanup => Ok(Vec::new()),
        FinalVoiceSettlement::OwnedCleanup => settle(permit).await,
    }
}

/// A busy file guard is affirmative pre-dispatch contention, not an unknown
/// stop. Wait without submitting commands or weakening its exclusive owner.
async fn final_voice_route_guard(data_root: &Path) -> Result<VoiceRouteGuard, String> {
    loop {
        match crate::call_mode_backups::try_call_mode_operation_shared_lock(data_root) {
            Ok(lock) => return Ok(VoiceRouteGuard::shared(lock)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(format!("无法确认模块通话模式操作状态：{error}")),
        }
    }
}

fn shutdown_route_receipt(
    result: Result<bool, RouteExecutionFailure>,
) -> Result<Vec<String>, String> {
    match result {
        Ok(false) => Ok(Vec::new()),
        Ok(true) => Err("module voice runtime still active".into()),
        Err(error) if error.stop_confirmed => Ok(vec![error.failure.message]),
        Err(error) => Err(error.failure.message),
    }
}

/// Checks the runtime's module boot ID and Rust-owned preparation fence.
/// Errors intentionally collapse to `false`, matching the former Go boolean
/// readiness adapter.
pub(crate) async fn is_module_voice_runtime_prepared(
    module_recovery: &crate::module_recovery::Gate,
    data_root: &Path,
    producer_id: &str,
    inbox: crate::message_archive::InboxState,
) -> bool {
    if module_recovery.is_admission_revoked()
        || ensure_current_producer(&inbox, producer_id).is_err()
    {
        return false;
    }
    let generation = {
        let Ok(mut readiness) = voice_runtime_readiness().lock() else {
            return false;
        };
        if readiness.producer_id != producer_id || readiness.generation == 0 {
            match readiness.advance_next(producer_id) {
                Ok(generation) => generation,
                Err(_) => return false,
            }
        } else {
            readiness.generation
        }
    };
    let Ok(authority) = module_recovery.admit_operation(producer_id) else {
        return false;
    };
    let gate = module_recovery.clone();
    let Ok(permit) = ADB_ADMISSION.try_acquire() else {
        return false;
    };
    let worker_root = data_root.to_path_buf();
    let worker_producer = producer_id.to_owned();
    let worker_inbox = inbox.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _authority = authority;
        ensure_voice_admission(&gate)?;
        module_voice_runtime_boot_id(&worker_root, &worker_producer, &worker_inbox)
    })
    .await;
    let Ok(Ok(boot_id)) = result else {
        return false;
    };
    if module_recovery.is_admission_revoked()
        || ensure_current_producer(&inbox, producer_id).is_err()
    {
        return false;
    }
    voice_runtime_readiness()
        .lock()
        .map(|mut readiness| readiness.is_prepared(producer_id, generation, &boot_id))
        .unwrap_or(false)
}

pub(crate) async fn handle_internal(
    request: Request<Incoming>,
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    module_radio: crate::module_radio::Service,
) -> Response<Full<Bytes>> {
    if module_radio.module_recovery().is_quarantined() {
        return crate::module_recovery::rejection();
    }
    let path = request.uri().path();
    let probe = path == INTERNAL_PROBE_PATH;
    let deploy = path == INTERNAL_DEPLOY_PATH;
    let runtime_status = path == INTERNAL_RUNTIME_STATUS_PATH;
    let runtime_generation_advance = path == INTERNAL_RUNTIME_GENERATION_PATH;
    let route_action = match path {
        INTERNAL_ROUTE_START_PATH => Some(VoiceRouteAction::Start),
        INTERNAL_ROUTE_STOP_PATH => Some(VoiceRouteAction::Stop),
        INTERNAL_ROUTE_STATUS_PATH => Some(VoiceRouteAction::Status),
        _ => None,
    };
    let stop_policy = StopPolicy::Always;
    if module_radio.module_recovery().is_admission_revoked()
        && !matches!(
            route_action,
            Some(VoiceRouteAction::Stop | VoiceRouteAction::Status)
        )
    {
        return crate::module_recovery::rejection();
    }
    if request.method() != Method::POST
        || (!probe
            && !deploy
            && !runtime_status
            && !runtime_generation_advance
            && route_action.is_none())
        || request.uri().query().is_some()
    {
        return route_request_response(
            route_action,
            stop_policy,
            StatusCode::NOT_FOUND,
            "route_not_found",
            false,
            "route not found",
        );
    }
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        MAX_REQUEST_BYTES,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::BAD_REQUEST,
                "invalid_request",
                false,
                if deploy {
                    "ADB 部署请求无效或超出大小限制"
                } else if route_action.is_some() {
                    "模块语音路由请求无效或超出大小限制"
                } else if runtime_status {
                    "模块语音运行时状态请求无效或超出大小限制"
                } else if runtime_generation_advance {
                    "模块语音运行时代次请求无效或超出大小限制"
                } else {
                    "ADB 探测请求无效或超出大小限制"
                },
            );
        }
        Err(crate::http_server::BodyReadError::Timeout) => {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                false,
                if deploy {
                    "ADB 部署请求读取超时"
                } else if route_action.is_some() {
                    "模块语音路由请求读取超时"
                } else if runtime_status {
                    "模块语音运行时状态请求读取超时"
                } else if runtime_generation_advance {
                    "模块语音运行时代次请求读取超时"
                } else {
                    "ADB 探测请求读取超时"
                },
            );
        }
    };
    let input = match serde_json::from_slice::<ProbeRequest>(&body) {
        Ok(input) if valid_producer(&input.producer_id) => input,
        _ => {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::BAD_REQUEST,
                "invalid_request",
                false,
                if deploy {
                    "ADB 部署需要有效的后端代次标识"
                } else if route_action.is_some() {
                    "模块语音路由操作需要有效的后端代次标识"
                } else if runtime_status {
                    "模块语音运行时状态需要有效的后端代次标识"
                } else if runtime_generation_advance {
                    "模块语音运行时代次请求需要有效的后端代次标识"
                } else {
                    "ADB 探测需要有效的后端代次标识"
                },
            );
        }
    };
    let stop_policy = match validate_stop_policy(&input, route_action) {
        Ok(policy) => policy,
        Err(error) => {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::BAD_REQUEST,
                error.code,
                false,
                error.message,
            );
        }
    };
    let credentials = match call_mode_credentials(
        &input,
        deploy
            || matches!(route_action, Some(VoiceRouteAction::Start))
            || (matches!(route_action, Some(VoiceRouteAction::Stop))
                && stop_policy == StopPolicy::Restore),
    ) {
        Ok(credentials) => credentials,
        Err(error) => {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::BAD_REQUEST,
                error.code,
                false,
                error.message,
            );
        }
    };
    if !inbox.owns_current_producer(&input.producer_id)
        || !inbox.owns_module_producer(&input.producer_id)
    {
        return route_request_response(
            route_action,
            stop_policy,
            StatusCode::CONFLICT,
            "stale_backend_generation",
            false,
            if deploy {
                "后端代次已变化；拒绝启动过期 ADB 部署"
            } else if route_action.is_some() {
                "后端代次已变化；拒绝过期模块语音路由操作"
            } else if runtime_status {
                "后端代次已变化；拒绝过期模块语音运行时状态请求"
            } else if runtime_generation_advance {
                "后端代次已变化；拒绝过期模块语音运行时代次请求"
            } else {
                "后端代次已变化；拒绝启动过期 ADB 探测"
            },
        );
    }
    if runtime_generation_advance {
        if input.runtime_generation.is_some() {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::BAD_REQUEST,
                "module_voice_runtime_generation_unexpected",
                false,
                "代次分配请求不得指定运行时代次",
            );
        }
        let mut readiness = match voice_runtime_readiness().lock() {
            Ok(readiness) => readiness,
            Err(_) => {
                return generation_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "module_voice_runtime_state_unavailable",
                    false,
                    "模块语音运行时状态暂不可用",
                    0,
                );
            }
        };
        return match readiness.advance_next(&input.producer_id) {
            Ok(generation) => generation_response(
                StatusCode::OK,
                "module_voice_runtime_generation_advanced",
                true,
                "",
                generation,
            ),
            Err(error) => generation_response(
                StatusCode::CONFLICT,
                error.code,
                false,
                error.to_string(),
                0,
            ),
        };
    }
    let runtime_generation = if deploy || runtime_status {
        match input.runtime_generation {
            Some(generation) if generation > 0 => {
                let readiness = match voice_runtime_readiness().lock() {
                    Ok(readiness) => readiness,
                    Err(_) => {
                        return route_request_response(
                            route_action,
                            stop_policy,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "module_voice_runtime_state_unavailable",
                            false,
                            "模块语音运行时状态暂不可用",
                        );
                    }
                };
                if let Err(error) = readiness.validate_token(&input.producer_id, generation) {
                    return route_request_response(
                        route_action,
                        stop_policy,
                        StatusCode::CONFLICT,
                        error.code,
                        false,
                        error.to_string(),
                    );
                }
                Some(generation)
            }
            _ => {
                return route_request_response(
                    route_action,
                    stop_policy,
                    StatusCode::BAD_REQUEST,
                    "module_voice_runtime_generation_invalid",
                    false,
                    "模块语音运行时请求需要正数代次",
                );
            }
        }
    } else {
        if input.runtime_generation.is_some() {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::BAD_REQUEST,
                "module_voice_runtime_generation_unexpected",
                false,
                "该 ADB 操作不接受模块语音运行时代次",
            );
        }
        None
    };
    if deploy {
        let (token, sequence) = credentials.expect("prepare requires call-mode credentials");
        let guard = match module_radio.borrow_call_mode_voice_operation(
            &input.producer_id,
            token,
            sequence,
        ) {
            Ok(guard) => VoiceRouteGuard::call_mode(guard),
            Err(error) => {
                return preparation_failure_response(PreparationFailure {
                    status: error.status,
                    error: ProbeError::new(error.code, error.message),
                    phase: PreparationPhase::Prepare,
                });
            }
        };
        return match execute_runtime_preparation(
            data_root,
            inbox,
            usb_at,
            input.producer_id,
            runtime_generation.expect("prepare requires runtime generation"),
            guard,
            module_radio.module_recovery().clone(),
        )
        .await
        {
            Ok(()) => runtime_readiness_response(
                StatusCode::OK,
                "module_voice_runtime_prepared",
                true,
                "",
                true,
            ),
            Err(error) => preparation_failure_response(error),
        };
    }
    if let Some(action) = route_action {
        let operation_guard = match credentials {
            Some((token, sequence)) => {
                match module_radio.borrow_call_mode_voice_operation(
                    &input.producer_id,
                    token,
                    sequence,
                ) {
                    Ok(guard) => Some(VoiceRouteGuard::call_mode(guard)),
                    Err(error) => {
                        return route_request_response(
                            route_action,
                            stop_policy,
                            error.status,
                            error.code,
                            false,
                            error.message,
                        );
                    }
                }
            }
            None => None,
        };
        return match execute_voice_route_action_with_policy(
            data_root,
            inbox,
            usb_at,
            module_radio.clone(),
            input.producer_id,
            action,
            stop_policy,
            operation_guard,
        )
        .await
        {
            Ok(active) => voice_route_response(
                StatusCode::OK,
                match action {
                    VoiceRouteAction::Start => "module_voice_route_started",
                    VoiceRouteAction::Stop => "module_voice_route_stopped",
                    VoiceRouteAction::Status if active => "module_voice_route_active",
                    VoiceRouteAction::Status => "module_voice_route_stopped",
                },
                true,
                "",
                active,
            ),
            Err(error) => voice_route_failure_response(error),
        };
    }
    let authority = match module_radio
        .module_recovery()
        .admit_operation(&input.producer_id)
    {
        Ok(authority) => authority,
        Err(()) => return crate::module_recovery::rejection(),
    };
    let gate = module_radio.module_recovery().clone();
    let permit = match ADB_ADMISSION.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            return route_request_response(
                route_action,
                stop_policy,
                StatusCode::CONFLICT,
                "adb_probe_busy",
                false,
                "模块 USB ADB 正由其他操作使用",
            );
        }
    };
    let executable = if probe {
        match std::env::current_exe() {
            Ok(executable) => Some(executable),
            Err(_) => {
                return route_request_response(
                    route_action,
                    stop_policy,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "adb_probe_unavailable",
                    false,
                    "无法定位隔离 ADB 探测程序",
                );
            }
        }
    } else {
        None
    };
    let producer_id = input.producer_id;
    let producer_for_response = producer_id.clone();
    let runtime_generation_for_worker = runtime_generation;
    let inbox_for_worker = inbox.clone();
    let data_root = data_root.to_path_buf();
    let (pending_sender, mut pending_receiver) = tokio::sync::watch::channel(None);
    let observation = executable.as_ref().map(|_| {
        ProbeObservation::begin_with_command(&data_root, &producer_id, Some(pending_sender))
    });
    let mut worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _authority = authority;
        ensure_voice_admission(&gate)?;
        if runtime_status {
            module_voice_runtime_boot_id(&data_root, &producer_id, &inbox_for_worker)
                .map(|boot_id| WorkerResponse::RuntimeStatus { boot_id })
        } else if let Some(executable) = executable {
            supervise_probe_child(
                &executable,
                &data_root,
                observation.expect("probe observation admitted"),
            )
            .map(|()| WorkerResponse::Ready("module_adb_root_ready"))
        } else {
            Err(ProbeError::new(
                "module_adb_probe_unavailable",
                "无法定位隔离 ADB 探测程序",
            ))
        }
    });
    let result = if probe {
        tokio::select! {
            result = &mut worker => result,
            changed = pending_receiver.changed() => {
                let pending = if changed.is_ok() { pending_receiver.borrow().clone() } else { None };
                if let Some(pending) = pending {
                    // Only observer delivery ends here. The original worker
                    // owns the permit/authority and the same unreaped Child.
                    return route_request_response(route_action, stop_policy,
                        StatusCode::SERVICE_UNAVAILABLE, pending.code, false, pending.message);
                }
                worker.await
            }
        }
    } else {
        worker.await
    };
    if module_radio.module_recovery().is_admission_revoked() {
        return crate::module_recovery::rejection();
    }
    match result {
        Ok(Ok(WorkerResponse::Ready(code))) => {
            route_request_response(route_action, stop_policy, StatusCode::OK, code, true, "")
        }
        Ok(Ok(WorkerResponse::RuntimeStatus { boot_id })) => {
            if !inbox.owns_current_producer(&producer_for_response)
                || !inbox.owns_module_producer(&producer_for_response)
            {
                return runtime_readiness_response(
                    StatusCode::CONFLICT,
                    "stale_backend_generation",
                    false,
                    "后端代次已变化；拒绝发布过期运行时状态",
                    false,
                );
            }
            let mut readiness = match voice_runtime_readiness().lock() {
                Ok(readiness) => readiness,
                Err(_) => {
                    return runtime_readiness_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "module_voice_runtime_state_unavailable",
                        false,
                        "模块语音运行时状态暂不可用",
                        false,
                    );
                }
            };
            let prepared = readiness.is_prepared(
                &producer_for_response,
                runtime_generation_for_worker.expect("runtime status route requires generation"),
                &boot_id,
            );
            runtime_readiness_response(
                StatusCode::OK,
                if prepared {
                    "module_voice_runtime_prepared"
                } else {
                    "module_voice_runtime_unprepared"
                },
                true,
                "",
                prepared,
            )
        }
        Ok(Err(error)) => {
            let status = if error.code == "module_adb_probe_timeout"
                || error.code == "module_voice_route_timeout"
            {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::FAILED_DEPENDENCY
            };
            route_request_response(
                route_action,
                stop_policy,
                status,
                error.code,
                false,
                error.message,
            )
        }
        Err(_) => route_request_response(
            route_action,
            stop_policy,
            StatusCode::INTERNAL_SERVER_ERROR,
            "module_adb_worker_failed",
            false,
            "模块 ADB 工作线程未能确认结果",
        ),
    }
}

pub(crate) async fn execute_voice_route_action(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    module_radio: crate::module_radio::Service,
    producer_id: String,
    action: VoiceRouteAction,
    operation_lock: Option<VoiceRouteGuard>,
) -> Result<bool, VoiceRouteFailure> {
    execute_voice_route_action_with_policy(
        data_root,
        inbox,
        usb_at,
        module_radio,
        producer_id,
        action,
        StopPolicy::Always,
        operation_lock,
    )
    .await
    .map_err(|error| error.failure)
}

async fn execute_voice_route_action_with_policy(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    module_radio: crate::module_radio::Service,
    producer_id: String,
    action: VoiceRouteAction,
    stop_policy: StopPolicy,
    operation_lock: Option<VoiceRouteGuard>,
) -> Result<bool, RouteExecutionFailure> {
    execute_voice_route_action_with_admission(
        data_root,
        inbox,
        usb_at,
        module_radio,
        producer_id,
        action,
        stop_policy,
        operation_lock,
        VoiceAdmission::Immediate,
        None,
    )
    .await
}

#[derive(Clone, Copy)]
enum VoiceAdmission {
    Immediate,
    FinalShutdown,
}

impl VoiceAdmission {
    async fn acquire_adb(
        self,
        semaphore: &Semaphore,
    ) -> Result<tokio::sync::SemaphorePermit<'_>, ()> {
        match self {
            Self::Immediate => semaphore.try_acquire().map_err(|_| ()),
            Self::FinalShutdown => semaphore.acquire().await.map_err(|_| ()),
        }
    }
}

async fn execute_voice_route_action_with_admission(
    data_root: &Path,
    inbox: crate::message_archive::InboxState,
    usb_at: crate::usb_at::Owner,
    module_radio: crate::module_radio::Service,
    producer_id: String,
    action: VoiceRouteAction,
    stop_policy: StopPolicy,
    operation_lock: Option<VoiceRouteGuard>,
    admission: VoiceAdmission,
    pre_admitted: Option<tokio::sync::SemaphorePermit<'static>>,
) -> Result<bool, RouteExecutionFailure> {
    if module_radio.module_recovery().is_quarantined() {
        return Err(RouteExecutionFailure {
            stop_confirmed: false,
            failure: VoiceRouteFailure {
                status: StatusCode::CONFLICT,
                code: crate::module_recovery::CODE,
                message: crate::module_recovery::MESSAGE.into(),
            },
            stop_phase: None,
        });
    }
    let stop_phase =
        matches!(action, VoiceRouteAction::Stop).then_some(if stop_policy == StopPolicy::Restore {
            StopPhase::Status
        } else {
            StopPhase::Stop
        });
    let gate = module_radio.module_recovery().clone();
    let retained = retained_voice_obligation(&producer_id)
        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
    if matches!(action, VoiceRouteAction::Stop) && retained.is_none() {
        return Err(RouteExecutionFailure::probe(
            ProbeError::new(
                "module_voice_cleanup_owner_unknown",
                "没有本进程原始语音清理义务，未访问ADB或提交PID信号",
            ),
            stop_phase,
        ));
    }
    let cleanup_obligation = if matches!(action, VoiceRouteAction::Stop | VoiceRouteAction::Status)
    {
        retained.clone()
    } else {
        None
    };
    let authority = match &cleanup_obligation {
        Some(obligation)
            if obligation
                .authority
                .right()
                .is_active_for(&gate, &producer_id) =>
        {
            obligation.authority.clone()
        }
        Some(_) => {
            return Err(RouteExecutionFailure::probe(
                ownership_unavailable(),
                stop_phase,
            ));
        }
        None => Arc::new(gate.admit_operation(&producer_id).map_err(|_| {
            RouteExecutionFailure::probe(
                ProbeError::new(
                    crate::module_recovery::CODE,
                    crate::module_recovery::MESSAGE,
                ),
                stop_phase,
            )
        })?),
    };
    // An authenticated RELEASE and checked native handle close already happened.
    // Retry only its immutable local receipt, before any current-device selection,
    // radio borrow, path lock creation, USB lease or ADB request.
    if matches!(action, VoiceRouteAction::Stop) {
        if let Some(original) = retained.as_ref() {
            if let Some(control) = &original.target.managed {
                if let Some(local) = control
                    .local_completion()
                    .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?
                {
                    let permit = match pre_admitted {
                        Some(permit) => permit,
                        None => admission.acquire_adb(&ADB_ADMISSION).await.map_err(|_| {
                            RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                        })?,
                    };
                    let original = original.clone();
                    let gate = gate.clone();
                    let task = spawn_guarded_voice_task(operation_lock, async move {
                        let _permit = permit;
                        let worker_authority = original.authority.clone();
                        let producer = original.producer.clone();
                        tokio::task::spawn_blocking(move || {
                            if !worker_authority.right().is_active_for(&gate, &producer) {
                                return Err(ownership_unavailable());
                            }
                            local.persist()?;
                            if !worker_authority.right().is_active_for(&gate, &producer) {
                                return Err(ownership_unavailable());
                            }
                            Ok(())
                        })
                        .await
                        .map_err(|_| {
                            RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                        })?
                        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                        voice_route_ownership()
                            .lock()
                            .map_err(|_| {
                                RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                            })?
                            .retire(&original, true);
                        Ok(false)
                    });
                    return task.await.map_err(|_| {
                        RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                    })?;
                }
            }
        }
    }
    let mut operation_lock =
        voice_route_guard(data_root, action, operation_lock).map_err(|failure| {
            RouteExecutionFailure {
                stop_confirmed: false,
                failure,
                stop_phase,
            }
        })?;
    if operation_lock
        .as_ref()
        .is_none_or(|guard| guard._call_mode.is_none())
    {
        let pin = if cleanup_obligation.is_some() {
            match admission {
                VoiceAdmission::Immediate => {
                    module_radio.borrow_shared_voice_settlement(&producer_id, &authority.right())
                }
                VoiceAdmission::FinalShutdown => {
                    module_radio
                        .borrow_shared_voice_shutdown_settlement(&producer_id, &authority.right())
                        .await
                }
            }
        } else {
            match admission {
                // Without a retained original obligation, Status is a new
                // operation. It cannot manufacture cleanup admission during
                // stopping and later select an unbound current ADB target.
                VoiceAdmission::Immediate => {
                    module_radio.borrow_shared_voice_operation(&producer_id, false)
                }
                VoiceAdmission::FinalShutdown => {
                    module_radio
                        .borrow_shared_voice_shutdown(&producer_id)
                        .await
                }
            }
        }
        .map_err(|error| RouteExecutionFailure {
            stop_confirmed: false,
            failure: VoiceRouteFailure {
                status: error.status,
                code: error.code,
                message: error.message,
            },
            stop_phase,
        })?;
        let guard = operation_lock.get_or_insert(VoiceRouteGuard {
            _shared: None,
            _call_mode: None,
            _radio_shared: None,
        });
        guard._radio_shared = Some(pin);
    }
    ensure_current_producer(&inbox, &producer_id)
        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
    if matches!(action, VoiceRouteAction::Start) {
        crate::call_mode_lifecycle::invalidate_pristine_marker(data_root).map_err(|_| {
            RouteExecutionFailure {
                stop_confirmed: false,
                failure: VoiceRouteFailure {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    code: "module_voice_route_obligation_unavailable",
                    message: "无法记录模块语音路由操作，未提交设备修改".into(),
                },
                stop_phase,
            }
        })?;
    }
    let permit = match pre_admitted {
        Some(permit) => permit,
        None => admission
            .acquire_adb(&ADB_ADMISSION)
            .await
            .map_err(|_| RouteExecutionFailure {
                stop_confirmed: false,
                failure: VoiceRouteFailure::from_probe(
                    StatusCode::CONFLICT,
                    ProbeError::new("adb_probe_busy", "模块 USB ADB 正由其他操作使用"),
                ),
                stop_phase,
            })?,
    };
    let root = data_root.to_path_buf();
    let worker = spawn_guarded_voice_task(operation_lock, async move {
        // Keep admission until obligation retirement and publication complete,
        // so final drain cannot observe an already settled but not retired plan.
        let _permit = permit;
        // Capture before preflight; installing the transition lease must
        // validate this same owner/session epoch instead of selecting again.
        let (generation, prepared) = prepared_voice_target(&producer_id)
            .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
        let bound = retained
            .as_ref()
            .map(|entry| entry.target.clone())
            .or(prepared);
        let capture_result = match bound.as_ref().and_then(|target| target.capture.as_ref()) {
            Some(original) => {
                usb_at
                    .validate_voice_cleanup_capture(&producer_id, original)
                    .await
            }
            None => usb_at.capture_voice_target(&producer_id).await,
        };
        let captured = capture_result.map_err(|error| RouteExecutionFailure {
            stop_confirmed: false,
            failure: VoiceRouteFailure {
                status: error.public_status(),
                code: error.public_code(),
                message: error.to_string(),
            },
            stop_phase,
        })?;
        let physical_target = captured.physical_target().cloned();
        let retained_capture = captured.clone();
        let obligation_slot = Arc::new(Mutex::new(None::<VoiceCleanupObligation>));
        let prepare_slot = obligation_slot.clone();
        let prepare_authority = authority.clone();
        let prepare_gate = gate.clone();
        let worker_gate = gate.clone();
        let worker_authority = authority.clone();
        let acquire_right = authority.right();
        let publication_inbox = inbox.clone();
        let publication_producer = producer_id.clone();
        let prepare_inbox = inbox.clone();
        let prepare_producer = producer_id.clone();
        let worker_inbox = inbox.clone();
        let worker_producer = producer_id.clone();
        let mut result = run_voice_route_pipeline(
            (),
            move || {
                let mut current = || ensure_current_producer(&prepare_inbox, &prepare_producer);
                current().map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                let lock = if let Some(control) =
                    bound.as_ref().and_then(|target| target.managed.as_ref())
                {
                    control.acquire_lock()
                } else if matches!(action, VoiceRouteAction::Start) {
                    prepare_gate
                        .root_proof_for(&root)
                        .map_err(|_| ownership_unavailable())
                        .and_then(|proof| {
                            crate::call_mode_backups::acquire_bound_recovery_lock(
                                &root,
                                &proof,
                                crate::call_mode_backups::RecoveryLockKind::AdbInterface,
                                false,
                            )
                            .map_err(|_| ownership_unavailable())
                        })
                } else {
                    acquire_interface_lock(&root)
                }
                .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                let timeout = match action {
                    VoiceRouteAction::Start => MODULE_VOICE_ROUTE_START_TIMEOUT,
                    VoiceRouteAction::Stop => {
                        MODULE_VOICE_ROUTE_STOP_TIMEOUT + MODULE_VOICE_ROUTE_STATUS_TIMEOUT
                    }
                    VoiceRouteAction::Status => MODULE_VOICE_ROUTE_STATUS_TIMEOUT,
                };
                if matches!(action, VoiceRouteAction::Start) {
                    ensure_voice_admission(&prepare_gate)
                        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                }
                let mut target = match bound {
                    Some(target) => {
                        target
                            .plan
                            .ensure_connected()
                            .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                        target
                    }
                    None => VoiceBoundTarget {
                        plan: AdbPlan::select_for_capture(physical_target.as_ref())
                            .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?,
                        capture: Some(retained_capture),
                        managed: None,
                    },
                };
                if matches!(action, VoiceRouteAction::Start) && target.managed.is_none() {
                    if retained.is_some() {
                        return Err(RouteExecutionFailure::probe(
                            ProbeError::new(
                                "module_voice_legacy_control_unknown",
                                "原未监督语音会话不能被事后收养",
                            ),
                            stop_phase,
                        ));
                    }
                    let proof = prepare_gate.root_proof_for(&root).map_err(|_| {
                        RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                    })?;
                    target.managed = Some(
                        managed_control::ManagedControl::create(
                            &root,
                            &proof,
                            &prepare_producer,
                            generation,
                        )
                        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?,
                    );
                }
                if matches!(action, VoiceRouteAction::Start | VoiceRouteAction::Stop) {
                    let obligation = retained.unwrap_or(VoiceCleanupObligation {
                        producer: prepare_producer.clone(),
                        generation,
                        target: target.clone(),
                        authority: prepare_authority,
                    });
                    voice_route_ownership()
                        .lock()
                        .map_err(|_| {
                            RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                        })?
                        .begin(obligation.clone())
                        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                    *prepare_slot.lock().map_err(|_| {
                        RouteExecutionFailure::probe(ownership_unavailable(), stop_phase)
                    })? = Some(obligation);
                }
                let managed = target.managed.clone();
                let mut session = if managed.is_some() {
                    ModuleVoiceADBSession::checked(Instant::now() + timeout, target.plan)
                } else {
                    ModuleVoiceADBSession::new(Instant::now() + timeout, target.plan)
                };
                let preflight = if matches!(action, VoiceRouteAction::Stop) && managed.is_none() {
                    let mut adb = FencedModuleVoiceADB {
                        adb: &mut session,
                        current: &mut current,
                    };
                    Some(obligation_stop_preflight(
                        quiesce_preflight(&mut adb, stop_policy)
                            .map_err(|error| RouteExecutionFailure::from_stop(error))?,
                    ))
                } else {
                    None
                };
                current().map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                let proceed = !matches!(preflight, Some(StopPreflight::Inactive));
                Ok((lock, (session, preflight, managed), proceed))
            },
            || async move {
                if matches!(action, VoiceRouteAction::Start | VoiceRouteAction::Stop) {
                    usb_at
                        .acquire_voice_transition_captured_with_right(
                            &producer_id,
                            captured,
                            &acquire_right,
                        )
                        .await
                        .map(Some)
                        .map_err(|error| RouteExecutionFailure {
                            stop_confirmed: false,
                            failure: VoiceRouteFailure {
                                status: error.public_status(),
                                code: error.public_code(),
                                message: error.to_string(),
                            },
                            stop_phase: stop_phase.map(|_| StopPhase::Stop),
                        })
                } else {
                    Ok(None)
                }
            },
            move |(mut session, preflight, managed)| {
                let mut current = || {
                    ensure_current_producer(&worker_inbox, &worker_producer)?;
                    if matches!(action, VoiceRouteAction::Start) {
                        ensure_voice_admission(&worker_gate)?;
                    }
                    if !worker_authority
                        .right()
                        .is_active_for(&worker_gate, &worker_producer)
                    {
                        return Err(ownership_unavailable());
                    }
                    Ok(())
                };
                let result = if let Some(control) = managed {
                    if matches!(action, VoiceRouteAction::Start) && !control.has_submitted() {
                        control
                            .submit(&session.plan, &mut current)
                            .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
                    }
                    let mut adb = FencedModuleVoiceADB {
                        adb: &mut session,
                        current: &mut current,
                    };
                    module_voice_route_root(&mut adb)
                        .and_then(|()| {
                            managed_control::route(&mut adb, &control, action, &mut || Ok(()))
                        })
                        .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))
                } else if let Some(preflight) = preflight {
                    let mut adb = FencedModuleVoiceADB {
                        adb: &mut session,
                        current: &mut current,
                    };
                    finish_quiesce(&mut adb, stop_policy, preflight)
                        .map(|()| false)
                        .map_err(|error| RouteExecutionFailure::from_stop(error))
                } else {
                    module_voice_route_with_adb(&mut session, action, stop_policy, &mut current)
                };
                current().map_err(|error| {
                    RouteExecutionFailure::probe(error, stop_phase.map(|_| StopPhase::Stop))
                })?;
                result
            },
            |lease| async move {
                match lease {
                    Some(lease) => lease
                        .release_confirmed()
                        .await
                        .map_err(|error| error.to_string()),
                    None => Ok(()),
                }
            },
            stop_phase,
        )
        .await;
        if matches!(action, VoiceRouteAction::Stop) {
            let original = obligation_slot
                .lock()
                .map_err(|_| RouteExecutionFailure::probe(ownership_unavailable(), stop_phase))?
                .clone();
            let confirmed = matches!(result, Ok(false))
                || result.as_ref().is_err_and(|error| error.stop_confirmed);
            if confirmed {
                if let Some(obligation) = original.clone() {
                    if let Err(error) = release_managed_obligation(
                        obligation,
                        gate.clone(),
                        publication_inbox.clone(),
                    )
                    .await
                    {
                        result = Err(RouteExecutionFailure::probe(error, stop_phase));
                    }
                }
            }
            let confirmed = matches!(result, Ok(false))
                || result.as_ref().is_err_and(|error| error.stop_confirmed);
            if let Some(obligation) = obligation_slot
                .lock()
                .map_err(|_| RouteExecutionFailure::probe(ownership_unavailable(), stop_phase))?
                .as_ref()
            {
                voice_route_ownership()
                    .lock()
                    .map_err(|_| RouteExecutionFailure::probe(ownership_unavailable(), stop_phase))?
                    .retire(obligation, confirmed);
            }
        }
        if result.as_ref().is_err_and(|error| error.stop_confirmed) {
            ensure_current_producer(&publication_inbox, &publication_producer)
                .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
        }
        result.and_then(|active| {
            ensure_current_producer(&publication_inbox, &publication_producer)
                .map_err(|error| RouteExecutionFailure::probe(error, stop_phase))?;
            Ok(active)
        })
    });
    worker.await.unwrap_or_else(|_| {
        Err(RouteExecutionFailure::probe(
            ProbeError::new("module_adb_worker_failed", "模块 ADB 工作线程未能确认结果"),
            stop_phase,
        ))
    })
}

// The same executor is used by native ADB and test fakes. The physical
// interface guard stays here while sessions move into blocking workers.
async fn run_voice_route_pipeline<P, G, S, L, PF, A, AF, W, R, RF>(
    admission: P,
    prepare: PF,
    acquire: A,
    work: W,
    release: R,
    stop_phase: Option<StopPhase>,
) -> Result<bool, RouteExecutionFailure>
where
    P: Send,
    G: Send + 'static,
    S: Send + 'static,
    PF: FnOnce() -> Result<(G, S, bool), RouteExecutionFailure> + Send + 'static,
    A: FnOnce() -> AF,
    AF: std::future::Future<Output = Result<L, RouteExecutionFailure>>,
    W: FnOnce(S) -> Result<bool, RouteExecutionFailure> + Send + 'static,
    R: FnOnce(L) -> RF,
    RF: std::future::Future<Output = Result<(), String>>,
{
    let _admission = admission;
    let (_physical_guard, state, proceed) = tokio::task::spawn_blocking(prepare)
        .await
        .unwrap_or_else(|_| {
            Err(RouteExecutionFailure::probe(
                ProbeError::new("module_adb_worker_failed", "模块 ADB 工作线程未能确认结果"),
                stop_phase,
            ))
        })?;
    if !proceed {
        return Ok(false);
    }
    let lease = acquire().await?;
    run_voice_route_worker(
        (),
        move || work(state),
        release(lease),
        stop_phase.map(|_| StopPhase::Stop),
    )
    .await
}

async fn run_voice_route_worker<P, W, C>(
    admission: P,
    work: W,
    cleanup: C,
    stop_phase: Option<StopPhase>,
) -> Result<bool, RouteExecutionFailure>
where
    P: Send,
    W: FnOnce() -> Result<bool, RouteExecutionFailure> + Send + 'static,
    C: std::future::Future<Output = Result<(), String>>,
{
    let _admission = admission;
    let result = tokio::task::spawn_blocking(work).await.unwrap_or_else(|_| {
        Err(RouteExecutionFailure::probe(
            ProbeError::new("module_adb_worker_failed", "模块 ADB 工作线程未能确认结果"),
            stop_phase,
        ))
    });
    match (result, cleanup.await) {
        (result, Ok(())) => result,
        (Err(mut error), Err(cleanup)) => {
            error.stop_confirmed = false;
            error
                .failure
                .message
                .push_str(&format!("；USB AT 租约释放未确认：{cleanup}"));
            Err(error)
        }
        (Ok(_), Err(cleanup)) => Err(RouteExecutionFailure::probe(
            ProbeError::new(
                "module_voice_route_cleanup_unconfirmed",
                format!("语音路由操作已完成，但 USB AT 租约释放未确认：{cleanup}"),
            ),
            stop_phase,
        )),
    }
}

pub(crate) fn voice_route_guard(
    data_root: &Path,
    action: VoiceRouteAction,
    operation_lock: Option<VoiceRouteGuard>,
) -> Result<Option<VoiceRouteGuard>, VoiceRouteFailure> {
    Ok(
        if matches!(action, VoiceRouteAction::Start | VoiceRouteAction::Stop) {
            match operation_lock {
                Some(lock) => Some(lock),
                None => Some(VoiceRouteGuard::shared(
                    crate::call_mode_backups::try_call_mode_operation_shared_lock(data_root)
                        .map_err(|error| {
                            if error.kind() == std::io::ErrorKind::WouldBlock {
                                VoiceRouteFailure {
                                    status: StatusCode::CONFLICT,
                                    code: "call_mode_operation_busy",
                                    message: "模块配置或通话模式正在变更，请等待操作完成".into(),
                                }
                            } else {
                                VoiceRouteFailure {
                                    status: StatusCode::SERVICE_UNAVAILABLE,
                                    code: "call_mode_operation_lock_unavailable",
                                    message: format!("无法确认模块通话模式操作状态：{error}"),
                                }
                            }
                        })?,
                )),
            }
        } else {
            operation_lock
        },
    )
}

pub(crate) fn spawn_guarded_voice_task<F, T>(
    operation_guard: Option<VoiceRouteGuard>,
    task: F,
) -> tokio::task::JoinHandle<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(async move {
        let _operation_guard = operation_guard;
        task.await
    })
}

fn response(
    status: StatusCode,
    code: &'static str,
    ready: bool,
    error: impl Into<String>,
) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &ProbeResponse {
            ready,
            code,
            error: error.into(),
            active: None,
            prepared: None,
            failure_phase: None,
            stop_phase: None,
        },
    )
}

fn route_request_response(
    action: Option<VoiceRouteAction>,
    policy: StopPolicy,
    status: StatusCode,
    code: &'static str,
    ready: bool,
    error: impl Into<String>,
) -> Response<Full<Bytes>> {
    if !ready && matches!(action, Some(VoiceRouteAction::Stop)) {
        voice_route_failure_response(RouteExecutionFailure {
            stop_confirmed: false,
            failure: VoiceRouteFailure {
                status,
                code,
                message: error.into(),
            },
            stop_phase: Some(if policy == StopPolicy::Restore {
                StopPhase::Status
            } else {
                StopPhase::Stop
            }),
        })
    } else {
        response(status, code, ready, error)
    }
}

fn voice_route_failure_response(error: RouteExecutionFailure) -> Response<Full<Bytes>> {
    crate::json_response(
        error.failure.status,
        &ProbeResponse {
            ready: false,
            code: error.failure.code,
            error: error.failure.message,
            active: None,
            prepared: None,
            failure_phase: None,
            stop_phase: error.stop_phase,
        },
    )
}

fn voice_route_response(
    status: StatusCode,
    code: &'static str,
    ready: bool,
    error: impl Into<String>,
    active: bool,
) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &ProbeResponse {
            ready,
            code,
            error: error.into(),
            active: Some(active),
            prepared: None,
            failure_phase: None,
            stop_phase: None,
        },
    )
}

fn runtime_readiness_response(
    status: StatusCode,
    code: &'static str,
    ready: bool,
    error: impl Into<String>,
    prepared: bool,
) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &ProbeResponse {
            ready,
            code,
            error: error.into(),
            active: None,
            prepared: Some(prepared),
            failure_phase: None,
            stop_phase: None,
        },
    )
}

fn preparation_failure_response(error: PreparationFailure) -> Response<Full<Bytes>> {
    crate::json_response(
        error.status,
        &ProbeResponse {
            ready: false,
            code: error.error.code,
            error: error.error.message,
            active: None,
            prepared: Some(false),
            failure_phase: Some(error.phase),
            stop_phase: None,
        },
    )
}

fn generation_response(
    status: StatusCode,
    code: &'static str,
    ready: bool,
    error: impl Into<String>,
    generation: u64,
) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &RuntimeGenerationResponse {
            ready,
            code,
            error: error.into(),
            runtime_generation: generation,
        },
    )
}

fn valid_producer(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

// Observers hold metadata only. The original blocking job retains the actual
// Child, ADB permit and recovery authority until a successful wait receipt.
struct ProbeObservation {
    root: PathBuf,
    producer: String,
    pending: Mutex<Option<ProbeError>>,
    command: Option<tokio::sync::watch::Sender<Option<ProbeError>>>,
}
#[cfg(not(test))]
static ACTIVE_PROBE: std::sync::OnceLock<Mutex<Option<std::sync::Weak<ProbeObservation>>>> =
    std::sync::OnceLock::new();
// Independent fixture admission permits overlapping namespaces in one test
// process. This metadata-only registry owns Weak references, never Child work.
#[cfg(test)]
static TEST_PROBE_OBSERVATIONS: std::sync::OnceLock<
    Mutex<std::collections::BTreeMap<(PathBuf, String), std::sync::Weak<ProbeObservation>>>,
> = std::sync::OnceLock::new();
tokio::task_local! {
    static COMMAND_PROBE_OBSERVER: tokio::sync::watch::Sender<Option<ProbeError>>;
}
pub(crate) async fn observe_command_probe<F: std::future::Future>(
    observer: tokio::sync::watch::Sender<Option<ProbeError>>,
    operation: F,
) -> F::Output {
    COMMAND_PROBE_OBSERVER.scope(observer, operation).await
}
impl ProbeObservation {
    fn begin(root: &Path, producer: &str) -> Arc<Self> {
        Self::begin_with_command(
            root,
            producer,
            COMMAND_PROBE_OBSERVER.try_with(Clone::clone).ok(),
        )
    }
    fn begin_with_command(
        root: &Path,
        producer: &str,
        command: Option<tokio::sync::watch::Sender<Option<ProbeError>>>,
    ) -> Arc<Self> {
        let observation = Arc::new(Self {
            root: root.into(),
            producer: producer.into(),
            pending: Mutex::new(None),
            command,
        });
        #[cfg(not(test))]
        {
            *ACTIVE_PROBE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&observation));
        }
        #[cfg(test)]
        {
            let mut observations = TEST_PROBE_OBSERVATIONS
                .get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            observations.retain(|_, weak| weak.strong_count() != 0);
            observations.insert(
                (observation.root.clone(), observation.producer.clone()),
                Arc::downgrade(&observation),
            );
        }
        observation
    }
    fn publish_failure(&self, failure: &ProbeError) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.is_none() {
            let error = ProbeError::new(
                "module_adb_cleanup_pending",
                format!(
                    "{}；原隔离进程退出与回收尚未确认，请等待清理完成",
                    failure.message
                ),
            );
            *pending = Some(error.clone());
            if let Some(command) = &self.command {
                command.send_replace(Some(error));
            }
        }
    }
}
// Matching both namespace and producer prevents a status observer from
// attaching a previous backend's failure to the current one. Weak ownership
// retires automatically; this registry never owns or poisons physical work.
pub(crate) fn pending_probe(root: &Path, producer: &str) -> Option<ProbeError> {
    #[cfg(not(test))]
    let active = ACTIVE_PROBE
        .get()?
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .upgrade()?;
    #[cfg(test)]
    let active = {
        let weak = {
            let mut observations = TEST_PROBE_OBSERVATIONS
                .get()?
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            observations.retain(|_, weak| weak.strong_count() != 0);
            observations
                .get(&(root.to_path_buf(), producer.to_owned()))?
                .clone()
        };
        // Do not hold the registry lock while upgrading or reading pending.
        weak.upgrade()?
    };
    if active.root != root || active.producer != producer {
        return None;
    }
    let error = active
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    error
}

fn supervise_probe_child(
    executable: &Path,
    data_root: &Path,
    observation: Arc<ProbeObservation>,
) -> Result<(), ProbeError> {
    let mut command = Command::new(executable);
    command
        .arg(INTERNAL_PROBE_ARGUMENT)
        .env("SWITCH_DATA_DIR", data_root);
    supervise_child_observed(
        command,
        PROBE_TIMEOUT,
        USB_RELEASE_COOLDOWN,
        Some(observation),
    )
}

#[cfg(test)]
fn supervise_child(
    command: Command,
    timeout: Duration,
    cooldown: Duration,
) -> Result<(), ProbeError> {
    supervise_child_observed(command, timeout, cooldown, None)
}

// Faults are per exact owner, private and compiled only in unit tests. They
// never fabricate an ExitStatus; every terminal receipt comes from this Child.
#[cfg(test)]
#[derive(Default)]
struct ChildFaults {
    kill_failures: usize,
    kill_blocked: Option<Arc<std::sync::atomic::AtomicBool>>,
    poll_failures: usize,
    wait_failures: usize,
    panic_once: bool,
    unconfirmed: Option<Arc<std::sync::atomic::AtomicBool>>,
}
struct ChildOwner {
    child: Child,
    settled: bool,
    signal_allowed: bool,
    primary: Option<ProbeError>,
    diagnostic: Vec<u8>,
    observation: Option<Arc<ProbeObservation>>,
    #[cfg(test)]
    faults: ChildFaults,
}
impl ChildOwner {
    fn new(child: Child, observation: Option<Arc<ProbeObservation>>) -> Self {
        Self {
            child,
            settled: false,
            signal_allowed: true,
            primary: None,
            diagnostic: Vec::with_capacity(MAX_HELPER_ERROR_BYTES),
            observation,
            #[cfg(test)]
            faults: ChildFaults::default(),
        }
    }
    fn fail(&mut self, error: ProbeError) {
        if self.primary.is_none() {
            self.primary = Some(error);
            if let (Some(observer), Some(primary)) = (&self.observation, &self.primary) {
                observer.publish_failure(primary);
            }
        }
    }
    fn wait_error(&mut self, error: std::io::Error) {
        // ECHILD can mean an external reaper consumed the identity. A numeric
        // PID must never be signalled again after that evidence. Retain the
        // obligation honestly; only an actual Child wait receipt can retire it.
        if error.raw_os_error() == Some(nix::libc::ECHILD) {
            self.signal_allowed = false;
        }
        if error.kind() != std::io::ErrorKind::Interrupted {
            self.fail(ProbeError::new(
                "module_adb_probe_unconfirmed",
                "无法确认隔离 ADB 探测进程状态",
            ));
        }
    }
    fn nonblocking_stderr(&mut self) {
        let Some(stderr) = &self.child.stderr else {
            return;
        };
        let configured =
            nix::fcntl::fcntl(stderr, nix::fcntl::FcntlArg::F_GETFL).and_then(|flags| {
                nix::fcntl::fcntl(
                    stderr,
                    nix::fcntl::FcntlArg::F_SETFL(
                        nix::fcntl::OFlag::from_bits_truncate(flags)
                            | nix::fcntl::OFlag::O_NONBLOCK,
                    ),
                )
            });
        if configured.is_err() {
            self.child.stderr.take();
            self.fail(ProbeError::new(
                "module_adb_probe_unconfirmed",
                "无法安全收集隔离 ADB 探测诊断",
            ));
        }
    }
    fn drain_stderr(&mut self) {
        let Some(stderr) = &mut self.child.stderr else {
            return;
        };
        let mut bytes = [0; 256];
        // Bounded work per poll, bounded retained data. A descendant retaining
        // the pipe cannot make post-reap collection block waiting for EOF.
        for _ in 0..8 {
            match stderr.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    let keep = count.min(MAX_HELPER_ERROR_BYTES - self.diagnostic.len());
                    self.diagnostic.extend_from_slice(&bytes[..keep]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }
    fn poll(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        #[cfg(test)]
        {
            if self.faults.panic_once {
                self.faults.panic_once = false;
                panic!("controlled exact-child supervisor panic");
            }
            if self
                .faults
                .unconfirmed
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(std::io::Error::from_raw_os_error(nix::libc::EIO));
            }
            if self.faults.poll_failures > 0 {
                self.faults.poll_failures -= 1;
                return Err(std::io::Error::from_raw_os_error(nix::libc::EIO));
            }
        }
        self.child.try_wait()
    }
    fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(test)]
        {
            if self
                .faults
                .kill_blocked
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(std::io::Error::from_raw_os_error(nix::libc::EPERM));
            }
            if self.faults.kill_failures > 0 {
                self.faults.kill_failures -= 1;
                return Err(std::io::Error::from_raw_os_error(nix::libc::EPERM));
            }
        }
        self.child.kill()
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(test)]
        {
            if self
                .faults
                .unconfirmed
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(std::io::Error::from_raw_os_error(nix::libc::EIO));
            }
            if self.faults.wait_failures > 0 {
                self.faults.wait_failures -= 1;
                return Err(std::io::Error::from_raw_os_error(nix::libc::EIO));
            }
        }
        self.child.wait()
    }
    fn settle(&mut self, deadline: Instant) -> std::process::ExitStatus {
        let mut next_kill = Instant::now();
        loop {
            self.drain_stderr();
            match self.poll() {
                Ok(Some(status)) => {
                    self.settled = true;
                    return status;
                }
                Ok(None) => {}
                Err(error) => self.wait_error(error),
            }
            if self.primary.is_none() && Instant::now() >= deadline {
                self.fail(ProbeError::new(
                    "module_adb_probe_timeout",
                    "模块 ADB 探测超过等待时限",
                ));
            }
            if self.primary.is_some() && self.signal_allowed && Instant::now() >= next_kill {
                // Kill is an attempt, never a settlement receipt. A failed
                // attempt retains this same owner and retries at bounded rate.
                if self.kill().is_ok() {
                    match self.wait() {
                        Ok(status) => {
                            self.settled = true;
                            return status;
                        }
                        Err(error) => self.wait_error(error),
                    }
                }
                next_kill = Instant::now() + Duration::from_millis(100);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn supervise(&mut self, timeout: Duration, cooldown: Duration) -> Result<(), ProbeError> {
        self.nonblocking_stderr();
        let deadline = Instant::now() + timeout;
        let status = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.settle(deadline)
        })) {
            Ok(status) => status,
            Err(_) => {
                self.fail(ProbeError::new(
                    "module_adb_probe_unconfirmed",
                    "隔离 ADB 探测监督异常",
                ));
                self.settle(Instant::now())
            }
        };
        self.drain_stderr();
        self.child.stderr.take();
        if let Some(mut error) = self.primary.take() {
            std::thread::sleep(cooldown);
            error.message.push_str("；隔离进程退出与回收已确认");
            return Err(error);
        }
        if status.success() {
            Ok(())
        } else {
            Err(helper_failure(&self.diagnostic))
        }
    }
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        if !self.settled {
            self.nonblocking_stderr();
            // Unwinding cannot discard the sole Child while outer guards drop.
            // Keep settling in this same blocking frame; never hand numeric
            // PID ownership to an unrelated task or claim clean by elapsed time.
            self.fail(ProbeError::new(
                "module_adb_probe_unconfirmed",
                "隔离 ADB 探测监督未正常完成",
            ));
            self.settle(Instant::now());
        }
    }
}
fn supervise_child_observed(
    mut command: Command,
    timeout: Duration,
    cooldown: Duration,
    observation: Option<Arc<ProbeObservation>>,
) -> Result<(), ProbeError> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            ProbeError::new("module_adb_probe_unavailable", "无法启动隔离 ADB 探测程序")
        })?;
    ChildOwner::new(child, observation).supervise(timeout, cooldown)
}
fn helper_failure(output: &[u8]) -> ProbeError {
    let detail = String::from_utf8_lossy(output);
    if detail.contains("module_adb_authentication_required") {
        ProbeError::new(
            "module_adb_authentication_required",
            "模块 ADB 要求认证，无法自动部署通话运行时",
        )
    } else if detail.contains("module_adb_root_required") {
        ProbeError::new("module_adb_probe_failed", "模块 ADB 未返回 root shell")
    } else {
        ProbeError::new(
            "module_adb_probe_failed",
            "模块 ADB 探测失败，未确认 root shell",
        )
    }
}

pub(crate) fn run_isolated_probe(data_root: &Path) -> Result<(), ProbeError> {
    let _lock = acquire_interface_lock(data_root)?;
    AdbPlan::select()?.with_usb(|usb| AdbProbe::new(usb).run())
}

fn deploy_module_voice_runtime(
    data_root: &Path,
    selected: &mut Option<AdbPlan>,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<String, ProbeError> {
    current()?;
    let files =
        crate::call_mode_runtime::read_deployment_artifacts(data_root).map_err(|error| {
            ProbeError::new(
                "call_mode_runtime_verification_failed",
                format!("Rust 语音运行时校验失败：{error}"),
            )
        })?;
    let supervisor = crate::call_mode_runtime::supervisor_artifact().map_err(|_| {
        ProbeError::new(
            "call_mode_runtime_verification_failed",
            "Rust语音控制程序校验失败",
        )
    })?;
    let _lock = acquire_interface_lock(data_root)?;
    current()?;
    // Preserve validation-before-USB ordering. Once selected, every later
    // self-test/reconciliation/rollback stage retains this exact plan.
    let plan = match selected {
        Some(plan) => plan,
        None => selected.insert(AdbPlan::select()?),
    };
    plan.with_usb(|usb| {
    let mut adb =
        AdbProbe::with_deadline(usb, Instant::now() + MODULE_VOICE_OPERATION_TIMEOUT);
    adb.connect()?;
    current()?;

    let (identity, status) = adb.shell_checked("id -u")?;
    let root_shell = std::str::from_utf8(&identity)
        .is_ok_and(|text| text.split_ascii_whitespace().any(|field| field == "0"));
    if status != 0 || !root_shell {
        return Err(ProbeError::new(
            "module_adb_root_required",
            "模块 ADB 没有 root 控制权限",
        ));
    }

    current()?;
    let (release, status) = adb.shell_checked("uname -r")?;
    let release = String::from_utf8_lossy(&release).trim().to_owned();
    let expected_kernel = &crate::call_mode_runtime_manifest::manifest().kernel;
    if status != 0
        || !release
            .split_ascii_whitespace()
            .any(|field| field == expected_kernel)
    {
        let actual = if release.is_empty() {
            "未知"
        } else {
            release.as_str()
        };
        return Err(ProbeError::new(
            "module_voice_kernel_mismatch",
            format!("模块内核版本不匹配；需要 {expected_kernel}，实际为 {actual}"),
        ));
    }

    let create_directory = format!(
        "mkdir -p '{MODULE_VOICE_REMOTE_DIRECTORY}' && chmod 700 '{MODULE_VOICE_REMOTE_DIRECTORY}'"
    );
    current()?;
    let (_, status) = adb.shell_checked(&create_directory).map_err(|_| {
        ProbeError::new(
            "module_voice_remote_directory_failed",
            "无法创建模块运行时目录",
        )
    })?;
    if status != 0 {
        return Err(ProbeError::new(
            "module_voice_remote_directory_failed",
            "无法创建模块运行时目录",
        ));
    }

    for (artifact, data) in files {
        current()?;
        let mode = 0o100000 | artifact.mode;
        let path = format!("{MODULE_VOICE_REMOTE_DIRECTORY}/{}", artifact.name);
        adb.push(&data, &path, mode).map_err(|error| {
            ProbeError::new(
                "module_adb_file_transfer_failed",
                format!("部署 {}：{error}", artifact.name),
            )
        })?;
        current()?;
    }
    current()?;
    adb.push(&supervisor.bytes, &format!("{MODULE_VOICE_REMOTE_DIRECTORY}/{}",supervisor.name),0o100000|supervisor.mode)?;
    current()?;
    let mut adb = FencedModuleVoiceADB {
        adb: &mut adb,
        current,
    };
    let prepared = prepare_module_voice_audio(&mut adb);
    (adb.current)()?;
    prepared?;
    read_module_boot_id(&mut adb)
    })
}

fn module_voice_runtime_boot_id(
    data_root: &Path,
    producer_id: &str,
    inbox: &crate::message_archive::InboxState,
) -> Result<String, ProbeError> {
    ensure_current_producer(inbox, producer_id)?;
    let (_, target) = prepared_voice_target(producer_id)?;
    let target = target.ok_or_else(|| {
        ProbeError::new(
            "module_voice_runtime_unprepared",
            "当前连接没有已确认的语音运行时准备计划",
        )
    })?;
    module_voice_runtime_boot_id_fenced(data_root, &target.plan, &mut || {
        ensure_current_producer(inbox, producer_id)
    })
}

fn module_voice_runtime_boot_id_fenced(
    data_root: &Path,
    plan: &AdbPlan,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<String, ProbeError> {
    current()?;
    let _lock = acquire_interface_lock(data_root)?;
    current()?;
    plan.with_usb(|usb| {
        let mut adb =
            AdbProbe::with_deadline(usb, Instant::now() + MODULE_VOICE_ROUTE_STATUS_TIMEOUT);
        adb.connect()?;
        current()?;
        let boot_id = read_module_boot_id(&mut adb)?;
        current()?;
        Ok(boot_id)
    })
}

fn read_module_boot_id(adb: &mut impl ModuleVoiceADB) -> Result<String, ProbeError> {
    let (output, status) = adb.shell_checked_timeout(
        "cat /proc/sys/kernel/random/boot_id",
        Duration::from_secs(8),
    )?;
    let boot_id = String::from_utf8_lossy(&output).trim().to_ascii_lowercase();
    if status != 0 || !valid_module_boot_id(&boot_id) {
        return Err(ProbeError::new(
            "module_voice_boot_id_unconfirmed",
            "无法确认模块当前启动代次",
        ));
    }
    Ok(boot_id)
}

fn valid_module_boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

trait ModuleVoiceADB {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError>;
}

struct FencedModuleVoiceADB<'a, A> {
    adb: &'a mut A,
    current: &'a mut dyn FnMut() -> Result<(), ProbeError>,
}
impl<A: ModuleVoiceADB> ModuleVoiceADB for FencedModuleVoiceADB<'_, A> {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        (self.current)()?;
        let result = self.adb.shell_checked_timeout(command, timeout);
        (self.current)()?;
        result
    }
}

impl<U: BulkIo> ModuleVoiceADB for AdbProbe<'_, U> {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        self.shell_checked_timeout(command, timeout)
    }
}

struct ModuleVoiceADBSession {
    checked: bool,
    deadline: Instant,
    plan: AdbPlan,
}

impl ModuleVoiceADBSession {
    fn checked(deadline: Instant, plan: AdbPlan) -> Self {
        Self {
            deadline,
            plan,
            checked: true,
        }
    }
    fn new(deadline: Instant, plan: AdbPlan) -> Self {
        Self {
            deadline,
            plan,
            checked: false,
        }
    }
}

impl ModuleVoiceADB for ModuleVoiceADBSession {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        self.plan.ensure_connected()?;
        let now = Instant::now();
        if now >= self.deadline {
            return Err(ProbeError::new(
                "module_voice_route_timeout",
                "模块 D4/UAC 路由操作超过时限",
            ));
        }
        let command_deadline = now + timeout.min(self.deadline.saturating_duration_since(now));
        let run = |usb: &mut PlanBulk<'_>| {
            let mut adb = AdbProbe::with_deadline(usb, command_deadline);
            adb.connect()?;
            adb.shell_checked_timeout(command, timeout)
        };
        if self.checked {
            self.plan.with_usb_checked(run)
        } else {
            self.plan.with_usb(run)
        }
    }
}

fn module_voice_route_action_fenced(
    data_root: &Path,
    plan: &AdbPlan,
    action: VoiceRouteAction,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<bool, ProbeError> {
    module_voice_route_action_with_policy_fenced(
        data_root,
        plan,
        action,
        StopPolicy::Always,
        current,
    )
    .map_err(|error| ProbeError::new(error.failure.code, error.failure.message))
}

fn module_voice_route_action_with_policy_fenced(
    data_root: &Path,
    plan: &AdbPlan,
    action: VoiceRouteAction,
    policy: StopPolicy,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<bool, RouteExecutionFailure> {
    let phase = matches!(action, VoiceRouteAction::Stop).then_some(StopPhase::Stop);
    current().map_err(|error| RouteExecutionFailure::probe(error, phase))?;
    let _lock = acquire_interface_lock(data_root)
        .map_err(|error| RouteExecutionFailure::probe(error, phase))?;
    current().map_err(|error| RouteExecutionFailure::probe(error, phase))?;
    let timeout = match action {
        VoiceRouteAction::Start => MODULE_VOICE_ROUTE_START_TIMEOUT,
        // Status, stop, rollback and reconciliation share one bounded session.
        VoiceRouteAction::Stop => {
            MODULE_VOICE_ROUTE_STOP_TIMEOUT + MODULE_VOICE_ROUTE_STATUS_TIMEOUT
        }
        VoiceRouteAction::Status => MODULE_VOICE_ROUTE_STATUS_TIMEOUT,
    };
    let mut session = ModuleVoiceADBSession::new(Instant::now() + timeout, plan.clone());
    module_voice_route_with_adb(&mut session, action, policy, current)
}

fn module_voice_route_with_adb(
    session: &mut impl ModuleVoiceADB,
    action: VoiceRouteAction,
    policy: StopPolicy,
    current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<bool, RouteExecutionFailure> {
    let mut adb = FencedModuleVoiceADB {
        adb: session,
        current,
    };
    let mut unfenced_current = || Ok(());
    let result = match action {
        VoiceRouteAction::Stop => quiesce_module_voice_route(&mut adb, policy)
            .map(|()| false)
            .map_err(|error| RouteExecutionFailure::from_stop(error)),
        VoiceRouteAction::Start => module_voice_route_root(&mut adb)
            .and_then(|()| {
                start_module_voice_route(
                    &mut adb,
                    Instant::now() + Duration::from_secs(30),
                    Duration::from_millis(250),
                    &mut unfenced_current,
                )
            })
            .map(|()| true)
            .map_err(|error| RouteExecutionFailure::probe(error, None)),
        VoiceRouteAction::Status => module_voice_route_root(&mut adb)
            .and_then(|()| module_voice_route_status(&mut adb))
            .map_err(|error| RouteExecutionFailure::probe(error, None)),
    };
    (adb.current)().map_err(|error| {
        RouteExecutionFailure::probe(
            error,
            matches!(action, VoiceRouteAction::Stop).then_some(StopPhase::Stop),
        )
    })?;
    result
}

fn module_voice_route_root(adb: &mut impl ModuleVoiceADB) -> Result<(), ProbeError> {
    let (identity, status) = adb.shell_checked_timeout("id -u", Duration::from_secs(8))?;
    if status == 0
        && std::str::from_utf8(&identity)
            .is_ok_and(|text| text.split_ascii_whitespace().any(|field| field == "0"))
    {
        Ok(())
    } else {
        Err(ProbeError::new(
            "module_adb_root_required",
            "模块 ADB 没有 root 控制权限",
        ))
    }
}

enum StopPreflight {
    Inactive,
    Proceed {
        status_error: Option<ProbeError>,
        root_confirmed: bool,
    },
}

fn obligation_stop_preflight(preflight: StopPreflight) -> StopPreflight {
    match preflight {
        StopPreflight::Inactive => StopPreflight::Proceed {
            status_error: None,
            root_confirmed: true,
        },
        proceed => proceed,
    }
}

fn quiesce_preflight(
    adb: &mut impl ModuleVoiceADB,
    policy: StopPolicy,
) -> Result<StopPreflight, StopFailure> {
    if policy == StopPolicy::Always {
        return Ok(StopPreflight::Proceed {
            status_error: None,
            root_confirmed: false,
        });
    }
    match module_voice_route_root(adb).and_then(|()| module_voice_route_status(adb)) {
        Ok(false) => Ok(StopPreflight::Inactive),
        Ok(true) => Ok(StopPreflight::Proceed {
            status_error: None,
            root_confirmed: true,
        }),
        Err(error) if policy == StopPolicy::Restore || error.code == TARGET_DISCONNECTED => {
            Err(StopFailure {
                stop_confirmed: false,
                error,
                phase: StopPhase::Status,
            })
        }
        Err(error) => Ok(StopPreflight::Proceed {
            status_error: Some(error),
            root_confirmed: false,
        }),
    }
}

fn quiesce_module_voice_route(
    adb: &mut impl ModuleVoiceADB,
    policy: StopPolicy,
) -> Result<(), StopFailure> {
    let preflight = quiesce_preflight(adb, policy)?;
    finish_quiesce(adb, policy, preflight)
}

fn finish_quiesce(
    adb: &mut impl ModuleVoiceADB,
    policy: StopPolicy,
    preflight: StopPreflight,
) -> Result<(), StopFailure> {
    let StopPreflight::Proceed {
        status_error,
        root_confirmed,
    } = preflight
    else {
        return Ok(());
    };
    // A failed root/status preflight is retried as part of stop, matching the
    // caller's prior recovery behavior without relinquishing admission.
    let stop = if !root_confirmed {
        module_voice_route_root(adb)
    } else {
        Ok(())
    }
    .and_then(|()| {
        stop_module_voice_route(
            adb,
            20,
            Duration::from_millis(100),
            5,
            Duration::from_millis(200),
            &mut || Ok(()),
        )
    });
    match (stop, status_error) {
        (Ok(()), Some(error)) if policy == StopPolicy::Shutdown => Err(StopFailure {
            stop_confirmed: true,
            error,
            phase: StopPhase::Status,
        }),
        (Ok(()), _) => Ok(()),
        (Err(mut error), initial) => {
            if let Some(initial) = initial {
                error
                    .message
                    .push_str(&format!("；停止前路由状态未确认：{}", initial.message));
            }
            Err(StopFailure {
                stop_confirmed: false,
                error,
                phase: StopPhase::Stop,
            })
        }
    }
}

fn ensure_current_producer(
    inbox: &crate::message_archive::InboxState,
    producer_id: &str,
) -> Result<(), ProbeError> {
    if inbox.owns_module_producer(producer_id) {
        Ok(())
    } else {
        Err(ProbeError::new(
            "stale_backend_generation",
            "后端代次已变化；拒绝继续模块语音路由操作",
        ))
    }
}

fn route_ready_or_disconnected(adb: &mut impl ModuleVoiceADB) -> Result<bool, ProbeError> {
    match module_voice_route_ready(adb) {
        Err(error) if error.code == TARGET_DISCONNECTED => Err(error),
        result => Ok(result.unwrap_or(false)),
    }
}

fn start_module_voice_route(
    adb: &mut impl ModuleVoiceADB,
    deadline: Instant,
    poll_interval: Duration,
    ensure_current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    if route_ready_or_disconnected(adb)? {
        return Ok(());
    }
    ensure_current()?;
    let launch = format!(
        "rm -f '/run/proletarius-voice-route.pid' '/run/proletarius-voice-route.log'; nohup '{MODULE_VOICE_HELPER}' --voice-route-session --verbose </dev/null >> '/run/proletarius-voice-route.log' 2>&1 & pid=$!; starttime=$(cut -d ' ' -f 22 \"/proc/$pid/stat\" 2>/dev/null); case \"$pid:$starttime\" in :*|*:|*[!0-9:]*) false;; *) printf '%s %s\\n' \"$pid\" \"$starttime\" > '/run/proletarius-voice-route.pid';; esac"
    );
    let (launch_output, launch_status, launch_error) =
        match adb.shell_checked_timeout(&launch, Duration::from_secs(8)) {
            Ok((output, status)) => (output, status, None),
            Err(error) if error.code == TARGET_DISCONNECTED => return Err(error),
            Err(error) => (Vec::new(), 0, Some(error)),
        };
    let launch_detail = if launch_status != 0 || launch_error.is_some() {
        first_nonempty_module_voice(&[
            String::from_utf8_lossy(&launch_output).trim().to_owned(),
            launch_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            format!("启动命令返回状态 {launch_status}"),
        ])
    } else {
        String::new()
    };

    while Instant::now() < deadline {
        ensure_current()?;
        if route_ready_or_disconnected(adb)? {
            return Ok(());
        }
        std::thread::sleep(poll_interval);
    }

    let log_output = adb
        .shell_checked_timeout(
            "test ! -f '/run/proletarius-voice-route.log' || tail -c 3500 '/run/proletarius-voice-route.log'",
            Duration::from_secs(8),
        )
        .map(|(output, _)| output)
        .unwrap_or_default();
    let mut detail = join_nonempty_module_voice(&[
        launch_detail,
        String::from_utf8_lossy(&log_output).trim().to_owned(),
    ]);
    if detail.is_empty() {
        detail = "模块 D4/UAC 语音路由没有进入 RUNNING".to_owned();
    }
    if let Err(error) = stop_module_voice_route(
        adb,
        20,
        Duration::from_millis(100),
        5,
        Duration::from_millis(200),
        ensure_current,
    ) {
        detail.push_str("\n自动回滚未完全确认：");
        detail.push_str(&error.to_string());
    }
    Err(ProbeError::new("module_voice_route_start_failed", detail))
}

fn stop_module_voice_route(
    adb: &mut impl ModuleVoiceADB,
    max_polls: usize,
    poll_interval: Duration,
    cleanup_attempts: usize,
    cleanup_delay: Duration,
    ensure_current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    ensure_current()?;
    let original = match adb
        .shell_checked_timeout(module_voice_route_stop_command(), Duration::from_secs(10))
    {
        Ok((_, 0)) => None,
        Ok((output, status)) => Some(ProbeError::new(
            "module_voice_route_stop_unconfirmed",
            first_nonempty_module_voice(&[
                String::from_utf8_lossy(&output).trim().to_owned(),
                format!("停止命令返回状态 {status}"),
            ]),
        )),
        Err(error) if error.code == TARGET_DISCONNECTED => return Err(error),
        Err(error) => Some(error),
    };
    let mut stopped = false;
    let mut status_detail = None;
    for _ in 0..max_polls {
        ensure_current()?;
        match module_voice_route_stopped(adb) {
            Ok(true) => {
                stopped = true;
                break;
            }
            Ok(false) => {}
            Err(error) if error.code == TARGET_DISCONNECTED => {
                return Err(match original {
                    Some(mut original) => {
                        original
                            .message
                            .push_str(&format!("；原设备已断开：{}", error.message));
                        original
                    }
                    None => error,
                });
            }
            Err(error) => status_detail = Some(error.message),
        }
        std::thread::sleep(poll_interval);
    }
    if !stopped {
        let detail = first_nonempty_module_voice(&[
            status_detail.unwrap_or_default(),
            "D4 语音 helper 未确认正常退出；为保留 mixer 回滚，没有发送 SIGKILL".to_owned(),
        ]);
        return Err(match original {
            Some(mut error) => {
                error
                    .message
                    .push_str(&format!("；helper 退出未确认：{detail}"));
                error
            }
            None => ProbeError::new("module_voice_route_stop_unconfirmed", detail),
        });
    }
    // A stopped helper is insufficient: physically run T/T/B and disable UAC
    // before accepting any status-based recovery of a lost stop reply.
    if let Err(cleanup) =
        cleanup_module_voice_audio(adb, cleanup_attempts, cleanup_delay, ensure_current)
    {
        return Err(match original {
            Some(mut error) => {
                error
                    .message
                    .push_str(&format!("；音频回滚未确认：{}", cleanup.message));
                error
            }
            None => cleanup,
        });
    }
    if let Some(mut error) = original {
        ensure_current()?;
        match module_voice_route_status(adb) {
            Ok(false) => Ok(()),
            Ok(true) => {
                error.message.push_str("；清理后语音路由仍处于活动状态");
                Err(error)
            }
            Err(status) => {
                error
                    .message
                    .push_str(&format!("；清理后路由状态也未确认：{}", status.message));
                Err(error)
            }
        }
    } else {
        Ok(())
    }
}

fn cleanup_module_voice_audio(
    adb: &mut impl ModuleVoiceADB,
    cleanup_attempts: usize,
    cleanup_delay: Duration,
    ensure_current: &mut impl FnMut() -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    let cleanup = "echo 0 > /sys/class/android_usb/f_audio/audio_enable || exit $?; if test -p /run/voc_svr; then printf 'T\\n' > /run/voc_svr && printf 'T\\n' > /run/voc_svr && printf 'B\\n' > /run/voc_svr || exit $?; fi; test \"$(cat /sys/class/android_usb/f_audio/audio_enable)\" = 0";
    let mut cleanup_detail = String::new();
    for attempt in 0..cleanup_attempts {
        ensure_current()?;
        match adb.shell_checked_timeout(cleanup, Duration::from_secs(8)) {
            Ok((_, 0)) => return Ok(()),
            Ok((output, status)) => {
                cleanup_detail = first_nonempty_module_voice(&[
                    String::from_utf8_lossy(&output).trim().to_owned(),
                    format!("清理命令返回状态 {status}"),
                ]);
            }
            Err(error) if error.code == TARGET_DISCONNECTED => return Err(error),
            Err(error) => cleanup_detail = error.to_string(),
        }
        if attempt + 1 < cleanup_attempts {
            std::thread::sleep(cleanup_delay);
        }
    }
    Err(ProbeError::new(
        "module_voice_route_cleanup_unconfirmed",
        first_nonempty_module_voice(&[
            cleanup_detail,
            "D4 helper 已退出，但 T/T/B 路由回滚未确认".to_owned(),
        ]),
    ))
}

fn module_voice_route_status(adb: &mut impl ModuleVoiceADB) -> Result<bool, ProbeError> {
    if module_voice_route_ready(adb)? {
        return Ok(true);
    }
    if module_voice_route_stopped(adb)? {
        return Ok(false);
    }
    Err(ProbeError::new(
        "module_voice_route_state_unconfirmed",
        "模块 D4/UAC 语音路由状态无法确认",
    ))
}

fn module_voice_route_ready(adb: &mut impl ModuleVoiceADB) -> Result<bool, ProbeError> {
    let command = "test -s '/run/proletarius-voice-route.pid' && read pid expected_start < '/run/proletarius-voice-route.pid' && test \"$(cut -d ' ' -f 22 \"/proc/$pid/stat\" 2>/dev/null)\" = \"$expected_start\" && test \"$(tr '\\000' '\\n' < \"/proc/$pid/cmdline\" 2>/dev/null | sed -n '1p')\" = '/tmp/proletarius-call/mavo-pcm-bridge.armv7' && tr '\\000' '\\n' < \"/proc/$pid/cmdline\" 2>/dev/null | grep -q '^--voice-route-session$' && grep -q 'VoLTE route session active on hw:0,4' '/run/proletarius-voice-route.log' && test \"$(cat /sys/class/android_usb/f_audio/audio_enable)\" = 1 && grep -q '^state: RUNNING' /proc/asound/card0/pcm4p/sub0/status && grep -q '^state: RUNNING' /proc/asound/card0/pcm4c/sub0/status";
    let (_, status) = adb.shell_checked_timeout(command, Duration::from_secs(8))?;
    Ok(status == 0)
}

fn module_voice_route_stopped(adb: &mut impl ModuleVoiceADB) -> Result<bool, ProbeError> {
    let command = "owned=0; if test -s '/run/proletarius-voice-route.pid'; then read pid expected_start < '/run/proletarius-voice-route.pid' || true; case \"$pid:$expected_start\" in :*|*:|*[!0-9:]*) true;; *) current_start=$(cut -d ' ' -f 22 \"/proc/$pid/stat\" 2>/dev/null); argv0=$(tr '\\000' '\\n' < \"/proc/$pid/cmdline\" 2>/dev/null | sed -n '1p'); args=$(tr '\\000' '\\n' < \"/proc/$pid/cmdline\" 2>/dev/null); if test \"$current_start\" = \"$expected_start\" && test \"$argv0\" = '/tmp/proletarius-call/mavo-pcm-bridge.armv7' && printf '%s\\n' \"$args\" | grep -q '^--voice-route-session$'; then owned=1; fi;; esac; fi; test \"$owned\" -eq 0";
    let (_, status) = adb.shell_checked_timeout(command, Duration::from_secs(8))?;
    Ok(status == 0)
}

fn module_voice_route_stop_command() -> &'static str {
    "helper_stopped=1; is_owned() { current_start=$(cut -d ' ' -f 22 \"/proc/$pid/stat\" 2>/dev/null); argv0=$(tr '\\000' '\\n' < \"/proc/$pid/cmdline\" 2>/dev/null | sed -n '1p'); args=$(tr '\\000' '\\n' < \"/proc/$pid/cmdline\" 2>/dev/null); test \"$current_start\" = \"$expected_start\" && test \"$argv0\" = '/tmp/proletarius-call/mavo-pcm-bridge.armv7' && printf '%s\\n' \"$args\" | grep -q '^--voice-route-session$'; }; if test -s '/run/proletarius-voice-route.pid'; then read pid expected_start < '/run/proletarius-voice-route.pid' || true; case \"$pid:$expected_start\" in :*|*:|*[!0-9:]*) true;; *) if is_owned; then kill -TERM \"$pid\" 2>/dev/null || true; n=0; while is_owned && test \"$n\" -lt 50; do sleep 0.1; n=$((n+1)); done; is_owned && helper_stopped=0 || true; fi;; esac; fi; test \"$helper_stopped\" -eq 1 && rm -f '/run/proletarius-voice-route.pid'"
}

fn prepare_module_voice_audio(adb: &mut impl ModuleVoiceADB) -> Result<(), ProbeError> {
    let sound_checks = module_voice_sound_device_checks();
    let (_, status) = adb.shell_checked_timeout(sound_checks, Duration::from_secs(8))?;
    if status != 0 {
        let (_, legacy_status) = adb
            .shell_checked_timeout(
                "grep -q '^qdc507_afe ' /proc/modules",
                Duration::from_secs(8),
            )
            .map_err(|error| {
                ProbeError::new("module_voice_driver_check_failed", error.to_string())
            })?;
        if legacy_status == 0 {
            return Err(ProbeError::new(
                "module_voice_legacy_driver_loaded",
                "检测到旧版 qdc507_afe 驱动仍在内核中；请重启模块后再试，应用不会热切换该驱动",
            ));
        }
        if legacy_status != 1 {
            return Err(ProbeError::new(
                "module_voice_driver_check_failed",
                "无法核对旧版音频驱动状态",
            ));
        }
        for (file, name) in [
            ("qdc507_aprv3.ko", "qdc507_aprv3"),
            ("qdc507_voice.ko", "qdc507_voice"),
        ] {
            let query = format!("grep -q '^{name} ' /proc/modules");
            let (_, present) = adb
                .shell_checked_timeout(&query, Duration::from_secs(8))
                .map_err(|error| {
                    ProbeError::new("module_voice_driver_check_failed", error.to_string())
                })?;
            if present == 0 {
                continue;
            }
            if present != 1 {
                return Err(ProbeError::new(
                    "module_voice_driver_check_failed",
                    "无法读取模块驱动列表",
                ));
            }
            let load = format!("insmod '{MODULE_VOICE_REMOTE_DIRECTORY}/{file}'");
            let (output, load_status) = adb
                .shell_checked_timeout(&load, Duration::from_secs(20))
                .map_err(|error| {
                ProbeError::new("module_voice_driver_load_failed", error.to_string())
            })?;
            if load_status != 0 {
                let diagnostics = adb
                    .shell_checked_timeout("dmesg | tail -c 3500", Duration::from_secs(8))
                    .map(|(diagnostics, _)| diagnostics)
                    .unwrap_or_default();
                let detail = join_nonempty_module_voice(&[
                    String::from_utf8_lossy(&output).trim().to_owned(),
                    String::from_utf8_lossy(&diagnostics).trim().to_owned(),
                ]);
                let detail = if detail.is_empty() {
                    "模块音频驱动加载失败".to_owned()
                } else {
                    detail
                };
                // APR/voice may receive late DSP callbacks. Never hot-unload
                // these modules after loading; reboot is the safe rollback.
                return Err(ProbeError::new("module_voice_driver_load_failed", detail));
            }
        }
    }

    let wait_devices = format!(
        "ready=0; n=0; while test \"$n\" -lt 100; do if {sound_checks}; then ready=1; break; fi; sleep 0.2; n=$((n+1)); done; test \"$ready\" -eq 1"
    );
    let (output, status) = adb
        .shell_checked_timeout(&wait_devices, Duration::from_secs(25))
        .map_err(|error| {
            ProbeError::new(
                "module_voice_alsa_not_ready",
                format!("音频设备检查未完成：{error}"),
            )
        })?;
    if status != 0 {
        let diagnostics = adb
            .shell_checked_timeout("dmesg | tail -c 3500", Duration::from_secs(8))
            .map(|(diagnostics, _)| diagnostics)
            .unwrap_or_default();
        let detail = first_nonempty_module_voice(&[
            String::from_utf8_lossy(&diagnostics).trim().to_owned(),
            String::from_utf8_lossy(&output).trim().to_owned(),
            "音频驱动已加载，但 ALSA 设备没有出现".to_owned(),
        ]);
        return Err(ProbeError::new("module_voice_alsa_not_ready", detail));
    }

    let (calibration_output, calibration_status) = adb
        .shell_checked_timeout(MODULE_VOICE_CALIBRATION_COMMAND, Duration::from_secs(25))
        .map_err(|error| {
            ProbeError::new(
                "module_voice_calibration_failed",
                format!("模块 VoLTE ACDB 校准服务没有就绪：{error}"),
            )
        })?;
    if calibration_status != 0 {
        let log_command = format!(
            "test ! -f '{MODULE_VOICE_CALIBRATION_LOG}' || tail -c 3500 '{MODULE_VOICE_CALIBRATION_LOG}'"
        );
        let logs = adb
            .shell_checked_timeout(&log_command, Duration::from_secs(8))
            .map(|(logs, _)| logs)
            .unwrap_or_default();
        let detail = first_nonempty_module_voice(&[
            String::from_utf8_lossy(&logs).trim().to_owned(),
            String::from_utf8_lossy(&calibration_output)
                .trim()
                .to_owned(),
            "模块 VoLTE ACDB 校准服务没有就绪".to_owned(),
        ]);
        return Err(ProbeError::new("module_voice_calibration_failed", detail));
    }

    let (output, status) = adb
        .shell_checked_timeout(
            "test -c /dev/ttyGS0 && test -p /run/voc_svr",
            Duration::from_secs(8),
        )
        .map_err(|error| {
            ProbeError::new(
                "module_voice_interfaces_not_ready",
                format!("模块缺少 ttyGS0 或 voc_svr，无法建立 USB 通话路由：{error}"),
            )
        })?;
    if status != 0 {
        return Err(ProbeError::new(
            "module_voice_interfaces_not_ready",
            first_nonempty_module_voice(&[
                String::from_utf8_lossy(&output).trim().to_owned(),
                "模块缺少 ttyGS0 或 voc_svr，无法建立 USB 通话路由".to_owned(),
            ]),
        ));
    }

    let helper_check = format!("'{MODULE_VOICE_HELPER}' --check");
    let (output, status) = adb
        .shell_checked_timeout(&helper_check, Duration::from_secs(15))
        .map_err(|error| {
            ProbeError::new(
                "module_voice_helper_check_failed",
                format!("模块 PCM 桥自检失败：{error}"),
            )
        })?;
    if status != 0 {
        return Err(ProbeError::new(
            "module_voice_helper_check_failed",
            first_nonempty_module_voice(&[
                String::from_utf8_lossy(&output).trim().to_owned(),
                "模块 PCM 桥自检失败".to_owned(),
            ]),
        ));
    }
    Ok(())
}

fn module_voice_sound_device_checks() -> &'static str {
    "test -c '/dev/snd/controlC0' && test -c '/dev/snd/pcmC0D4p' && test -c '/dev/snd/pcmC0D4c' && test -c '/dev/snd/pcmC0D5p' && test -c '/dev/snd/pcmC0D6c' && grep -Fq 'mdm9607-tomtom-i2s-snd-card' /proc/asound/cards"
}

const MODULE_VOICE_CALIBRATION_COMMAND: &str = r#"owned=0; if test -s '/run/proletarius-alsaucm.pid'; then read pid expected_start < '/run/proletarius-alsaucm.pid' || true; current_start=$(cut -d ' ' -f 22 "/proc/$pid/stat" 2>/dev/null); argv0=$(tr '\000' '\n' < "/proc/$pid/cmdline" 2>/dev/null | sed -n '1p'); test "$current_start" = "$expected_start" && test "$argv0" = /usr/bin/alsaucm_test && owned=1 || true; fi; if test "$owned" -eq 0; then for proc in /proc/[0-9]*; do test -r "$proc/cmdline" || continue; argv0=$(tr '\000' '\n' < "$proc/cmdline" 2>/dev/null | sed -n '1p'); test "$argv0" = /usr/bin/alsaucm_test || continue; oldpid=${proc##*/}; kill -TERM "$oldpid" 2>/dev/null || true; n=0; while kill -0 "$oldpid" 2>/dev/null && test "$n" -lt 30; do sleep 0.1; n=$((n+1)); done; kill -0 "$oldpid" 2>/dev/null && exit 71 || true; done; rm -f /run/alsaucm_test '/run/proletarius-alsaucm.pid' '/run/proletarius-alsaucm.log'; nohup /usr/bin/alsaucm_test </dev/null >> '/run/proletarius-alsaucm.log' 2>&1 & pid=$!; starttime=$(cut -d ' ' -f 22 "/proc/$pid/stat" 2>/dev/null); printf '%s %s\n' "$pid" "$starttime" > '/run/proletarius-alsaucm.pid'; n=0; while test "$n" -lt 50 && test ! -p /run/alsaucm_test; do kill -0 "$pid" 2>/dev/null || exit 72; sleep 0.1; n=$((n+1)); done; test -p /run/alsaucm_test || exit 73; fi; if ! grep -q 'ACDB -> Sent VocProc Cal!' '/run/proletarius-alsaucm.log' 2>/dev/null; then printf 'open snd_soc_msm_9x07_Tomtom_I2S\n' > /run/alsaucm_test; printf 'set _verb VoLTE\n' > /run/alsaucm_test; printf 'set _enadev Auxpcm Rx\n' > /run/alsaucm_test; printf 'set _enadev Auxpcm Tx\n' > /run/alsaucm_test; n=0; while test "$n" -lt 100; do grep -q 'ACDB -> Sent VocProc Cal!' '/run/proletarius-alsaucm.log' 2>/dev/null && break; sleep 0.1; n=$((n+1)); done; fi; grep -q 'ACDB -> Sent VocProc Cal!' '/run/proletarius-alsaucm.log'"#;

fn first_nonempty_module_voice(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "模块语音运行时准备失败".to_owned())
}

fn join_nonempty_module_voice(values: &[String]) -> String {
    values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn acquire_interface_lock(
    data_root: &Path,
) -> Result<crate::call_mode_backups::OperationFileLock, ProbeError> {
    std::fs::create_dir_all(data_root).map_err(|_| {
        ProbeError::new(
            "module_adb_lock_unavailable",
            "Switch 数据目录不可用，无法锁定 ADB",
        )
    })?;
    crate::call_mode_backups::try_adb_interface_file_lock(data_root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            ProbeError::new(
                "module_adb_interface_busy",
                "模块 USB ADB 接口正由其他操作使用",
            )
        } else {
            ProbeError::new("module_adb_lock_unavailable", "无法打开模块 USB ADB 接口锁")
        }
    })
}

trait BulkIo {
    fn close_confirmed(&mut self) -> Result<(), ProbeError> {
        Err(ProbeError::new(
            "module_adb_close_unconfirmed",
            "USB ADB close未确认",
        ))
    }
    fn read_bulk(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, ProbeError>;
    fn write_bulk(&mut self, payload: &[u8], timeout: Duration) -> Result<usize, ProbeError>;
}

impl<T: BulkIo + ?Sized> BulkIo for Box<T> {
    fn close_confirmed(&mut self) -> Result<(), ProbeError> {
        (**self).close_confirmed()
    }
    fn read_bulk(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, ProbeError> {
        (**self).read_bulk(buffer, timeout)
    }

    fn write_bulk(&mut self, payload: &[u8], timeout: Duration) -> Result<usize, ProbeError> {
        (**self).write_bulk(payload, timeout)
    }
}

struct AdbProbe<'a, U: BulkIo> {
    usb: &'a mut U,
    remote_max_payload: usize,
    next_local_id: u32,
    operation_deadline: Option<Instant>,
}

impl<'a, U: BulkIo> AdbProbe<'a, U> {
    fn new(usb: &'a mut U) -> Self {
        Self {
            usb,
            remote_max_payload: MAX_PAYLOAD,
            next_local_id: 1,
            operation_deadline: None,
        }
    }

    fn with_deadline(usb: &'a mut U, deadline: Instant) -> Self {
        let mut probe = Self::new(usb);
        probe.operation_deadline = Some(deadline);
        probe
    }

    fn run(&mut self) -> Result<(), ProbeError> {
        let token = random_token()?;
        self.run_with_token(&token)
    }

    fn run_with_token(&mut self, token: &str) -> Result<(), ProbeError> {
        self.connect()?;
        let (output, status) = self.shell_checked_with_token("id -u", token)?;
        let root_shell = std::str::from_utf8(&output)
            .is_ok_and(|text| text.split_ascii_whitespace().any(|field| field == "0"));
        if status != 0 || !root_shell {
            return Err(ProbeError::new(
                "module_adb_root_required",
                "模块 ADB 未返回 root shell",
            ));
        }
        Ok(())
    }

    fn shell_checked(&mut self, command: &str) -> Result<(Vec<u8>, u32), ProbeError> {
        self.shell_checked_timeout(command, ADB_STREAM_OPEN_TIMEOUT)
    }

    fn shell_checked_timeout(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        let token = random_token()?;
        self.shell_checked_with_token_timeout(command, &token, timeout)
    }

    fn shell_checked_with_token(
        &mut self,
        command: &str,
        token: &str,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        self.shell_checked_with_token_timeout(command, token, ADB_STREAM_OPEN_TIMEOUT)
    }

    fn shell_checked_with_token_timeout(
        &mut self,
        command: &str,
        token: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        let service = format!(
            "shell:{{ {command}; }}; __switch_status=$?; printf '\\n__SWITCH_ADB_STATUS_{token}_%u__\\n' \"$__switch_status\""
        );
        self.shell_timeout(&service, token, timeout)
    }

    fn connect(&mut self) -> Result<(), ProbeError> {
        let mut banner = b"host::Switch".to_vec();
        banner.push(0);
        self.send(ADB_CNXN, ADB_VERSION, MAX_PAYLOAD as u32, &banner)?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut stale_messages = 0;
        while Instant::now() < deadline {
            let message = self.receive(deadline)?;
            match message.command {
                ADB_AUTH => {
                    return Err(ProbeError::new(
                        "module_adb_authentication_required",
                        "模块 ADB 要求认证，无法自动部署通话运行时",
                    ));
                }
                ADB_CNXN if message.arg1 > 0 => {
                    self.remote_max_payload = (message.arg1 as usize).min(MAX_PAYLOAD);
                    return Ok(());
                }
                ADB_CNXN => {}
                ADB_WRTE | ADB_OKAY | ADB_CLSE => {
                    stale_messages += 1;
                    if stale_messages > 64 {
                        return Err(ProbeError::new(
                            "module_adb_stale_streams",
                            "ADB 旧数据流过多，无法安全探测",
                        ));
                    }
                    if message.arg0 != 0 && message.arg1 != 0 {
                        self.send(ADB_CLSE, message.arg1, message.arg0, &[])?;
                    }
                    self.send(ADB_CNXN, ADB_VERSION, MAX_PAYLOAD as u32, &banner)?;
                }
                _ => {
                    return Err(ProbeError::new(
                        "module_adb_protocol_invalid",
                        "模块未接受 ADB CNXN",
                    ));
                }
            }
        }
        Err(ProbeError::new(
            "module_adb_probe_timeout",
            "等待模块接受 ADB CNXN 超时",
        ))
    }

    fn shell_timeout(
        &mut self,
        service: &str,
        expected_token: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        if service.len() + 1 > self.remote_max_payload {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB shell 服务名超出设备限制",
            ));
        }
        let local_id = self.next_local_id.max(1);
        self.next_local_id = local_id.wrapping_add(1).max(1);
        let mut payload = service.as_bytes().to_vec();
        payload.push(0);
        self.send(ADB_OPEN, local_id, 0, &payload)?;
        let deadline = Instant::now() + timeout;
        let open_deadline = deadline;
        let remote_id = loop {
            if Instant::now() >= open_deadline {
                return Err(ProbeError::new(
                    "module_adb_probe_timeout",
                    "等待模块打开 ADB shell 超时",
                ));
            }
            let message = self.receive(open_deadline)?;
            match message.command {
                ADB_OKAY
                    if message.arg0 != 0
                        && message.arg1 == local_id
                        && message.payload.is_empty() =>
                {
                    break message.arg0;
                }
                ADB_CNXN if message.arg1 > 0 => {
                    self.remote_max_payload = (message.arg1 as usize).min(MAX_PAYLOAD)
                }
                ADB_CLSE if message.arg1 == local_id => {
                    return Err(ProbeError::new(
                        "module_adb_shell_rejected",
                        "模块拒绝 ADB shell 服务",
                    ));
                }
                ADB_WRTE | ADB_OKAY | ADB_CLSE if message.arg0 != 0 && message.arg1 != 0 => {
                    self.send(ADB_CLSE, message.arg1, message.arg0, &[])?
                }
                _ => {
                    return Err(ProbeError::new(
                        "module_adb_protocol_invalid",
                        "模块返回无效 ADB shell 响应",
                    ));
                }
            }
        };
        let mut output = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return Err(ProbeError::new(
                    "module_adb_probe_timeout",
                    "等待模块 shell 结束超时",
                ));
            }
            let message = self.receive(deadline)?;
            match message.command {
                ADB_WRTE if message.arg0 == remote_id && message.arg1 == local_id => {
                    if output.len().saturating_add(message.payload.len()) > MAX_OUTPUT_BYTES {
                        return Err(ProbeError::new(
                            "module_adb_protocol_invalid",
                            "模块 ADB shell 输出超出大小限制",
                        ));
                    }
                    output.extend_from_slice(&message.payload);
                    self.send(ADB_OKAY, local_id, remote_id, &[])?;
                }
                ADB_CLSE if message.arg1 == local_id => {
                    if message.arg0 != 0 {
                        self.send(ADB_CLSE, local_id, remote_id, &[])?;
                    }
                    return parse_shell_result(&output, expected_token);
                }
                ADB_CNXN | ADB_WRTE | ADB_OKAY | ADB_CLSE => {}
                _ => {
                    return Err(ProbeError::new(
                        "module_adb_protocol_invalid",
                        "模块返回未知 ADB shell 数据包",
                    ));
                }
            }
        }
    }

    fn push(&mut self, data: &[u8], remote_path: &str, mode: u32) -> Result<(), ProbeError> {
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(u64::from(u32::MAX)) as u32;
        self.push_with_mtime(data, remote_path, mode, mtime)
    }

    fn push_with_mtime(
        &mut self,
        data: &[u8],
        remote_path: &str,
        mode: u32,
        mtime: u32,
    ) -> Result<(), ProbeError> {
        if remote_path.is_empty() || remote_path.contains(',') || remote_path.contains('\0') {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB push 目标路径无效",
            ));
        }
        if data.len() as u64 > crate::call_mode_runtime_manifest::MAX_ARTIFACT_BYTES {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB push 文件超出大小限制",
            ));
        }
        let stream = self.open_service("sync:")?;
        let result = self.push_on_stream(stream, data, remote_path, mode, mtime);
        if result.is_err() {
            let _ = self.close_stream(stream);
        }
        result
    }

    fn push_on_stream(
        &mut self,
        stream: AdbStream,
        data: &[u8],
        remote_path: &str,
        mode: u32,
        mtime: u32,
    ) -> Result<(), ProbeError> {
        let send_name = format!("{remote_path},{mode}");
        self.write_sync_packet(stream, *b"SEND", send_name.as_bytes())?;
        let chunk_capacity = self.remote_max_payload.checked_sub(8).ok_or_else(|| {
            ProbeError::new("module_adb_protocol_invalid", "ADB sync 数据块长度无效")
        })?;
        if chunk_capacity == 0 {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB sync 数据块长度无效",
            ));
        }
        for chunk in data.chunks(chunk_capacity) {
            self.write_sync_packet(stream, *b"DATA", chunk)?;
        }
        self.write_stream(stream, &sync_done_packet(mtime))?;
        self.read_sync_result(stream)?;
        self.close_stream(stream)
    }

    fn write_sync_packet(
        &mut self,
        stream: AdbStream,
        identifier: [u8; 4],
        payload: &[u8],
    ) -> Result<(), ProbeError> {
        let packet_length = 8usize
            .checked_add(payload.len())
            .ok_or_else(|| ProbeError::new("module_adb_protocol_invalid", "ADB sync 数据块过大"))?;
        if packet_length > self.remote_max_payload {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB sync 数据块过大",
            ));
        }
        // A sync DATA frame may carry the private supervisor key.
        let mut packet = zeroize::Zeroizing::new(Vec::with_capacity(packet_length));
        packet.extend_from_slice(&identifier);
        packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        packet.extend_from_slice(payload);
        self.write_stream(stream, &packet)
    }

    fn write_stream(&mut self, stream: AdbStream, payload: &[u8]) -> Result<(), ProbeError> {
        if payload.len() > self.remote_max_payload {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB sync 数据块过大",
            ));
        }
        self.send(ADB_WRTE, stream.local_id, stream.remote_id, payload)?;
        let deadline = Instant::now() + ADB_STREAM_ACK_TIMEOUT;
        let message = self.receive(deadline)?;
        if message.command != ADB_OKAY
            || message.arg0 != stream.remote_id
            || message.arg1 != stream.local_id
            || !message.payload.is_empty()
        {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "模块未确认 ADB 数据块",
            ));
        }
        Ok(())
    }

    fn read_sync_result(&mut self, stream: AdbStream) -> Result<(), ProbeError> {
        let deadline = Instant::now() + ADB_SYNC_RESPONSE_TIMEOUT;
        let mut response = Vec::new();
        let mut expected_length = None;
        while Instant::now() < deadline {
            let message = self.receive(deadline)?;
            match message.command {
                ADB_WRTE if message.arg0 == stream.remote_id && message.arg1 == stream.local_id => {
                    if response.len().saturating_add(message.payload.len())
                        > ADB_SYNC_RESPONSE_LIMIT
                    {
                        return Err(ProbeError::new(
                            "module_adb_protocol_invalid",
                            "ADB sync 响应超出大小限制",
                        ));
                    }
                    response.extend_from_slice(&message.payload);
                    self.send(ADB_OKAY, stream.local_id, stream.remote_id, &[])?;
                    if response.len() >= 8 && expected_length.is_none() {
                        let identifier = &response[..4];
                        let length =
                            u32::from_le_bytes(response[4..8].try_into().unwrap()) as usize;
                        if identifier == b"OKAY" {
                            if length != 0 || response.len() != 8 {
                                return Err(ProbeError::new(
                                    "module_adb_protocol_invalid",
                                    "ADB sync 返回无效状态",
                                ));
                            }
                            return Ok(());
                        }
                        if identifier != b"FAIL" || length > ADB_SYNC_RESPONSE_LIMIT - 8 {
                            return Err(ProbeError::new(
                                "module_adb_protocol_invalid",
                                "ADB sync 返回无效状态",
                            ));
                        }
                        expected_length = Some(8 + length);
                    }
                    if let Some(expected) = expected_length {
                        if response.len() >= expected {
                            let detail = String::from_utf8_lossy(&response[8..expected])
                                .chars()
                                .filter(|character| !character.is_control() || *character == '\t')
                                .collect::<String>();
                            return Err(ProbeError::new(
                                "module_adb_file_transfer_failed",
                                format!("模块拒绝文件传输：{detail}"),
                            ));
                        }
                    }
                }
                ADB_CLSE if message.arg1 == stream.local_id => {
                    return Err(ProbeError::new(
                        "module_adb_protocol_invalid",
                        "ADB sync 提前关闭",
                    ));
                }
                _ => {}
            }
        }
        Err(ProbeError::new(
            "module_adb_probe_timeout",
            "等待模块 ADB sync 响应超时",
        ))
    }

    fn open_service(&mut self, service: &str) -> Result<AdbStream, ProbeError> {
        let service_payload = service.as_bytes();
        if service_payload.len() + 1 > self.remote_max_payload {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB 服务命令过长",
            ));
        }
        let local_id = self.next_local_id.max(1);
        self.next_local_id = local_id.wrapping_add(1).max(1);
        let mut payload = service_payload.to_vec();
        payload.push(0);
        self.send(ADB_OPEN, local_id, 0, &payload)?;
        let deadline = Instant::now() + ADB_STREAM_OPEN_TIMEOUT;
        while Instant::now() < deadline {
            let message = self.receive(deadline)?;
            match message.command {
                ADB_OKAY
                    if message.arg0 != 0
                        && message.arg1 == local_id
                        && message.payload.is_empty() =>
                {
                    return Ok(AdbStream {
                        local_id,
                        remote_id: message.arg0,
                    });
                }
                ADB_CNXN if message.arg1 > 0 => {
                    self.remote_max_payload = (message.arg1 as usize).min(MAX_PAYLOAD);
                }
                ADB_CLSE if message.arg1 == local_id => {
                    return Err(ProbeError::new(
                        "module_adb_shell_rejected",
                        "模块拒绝 ADB 服务",
                    ));
                }
                ADB_WRTE | ADB_OKAY | ADB_CLSE if message.arg0 != 0 && message.arg1 != 0 => {
                    self.send(ADB_CLSE, message.arg1, message.arg0, &[])?;
                }
                _ => {
                    return Err(ProbeError::new(
                        "module_adb_protocol_invalid",
                        "模块返回无效 ADB 服务响应",
                    ));
                }
            }
        }
        Err(ProbeError::new(
            "module_adb_probe_timeout",
            "等待模块打开 ADB 服务超时",
        ))
    }

    fn close_stream(&mut self, stream: AdbStream) -> Result<(), ProbeError> {
        self.send(ADB_CLSE, stream.local_id, stream.remote_id, &[])?;
        let deadline = Instant::now() + ADB_STREAM_CLOSE_TIMEOUT;
        while Instant::now() < deadline {
            let message = self.receive(deadline)?;
            if message.command == ADB_CLSE
                && message.arg0 == stream.remote_id
                && message.arg1 == stream.local_id
                && message.payload.is_empty()
            {
                return Ok(());
            }
            if message.command == ADB_WRTE
                && message.arg0 == stream.remote_id
                && message.arg1 == stream.local_id
            {
                self.send(ADB_OKAY, stream.local_id, stream.remote_id, &[])?;
                continue;
            }
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB sync 关闭响应无效",
            ));
        }
        Err(ProbeError::new(
            "module_adb_probe_timeout",
            "等待模块关闭 ADB sync 流超时",
        ))
    }

    fn send(
        &mut self,
        command: u32,
        arg0: u32,
        arg1: u32,
        payload: &[u8],
    ) -> Result<(), ProbeError> {
        if self
            .operation_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ProbeError::new(
                "module_adb_probe_timeout",
                "模块语音运行时部署与准备超过 180 秒",
            ));
        }
        let header = encode_header(command, arg0, arg1, payload)?;
        write_exact(self.usb, &header)?;
        if !payload.is_empty() {
            write_exact(self.usb, payload)?;
        }
        Ok(())
    }

    fn receive(&mut self, deadline: Instant) -> Result<Message, ProbeError> {
        let deadline = self
            .operation_deadline
            .map_or(deadline, |limit| deadline.min(limit));
        let header = read_exact(self.usb, 24, deadline)?;
        let (command, arg0, arg1, size, checksum, magic) = decode_header(&header)?;
        if size as usize > self.remote_max_payload || size as usize > MAX_PAYLOAD {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB 数据包长度超出设备限制",
            ));
        }
        let payload = read_exact(self.usb, size as usize, deadline)?;
        if checksum != adb_checksum(&payload) || magic != command ^ u32::MAX {
            return Err(ProbeError::new(
                "module_adb_protocol_invalid",
                "ADB 数据包校验失败",
            ));
        }
        Ok(Message {
            command,
            arg0,
            arg1,
            payload,
        })
    }
}

#[derive(Clone, Copy)]
struct AdbStream {
    local_id: u32,
    remote_id: u32,
}

struct Message {
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: Vec<u8>,
}

fn sync_done_packet(modification_time: u32) -> [u8; 8] {
    let mut packet = [0u8; 8];
    packet[..4].copy_from_slice(b"DONE");
    packet[4..].copy_from_slice(&modification_time.to_le_bytes());
    packet
}

fn parse_shell_result(bytes: &[u8], token: &str) -> Result<(Vec<u8>, u32), ProbeError> {
    let output = std::str::from_utf8(bytes).map_err(|_| {
        ProbeError::new(
            "module_adb_protocol_invalid",
            "模块 ADB shell 返回了无效文本",
        )
    })?;
    let marker = format!("__SWITCH_ADB_STATUS_{token}_");
    let index = output.rfind(&marker).ok_or_else(|| {
        ProbeError::new("module_adb_protocol_invalid", "模块 shell 没有返回退出状态")
    })?;
    let before = &output[..index];
    let after = &output[index + marker.len()..];
    let suffix = after.find("__").ok_or_else(|| {
        ProbeError::new("module_adb_protocol_invalid", "模块 shell 退出状态格式无效")
    })?;
    let status = after[..suffix].parse::<u32>().map_err(|_| {
        ProbeError::new("module_adb_protocol_invalid", "模块 shell 退出状态超出范围")
    })?;
    if status > 255 {
        return Err(ProbeError::new(
            "module_adb_protocol_invalid",
            "模块 shell 退出状态超出范围",
        ));
    }
    Ok((before.as_bytes().to_vec(), status))
}

fn random_token() -> Result<String, ProbeError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| {
        ProbeError::new(
            "module_adb_probe_unavailable",
            "无法生成 ADB shell 状态标识",
        )
    })?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

const fn command(bytes: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*bytes)
}
fn adb_checksum(payload: &[u8]) -> u32 {
    payload.iter().map(|byte| u32::from(*byte)).sum()
}

fn encode_header(
    command: u32,
    arg0: u32,
    arg1: u32,
    payload: &[u8],
) -> Result<[u8; 24], ProbeError> {
    let size = u32::try_from(payload.len())
        .map_err(|_| ProbeError::new("module_adb_protocol_invalid", "ADB 数据包过大"))?;
    let mut header = [0u8; 24];
    header[0..4].copy_from_slice(&command.to_le_bytes());
    header[4..8].copy_from_slice(&arg0.to_le_bytes());
    header[8..12].copy_from_slice(&arg1.to_le_bytes());
    header[12..16].copy_from_slice(&size.to_le_bytes());
    header[16..20].copy_from_slice(&adb_checksum(payload).to_le_bytes());
    header[20..24].copy_from_slice(&(command ^ u32::MAX).to_le_bytes());
    Ok(header)
}

fn decode_header(header: &[u8]) -> Result<(u32, u32, u32, u32, u32, u32), ProbeError> {
    if header.len() != 24 {
        return Err(ProbeError::new(
            "module_adb_protocol_invalid",
            "ADB header 长度无效",
        ));
    }
    Ok((
        le_u32(&header[0..4]),
        le_u32(&header[4..8]),
        le_u32(&header[8..12]),
        le_u32(&header[12..16]),
        le_u32(&header[16..20]),
        le_u32(&header[20..24]),
    ))
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four byte slice"))
}
fn write_exact<U: BulkIo>(usb: &mut U, bytes: &[u8]) -> Result<(), ProbeError> {
    let written = usb.write_bulk(bytes, Duration::from_secs(2))?;
    if written != bytes.len() {
        return Err(ProbeError::new(
            "module_adb_usb_write_failed",
            "模块 USB ADB 写入不完整",
        ));
    }
    Ok(())
}

fn read_exact<U: BulkIo>(
    usb: &mut U,
    length: usize,
    deadline: Instant,
) -> Result<Vec<u8>, ProbeError> {
    let mut output = Vec::with_capacity(length);
    while output.len() < length && Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        let mut buffer = vec![0; (length - output.len()).min(MAX_PAYLOAD)];
        match usb.read_bulk(&mut buffer, remaining) {
            Ok(count) if count <= buffer.len() => output.extend_from_slice(&buffer[..count]),
            Ok(_) => {
                return Err(ProbeError::new(
                    "module_adb_protocol_invalid",
                    "模块 USB ADB 返回了无效长度",
                ));
            }
            Err(error) if error.code == "module_adb_usb_timeout" => {}
            Err(error) => return Err(error),
        }
    }
    if output.len() != length {
        return Err(ProbeError::new(
            "module_adb_probe_timeout",
            "等待模块 ADB 数据超时",
        ));
    }
    Ok(output)
}

/// A target is selected once for one worker plan. It never resolves a bus/
/// address string back through enumeration: native implementations retain the
/// libusb Device reference (and its Context) belonging to that connection.
trait AdbTarget: Send {
    fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError>;
    fn capture_managed_restore_helper(
        &mut self,
        _original: &crate::usb_at_transport::PhysicalTarget,
        _deadline: Instant,
        _pid_path: &str,
        _current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<recovery_binding::NativeHelperObservation, ProbeError> {
        Err(ProbeError::new(
            "module_voice_capture_unbound",
            "没有原生managed语音观察能力",
        ))
    }
    /// Read only this already selected connection. Ordinary target opening and
    /// all existing Start/Stop paths retain their original behavior.
    fn capture_restore_helper(
        &mut self,
        _original: &crate::usb_at_transport::PhysicalTarget,
        _deadline: Instant,
        _current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<recovery_binding::NativeHelperObservation, ProbeError> {
        Err(ProbeError::new(
            "module_voice_capture_unbound",
            "原语音计划没有原生物理观察能力",
        ))
    }
}

const TARGET_DISCONNECTED: &str = "module_adb_target_disconnected";

/// Exact connection shared by preparation stages and retained route obligations.
/// It is never reused across controlled reboot or replaced after disappearance.
/// A confirmed disappearance invalidates this captured connection. Ordinary
/// I/O/protocol failures do not revoke its exact cleanup right: existing stop/
/// rollback may reopen this same target, without selecting a replacement.
#[derive(Clone)]
struct AdbPlan(Arc<Mutex<SelectedAdbPlan>>);
struct SelectedAdbPlan {
    target: Box<dyn AdbTarget>,
    disconnected: Option<ProbeError>,
}
impl AdbPlan {
    fn select() -> Result<Self, ProbeError> {
        Ok(Self::from_target(select_adb_target()?))
    }
    fn select_for_capture(
        target: Option<&crate::usb_at_transport::PhysicalTarget>,
    ) -> Result<Self, ProbeError> {
        Ok(Self::from_target(select_captured_adb_target(
            target,
            adb_target_from_physical,
            select_adb_target,
        )?))
    }
    fn from_physical_target(
        target: &crate::usb_at_transport::PhysicalTarget,
    ) -> Result<Self, ProbeError> {
        Self::select_for_capture(Some(target))
    }
    fn from_target(target: Box<dyn AdbTarget>) -> Self {
        Self(Arc::new(Mutex::new(SelectedAdbPlan {
            target,
            disconnected: None,
        })))
    }
    fn ensure_connected(&self) -> Result<(), ProbeError> {
        let selected = self.0.lock().map_err(|_| {
            ProbeError::new("module_adb_target_unconfirmed", "原 ADB 设备计划状态未确认")
        })?;
        match &selected.disconnected {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
    fn with_usb_checked<T>(
        &self,
        run: impl FnOnce(&mut PlanBulk<'_>) -> Result<T, ProbeError>,
    ) -> Result<T, ProbeError> {
        self.with_usb(|usb| {
            let result = run(usb);
            let closed = usb.usb.close_confirmed();
            match closed {
                Err(error) => Err(error),
                Ok(()) => result,
            }
        })
    }
    fn with_usb<T>(
        &self,
        run: impl FnOnce(&mut PlanBulk<'_>) -> Result<T, ProbeError>,
    ) -> Result<T, ProbeError> {
        let mut selected = self.0.lock().map_err(|_| {
            ProbeError::new("module_adb_target_unconfirmed", "原 ADB 设备计划状态未确认")
        })?;
        if let Some(error) = &selected.disconnected {
            return Err(error.clone());
        }
        let usb = match selected.target.open() {
            Ok(usb) => usb,
            Err(error) => {
                if error.code == TARGET_DISCONNECTED {
                    selected.disconnected = Some(error.clone());
                }
                return Err(error);
            }
        };
        run(&mut PlanBulk {
            usb,
            disconnected: &mut selected.disconnected,
        })
    }
}
struct PlanBulk<'a> {
    usb: Box<dyn BulkIo>,
    disconnected: &'a mut Option<ProbeError>,
}
impl PlanBulk<'_> {
    fn record<T>(&mut self, result: Result<T, ProbeError>) -> Result<T, ProbeError> {
        if let Err(error) = &result
            && error.code == TARGET_DISCONNECTED
        {
            *self.disconnected = Some(error.clone());
        }
        result
    }
}
impl BulkIo for PlanBulk<'_> {
    fn read_bulk(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, ProbeError> {
        if let Some(error) = self.disconnected.as_ref() {
            return Err(error.clone());
        }
        let result = self.usb.read_bulk(buffer, timeout);
        self.record(result)
    }
    fn write_bulk(&mut self, payload: &[u8], timeout: Duration) -> Result<usize, ProbeError> {
        if let Some(error) = self.disconnected.as_ref() {
            return Err(error.clone());
        }
        let result = self.usb.write_bulk(payload, timeout);
        self.record(result)
    }
}

/// Inspect the complete supported candidate set before opening any handle.
/// There is no "first openable" fallback when one candidate cannot be claimed.
fn unique_adb_target<T>(candidates: Vec<T>) -> Result<T, ProbeError> {
    match candidates.len() {
        0 => Err(ProbeError::new(
            "module_adb_device_unavailable",
            "未找到支持的模块 USB ADB 接口",
        )),
        1 => Ok(candidates.into_iter().next().expect("one candidate")),
        _ => Err(ProbeError::new(
            "module_adb_device_ambiguous",
            "发现多个支持的模块 USB ADB 设备，无法唯一确认本次操作目标",
        )),
    }
}

/// An exact capture's error is final. The legacy unbound selector is called
/// only when there was genuinely no opened AT session at capture time.
fn select_captured_adb_target<T>(
    captured: Option<T>,
    exact: impl FnOnce(T) -> Result<Box<dyn AdbTarget>, ProbeError>,
    unbound: impl FnOnce() -> Result<Box<dyn AdbTarget>, ProbeError>,
) -> Result<Box<dyn AdbTarget>, ProbeError> {
    match captured {
        Some(target) => exact(target),
        None => unbound(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdbInterface {
    interface: u8,
    alternate: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}

fn adb_interface(
    interface: u8,
    alternate: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    endpoints: impl IntoIterator<Item = (u8, bool)>,
) -> Option<AdbInterface> {
    if (class, subclass, protocol) != (0xff, 0x42, 0x01) {
        return None;
    }
    let (mut endpoint_in, mut endpoint_out) = (None, None);
    for (address, bulk) in endpoints {
        if !bulk || address & 0x0f == 0 {
            continue;
        }
        if address & 0x80 != 0 {
            endpoint_in = Some(address);
        } else {
            endpoint_out = Some(address);
        }
    }
    Some(AdbInterface {
        interface,
        alternate,
        endpoint_in: endpoint_in?,
        endpoint_out: endpoint_out?,
    })
}

#[cfg(target_os = "macos")]
fn adb_interface_on_device(
    device: rusb::Device<rusb::Context>,
) -> Result<Option<NativeAdbTarget>, ProbeError> {
    use rusb::TransferType;
    let configuration = device.active_config_descriptor().map_err(|error| {
        native_adb_error(
            error,
            "module_adb_device_unavailable",
            "无法核对原模块 USB ADB 接口",
        )
    })?;
    for interface in configuration.interfaces() {
        for alternate in interface.descriptors() {
            if let Some(candidate) = adb_interface(
                alternate.interface_number(),
                alternate.setting_number(),
                alternate.class_code(),
                alternate.sub_class_code(),
                alternate.protocol_code(),
                alternate.endpoint_descriptors().map(|endpoint| {
                    (
                        endpoint.address(),
                        endpoint.transfer_type() == TransferType::Bulk,
                    )
                }),
            ) {
                return Ok(Some(NativeAdbTarget {
                    device,
                    interface: candidate.interface,
                    alternate: candidate.alternate,
                    endpoint_in: candidate.endpoint_in,
                    endpoint_out: candidate.endpoint_out,
                }));
            }
        }
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn adb_target_from_physical(
    target: &crate::usb_at_transport::PhysicalTarget,
) -> Result<Box<dyn AdbTarget>, ProbeError> {
    // This is the Device retained from the opened AT handle, even after AT
    // closes. A reboot/re-enumeration must not substitute any new connection.
    let device = target
        .native_device()
        .map_err(|error| ProbeError::new(error.code, error.message))?;
    let descriptor = device.device_descriptor().map_err(|error| {
        native_adb_error(
            error,
            "module_adb_target_unconfirmed",
            "无法核对原 AT 连接的 USB ADB 设备",
        )
    })?;
    if !matches!(
        (descriptor.vendor_id(), descriptor.product_id()),
        (0x2ca3, 0x4006) | (0x2c7c, 0x0125)
    ) {
        return Err(ProbeError::new(
            "module_adb_target_unconfirmed",
            "原 AT 连接不是支持的 USB ADB 设备",
        ));
    }
    match adb_interface_on_device(device)? {
        Some(target) => Ok(Box::new(target)),
        None => Err(ProbeError::new(
            "module_adb_target_unconfirmed",
            "原 AT 连接未提供可确认的 USB ADB 接口",
        )),
    }
}

#[cfg(not(target_os = "macos"))]
fn adb_target_from_physical(
    _: &crate::usb_at_transport::PhysicalTarget,
) -> Result<Box<dyn AdbTarget>, ProbeError> {
    Err(ProbeError::new(
        "module_adb_unsupported",
        "模块 USB ADB 物理连接绑定仅支持 macOS",
    ))
}

#[cfg(target_os = "macos")]
fn native_adb_error(error: rusb::Error, code: &'static str, message: &str) -> ProbeError {
    if error == rusb::Error::NoDevice {
        ProbeError::new(TARGET_DISCONNECTED, "本次 ADB 计划的原 USB 设备已断开")
    } else {
        ProbeError::new(code, message)
    }
}

#[cfg(target_os = "macos")]
struct NativeAdbTarget {
    device: rusb::Device<rusb::Context>,
    interface: u8,
    alternate: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}
#[cfg(target_os = "macos")]
struct LibusbADB {
    claimed: bool,
    handle: rusb::DeviceHandle<rusb::Context>,
    interface: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}
#[cfg(target_os = "macos")]
impl Drop for LibusbADB {
    fn drop(&mut self) {
        if self.claimed {
            let _ = self.handle.release_interface(self.interface);
        }
    }
}
#[cfg(target_os = "macos")]
impl BulkIo for LibusbADB {
    fn close_confirmed(&mut self) -> Result<(), ProbeError> {
        let result = self.handle.release_interface(self.interface);
        self.claimed = false;
        result.map_err(|_| {
            ProbeError::new(
                "module_adb_close_unconfirmed",
                "原ADB interface release未确认",
            )
        })
    }
    fn read_bulk(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, ProbeError> {
        self.handle
            .read_bulk(self.endpoint_in, buffer, timeout)
            .map_err(|error| {
                if error == rusb::Error::Timeout {
                    ProbeError::new("module_adb_usb_timeout", "USB ADB read timed out")
                } else {
                    native_adb_error(
                        error,
                        "module_adb_device_unavailable",
                        "USB ADB read failed",
                    )
                }
            })
    }
    fn write_bulk(&mut self, payload: &[u8], timeout: Duration) -> Result<usize, ProbeError> {
        self.handle
            .write_bulk(self.endpoint_out, payload, timeout)
            .map_err(|error| {
                native_adb_error(error, "module_adb_usb_write_failed", "USB ADB write failed")
            })
    }
}
#[cfg(target_os = "macos")]
impl AdbTarget for NativeAdbTarget {
    fn capture_managed_restore_helper(
        &mut self,
        original: &crate::usb_at_transport::PhysicalTarget,
        deadline: Instant,
        pid_path: &str,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<recovery_binding::NativeHelperObservation, ProbeError> {
        recovery_binding::capture_native_with_pid(self, original, deadline, pid_path, current)
    }
    fn capture_restore_helper(
        &mut self,
        original: &crate::usb_at_transport::PhysicalTarget,
        deadline: Instant,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<recovery_binding::NativeHelperObservation, ProbeError> {
        recovery_binding::capture_native(self, original, deadline, current)
    }
    fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
        // This retained Device is the original libusb connection object. Do
        // not enumerate again, even if the same port/address now has another device.
        let handle = self.device.open().map_err(|error| {
            native_adb_error(
                error,
                "module_adb_device_unavailable",
                "无法打开原模块 USB ADB 设备",
            )
        })?;
        handle.claim_interface(self.interface).map_err(|error| {
            native_adb_error(
                error,
                "module_adb_device_unavailable",
                "无法占用原模块 USB ADB interface",
            )
        })?;
        if self.alternate != 0 {
            if let Err(error) = handle.set_alternate_setting(self.interface, self.alternate) {
                let _ = handle.release_interface(self.interface);
                return Err(native_adb_error(
                    error,
                    "module_adb_device_unavailable",
                    "无法选择原模块 USB ADB alternate setting",
                ));
            }
        }
        Ok(Box::new(LibusbADB {
            claimed: true,
            handle,
            interface: self.interface,
            endpoint_in: self.endpoint_in,
            endpoint_out: self.endpoint_out,
        }))
    }
}

#[cfg(target_os = "macos")]
fn select_adb_target() -> Result<Box<dyn AdbTarget>, ProbeError> {
    use rusb::UsbContext;
    let context = rusb::Context::new().map_err(|_| {
        ProbeError::new(
            "module_adb_device_unavailable",
            "USB ADB context unavailable",
        )
    })?;
    let devices = context.devices().map_err(|_| {
        ProbeError::new(
            "module_adb_device_unavailable",
            "USB ADB device enumeration failed",
        )
    })?;
    let mut candidates = Vec::new();
    for device in devices.iter() {
        // Incomplete enumeration cannot establish uniqueness. No device has
        // been opened/claimed at this stage, including on these error paths.
        let descriptor = device.device_descriptor().map_err(|error| {
            native_adb_error(
                error,
                "module_adb_device_unavailable",
                "无法完整核对 USB ADB 候选设备",
            )
        })?;
        if !matches!(
            (descriptor.vendor_id(), descriptor.product_id()),
            (0x2ca3, 0x4006) | (0x2c7c, 0x0125)
        ) {
            continue;
        }
        if let Some(candidate) = adb_interface_on_device(device)? {
            candidates.push(candidate);
        }
    }
    Ok(Box::new(unique_adb_target(candidates)?))
}

#[cfg(not(target_os = "macos"))]
fn select_adb_target() -> Result<Box<dyn AdbTarget>, ProbeError> {
    Err(ProbeError::new(
        "module_adb_unsupported",
        "模块 USB ADB 探测仅支持 macOS",
    ))
}

#[cfg(test)]
mod prepare_tests;
#[cfg(test)]
mod quiesce_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_tests;

#[cfg(test)]
pub(crate) mod child_tests;
