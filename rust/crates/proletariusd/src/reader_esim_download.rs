//! Rust owner for reader profile downloads.
//!
//! Celmux's bounded per-device operation lifecycle informed this service,
//! while protocol and UI contracts remain Switch-owned. BLE uses the Swift
//! CoreBluetooth bridge. Rust owns reader selection, leases, lifecycle and LPA;
//! module writers share the native operation journal and physical reservations.

mod notifications;

#[cfg(test)]
mod notification_runtime_tests {
    include!("reader_esim_download/notification_runtime_tests.rs");
}

use std::{
    collections::HashMap,
    fmt,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

use bytes::Bytes;
use hmac::{Hmac, Mac};
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_ccid::Error as CcidError;
use proletarius_euicc::{
    CardTransport, Error as EuiccError, Euicc, KNOWN_ISDR_APPLICATION_AIDS, MAX_NOTIFICATION_COUNT,
    MAX_PROFILE_COUNT, NotificationInfo, ProductField, ProfileClass, ProfileIdentifier,
    ProfileInfo, ProfileOperation, ProfileOperationResult, ProfileState, encode_iccid,
};
use proletarius_lpa::{
    ActivationCode, DownloadObserver, DownloadStage, Es9Transport, HttpEs9Client,
    NOTIFICATION_TIMEOUT, ProfileMetadata, RspCard,
};
use proletarius_reader::{CardPresence, DiscoverySnapshot, euicc_lpa::open_lpa_client};
use proletarius_sim::ApduTransport;
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    ServerState, b0_auth, json_response,
    reader_session_lease::{
        NativeCardContext, NativeReaderSessionHeartbeat, NativeReaderSessionLease,
    },
};

const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024;
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const READER_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(30);
const FREE_NVRAM_GUARD: u64 = 80 * 1024;
const READER_MSS: usize = 240;
const MAX_MODULE_DOWNLOAD_REQUEST_BYTES: usize = 1 << 20;
const OPERATION_JOURNAL_VERSION: u8 = 2;
const CARD_PROOF_CONTEXT: &[u8] = b"Switch/eSIM/card-recovery/v1\0";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DownloadRequest {
    #[serde(default)]
    client_request_id: String,
    #[serde(default)]
    smdp: String,
    #[serde(default)]
    matching_id: String,
    #[serde(default)]
    confirmation_code: String,
    #[serde(default)]
    aid: String,
    #[serde(default)]
    imei: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RenameProfileRequest {
    #[serde(default)]
    client_request_id: String,
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    aid: String,
    #[serde(default)]
    name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DeleteProfileRequest {
    #[serde(default)]
    client_request_id: String,
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    aid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SwitchProfileRequest {
    #[serde(default)]
    client_request_id: String,
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    aid: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryNotificationRequest {
    client_request_id: String,
    sequence_number: i64,
    #[serde(default)]
    aid: String,
}

#[derive(Clone, Debug, Serialize)]
struct RetryNotificationResponse {
    message: String,
    sequence_number: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct DeleteProfileResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) warning_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space_delta: Option<SpaceDelta>,
}

#[derive(Clone, Debug, Serialize)]
struct SwitchProfileResponse {
    switch_accepted: bool,
    phase: String,
    target_iccid: String,
    recovery_pending: bool,
    module_reboot_requested: bool,
    module_reboot_response: String,
    module_reboot_warning: String,
    reconnect_wait_seconds: i32,
    reader_refresh_warning: String,
    reader_refresh_warning_code: String,
    reader_info: Value,
    operation: OperationSnapshot,
}

#[derive(Clone, Debug, Deserialize)]
struct ConfirmationRequest {
    operation_id: String,
    confirm: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeLegacyRequest {
    operation_id: String,
    expected_updated_at: String,
    expected_state: String,
    expected_error_code: String,
    legacy_manual_ack: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationSnapshot {
    id: String,
    kind: String,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    progress: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    estimated_profile_size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_iccid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
    #[serde(default)]
    recoverable: bool,
    #[serde(default)]
    refresh_after_seconds: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<DownloadResult>,
    started_at: String,
    updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DownloadResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    space_delta: Option<SpaceDelta>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SpaceDelta {
    direction: String,
    bytes: i64,
}

/// This wrapper is disk-only. It must never be embedded in an HTTP response.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedOperation {
    schema_version: u8,
    snapshot: OperationSnapshot,
    proof: CardProof,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolution: Option<PrivateResolution>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CardProof {
    NotArmed,
    Armed { digest_hex: String },
    LegacyUnknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PrivateResolution {
    ManualAcknowledged,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CardProofKeyRecord {
    schema_version: u8,
    key_hex: String,
}

#[derive(Default)]
struct Service {
    state: Arc<Mutex<Option<OperationRecord>>>,
    journal: Option<ArchiveStore>,
    proof_key: Option<Arc<Zeroizing<[u8; 32]>>>,
    journal_faulted: Arc<AtomicBool>,
    operation_gate: Arc<AtomicBool>,
    shutting_down: Arc<AtomicBool>,
}

struct OperationRecord {
    snapshot: OperationSnapshot,
    proof: CardProof,
    resolution: Option<PrivateResolution>,
    confirmation: Option<SyncSender<bool>>,
    // Only an actual native reader lease can establish card-specific ownership.
    // The journal and global operation endpoint remain reader-agnostic.
    reader_owner: Option<(String, u64)>,
}

struct DownloadPermit {
    id: String,
    confirmation: Receiver<bool>,
    cancelled: Arc<AtomicBool>,
    _ownership: OperationOwnershipGuard,
    _operation_gate: OperationGatePermit,
}

struct SwitchPermit {
    id: String,
    _ownership: OperationOwnershipGuard,
    _operation_gate: OperationGatePermit,
}

/// If an admitted card operation loses its async owner before writing a
/// terminal result, durably mark the outcome unknown before releasing the
/// shared eSIM gate. Startup recovery applies the same rule after a crash.
struct OperationOwnershipGuard {
    id: String,
    state: Arc<Mutex<Option<OperationRecord>>>,
    journal: Option<ArchiveStore>,
    journal_faulted: Arc<AtomicBool>,
}

impl Drop for OperationOwnershipGuard {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state
            .as_mut()
            .filter(|record| record.snapshot.id == self.id)
        else {
            return;
        };
        if !is_active(&record.snapshot.state) {
            return;
        }
        record.confirmation = None;
        mark_interrupted(&mut record.snapshot);
        if persist_operation_snapshot(
            self.journal.as_ref(),
            &record.snapshot,
            &record.proof,
            record.resolution,
        )
        .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
        }
    }
}

/// Durable switch-operation ownership shared with the USB module eSIM path.
/// Keeping this permit alive holds the same atomic gate as reader download and
/// profile mutations until the module recovery monitor records a terminal
/// result.
pub(crate) struct ModuleSwitchOperation {
    permit: SwitchPermit,
}

impl ModuleSwitchOperation {
    pub(crate) fn id(&self) -> &str {
        &self.permit.id
    }
}

#[derive(Debug)]
struct OperationGatePermit {
    active: Arc<AtomicBool>,
}

impl Drop for OperationGatePermit {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

pub(crate) struct ModuleOperationPermit {
    _operation_gate: OperationGatePermit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleOperationAdmissionError {
    Busy,
    UnsafeState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BeginError {
    Busy,
    Journal,
    Unavailable,
    ReconciliationRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfirmError {
    Expired,
    Journal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcknowledgeError {
    Invalid,
    Conflict,
    Journal,
}

impl Service {
    fn open(data_root: &Path) -> Result<Self, StoreError> {
        let journal = ArchiveStore::with_limits(
            data_root.join("reader-esim-operation.json"),
            MAX_JOURNAL_BYTES,
            1,
        )?;
        let journal_present = if let Some(file) = journal.snapshot_file()? {
            let mut bytes = Vec::new();
            file.take(MAX_JOURNAL_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(StoreError::Io)?;
            if bytes.is_empty()
                || bytes
                    .iter()
                    .copied()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .eq(b"null".iter().copied())
            {
                return Err(StoreError::Configuration(
                    "eSIM operation journal is empty or null".into(),
                ));
            }
            true
        } else {
            false
        };
        let items = journal.load()?;
        if journal_present && items.is_empty() {
            return Err(StoreError::Configuration(
                "eSIM operation journal has no operation record".into(),
            ));
        }
        let loaded_from_v2 = items
            .first()
            .is_some_and(|item| item.get("schema_version").is_some());
        let mut loaded = items
            .into_iter()
            .next()
            .map(|item| {
                if item.get("schema_version").is_some() {
                    let record: PersistedOperation =
                        serde_json::from_value(item).map_err(StoreError::Json)?;
                    if record.schema_version != OPERATION_JOURNAL_VERSION {
                        return Err(StoreError::Configuration(
                            "unsupported eSIM operation journal version".into(),
                        ));
                    }
                    validate_card_proof(&record.proof, record.resolution)?;
                    Ok(record)
                } else {
                    let snapshot = serde_json::from_value(item).map_err(StoreError::Json)?;
                    Ok(PersistedOperation {
                        schema_version: OPERATION_JOURNAL_VERSION,
                        snapshot,
                        proof: CardProof::LegacyUnknown,
                        resolution: None,
                    })
                }
            })
            .transpose()?;
        // Never replace a lost key for an armed record: doing so could make a
        // later scan appear to prove a different card after a restart.
        let proof_key = load_or_create_card_proof_key(data_root, loaded_from_v2)?;
        if let Some(record) = loaded.as_mut() {
            let snapshot = &mut record.snapshot;
            validate_snapshot(snapshot)?;
            let interrupted = is_active(&snapshot.state);
            let needs_sanitizing = snapshot.target_iccid.is_some()
                || snapshot.diagnostic.is_some()
                || snapshot
                    .result
                    .as_ref()
                    .is_some_and(|result| result.warning.is_some());
            snapshot.target_iccid = None;
            snapshot.diagnostic = None;
            if let Some(result) = snapshot.result.as_mut() {
                result.warning = None;
            }
            if interrupted {
                mark_interrupted(snapshot);
            }
            if interrupted || needs_sanitizing {
                persist_operation_snapshot(
                    Some(&journal),
                    snapshot,
                    &record.proof,
                    record.resolution,
                )?;
            }
        }
        Ok(Self {
            state: Arc::new(Mutex::new(loaded.map(|record| OperationRecord {
                snapshot: record.snapshot,
                proof: record.proof,
                resolution: record.resolution,
                confirmation: None,
                reader_owner: None,
            }))),
            journal: Some(journal),
            proof_key: Some(Arc::new(proof_key)),
            journal_faulted: Arc::new(AtomicBool::new(false)),
            operation_gate: Arc::default(),
            shutting_down: Arc::new(AtomicBool::new(false)),
        })
    }

    fn persist(
        &self,
        snapshot: &OperationSnapshot,
        proof: &CardProof,
        resolution: Option<PrivateResolution>,
    ) -> Result<(), StoreError> {
        persist_operation_snapshot(self.journal.as_ref(), snapshot, proof, resolution)
    }

    fn ownership_guard(&self, id: &str) -> OperationOwnershipGuard {
        OperationOwnershipGuard {
            id: id.to_owned(),
            state: self.state.clone(),
            journal: self.journal.clone(),
            journal_faulted: self.journal_faulted.clone(),
        }
    }

    fn await_confirmation(&self, id: &str, estimated_size: i64) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut().filter(|record| record.snapshot.id == id) else {
            return false;
        };
        let mut snapshot = record.snapshot.clone();
        snapshot.state = "awaiting_confirmation".into();
        snapshot.step = Some("confirm_download".into());
        snapshot.message = Some("已读取 Profile 信息，请确认后开始写入 eUICC".into());
        snapshot.estimated_profile_size_bytes = (estimated_size > 0).then_some(estimated_size);
        snapshot.progress = 58;
        snapshot.updated_at = timestamp();
        if self
            .persist(&snapshot, &record.proof, record.resolution)
            .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            return false;
        }
        record.snapshot = snapshot;
        true
    }

    fn current(&self) -> Option<OperationSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|record| record.snapshot.clone())
    }

    fn manual_ack_available(&self) -> bool {
        self.current_with_manual_ack_available().1
    }

    fn current_with_manual_ack_available(&self) -> (Option<OperationSnapshot>, bool) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = state.as_ref().map(|record| record.snapshot.clone());
        if self.operation_gate.load(Ordering::Acquire)
            || self.journal_faulted.load(Ordering::Acquire)
        {
            return (snapshot, false);
        }
        let available = state.as_ref().is_some_and(|record| {
            matches!(record.proof, CardProof::LegacyUnknown)
                && record.resolution.is_none()
                && record.reader_owner.is_none()
                && record.snapshot.state == "failed"
                && record.snapshot.error_code.as_deref() == Some("esim_operation_interrupted")
                && !record.snapshot.recoverable
        });
        (snapshot, available)
    }

    fn acknowledge_legacy(
        &self,
        request: AcknowledgeLegacyRequest,
    ) -> Result<OperationSnapshot, AcknowledgeError> {
        if !request.legacy_manual_ack
            || request.expected_state != "failed"
            || request.expected_error_code != "esim_operation_interrupted"
        {
            return Err(AcknowledgeError::Invalid);
        }
        if self.operation_gate.load(Ordering::Acquire) {
            return Err(AcknowledgeError::Conflict);
        }
        if self.journal_faulted.load(Ordering::Acquire) {
            return Err(AcknowledgeError::Journal);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state.as_mut().ok_or(AcknowledgeError::Conflict)?;
        if record.snapshot.id != request.operation_id
            || record.snapshot.updated_at != request.expected_updated_at
            || record.snapshot.state != request.expected_state
            || record.snapshot.error_code.as_deref() != Some(request.expected_error_code.as_str())
            || record.snapshot.recoverable
            || record.reader_owner.is_some()
            || !matches!(record.proof, CardProof::LegacyUnknown)
            || record.resolution.is_some()
        {
            return Err(AcknowledgeError::Conflict);
        }
        let mut snapshot = record.snapshot.clone();
        snapshot.recoverable = true;
        snapshot.message =
            Some("已人工确认旧版未知结果；请核对实际卡片状态，系统不会自动重试。".into());
        snapshot.error = snapshot.message.clone();
        snapshot.updated_at = timestamp();
        let resolution = Some(PrivateResolution::ManualAcknowledged);
        if self.persist(&snapshot, &record.proof, resolution).is_err() {
            self.journal_faulted.store(true, Ordering::Release);
            return Err(AcknowledgeError::Journal);
        }
        record.snapshot = snapshot.clone();
        record.resolution = resolution;
        Ok(snapshot)
    }

    fn arm_card_proof(&self, id: &str, eid: &[u8], aid: &[u8]) -> Result<(), &'static str> {
        let key = self
            .proof_key
            .as_ref()
            .ok_or("eSIM recovery key unavailable")?;
        let digest = card_proof_digest(key, eid, aid).ok_or("invalid eSIM card identity")?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state
            .as_mut()
            .filter(|record| record.snapshot.id == id && is_active(&record.snapshot.state))
            .ok_or("eSIM operation owner changed")?;
        if !matches!(record.proof, CardProof::NotArmed) {
            return Err("eSIM card proof already armed");
        }
        let proof = CardProof::Armed {
            digest_hex: encode_hex_lower(&digest),
        };
        if self.journal_faulted.load(Ordering::Acquire)
            || self.persist(&record.snapshot, &proof, None).is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            return Err("eSIM recovery proof could not be persisted");
        }
        record.proof = proof;
        Ok(())
    }

    fn bind_reader_owner(&self, id: &str, reader: &str, generation: u64) -> bool {
        if reader.is_empty() || generation == 0 {
            return false;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut().filter(|record| record.snapshot.id == id) else {
            return false;
        };
        let owner = (reader.to_owned(), generation);
        match record.reader_owner.as_ref() {
            Some(existing) => existing == &owner,
            None => {
                record.reader_owner = Some(owner);
                true
            }
        }
    }

    fn current_for_selected(&self, selected: Option<(String, u64)>) -> Option<OperationSnapshot> {
        let selected = selected?;
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|record| record.reader_owner.as_ref() == Some(&selected))
            .map(|record| record.snapshot.clone())
    }

    fn active(&self) -> bool {
        self.operation_gate.load(Ordering::Acquire)
            || self
                .current()
                .is_some_and(|snapshot| is_active(&snapshot.state))
    }

    fn acquire_operation_gate(&self) -> Result<OperationGatePermit, BeginError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(BeginError::Unavailable);
        }
        let active = self.operation_gate.clone();
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BeginError::Busy)?;
        let permit = OperationGatePermit { active };
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(BeginError::Unavailable);
        }
        if self.journal_faulted.load(Ordering::Acquire) {
            return Err(BeginError::Journal);
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .as_ref()
            .is_some_and(|record| is_active(&record.snapshot.state))
        {
            return Err(BeginError::Busy);
        }
        if state.as_ref().is_some_and(|record| {
            record.snapshot.error_code.as_deref() == Some("esim_operation_interrupted")
                && !record.snapshot.recoverable
        }) {
            return Err(BeginError::ReconciliationRequired);
        }
        drop(state);
        Ok(permit)
    }

    fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
    }

    async fn wait_for_shutdown(&self) -> Result<(), String> {
        while self.operation_gate.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut() else {
            return Ok(());
        };
        if is_active(&record.snapshot.state) {
            record.confirmation = None;
            mark_interrupted(&mut record.snapshot);
            self.persist(&record.snapshot, &record.proof, record.resolution)
                .map_err(|error| format!("persist interrupted eSIM operation: {error}"))?;
        }
        Ok(())
    }

    fn begin_module_operation(
        &self,
    ) -> Result<ModuleOperationPermit, ModuleOperationAdmissionError> {
        self.acquire_operation_gate()
            .map(|operation_gate| ModuleOperationPermit {
                _operation_gate: operation_gate,
            })
            .map_err(|error| match error {
                BeginError::Busy | BeginError::ReconciliationRequired => {
                    ModuleOperationAdmissionError::Busy
                }
                BeginError::Journal | BeginError::Unavailable => {
                    ModuleOperationAdmissionError::UnsafeState
                }
            })
    }

    fn reconcile_after_overview_refresh(
        &self,
        reader: &str,
        generation: u64,
        identities: &[(Vec<u8>, Vec<u8>)],
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut().filter(|record| {
            record.snapshot.state == "failed"
                && record.snapshot.error_code.as_deref() == Some("esim_operation_interrupted")
                && !record.snapshot.recoverable
        }) else {
            return true;
        };
        if self.operation_gate.load(Ordering::Acquire) {
            return true;
        }
        match &record.proof {
            CardProof::LegacyUnknown => return true,
            CardProof::NotArmed => {
                // No mutating APDU can be submitted until proof is durably
                // armed. Preserve the live lease owner bound from i662.
                if record
                    .reader_owner
                    .as_ref()
                    .is_some_and(|owner| owner != &(reader.to_owned(), generation))
                {
                    return true;
                }
            }
            CardProof::Armed { .. } => {
                let Some(key) = self.proof_key.as_ref() else {
                    return false;
                };
                if !identities
                    .iter()
                    .any(|(eid, aid)| card_proof_matches(key, &record.proof, eid, aid))
                {
                    return true;
                }
            }
        }
        let mut snapshot = record.snapshot.clone();
        snapshot.recoverable = true;
        snapshot.message = Some(if snapshot.kind == "switch_profile" {
            "上次 Profile 切换结果未知。列表已刷新，请核对当前卡片身份后再操作。".into()
        } else {
            "上次下载结果未知。Profile 列表已刷新，请核对当前卡片状态后再操作。".into()
        });
        snapshot.error = snapshot.message.clone();
        snapshot.updated_at = timestamp();
        if self
            .persist(&snapshot, &record.proof, record.resolution)
            .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            return false;
        }
        record.snapshot = snapshot;
        true
    }

    fn begin(&self) -> Result<DownloadPermit, BeginError> {
        let operation_gate = self.acquire_operation_gate()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.journal_faulted.load(Ordering::Acquire) {
            return Err(BeginError::Journal);
        }
        if state
            .as_ref()
            .is_some_and(|record| is_active(&record.snapshot.state))
        {
            return Err(BeginError::Busy);
        }
        if state.as_ref().is_some_and(|record| {
            record.snapshot.error_code.as_deref() == Some("esim_operation_interrupted")
                && !record.snapshot.recoverable
        }) {
            return Err(BeginError::ReconciliationRequired);
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| BeginError::Unavailable)?;
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let now = timestamp();
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let snapshot = OperationSnapshot {
            id: id.clone(),
            kind: "download_profile".into(),
            state: "running".into(),
            step: Some("preflight".into()),
            message: Some("正在准备下载 Profile…".into()),
            progress: 5,
            started_at: now.clone(),
            updated_at: now,
            recoverable: true,
            ..OperationSnapshot::default()
        };
        if self.persist(&snapshot, &CardProof::NotArmed, None).is_err() {
            self.journal_faulted.store(true, Ordering::Release);
            return Err(BeginError::Journal);
        }
        *state = Some(OperationRecord {
            snapshot,
            proof: CardProof::NotArmed,
            resolution: None,
            confirmation: Some(sender),
            reader_owner: None,
        });
        Ok(DownloadPermit {
            _ownership: self.ownership_guard(&id),
            id,
            confirmation: receiver,
            cancelled,
            _operation_gate: operation_gate,
        })
    }

    fn begin_switch(&self, target_iccid: &str) -> Result<SwitchPermit, BeginError> {
        let operation_gate = self.acquire_operation_gate()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.journal_faulted.load(Ordering::Acquire) {
            return Err(BeginError::Journal);
        }
        if state
            .as_ref()
            .is_some_and(|record| is_active(&record.snapshot.state))
        {
            return Err(BeginError::Busy);
        }
        if state.as_ref().is_some_and(|record| {
            record.snapshot.error_code.as_deref() == Some("esim_operation_interrupted")
                && !record.snapshot.recoverable
        }) {
            return Err(BeginError::ReconciliationRequired);
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| BeginError::Unavailable)?;
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let now = timestamp();
        let snapshot = OperationSnapshot {
            id: id.clone(),
            kind: "switch_profile".into(),
            state: "running".into(),
            step: Some("switch".into()),
            message: Some("正在向 eUICC 提交 Profile 切换…".into()),
            progress: 25,
            target_iccid: Some(target_iccid.to_owned()),
            started_at: now.clone(),
            updated_at: now,
            recoverable: true,
            ..OperationSnapshot::default()
        };
        if self.persist(&snapshot, &CardProof::NotArmed, None).is_err() {
            self.journal_faulted.store(true, Ordering::Release);
            return Err(BeginError::Journal);
        }
        *state = Some(OperationRecord {
            snapshot,
            proof: CardProof::NotArmed,
            resolution: None,
            confirmation: None,
            reader_owner: None,
        });
        Ok(SwitchPermit {
            _ownership: self.ownership_guard(&id),
            id,
            _operation_gate: operation_gate,
        })
    }

    fn finish_switch(&self, id: &str, result: Result<DownloadResult, DownloadFailure>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut().filter(|record| record.snapshot.id == id) else {
            return;
        };
        let mut snapshot = record.snapshot.clone();
        match result {
            Ok(result) => {
                snapshot.state = if result.warning.is_some() {
                    "warning"
                } else {
                    "succeeded"
                }
                .into();
                snapshot.step = Some("done".into());
                snapshot.message = Some(if result.warning.is_some() {
                    "Profile 切换已受理，激活结果待确认".into()
                } else {
                    "Profile 切换完成".into()
                });
                snapshot.progress = 100;
                snapshot.result = Some(result);
                snapshot.refresh_after_seconds = 2;
            }
            Err(DownloadFailure::Cancelled) => {
                snapshot.state = "cancelled".into();
                snapshot.step = Some("cancelled".into());
                snapshot.message = Some("Profile 切换未提交".into());
                snapshot.progress = 100;
            }
            Err(DownloadFailure::Failed {
                code,
                message,
                diagnostic,
            }) => {
                snapshot.state = "failed".into();
                snapshot.step = Some("failed".into());
                snapshot.message = Some(message.clone());
                snapshot.error_code = Some(code);
                snapshot.error = Some(message);
                snapshot.diagnostic = Some(diagnostic);
                snapshot.progress = 100;
                snapshot.recoverable = true;
            }
        }
        let now = timestamp();
        snapshot.updated_at = now.clone();
        snapshot.finished_at = Some(now);
        if self.journal_faulted.load(Ordering::Acquire)
            || self
                .persist(&snapshot, &record.proof, record.resolution)
                .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            mark_interrupted(&mut snapshot);
            let _ = self.persist(&snapshot, &record.proof, record.resolution);
        }
        record.snapshot = snapshot;
    }

    fn update(&self, id: &str, state_name: &str, step: &str, message: &str, progress: i32) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut().filter(|record| record.snapshot.id == id) else {
            return false;
        };
        let mut snapshot = record.snapshot.clone();
        snapshot.state = state_name.into();
        snapshot.step = Some(step.into());
        snapshot.message = Some(message.into());
        snapshot.progress = progress.clamp(0, 100);
        snapshot.updated_at = timestamp();
        if self
            .persist(&snapshot, &record.proof, record.resolution)
            .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            return false;
        }
        record.snapshot = snapshot;
        true
    }

    fn confirm(&self, request: ConfirmationRequest) -> Result<OperationSnapshot, ConfirmError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.journal_faulted.load(Ordering::Acquire) {
            return Err(ConfirmError::Journal);
        }
        let Some(record) = state.as_mut().filter(|record| {
            record.snapshot.id == request.operation_id
                && record.snapshot.kind == "download_profile"
                && record.snapshot.state == "awaiting_confirmation"
        }) else {
            return Err(ConfirmError::Expired);
        };
        if record.confirmation.is_none() {
            return Err(ConfirmError::Expired);
        }
        let mut snapshot = record.snapshot.clone();
        snapshot.state = "running".into();
        snapshot.step = Some("confirm_download".into());
        snapshot.message = Some(if request.confirm {
            "已确认，正在获取 Profile 数据包…".into()
        } else {
            "正在取消下载…".into()
        });
        snapshot.progress = 62;
        snapshot.updated_at = timestamp();
        if self
            .persist(&snapshot, &record.proof, record.resolution)
            .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            return Err(ConfirmError::Journal);
        }
        let sender = record.confirmation.take().ok_or(ConfirmError::Expired)?;
        record.snapshot = snapshot.clone();
        if sender.send(request.confirm).is_err() {
            self.journal_faulted.store(true, Ordering::Release);
            mark_interrupted(&mut record.snapshot);
            let _ = self.persist(&record.snapshot, &record.proof, record.resolution);
            return Err(ConfirmError::Expired);
        }
        Ok(snapshot)
    }

    fn finish(&self, id: &str, result: Result<DownloadResult, DownloadFailure>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = state.as_mut().filter(|record| record.snapshot.id == id) else {
            return;
        };
        record.confirmation = None;
        if self.journal_faulted.load(Ordering::Acquire) {
            mark_interrupted(&mut record.snapshot);
            let _ = self.persist(&record.snapshot, &record.proof, record.resolution);
            return;
        }
        let now = timestamp();
        let mut snapshot = record.snapshot.clone();
        match result {
            Ok(result) => {
                snapshot.state = if result.warning.is_some() {
                    "warning"
                } else {
                    "succeeded"
                }
                .into();
                snapshot.step = Some("done".into());
                snapshot.message = Some(if result.warning.is_some() {
                    "Profile 已安装，但部分收尾状态尚未确认".into()
                } else {
                    "Profile 下载并安装完成".into()
                });
                snapshot.progress = 100;
                snapshot.result = Some(result);
                snapshot.refresh_after_seconds = 2;
            }
            Err(DownloadFailure::Cancelled) => {
                snapshot.state = "cancelled".into();
                snapshot.step = Some("cancelled".into());
                snapshot.message = Some("已取消下载，Profile 未写入 eUICC".into());
                snapshot.progress = 100;
            }
            Err(DownloadFailure::Failed {
                code,
                message,
                diagnostic,
            }) => {
                snapshot.state = "failed".into();
                snapshot.step = Some("failed".into());
                snapshot.message = Some(message);
                snapshot.error_code = Some(code);
                snapshot.error = snapshot.message.clone();
                snapshot.diagnostic = Some(diagnostic);
                snapshot.progress = 100;
                snapshot.recoverable = true;
            }
        }
        snapshot.updated_at = now.clone();
        snapshot.finished_at = Some(now);
        if self
            .persist(&snapshot, &record.proof, record.resolution)
            .is_err()
        {
            self.journal_faulted.store(true, Ordering::Release);
            mark_interrupted(&mut snapshot);
            let _ = self.persist(&snapshot, &record.proof, record.resolution);
        }
        record.snapshot = snapshot;
    }
}

fn persist_operation_snapshot(
    journal: Option<&ArchiveStore>,
    snapshot: &OperationSnapshot,
    proof: &CardProof,
    resolution: Option<PrivateResolution>,
) -> Result<(), StoreError> {
    if let Some(journal) = journal {
        // Keep diagnostics and subscriber identifiers volatile. Persist only
        // user-safe state; activation inputs never enter this DTO.
        let mut safe = snapshot.clone();
        safe.target_iccid = None;
        safe.diagnostic = None;
        if let Some(result) = safe.result.as_mut() {
            result.warning = None;
        }
        let item = serde_json::to_value(PersistedOperation {
            schema_version: OPERATION_JOURNAL_VERSION,
            snapshot: safe,
            proof: proof.clone(),
            resolution,
        })
        .map_err(StoreError::Json)?;
        journal.replace(vec![item])?;
    }
    Ok(())
}

fn validate_card_proof(
    proof: &CardProof,
    resolution: Option<PrivateResolution>,
) -> Result<(), StoreError> {
    if let CardProof::Armed { digest_hex } = proof {
        if digest_hex.len() != 64 || parse_hex(digest_hex).is_none() {
            return Err(StoreError::Configuration(
                "invalid eSIM card recovery proof".into(),
            ));
        }
    }
    if resolution.is_some() && !matches!(proof, CardProof::LegacyUnknown) {
        return Err(StoreError::Configuration(
            "invalid eSIM recovery resolution".into(),
        ));
    }
    Ok(())
}

fn load_or_create_card_proof_key(
    data_root: &Path,
    loaded_from_v2: bool,
) -> Result<Zeroizing<[u8; 32]>, StoreError> {
    let store =
        ArchiveStore::with_limits(data_root.join("reader-esim-card-proof-key.json"), 1024, 1)?;
    if let Ok(metadata) = std::fs::symlink_metadata(store.path()) {
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(StoreError::Configuration(
                "eSIM card recovery key has unsafe permissions".into(),
            ));
        }
    }
    let present = store.snapshot_file()?.is_some();
    let items = store.load()?;
    if let Some(item) = items.into_iter().next() {
        let record: CardProofKeyRecord = serde_json::from_value(item).map_err(StoreError::Json)?;
        let bytes = parse_hex(&record.key_hex);
        if record.schema_version != 1 || bytes.as_ref().is_none_or(|bytes| bytes.len() != 32) {
            return Err(StoreError::Configuration(
                "invalid eSIM card recovery key".into(),
            ));
        }
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes.expect("validated key length"));
        return Ok(key);
    }
    if present {
        return Err(StoreError::Configuration(
            "eSIM card recovery key is empty or corrupt".into(),
        ));
    }
    if loaded_from_v2 {
        return Err(StoreError::Configuration(
            "eSIM card recovery key missing for a versioned operation".into(),
        ));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(key.as_mut())
        .map_err(|_| StoreError::Configuration("cannot generate eSIM card recovery key".into()))?;
    store.replace(vec![
        serde_json::to_value(CardProofKeyRecord {
            schema_version: 1,
            key_hex: encode_hex_lower(&key[..]),
        })
        .map_err(StoreError::Json)?,
    ])?;
    Ok(key)
}

fn card_proof_digest(key: &[u8; 32], eid: &[u8], aid: &[u8]) -> Option<[u8; 32]> {
    if format_eid(eid).is_none() || aid.is_empty() || aid.len() > 32 {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(CARD_PROOF_CONTEXT);
    mac.update(eid);
    mac.update(&[aid.len() as u8]);
    mac.update(aid);
    Some(mac.finalize().into_bytes().into())
}

fn card_proof_matches(key: &[u8; 32], proof: &CardProof, eid: &[u8], aid: &[u8]) -> bool {
    let CardProof::Armed { digest_hex } = proof else {
        return false;
    };
    let Some(expected) = parse_hex(digest_hex) else {
        return false;
    };
    let Some(observed) = card_proof_digest(key, eid, aid) else {
        return false;
    };
    bool::from(expected.as_slice().ct_eq(&observed))
}

fn validate_snapshot(snapshot: &OperationSnapshot) -> Result<(), StoreError> {
    let valid_id =
        snapshot.id.len() == 32 && snapshot.id.bytes().all(|byte| byte.is_ascii_hexdigit());
    let valid_state = matches!(
        snapshot.state.as_str(),
        "queued"
            | "running"
            | "awaiting_confirmation"
            | "succeeded"
            | "warning"
            | "cancelled"
            | "failed"
    );
    let bounded = matches!(
        snapshot.kind.as_str(),
        "download_profile" | "switch_profile"
    ) && (0..=100).contains(&snapshot.progress)
        && snapshot.step.as_ref().is_none_or(|value| value.len() <= 64)
        && snapshot
            .message
            .as_ref()
            .is_none_or(|value| value.len() <= 512)
        && snapshot
            .error_code
            .as_ref()
            .is_none_or(|value| value.len() <= 96)
        && snapshot
            .error
            .as_ref()
            .is_none_or(|value| value.len() <= 512)
        && snapshot.started_at.len() <= 64
        && snapshot.updated_at.len() <= 64;
    if !valid_id || !valid_state || !bounded {
        return Err(StoreError::Configuration(
            "eSIM operation journal contains an invalid record".into(),
        ));
    }
    Ok(())
}

fn mark_interrupted(snapshot: &mut OperationSnapshot) {
    let message = if snapshot.kind == "switch_profile" {
        "应用上次退出时 Profile 切换结果未知。请刷新 Profile 列表并核对当前卡片身份后，再决定是否重新切换。"
    } else {
        "应用上次退出时下载结果未知。请先刷新 Profile 列表确认卡片状态，再决定是否重新下载。"
    };
    snapshot.state = "failed".into();
    snapshot.step = Some("interrupted".into());
    snapshot.message = Some(message.into());
    snapshot.error_code = Some("esim_operation_interrupted".into());
    snapshot.error = Some(message.into());
    snapshot.diagnostic = None;
    snapshot.target_iccid = None;
    snapshot.result = None;
    snapshot.progress = 100;
    snapshot.recoverable = false;
    snapshot.refresh_after_seconds = 0;
    snapshot.updated_at = timestamp();
    snapshot.finished_at = Some(snapshot.updated_at.clone());
}

#[derive(Debug)]
enum DownloadFailure {
    Cancelled,
    Failed {
        code: String,
        message: String,
        diagnostic: String,
    },
}

/// Dropping the HTTP waiter's JoinHandle detaches accepted write work. The
/// Service gate and shutdown drain, rather than the client connection, own its
/// lifetime through physical cleanup and journal completion.
fn spawn_retained_operation<F, T>(operation: F) -> tokio::task::JoinHandle<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(operation)
}

fn service() -> &'static Service {
    ESIM_SERVICE
        .get()
        .expect("eSIM operation journal initialized before serving")
}

static ESIM_SERVICE: OnceLock<Service> = OnceLock::new();

pub(crate) fn initialize(data_root: &Path) -> Result<(), StoreError> {
    let service = Service::open(data_root)?;
    ESIM_SERVICE.set(service).map_err(|_| {
        StoreError::Configuration("eSIM operation journal initialized more than once".into())
    })
}

fn download_pool() -> &'static crate::blocking_pool::BlockingPool {
    static POOL: OnceLock<crate::blocking_pool::BlockingPool> = OnceLock::new();
    POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(1))
}

fn journal_pool() -> &'static crate::blocking_pool::BlockingPool {
    static POOL: OnceLock<crate::blocking_pool::BlockingPool> = OnceLock::new();
    POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(1))
}

pub(crate) fn has_active_operation() -> bool {
    service().active()
}

pub(crate) fn has_active_download_operation() -> bool {
    service()
        .current()
        .is_some_and(|snapshot| snapshot.kind == "download_profile" && is_active(&snapshot.state))
}

pub(crate) fn begin_shutdown() {
    service().begin_shutdown();
}

/// Stop teardown from finalizing module radio while an already accepted eSIM
/// card transaction still owns the shared mutation gate.
pub(crate) async fn wait_for_shutdown() -> Result<(), String> {
    service().wait_for_shutdown().await
}

pub(crate) fn begin_module_operation()
-> Result<ModuleOperationPermit, ModuleOperationAdmissionError> {
    service().begin_module_operation()
}

pub(crate) fn begin_module_switch(
    target_iccid: &str,
) -> Result<ModuleSwitchOperation, ModuleOperationAdmissionError> {
    service()
        .begin_switch(target_iccid)
        .map(|permit| ModuleSwitchOperation { permit })
        .map_err(|error| match error {
            BeginError::Busy | BeginError::ReconciliationRequired => {
                ModuleOperationAdmissionError::Busy
            }
            BeginError::Journal | BeginError::Unavailable => {
                ModuleOperationAdmissionError::UnsafeState
            }
        })
}

pub(crate) fn arm_module_switch_proof(
    operation: &ModuleSwitchOperation,
    eid: &[u8],
    aid: &[u8],
) -> Result<(), &'static str> {
    service().arm_card_proof(operation.id(), eid, aid)
}

pub(crate) fn update_module_switch(
    operation: &ModuleSwitchOperation,
    step: &str,
    message: &str,
    progress: i32,
) -> bool {
    service().update(operation.id(), "running", step, message, progress)
}

pub(crate) fn finish_module_switch(
    operation: Arc<ModuleSwitchOperation>,
    warning: Option<(String, String)>,
) {
    let result = DownloadResult {
        warning: warning.as_ref().map(|(message, _)| message.clone()),
        warning_code: warning.map(|(_, code)| code),
        space_delta: None,
    };
    service().finish_switch(operation.id(), Ok(result));
    drop(operation);
}

pub(crate) fn fail_module_switch(
    operation: Arc<ModuleSwitchOperation>,
    code: &str,
    message: &str,
    diagnostic: &str,
) {
    service().finish_switch(
        operation.id(),
        Err(DownloadFailure::Failed {
            code: code.to_owned(),
            message: message.to_owned(),
            diagnostic: diagnostic.to_owned(),
        }),
    );
    drop(operation);
}

pub(crate) fn current_operation_json() -> Option<Value> {
    service()
        .current()
        .and_then(|snapshot| serde_json::to_value(snapshot).ok())
}

pub(crate) fn selected_reader(state: &ServerState) -> Option<String> {
    let discovery = state
        .reader_discovery
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .snapshot();
    let selected = state.reader_gate.selected_reader();
    let ble_connection_bound = selected
        .as_deref()
        .is_some_and(|reader| selected_ble_connection_bound(&state.reader_gate, reader));
    selected_reader_from_snapshot(selected.as_deref(), &discovery, ble_connection_bound)
}

fn selected_ble_connection_bound(gate: &crate::reader_reservation::Gate, reader: &str) -> bool {
    valid_ble_reader_name(reader)
        && gate
            .ble_connection_binding(reader)
            .is_some_and(|binding| binding.reader == reader)
}

fn selected_reader_from_snapshot(
    selected: Option<&str>,
    discovery: &DiscoverySnapshot,
    ble_connection_bound: bool,
) -> Option<String> {
    // Discovery is an observation, never selection authority. In particular,
    // its cached selected hint cannot revive a pending or retired selection.
    let selected = selected?;
    if selected.starts_with("BLE:") {
        // BLE has no removable-slot observation, so inventory correctly keeps
        // CardPresence::Unknown. Native connect must have captured this exact
        // bridge connection in the current Gate selection. Every card worker
        // rechecks that binding before submitting an APDU.
        let contradictory_card_observation = discovery.readers.iter().any(|reader| {
            reader.name == selected
                && matches!(
                    reader.card,
                    CardPresence::Absent | CardPresence::Unavailable
                )
        });
        return (valid_ble_reader_name(selected)
            && ble_connection_bound
            && !contradictory_card_observation)
            .then(|| selected.to_owned());
    }
    discovery
        .readers
        .iter()
        .any(|reader| reader.name == selected && reader.card == CardPresence::Present)
        .then(|| selected.to_owned())
}

fn valid_ble_reader_name(name: &str) -> bool {
    let Some(id) = name.strip_prefix("BLE:") else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Rust owns the selected reader's profile health APDUs on both USB/PCSC and
/// BLE. Swift remains the CoreBluetooth system-framework bridge, while the
/// Rust lease excludes concurrent card operations.
pub(crate) async fn health_or_forward(
    request: Request<Incoming>,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    health_or_forward_with_selection(request, state, reader, || selected_reader(state)).await
}

pub(crate) async fn health_or_forward_with_selection(
    _request: Request<Incoming>,
    state: &ServerState,
    reader: String,
    current_selection: impl FnOnce() -> Option<String>,
) -> Response<Full<Bytes>> {
    let lease = match NativeReaderSessionLease::acquire_for_card_operation(
        &state.go_socket_path,
        state.app_token.as_deref(),
        &reader,
        state.reader_gate.clone(),
        state.reader_pcsc.clone(),
    )
    .await
    {
        Ok(lease) => Arc::new(lease),
        Err(error)
            if matches!(
                error,
                proletarius_domain::BackendFailure::ReaderAdmissionRejected { .. }
            ) =>
        {
            return failure(
                StatusCode::CONFLICT,
                "reader_operation_active",
                "读卡器操作正在进行，请稍后重试",
                true,
            );
        }
        Err(_) => return health_failed(),
    };

    // Discovery is an observation, not selection authority. Re-check the Rust
    // selection after pinning the adapter, then let the generation- and
    // session-bound PC/SC binding reject a card replacement during the APDUs.
    if current_selection().as_deref() != Some(reader.as_str()) {
        if lease.release().await.is_err() {
            return health_failed();
        }
        return health_failed();
    }
    match lease.esim_manager_available {
        Some(true) => {}
        Some(false) if lease_has_usim_identity(&lease) => {}
        Some(false) => {
            if lease.release_with_confirmation().await.is_err() {
                return health_failed();
            }
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_esim_unavailable",
                "读卡器 eSIM 管理服务不可用",
                true,
            );
        }
        // A snapshot without this fact cannot establish eSIM availability.
        // Release the pin before reporting the incomplete native facts.
        None => {
            if lease.release_with_confirmation().await.is_err() {
                return health_failed();
            }
            return health_failed();
        }
    }
    let card_context = if reader.starts_with("BLE:") {
        None
    } else {
        match lease.pcsc_context(state.reader_pcsc.clone()) {
            Ok(context) => Some(context),
            Err(_) => {
                if lease.release().await.is_err() {
                    return health_failed();
                }
                return health_failed();
            }
        }
    };
    let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
    let app_token = state.app_token.clone();

    // Detach the lease/worker lifetime from the HTTP request. If a client
    // disconnects while an APDU is executing, the task still waits for the
    // submitted transaction to finish before releasing the cross-process lock.
    let lease_owner = lease.clone();
    let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
    let operation = tokio::spawn(async move {
        let cancelled = Arc::new(AtomicBool::new(false));
        let watcher_cancelled = cancelled.clone();
        let mut lost_signal = heartbeat.lost_receiver();
        let watcher = tokio::spawn(async move {
            loop {
                if *lost_signal.borrow() {
                    watcher_cancelled.store(true, Ordering::Release);
                    return;
                }
                if lost_signal.changed().await.is_err() {
                    return;
                }
            }
        });
        let worker_cancelled = cancelled.clone();
        let scan = lease_owner
            .run_card_worker(download_pool(), move || {
                if let Some(context) = card_context {
                    read_health_profiles(&context, worker_cancelled)
                        .map_err(|_| HealthReadError::Operation)
                } else {
                    let transport = crate::reader_ble_apdu::BleApduTransport::connect(
                        ble_socket.as_deref(),
                        app_token.as_deref(),
                        &reader,
                    )
                    .map_err(|_| HealthReadError::BridgePreflight)?;
                    let mut guarded = LeaseAwareTransport {
                        inner: transport,
                        cancelled: worker_cancelled,
                    };
                    read_health_profiles_on_transport(&mut guarded)
                        .map_err(|_| HealthReadError::Operation)
                }
            })
            .await;
        let lease_lost = cancelled.load(Ordering::Acquire);
        heartbeat.stop().await;
        watcher.abort();
        let release = lease_owner.release().await;
        (scan, lease_lost, release)
    });
    let (scan, lease_lost, release) = match operation.await {
        Ok(result) => result,
        Err(_) => return health_failed(),
    };
    if lease_lost || release.is_err() {
        // A successful card response is not enough to claim health if ownership
        // could not be returned to the reader manager.
        return health_failed();
    }
    let scan = match scan {
        Ok(Ok(scan)) => scan,
        Err(_) => return health_failed(),
        Ok(Err(HealthReadError::BridgePreflight)) if !lease_lost => {
            return health_failed();
        }
        Ok(Err(_)) => return health_failed(),
    };
    if state.reader_gate.selected_reader_generation()
        != Some((lease.reader.clone(), lease.reader_generation))
    {
        return health_failed();
    }
    if scan == HealthProfiles::NoEuicc {
        return if lease_has_usim_identity(&lease) {
            json_response(
                StatusCode::OK,
                &json!({
                    "card_type":"physical_sim", "state":"physical_sim",
                    "operation": current_health_operation(state, &lease).await,
                }),
            )
        } else {
            health_failed()
        };
    }
    let operation = current_health_operation(state, &lease).await;
    health_response(scan, operation)
}

/// Only a positive native identity receipt plus a fresh all-AID miss proves
/// the physical-SIM projection. Cached has_esim=false alone is insufficient.
fn lease_has_usim_identity(lease: &NativeReaderSessionLease) -> bool {
    !lease.identity.iccid.is_empty() && !lease.identity.imsi.is_empty()
}

fn physical_sim_overview(operation: Option<OperationSnapshot>) -> Response<Full<Bytes>> {
    json_response(
        StatusCode::OK,
        &json!({
            "card_type":"physical_sim",
            "message":"读卡器中的卡片为实体 SIM，不支持 eSIM Profile 管理",
            "profiles":[],
            "capabilities": {
                "can_refresh":false, "can_download":false, "can_switch":false,
                "can_rename":false, "can_delete":false, "can_probe_phonebook":false,
                "vendor_serial_available":false,
            },
            "operation":operation,
        }),
    )
}

pub(crate) fn no_selection() -> Response<Full<Bytes>> {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "reader_esim_unavailable",
        "读卡器 eSIM 管理服务不可用（未连接读卡器或当前卡片非 eUICC）",
        true,
    )
}

/// The retained lifecycle validates this exact generation after admission.
/// A replacement card/selection may never receive the previous write's refresh.
async fn refresh_written_reader(
    state: &ServerState,
    reader: &str,
    generation: u64,
) -> Result<(), &'static str> {
    let lifecycle = state
        .reader_lifecycle()
        .map_err(|_| "reader_lifecycle_unavailable")?;
    let response = lifecycle
        .handle_service(json!({
            "action": "refresh", "reader": reader, "expected_generation": generation,
        }))
        .await
        .map_err(|failure| failure.code)?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("reader_refresh_unconfirmed");
    }
    Ok(())
}

/// Do not persist a terminal success until the reader lease and its final
/// refresh have both been confirmed. Preserve the primary download failure,
/// while recording a safe finalization code in its diagnostic when needed.
fn apply_reader_finalization(
    result: &mut Result<DownloadResult, DownloadFailure>,
    finalization: Result<(), &'static str>,
) {
    let Err(code) = finalization else { return };
    let (message, warning_code) = if code == "reader_session_lease_release_failed" {
        (
            "Profile 已安装，但读卡器会话释放未确认；请刷新读卡器状态",
            "reader_session_lease_release_failed",
        )
    } else {
        (
            "Profile 已安装，但读卡器状态刷新未确认；请手动刷新 Profile 列表",
            "reader_profile_refresh_unconfirmed",
        )
    };
    match result {
        Ok(download) => {
            let mut warning = download.warning.take().unwrap_or_default();
            let mut codes = download.warning_code.take().unwrap_or_default();
            append_warning(&mut warning, &mut codes, message, warning_code);
            download.warning = Some(warning);
            download.warning_code = Some(codes);
        }
        Err(DownloadFailure::Cancelled) => {
            *result = Err(DownloadFailure::Failed {
                code: warning_code.into(),
                message: "下载已取消，但读卡器最终状态未确认；刷新 Profile 列表后再继续".into(),
                diagnostic: format!("reader finalization not confirmed ({code})"),
            });
        }
        Err(DownloadFailure::Failed { diagnostic, .. }) => {
            diagnostic.push_str(&format!("; reader finalization not confirmed ({code})"));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HealthProfiles {
    NoEuicc,
    ReadFailed,
    Listed(Vec<HealthProfile>),
}

#[derive(Debug)]
enum HealthReadError {
    /// Native reader status/configuration failed before the first APDU.
    BridgePreflight,
    Operation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct HealthProfile {
    iccid: String,
    name: String,
    service_provider_name: String,
    state: i32,
    state_text: &'static str,
    class_text: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReaderOverview {
    eids: Vec<Value>,
    profiles: Vec<Value>,
    product: Option<ReaderProductInfo>,
    // Private evidence for journal reconciliation. Never serialized to HTTP.
    card_identities: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EuiccFacts {
    pub(crate) available: bool,
    pub(crate) notification_aids: Vec<Vec<u8>>,
    pub(crate) profile_name: String,
}

/// Probe actual ISD-R/EID/profile facts while the lifecycle owner holds the
/// physical transaction. An I/O failure is not evidence of a non-eUICC card.
pub(crate) fn read_lifecycle_facts<T: ApduTransport>(
    transport: &mut T,
) -> Result<EuiccFacts, String>
where
    T::Error: fmt::Display,
{
    let Some(overview) = read_overview_on_transport(transport)? else {
        return Ok(EuiccFacts::default());
    };
    Ok(lifecycle_facts_from_overview(&overview))
}

/// N-wait must bind the live eUICC (and requested ISD-R) before reading a
/// profile or issuing AUTH. Inventory and an enabled-profile flag are not proof.
/// Inventory uses the same AID probing and profile decoding as the reader page.
/// Failed profile groups are not silently published as an empty card.
pub(crate) fn multistandby_inventory<T: ApduTransport>(transport: &mut T) -> Result<Value, String>
where
    T::Error: fmt::Display,
{
    let overview = read_overview_on_transport(transport)?
        .ok_or_else(|| "请先连接含eSIM的读卡器".to_owned())?;
    if overview
        .profiles
        .iter()
        .any(|group| group["read_state"] != "ready")
    {
        return Err("Profile 列表读取失败，请刷新后重试".into());
    }
    Ok(json!({"groups":overview.profiles}))
}

pub(crate) fn multistandby_euicc<T: ApduTransport>(
    transport: &mut T,
    expected_eid: &str,
    requested_aid: Option<&[u8]>,
) -> Result<EuiccFacts, String>
where
    T::Error: fmt::Display,
{
    let overview = read_overview_on_transport(transport)?
        .ok_or_else(|| "multistandby_identity_changed".to_owned())?;
    let matches = overview.eids.iter().any(|entry| {
        entry["eid"]
            .as_str()
            .is_some_and(|eid| eid.eq_ignore_ascii_case(expected_eid))
            && requested_aid.is_none_or(|aid| {
                entry["aid"]
                    .as_str()
                    .and_then(notification_aid_bytes)
                    .as_deref()
                    == Some(aid)
            })
    });
    if !matches {
        return Err("multistandby_identity_changed".into());
    }
    Ok(lifecycle_facts_from_overview(&overview))
}

/// Uses the same real ES10c implementation, including CAT_BUSY-only retries.
pub(crate) fn multistandby_enable<T: ApduTransport>(
    transport: &mut T,
    iccid: &str,
    aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String>
where
    T::Error: fmt::Display,
{
    switch_profile_on_transport(transport, iccid, aid, cancelled).map_err(|e| e.diagnostic())
}

fn lifecycle_facts_from_overview(overview: &ReaderOverview) -> EuiccFacts {
    let profile_name = overview
        .profiles
        .iter()
        .filter_map(|group| group["profiles"].as_array())
        .flatten()
        .find(|profile| profile["state"].as_i64() == Some(1))
        .map(|profile| {
            let provider = profile["service_provider_name"]
                .as_str()
                .unwrap_or_default()
                .trim();
            if provider.is_empty() {
                profile["name"].as_str().unwrap_or_default().trim()
            } else {
                provider
            }
        })
        .unwrap_or_default()
        .to_owned();
    EuiccFacts {
        available: !overview.eids.is_empty(),
        notification_aids: overview
            .eids
            .iter()
            .filter_map(|eid| eid["aid"].as_str().and_then(notification_aid_bytes))
            .collect(),
        profile_name,
    }
}

/// Preserve switch-finalization notification delivery and report outstanding
/// enable/disable entries; no card operation is retried by this lifecycle layer.
pub(crate) fn process_lifecycle_notifications<T: ApduTransport>(
    transport: &mut T,
    aids: &[Vec<u8>],
) -> Result<(), String>
where
    T::Error: fmt::Display,
{
    let pending = list_notifications_on_transport(
        transport,
        aids,
        aids.len() == 1,
        Arc::new(AtomicBool::new(false)),
    )
    .map_err(|_| "切卡通知同步失败".to_owned())?;
    if pending
        .iter()
        .any(|item| matches!(item.event, "enable" | "disable"))
    {
        return Err("运营商切卡通知仍待发送".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReaderProductInfo {
    sku_name: Option<String>,
    serial_number: Option<String>,
    firmware: Option<String>,
}

impl ReaderOverview {
    fn response(&self, operation: Option<OperationSnapshot>) -> Value {
        let product = self.product.as_ref();
        let firmware = product
            .and_then(|product| product.firmware.clone())
            .or_else(|| {
                self.eids
                    .first()
                    .and_then(|eid| eid["firmware"].as_str())
                    .map(str::to_owned)
            });
        let mut chip_info = json!({"eids": self.eids});
        if let Some(firmware) = firmware {
            chip_info["firmware"] = json!(firmware);
        }
        if let Some(sku_name) = product.and_then(|product| product.sku_name.as_ref()) {
            chip_info["sku_name"] = json!(sku_name);
            let mut identity = json!({
                "model": sku_name,
                "source": "card_private_aid",
                "confidence": "confirmed",
                "rule_id": "card-private-product-aid-v1",
            });
            if sku_name.to_ascii_lowercase().contains("estk") {
                identity["brand"] = json!("eSTK.me");
            }
            chip_info["identity"] = identity;
        }
        if let Some(serial_number) = product.and_then(|product| product.serial_number.as_ref()) {
            chip_info["serial_number"] = json!(serial_number);
        }
        let mut response = json!({
            "card_type": "euicc",
            "chip_info": chip_info,
            "profiles": self.profiles,
            "capabilities": {
                "can_refresh": !self.eids.is_empty(),
                "can_download": !self.eids.is_empty(),
                "can_switch": !self.eids.is_empty(),
                "can_rename": !self.eids.is_empty(),
                "can_delete": !self.eids.is_empty(),
                "can_probe_phonebook": false,
                "vendor_serial_available": product
                    .and_then(|product| product.serial_number.as_ref())
                    .is_some_and(|serial| !serial.trim().is_empty()),
            },
            "updated_at": time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        });
        if let Some(operation) = operation {
            response["operation"] = json!(operation);
        }
        response
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverviewReadError {
    BridgePreflight,
    TransportPreflight,
    Operation,
}

fn overview_transport_failure(
    error: &crate::reader_pcsc::Failure,
    entered: bool,
) -> OverviewReadError {
    if !entered
        && !error.submitted
        && !error.uncertain
        && matches!(
            error.code,
            "reader_pcsc_busy" | "reader_pcsc_io_failed" | "reader_pcsc_unresponsive_card"
        )
    {
        OverviewReadError::TransportPreflight
    } else {
        OverviewReadError::Operation
    }
}

fn read_overview_on_transport<T>(transport: &mut T) -> Result<Option<ReaderOverview>, String>
where
    T: ApduTransport,
    T::Error: fmt::Display,
{
    use std::collections::HashSet;

    let mut overview = ReaderOverview {
        eids: Vec::new(),
        profiles: Vec::new(),
        product: None,
        card_identities: Vec::new(),
    };
    let mut seen_eids = HashSet::new();
    let mut successful_aids: Vec<&[u8]> = Vec::new();
    for aid in KNOWN_ISDR_APPLICATION_AIDS {
        if !successful_aids.is_empty()
            && !successful_aids
                .iter()
                .any(|found| is_estkme_application_aid(found) && is_estkme_application_aid(aid))
        {
            break;
        }
        let guarded = LeaseAwareTransport {
            inner: BorrowedTransport(&mut *transport),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let channel = match proletarius_reader::euicc_transport::open_euicc_channel(guarded, aid) {
            Ok(channel) => channel,
            // A failed channel close is cleanup-unknown, never an AID miss.
            Err(error) if missing_isdr_aid(&error) => continue,
            // Do not treat security/status/transport failures as an AID miss
            // or continue probing after an ambiguous card response.
            Err(error) => return Err(error.to_string()),
        };
        let channel_number = channel.channel();
        let mut card =
            Euicc::new(channel, channel_number, READER_MSS).map_err(|error| error.to_string())?;
        let outcome = (|| -> Result<(), String> {
            let mut eid_bytes = [0_u8; proletarius_euicc::EID_LENGTH_BYTES];
            let eid_length = card
                .get_eid(&mut eid_bytes)
                .map_err(|error| error.to_string())?;
            if format_eid(&eid_bytes[..eid_length]).is_none() {
                return Err("invalid eUICC EID encoding".into());
            }
            let eid = encode_hex_lower(&eid_bytes[..eid_length]);
            let first_eid = seen_eids.insert(eid.clone());
            successful_aids.push(aid);
            if !first_eid {
                // Display deduplicates by EID, but proof evidence cannot. A
                // second AID is eligible only after its own ListProfiles succeeds.
                let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
                if card.list_profiles_into(&mut profiles).is_ok() {
                    overview
                        .card_identities
                        .push((eid_bytes[..eid_length].to_vec(), aid.to_vec()));
                }
                return Ok(());
            }

            let euicc_info = card.get_euicc_info_preferred().ok();
            let firmware = euicc_info.as_ref().and_then(|info| info.firmware_string());
            let nvram_bytes = euicc_info
                .as_ref()
                .and_then(|info| info.free_nvram_bytes)
                .unwrap_or_default();
            let mut eid_info = json!({
                "aid": encode_hex_upper(aid),
                "eid": eid,
                "spec_guess": "sgp22_compatible",
                "spec_confidence": "inferred",
                "free_nvram_bytes": nvram_bytes.min(i32::MAX as u64),
                "free_nvram": format_nvram(nvram_bytes),
            });
            if let Some(firmware) = firmware.as_ref() {
                eid_info["firmware"] = json!(firmware);
            }
            if let Some(info) = euicc_info.as_ref() {
                eid_info["info_source"] = json!(format!("euicc_info{}", info.version));
                eid_info["info_version"] = json!(info.version.to_string());
                let sas = info.sas_accreditation_number_str().unwrap_or_default();
                if !sas.is_empty() {
                    eid_info["sas_accreditation_number"] = json!(sas);
                }
                if let Some(manufacturer) = lookup_euicc_manufacturer(&eid, sas) {
                    eid_info["manufacturer"] = json!(manufacturer);
                }
                let certificates = lookup_certificate_issuers(&info.certificate_key_ids);
                if !certificates.is_empty() {
                    eid_info["certificates"] = json!(certificates);
                }
            }
            if let Ok(addresses) = card.get_configured_addresses() {
                if let Some(address) = addresses
                    .default_smdp_address_str()
                    .filter(|address| !address.is_empty())
                {
                    eid_info["default_smdp_address"] = json!(address);
                }
                if let Some(address) = addresses
                    .root_smds_address_str()
                    .filter(|address| !address.is_empty())
                {
                    eid_info["root_ds_address"] = json!(address);
                }
            }
            overview.eids.push(eid_info);

            let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
            match card.list_profiles_into(&mut profiles) {
                Ok(count) => {
                    overview
                        .card_identities
                        .push((eid_bytes[..eid_length].to_vec(), aid.to_vec()));
                    overview.profiles.push(json!({
                    "eid": eid,
                    "aid_hex": encode_hex_upper(aid),
                    "read_state": "ready",
                    "profiles": profiles[..count].iter().map(overview_profile).collect::<Vec<_>>(),
                }));
                }
                Err(_) => overview.profiles.push(json!({
                    "eid": eid,
                    "aid_hex": encode_hex_upper(aid),
                    "read_state": "error",
                    "read_error": "Profile 列表读取失败，请刷新后重试",
                    "profiles": [],
                })),
            }
            Ok(())
        })();
        card.transport_mut()
            .close()
            .map_err(|error| format!("逻辑通道关闭未确认：{error}"))?;
        outcome?;
    }

    if overview.eids.is_empty() {
        return Ok(None);
    }
    overview.product = read_estk_product_info_on_transport(transport)?;
    Ok(Some(overview))
}

fn read_estk_product_info_on_transport<T>(
    transport: &mut T,
) -> Result<Option<ReaderProductInfo>, String>
where
    T: ApduTransport,
    T::Error: fmt::Display,
{
    let guarded = LeaseAwareTransport {
        inner: BorrowedTransport(&mut *transport),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let channel = match proletarius_reader::euicc_transport::open_euicc_channel(
        guarded,
        proletarius_euicc::ESTKME_PRODUCT_APPLICATION_AID,
    ) {
        Ok(channel) => channel,
        // Product metadata is optional. A known card rejection has no pending
        // allocation, whereas failed cleanup/transport must remain visible.
        Err(proletarius_reader::euicc_transport::Error::Protocol(CcidError::CardStatus(_))) => {
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };
    let channel_number = channel.channel();
    let mut card =
        Euicc::new(channel, channel_number, READER_MSS).map_err(|error| error.to_string())?;
    let sku_name = read_product_field(&mut card, ProductField::SkuName);
    let serial_number = read_product_field(&mut card, ProductField::SerialNumber);
    let bootloader = read_product_field(&mut card, ProductField::Bootloader);
    let product_firmware = read_product_field(&mut card, ProductField::Firmware);
    let firmware = match (bootloader, product_firmware) {
        (Some(bootloader), Some(firmware)) => Some(format!("{bootloader}-{firmware}")),
        (None, firmware) => firmware,
        (Some(_), None) => None,
    };
    let product = ReaderProductInfo {
        sku_name,
        serial_number,
        firmware,
    };
    card.transport_mut()
        .close()
        .map_err(|error| format!("逻辑通道关闭未确认：{error}"))?;
    Ok(Some(product))
}

pub(crate) fn read_product_field<T: CardTransport>(
    card: &mut Euicc<T>,
    field: ProductField,
) -> Option<String> {
    let mut output = [0_u8; 512];
    let length = card.read_product_field_into(field, &mut output).ok()?;
    let value = std::str::from_utf8(&output[..length]).ok()?.to_owned();
    (!value.trim().is_empty()).then_some(value)
}

pub(crate) fn is_estkme_application_aid(aid: &[u8]) -> bool {
    aid.starts_with(&[0xa0, 0x65, 0x73, 0x74, 0x6b, 0x6d, 0x65])
}

pub(crate) fn overview_profile(profile: &ProfileInfo) -> Value {
    let enabled = profile.state == ProfileState::Enabled;
    let name = profile
        .nickname_str()
        .filter(|name| !name.is_empty())
        .or_else(|| profile.profile_name_str())
        .unwrap_or_default();
    let state = match profile.state {
        ProfileState::Disabled => 0,
        ProfileState::Enabled => 1,
        ProfileState::Unknown(state) => i32::from(state),
    };
    let class_text = match profile.class {
        ProfileClass::Test => "test",
        ProfileClass::Provisioning => "provisioning",
        ProfileClass::Operational => "operational",
        ProfileClass::Unknown(_) => "unknown",
    };
    json!({
        "iccid": profile.iccid_string().unwrap_or_default(),
        "name": name,
        "service_provider_name": profile.service_provider_name_str().unwrap_or_default(),
        "state": state,
        "state_text": if enabled { "已启用" } else { "已禁用" },
        "class_text": class_text,
    })
}

pub(crate) fn format_nvram(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn encode_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
}

pub(crate) fn encode_hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
}

#[derive(Deserialize)]
struct PkiIssuer {
    #[serde(rename = "key-id")]
    key_id: String,
    name: String,
}

#[derive(Deserialize)]
struct AccreditedPki {
    suppliers: Vec<AccreditedSupplier>,
}

#[derive(Deserialize)]
struct AccreditedSupplier {
    name: String,
    #[serde(default)]
    eum: Vec<String>,
    country: String,
    #[serde(default)]
    locations: HashMap<String, String>,
}

pub(crate) fn lookup_euicc_manufacturer(
    eid: &str,
    sas_accreditation_number: &str,
) -> Option<String> {
    let eum = eid.get(..8)?;
    let (_, suppliers) = euicc_pki_data();
    let supplier = suppliers.iter().find(|supplier| {
        supplier
            .eum
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(eum))
    })?;
    let region = sas_accreditation_number
        .get(..5)
        .and_then(|site| supplier.locations.get(site))
        .map(String::as_str)
        .unwrap_or(&supplier.country);
    Some(format!("{} {}", supplier.name, region_flag(region)))
}

pub(crate) fn lookup_certificate_issuers(key_ids: &[Vec<u8>]) -> Vec<String> {
    let (issuers, _) = euicc_pki_data();
    key_ids
        .iter()
        .map(|key_id| {
            let encoded = encode_hex_lower(key_id);
            issuers
                .iter()
                .find(|issuer| encoded.starts_with(&issuer.key_id.to_ascii_lowercase()))
                .map(|issuer| issuer.name.clone())
                .unwrap_or(encoded)
        })
        .collect()
}

fn region_flag(region: &str) -> String {
    let bytes = region.as_bytes();
    if bytes.len() < 2 || !bytes[..2].iter().all(u8::is_ascii_alphabetic) {
        return String::new();
    }
    bytes[..2]
        .iter()
        .map(|byte| {
            let letter = byte.to_ascii_uppercase();
            char::from_u32(0x1f1e6 + u32::from(letter - b'A')).unwrap_or_default()
        })
        .collect()
}

fn euicc_pki_data() -> (&'static [PkiIssuer], &'static [AccreditedSupplier]) {
    static DATA: OnceLock<(Vec<PkiIssuer>, Vec<AccreditedSupplier>)> = OnceLock::new();
    let (issuers, accredited) = DATA.get_or_init(|| {
        let issuers: Vec<PkiIssuer> =
            serde_json::from_str(proletarius_euicc::PKI_CERTIFICATE_ISSUERS_JSON)
                .expect("bundled eUICC certificate issuer data must be valid JSON");
        let accredited: AccreditedPki =
            serde_json::from_str(proletarius_euicc::PKI_ACCREDITED_SUPPLIERS_JSON)
                .expect("bundled eUICC accreditation data must be valid JSON");
        (issuers, accredited.suppliers)
    });
    (issuers, accredited)
}

fn read_health_profiles(
    context: &NativeCardContext,
    cancelled: Arc<AtomicBool>,
) -> Result<HealthProfiles, String> {
    context
        .with_transaction(|transport| {
            let mut guarded = LeaseAwareTransport {
                inner: BorrowedTransport(transport),
                cancelled,
            };
            read_health_profiles_on_transport(&mut guarded)
        })
        .map_err(|error| error.to_string())?
}

fn read_health_profiles_on_transport<T>(transport: &mut T) -> Result<HealthProfiles, String>
where
    T: ApduTransport,
    T::Error: fmt::Display,
{
    for aid in KNOWN_ISDR_APPLICATION_AIDS {
        let guarded = LeaseAwareTransport {
            inner: BorrowedTransport(&mut *transport),
            // The outer lease-aware transport enforces cancellation for both
            // native PC/SC and BLE bridge APDUs.
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let channel = match proletarius_reader::euicc_transport::open_euicc_channel(guarded, aid) {
            Ok(channel) => channel,
            // A failed channel close is cleanup-unknown, never an AID miss.
            Err(error) if missing_isdr_aid(&error) => continue,
            // Do not downgrade a security/status/transport failure into an AID
            // miss. In particular this path does not retry or bypass 6982.
            Err(error) => return Err(error.to_string()),
        };
        let channel_number = channel.channel();
        let mut card =
            Euicc::new(channel, channel_number, READER_MSS).map_err(|error| error.to_string())?;
        let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
        let listed = card.list_profiles_into(&mut profiles);
        card.transport_mut()
            .close()
            .map_err(|error| format!("逻辑通道关闭未确认：{error}"))?;
        let count = match listed {
            Ok(count) => count,
            Err(_) => return Ok(HealthProfiles::ReadFailed),
        };
        return Ok(HealthProfiles::Listed(
            profiles[..count].iter().map(health_profile).collect(),
        ));
    }
    Ok(HealthProfiles::NoEuicc)
}

fn missing_isdr_aid<E>(error: &proletarius_reader::euicc_transport::Error<E>) -> bool {
    matches!(
        error,
        proletarius_reader::euicc_transport::Error::Protocol(CcidError::CardStatus(0x6a82))
    )
}

fn health_profile(profile: &ProfileInfo) -> HealthProfile {
    let name = profile
        .nickname_str()
        .filter(|name| !name.is_empty())
        .or_else(|| profile.profile_name_str())
        .unwrap_or_default()
        .to_owned();
    let enabled = profile.state == ProfileState::Enabled;
    HealthProfile {
        iccid: profile.iccid_string().unwrap_or_default(),
        name,
        service_provider_name: profile
            .service_provider_name_str()
            .unwrap_or_default()
            .to_owned(),
        state: i32::from(enabled),
        state_text: if enabled { "已启用" } else { "已禁用" },
        class_text: match profile.class {
            ProfileClass::Test => "test",
            ProfileClass::Provisioning => "provisioning",
            ProfileClass::Operational => "operational",
            ProfileClass::Unknown(_) => "unknown",
        },
    }
}

fn current_for_reader_lease(
    operations: &Service,
    gate: &crate::reader_reservation::Gate,
    reader: &str,
    generation: u64,
) -> Option<OperationSnapshot> {
    // The completed APDU belongs to this captured lease. A later selection
    // must never attach its operation to the old card's health/overview body.
    gate.with_selected_generation(reader, generation, || {
        operations.current_for_selected(Some((reader.to_owned(), generation)))
    })
    .ok()
    .flatten()
}

async fn current_health_operation(
    state: &ServerState,
    lease: &NativeReaderSessionLease,
) -> Option<OperationSnapshot> {
    current_for_reader_lease(
        service(),
        &state.reader_gate,
        &lease.reader,
        lease.reader_generation,
    )
}

fn health_response(
    profiles: HealthProfiles,
    operation: Option<OperationSnapshot>,
) -> Response<Full<Bytes>> {
    match profiles {
        HealthProfiles::NoEuicc => health_failed(),
        HealthProfiles::ReadFailed => json_response(
            StatusCode::OK,
            &json!({
                "card_type": "euicc",
                "state": "profile_read_error",
                "message": "读卡器 eSIM 卡片已识别，但 Profile 状态读取失败",
                "operation": operation,
            }),
        ),
        HealthProfiles::Listed(profiles) => {
            let active = profiles.into_iter().find(|profile| profile.state == 1);
            match active {
                Some(active) => json_response(
                    StatusCode::OK,
                    &json!({
                        "card_type": "euicc",
                        "state": "active",
                        "active_profile": active,
                        "operation": operation,
                    }),
                ),
                None => json_response(
                    StatusCode::OK,
                    &json!({
                        "card_type": "euicc",
                        "state": "no_active_profile",
                        "message": "读卡器 eSIM 卡片已识别，但没有已启用的 Profile",
                        "operation": operation,
                    }),
                ),
            }
        }
    }
}

fn health_failed() -> Response<Full<Bytes>> {
    failure(
        StatusCode::BAD_GATEWAY,
        "reader_esim_health_failed",
        "读取读卡器 eSIM 运行状态失败",
        true,
    )
}

pub(crate) async fn start(
    request: Request<Incoming>,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let body: DownloadRequest = match decode_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::reader_request(
        state,
        &reader,
        &body.client_request_id,
        "download",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let state = state.clone();
    match spawn_retained_operation(async move {
        crate::esim_mutation_intent::execute(intent, || start_reader_download(body, state, reader))
            .await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_download_owner_failed",
            "下载结果待确认，请刷新 Profile 状态后再操作",
            true,
        ),
    }
}

/// Retain reader download admission and cleanup independently of the HTTP
/// waiter. Shutdown closes admission first, then waits on the durable gate
/// held by this owner through lease release and the final refresh.
async fn start_reader_download(
    body: DownloadRequest,
    owned_state: ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let state = &owned_state;
    if service().active() {
        return failure(
            StatusCode::CONFLICT,
            "esim_operation_busy",
            "已有 eSIM 操作正在进行，请等待完成后重试",
            true,
        );
    }
    let (activation, aid, imei, matching_id, confirmation_code) = match prepare_request(body) {
        Ok(prepared) => prepared,
        Err((code, message)) => return failure(StatusCode::BAD_REQUEST, code, message, false),
    };

    // One retained mutation admission fences replacement startup before
    // stopping ordinary IMS, and remains held through real card lease release.
    let lease = match NativeReaderSessionLease::acquire_for_esim_mutation(
        &state.go_socket_path,
        state.app_token.as_deref(),
        &reader,
        state.reader_gate.clone(),
        state.reader_pcsc.clone(),
    )
    .await
    {
        Ok(lease) => Arc::new(lease),
        Err(_) => {
            return failure(
                StatusCode::CONFLICT,
                "reader_session_lease_busy",
                "读卡器仍被其它操作占用，下载尚未开始",
                true,
            );
        }
    };
    if selected_reader(state).as_deref() != Some(reader.as_str()) {
        if lease.release().await.is_err() {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_session_lease_release_failed",
                "读卡器选择已变化且租约释放未确认；下载尚未开始，请刷新读卡器状态",
                true,
            );
        }
        return failure(
            StatusCode::CONFLICT,
            "reader_session_lease_busy",
            "读卡器选择已变化，下载尚未开始；请刷新后重试",
            true,
        );
    }
    let operation_service = service();
    let begin = journal_pool().run(move || operation_service.begin()).await;
    let permit = match begin {
        Ok(Ok(permit)) => permit,
        Ok(Err(BeginError::Busy)) => {
            let _ = lease.release().await;
            return failure(
                StatusCode::CONFLICT,
                "esim_operation_busy",
                "已有 eSIM 操作正在进行，请等待完成后重试",
                true,
            );
        }
        Ok(Err(BeginError::Journal | BeginError::Unavailable)) | Err(_) => {
            let _ = lease.release().await;
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_operation_journal_failed",
                "无法安全记录 eSIM 下载状态，操作未开始",
                true,
            );
        }
        Ok(Err(BeginError::ReconciliationRequired)) => {
            let _ = lease.release().await;
            return failure(
                StatusCode::CONFLICT,
                "esim_operation_reconciliation_required",
                "上次下载结果未知，请先刷新并核对 Profile 列表，再开始新下载",
                true,
            );
        }
    };
    if !operation_service.bind_reader_owner(&permit.id, &lease.reader, lease.reader_generation) {
        operation_service.finish(
            &permit.id,
            Err(failed(
                "reader_operation_owner_missing",
                "无法确认读卡器操作归属，下载尚未开始",
                "native reader lease could not bind operation ownership",
            )),
        );
        let _ = lease.release().await;
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_operation_owner_missing",
            "无法确认读卡器操作归属，下载尚未开始",
            true,
        );
    }
    let snapshot = service().current().expect("operation was just admitted");
    let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
    let mut lease_lost = heartbeat.lost_receiver();
    let DownloadPermit {
        id,
        confirmation,
        cancelled,
        _ownership: operation_owner,
        _operation_gate: operation_gate,
    } = permit;
    let worker_cancelled = cancelled.clone();
    let is_ble_reader = reader.starts_with("BLE:");
    let card_context = (!is_ble_reader).then(|| lease.pcsc_context(state.reader_pcsc.clone()));
    let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
    let worker_reader = reader.clone();
    let operation_service = service();
    let refresh_state = owned_state.clone();
    let app_token = state.app_token.clone();
    tokio::spawn(async move {
        let lease_watch_cancelled = worker_cancelled.clone();
        let lease_watch = tokio::spawn(async move {
            if !*lease_lost.borrow() {
                let _ = lease_lost.changed().await;
            }
            if *lease_lost.borrow() {
                lease_watch_cancelled.store(true, Ordering::Release);
            }
        });
        let worker_service = operation_service;
        let worker_id = id.clone();
        let worker_token = app_token.clone();
        let result = lease
            .run_card_worker(download_pool(), move || {
                if worker_reader.starts_with("BLE:") {
                    let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                        ble_socket.as_deref(),
                        worker_token.as_deref(),
                        &worker_reader,
                    )
                    .map_err(|error| {
                        failed("reader_connect_failed", "无法连接选中的蓝牙读卡器", error)
                    })?;
                    run_download_on_transport(
                        &mut transport,
                        activation,
                        &imei,
                        aid,
                        &matching_id,
                        &confirmation_code,
                        confirmation,
                        worker_cancelled,
                        worker_service,
                        &worker_id,
                    )
                } else {
                    let context = card_context
                        .expect("non-BLE reader has a PC/SC context")
                        .map_err(|error| {
                            failed(
                                "reader_connect_failed",
                                "无法取得读卡器原生租约",
                                error.to_string(),
                            )
                        })?;
                    run_download(
                        &context,
                        activation,
                        &imei,
                        aid,
                        &matching_id,
                        &confirmation_code,
                        confirmation,
                        worker_cancelled,
                        worker_service,
                        &worker_id,
                    )
                }
            })
            .await;
        lease_watch.abort();
        heartbeat.stop().await;
        let release = lease.release().await;
        let mut result = match result {
            Ok(result) => result,
            Err(error) => Err(DownloadFailure::Failed {
                code: "reader_download_worker_failed".into(),
                message: "读卡器下载任务异常结束；请先刷新 Profile 列表确认结果".into(),
                diagnostic: error.to_string(),
            }),
        };
        let finalization = if release.is_ok() {
            refresh_written_reader(&refresh_state, &lease.reader, lease.reader_generation).await
        } else {
            Err("reader_session_lease_release_failed")
        };
        apply_reader_finalization(&mut result, finalization);
        worker_service.finish(&id, result);
        drop(operation_owner);
        // Keep reader, module, and profile mutations fenced until the complete
        // detached owner has released its lease and finished the refresh.
        drop(operation_gate);
    });
    json_response(StatusCode::ACCEPTED, &snapshot)
}

/// Own the native module eSIM download workflow, including confirmation,
/// progress, card I/O and durable operation state.
pub(crate) async fn start_module_download(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    if !crate::module_esim_transport::native(state) {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_unavailable",
            "Rust eSIM 管理服务当前不可用，尚未提交下载",
            true,
        );
    }
    let body: DownloadRequest = match decode_module_download_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::module_request(
        state,
        &body.client_request_id,
        "download",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let state = state.clone();
    // Retain admission, preflight and RF cleanup even when HTTP is cancelled.
    match tokio::spawn(async move {
        crate::esim_mutation_intent::execute(intent, || start_module_download_native(body, state))
            .await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_download_worker_failed",
            "下载结果待确认，请刷新操作状态",
            true,
        ),
    }
}

async fn start_module_download_native(
    body: DownloadRequest,
    owned_state: ServerState,
) -> Response<Full<Bytes>> {
    let state = &owned_state;
    let (activation, requested_aid, imei, matching_id, confirmation_code) =
        match prepare_request(body) {
            Ok(prepared) => prepared,
            Err((code, message)) => {
                return failure(StatusCode::BAD_REQUEST, code, message, false);
            }
        };
    if service().active() {
        return failure(
            StatusCode::CONFLICT,
            "esim_operation_busy",
            "已有 eSIM 操作正在进行，请等待完成后重试",
            true,
        );
    }

    let operation_service = service();
    let begin = journal_pool().run(move || operation_service.begin()).await;
    let permit = match begin {
        Ok(Ok(permit)) => permit,
        Ok(Err(BeginError::Busy)) => {
            return failure(
                StatusCode::CONFLICT,
                "esim_operation_busy",
                "已有 eSIM 操作正在进行，请等待完成后重试",
                true,
            );
        }
        Ok(Err(BeginError::ReconciliationRequired)) => {
            return failure(
                StatusCode::CONFLICT,
                "esim_operation_reconciliation_required",
                "上次下载结果未知，请先刷新并核对 Profile 列表，再开始新下载",
                true,
            );
        }
        Ok(Err(BeginError::Journal | BeginError::Unavailable)) | Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_operation_journal_failed",
                "无法安全记录 eSIM 下载状态，操作未开始",
                true,
            );
        }
    };
    let id = permit.id.clone();
    if let Err(error) = crate::module_esim::stop_module_vowifi_before_write(state).await {
        // Keep the established download error categories while the typed
        // native boundary distinguishes call/startup/cleanup internally.
        let code = if error.status == StatusCode::CONFLICT {
            "module_vowifi_busy"
        } else {
            "module_vowifi_stop_unavailable"
        };
        operation_service.finish(
            &id,
            Err(failed(
                code,
                error.message,
                "native module IMS/RF retirement rejected download before APDU submission",
            )),
        );
        drop(permit);
        return failure(error.status, code, error.message, true);
    }

    let reservation = match state.module_radio.reserve_esim().await {
        Ok(reservation) => reservation,
        Err(error) => {
            let code = error.public_code();
            let message = error.public_message().to_owned();
            operation_service.finish(
                &id,
                Err(failed(
                    code,
                    &message,
                    "module eSIM radio reservation failed before APDU submission",
                )),
            );
            drop(permit);
            return error.module_radio_compat_response();
        }
    };
    if !operation_service.update(&id, "running", "preflight", "正在准备下载 Profile…", 5) {
        let release_failed = state.module_radio.release_esim(reservation).await.is_err();
        operation_service.finish(
            &id,
            Err(failed(
                "esim_operation_journal_failed",
                "无法安全记录 eSIM 下载状态，尚未操作 eUICC",
                if release_failed {
                    "operation journal update failed and module radio release is unconfirmed"
                } else {
                    "operation journal update failed before eUICC APDU submission"
                },
            )),
        );
        drop(permit);
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_operation_journal_failed",
            "无法安全记录 eSIM 下载状态，尚未操作 eUICC",
            true,
        );
    }

    let snapshot = operation_service
        .current()
        .expect("operation was just admitted");
    let worker_state = state.clone();
    let worker_service = service();
    tokio::spawn(async move {
        let DownloadPermit {
            id,
            confirmation,
            cancelled,
            _ownership: operation_owner,
            _operation_gate,
        } = permit;
        let worker_id = id.clone();
        let worker_cancelled = cancelled.clone();
        let refresh_fence = crate::module_esim_transport::mutation_fence(&worker_state);
        let transaction = crate::module_esim_transport::transaction_fenced(
            &worker_state,
            &refresh_fence,
            move |transport| {
                Ok(run_module_download_on_transport(
                    transport,
                    activation,
                    &imei,
                    requested_aid,
                    &matching_id,
                    &confirmation_code,
                    confirmation,
                    worker_cancelled,
                    worker_service,
                    &worker_id,
                ))
            },
        )
        .await;
        let refreshed = crate::module_esim_transport::refresh_reserved(
            &worker_state,
            &refresh_fence,
            &reservation,
        )
        .await;
        let release = worker_state.module_radio.release_esim(reservation).await;
        if release.is_err() {
            crate::module_esim_transport::discard_unreleased_refresh(
                &worker_state,
                refreshed.as_ref(),
            );
        }
        let mut result = match transaction {
            Ok(result) => result,
            Err(error) => Err(failed(
                "module_download_transport_failed",
                "模块下载任务异常结束；请刷新 Profile 列表确认结果",
                error.to_string(),
            )),
        };
        if release.is_err() {
            result = match result {
                Ok(mut download) => {
                    download.warning = Some("模块射频操作权释放未确认，请刷新模块状态".into());
                    download.warning_code = Some("module_radio_release_unconfirmed".into());
                    Ok(download)
                }
                Err(DownloadFailure::Failed {
                    code,
                    message,
                    mut diagnostic,
                }) => {
                    diagnostic.push_str("; module eSIM radio release is unconfirmed");
                    Err(DownloadFailure::Failed {
                        code,
                        message,
                        diagnostic,
                    })
                }
                Err(DownloadFailure::Cancelled) => Err(failed(
                    "module_radio_release_unconfirmed",
                    "下载已取消，但模块射频操作权释放未确认；请刷新模块状态",
                    "module eSIM radio release is unconfirmed after cancellation",
                )),
            };
        }
        worker_service.finish(&id, result);
        drop(operation_owner);
        drop(_operation_gate);
    });
    json_response(StatusCode::ACCEPTED, &snapshot)
}

async fn forward_module_download_compat(
    _request: Request<Incoming>,
    _state: &ServerState,
) -> Response<Full<Bytes>> {
    failure(
        StatusCode::SERVICE_UNAVAILABLE,
        "esim_unavailable",
        "eSIM 管理服务当前不可用",
        true,
    )
}

async fn decode_module_download_body(
    request: Request<Incoming>,
) -> Result<DownloadRequest, Response<Full<Bytes>>> {
    let body = match crate::http_server::collect_body(
        request.into_body(),
        MAX_MODULE_DOWNLOAD_REQUEST_BYTES,
    )
    .await
    {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(crate::http_server::body_timeout_response());
        }
        Ok(body) => body,
        Err(_) => {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "invalid_request_body",
                "请求内容无效或过大",
                false,
            ));
        }
    };
    serde_json::from_slice(&body).map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "invalid_request_body",
            "请求内容无效",
            false,
        )
    })
}

fn run_module_download_on_transport(
    transport: &mut dyn crate::module_esim_channel::AtCommandTransport<Error = crate::usb_at::Failure>,
    activation: ActivationCode,
    imei: &str,
    requested_aid: Option<Vec<u8>>,
    matching_id: &str,
    confirmation_code: &str,
    confirmation: Receiver<bool>,
    cancelled: Arc<AtomicBool>,
    operation_service: &'static Service,
    operation_id: &str,
) -> Result<DownloadResult, DownloadFailure> {
    let candidates: Vec<Vec<u8>> =
        requested_aid
            .clone()
            .map(|aid| vec![aid])
            .unwrap_or_else(|| {
                KNOWN_ISDR_APPLICATION_AIDS
                    .iter()
                    .map(|aid| aid.to_vec())
                    .collect()
            });
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err(DownloadFailure::Cancelled);
        }
        match crate::module_esim_channel::ModuleEuiccChannel::open(transport, &aid) {
            Err(crate::module_esim_channel::Error::ModemRejected) if requested_aid.is_none() => {
                // Retry only a CCHO AID that the modem explicitly rejected
                // before any APDU was submitted to the eUICC.
                continue;
            }
            Err(error) => {
                return Err(failed(
                    "download_failed",
                    "无法打开模块 eUICC 管理通道",
                    error.to_string(),
                ));
            }
            Ok(mut channel) => {
                // Reuse the same bounded LPA transaction as PC/SC and BLE.
                let mut result = run_download_on_transport(
                    &mut channel,
                    activation,
                    imei,
                    Some(aid),
                    matching_id,
                    confirmation_code,
                    confirmation,
                    cancelled,
                    operation_service,
                    operation_id,
                );
                if let Err(error) = channel.close() {
                    result = match result {
                        Ok(mut download) => {
                            download.warning =
                                Some("Profile 已安装，但 eUICC 通道关闭未确认".into());
                            download.warning_code =
                                Some("module_esim_channel_cleanup_failed".into());
                            Ok(download)
                        }
                        Err(DownloadFailure::Failed {
                            code,
                            message,
                            mut diagnostic,
                        }) => {
                            diagnostic.push_str("; eUICC logical-channel cleanup was unconfirmed");
                            Err(DownloadFailure::Failed {
                                code,
                                message,
                                diagnostic,
                            })
                        }
                        Err(DownloadFailure::Cancelled) => Err(failed(
                            "module_esim_channel_cleanup_failed",
                            "下载已取消，但 eUICC 通道关闭未确认；请刷新 Profile 列表",
                            error.to_string(),
                        )),
                    };
                }
                return result;
            }
        }
    }
    Err(failed(
        "reader_esim_unavailable",
        "未找到可用的 eUICC 管理应用",
        "modem rejected every known ISD-R AID before an APDU channel opened",
    ))
}

/// Rename profiles on selected USB/PCSC and BLE readers through Rust while
/// preserving the existing public JSON contract. The native Rust reader
/// session supplier provides the generation-bound mutation lease; if the
/// selected reader is absent from its inventory, admission fails closed.
pub(crate) async fn rename_profile(
    request: Request<Incoming>,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let body: RenameProfileRequest = match decode_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::reader_request(
        state,
        &reader,
        &body.client_request_id,
        "rename",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let (iccid, aid, name) = match validate_rename_request(body) {
        Ok(request) => request,
        Err((code, message)) => {
            return failure(StatusCode::BAD_REQUEST, code, message, false);
        }
    };
    let state = state.clone();
    with_durable_intent(intent, move || async move {
        with_profile_mutation_gate(service(), || async move {
            // Native admission applies the eSIM mutation rule and retains its shared
            // maintenance fence across ordinary IMS drain and the physical lease.
            // This preserves N-wait reservations and also permits cards without an
            // active USIM identity to rename a stored eUICC profile.
            let lease = match NativeReaderSessionLease::acquire_for_esim_mutation(
                &state.go_socket_path,
                state.app_token.as_deref(),
                &reader,
                state.reader_gate.clone(),
                state.reader_pcsc.clone(),
            )
            .await
            {
                Ok(lease) => Arc::new(lease),
                Err(_) => {
                    return failure(
                        StatusCode::CONFLICT,
                        "reader_session_lease_busy",
                        "读卡器仍由其它操作使用，Profile 名称未修改",
                        true,
                    );
                }
            };
            if selected_reader(&state).as_deref() != Some(reader.as_str()) {
                if lease.release().await.is_err() {
                    return failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "reader_session_lease_release_failed",
                        "读卡器选择已变化且租约释放未确认；Profile 名称未修改，请稍后刷新状态",
                        true,
                    );
                }
                return failure(
                    StatusCode::CONFLICT,
                    "reader_session_lease_busy",
                    "读卡器选择已变化，Profile 名称未修改；请刷新后重试",
                    true,
                );
            }
            let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
            let mut lease_lost = heartbeat.lost_receiver();
            let cancelled = Arc::new(AtomicBool::new(false));
            let watch_cancelled = cancelled.clone();
            let lease_watch = tokio::spawn(async move {
                if !*lease_lost.borrow() {
                    let _ = lease_lost.changed().await;
                }
                if *lease_lost.borrow() {
                    watch_cancelled.store(true, Ordering::Release);
                }
            });
            let card_context = if reader.starts_with("BLE:") {
                None
            } else {
                Some(lease.pcsc_context(state.reader_pcsc.clone()))
            };
            let ble_socket =
                std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
            let app_token = state.app_token.clone();
            let worker_cancelled = cancelled.clone();
            let result = lease
                .run_card_worker(download_pool(), move || {
                    if reader.starts_with("BLE:") {
                        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                            ble_socket.as_deref(),
                            app_token.as_deref(),
                            &reader,
                        )?;
                        rename_on_transport(
                            &mut transport,
                            &iccid,
                            aid.as_deref(),
                            &name,
                            worker_cancelled,
                        )
                    } else {
                        let context = card_context
                            .expect("non-BLE reader has a PC/SC context")
                            .map_err(|error| error.to_string())?;
                        rename_on_reader(&context, &iccid, aid.as_deref(), &name, worker_cancelled)
                    }
                })
                .await;
            lease_watch.abort();
            heartbeat.stop().await;
            let release = lease.release().await;
            match result {
                Ok(Ok(())) if release.is_ok() => {
                    let refresh =
                        refresh_written_reader(&state, &lease.reader, lease.reader_generation)
                            .await;
                    let warning = refresh
                        .err()
                        .map(|_| "名称已修改，但读卡器状态刷新未确认；请手动刷新 Profile 列表");
                    json_response(
                        StatusCode::OK,
                        &json!({
                            "message":"Profile 名称修改成功",
                            "warning":warning,
                            "warning_code":warning.map(|_| "reader_profile_refresh_unconfirmed"),
                        }),
                    )
                }
                Ok(Ok(())) => failure(
                    StatusCode::BAD_GATEWAY,
                    "reader_session_lease_release_failed",
                    "Profile 名称已提交，但读卡器租约释放未确认；请刷新读卡器状态",
                    true,
                ),
                Ok(Err(_)) => failure(
                    StatusCode::BAD_GATEWAY,
                    "rename_profile_failed",
                    "修改 Profile 名称失败，请确认 Profile 仍存在后重试",
                    true,
                ),
                Err(_) => failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_operation_worker_failed",
                    "读卡器操作线程异常结束，请刷新 Profile 列表确认状态",
                    true,
                ),
            }
        })
        .await
    })
    .await
}

fn validate_rename_request(
    mut request: RenameProfileRequest,
) -> Result<(String, Option<Vec<u8>>, String), (&'static str, &'static str)> {
    let iccid = request.iccid.trim().to_owned();
    let name = request.name.trim().to_owned();
    if !valid_iccid(&iccid) {
        return Err(("invalid_iccid", "ICCID 格式无效"));
    }
    if name.is_empty() || name.len() > 64 {
        return Err((
            "invalid_profile_name",
            "Profile 名称不能为空且不能超过 64 字节",
        ));
    }
    let aid = if request.aid.trim().is_empty() {
        None
    } else {
        Some(parse_hex(request.aid.trim()).ok_or(("invalid_aid", "AID 格式无效"))?)
    };
    request.iccid.zeroize();
    request.aid.zeroize();
    request.name.zeroize();
    Ok((iccid, aid, name))
}

fn valid_iccid(input: &str) -> bool {
    if !(19..=20).contains(&input.len()) || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = input
        .bytes()
        .rev()
        .enumerate()
        .map(|(index, byte)| {
            let mut digit = u32::from(byte - b'0');
            if index % 2 == 1 {
                digit *= 2;
                if digit > 9 {
                    digit -= 9;
                }
            }
            digit
        })
        .sum();
    sum % 10 == 0
}

fn rename_on_reader(
    context: &NativeCardContext,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    name: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    context
        .with_transaction(|transport| {
            rename_on_transport(transport, iccid, requested_aid, name, cancelled)
        })
        .map_err(|error| error.to_string())?
}

fn rename_on_transport<T: ApduTransport>(
    transport: &mut T,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    name: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String>
where
    T::Error: fmt::Display,
{
    let candidates: Vec<&[u8]> =
        requested_aid.map_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec(), |aid| vec![aid]);
    let mut last_error = None;
    let mut found_aid = false;
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err("reader session lease was lost".into());
        }
        match rename_on_aid(transport, aid, iccid, name, cancelled.clone()) {
            Ok(true) => return Ok(()),
            Ok(false) => found_aid = true,
            Err(AidProbeFailure::Unusable(error)) if requested_aid.is_none() => {
                // Only a read-only miss with confirmed cleanup may advance.
                last_error = Some(error);
            }
            Err(error) => {
                // After selection this may be an unknown nickname write, or
                // an unconfirmed cleanup. Never repeat it on another AID.
                return Err(error.message());
            }
        }
    }
    if found_aid {
        Err("target ICCID was not found on the eUICC".into())
    } else {
        Err(last_error.unwrap_or_else(|| "no supported ISD-R AID found".into()))
    }
}

fn rename_on_aid<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    iccid: &str,
    name: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<bool, AidProbeFailure>
where
    T::Error: fmt::Display,
{
    let guarded = LeaseAwareTransport {
        inner: BorrowedTransport(&mut *transport),
        cancelled,
    };
    let network = HttpEs9Client::new().map_err(|error| AidProbeFailure::Stop(error.to_string()))?;
    let mut client = match open_lpa_client(guarded, aid, READER_MSS, network) {
        Ok(client) => client,
        Err(proletarius_reader::euicc_lpa::ClientOpenError::Channel(error))
            if missing_isdr_aid(&error) =>
        {
            return Err(AidProbeFailure::Unusable(error.to_string()));
        }
        Err(error) => return Err(AidProbeFailure::Stop(error.to_string())),
    };
    let outcome = (|| {
        let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
        let count = client
            .card_mut()
            .list_profiles_into(&mut profiles)
            .map_err(|error| AidProbeFailure::Unusable(error.to_string()))?;
        if !profiles[..count]
            .iter()
            .any(|profile| profile.iccid_string().as_deref() == Ok(iccid))
        {
            return Ok(false);
        }
        let encoded_iccid =
            encode_iccid(iccid).map_err(|error| AidProbeFailure::Stop(error.to_string()))?;
        client
            .card_mut()
            .set_nickname(ProfileIdentifier::Iccid(&encoded_iccid), name.as_bytes())
            .map_err(|error| AidProbeFailure::Stop(error.to_string()))?;
        Ok(true)
    })();
    client
        .card_mut()
        .transport_mut()
        .close()
        .map_err(|error| AidProbeFailure::Stop(format!("逻辑通道关闭未确认：{error}")))?;
    outcome
}

pub(crate) async fn confirm(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body: ConfirmationRequest = match decode_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let operation_service = service();
    match journal_pool()
        .run(move || operation_service.confirm(body))
        .await
    {
        Ok(Ok(snapshot)) => json_response(StatusCode::OK, &snapshot),
        Ok(Err(ConfirmError::Expired)) => failure(
            StatusCode::CONFLICT,
            "esim_download_confirmation_expired",
            "下载确认已过期，请重新开始",
            true,
        ),
        Ok(Err(ConfirmError::Journal)) | Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_operation_journal_failed",
            "无法安全记录下载确认，尚未继续写入 eUICC",
            true,
        ),
    }
}

pub(crate) async fn get_operation(_state: &ServerState) -> Response<Full<Bytes>> {
    let (operation, manual_ack_available) = service().current_with_manual_ack_available();
    json_response(
        StatusCode::OK,
        &json!({
            "operation": operation,
            "manual_ack_available": manual_ack_available,
        }),
    )
}

pub(crate) async fn acknowledge_legacy_operation(
    request: Request<Incoming>,
) -> Response<Full<Bytes>> {
    let body: AcknowledgeLegacyRequest = match decode_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let operation_service = service();
    match journal_pool()
        .run(move || operation_service.acknowledge_legacy(body))
        .await
    {
        Ok(Ok(snapshot)) => json_response(StatusCode::OK, &snapshot),
        Ok(Err(AcknowledgeError::Invalid)) => failure(
            StatusCode::BAD_REQUEST,
            "esim_legacy_ack_invalid",
            "旧版操作确认参数无效",
            false,
        ),
        Ok(Err(AcknowledgeError::Conflict)) => failure(
            StatusCode::CONFLICT,
            "esim_legacy_ack_conflict",
            "操作状态已变化，请刷新后重新核对",
            true,
        ),
        Ok(Err(AcknowledgeError::Journal)) | Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_operation_journal_failed",
            "无法保存旧版操作人工确认，状态未解除",
            true,
        ),
    }
}

pub(crate) async fn get_module_operation(state: &ServerState) -> Response<Full<Bytes>> {
    // The durable native journal remains authoritative before discovery and
    // after transport loss; unresolved outcomes cannot be hidden by a peer.
    get_operation(state).await
}

pub(crate) async fn forward_overview(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    let Some(reader) = selected_reader(state) else {
        return no_selection();
    };
    overview_native(request, state, reader).await
}

/// Both reader transports share the generation-bound native LPA workflow.
async fn overview_native(
    request: Request<Incoming>,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let refresh_requested = request.uri().query().is_some_and(|query| {
        query.split('&').any(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            key == "refresh" && (value == "1" || value.eq_ignore_ascii_case("true"))
        })
    });
    if !refresh_requested {
        return overview_once(
            state,
            reader,
            false,
            None,
            &mut crate::esim_refresh::Evidence::default(),
        )
        .await;
    }
    let expected = state
        .reader_gate
        .selected_reader_generation()
        .filter(|(name, _)| name == &reader);
    let owned = state.clone();
    let intent_id = crate::snapshot::new_instance_id();
    let worker_id = intent_id.clone();
    let progress = crate::esim_refresh::Progress::default();
    let worker_progress = progress.clone();
    // One accepted intent owns acquisition, both possible attempts, cleanup
    // and reconciliation even if the original HTTP waiter disappears.
    let mut job = tokio::spawn(async move {
        let current_state = owned.clone();
        let current_binding = expected.clone();
        crate::esim_refresh::run_with_id(
            &worker_id,
            "reader",
            move || {
                worker_progress.started();
                let state = owned.clone();
                let reader = reader.clone();
                let expected = expected.clone();
                async move {
                    let mut evidence = crate::esim_refresh::Evidence::unknown(false);
                    let response =
                        overview_once(&state, reader, true, expected.as_ref(), &mut evidence).await;
                    if expected.is_none() {
                        evidence.preflight_transport = false;
                    }
                    crate::esim_refresh::Attempt { response, evidence }
                }
            },
            move || {
                current_binding.as_ref().is_none_or(|binding| {
                    current_state
                        .reader_gate
                        .selected_reader_generation()
                        .as_ref()
                        == Some(binding)
                })
            },
            tokio::time::sleep,
        )
        .await
    });
    crate::esim_refresh::wait(
        &intent_id,
        "reader",
        &progress,
        &mut job,
        crate::esim_refresh::WAITER_TIMEOUT,
    )
    .await
}

async fn overview_once(
    state: &ServerState,
    reader: String,
    refresh_requested: bool,
    expected: Option<&(String, u64)>,
    evidence: &mut crate::esim_refresh::Evidence,
) -> Response<Full<Bytes>> {
    let lease = match NativeReaderSessionLease::acquire_for_card_operation(
        &state.go_socket_path,
        state.app_token.as_deref(),
        &reader,
        state.reader_gate.clone(),
        state.reader_pcsc.clone(),
    )
    .await
    {
        Ok(lease) => Arc::new(lease),
        Err(error)
            if matches!(
                error,
                proletarius_domain::BackendFailure::ReaderAdmissionRejected { .. }
            ) =>
        {
            return failure(
                StatusCode::CONFLICT,
                "esim_operation_busy",
                "eSIM 操作正在进行，当前显示内容暂不可刷新",
                true,
            );
        }
        Err(_) => return overview_failed(),
    };
    if selected_reader(state).as_deref() != Some(reader.as_str())
        || expected
            .is_some_and(|binding| binding != &(lease.reader.clone(), lease.reader_generation))
    {
        if lease.release().await.is_err() {
            return overview_failed();
        }
        return overview_failed();
    }

    if lease.esim_manager_available.is_none()
        || (lease.esim_manager_available == Some(false) && !lease_has_usim_identity(&lease))
    {
        let unavailable = lease.esim_manager_available == Some(false);
        return if lease.release().await.is_ok() && unavailable {
            no_selection()
        } else {
            overview_failed()
        };
    }
    let card_context = if reader.starts_with("BLE:") {
        None
    } else {
        match lease.pcsc_context(state.reader_pcsc.clone()) {
            Ok(context) => Some(context),
            Err(_) => {
                let _ = lease.release().await;
                return overview_failed();
            }
        }
    };
    let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
    let app_token = state.app_token.clone();
    let lease_owner = lease.clone();
    let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
    let operation = tokio::spawn(async move {
        let cancelled = Arc::new(AtomicBool::new(false));
        let watcher_cancelled = cancelled.clone();
        let mut lost_signal = heartbeat.lost_receiver();
        let watcher = tokio::spawn(async move {
            loop {
                if *lost_signal.borrow() {
                    watcher_cancelled.store(true, Ordering::Release);
                    return;
                }
                if lost_signal.changed().await.is_err() {
                    return;
                }
            }
        });
        let worker_cancelled = cancelled.clone();
        let scan = lease_owner
            .run_card_worker(download_pool(), move || {
                if let Some(context) = card_context {
                    let entered = AtomicBool::new(false);
                    return context
                        .with_transaction(|transport| {
                            entered.store(true, Ordering::Release);
                            let mut guarded = LeaseAwareTransport {
                                inner: BorrowedTransport(transport),
                                cancelled: worker_cancelled,
                            };
                            read_overview_on_transport(&mut guarded)
                                .map_err(|_| OverviewReadError::Operation)
                        })
                        .map_err(|error| {
                            overview_transport_failure(&error, entered.load(Ordering::Acquire))
                        })?;
                }
                let transport = crate::reader_ble_apdu::BleApduTransport::connect(
                    ble_socket.as_deref(),
                    app_token.as_deref(),
                    &reader,
                )
                .map_err(|_| OverviewReadError::BridgePreflight)?;
                let mut guarded = LeaseAwareTransport {
                    inner: transport,
                    cancelled: worker_cancelled,
                };
                read_overview_on_transport(&mut guarded).map_err(|_| OverviewReadError::Operation)
            })
            .await;
        let lease_lost = cancelled.load(Ordering::Acquire);
        heartbeat.stop().await;
        watcher.abort();
        let release = lease_owner.release().await;
        (scan, lease_lost, release)
    });
    let (scan, lease_lost, release) = match operation.await {
        Ok(result) => result,
        Err(_) => return overview_failed(),
    };
    if lease_lost || release.is_err() {
        return overview_failed();
    }
    *evidence = crate::esim_refresh::Evidence::unknown(true);
    if state.reader_gate.selected_reader_generation()
        != Some((lease.reader.clone(), lease.reader_generation))
    {
        return overview_failed();
    }
    let mut overview = match scan {
        Ok(Ok(Some(overview))) => overview,
        Ok(Ok(None)) if lease_has_usim_identity(&lease) => {
            evidence.succeeded = true;
            return physical_sim_overview(current_health_operation(state, &lease).await);
        }
        // An absent applet without positive identity, or a transport/security
        // error, is not enough evidence to publish the physical-SIM projection.
        Ok(Err(OverviewReadError::BridgePreflight | OverviewReadError::TransportPreflight)) => {
            *evidence = crate::esim_refresh::Evidence::preflight(true);
            return overview_failed();
        }
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return overview_failed(),
    };
    // Only a complete scan whose actual lease cleanup succeeded may clear an
    // interrupted operation. Partial profile reads remain display-only.
    if refresh_requested {
        let gate = state.reader_gate.clone();
        let binding = (lease.reader.clone(), lease.reader_generation);
        let journal_result = journal_pool()
            .run(move || {
                let result =
                    reconcile_reader_overview(service(), &gate, &binding.0, binding.1, &overview);
                (overview, result)
            })
            .await;
        match journal_result {
            Ok((observed, Ok(true))) => overview = observed,
            Ok((_, Err(_))) => return overview_failed(),
            _ => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "esim_operation_journal_failed",
                    "Profile 列表已刷新，但无法保存下载结果核对状态",
                    true,
                );
            }
        }
    }
    let operation = current_health_operation(state, &lease).await;
    evidence.succeeded = true;
    json_response(StatusCode::OK, &overview.response(operation))
}

fn reconcile_reader_overview(
    operations: &Service,
    gate: &crate::reader_reservation::Gate,
    reader: &str,
    generation: u64,
    overview: &ReaderOverview,
) -> Result<bool, &'static str> {
    gate.with_selected_generation(reader, generation, || {
        if overview.eids.is_empty()
            || overview.eids.len() != overview.profiles.len()
            || !overview
                .profiles
                .iter()
                .all(|group| group["read_state"] == "ready")
        {
            // Successful display of partial facts cannot make an interrupted
            // card mutation safe to retry. Preserve its durable unknown state.
            return true;
        }
        operations.reconcile_after_overview_refresh(reader, generation, &overview.card_identities)
    })
}

/// The module's complete native overview can settle only an operation whose
/// private card proof matches its freshly observed EID and AID. Cached or
/// partial projections never call this path.
pub(crate) async fn reconcile_module_overview(observation: &Value) -> bool {
    let Some(operations) = ESIM_SERVICE.get() else {
        return true;
    };
    let Some(eids) = observation["chip_info"]["eids"].as_array() else {
        return true;
    };
    let Some(groups) = observation["profiles"].as_array() else {
        return true;
    };
    if eids.is_empty() || eids.len() != groups.len() {
        return true;
    }
    let mut identities = Vec::with_capacity(eids.len());
    for (entry, group) in eids.iter().zip(groups) {
        if group["read_state"] != "ready" || entry["eid"] != group["eid"] {
            return true;
        }
        let (Some(eid_hex), Some(aid_hex), Some(group_aid)) = (
            entry["eid"].as_str(),
            entry["aid"].as_str(),
            group["aid_hex"].as_str(),
        ) else {
            return true;
        };
        if !aid_hex.eq_ignore_ascii_case(group_aid) {
            return true;
        }
        let (Some(eid), Some(aid)) = (parse_hex(eid_hex), parse_hex(aid_hex)) else {
            return true;
        };
        if format_eid(&eid).is_none() || aid.is_empty() || aid.len() > 32 {
            return true;
        }
        identities.push((eid, aid));
    }
    let observation = observation.clone();
    journal_pool()
        .run(move || reconcile_module_overview_at_commit_for(operations, &observation, &identities))
        .await
        .unwrap_or(false)
}

/// Invoke only while the module owner holds its generation/session fence.
/// `identities` must contain only AIDs whose own ListProfiles completed.
pub(crate) fn reconcile_module_overview_at_commit(
    observation: &Value,
    identities: &[(Vec<u8>, Vec<u8>)],
) -> bool {
    ESIM_SERVICE.get().is_none_or(|operations| {
        reconcile_module_overview_at_commit_for(operations, observation, identities)
    })
}

fn reconcile_module_overview_at_commit_for(
    operations: &Service,
    observation: &Value,
    identities: &[(Vec<u8>, Vec<u8>)],
) -> bool {
    let Some(eids) = observation["chip_info"]["eids"].as_array() else {
        return true;
    };
    let Some(groups) = observation["profiles"].as_array() else {
        return true;
    };
    if eids.is_empty()
        || eids.len() != groups.len()
        || !groups.iter().all(|group| group["read_state"] == "ready")
        || identities.is_empty()
    {
        return true;
    }
    operations.reconcile_after_overview_refresh("", 0, identities)
}

fn overview_failed() -> Response<Full<Bytes>> {
    failure(
        StatusCode::BAD_GATEWAY,
        "reader_esim_overview_failed",
        "读取读卡器 eSIM 卡片信息失败，请重新连接读卡器后重试",
        true,
    )
}

pub(crate) async fn rename_or_forward(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    if let Some(reader) = selected_reader(state) {
        return rename_profile(request, state, reader).await;
    }
    no_selection()
}

/// Route profile deletion for the selected USB/PCSC or BLE reader through Rust.
pub(crate) async fn delete_or_forward(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    if let Some(reader) = selected_reader(state) {
        return delete_profile(request, state, reader).await;
    }
    no_selection()
}

/// Preserve the synchronous DELETE response while moving the selected PC/SC
/// card operation, notification delivery, and NVRAM accounting into Rust.
pub(crate) async fn delete_profile(
    request: Request<Incoming>,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let body: DeleteProfileRequest = match decode_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::reader_request(
        state,
        &reader,
        &body.client_request_id,
        "delete",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let (iccid, aid) = match validate_delete_request(body) {
        Ok(request) => request,
        Err((code, message)) => {
            return failure(StatusCode::BAD_REQUEST, code, message, false);
        }
    };
    let state = state.clone();
    with_durable_intent(intent, move || async move {
        with_profile_mutation_gate(service(), || async move {
            // Native admission rejects conflicting reader work and holds the same
            // maintenance fence through ordinary IMS drain and this identity-free lease.
            let lease = match NativeReaderSessionLease::acquire_for_esim_mutation(
                &state.go_socket_path,
                state.app_token.as_deref(),
                &reader,
                state.reader_gate.clone(),
                state.reader_pcsc.clone(),
            )
            .await
            {
                Ok(lease) => Arc::new(lease),
                Err(_) => {
                    return failure(
                        StatusCode::CONFLICT,
                        "reader_session_lease_busy",
                        "读卡器仍由其它操作使用，未删除 Profile",
                        true,
                    );
                }
            };
            if selected_reader(&state).as_deref() != Some(reader.as_str()) {
                if lease.release().await.is_err() {
                    return failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "reader_session_lease_release_failed",
                        "读卡器选择已变化且租约释放未确认；Profile 未删除，请稍后刷新状态",
                        true,
                    );
                }
                return failure(
                    StatusCode::CONFLICT,
                    "reader_session_lease_busy",
                    "读卡器选择已变化，Profile 未删除；请刷新后重试",
                    true,
                );
            }
            let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
            let mut lease_lost = heartbeat.lost_receiver();
            let cancelled = Arc::new(AtomicBool::new(false));
            let watch_cancelled = cancelled.clone();
            let lease_watch = tokio::spawn(async move {
                if !*lease_lost.borrow() {
                    let _ = lease_lost.changed().await;
                }
                if *lease_lost.borrow() {
                    watch_cancelled.store(true, Ordering::Release);
                }
            });
            let card_context = if reader.starts_with("BLE:") {
                None
            } else {
                Some(lease.pcsc_context(state.reader_pcsc.clone()))
            };
            let ble_socket =
                std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
            let app_token = state.app_token.clone();
            let result = lease
                .run_card_worker(download_pool(), move || {
                    if reader.starts_with("BLE:") {
                        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                            ble_socket.as_deref(),
                            app_token.as_deref(),
                            &reader,
                        )
                        .map_err(|_| DeleteFailure::Operation)?;
                        delete_profile_on_transport(
                            &mut transport,
                            &iccid,
                            aid.as_deref(),
                            cancelled,
                        )
                    } else {
                        let context = card_context
                            .expect("non-BLE reader has a PC/SC context")
                            .map_err(|_| DeleteFailure::Operation)?;
                        delete_profile_on_reader(&context, &iccid, aid.as_deref(), cancelled)
                    }
                })
                .await;
            lease_watch.abort();
            heartbeat.stop().await;
            let release = lease.release().await;
            match result {
                Ok(Ok(Some(mut result))) if release.is_ok() => {
                    if refresh_written_reader(&state, &lease.reader, lease.reader_generation)
                        .await
                        .is_err()
                    {
                        let mut warning = result.warning.take().unwrap_or_default();
                        let mut code = result.warning_code.take().unwrap_or_default();
                        append_warning(
                            &mut warning,
                            &mut code,
                            "Profile 已删除，但读卡器状态刷新未确认；请手动刷新 Profile 列表",
                            "reader_profile_refresh_unconfirmed",
                        );
                        result.warning = Some(warning);
                        result.warning_code = Some(code);
                    }
                    json_response(StatusCode::OK, &result)
                }
                Ok(Ok(Some(_))) => failure(
                    StatusCode::BAD_GATEWAY,
                    "reader_session_lease_release_failed",
                    "Profile 删除已提交，但读卡器租约释放未确认；请刷新卡片状态",
                    true,
                ),
                Ok(Ok(None)) => failure(
                    StatusCode::NOT_FOUND,
                    "profile_not_found",
                    "未找到指定的 eSIM Profile",
                    false,
                ),
                Ok(Err(DeleteFailure::EuiccNotFound)) => failure(
                    StatusCode::NOT_FOUND,
                    "euicc_not_found",
                    "当前读卡器中未找到可管理的 eUICC",
                    false,
                ),
                Ok(Err(DeleteFailure::Operation)) => failure(
                    StatusCode::BAD_GATEWAY,
                    "delete_profile_failed",
                    "删除 Profile 失败，请刷新卡片状态后重试",
                    true,
                ),
                Err(_) => failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_operation_worker_failed",
                    "读卡器操作线程异常结束；请刷新 Profile 列表确认状态",
                    true,
                ),
            }
        })
        .await
    })
    .await
}

pub(crate) async fn switch_or_forward(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    if let Some(reader) = selected_reader(state) {
        return switch_profile(request, state, reader).await;
    }
    no_selection()
}

pub(crate) async fn list_notifications_or_forward(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    list_notifications_for_selection(request, state, selected_reader(state)).await
}

async fn list_notifications_for_selection(
    request: Request<Incoming>,
    state: &ServerState,
    selected: Option<String>,
) -> Response<Full<Bytes>> {
    let Some(reader) = selected else {
        return no_selection();
    };
    let aid = match notification_query_aid(request.uri().query().unwrap_or_default()) {
        Ok(aid) => aid,
        Err(()) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_notification",
                "通知 AID 格式无效",
                false,
            );
        }
    };
    if service().active() {
        return notification_list_response(Err(RetryNotificationFailure::Busy));
    }
    let socket = state.go_socket_path.clone();
    let token = state.app_token.clone().map(Zeroizing::new);
    let pcsc_owner = state.reader_pcsc.clone();
    let reader_gate = state.reader_gate.clone();
    let reader_discovery = state.reader_discovery.clone();
    // The supervisor owns acquisition, heartbeat and release even if the HTTP
    // future is dropped. Its worker checks cancellation before every APDU and
    // every remote retry; release cannot race an in-flight blocking operation.
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelNotificationRequest(cancelled.clone());
    match tokio::spawn(async move {
        if cancelled.load(Ordering::Acquire) {
            return notification_list_response(Err(RetryNotificationFailure::Cancelled));
        }
        let lease = match NativeReaderSessionLease::acquire_for_card_operation(
            &socket,
            token.as_deref().map(|s| s.as_str()),
            &reader,
            reader_gate.clone(),
            pcsc_owner.clone(),
        )
        .await
        {
            Ok(lease) => Arc::new(lease),
            Err(error) if is_reader_lease_conflict(&error) => {
                return notification_list_response(Err(RetryNotificationFailure::Busy));
            }
            Err(_) => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_session_lease_unavailable",
                    "无法取得读卡器操作租约；未读取运营商通知",
                    true,
                );
            }
        };
        let current_selection = reader_discovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot();
        let selected = reader_gate.selected_reader_generation();
        if selected.as_ref().map(|(_, generation)| *generation) != Some(lease.reader_generation)
            || selected_reader_from_snapshot(
                selected.as_ref().map(|(name, _)| name.as_str()),
                &current_selection,
                selected
                    .as_ref()
                    .is_some_and(|(name, _)| selected_ble_connection_bound(&reader_gate, name)),
            )
            .as_deref()
                != Some(reader.as_str())
        {
            return match lease.release().await {
                Ok(()) => notification_list_response(Err(RetryNotificationFailure::Busy)),
                Err(_) => failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "reader_session_lease_release_failed",
                    "读卡器选择已变化且租约释放未确认；未读取运营商通知，请刷新状态",
                    true,
                ),
            };
        }
        let candidates = notification_candidates(aid.as_deref(), &lease.notification_aids);
        let card_context = if reader.starts_with("BLE:") {
            None
        } else {
            Some(lease.pcsc_context(pcsc_owner))
        };
        let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
        let worker_reader = reader.clone();
        let explicit_aid = aid.is_some();
        let result = run_notification_lease(lease, cancelled, move |cancelled| {
            if worker_reader.starts_with("BLE:") {
                let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                    ble_socket.as_deref(),
                    token.as_deref().map(|value| value.as_str()),
                    &worker_reader,
                )
                .map_err(|error| RetryNotificationFailure::Failed(error.to_string()))?;
                list_notifications_on_transport(
                    &mut transport,
                    &candidates,
                    explicit_aid,
                    cancelled,
                )
            } else {
                let context = card_context
                    .expect("non-BLE reader has a PC/SC context")
                    .map_err(|error| RetryNotificationFailure::Failed(error.to_string()))?;
                list_notifications_on_reader(&context, &candidates, explicit_aid, cancelled)
            }
        })
        .await;
        notification_list_response(result)
    })
    .await
    {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_operation_worker_failed",
            "读卡器通知任务异常结束；请刷新后核对状态",
            true,
        ),
    }
}

struct CancelNotificationRequest(Arc<AtomicBool>);

impl Drop for CancelNotificationRequest {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

async fn run_notification_lease<T: Send + 'static>(
    lease: Arc<NativeReaderSessionLease>,
    cancelled: Arc<AtomicBool>,
    worker: impl FnOnce(Arc<AtomicBool>) -> Result<T, RetryNotificationFailure> + Send + 'static,
) -> Result<T, RetryNotificationFailure> {
    let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
    let mut lost = heartbeat.lost_receiver();
    let watch_cancelled = cancelled.clone();
    let watch = tokio::spawn(async move {
        while !*lost.borrow() {
            if lost.changed().await.is_err() {
                return;
            }
        }
        watch_cancelled.store(true, Ordering::Release);
    });
    let result = lease
        .run_card_worker(download_pool(), move || {
            if cancelled.load(Ordering::Acquire) {
                return Err(RetryNotificationFailure::Cancelled);
            }
            worker(cancelled)
        })
        .await;
    watch.abort();
    heartbeat.stop().await;
    let release = lease.release().await;
    match result {
        Ok(Ok(value)) if release.is_ok() => Ok(value),
        Ok(Ok(_)) => Err(RetryNotificationFailure::Uncertain(
            "读卡器租约释放未确认".into(),
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(RetryNotificationFailure::Uncertain(
            "读卡器通知任务异常结束".into(),
        )),
    }
}

fn notification_list_response(
    result: Result<Vec<notifications::NotificationItem>, RetryNotificationFailure>,
) -> Response<Full<Bytes>> {
    match result {
        Ok(items) => json_response(StatusCode::OK, &json!({"notifications": items})),
        Err(RetryNotificationFailure::Busy | RetryNotificationFailure::Cancelled) => failure(
            StatusCode::CONFLICT,
            "reader_operation_active",
            "读卡器操作正在进行或租约已失效，请刷新后重试",
            true,
        ),
        Err(RetryNotificationFailure::NotFound) => failure(
            StatusCode::NOT_FOUND,
            "notification_not_found",
            "读卡器中未找到指定通知",
            false,
        ),
        Err(RetryNotificationFailure::Failed(message)) => failure(
            StatusCode::BAD_GATEWAY,
            "notification_sync_failed",
            &message,
            true,
        ),
        Err(RetryNotificationFailure::Uncertain(message)) => failure(
            StatusCode::BAD_GATEWAY,
            "notification_sync_failed",
            &format!("通知状态待核对，请刷新通知列表：{message}"),
            true,
        ),
    }
}

fn notification_aid_bytes(value: &str) -> Option<Vec<u8>> {
    // SELECT uses a short APDU; reject oversized or malformed AIDs before I/O.
    if value.is_empty() || value.len() > 510 || value.len() % 2 != 0 {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some(hex_nibble(pair[0])? << 4 | hex_nibble(pair[1])?))
        .collect()
}

fn notification_query_aid(query: &str) -> Result<Option<Vec<u8>>, ()> {
    // Match Go Query().Get: URL decode, take the first value, then trim.
    // net/url omits malformed pairs (including unescaped semicolons), rather
    // than letting a malformed first pair hide a subsequent valid value.
    let value = query
        .split('&')
        .filter(|pair| {
            if pair.contains(';') {
                return false;
            }
            let bytes = pair.as_bytes();
            bytes.iter().enumerate().all(|(i, byte)| {
                *byte != b'%'
                    || bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                        && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
            })
        })
        .find_map(|pair| {
            url::form_urlencoded::parse(pair.as_bytes())
                .find(|(key, _)| key == "aid")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_default();
    if value.trim().is_empty() {
        return Ok(None);
    }
    notification_aid_bytes(value.trim()).map(Some).ok_or(())
}

fn notification_candidates(requested: Option<&[u8]>, discovered: &[String]) -> Vec<Vec<u8>> {
    if let Some(aid) = requested {
        return vec![aid.to_vec()];
    }
    let mut candidates = Vec::new();
    for aid in discovered
        .iter()
        .filter_map(|value| notification_aid_bytes(value))
        .chain(KNOWN_ISDR_APPLICATION_AIDS.iter().map(|aid| aid.to_vec()))
    {
        if !candidates.contains(&aid) {
            candidates.push(aid);
        }
    }
    candidates
}

struct NotificationCardAdapter<'a, T: CardTransport>(&'a mut Euicc<T>);

impl<T: CardTransport> notifications::NotificationCard for NotificationCardAdapter<'_, T>
where
    T::Error: fmt::Display,
{
    fn list(&mut self) -> Result<Vec<NotificationInfo>, String> {
        let mut output = vec![NotificationInfo::default(); MAX_NOTIFICATION_COUNT];
        let count = self
            .0
            .list_notifications_into(&mut output)
            .map_err(|e| e.to_string())?;
        output.truncate(count);
        Ok(output)
    }
    fn retrieve(
        &mut self,
        sequence: u64,
    ) -> Result<Vec<proletarius_euicc::PendingNotification>, String> {
        self.0
            .retrieve_notifications(sequence)
            .map_err(|e| e.to_string())
    }
    fn remove(&mut self, sequence: u64) -> Result<(), String> {
        notification_removal_action(self.0.remove_notification(sequence))
    }
}

fn list_notifications_on_reader(
    context: &NativeCardContext,
    candidates: &[Vec<u8>],
    explicit_aid: bool,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<notifications::NotificationItem>, RetryNotificationFailure> {
    context
        .with_transaction(|transport| {
            list_notifications_on_transport(transport, candidates, explicit_aid, cancelled)
        })
        .map_err(|error| RetryNotificationFailure::Failed(error.to_string()))?
}

fn list_notifications_on_transport<T: ApduTransport>(
    transport: &mut T,
    candidates: &[Vec<u8>],
    explicit_aid: bool,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<notifications::NotificationItem>, RetryNotificationFailure>
where
    T::Error: fmt::Display,
{
    collect_notification_aids(candidates, explicit_aid, &cancelled, |aid| {
        let guarded = LeaseAwareTransport {
            inner: BorrowedTransport(&mut *transport),
            cancelled: cancelled.clone(),
        };
        let network = HttpEs9Client::new()
            .map_err(|error| RetryNotificationFailure::Failed(error.to_string()))?;
        let client = open_lpa_client(guarded, aid, READER_MSS, network)
            .map_err(notification_open_failure)?;
        let (mut card, _network) = client.into_parts();
        let result = notifications::list_read_only(
            &mut NotificationCardAdapter(&mut card),
            &aid_hex(Some(aid)),
            &cancelled,
        );
        let close = card.transport_mut().close();
        // The next AID must not allocate another channel while cleanup of
        // this one is unknown, even if enumeration itself also failed.
        close.map_err(|error| {
            RetryNotificationFailure::Uncertain(format!("逻辑通道关闭未确认：{error}"))
        })?;
        result
    })
}

fn notification_open_failure<E: std::fmt::Display>(
    error: proletarius_reader::euicc_lpa::ClientOpenError<E>,
) -> RetryNotificationFailure {
    use proletarius_reader::{euicc_lpa::ClientOpenError, euicc_transport};
    if matches!(
        &error,
        ClientOpenError::Channel(euicc_transport::Error::Protocol(CcidError::CardStatus(_)))
    ) {
        // A known card rejection allocated no channel, or SELECT's own
        // cleanup succeeded. Lost/malformed allocation responses are unknown.
        RetryNotificationFailure::Failed(error.to_string())
    } else {
        RetryNotificationFailure::Uncertain(error.to_string())
    }
}

fn collect_notification_aids(
    candidates: &[Vec<u8>],
    explicit_aid: bool,
    cancelled: &AtomicBool,
    mut list: impl FnMut(
        &[u8],
    ) -> Result<Vec<notifications::NotificationItem>, RetryNotificationFailure>,
) -> Result<Vec<notifications::NotificationItem>, RetryNotificationFailure> {
    let mut output = Vec::new();
    let mut succeeded = false;
    let mut last_error = None;
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err(RetryNotificationFailure::Cancelled);
        }
        match list(aid) {
            Ok(items) => {
                succeeded = true;
                output.extend(items);
            }
            Err(
                error @ (RetryNotificationFailure::Cancelled
                | RetryNotificationFailure::Uncertain(_)
                | RetryNotificationFailure::Busy),
            ) => return Err(error),
            Err(error) if explicit_aid => return Err(error),
            Err(error) => last_error = Some(error),
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(RetryNotificationFailure::Cancelled);
    }
    if succeeded {
        Ok(if explicit_aid {
            output
        } else {
            notifications::merge_items(output)
        })
    } else {
        Err(last_error
            .unwrap_or_else(|| RetryNotificationFailure::Failed("未找到可用的 ISD-R 应用".into())))
    }
}

pub(crate) async fn retry_notification_or_forward(
    request: Request<Incoming>,
    state: &ServerState,
) -> Response<Full<Bytes>> {
    let Some(reader) = selected_reader(state) else {
        return no_selection();
    };
    let raw = match crate::http_server::collect_body(request.into_body(), 8 * 1024).await {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Ok(body) => body,
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request_body",
                "通知请求内容无效或过大",
                false,
            );
        }
    };
    let body: RetryNotificationRequest = match serde_json::from_slice(&raw) {
        Ok(body) => body,
        Err(_) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request_body",
                "通知请求内容无效",
                false,
            );
        }
    };
    if !crate::reader_mutation_intent::valid_client_request_id(&body.client_request_id) {
        return failure(
            StatusCode::BAD_REQUEST,
            "client_request_id_required",
            "通知同步必须提供有效的 client_request_id",
            false,
        );
    }
    let receipt_store = match crate::reader_notification_intent::preflight(
        state.multistandby_store.data_root(),
        &body.client_request_id,
        &raw,
    ) {
        Ok(crate::reader_notification_intent::Preflight::Replay(response)) => return response,
        Ok(crate::reader_notification_intent::Preflight::New(store)) => store,
        Err(StoreError::Configuration(code)) if code == "notification_request_id_conflict" => {
            return failure(
                StatusCode::CONFLICT,
                "notification_request_id_conflict",
                "client_request_id 已绑定到另一通知请求",
                false,
            );
        }
        Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "notification_receipt_store_unavailable",
                "通知持久回执不可用，未执行通知投递",
                false,
            );
        }
    };
    retry_notification(body, raw, receipt_store, state, reader).await
}

async fn retry_notification(
    body: RetryNotificationRequest,
    raw: Bytes,
    receipt_store: crate::reader_notification_intent::Store,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    if body.sequence_number <= 0 {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_notification",
            "通知序号必须大于零",
            false,
        );
    }
    let requested_aid = if body.aid.trim().is_empty() {
        None
    } else {
        match parse_hex(body.aid.trim()) {
            Some(aid) if !aid.is_empty() => Some(aid),
            _ => {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "invalid_notification",
                    "通知 AID 格式无效",
                    false,
                );
            }
        }
    };
    if service().active() {
        return failure(
            StatusCode::CONFLICT,
            "reader_operation_active",
            "读卡器操作正在进行，请稍后重试",
            true,
        );
    }

    // This short lease serializes notification APDUs with other reader work
    // but deliberately leaves an established VoWiFi tunnel running.
    let lease = match NativeReaderSessionLease::acquire_for_card_operation(
        &state.go_socket_path,
        state.app_token.as_deref(),
        &reader,
        state.reader_gate.clone(),
        state.reader_pcsc.clone(),
    )
    .await
    {
        Ok(lease) => Arc::new(lease),
        Err(error) if is_reader_lease_conflict(&error) => {
            return failure(
                StatusCode::CONFLICT,
                "reader_operation_active",
                "读卡器操作正在进行，请稍后重试",
                true,
            );
        }
        Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_session_lease_unavailable",
                "无法取得读卡器操作租约；未重试运营商通知",
                true,
            );
        }
    };
    if selected_reader(state).as_deref() != Some(reader.as_str()) {
        let released = lease.release().await.is_ok();
        return if released {
            failure(
                StatusCode::CONFLICT,
                "reader_operation_active",
                "读卡器选择已变化，未重试运营商通知；请刷新后重试",
                true,
            )
        } else {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_session_lease_release_failed",
                "读卡器选择已变化且租约释放未确认；未重试运营商通知，请刷新状态",
                true,
            )
        };
    }
    let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
    let mut lease_lost = heartbeat.lost_receiver();
    let cancelled = Arc::new(AtomicBool::new(false));
    let watch_cancelled = cancelled.clone();
    let lease_watch = tokio::spawn(async move {
        if !*lease_lost.borrow() {
            let _ = lease_lost.changed().await;
        }
        if *lease_lost.borrow() {
            watch_cancelled.store(true, Ordering::Release);
        }
    });
    let card_context = if reader.starts_with("BLE:") {
        None
    } else {
        Some(lease.pcsc_context(state.reader_pcsc.clone()))
    };
    let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
    let app_token = state.app_token.clone();
    let worker_reader = reader.clone();
    let sequence_number = body.sequence_number as u64;
    let client_request_id = body.client_request_id.clone();
    let request_fingerprint = crate::reader_notification_intent::request_fingerprint(&raw);
    let reader_generation = lease.reader_generation;
    let card_identity_hash =
        crate::reader_notification_intent::card_identity_hash(&lease.identity.iccid);
    let result = lease
        .run_card_worker(download_pool(), move || {
            if worker_reader.starts_with("BLE:") {
                let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                    ble_socket.as_deref(),
                    app_token.as_deref(),
                    &worker_reader,
                )
                .map_err(|error| RetryNotificationFailure::Failed(error.to_string()))?;
                retry_notification_on_transport(
                    &mut transport,
                    sequence_number,
                    requested_aid.as_deref(),
                    cancelled,
                    &receipt_store,
                    &client_request_id,
                    &request_fingerprint,
                    &worker_reader,
                    reader_generation,
                    &card_identity_hash,
                )
            } else {
                let context = card_context
                    .expect("non-BLE reader has a PC/SC context")
                    .map_err(|error| RetryNotificationFailure::Failed(error.to_string()))?;
                retry_notification_on_reader(
                    &context,
                    sequence_number,
                    requested_aid.as_deref(),
                    cancelled,
                    &receipt_store,
                    &client_request_id,
                    &request_fingerprint,
                    &worker_reader,
                    reader_generation,
                    &card_identity_hash,
                )
            }
        })
        .await;
    lease_watch.abort();
    heartbeat.stop().await;
    let release = lease.release().await;
    match result {
        Ok(Ok(())) if release.is_ok() => json_response(
            StatusCode::OK,
            &json!({
                "message":"运营商通知发送并确认完成",
                "sequence_number":body.sequence_number,
                "mutation_receipt":{
                    "client_request_id":body.client_request_id,
                    "outcome":"completed"
                }
            }),
        ),
        Ok(Ok(())) => failure(
            StatusCode::BAD_GATEWAY,
            "notification_sync_failed",
            "运营商通知已处理，但读卡器租约释放未确认；请刷新通知列表核对状态",
            true,
        ),
        Ok(Err(RetryNotificationFailure::NotFound)) => failure(
            StatusCode::NOT_FOUND,
            "notification_not_found",
            "读卡器中未找到指定的待发送通知",
            false,
        ),
        Ok(Err(RetryNotificationFailure::Busy)) => failure(
            StatusCode::CONFLICT,
            "reader_operation_active",
            "读卡器操作正在进行，请稍后重试",
            true,
        ),
        Ok(Err(RetryNotificationFailure::Cancelled)) => failure(
            StatusCode::CONFLICT,
            "reader_operation_active",
            "读卡器租约已失效，未确认通知操作；请刷新通知列表后再试",
            true,
        ),
        Ok(Err(RetryNotificationFailure::Failed(error))) => failure(
            StatusCode::BAD_GATEWAY,
            "notification_sync_failed",
            &format!("重试运营商通知失败：{error}"),
            true,
        ),
        Ok(Err(RetryNotificationFailure::Uncertain(error))) => failure(
            StatusCode::BAD_GATEWAY,
            "notification_sync_failed",
            &format!("通知投递结果或卡内确认状态未知，请刷新通知列表核对后再操作：{error}"),
            true,
        ),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_operation_worker_failed",
            "读卡器操作线程异常结束；请刷新通知列表确认状态",
            true,
        ),
    }
}

async fn switch_profile(
    request: Request<Incoming>,
    state: &ServerState,
    reader: String,
) -> Response<Full<Bytes>> {
    let body: SwitchProfileRequest = match decode_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::reader_request(
        state,
        &reader,
        &body.client_request_id,
        "switch",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let (iccid, aid) = match validate_switch_request(body) {
        Ok(request) => request,
        Err((code, message)) => return failure(StatusCode::BAD_REQUEST, code, message, false),
    };
    let owned_state = state.clone();
    match spawn_retained_operation(async move {
        crate::esim_mutation_intent::execute(intent, || {
            switch_profile_owned(owned_state, reader, iccid, aid)
        })
        .await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_switch_owner_failed",
            "切卡结果待确认，请刷新 Profile 列表和当前卡片身份后再操作",
            true,
        ),
    }
}

/// Owns the complete durable switch operation. The HTTP task only waits for
/// its response; cancellation cannot discard an in-flight APDU, finalization,
/// physical release, or terminal journal write.
async fn switch_profile_owned(
    owned_state: ServerState,
    reader: String,
    iccid: String,
    aid: Option<Vec<u8>>,
) -> Response<Full<Bytes>> {
    let state = &owned_state;
    if service().active() {
        return failure(
            StatusCode::CONFLICT,
            "esim_operation_busy",
            "已有 eSIM 操作正在进行，请等待完成后重试",
            true,
        );
    }
    let switch_permit = match service().begin_switch(&iccid) {
        Ok(permit) => permit,
        Err(BeginError::Busy) | Err(BeginError::ReconciliationRequired) => {
            return failure(
                StatusCode::CONFLICT,
                "esim_operation_busy",
                "已有 eSIM 操作正在进行或上次切卡状态尚待核对",
                true,
            );
        }
        Err(BeginError::Journal) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_operation_journal_failed",
                "无法安全记录切卡状态，尚未操作 eUICC",
                true,
            );
        }
        Err(BeginError::Unavailable) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_operation_unavailable",
                "无法创建 Profile 切卡操作",
                true,
            );
        }
    };
    let operation_id = switch_permit.id.clone();

    // The native coordinator atomically preserves the VoWiFi/N-wait
    // admission rule and owns maintenance until the mutation lease retires.
    let lease = match NativeReaderSessionLease::acquire_for_esim_mutation(
        &state.go_socket_path,
        state.app_token.as_deref(),
        &reader,
        state.reader_gate.clone(),
        state.reader_pcsc.clone(),
    )
    .await
    {
        Ok(lease) => Arc::new(lease),
        Err(_) => {
            let failed = failed(
                "reader_session_lease_busy",
                "读卡器正由其它操作使用，尚未提交 Profile 切换",
                "reader mutation lease acquisition failed before APDU admission",
            );
            service().finish_switch(&operation_id, Err(failed));
            return failure(
                StatusCode::CONFLICT,
                "reader_session_lease_busy",
                "读卡器仍由其它操作使用，请稍后重试",
                true,
            );
        }
    };
    if selected_reader(state).as_deref() != Some(reader.as_str()) {
        let released = lease.release().await.is_ok();
        let code = if released {
            "reader_session_lease_busy"
        } else {
            "reader_session_lease_release_failed"
        };
        let message = if released {
            "读卡器选择已变化，Profile 未切换；请刷新后重试"
        } else {
            "读卡器选择已变化且租约释放未确认；Profile 未切换，请刷新状态后再试"
        };
        service().finish_switch(
            &operation_id,
            Err(failed(
                code,
                message,
                "selected reader changed before switch APDU",
            )),
        );
        return failure(
            if released {
                StatusCode::CONFLICT
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            code,
            message,
            true,
        );
    }

    if !service().bind_reader_owner(&operation_id, &lease.reader, lease.reader_generation) {
        let released = lease.release().await.is_ok();
        service().finish_switch(
            &operation_id,
            Err(failed(
                "reader_operation_owner_missing",
                "无法确认读卡器操作归属，Profile 未切换",
                "native reader lease could not bind switch operation ownership",
            )),
        );
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            if released {
                "reader_operation_owner_missing"
            } else {
                "reader_session_lease_release_failed"
            },
            "无法确认读卡器操作归属，Profile 未切换；请刷新状态后重试",
            true,
        );
    }

    let heartbeat = NativeReaderSessionHeartbeat::start(lease.clone());
    let mut lease_lost = heartbeat.lost_receiver();
    let cancelled = Arc::new(AtomicBool::new(false));
    let watch_cancelled = cancelled.clone();
    let lease_watch = tokio::spawn(async move {
        if !*lease_lost.borrow() {
            let _ = lease_lost.changed().await;
        }
        if *lease_lost.borrow() {
            watch_cancelled.store(true, Ordering::Release);
        }
    });
    let card_context = if reader.starts_with("BLE:") {
        None
    } else {
        Some(lease.pcsc_context(state.reader_pcsc.clone()))
    };
    let ble_socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
    let app_token = state.app_token.clone();
    let worker_reader = reader.clone();
    let worker_iccid = iccid.clone();
    let worker_aid = aid.clone();
    let worker_cancelled = cancelled.clone();
    let worker_operation_id = operation_id.clone();
    let worker_result = lease
        .run_card_worker(download_pool(), move || {
            if worker_reader.starts_with("BLE:") {
                let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                    ble_socket.as_deref(),
                    app_token.as_deref(),
                    &worker_reader,
                )
                .map_err(SwitchFailure::Operation)?;
                switch_profile_on_transport_with_proof(
                    &mut transport,
                    &worker_iccid,
                    worker_aid.as_deref(),
                    worker_cancelled,
                    Some((service(), &worker_operation_id)),
                )
            } else {
                let context = card_context
                    .expect("non-BLE reader has a PC/SC context")
                    .map_err(|error| SwitchFailure::Operation(error.to_string()))?;
                switch_profile_on_reader_with_proof(
                    &context,
                    &worker_iccid,
                    worker_aid.as_deref(),
                    worker_cancelled,
                    Some((service(), &worker_operation_id)),
                )
            }
        })
        .await;
    lease_watch.abort();
    let switch_result = match worker_result {
        Ok(result) => result,
        Err(error) => {
            heartbeat.stop().await;
            let _ = lease.release().await;
            let operation_error = failed(
                "reader_operation_worker_failed",
                "切卡线程异常结束；目标 Profile 的状态需要刷新核对",
                error.to_string(),
            );
            service().finish_switch(&operation_id, Err(operation_error));
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_operation_worker_failed",
                "切卡线程异常结束；请刷新 Profile 列表和当前卡片身份",
                true,
            );
        }
    };
    if let Err(error) = switch_result {
        heartbeat.stop().await;
        let release = lease.release().await;
        let (status, code, message, retryable) = switch_failure_response(&error);
        let operation_error = failed(code, &message, error.diagnostic());
        service().finish_switch(&operation_id, Err(operation_error));
        if release.is_err() {
            return failure(
                StatusCode::BAD_GATEWAY,
                "reader_session_lease_release_failed",
                "切卡未能确认；读卡器租约释放也未确认，请刷新卡片状态后再操作",
                true,
            );
        }
        return failure(status, code, &message, retryable);
    }

    heartbeat.stop().await;
    if cancelled.load(Ordering::Acquire) {
        let _ = lease.release().await;
        let error = failed(
            "reader_session_lease_lost",
            "切卡结果待确认；读卡器租约失效，请刷新当前卡片身份后再操作",
            "reader lease was lost after profile enable submission",
        );
        service().finish_switch(&operation_id, Err(error));
        return failure(
            StatusCode::BAD_GATEWAY,
            "reader_session_lease_lost",
            "切卡结果待确认；请刷新当前卡片身份，不要立即重复切卡",
            true,
        );
    }

    let _ = service().update(
        &operation_id,
        "running",
        "reader_reconnect",
        "Profile 已受理，正在重建读卡器会话…",
        70,
    );
    // The Rust coordinator owns this finalization through reset, adapter rebuild,
    // notification sync and identity confirmation. A callback reply must also
    // have physical completion proof before the lease can be released.
    let finalization = lease
        .finalize_esim_switch(&iccid, &aid_hex(aid.as_deref()))
        .await;
    let release = lease.release().await;
    let finalized = match finalization {
        Ok(finalized) => finalized,
        Err(error) => {
            let operation_error = failed(
                "switch_reader_reconnect_failed",
                "Profile 切换已受理，但读卡器会话重建结果未知；请刷新卡片状态后再操作",
                error.to_string(),
            );
            service().finish_switch(&operation_id, Err(operation_error));
            return failure(
                StatusCode::BAD_GATEWAY,
                "switch_reader_reconnect_failed",
                "Profile 切换已受理，但读卡器会话重建未确认；请刷新卡片状态，不要立即重复切卡",
                true,
            );
        }
    };
    let mut warning = finalized.reader_refresh_warning.trim().to_owned();
    let mut warning_code = finalized.reader_refresh_warning_code.trim().to_owned();
    if !finalized.target_identity_confirmed && warning.is_empty() {
        warning = "Profile 切换已受理，但尚未确认读卡器呈现目标卡片身份；请刷新卡片状态，确认前不要重复切卡".into();
        warning_code = "switch_reader_identity_unconfirmed".into();
    }
    if release.is_err() {
        append_warning(
            &mut warning,
            &mut warning_code,
            "Profile 切换已受理，但读卡器租约释放未确认；请稍后刷新卡片状态",
            "reader_session_lease_release_failed",
        );
    }
    let mut result = DownloadResult::default();
    if !warning.is_empty() {
        result.warning = Some(warning.clone());
        result.warning_code = Some(warning_code.clone());
    }
    let _ = service().update(
        &operation_id,
        "running",
        "identity_refresh",
        "正在核对读卡器呈现的目标卡片身份…",
        90,
    );
    service().finish_switch(&operation_id, Ok(result));
    let operation = service().current().unwrap_or_default();
    let response = json_response(
        StatusCode::OK,
        &SwitchProfileResponse {
            switch_accepted: true,
            phase: "apdu_switching".into(),
            target_iccid: iccid,
            recovery_pending: false,
            module_reboot_requested: false,
            module_reboot_response: String::new(),
            module_reboot_warning: String::new(),
            reconnect_wait_seconds: 0,
            reader_refresh_warning: warning,
            reader_refresh_warning_code: warning_code,
            reader_info: finalized.reader_info,
            operation,
        },
    );
    drop(switch_permit);
    response
}

fn aid_hex(aid: Option<&[u8]>) -> String {
    aid.map(|aid| aid.iter().map(|byte| format!("{byte:02X}")).collect())
        .unwrap_or_default()
}

fn append_warning(warning: &mut String, code: &mut String, message: &str, warning_code: &str) {
    if !warning.is_empty() {
        warning.push('；');
        code.push(',');
    }
    warning.push_str(message);
    code.push_str(warning_code);
}

fn validate_switch_request(
    mut request: SwitchProfileRequest,
) -> Result<(String, Option<Vec<u8>>), (&'static str, &'static str)> {
    let iccid = request.iccid.trim().to_owned();
    if iccid.is_empty() {
        request.iccid.zeroize();
        request.aid.zeroize();
        return Err(("invalid_iccid", "请选择要启用的 Profile"));
    }
    if !valid_iccid(&iccid) {
        request.iccid.zeroize();
        request.aid.zeroize();
        return Err(("invalid_iccid", "ICCID 格式无效"));
    }
    let aid = if request.aid.trim().is_empty() {
        None
    } else {
        match parse_hex(request.aid.trim()) {
            Some(aid) => Some(aid),
            None => {
                request.iccid.zeroize();
                request.aid.zeroize();
                return Err(("invalid_aid_hex", "AID 格式无效"));
            }
        }
    };
    request.iccid.zeroize();
    request.aid.zeroize();
    Ok((iccid, aid))
}

#[derive(Debug)]
enum SwitchFailure {
    EuiccNotFound(String),
    TargetNotFound,
    CardBusy(String),
    CardResult(ProfileOperationResult),
    Operation(String),
}

impl SwitchFailure {
    fn diagnostic(&self) -> String {
        match self {
            Self::EuiccNotFound(error) | Self::CardBusy(error) | Self::Operation(error) => {
                error.clone()
            }
            Self::CardResult(result) => {
                format!(
                    "enableProfile / {}, result code {}",
                    switch_result_name(*result),
                    result.code()
                )
            }
            Self::TargetNotFound => "target ICCID is absent from the profile list".into(),
        }
    }
}

fn switch_result_name(result: ProfileOperationResult) -> String {
    match result {
        // ES10c uses operation-specific names for this shared numeric result.
        ProfileOperationResult::ProfileInWrongState => "profileNotInDisabledState".into(),
        result => result.to_string(),
    }
}

fn switch_failure_response(error: &SwitchFailure) -> (StatusCode, &'static str, String, bool) {
    let (status, code, message, retryable) = match error {
        SwitchFailure::EuiccNotFound(_) | SwitchFailure::Operation(_) => (
            StatusCode::BAD_GATEWAY,
            "profile_switch_failed",
            "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
            true,
        ),
        SwitchFailure::TargetNotFound
        | SwitchFailure::CardResult(ProfileOperationResult::IccidOrAidNotFound) => (
            StatusCode::NOT_FOUND,
            "profile_switch_target_missing",
            "卡片未找到目标 Profile，请刷新列表后重新选择",
            false,
        ),
        SwitchFailure::CardBusy(_) | SwitchFailure::CardResult(ProfileOperationResult::CatBusy) => {
            (
                StatusCode::CONFLICT,
                "profile_switch_card_busy",
                "卡片仍在处理上一项操作，请等待完成后刷新状态",
                true,
            )
        }
        SwitchFailure::CardResult(
            ProfileOperationResult::DisallowedByPolicy
            | ProfileOperationResult::DisallowedByEnterpriseRule
            | ProfileOperationResult::DisallowedForRpm
            | ProfileOperationResult::NoEsimPortAvailable,
        ) => (
            StatusCode::BAD_GATEWAY,
            "profile_switch_policy_denied",
            "卡片策略不允许启用此 Profile，请核对号卡服务商的限制",
            false,
        ),
        SwitchFailure::CardResult(ProfileOperationResult::ProfileInWrongState) => (
            StatusCode::BAD_GATEWAY,
            "profile_switch_state_unconfirmed",
            "目标 Profile 不处于可启用状态，且尚未确认激活结果；请刷新卡片身份，不要重复切卡",
            true,
        ),
        SwitchFailure::CardResult(ProfileOperationResult::Undefined) => (
            StatusCode::BAD_GATEWAY,
            "profile_switch_card_rejected",
            "卡片拒绝启用 Profile，但未提供具体原因；请刷新卡片状态并核对兼容性，不要连续重复切卡",
            true,
        ),
        SwitchFailure::CardResult(ProfileOperationResult::WrongProfileReenabling) => (
            StatusCode::BAD_GATEWAY,
            "profile_switch_rejected",
            "卡片拒绝重新启用此 Profile，请刷新卡片状态并核对号卡限制",
            true,
        ),
        SwitchFailure::CardResult(_) => (
            StatusCode::BAD_GATEWAY,
            "profile_switch_rejected",
            "卡片返回未识别的启用结果，请保留错误码用于诊断",
            true,
        ),
    };
    // Only the typed card result is public. Wrapped transport errors may
    // contain subscriber identities or APDUs and remain out of this message.
    let message = match error {
        SwitchFailure::CardResult(result) => {
            format!(
                "{message}（enableProfile / {}，结果码 {}）",
                switch_result_name(*result),
                result.code()
            )
        }
        _ => message.to_owned(),
    };
    (status, code, message, retryable)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitchAidResult {
    TargetMissing,
    Accepted,
}

#[derive(Debug, Eq, PartialEq)]
enum RetryNotificationFailure {
    NotFound,
    Busy,
    Cancelled,
    Failed(String),
    Uncertain(String),
}

fn retry_notification_on_reader(
    context: &NativeCardContext,
    sequence_number: u64,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
    receipt_store: &crate::reader_notification_intent::Store,
    client_request_id: &str,
    request_fingerprint: &str,
    reader: &str,
    reader_generation: u64,
    card_identity_hash: &str,
) -> Result<(), RetryNotificationFailure> {
    let operation = context.with_transaction(|transport| {
        retry_notification_on_transport(
            transport,
            sequence_number,
            requested_aid,
            cancelled,
            receipt_store,
            client_request_id,
            request_fingerprint,
            reader,
            reader_generation,
            card_identity_hash,
        )
    });
    match operation {
        Ok(result) => result,
        Err(error) if matches!(error.code, "reader_pcsc_busy" | "reader_busy" | "card_busy") => {
            Err(RetryNotificationFailure::Busy)
        }
        Err(error) => Err(RetryNotificationFailure::Failed(error.to_string())),
    }
}

fn retry_notification_on_transport<T: ApduTransport>(
    transport: &mut T,
    sequence_number: u64,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
    receipt_store: &crate::reader_notification_intent::Store,
    client_request_id: &str,
    request_fingerprint: &str,
    reader: &str,
    reader_generation: u64,
    card_identity_hash: &str,
) -> Result<(), RetryNotificationFailure>
where
    T::Error: fmt::Display,
{
    let candidates: Vec<&[u8]> =
        requested_aid.map_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec(), |aid| vec![aid]);
    let mut euicc_found = false;
    let mut last_error = None;
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err(RetryNotificationFailure::Cancelled);
        }
        let guarded = LeaseAwareTransport {
            inner: BorrowedTransport(&mut *transport),
            cancelled: cancelled.clone(),
        };
        let network = match HttpEs9Client::new() {
            Ok(network) => network,
            Err(error) => {
                return Err(RetryNotificationFailure::Failed(error.to_string()));
            }
        };
        let client = match open_lpa_client(guarded, aid, READER_MSS, network) {
            Ok(client) => client,
            Err(error) => {
                let error = notification_open_failure(error);
                if matches!(&error, RetryNotificationFailure::Uncertain(_)) {
                    return Err(error);
                }
                if let RetryNotificationFailure::Failed(message) = error {
                    last_error = Some(message);
                }
                continue;
            }
        };
        euicc_found = true;
        let (mut card, network) = client.into_parts();
        // Always close explicitly after any result, including empty cursors,
        // storage failure and network ambiguity. Drop cannot report cleanup.
        let outcome = (|| -> Result<bool, RetryNotificationFailure> {
            let pending = match card.retrieve_notifications(sequence_number) {
                Ok(pending) if !pending.is_empty() => pending,
                Ok(_) => return Ok(false),
                Err(error) => {
                    last_error = Some(error.to_string());
                    return Ok(false);
                }
            };
            let binding = crate::reader_notification_intent::Binding {
                reader: reader.to_owned(),
                reader_generation,
                card_identity_hash: card_identity_hash.to_owned(),
                aid_hex: aid_hex(Some(aid)),
                sequence_number,
            };
            match receipt_store.claim(client_request_id, request_fingerprint, binding) {
                Ok(None) => {}
                Ok(Some(_)) => {
                    return Err(RetryNotificationFailure::Uncertain(
                        "通知请求已有持久回执，未再次投递".into(),
                    ));
                }
                Err(error) => {
                    return Err(RetryNotificationFailure::Failed(format!(
                        "notification receipt unavailable: {error}"
                    )));
                }
            }
            let fingerprints = receipt_store
                .set_payloads(client_request_id, &pending)
                .map_err(|error| {
                    RetryNotificationFailure::Uncertain(format!(
                        "notification payload receipt unavailable: {error}"
                    ))
                })?;
            for (notification, fingerprint) in pending.iter().zip(&fingerprints) {
                notifications::ensure_active(&cancelled, true)?;
                let dispatch = receipt_store
                    .claim_payload(client_request_id, fingerprint)
                    .map_err(|error| {
                        RetryNotificationFailure::Uncertain(format!(
                            "notification payload receipt unavailable: {error}"
                        ))
                    })?;
                if !dispatch {
                    continue;
                }
                let delivered = network
                    .handle_notification(
                        &notification.address,
                        &notification.encoded,
                        READER_NOTIFICATION_TIMEOUT,
                    )
                    .map_err(|error| error.to_string());
                receipt_store
                    .finish_payload(client_request_id, fingerprint, delivered.is_ok())
                    .map_err(|error| {
                        RetryNotificationFailure::Uncertain(format!(
                            "notification payload completion unavailable: {error}"
                        ))
                    })?;
                if let Err(error) = delivered {
                    return Err(RetryNotificationFailure::Uncertain(error));
                }
            }
            notifications::ensure_active(&cancelled, true)?;
            if receipt_store
                .claim_confirm(client_request_id)
                .map_err(|error| {
                    RetryNotificationFailure::Uncertain(format!(
                        "notification confirmation receipt unavailable: {error}"
                    ))
                })?
            {
                let confirmed =
                    notification_removal_action(card.remove_notification(sequence_number));
                receipt_store
                    .finish_confirm(client_request_id, confirmed.is_ok())
                    .map_err(|error| {
                        RetryNotificationFailure::Uncertain(format!(
                            "notification confirmation completion unavailable: {error}"
                        ))
                    })?;
                if let Err(error) = confirmed {
                    return Err(RetryNotificationFailure::Uncertain(error));
                }
            }
            Ok(true)
        })();
        card.transport_mut().close().map_err(|error| {
            RetryNotificationFailure::Uncertain(format!("逻辑通道关闭未确认：{error}"))
        })?;
        if !outcome? {
            continue;
        }
        receipt_store.complete(client_request_id).map_err(|error| {
            RetryNotificationFailure::Uncertain(format!(
                "notification terminal receipt unavailable: {error}"
            ))
        })?;
        return Ok(());
    }
    if euicc_found {
        Err(RetryNotificationFailure::NotFound)
    } else {
        Err(RetryNotificationFailure::Failed(last_error.unwrap_or_else(
            || "no usable ISD-R application was found".into(),
        )))
    }
}

fn is_reader_lease_conflict(error: &proletarius_domain::BackendFailure) -> bool {
    if matches!(
        error,
        proletarius_domain::BackendFailure::ReaderAdmissionRejected { .. }
    ) {
        return true;
    }
    matches!(
        error,
        proletarius_domain::BackendFailure::GoBackendUnavailable(message)
            if message.contains("returned 409") || message.contains("reader native lease: card_busy") || message.contains("reader native lease: nwait_owns_reader")
    )
}

fn notification_removal_action<E: fmt::Display>(
    result: Result<(), EuiccError<E>>,
) -> Result<(), String> {
    match result {
        Ok(())
        | Err(EuiccError::NotificationOperation {
            operation: proletarius_euicc::NotificationOperation::Remove,
            result: proletarius_euicc::NotificationResult::NotFound,
        }) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn switch_profile_on_reader_with_proof(
    context: &NativeCardContext,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
    proof_owner: Option<(&Service, &str)>,
) -> Result<(), SwitchFailure> {
    context
        .with_transaction(|transport| {
            switch_profile_on_transport_with_proof(
                transport,
                iccid,
                requested_aid,
                cancelled,
                proof_owner,
            )
        })
        .map_err(|error| SwitchFailure::Operation(error.to_string()))?
}

fn switch_profile_on_transport<T: ApduTransport>(
    transport: &mut T,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), SwitchFailure>
where
    T::Error: fmt::Display,
{
    switch_profile_on_transport_with_proof(transport, iccid, requested_aid, cancelled, None)
}

fn switch_profile_on_transport_with_proof<T: ApduTransport>(
    transport: &mut T,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
    proof_owner: Option<(&Service, &str)>,
) -> Result<(), SwitchFailure>
where
    T::Error: fmt::Display,
{
    let candidates: Vec<&[u8]> =
        requested_aid.map_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec(), |aid| vec![aid]);
    let mut any_euicc = false;
    let mut last_error = None;
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err(SwitchFailure::Operation(
                "reader lease was lost before APDU submission".into(),
            ));
        }
        match switch_profile_on_aid(transport, aid, iccid, cancelled.clone(), proof_owner) {
            Ok(SwitchAidResult::Accepted) => return Ok(()),
            Ok(SwitchAidResult::TargetMissing) => any_euicc = true,
            Err(SwitchFailure::EuiccNotFound(error)) => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    if any_euicc {
        Err(SwitchFailure::TargetNotFound)
    } else {
        Err(SwitchFailure::EuiccNotFound(last_error.unwrap_or_else(
            || "no candidate ISD-R application was usable".into(),
        )))
    }
}

fn switch_profile_on_aid<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    iccid: &str,
    cancelled: Arc<AtomicBool>,
    proof_owner: Option<(&Service, &str)>,
) -> Result<SwitchAidResult, SwitchFailure>
where
    T::Error: fmt::Display,
{
    let guarded = LeaseAwareTransport {
        inner: BorrowedTransport(&mut *transport),
        cancelled: cancelled.clone(),
    };
    // Switching is a local ES10c operation. Use the owned logical-channel
    // adapter without constructing an ES9+ network client.
    let channel =
        proletarius_reader::euicc_transport::open_euicc_channel(guarded, aid).map_err(|error| {
            if missing_isdr_aid(&error) {
                SwitchFailure::EuiccNotFound(error.to_string())
            } else {
                SwitchFailure::Operation(error.to_string())
            }
        })?;
    let channel_number = channel.channel();
    let mut card = Euicc::new(channel, channel_number, READER_MSS)
        .map_err(|error| SwitchFailure::EuiccNotFound(error.to_string()))?;
    let outcome = (|| {
        let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
        let count = card
            .list_profiles_into(&mut profiles)
            .map_err(|error| SwitchFailure::EuiccNotFound(error.to_string()))?;
        let Some(target) = profiles[..count]
            .iter()
            .find(|profile| profile.iccid_string().as_deref() == Ok(iccid))
        else {
            return Ok(SwitchAidResult::TargetMissing);
        };
        if target.state == ProfileState::Enabled {
            return Ok(SwitchAidResult::Accepted);
        }
        if let Some((operations, operation_id)) = proof_owner {
            let mut eid = [0u8; proletarius_euicc::EID_LENGTH_BYTES];
            let length = card.get_eid(&mut eid).map_err(|_| {
                SwitchFailure::Operation("switch card identity could not be confirmed".into())
            })?;
            let armed = operations.arm_card_proof(operation_id, &eid[..length], aid);
            eid.zeroize();
            armed.map_err(|_| {
                SwitchFailure::Operation("switch recovery proof could not be persisted".into())
            })?;
        }
        let encoded_iccid = encode_iccid(iccid).map_err(|error| {
            SwitchFailure::Operation(format!("target ICCID encoding failed: {error}"))
        })?;
        enable_profile_with_retry(
            &mut card,
            ProfileIdentifier::Iccid(&encoded_iccid),
            &cancelled,
        )?;
        Ok(SwitchAidResult::Accepted)
    })();
    card.transport_mut()
        .close()
        .map_err(|error| SwitchFailure::Operation(format!("逻辑通道关闭未确认：{error}")))?;
    outcome
}

fn enable_profile_with_retry<T: CardTransport>(
    card: &mut Euicc<T>,
    identifier: ProfileIdentifier<'_>,
    cancelled: &AtomicBool,
) -> Result<(), SwitchFailure>
where
    T::Error: fmt::Display,
{
    enable_profile_with_retry_and_wait(card, identifier, cancelled, || {
        std::thread::sleep(Duration::from_millis(800));
    })
}

fn enable_profile_with_retry_and_wait<T: CardTransport>(
    card: &mut Euicc<T>,
    identifier: ProfileIdentifier<'_>,
    cancelled: &AtomicBool,
    mut wait_before_retry: impl FnMut(),
) -> Result<(), SwitchFailure>
where
    T::Error: fmt::Display,
{
    const MAX_CAT_BUSY_RETRIES: usize = 3;
    for attempt in 0..=MAX_CAT_BUSY_RETRIES {
        if cancelled.load(Ordering::Acquire) {
            return Err(SwitchFailure::Operation(
                "reader lease was lost during profile switch".into(),
            ));
        }
        match card.enable_profile_command_strict(identifier, false) {
            Ok(()) => return Ok(()),
            Err(EuiccError::ProfileOperation {
                result: ProfileOperationResult::CatBusy,
                ..
            }) if attempt < MAX_CAT_BUSY_RETRIES => {
                wait_before_retry();
            }
            Err(error) => return Err(classify_switch_apdu_error(error)),
        }
    }
    Err(SwitchFailure::CardBusy(
        "CAT_BUSY retry limit reached".into(),
    ))
}

fn classify_switch_apdu_error<E: fmt::Display>(error: EuiccError<E>) -> SwitchFailure {
    match error {
        EuiccError::ProfileOperation {
            operation: ProfileOperation::Enable,
            result,
        } => SwitchFailure::CardResult(result),
        error => SwitchFailure::Operation(error.to_string()),
    }
}

fn validate_delete_request(
    mut request: DeleteProfileRequest,
) -> Result<(String, Option<Vec<u8>>), (&'static str, &'static str)> {
    let iccid = request.iccid.trim().to_owned();
    if !valid_iccid(&iccid) {
        request.iccid.zeroize();
        request.aid.zeroize();
        return Err(("invalid_iccid", "ICCID 格式无效"));
    }
    let aid = if request.aid.trim().is_empty() {
        None
    } else {
        match parse_hex(request.aid.trim()) {
            Some(aid) => Some(aid),
            None => {
                request.iccid.zeroize();
                request.aid.zeroize();
                return Err(("invalid_aid_hex", "AID 格式无效"));
            }
        }
    };
    request.iccid.zeroize();
    request.aid.zeroize();
    Ok((iccid, aid))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeleteFailure {
    EuiccNotFound,
    Operation,
}

fn delete_profile_on_reader(
    context: &NativeCardContext,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<DeleteProfileResult>, DeleteFailure> {
    context
        .with_transaction(|transport| {
            delete_profile_on_transport(transport, iccid, requested_aid, cancelled)
        })
        .map_err(|_| DeleteFailure::Operation)?
}

fn delete_profile_on_transport<T: ApduTransport>(
    transport: &mut T,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<DeleteProfileResult>, DeleteFailure>
where
    T::Error: fmt::Display,
{
    let candidates: Vec<&[u8]> =
        requested_aid.map_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec(), |aid| vec![aid]);
    let mut euicc_found = false;
    let mut last_error = None;
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err(DeleteFailure::Operation);
        }
        match delete_profile_on_aid(transport, aid, iccid, cancelled.clone()) {
            Ok(Some(result)) => return Ok(Some(result)),
            Ok(None) => euicc_found = true,
            Err(DeleteFailure::EuiccNotFound) => last_error = Some(()),
            Err(DeleteFailure::Operation) => return Err(DeleteFailure::Operation),
        }
    }
    if euicc_found {
        Ok(None)
    } else if last_error.is_some() {
        Err(DeleteFailure::EuiccNotFound)
    } else {
        Err(DeleteFailure::Operation)
    }
}

fn delete_profile_on_aid<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    iccid: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<DeleteProfileResult>, DeleteFailure>
where
    T::Error: fmt::Display,
{
    let guarded = LeaseAwareTransport {
        inner: BorrowedTransport(&mut *transport),
        cancelled: cancelled.clone(),
    };
    let network = HttpEs9Client::new().map_err(|_| DeleteFailure::Operation)?;
    let client =
        open_lpa_client(guarded, aid, READER_MSS, network).map_err(|error| match error {
            proletarius_reader::euicc_lpa::ClientOpenError::Channel(error)
                if missing_isdr_aid(&error) =>
            {
                DeleteFailure::EuiccNotFound
            }
            _ => DeleteFailure::Operation,
        })?;
    let (mut card, network) = client.into_parts();
    let outcome = delete_profile_on_euicc(&mut card, &network, iccid, &cancelled)
        .map_err(|_| DeleteFailure::Operation);
    card.transport_mut()
        .close()
        .map_err(|_| DeleteFailure::Operation)?;
    outcome
}

/// Run the shared SGP.22 delete, notification, and NVRAM sequence against an
/// already selected eUICC channel. The module USB owner uses this same core
/// after opening its AT logical channel, so both reader and module paths keep
/// identical operation semantics without transferring transport ownership.
pub(crate) fn delete_profile_on_euicc<T, N>(
    card: &mut Euicc<T>,
    network: &N,
    iccid: &str,
    cancelled: &AtomicBool,
) -> Result<Option<DeleteProfileResult>, ()>
where
    T: CardTransport,
    T::Error: fmt::Display,
    N: Es9Transport,
{
    // Record the notification cursor before mutation, matching the Go path;
    // older queued delete notices must not be mistaken for this operation.
    let mut before_notifications = vec![NotificationInfo::default(); MAX_NOTIFICATION_COUNT];
    let last_sequence = card
        .list_notifications_into(&mut before_notifications)
        .map(|count| max_notification_sequence(&before_notifications[..count]))
        .unwrap_or(0);
    let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
    let count = card.list_profiles_into(&mut profiles).map_err(|_| ())?;
    if !profiles[..count]
        .iter()
        .any(|profile| profile.iccid_string().as_deref() == Ok(iccid))
    {
        return Ok(None);
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(());
    }
    let encoded_iccid = encode_iccid(iccid).map_err(|_| ())?;
    let before_space = card
        .get_euicc_info_preferred()
        .ok()
        .and_then(|info| info.free_nvram_bytes)
        .filter(|bytes| *bytes > 0);
    let remaining = card
        .apply_profile_operation(
            ProfileOperation::Delete,
            ProfileIdentifier::Iccid(&encoded_iccid),
            false,
            &mut profiles,
            |_| std::thread::sleep(Duration::from_millis(300)),
        )
        .map_err(|_| ())?;
    if profiles[..remaining]
        .iter()
        .any(|profile| profile.iccid_string().as_deref() == Ok(iccid))
    {
        return Err(());
    }

    let warning_code =
        deliver_delete_notification(card, network, last_sequence, &encoded_iccid, cancelled);
    let after_space = read_free_nvram_with_retry(card);
    Ok(Some(DeleteProfileResult {
        warning: warning_code
            .as_ref()
            .map(|_| "Profile 已删除，但删除通知发送未完全确认".into()),
        warning_code,
        space_delta: deletion_space_delta(before_space, after_space),
    }))
}

fn max_notification_sequence(notifications: &[NotificationInfo]) -> u64 {
    notifications
        .iter()
        .map(|notification| notification.sequence_number)
        .max()
        .unwrap_or(0)
}

fn find_delete_notification(
    notifications: &[NotificationInfo],
    last_sequence: u64,
    iccid: &[u8],
) -> Option<u64> {
    notifications
        .iter()
        .find(|notification| {
            notification.sequence_number > last_sequence
                && notification.event == 3
                && notification.iccid == iccid
        })
        .map(|notification| notification.sequence_number)
}

trait DeleteCard: RspCard {
    fn list_all_notifications(&mut self, output: &mut [NotificationInfo]) -> Result<usize, String>;
}

impl<T> DeleteCard for Euicc<T>
where
    T: CardTransport,
    T::Error: fmt::Display,
{
    fn list_all_notifications(&mut self, output: &mut [NotificationInfo]) -> Result<usize, String> {
        self.list_notifications_into(output)
            .map_err(|error| error.to_string())
    }
}

fn deliver_delete_notification<C, N>(
    card: &mut C,
    network: &N,
    last_sequence: u64,
    iccid: &[u8],
    cancelled: &AtomicBool,
) -> Option<String>
where
    C: DeleteCard,
    N: Es9Transport,
{
    let mut sequence = None;
    for attempt in 0..3 {
        if cancelled.load(Ordering::Acquire) {
            return Some("delete_notification_retrieve_failed".into());
        }
        let mut notifications = vec![NotificationInfo::default(); MAX_NOTIFICATION_COUNT];
        if let Ok(count) = card.list_all_notifications(&mut notifications) {
            sequence = find_delete_notification(&notifications[..count], last_sequence, iccid);
            if sequence.is_some() {
                break;
            }
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    let Some(sequence) = sequence else {
        return Some("delete_notification_not_observed".into());
    };

    let mut pending = None;
    for attempt in 0..3 {
        if cancelled.load(Ordering::Acquire) {
            return Some("delete_notification_retrieve_failed".into());
        }
        match card.retrieve_notification(sequence) {
            Ok(Some(notification)) => {
                pending = Some(notification);
                break;
            }
            Ok(None) | Err(_) if attempt < 2 => std::thread::sleep(Duration::from_millis(300)),
            Ok(None) | Err(_) => {}
        }
    }
    let Some(notification) = pending else {
        return Some("delete_notification_retrieve_failed".into());
    };

    let mut delivered = false;
    for attempt in 0..3 {
        if cancelled.load(Ordering::Acquire) {
            return Some("delete_notification_handle_failed".into());
        }
        if network
            .handle_notification(
                &notification.address,
                &notification.encoded,
                NOTIFICATION_TIMEOUT,
            )
            .is_ok()
        {
            delivered = true;
            break;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    if !delivered {
        return Some("delete_notification_handle_failed".into());
    }

    for attempt in 0..3 {
        if cancelled.load(Ordering::Acquire) {
            return Some("delete_notification_handle_failed".into());
        }
        if card.remove_notification(sequence).is_ok() {
            return None;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    Some("delete_notification_handle_failed".into())
}

fn read_free_nvram_with_retry<T: CardTransport>(card: &mut Euicc<T>) -> Option<u64> {
    for attempt in 0..3 {
        if let Ok(info) = card.get_euicc_info_preferred()
            && let Some(bytes) = info.free_nvram_bytes.filter(|bytes| *bytes > 0)
        {
            return Some(bytes);
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    None
}

fn deletion_space_delta(before: Option<u64>, after: Option<u64>) -> Option<SpaceDelta> {
    let (Some(before), Some(after)) = (before, after) else {
        return None;
    };
    if after <= before {
        return None;
    }
    Some(SpaceDelta {
        direction: "released".into(),
        bytes: i64::try_from(after - before).ok()?,
    })
}

async fn decode_body<T: for<'de> Deserialize<'de>>(
    request: Request<Incoming>,
) -> Result<T, Response<Full<Bytes>>> {
    let body = match crate::http_server::collect_body(request.into_body(), MAX_REQUEST_BYTES).await
    {
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(crate::http_server::body_timeout_response());
        }
        Ok(body) => body,
        Err(_) => {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "invalid_request_body",
                "请求内容无效或过大",
                false,
            ));
        }
    };
    serde_json::from_slice(&body).map_err(|_| {
        failure(
            StatusCode::BAD_REQUEST,
            "invalid_request_body",
            "请求内容无效",
            false,
        )
    })
}

fn prepare_request(
    mut request: DownloadRequest,
) -> Result<
    (
        ActivationCode,
        Option<Vec<u8>>,
        Zeroizing<String>,
        Zeroizing<String>,
        Zeroizing<String>,
    ),
    (&'static str, &'static str),
> {
    let smdp = Zeroizing::new(
        request
            .smdp
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_owned(),
    );
    if smdp.is_empty() {
        return Err(("invalid_smdp", "SM-DP+ 地址不能为空"));
    }
    let matching_id = Zeroizing::new(request.matching_id.trim().to_owned());
    let activation_input =
        Zeroizing::new(format!("LPA:1${}${}", smdp.as_str(), matching_id.as_str()));
    let mut activation = ActivationCode::parse(&activation_input)
        .map_err(|_| ("invalid_activation_code", "SM-DP+ 地址或匹配码无效"))?;
    let confirmation_code = Zeroizing::new(request.confirmation_code.trim().to_owned());
    activation
        .set_confirmation_code(
            (!confirmation_code.is_empty()).then_some(confirmation_code.as_str()),
        )
        .map_err(|_| ("invalid_confirmation_code", "确认码格式无效"))?;
    let aid = if request.aid.trim().is_empty() {
        None
    } else {
        Some(parse_hex(request.aid.trim()).ok_or(("invalid_aid", "AID 格式无效"))?)
    };
    let imei = Zeroizing::new(
        validate_imei(request.imei.trim())
            .map_err(|_| ("invalid_imei", "IMEI 必须是 8 位 TAC 或有效的 15 位 IMEI"))?,
    );
    request.matching_id.zeroize();
    request.confirmation_code.zeroize();
    request.smdp.zeroize();
    request.imei.zeroize();
    Ok((activation, aid, imei, matching_id, confirmation_code))
}

fn validate_imei(input: &str) -> Result<String, ()> {
    if input.is_empty() {
        return Ok("35383741".into());
    }
    if !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    if input.len() == 8 {
        return Ok(input.into());
    }
    if input.len() != 15 {
        return Err(());
    }
    let sum: u32 = input
        .bytes()
        .rev()
        .enumerate()
        .map(|(index, byte)| {
            let mut digit = u32::from(byte - b'0');
            if index % 2 == 1 {
                digit *= 2;
                if digit > 9 {
                    digit -= 9;
                }
            }
            digit
        })
        .sum();
    (sum % 10 == 0).then(|| input.to_owned()).ok_or(())
}

fn parse_hex(input: &str) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > 64 || input.len() % 2 != 0 {
        return None;
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some(hex_nibble(pair[0])? << 4 | hex_nibble(pair[1])?))
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn run_download(
    context: &NativeCardContext,
    activation: ActivationCode,
    imei: &str,
    requested_aid: Option<Vec<u8>>,
    matching_id: &str,
    confirmation_code: &str,
    confirmation: Receiver<bool>,
    cancelled: Arc<AtomicBool>,
    operation_service: &Service,
    operation_id: &str,
) -> Result<DownloadResult, DownloadFailure> {
    let result = context.with_transaction(|transport| {
        run_download_on_transport(
            transport,
            activation,
            imei,
            requested_aid,
            matching_id,
            confirmation_code,
            confirmation,
            cancelled,
            operation_service,
            operation_id,
        )
    });
    match result {
        Ok(result) => result,
        Err(error) => Err(failed(
            "reader_operation_busy",
            "读卡器正由其它操作使用，请稍后重试",
            error.to_string(),
        )),
    }
}

/// Run the same eUICC/LPA transaction on an already admitted APDU transport.
/// PC/SC and BLE share this business path; only their leased transport opener
/// differs. The BLE bridge continues to bind every APDU to its selected
/// CoreBluetooth reader identity.
fn run_download_on_transport<T: ApduTransport>(
    transport: &mut T,
    activation: ActivationCode,
    imei: &str,
    requested_aid: Option<Vec<u8>>,
    matching_id: &str,
    confirmation_code: &str,
    confirmation: Receiver<bool>,
    cancelled: Arc<AtomicBool>,
    operation_service: &Service,
    operation_id: &str,
) -> Result<DownloadResult, DownloadFailure>
where
    T::Error: fmt::Display,
{
    if !operation_service.update(
        operation_id,
        "running",
        "preflight",
        "正在检查 eUICC 剩余空间…",
        10,
    ) {
        return Err(failed(
            "esim_operation_journal_failed",
            "无法持久记录下载状态，已停止卡片操作",
            "operation journal write failed before eUICC preflight",
        ));
    }
    let candidates: Vec<Vec<u8>> =
        requested_aid
            .clone()
            .map(|aid| vec![aid])
            .unwrap_or_else(|| {
                KNOWN_ISDR_APPLICATION_AIDS
                    .iter()
                    .map(|aid| aid.to_vec())
                    .collect()
            });
    let mut selected = None;
    for aid in candidates {
        if cancelled.load(Ordering::Acquire) {
            return Err(DownloadFailure::Cancelled);
        }
        match preflight_reader_aid(transport, &aid, cancelled.clone()) {
            Ok((eid, free)) => {
                selected = Some((aid, eid, free));
                break;
            }
            Err(AidProbeFailure::Unusable(_)) if requested_aid.is_none() => continue,
            Err(error) => {
                return Err(failed(
                    "download_failed",
                    "无法读取目标 eUICC 信息",
                    error.message(),
                ));
            }
        }
    }
    let Some((aid, eid, before_free)) = selected else {
        return Err(failed(
            "reader_esim_unavailable",
            "未找到可用的 eUICC 管理应用",
            "没有已知的 ISD-R AID 能读取 EID",
        ));
    };
    if before_free.is_some_and(|free| free > 0 && free < FREE_NVRAM_GUARD) {
        return Err(failed(
            "insufficient_euicc_space",
            "eUICC 可用空间不足 80 KiB，请先删除不需要的 Profile",
            "free NVRAM is below 80 KiB",
        ));
    }
    let mut observer = ReaderDownloadObserver {
        service: operation_service,
        id: operation_id,
        confirmation,
        cancelled: cancelled.clone(),
        matching_id,
        confirmation_code,
        stage: "init_auth",
    };
    install_profile(
        transport,
        &aid,
        &eid,
        &activation,
        imei,
        before_free,
        &mut observer,
    )
}

#[derive(Debug)]
enum AidProbeFailure {
    // Read-only probe failed, but its selected channel was confirmed closed.
    Unusable(String),
    // An ambiguous allocation or close is not permission to probe another AID.
    Stop(String),
}

impl AidProbeFailure {
    fn message(self) -> String {
        match self {
            Self::Unusable(message) | Self::Stop(message) => message,
        }
    }
}

fn preflight_reader_aid<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    cancelled: Arc<AtomicBool>,
) -> Result<(String, Option<u64>), AidProbeFailure>
where
    T::Error: fmt::Display,
{
    if cancelled.load(Ordering::Acquire) {
        return Err(AidProbeFailure::Stop("reader lease was lost".into()));
    }
    let guarded = LeaseAwareTransport {
        inner: BorrowedTransport(&mut *transport),
        cancelled,
    };
    let network = HttpEs9Client::new().map_err(|error| AidProbeFailure::Stop(error.to_string()))?;
    let mut client = match open_lpa_client(guarded, aid, READER_MSS, network) {
        Ok(client) => client,
        Err(proletarius_reader::euicc_lpa::ClientOpenError::Channel(error))
            if missing_isdr_aid(&error) =>
        {
            return Err(AidProbeFailure::Unusable(error.to_string()));
        }
        Err(error) => return Err(AidProbeFailure::Stop(error.to_string())),
    };
    let outcome = (|| {
        let mut raw_eid = [0u8; 16];
        let len = client
            .card_mut()
            .get_eid(&mut raw_eid)
            .map_err(|error| error.to_string())?;
        let eid = format_eid(&raw_eid[..len])
            .ok_or_else(|| "eUICC EID encoding is invalid".to_owned())?;
        let info = client
            .card_mut()
            .get_euicc_info_preferred()
            .map_err(|error| error.to_string())?;
        Ok((eid, info.free_nvram_bytes))
    })();
    client
        .card_mut()
        .transport_mut()
        .close()
        .map_err(|error| AidProbeFailure::Stop(format!("逻辑通道关闭未确认：{error}")))?;
    outcome.map_err(AidProbeFailure::Unusable)
}

fn install_profile<T: ApduTransport>(
    transport: &mut T,
    aid: &[u8],
    eid: &str,
    activation: &ActivationCode,
    imei: &str,
    before_free: Option<u64>,
    observer: &mut ReaderDownloadObserver<'_>,
) -> Result<DownloadResult, DownloadFailure>
where
    T::Error: fmt::Display,
{
    if observer.cancelled.load(Ordering::Acquire) {
        return Err(DownloadFailure::Cancelled);
    }
    let guarded = LeaseAwareTransport {
        inner: BorrowedTransport(&mut *transport),
        cancelled: observer.cancelled.clone(),
    };
    let network = HttpEs9Client::new().map_err(|error| {
        failed(
            "download_failed",
            "无法初始化 eSIM 下载连接",
            error.to_string(),
        )
    })?;
    let mut client = open_lpa_client(guarded, aid, READER_MSS, network).map_err(|error| {
        failed(
            "download_failed",
            "无法打开 eUICC 管理通道",
            error.to_string(),
        )
    })?;

    let outcome = (|| {
        // Preflight and installation open separate channels. Verify the actual
        // channel's card and durably arm its identity before any download APDU.
        let mut actual_eid = [0u8; proletarius_euicc::EID_LENGTH_BYTES];
        let actual_length = client.card_mut().get_eid(&mut actual_eid).map_err(|_| {
            failed(
                "esim_card_identity_unconfirmed",
                "无法核对下载目标卡片身份，尚未写卡",
                "installation channel GetEID failed before mutation",
            )
        })?;
        if format_eid(&actual_eid[..actual_length]).as_deref() != Some(eid) {
            actual_eid.zeroize();
            return Err(failed(
                "esim_card_identity_changed",
                "下载前卡片身份发生变化，尚未写卡",
                "installation card differs from preflight card",
            ));
        }
        let arm_result =
            observer
                .service
                .arm_card_proof(observer.id, &actual_eid[..actual_length], aid);
        actual_eid.zeroize();
        arm_result.map_err(|_| {
            failed(
                "esim_operation_journal_failed",
                "无法安全记录目标卡片身份，尚未写卡",
                "card recovery proof was not durably armed",
            )
        })?;

        let baseline = read_install_notification_baseline(client.card_mut());
        match client.download_with_eid_and_auth_hook(
            activation,
            imei,
            eid,
            observer,
            b0_auth::authenticate,
        ) {
            Ok(_downloaded) => {}
            Err(error) => {
                let diagnostic = redact(
                    &format!("stage={}: {error}", observer.stage),
                    &[observer.matching_id, observer.confirmation_code, imei],
                );
                if observer.cancelled.load(Ordering::Acquire)
                    || matches!(
                        error,
                        proletarius_lpa::Error::Cancelled | proletarius_lpa::Error::Rejected
                    )
                {
                    return Err(DownloadFailure::Cancelled);
                }
                return Err(failed(
                    "download_failed",
                    "eSIM Profile 下载或安装失败",
                    diagnostic,
                ));
            }
        }
        let mut result = DownloadResult::default();
        observer.service.update(
            observer.id,
            "running",
            "notify",
            "正在向运营商发送下载通知…",
            90,
        );
        let mut observed = false;
        for attempt in 0..3 {
            match read_install_notifications(client.card_mut()) {
                Ok(notifications) => {
                    let mut pending = notifications
                        .into_iter()
                        .filter(|notification| notification.sequence_number > baseline)
                        .collect::<Vec<_>>();
                    pending.sort_by_key(|notification| notification.sequence_number);
                    if !pending.is_empty() {
                        observed = true;
                        for notification in pending {
                            if let Err(error) =
                                client.retry_notification(notification.sequence_number)
                            {
                                let diagnostic = redact(
                                    &error.to_string(),
                                    &[observer.matching_id, observer.confirmation_code, imei],
                                );
                                result.warning = Some(format!(
                                    "Profile 已安装，但下载通知发送失败：{diagnostic}"
                                ));
                                result.warning_code =
                                    Some("download_notification_handle_failed".into());
                                break;
                            }
                        }
                        break;
                    }
                }
                Err(_) if attempt == 2 => {
                    result.warning = Some("Profile 已安装，但无法读取运营商下载通知".into());
                    result.warning_code = Some("download_notification_retrieve_failed".into());
                }
                Err(_) => {}
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        if !observed && result.warning.is_none() {
            result.warning = Some("Profile 已安装，但未观察到运营商下载通知".into());
            result.warning_code = Some("download_notification_not_observed".into());
        }
        if let Ok(info) = client.card_mut().get_euicc_info_preferred()
            && let (Some(before), Some(after)) = (before_free, info.free_nvram_bytes)
            && before != after
        {
            let released = after > before;
            result.space_delta = Some(SpaceDelta {
                direction: if released { "released" } else { "consumed" }.into(),
                bytes: before.abs_diff(after).min(i64::MAX as u64) as i64,
            });
        }
        Ok(result)
    })();
    client.card_mut().transport_mut().close().map_err(|error| {
        failed(
            "esim_channel_cleanup_unconfirmed",
            "eSIM 操作通道关闭未确认；请先刷新卡片状态，切勿重复下载",
            error.to_string(),
        )
    })?;
    outcome
}

fn read_install_notification_baseline<T: CardTransport>(card: &mut Euicc<T>) -> u64
where
    T::Error: fmt::Display,
{
    read_install_notifications(card)
        .ok()
        .and_then(|notifications| notifications.iter().map(|item| item.sequence_number).max())
        .unwrap_or(0)
}

fn read_install_notifications<T: CardTransport>(
    card: &mut Euicc<T>,
) -> Result<Vec<NotificationInfo>, String>
where
    T::Error: fmt::Display,
{
    let mut buffer: [NotificationInfo; MAX_NOTIFICATION_COUNT] =
        std::array::from_fn(|_| NotificationInfo::default());
    let count = card
        .list_notifications_into(&mut buffer)
        .map_err(|error| error.to_string())?;
    Ok(buffer.into_iter().take(count).collect())
}

struct ReaderDownloadObserver<'a> {
    service: &'a Service,
    id: &'a str,
    confirmation: Receiver<bool>,
    cancelled: Arc<AtomicBool>,
    matching_id: &'a str,
    confirmation_code: &'a str,
    stage: &'static str,
}

impl DownloadObserver for ReaderDownloadObserver<'_> {
    fn on_stage(&mut self, stage: DownloadStage) {
        let (code, message, progress) = match stage {
            DownloadStage::InitiateAuthentication => ("init_auth", "正在向 SM-DP+ 发起认证…", 12),
            DownloadStage::AuthenticateEuicc => {
                ("auth_euicc", "正在由 eUICC 验证 SM-DP+ 服务器签名…", 24)
            }
            DownloadStage::AuthenticateClient => {
                ("auth_client", "正在向 SM-DP+ 提交 eUICC 认证结果…", 36)
            }
            DownloadStage::AuthenticateServer => {
                ("auth_server", "正在向 SM-DP+ 请求 Profile 数据包…", 60)
            }
            DownloadStage::Install => ("install", "正在将 Profile 写入 eUICC…", 80),
            DownloadStage::Done => ("notify", "Profile 已写入，正在确认下载通知…", 90),
        };
        self.stage = code;
        if !self
            .service
            .update(self.id, "running", code, message, progress)
        {
            self.cancelled.store(true, Ordering::Release);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn confirm_profile_details(
        &mut self,
        _encoded_metadata: &[u8],
        _metadata: &ProfileMetadata,
        estimated_size: i64,
    ) -> bool {
        if !self.service.await_confirmation(self.id, estimated_size) {
            self.cancelled.store(true, Ordering::Release);
            return false;
        }
        match self.confirmation.recv_timeout(CONFIRMATION_TIMEOUT) {
            Ok(true) => {
                let updated = self.service.update(
                    self.id,
                    "running",
                    "confirm_download",
                    "已确认，正在获取 Profile 数据包…",
                    62,
                );
                if !updated {
                    self.cancelled.store(true, Ordering::Release);
                }
                updated
            }
            Ok(false) => false,
            Err(_) => false,
        }
    }
}

struct BorrowedTransport<'a, T>(&'a mut T);

impl<T: ApduTransport> ApduTransport for BorrowedTransport<'_, T> {
    type Error = T::Error;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.0.transmit(command)
    }
}

struct LeaseAwareTransport<T> {
    inner: T,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
enum LeaseAwareError<E> {
    LeaseLost,
    Transport(E),
}

impl<E: fmt::Display> fmt::Display for LeaseAwareError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LeaseLost => formatter.write_str("reader ownership lease was lost"),
            Self::Transport(error) => write!(formatter, "{error}"),
        }
    }
}

impl<T: ApduTransport> ApduTransport for LeaseAwareTransport<T> {
    type Error = LeaseAwareError<T::Error>;

    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(LeaseAwareError::LeaseLost);
        }
        self.inner
            .transmit(command)
            .map_err(LeaseAwareError::Transport)
    }
}

fn format_eid(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 {
        return None;
    }
    let mut eid = String::with_capacity(32);
    for byte in bytes {
        for digit in [byte >> 4, byte & 0x0f] {
            if digit > 9 {
                return None;
            }
            eid.push(char::from(b'0' + digit));
        }
    }
    Some(eid)
}

fn failed(code: &str, message: &str, diagnostic: impl Into<String>) -> DownloadFailure {
    DownloadFailure::Failed {
        code: code.into(),
        message: message.into(),
        diagnostic: diagnostic.into(),
    }
}

fn redact(input: &str, secrets: &[&str]) -> String {
    let mut output = input.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        output = output.replace(secret, "[redacted]");
    }
    output.chars().take(600).collect()
}

fn is_active(state: &str) -> bool {
    matches!(state, "queued" | "running" | "awaiting_confirmation")
}

fn timestamp() -> String {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

fn failure(
    status: StatusCode,
    code: &'static str,
    message: &str,
    retryable: bool,
) -> Response<Full<Bytes>> {
    json_response(
        status,
        &crate::ErrorResponse {
            error: message,
            code,
            retryable,
        },
    )
}

/// Admit every eSIM profile mutation through the same durable-operation gate
/// used by install and switch. The permit must stay in the route scope until
/// the reader lease has been released and its result reconciled.
fn acquire_profile_mutation_gate(
    operation_service: &Service,
) -> Result<OperationGatePermit, Response<Full<Bytes>>> {
    operation_service
        .acquire_operation_gate()
        .map_err(|error| match error {
            BeginError::Busy => failure(
                StatusCode::CONFLICT,
                "esim_operation_busy",
                "已有 eSIM 操作正在进行，请等待完成后重试",
                true,
            ),
            BeginError::ReconciliationRequired => failure(
                StatusCode::CONFLICT,
                "esim_operation_reconciliation_required",
                "上次 eSIM 写卡结果尚未核对，刷新 Profile 状态后再试",
                true,
            ),
            BeginError::Journal => failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_operation_journal_failed",
                "无法安全记录 eSIM 操作状态，尚未修改 Profile",
                true,
            ),
            BeginError::Unavailable => failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "esim_operation_unavailable",
                "无法安全开始 eSIM Profile 操作",
                true,
            ),
        })
}

/// Keep profile mutation admission and the complete physical operation in one
/// scope. The lazy closure is never invoked when the durable gate rejects the
/// request, and the permit remains held through reader release and refresh.
async fn with_profile_mutation_gate<F, Fut>(
    operation_service: &Service,
    operation: F,
) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Response<Full<Bytes>>> + Send + 'static,
{
    let permit = match acquire_profile_mutation_gate(operation_service) {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    spawn_retained_operation(async move {
        let _permit = permit;
        operation().await
    })
    .await
    .unwrap_or_else(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_operation_worker_failed",
            "读卡器操作线程异常结束，请刷新 Profile 列表确认状态",
            true,
        )
    })
}

/// Retain the durable identity owner even when the HTTP waiter disappears.
/// The lazy physical closure is constructed only after the archive claim has
/// been committed, so a request can never reach the dispatcher twice.
async fn with_durable_intent<F, Fut>(
    intent: crate::esim_mutation_intent::MutationRequest,
    operation: F,
) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Response<Full<Bytes>>> + Send + 'static,
{
    spawn_retained_operation(async move {
        crate::esim_mutation_intent::execute(intent, operation).await
    })
    .await
    .unwrap_or_else(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "esim_mutation_owner_failed",
            "eSIM 操作结果待确认；请按原请求编号查询，切勿重试",
            true,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn esim_refresh_reader_preflight_never_treats_postworker_cleanup_or_unknown_as_safe() {
        let failure = |submitted, uncertain, code| crate::reader_pcsc::Failure {
            status: StatusCode::BAD_GATEWAY,
            code,
            message: "synthetic".into(),
            submitted,
            uncertain,
        };
        assert_eq!(
            overview_transport_failure(&failure(false, false, "reader_pcsc_io_failed"), false),
            OverviewReadError::TransportPreflight
        );
        for (submitted, uncertain, entered, code) in [
            (false, false, true, "reader_pcsc_io_failed"),
            (true, false, false, "reader_pcsc_io_failed"),
            (false, true, false, "reader_pcsc_io_failed"),
            (false, false, false, "reader_pcsc_stale_session"),
            (false, false, false, "reader_pcsc_outcome_unknown"),
        ] {
            assert_eq!(
                overview_transport_failure(&failure(submitted, uncertain, code), entered),
                OverviewReadError::Operation
            );
        }
    }
    use proletarius_reader::ReaderObservation;
    use std::{
        collections::VecDeque,
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    mod journal_fault_integration {
        include!("reader_esim_download/journal_fault_integration_tests.rs");
    }

    struct FakeHealthTransport {
        responses: VecDeque<Vec<u8>>,
        commands: Vec<Vec<u8>>,
    }

    const BLE_FIXTURE_READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";
    fn ble_terminal_reply(request: &Value, mut response: Value) -> String {
        assert_eq!(request["bridge_id"], "fixture-bridge");
        assert_eq!(request["operation_id"].as_str().unwrap().len(), 64);
        response["operation_id"] = request["operation_id"].clone();
        response["bridge_id"] = request["bridge_id"].clone();
        response["terminal"] = json!(true);
        format!("{response}\n")
    }

    const BLE_FIXTURE_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct BleApduFixture {
        socket: PathBuf,
        bridge: Option<thread::JoinHandle<Vec<Vec<u8>>>>,
    }

    impl BleApduFixture {
        fn join(mut self) -> Vec<Vec<u8>> {
            self.bridge
                .take()
                .expect("BLE fixture bridge")
                .join()
                .expect("BLE fixture bridge must exit normally")
        }
    }

    impl Drop for BleApduFixture {
        fn drop(&mut self) {
            if let Some(bridge) = self.bridge.take() {
                // Every accept and stream read has a deadline, so this join is
                // bounded even while unwinding a failed assertion.
                let _ = bridge.join();
            }
            let _ = fs::remove_file(&self.socket);
        }
    }

    fn start_ble_apdu_fixture(responses: Vec<String>) -> BleApduFixture {
        start_ble_apdu_fixture_with_status_signal(responses, None)
    }

    fn start_ble_apdu_fixture_with_status_signal(
        responses: Vec<String>,
        status_accepted: Option<std::sync::mpsc::Sender<bool>>,
    ) -> BleApduFixture {
        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
        fn decode_apdu_hex(value: &str) -> Vec<u8> {
            value
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let socket = std::env::temp_dir().join(format!(
            "sbn-{}-{nonce}-{sequence}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let bridge = thread::spawn(move || {
            let accept = |listener: &UnixListener| {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // macOS can inherit O_NONBLOCK from the listener.
                            // Wait for a complete request after accept, while
                            // retaining the fixture's bounded I/O deadlines.
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            return stream;
                        }
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("BLE fixture accept failed: {error}"),
                    }
                }
            };
            let mut status_stream = accept(&listener);
            if let Some(status_accepted) = status_accepted {
                let flags =
                    nix::fcntl::fcntl(&status_stream, nix::fcntl::FcntlArg::F_GETFL).unwrap();
                let blocking = !nix::fcntl::OFlag::from_bits_truncate(flags)
                    .contains(nix::fcntl::OFlag::O_NONBLOCK);
                status_accepted.send(blocking).unwrap();
            }
            let mut request = String::new();
            BufReader::new(status_stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let status: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(status["action"], "status");
            assert!(status["reader"].is_null());
            assert_eq!(status["token"], BLE_FIXTURE_TOKEN);
            status_stream
                .write_all(
                    format!(
                        "{{\"ok\":true,\"reader\":\"{BLE_FIXTURE_READER}\",\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            let mut commands = Vec::new();
            for response in responses {
                let mut stream = accept(&listener);
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["action"], "apdu");
                assert_eq!(request["reader"], BLE_FIXTURE_READER);
                assert_eq!(request["token"], BLE_FIXTURE_TOKEN);
                commands.push(decode_apdu_hex(request["hex"].as_str().unwrap()));
                stream
                    .write_all(
                        ble_terminal_reply(&request, json!({"ok":true,"hex":response})).as_bytes(),
                    )
                    .unwrap();
            }
            commands
        });
        BleApduFixture {
            socket,
            bridge: Some(bridge),
        }
    }

    fn connect_ble_fixture(socket: &std::path::Path) -> crate::reader_ble_apdu::BleApduTransport {
        crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(socket),
            Some(BLE_FIXTURE_TOKEN),
            BLE_FIXTURE_READER,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap()
    }

    #[derive(Default)]
    struct FailingDownloadTransport {
        commands: Vec<Vec<u8>>,
    }

    impl ApduTransport for FailingDownloadTransport {
        type Error = &'static str;

        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.commands.push(command.to_vec());
            Err("offline fake APDU failure")
        }
    }

    impl ApduTransport for FakeHealthTransport {
        type Error = &'static str;

        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.commands.push(command.to_vec());
            self.responses.pop_front().ok_or("fake response exhausted")
        }
    }

    fn health_reply(response: &[u8]) -> Vec<u8> {
        response.to_vec()
    }

    #[test]
    fn selected_reader_requires_current_ble_binding_not_card_presence() {
        let ble = "BLE:12345678-1234-1234-1234-123456789abc";
        let snapshot = DiscoverySnapshot {
            revision: 1,
            readers: vec![ReaderObservation {
                name: ble.into(),
                card: CardPresence::Unknown,
                in_use: false,
                muted: false,
                atr: Vec::new(),
            }],
            selected: Some(ble.into()),
        };
        assert_eq!(
            selected_reader_from_snapshot(Some(ble), &snapshot, true).as_deref(),
            Some(ble)
        );
        assert_eq!(
            selected_reader_from_snapshot(Some(ble), &snapshot, false),
            None
        );
        // A native connect can finish before the next inventory refresh.
        assert_eq!(
            selected_reader_from_snapshot(Some(ble), &DiscoverySnapshot::default(), true)
                .as_deref(),
            Some(ble)
        );

        let mut absent = snapshot.clone();
        absent.readers[0].card = CardPresence::Absent;
        assert_eq!(
            selected_reader_from_snapshot(Some(ble), &absent, true),
            None
        );

        let mut false_present = snapshot.clone();
        false_present.readers[0].card = CardPresence::Present;
        assert_eq!(
            selected_reader_from_snapshot(Some(ble), &false_present, false),
            None,
            "a PC/SC reader named BLE:UUID cannot bypass native BLE binding"
        );

        let mut malformed = snapshot;
        malformed.selected = Some("BLE:../outside".into());
        assert_eq!(
            selected_reader_from_snapshot(Some("BLE:../outside"), &malformed, true),
            None
        );
    }

    #[test]
    fn ble_binding_must_name_the_selected_reader() {
        let selected = "BLE:12345678-1234-1234-1234-123456789abc";
        let foreign = "BLE:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let gate = crate::reader_reservation::Gate::default();
        gate.test_seed_ble_selection(
            selected,
            1,
            Some(crate::reader_ble_apdu::ConnectionBinding {
                reader: selected.into(),
                bridge_id: "bridge-1".into(),
                connection_id: "connection-1".into(),
                close_operation_id: "close-1".into(),
            }),
        );
        assert!(selected_ble_connection_bound(&gate, selected));
        assert!(!selected_ble_connection_bound(&gate, foreign));

        gate.test_seed_ble_selection(
            selected,
            2,
            Some(crate::reader_ble_apdu::ConnectionBinding {
                reader: foreign.into(),
                bridge_id: "bridge-2".into(),
                connection_id: "connection-2".into(),
                close_operation_id: "close-2".into(),
            }),
        );
        assert!(!selected_ble_connection_bound(&gate, selected));
        gate.test_seed_ble_selection(selected, 3, None);
        assert!(!selected_ble_connection_bound(&gate, selected));
    }

    #[test]
    fn stale_discovery_selection_cannot_override_native_selection_or_pending_state() {
        let current = "Reader current";
        let old = "Reader retired";
        let snapshot = DiscoverySnapshot {
            revision: 99,
            readers: [current, old]
                .into_iter()
                .map(|name| ReaderObservation {
                    name: name.into(),
                    card: CardPresence::Present,
                    in_use: false,
                    muted: false,
                    atr: Vec::new(),
                })
                .collect(),
            selected: Some(old.into()),
        };
        assert_eq!(
            selected_reader_from_snapshot(Some(current), &snapshot, false).as_deref(),
            Some(current)
        );
        // Gate returns None while a transition is pending or after disconnect.
        assert_eq!(selected_reader_from_snapshot(None, &snapshot, false), None);
        assert_eq!(
            selected_reader_from_snapshot(Some("Missing reader"), &snapshot, false),
            None
        );
    }

    #[test]
    fn notification_lost_or_malformed_allocation_never_opens_another_aid() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let candidates = KNOWN_ISDR_APPLICATION_AIDS
            .iter()
            .map(|aid| aid.to_vec())
            .collect::<Vec<_>>();
        let mut lost = FailingDownloadTransport::default();
        assert!(matches!(
            list_notifications_on_transport(&mut lost, &candidates, false, cancelled.clone()),
            Err(RetryNotificationFailure::Uncertain(_))
        ));
        assert_eq!(lost.commands.len(), 1);
        let mut malformed = FakeHealthTransport {
            responses: VecDeque::from([vec![0x90, 0]]), // Successful allocation, missing channel number.
            commands: Vec::new(),
        };
        assert!(matches!(
            list_notifications_on_transport(&mut malformed, &candidates, false, cancelled),
            Err(RetryNotificationFailure::Uncertain(_))
        ));
        assert_eq!(malformed.commands.len(), 1);
    }

    #[test]
    fn nickname_read_only_failure_may_probe_after_confirmed_close() {
        let mut replies = VecDeque::new();
        for _ in KNOWN_ISDR_APPLICATION_AIDS {
            replies.extend([
                vec![1, 0x90, 0],
                vec![0x90, 0],
                vec![0x6a, 0x81],
                vec![0x90, 0],
            ]);
        }
        let mut card = FakeHealthTransport {
            responses: replies,
            commands: Vec::new(),
        };
        assert!(
            rename_on_transport(
                &mut card,
                "8985200014631959072",
                None,
                "name",
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert_eq!(card.commands.len(), 4 * KNOWN_ISDR_APPLICATION_AIDS.len());
        assert!(
            card.commands
                .iter()
                .all(|command| !command.windows(2).any(|tag| tag == [0xbf, 0x29])),
            "read-only fallback must never submit nickname mutation"
        );
    }

    #[test]
    fn all_mutation_probes_stop_on_select_cleanup_failure() {
        for action in ["rename", "switch", "delete", "preflight"] {
            let mut card = FakeHealthTransport {
                responses: VecDeque::from([vec![1, 0x90, 0], vec![0x6a, 0x82], vec![0x6a, 0x81]]),
                commands: Vec::new(),
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let failed = match action {
                "rename" => {
                    rename_on_transport(&mut card, "8985200014631959072", None, "name", cancelled)
                        .is_err()
                }
                "switch" => matches!(
                    switch_profile_on_transport(&mut card, "8985200014631959072", None, cancelled),
                    Err(SwitchFailure::Operation(_))
                ),
                "delete" => matches!(
                    delete_profile_on_transport(&mut card, "8985200014631959072", None, cancelled),
                    Err(DeleteFailure::Operation)
                ),
                _ => matches!(
                    preflight_reader_aid(&mut card, KNOWN_ISDR_APPLICATION_AIDS[0], cancelled),
                    Err(AidProbeFailure::Stop(_))
                ),
            };
            assert!(failed, "{action} must preserve unconfirmed cleanup");
            assert_eq!(
                card.commands.len(),
                3,
                "{action} must not allocate another AID"
            );
            assert!(card.responses.is_empty());
        }
    }

    #[test]
    fn selected_read_failures_or_missing_targets_still_require_confirmed_close() {
        for action in ["rename", "switch", "delete", "preflight", "health"] {
            // Delete first records the notification cursor. Other readers
            // immediately perform a read-only profile/EID operation.
            let mut replies = vec![vec![1, 0x90, 0], vec![0x90, 0]];
            if action == "delete" {
                replies.push(vec![0xbf, 0x28, 2, 0xa0, 0, 0x90, 0]);
            }
            replies.extend([vec![0x6a, 0x81], vec![0x6a, 0x81]]);
            let mut card = FakeHealthTransport {
                responses: replies.into(),
                commands: Vec::new(),
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let failed = match action {
                "rename" => {
                    rename_on_transport(&mut card, "8985200014631959072", None, "name", cancelled)
                        .is_err()
                }
                "switch" => matches!(
                    switch_profile_on_transport(&mut card, "8985200014631959072", None, cancelled),
                    Err(SwitchFailure::Operation(_))
                ),
                "delete" => matches!(
                    delete_profile_on_transport(&mut card, "8985200014631959072", None, cancelled),
                    Err(DeleteFailure::Operation)
                ),
                "health" => read_health_profiles_on_transport(&mut card).is_err(),
                _ => matches!(
                    preflight_reader_aid(&mut card, KNOWN_ISDR_APPLICATION_AIDS[0], cancelled),
                    Err(AidProbeFailure::Stop(_))
                ),
            };
            assert!(
                failed,
                "{action} must not turn cleanup failure into a missing card"
            );
            assert_eq!(card.commands.len(), if action == "delete" { 5 } else { 4 });
            assert!(card.responses.is_empty());
        }
    }

    #[test]
    fn overview_close_failure_stops_before_other_aids_or_product_metadata() {
        for at in [7, 17] {
            let mut replies = overview_fixture_responses();
            assert_eq!(replies[at], vec![0x90, 0]);
            replies[at] = vec![0x6a, 0x81];
            replies.truncate(at + 1);
            let mut card = FakeHealthTransport {
                responses: replies.into(),
                commands: Vec::new(),
            };
            assert!(read_overview_on_transport(&mut card).is_err());
            assert_eq!(card.commands.len(), at + 1);
        }
    }

    #[test]
    fn health_scan_treats_only_6a82_as_an_unsupported_aid() {
        let mut responses = VecDeque::new();
        for _ in KNOWN_ISDR_APPLICATION_AIDS {
            responses.extend([
                health_reply(&[0x01, 0x90, 0x00]), // MANAGE CHANNEL
                health_reply(&[0x6a, 0x82]),       // SELECT AID
                health_reply(&[0x90, 0x00]),       // close this channel
            ]);
        }
        let mut card = FakeHealthTransport {
            responses,
            commands: Vec::new(),
        };

        assert_eq!(
            read_health_profiles_on_transport(&mut card).unwrap(),
            HealthProfiles::NoEuicc
        );
        assert_eq!(card.commands.len(), KNOWN_ISDR_APPLICATION_AIDS.len() * 3);
    }

    #[test]
    fn health_scan_does_not_retry_6982_as_a_missing_application() {
        let mut card = FakeHealthTransport {
            responses: VecDeque::from([
                health_reply(&[0x01, 0x90, 0x00]),
                health_reply(&[0x69, 0x82]),
                health_reply(&[0x90, 0x00]),
            ]),
            commands: Vec::new(),
        };

        assert!(read_health_profiles_on_transport(&mut card).is_err());
        assert_eq!(
            card.commands.len(),
            3,
            "security failure must stop the AID scan"
        );
    }

    #[test]
    fn health_scan_stops_when_missing_aid_channel_close_is_rejected() {
        let mut card = FakeHealthTransport {
            responses: VecDeque::from([
                health_reply(&[0x01, 0x90, 0x00]), // MANAGE CHANNEL open
                health_reply(&[0x6a, 0x82]),       // SELECT AID miss
                health_reply(&[0x6a, 0x81]),       // MANAGE CHANNEL close rejected
                health_reply(&[0x02, 0x90, 0x00]), // must not open another channel
            ]),
            commands: Vec::new(),
        };

        let error = read_health_profiles_on_transport(&mut card).unwrap_err();
        assert!(error.contains("logical channel cleanup unconfirmed"));
        assert_eq!(card.commands.len(), 3);
    }

    #[test]
    fn health_scan_keeps_profile_list_failure_distinct_from_no_euicc() {
        let mut card = FakeHealthTransport {
            responses: VecDeque::from([
                health_reply(&[0x01, 0x90, 0x00]),
                health_reply(&[0x90, 0x00]),
                health_reply(&[0x69, 0x82]),
                health_reply(&[0x90, 0x00]),
            ]),
            commands: Vec::new(),
        };

        assert_eq!(
            read_health_profiles_on_transport(&mut card).unwrap(),
            HealthProfiles::ReadFailed
        );
        assert_eq!(card.commands.len(), 4);
    }

    fn overview_fixture_responses() -> Vec<Vec<u8>> {
        fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut encoded = Vec::with_capacity(tag.len() + value.len() + 2);
            encoded.extend_from_slice(tag);
            encoded.push(value.len() as u8);
            encoded.extend_from_slice(value);
            encoded
        }
        fn with_status(mut response: Vec<u8>) -> Vec<u8> {
            response.extend_from_slice(&[0x90, 0x00]);
            response
        }

        let eid = [
            0x89, 0x04, 0x90, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ];
        let eid_reply = with_status(tlv(&[0xbf, 0x3e], &tlv(&[0x5a], &eid)));
        let iccid = encode_iccid("8901000000000000001").unwrap();
        let mut profile_fields = tlv(&[0x5a], &iccid);
        profile_fields.extend_from_slice(&tlv(&[0x9f, 0x70], &[1]));
        profile_fields.extend_from_slice(&tlv(&[0x90], b"Main"));
        profile_fields.extend_from_slice(&tlv(&[0x91], b"Example"));
        profile_fields.extend_from_slice(&tlv(&[0x95], &[2]));
        let profile = tlv(&[0xe3], &profile_fields);
        let list = with_status(tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profile)));

        vec![
            vec![0x01, 0x90, 0x00], // MANAGE CHANNEL
            vec![0x90, 0x00],       // SELECT ISD-R
            eid_reply,
            vec![0x6a, 0x81], // Info2 unsupported; bounded fallback to Info1
            vec![0x6a, 0x81], // Info1 unavailable; profile read remains usable
            with_status(tlv(
                &[0xbf, 0x3c],
                &[
                    tlv(&[0x80], b"example.smdp.com"),
                    tlv(&[0x81], b"example.rootds.com"),
                ]
                .concat(),
            )),
            list,
            vec![0x90, 0x00],       // Close logical channel
            vec![0x01, 0x90, 0x00], // Probe the eSTK.me SE1 AID too
            vec![0x6a, 0x82],
            vec![0x90, 0x00],
            vec![0x01, 0x90, 0x00], // Optional eSTK.me product application
            vec![0x90, 0x00],
            with_status(b"ESTKme-RED".to_vec()),
            with_status(b"T3VAMD0".to_vec()),
            with_status(b"1.0.0".to_vec()),
            with_status(b"37.4.3".to_vec()),
            vec![0x90, 0x00],
        ]
    }

    #[test]
    fn duplicate_eid_keeps_private_proof_readiness_per_aid() {
        for second_profile_read_succeeds in [true, false] {
            let mut replies = overview_fixture_responses();
            let eid_reply = replies[2].clone();
            let profile_reply = if second_profile_read_succeeds {
                replies[6].clone()
            } else {
                vec![0x6a, 0x81]
            };
            replies.splice(
                8..11,
                [
                    vec![0x01, 0x90, 0x00], // Open SE1 after SE0 succeeded.
                    vec![0x90, 0x00],       // Select SE1.
                    eid_reply,              // Both AIDs report the same EID.
                    profile_reply,          // Only a complete read proves SE1.
                    vec![0x90, 0x00],       // Close SE1.
                ],
            );
            let mut transport = FakeHealthTransport {
                responses: replies.into(),
                commands: Vec::new(),
            };
            let overview = read_overview_on_transport(&mut transport).unwrap().unwrap();
            assert!(transport.responses.is_empty());
            assert_eq!(overview.eids.len(), 1, "public display still dedups EID");
            assert_eq!(overview.profiles.len(), 1);
            assert_eq!(
                overview.card_identities[0].1,
                KNOWN_ISDR_APPLICATION_AIDS[0]
            );
            assert_eq!(
                overview.card_identities.len(),
                if second_profile_read_succeeds { 2 } else { 1 }
            );
            if second_profile_read_succeeds {
                assert_eq!(
                    overview.card_identities[1].1,
                    KNOWN_ISDR_APPLICATION_AIDS[1]
                );
            }
            assert!(
                !overview
                    .response(None)
                    .to_string()
                    .contains(&encode_hex_upper(KNOWN_ISDR_APPLICATION_AIDS[1]))
            );
        }
    }

    #[test]
    fn multistandby_euicc_requires_real_eid_and_matching_application() {
        for (eid, aid, expected) in [
            (
                "89049032000000000000000000000001",
                Some(KNOWN_ISDR_APPLICATION_AIDS[0]),
                true,
            ),
            (
                "89049032000000000000000000000002",
                Some(KNOWN_ISDR_APPLICATION_AIDS[0]),
                false,
            ),
            (
                "89049032000000000000000000000001",
                Some(KNOWN_ISDR_APPLICATION_AIDS[1]),
                false,
            ),
            ("89049032000000000000000000000001", None, true),
        ] {
            let mut card = FakeHealthTransport {
                responses: overview_fixture_responses().into(),
                commands: Vec::new(),
            };
            let result = multistandby_euicc(&mut card, eid, aid);
            assert_eq!(result.is_ok(), expected);
            if !expected {
                assert_eq!(result.unwrap_err(), "multistandby_identity_changed");
            }
            assert!(card.responses.is_empty());
            assert!(
                card.commands
                    .iter()
                    .any(|command| command.windows(2).any(|tag| tag == [0xbf, 0x3e]))
            );
        }
    }

    #[test]
    fn overview_scan_projects_eid_and_profiles_with_legacy_json_fields() {
        let mut card = FakeHealthTransport {
            responses: overview_fixture_responses().into(),
            commands: Vec::new(),
        };

        let overview = read_overview_on_transport(&mut card).unwrap().unwrap();
        let lifecycle = lifecycle_facts_from_overview(&overview);
        assert!(lifecycle.available);
        assert_eq!(lifecycle.profile_name, "Example");
        assert_eq!(
            lifecycle.notification_aids,
            vec![notification_aid_bytes("A06573746B6D65FFFF4953442D522030").unwrap()]
        );
        let response = overview.response(None);
        assert_eq!(response["card_type"], "euicc");
        assert_eq!(
            response["chip_info"]["eids"][0]["aid"],
            "A06573746B6D65FFFF4953442D522030"
        );
        assert_eq!(
            response["chip_info"]["eids"][0]["eid"],
            "89049032000000000000000000000001"
        );
        assert!(
            response["chip_info"]["eids"][0]
                .get("manufacturer")
                .is_none()
        );
        assert_eq!(
            response["chip_info"]["eids"][0]["default_smdp_address"],
            "example.smdp.com"
        );
        assert_eq!(
            response["chip_info"]["eids"][0]["root_ds_address"],
            "example.rootds.com"
        );
        assert_eq!(response["profiles"][0]["read_state"], "ready");
        assert_eq!(
            response["profiles"][0]["profiles"][0]["iccid"],
            "8901000000000000001"
        );
        assert_eq!(response["profiles"][0]["profiles"][0]["name"], "Main");
        assert_eq!(
            response["profiles"][0]["profiles"][0]["service_provider_name"],
            "Example"
        );
        assert_eq!(response["profiles"][0]["profiles"][0]["state"], 1);
        assert_eq!(
            response["profiles"][0]["profiles"][0]["class_text"],
            "operational"
        );
        assert_eq!(response["chip_info"]["sku_name"], "ESTKme-RED");
        assert_eq!(response["chip_info"]["serial_number"], "T3VAMD0");
        assert_eq!(response["chip_info"]["firmware"], "1.0.0-37.4.3");
        assert_eq!(response["chip_info"]["identity"]["brand"], "eSTK.me");
        assert_eq!(response["capabilities"]["can_probe_phonebook"], false);
        assert_eq!(response["capabilities"]["vendor_serial_available"], true);
        assert!(response["updated_at"].as_str().unwrap().ends_with('Z'));
        assert_eq!(card.commands.len(), 18);
    }

    #[test]
    fn overview_pki_projection_matches_go_lookup_fallbacks() {
        assert_eq!(
            lookup_euicc_manufacturer("8904903200000000", ""),
            Some("Giesecke+Devrient GmbH 🇩🇪".to_owned())
        );
        assert_eq!(
            lookup_certificate_issuers(&[vec![0x81, 0x37, 0x0f], vec![0xaa]]),
            vec![
                "GSM Association - RSP2 Root CI1".to_owned(),
                "aa".to_owned()
            ]
        );
    }

    #[test]
    fn ble_bridge_binds_every_overview_apdu_to_the_selected_reader() {
        fn hex(bytes: &[u8]) -> String {
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "switch-ble-overview-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let ble_reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let replies = overview_fixture_responses();
        let bridge = thread::spawn(move || {
            let (mut status_stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(status_stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let status_request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(status_request["action"], "status");
            status_stream
                .write_all(
                    format!(
                        "{{\"ok\":true,\"reader\":\"{ble_reader}\",\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            let mut commands = Vec::new();
            for response in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["action"], "apdu");
                assert_eq!(request["reader"], ble_reader);
                commands.push(request["hex"].as_str().unwrap().to_owned());
                stream
                    .write_all(
                        ble_terminal_reply(&request, json!({"ok":true,"hex":hex(&response)}))
                            .as_bytes(),
                    )
                    .unwrap();
            }
            commands
        });

        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&socket),
            Some(&"a".repeat(32)),
            ble_reader,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        let overview = read_overview_on_transport(&mut transport).unwrap().unwrap();
        assert_eq!(overview.profiles.len(), 1);
        let commands = bridge.join().unwrap();
        assert_eq!(commands.len(), 18);
        assert!(commands.iter().all(|command| !command.is_empty()));
        let _ = fs::remove_file(socket);
    }

    #[test]
    fn ble_bridge_drives_rust_health_scan_under_the_same_apdu_contract() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "switch-ble-health-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let ble_reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let bridge = thread::spawn(move || {
            let (mut status_stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(status_stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let status_request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(status_request["action"], "status");
            assert_eq!(status_request["token"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            let status_body = format!(
                "{{\"ok\":true,\"reader\":\"{ble_reader}\",\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n"
            );
            status_stream.write_all(status_body.as_bytes()).unwrap();

            let mut commands = Vec::new();
            for _ in KNOWN_ISDR_APPLICATION_AIDS {
                for response in ["019000", "6a82", "9000"] {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut request = String::new();
                    BufReader::new(stream.try_clone().unwrap())
                        .read_line(&mut request)
                        .unwrap();
                    let request: Value = serde_json::from_str(&request).unwrap();
                    assert_eq!(request["action"], "apdu");
                    let command = request["hex"].as_str().unwrap().to_owned();
                    commands.push(command);
                    let body = ble_terminal_reply(&request, json!({"ok":true,"hex":response}));
                    stream.write_all(body.as_bytes()).unwrap();
                }
            }
            commands
        });

        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&socket),
            Some(&"a".repeat(32)),
            ble_reader,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        let scan = read_health_profiles_on_transport(&mut transport).unwrap();
        assert_eq!(scan, HealthProfiles::NoEuicc);
        let commands = bridge.join().unwrap();
        assert_eq!(commands.len(), KNOWN_ISDR_APPLICATION_AIDS.len() * 3);
        assert!(commands.iter().all(|command| !command.is_empty()));
        let _ = fs::remove_file(socket);
    }

    #[test]
    fn ble_bridge_runs_profile_rename_lpa_and_binds_every_apdu_to_selected_reader() {
        fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut encoded = Vec::with_capacity(tag.len() + value.len() + 2);
            encoded.extend_from_slice(tag);
            encoded.push(value.len() as u8);
            encoded.extend_from_slice(value);
            encoded
        }
        fn decode_apdu_hex(value: &str) -> Vec<u8> {
            value
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "switch-ble-rename-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let ble_reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let iccid = "8985200014631959072";
        let encoded_iccid = encode_iccid(iccid).unwrap();
        let profile = tlv(&[0xe3], &tlv(&[0x5a], &encoded_iccid));
        let profile_list = tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profile));
        let mut profile_response = profile_list;
        profile_response.extend_from_slice(&[0x90, 0x00]);
        let response_hex = vec![
            "019000".to_owned(), // MANAGE CHANNEL
            "9000".to_owned(),   // SELECT ISD-R
            profile_response
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "bf29038001009000".to_owned(), // SET NICKNAME succeeded
            "9000".to_owned(),             // Close the allocated channel
        ];
        let expected_apdu_count = response_hex.len();
        let bridge = thread::spawn(move || {
            let (mut status_stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(status_stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let status: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(status["action"], "status");
            status_stream
                .write_all(
                    format!(
                        "{{\"ok\":true,\"reader\":\"{ble_reader}\",\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            let mut commands = Vec::new();
            for response in response_hex {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["action"], "apdu");
                assert_eq!(request["reader"], ble_reader);
                let command = decode_apdu_hex(request["hex"].as_str().unwrap());
                commands.push(command);
                stream
                    .write_all(
                        ble_terminal_reply(&request, json!({"ok":true,"hex":response})).as_bytes(),
                    )
                    .unwrap();
            }
            commands
        });

        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&socket),
            Some(&"a".repeat(32)),
            ble_reader,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        rename_on_transport(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            "Switch Demo",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        let commands = bridge.join().unwrap();
        assert_eq!(commands.len(), expected_apdu_count);
        assert_eq!(commands[0], [0x00, 0x70, 0x00, 0x00, 0x01]);
        assert!(
            commands[3]
                .windows(b"Switch Demo".len())
                .any(|window| window == b"Switch Demo")
        );
        assert!(
            commands[3]
                .windows(encoded_iccid.len())
                .any(|window| window == encoded_iccid)
        );
        let _ = fs::remove_file(socket);
    }

    #[test]
    fn ble_bridge_runs_profile_delete_lpa_and_binds_every_apdu_to_selected_reader() {
        fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut encoded = Vec::with_capacity(tag.len() + value.len() + 2);
            encoded.extend_from_slice(tag);
            encoded.push(value.len() as u8);
            encoded.extend_from_slice(value);
            encoded
        }
        fn decode_apdu_hex(value: &str) -> Vec<u8> {
            value
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "switch-ble-delete-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let ble_reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let iccid = "8985200014631959072";
        let encoded_iccid = encode_iccid(iccid).unwrap();
        let profile = tlv(&[0xe3], &tlv(&[0x5a], &encoded_iccid));
        let profile_list = tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profile));
        let mut profile_response = profile_list;
        profile_response.extend_from_slice(&[0x90, 0x00]);
        let profile_response = profile_response
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut responses = vec![
            "019000".to_owned(),       // MANAGE CHANNEL
            "9000".to_owned(),         // SELECT ISD-R
            "bf2802a0009000".into(),   // Empty notification cursor before deletion
            profile_response,          // Locate the ICCID on the eUICC
            "6a81".into(),             // EUICCInfo2 is unsupported
            "6a81".into(),             // EUICCInfo1 is also unsupported
            "bf33038001009000".into(), // DELETE PROFILE accepted
            "bf2d02a0009000".into(),   // Authoritative empty profile list
            "bf2802a0009000".into(),   // No delete notification observed (attempt 1)
            "bf2802a0009000".into(),   // attempt 2
            "bf2802a0009000".into(),   // attempt 3
        ];
        // NVRAM accounting is best-effort and probes EUICCInfo2 then EUICCInfo1
        // three times. Keep the scripted card deterministic and offline.
        for _ in 0..3 {
            responses.push("6a81".into());
            responses.push("6a81".into());
        }
        responses.push("9000".into()); // CLOSE CHANNEL
        let expected_apdu_count = responses.len();
        let bridge = thread::spawn(move || {
            let (mut status_stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(status_stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let status: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(status["action"], "status");
            assert_eq!(status["token"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            status_stream
                .write_all(
                    format!(
                        "{{\"ok\":true,\"reader\":\"{ble_reader}\",\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            let mut commands = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["action"], "apdu");
                assert_eq!(request["reader"], ble_reader);
                assert_eq!(request["token"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                commands.push(decode_apdu_hex(request["hex"].as_str().unwrap()));
                stream
                    .write_all(
                        ble_terminal_reply(&request, json!({"ok":true,"hex":response})).as_bytes(),
                    )
                    .unwrap();
            }
            commands
        });

        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&socket),
            Some(&"a".repeat(32)),
            ble_reader,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        let result = delete_profile_on_transport(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.is_some());

        let commands = bridge.join().unwrap();
        assert_eq!(commands.len(), expected_apdu_count);
        assert_eq!(commands[0], [0x00, 0x70, 0x00, 0x00, 0x01]);
        assert!(commands.iter().any(|command| {
            command
                .windows(encoded_iccid.len())
                .any(|window| window == encoded_iccid)
        }));
        assert!(
            commands
                .iter()
                .any(|command| command.windows(2).any(|window| window == [0xbf, 0x33]))
        );
        let _ = fs::remove_file(socket);
    }

    #[test]
    fn ble_fixture_waits_for_status_request_after_accept() {
        let (accepted, accepted_rx) = std::sync::mpsc::channel();
        let fixture = start_ble_apdu_fixture_with_status_signal(vec![], Some(accepted));
        let mut stream = UnixStream::connect(&fixture.socket).unwrap();
        stream
            .write_all(
                format!(
                    "{{\"action\":\"status\",\"reader\":null,\"token\":\"{BLE_FIXTURE_TOKEN}\"}}\n"
                )
                .as_bytes(),
            )
            .unwrap();
        assert!(
            accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "fixture must wait on a blocking accepted status stream"
        );
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["reader"], BLE_FIXTURE_READER);
        assert!(fixture.join().is_empty());
    }

    #[test]
    fn notification_discovery_does_not_downgrade_failed_select_cleanup() {
        let fixture = start_ble_apdu_fixture(vec!["019000".into(), "6a82".into(), "6a81".into()]);
        let mut transport = connect_ble_fixture(&fixture.socket);
        let result = list_notifications_on_transport(
            &mut transport,
            &[
                KNOWN_ISDR_APPLICATION_AIDS[0].to_vec(),
                KNOWN_ISDR_APPLICATION_AIDS[1].to_vec(),
            ],
            false,
            Arc::new(AtomicBool::new(false)),
        );
        assert!(matches!(
            result,
            Err(RetryNotificationFailure::Uncertain(_))
        ));
        assert_eq!(
            fixture.join().len(),
            3,
            "failed SELECT cleanup must stop AID discovery"
        );
    }

    #[test]
    fn notification_discovery_stops_at_unconfirmed_channel_close() {
        for list in ["bf2802a0009000", "6a81"] {
            let fixture = start_ble_apdu_fixture(vec![
                "019000".into(),
                "9000".into(),
                list.into(),
                "6a81".into(), // CLOSE rejected, even if listing also failed
            ]);
            let mut transport = connect_ble_fixture(&fixture.socket);
            let result = list_notifications_on_transport(
                &mut transport,
                &[
                    KNOWN_ISDR_APPLICATION_AIDS[0].to_vec(),
                    KNOWN_ISDR_APPLICATION_AIDS[1].to_vec(),
                ],
                false,
                Arc::new(AtomicBool::new(false)),
            );
            assert!(matches!(
                result,
                Err(RetryNotificationFailure::Uncertain(_))
            ));
            assert_eq!(fixture.join().len(), 4, "must not open the next AID");
        }
    }

    #[test]
    fn ble_bridge_lists_notifications_on_the_selected_reader() {
        let fixture = start_ble_apdu_fixture(vec![
            "019000".into(),         // MANAGE CHANNEL
            "9000".into(),           // SELECT ISD-R
            "bf2802a0009000".into(), // Empty notification list
            "9000".into(),           // CLOSE CHANNEL
        ]);
        let mut transport = connect_ble_fixture(&fixture.socket);

        let items = list_notifications_on_transport(
            &mut transport,
            &[KNOWN_ISDR_APPLICATION_AIDS[0].to_vec()],
            true,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        assert!(items.is_empty());
        let commands = fixture.join();
        assert_eq!(commands.len(), 4);
        assert_eq!(commands[0], [0x00, 0x70, 0x00, 0x00, 0x01]);
        assert!(
            commands
                .iter()
                .any(|command| { command.windows(2).any(|window| window == [0xbf, 0x28]) })
        );
    }

    #[test]
    fn lifecycle_reports_pending_switch_notifications_without_removing_them() {
        for event in [1, 2] {
            let fixture = start_ble_apdu_fixture(vec![
                "019000".into(), // MANAGE CHANNEL
                "9000".into(),   // SELECT ISD-R
                format!("bf280ba009bf2f068001018101{event:02x}9000"),
                "9000".into(), // CLOSE CHANNEL
            ]);
            let mut transport = connect_ble_fixture(&fixture.socket);
            assert_eq!(
                process_lifecycle_notifications(
                    &mut transport,
                    &[KNOWN_ISDR_APPLICATION_AIDS[0].to_vec()],
                )
                .unwrap_err(),
                "运营商切卡通知仍待发送"
            );
            let commands = fixture.join();
            assert_eq!(commands.len(), 4);
            assert!(
                !commands
                    .iter()
                    .any(|command| command.windows(2).any(|tag| tag == [0xbf, 0x2b]))
            );
            assert!(
                !commands
                    .iter()
                    .any(|command| command.windows(2).any(|tag| tag == [0xbf, 0x30]))
            );
        }
    }

    #[test]
    fn ble_bridge_retries_missing_notification_on_the_selected_reader() {
        let fixture = start_ble_apdu_fixture(vec![
            "019000".into(),         // MANAGE CHANNEL
            "9000".into(),           // SELECT ISD-R
            "bf2b02a0009000".into(), // Sequence has no pending payload
            "9000".into(),           // CLOSE CHANNEL
        ]);
        let mut transport = connect_ble_fixture(&fixture.socket);
        let receipt_root = std::env::temp_dir().join(format!(
            "switch-notification-transport-test-{}-{}",
            std::process::id(),
            crate::unix_now_ms()
        ));
        std::fs::create_dir_all(&receipt_root).unwrap();
        let request_id = "123e4567-e89b-42d3-a456-426614174000";
        let request_body = b"transport-test";
        let receipt_store = match crate::reader_notification_intent::preflight(
            &receipt_root,
            request_id,
            request_body,
        )
        .unwrap()
        {
            crate::reader_notification_intent::Preflight::New(store) => store,
            _ => unreachable!(),
        };

        let result = retry_notification_on_transport(
            &mut transport,
            123,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            Arc::new(AtomicBool::new(false)),
            &receipt_store,
            request_id,
            &crate::reader_notification_intent::request_fingerprint(request_body),
            "BLE:fixture",
            1,
            &crate::reader_notification_intent::card_identity_hash("fixture-card"),
        );

        assert_eq!(result, Err(RetryNotificationFailure::NotFound));
        let commands = fixture.join();
        assert_eq!(commands.len(), 4);
        assert_eq!(commands[0], [0x00, 0x70, 0x00, 0x00, 0x01]);
        assert!(
            commands
                .iter()
                .any(|command| { command.windows(2).any(|window| window == [0xbf, 0x2b]) })
        );
        let _ = fs::remove_dir_all(receipt_root);
    }

    #[test]
    fn ble_bridge_runs_profile_switch_lpa_and_binds_every_apdu_to_selected_reader() {
        fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut encoded = Vec::with_capacity(tag.len() + value.len() + 2);
            encoded.extend_from_slice(tag);
            encoded.push(value.len() as u8);
            encoded.extend_from_slice(value);
            encoded
        }
        fn decode_apdu_hex(value: &str) -> Vec<u8> {
            value
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!("sbsw-{}-{nonce}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket).unwrap();
        let ble_reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let iccid = "8985200014631959072";
        let encoded_iccid = encode_iccid(iccid).unwrap();
        let mut profile_fields = tlv(&[0x5a], &encoded_iccid);
        profile_fields.extend_from_slice(&tlv(&[0x9f, 0x70], &[0x00])); // disabled
        let profile = tlv(&[0xe3], &profile_fields);
        let profile_list = tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profile));
        let mut profile_response = profile_list;
        profile_response.extend_from_slice(&[0x90, 0x00]);
        let response_hex = vec![
            "019000".to_owned(), // MANAGE CHANNEL
            "9000".to_owned(),   // SELECT ISD-R
            profile_response
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "bf31038001009000".to_owned(), // ENABLE PROFILE accepted
            "9000".to_owned(),             // Close the allocated channel
        ];
        let expected_apdu_count = response_hex.len();
        let bridge = thread::spawn(move || {
            let (mut status_stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(status_stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let status: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(status["action"], "status");
            assert_eq!(status["token"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            status_stream
                .write_all(
                    format!(
                        "{{\"ok\":true,\"reader\":\"{ble_reader}\",\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            let mut commands = Vec::new();
            for response in response_hex {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["action"], "apdu");
                assert_eq!(request["reader"], ble_reader);
                assert_eq!(request["token"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                let command = decode_apdu_hex(request["hex"].as_str().unwrap());
                commands.push(command);
                stream
                    .write_all(
                        ble_terminal_reply(&request, json!({"ok":true,"hex":response})).as_bytes(),
                    )
                    .unwrap();
            }
            commands
        });

        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&socket),
            Some(&"a".repeat(32)),
            ble_reader,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        switch_profile_on_transport(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        let commands = bridge.join().unwrap();
        assert_eq!(commands.len(), expected_apdu_count);
        assert_eq!(commands[0], [0x00, 0x70, 0x00, 0x00, 0x01]);
        assert!(
            commands[3]
                .windows(encoded_iccid.len())
                .any(|window| window == encoded_iccid)
        );
        assert!(commands.iter().all(|command| !command.is_empty()));
        let _ = fs::remove_file(socket);
    }

    #[tokio::test]
    async fn health_response_preserves_active_profile_and_operation_projection() {
        let mut profile = ProfileInfo::default();
        profile.iccid = encode_iccid("8985200014631959072").unwrap();
        profile.nickname = b"Test nickname".to_vec();
        profile.profile_name = b"Original name".to_vec();
        profile.service_provider_name = b"SIMworld".to_vec();
        profile.state = ProfileState::Enabled;
        profile.class = ProfileClass::Operational;
        let mut operation = OperationSnapshot::default();
        operation.id = "operation-7".into();
        operation.state = "running".into();

        let response = health_response(
            HealthProfiles::Listed(vec![health_profile(&profile)]),
            Some(operation),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["card_type"], "euicc");
        assert_eq!(value["state"], "active");
        assert_eq!(value["active_profile"]["iccid"], "8985200014631959072");
        assert_eq!(value["active_profile"]["name"], "Test nickname");
        assert_eq!(value["active_profile"]["service_provider_name"], "SIMworld");
        assert_eq!(value["active_profile"]["state"], 1);
        assert_eq!(value["active_profile"]["state_text"], "已启用");
        assert_eq!(value["active_profile"]["class_text"], "operational");
        assert_eq!(value["operation"]["id"], "operation-7");
        assert_eq!(value["operation"]["state"], "running");
    }

    #[tokio::test]
    async fn health_response_distinguishes_empty_profile_list_from_read_error() {
        for (profiles, expected_state) in [
            (HealthProfiles::Listed(Vec::new()), "no_active_profile"),
            (HealthProfiles::ReadFailed, "profile_read_error"),
        ] {
            let response = health_response(profiles, None);
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(value["state"], expected_state);
            assert!(value["message"].as_str().is_some());
            assert!(value["operation"].is_null());
        }
    }

    #[tokio::test]
    async fn health_response_maps_missing_reader_euicc_to_legacy_probe_failure() {
        let response = health_response(HealthProfiles::NoEuicc, None);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["code"], "reader_esim_health_failed");
        assert_eq!(value["error"], "读取读卡器 eSIM 运行状态失败");
        assert_eq!(value["retryable"], true);
    }

    struct JournalFixture(PathBuf);

    impl JournalFixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../build/runtime-tests/reader-esim-journal");
            let path = root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for JournalFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FakeDeleteCard {
        notifications: Vec<NotificationInfo>,
        fail_retrieve: bool,
        removed: usize,
    }

    struct FakeSwitchCard {
        responses: VecDeque<Vec<u8>>,
        commands: usize,
    }

    impl CardTransport for FakeSwitchCard {
        type Error = EuiccError<String>;

        fn transmit(&mut self, _command: &[u8], response: &mut Vec<u8>) -> Result<(), Self::Error> {
            self.commands += 1;
            response.clear();
            response.extend(
                self.responses
                    .pop_front()
                    .ok_or(EuiccError::Invalid("fake switch response queue exhausted"))?,
            );
            Ok(())
        }
    }

    impl RspCard for FakeDeleteCard {
        type Error = String;

        fn challenge(&mut self) -> Result<Vec<u8>, Self::Error> {
            Err("unexpected challenge".into())
        }

        fn euicc_info1(&mut self) -> Result<Vec<u8>, Self::Error> {
            Err("unexpected info1".into())
        }

        fn authenticate_server(
            &mut self,
            _input: proletarius_euicc::ServerAuthenticationRequest<'_>,
        ) -> Result<Vec<u8>, Self::Error> {
            Err("unexpected authentication".into())
        }

        fn transmit_b0_authentication(&mut self, _body: &[u8]) -> Result<Vec<u8>, Self::Error> {
            Err("unexpected B0 authentication".into())
        }

        fn prepare_download(
            &mut self,
            _input: proletarius_euicc::PrepareDownloadRequest<'_>,
        ) -> Result<Vec<u8>, Self::Error> {
            Err("unexpected prepare download".into())
        }

        fn install(
            &mut self,
            _package: &[u8],
        ) -> Result<proletarius_euicc::InstalledProfile, Self::Error> {
            Err("unexpected install".into())
        }

        fn cancel(
            &mut self,
            _transaction_id: &[u8],
            _reason: proletarius_euicc::CancelSessionReason,
        ) -> Result<Vec<u8>, Self::Error> {
            Err("unexpected cancel".into())
        }

        fn retrieve_notification(
            &mut self,
            sequence_number: u64,
        ) -> Result<Option<proletarius_euicc::PendingNotification>, Self::Error> {
            if self.fail_retrieve {
                return Err("fixture retrieve failure".into());
            }
            Ok(Some(proletarius_euicc::PendingNotification {
                sequence_number,
                address: "smdp.example".into(),
                encoded: vec![0x30, 0x00],
            }))
        }

        fn remove_notification(&mut self, _sequence_number: u64) -> Result<(), Self::Error> {
            self.removed += 1;
            Ok(())
        }
    }

    impl DeleteCard for FakeDeleteCard {
        fn list_all_notifications(
            &mut self,
            output: &mut [NotificationInfo],
        ) -> Result<usize, String> {
            if self.notifications.len() > output.len() {
                return Err("fixture list exceeds capacity".into());
            }
            output[..self.notifications.len()].clone_from_slice(&self.notifications);
            Ok(self.notifications.len())
        }
    }

    struct FakeDeleteNetwork {
        fail_notification: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Es9Transport for FakeDeleteNetwork {
        type Error = String;

        fn initiate(
            &self,
            _activation: &ActivationCode,
            _challenge: &[u8],
            _euicc_info1: &[u8],
            _timeout: Duration,
        ) -> Result<proletarius_lpa::InitiateResponse, Self::Error> {
            Err("unexpected initiate".into())
        }

        fn authenticate_client(
            &self,
            _activation: &ActivationCode,
            _server_transaction_id: &str,
            _authenticate_server_response: &[u8],
            _timeout: Duration,
        ) -> Result<proletarius_lpa::AuthenticateResponse, Self::Error> {
            Err("unexpected authentication".into())
        }

        fn get_bound_profile_package(
            &self,
            _activation: &ActivationCode,
            _server_transaction_id: &str,
            _prepare_download_response: &[u8],
            _timeout: Duration,
        ) -> Result<proletarius_lpa::BoundProfilePackage, Self::Error> {
            Err("unexpected package".into())
        }

        fn handle_notification(
            &self,
            _address: &str,
            _notification: &[u8],
            _timeout: Duration,
        ) -> Result<(), Self::Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_notification {
                Err("fixture delivery failure".into())
            } else {
                Ok(())
            }
        }

        fn cancel_session(
            &self,
            _activation: &ActivationCode,
            _server_transaction_id: &str,
            _cancel_response: &[u8],
            _timeout: Duration,
        ) -> Result<(), Self::Error> {
            Err("unexpected cancel".into())
        }
    }

    fn fake_delete_notification(iccid: &[u8]) -> NotificationInfo {
        NotificationInfo {
            sequence_number: 14,
            event: 3,
            address: b"smdp.example".to_vec(),
            iccid: iccid.to_vec(),
        }
    }

    #[test]
    fn imei_accepts_removable_reader_tac_and_valid_imei_only() {
        assert_eq!(validate_imei("").unwrap(), "35383741");
        assert_eq!(validate_imei("35383741").unwrap(), "35383741");
        assert_eq!(validate_imei("350225641234561").unwrap(), "350225641234561");
        assert!(validate_imei("350225641234560").is_err());
    }

    #[test]
    fn rename_request_requires_a_valid_iccid_and_bounded_nonempty_name() {
        let (iccid, aid, name) = validate_rename_request(RenameProfileRequest {
            client_request_id: String::new(),
            iccid: " 8985200014631959072 ".into(),
            aid: "A0000005591010".into(),
            name: "  Travel  ".into(),
        })
        .unwrap();
        assert_eq!(iccid, "8985200014631959072");
        assert_eq!(aid.unwrap(), vec![0xa0, 0, 0, 5, 0x59, 0x10, 0x10]);
        assert_eq!(name, "Travel");

        let long_name = "x".repeat(65);
        for (iccid, name) in [
            ("8985200014631959073", "Travel"),
            ("8985200014631959072", "  "),
            ("8985200014631959072", long_name.as_str()),
        ] {
            assert!(
                validate_rename_request(RenameProfileRequest {
                    client_request_id: String::new(),
                    iccid: iccid.into(),
                    aid: String::new(),
                    name: name.into(),
                })
                .is_err()
            );
        }
    }

    #[test]
    fn hex_aid_and_eid_encoding_are_bounded_and_digit_only() {
        assert_eq!(parse_hex("A000").unwrap(), [0xa0, 0x00]);
        assert!(parse_hex("A00").is_none());
        assert!(parse_hex("0Z00").is_none());
        assert_eq!(
            format_eid(&[0x89; 16]).unwrap(),
            "89898989898989898989898989898989"
        );
        assert!(format_eid(&[0xfa; 16]).is_none());
    }

    #[test]
    fn operation_confirmation_is_one_shot_and_completes_snapshot() {
        let service = Service::default();
        let permit = service.begin().unwrap();
        {
            let mut state = service.state.lock().unwrap();
            let record = state.as_mut().unwrap();
            record.snapshot.state = "awaiting_confirmation".into();
        }
        let snapshot = service
            .confirm(ConfirmationRequest {
                operation_id: permit.id.clone(),
                confirm: true,
            })
            .unwrap();
        assert_eq!(snapshot.state, "running");
        assert!(
            permit
                .confirmation
                .recv_timeout(Duration::from_millis(50))
                .unwrap()
        );
        assert!(
            service
                .confirm(ConfirmationRequest {
                    operation_id: permit.id,
                    confirm: true,
                })
                .is_err()
        );
    }

    #[test]
    fn journal_recovery_marks_inflight_install_unknown_and_strips_sensitive_fields() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        assert!(service.update(
            &permit.id,
            "running",
            "install",
            "正在将 Profile 写入 eUICC…",
            80,
        ));
        let mut sensitive = service.current().unwrap();
        sensitive.target_iccid = Some("89882110000000000001".into());
        sensitive.diagnostic = Some("matching-secret confirmation-secret imei-secret".into());
        service
            .persist(&sensitive, &CardProof::NotArmed, None)
            .unwrap();
        drop(permit);
        drop(service);

        let recovered = Service::open(&fixture.0).unwrap();
        let snapshot = recovered.current().unwrap();
        assert_eq!(snapshot.state, "failed");
        assert_eq!(
            snapshot.error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
        assert_eq!(snapshot.step.as_deref(), Some("interrupted"));
        assert!(!snapshot.recoverable);
        assert!(!is_active(&snapshot.state));
        assert!(snapshot.diagnostic.is_none());
        assert!(snapshot.target_iccid.is_none());
        assert!(matches!(
            recovered.begin(),
            Err(BeginError::ReconciliationRequired)
        ));
        assert!(matches!(
            recovered.begin_module_operation(),
            Err(ModuleOperationAdmissionError::Busy)
        ));
        assert!(recovered.reconcile_after_overview_refresh("Reader A", 1, &[]));
        assert!(recovered.current().unwrap().recoverable);
        let journal = fs::read(fixture.0.join("reader-esim-operation.json")).unwrap();
        let journal = String::from_utf8(journal).unwrap();
        for secret in [
            "89882110000000000001",
            "matching-secret",
            "confirmation-secret",
            "imei-secret",
        ] {
            assert!(!journal.contains(secret));
        }
        assert!(journal.contains("esim_operation_interrupted"));
        let next = recovered.begin().unwrap();
        assert_ne!(next.id, snapshot.id);
    }

    #[test]
    fn journal_write_failure_rejects_operation_admission() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        fs::create_dir(fixture.0.join("reader-esim-operation.json")).unwrap();
        assert!(matches!(service.begin(), Err(BeginError::Journal)));
        assert!(service.current().is_none());
        assert!(matches!(
            service.begin_module_operation(),
            Err(ModuleOperationAdmissionError::UnsafeState)
        ));
    }

    #[test]
    fn dropped_download_owner_persists_unknown_outcome_before_releasing_gate() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().expect("admit operation");
        let id = permit.id.clone();
        drop(permit);

        let snapshot = service
            .current()
            .expect("interrupted record remains visible");
        assert_eq!(snapshot.id, id);
        assert_eq!(snapshot.state, "failed");
        assert_eq!(
            snapshot.error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
        assert!(!snapshot.recoverable);
        assert!(matches!(
            service.begin(),
            Err(BeginError::ReconciliationRequired)
        ));

        let persisted: Vec<PersistedOperation> = serde_json::from_slice(
            &fs::read(fixture.0.join("reader-esim-operation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted[0].snapshot.error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
        assert!(persisted[0].snapshot.diagnostic.is_none());
        assert!(persisted[0].snapshot.target_iccid.is_none());
    }

    #[test]
    fn dropped_switch_owner_also_requires_identity_reconciliation() {
        let service = Service::default();
        let permit = service
            .begin_switch("89882110000000000001")
            .expect("admit switch");
        drop(permit);

        let snapshot = service
            .current()
            .expect("interrupted switch remains visible");
        assert_eq!(snapshot.kind, "switch_profile");
        assert_eq!(snapshot.state, "failed");
        assert_eq!(
            snapshot.error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
        assert!(!snapshot.recoverable);
        assert!(matches!(
            service.begin_switch("89882110000000000002"),
            Err(BeginError::ReconciliationRequired)
        ));
    }

    #[tokio::test]
    async fn card_specific_health_and_overview_hide_other_reader_operation() {
        const TARGET: &str = "89882110000000000001";
        let service = Service::default();
        let gate = crate::reader_reservation::Gate::default();
        gate.test_seed_selection("reader-a", 7);
        let permit = service.begin_switch(TARGET).unwrap();

        // The operation becomes card-specific only after a native lease has
        // established its exact reader and selection generation.
        assert!(
            service
                .current_for_selected(gate.selected_reader_generation())
                .is_none()
        );
        assert!(service.bind_reader_owner(&permit.id, "reader-a", 7));
        assert!(!service.bind_reader_owner(&permit.id, "reader-b", 8));
        let selected = current_for_reader_lease(&service, &gate, "reader-a", 7)
            .expect("same card and generation may show its operation");
        assert_eq!(selected.target_iccid.as_deref(), Some(TARGET));

        let health = health_response(HealthProfiles::Listed(Vec::new()), Some(selected.clone()));
        let health: Value =
            serde_json::from_slice(&health.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(health["operation"]["target_iccid"], TARGET);
        let overview = ReaderOverview {
            eids: Vec::new(),
            profiles: Vec::new(),
            product: None,
            card_identities: Vec::new(),
        };
        assert_eq!(
            overview.response(Some(selected))["operation"]["target_iccid"],
            TARGET
        );

        for (reader, generation) in [("reader-b", 8), ("reader-a", 8), ("", 0)] {
            gate.test_seed_selection(reader, generation);
            let operation = current_for_reader_lease(&service, &gate, "reader-a", 7);
            assert!(
                operation.is_none(),
                "stale operation shown for {reader}:{generation}"
            );
            let health = health_response(HealthProfiles::Listed(Vec::new()), operation.clone());
            let health: Value =
                serde_json::from_slice(&health.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert!(health["operation"].is_null());
            let overview = overview.response(operation);
            assert!(overview.get("operation").is_none());
            assert!(!overview.to_string().contains(TARGET));
        }
        // The global operation journal endpoint retains recovery visibility.
        assert_eq!(
            service.current().unwrap().target_iccid.as_deref(),
            Some(TARGET)
        );
        service.finish_switch(&permit.id, Ok(DownloadResult::default()));
        drop(permit);

        // Interleaving: A finished an APDU, then selection and the global
        // operation advanced to B before A's HTTP response was projected.
        gate.test_seed_selection("reader-b", 8);
        let b = service.begin_switch("89882110000000000002").unwrap();
        assert!(service.bind_reader_owner(&b.id, "reader-b", 8));
        assert!(current_for_reader_lease(&service, &gate, "reader-a", 7).is_none());
        assert_eq!(
            current_for_reader_lease(&service, &gate, "reader-b", 8)
                .unwrap()
                .target_iccid
                .as_deref(),
            Some("89882110000000000002")
        );
        drop(b);
    }

    #[test]
    fn recovered_operation_without_native_lease_owner_is_global_only() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin_switch("89882110000000000001").unwrap();
        assert!(service.bind_reader_owner(&permit.id, "reader-a", 7));
        service.finish_switch(&permit.id, Ok(DownloadResult::default()));
        drop(permit);
        drop(service);

        let recovered = Service::open(&fixture.0).unwrap();
        assert!(recovered.current().is_some());
        assert!(
            recovered
                .current_for_selected(Some(("reader-a".into(), 7)))
                .is_none()
        );
        assert!(recovered.current_for_selected(None).is_none());
    }

    #[tokio::test]
    async fn shutdown_closes_esim_admission_and_waits_for_owned_operation() {
        let service = Service::default();
        let permit = service.begin().expect("admit operation");
        service.begin_shutdown();
        assert!(matches!(service.begin(), Err(BeginError::Unavailable)));

        let mut wait = Box::pin(service.wait_for_shutdown());
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut wait)
                .await
                .is_err()
        );
        drop(permit);
        wait.await.expect("owner released before teardown proceeds");
        assert_eq!(
            service.current().unwrap().error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
    }

    #[tokio::test]
    async fn retained_reader_owner_survives_cancelled_http_waiter_and_shutdown_drains_it() {
        let service = Arc::new(Service::default());
        let worker_service = service.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            spawn_retained_operation(async move {
                let permit = worker_service.begin().expect("admit retained switch");
                let operation_id = permit.id.clone();
                entered_tx.send(()).expect("test waiter remains alive");
                release_rx.await.expect("test releases operation owner");
                worker_service.finish(&operation_id, Ok(DownloadResult::default()));
                drop(permit);
            })
            .await
            .expect("retained operation completes")
        });

        entered_rx.await.expect("owner acquired durable gate");
        waiter.abort();
        assert!(
            waiter
                .await
                .expect_err("HTTP waiter was cancelled")
                .is_cancelled()
        );

        service.begin_shutdown();
        let shutdown_service = service.clone();
        let shutdown = tokio::spawn(async move { shutdown_service.wait_for_shutdown().await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must retain the active owner"
        );

        release_tx.send(()).expect("release retained operation");
        shutdown
            .await
            .expect("shutdown coordinator remains alive")
            .expect("shutdown drains the owner");
        assert_eq!(service.current().unwrap().state, "succeeded");
    }

    #[tokio::test]
    async fn cancelled_waiter_before_durable_begin_is_fenced_by_shutdown_admission() {
        let service = Arc::new(Service::default());
        let owner_service = service.clone();
        let (owner_started_tx, owner_started_rx) = tokio::sync::oneshot::channel();
        let (allow_begin_tx, allow_begin_rx) = tokio::sync::oneshot::channel();
        let (owner_finished_tx, owner_finished_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            spawn_retained_operation(async move {
                owner_started_tx
                    .send(())
                    .expect("test observes retained owner");
                allow_begin_rx
                    .await
                    .expect("test releases pre-begin barrier");
                let admitted = match owner_service.begin() {
                    Ok(permit) => {
                        drop(permit);
                        true
                    }
                    Err(BeginError::Unavailable) => false,
                    Err(error) => panic!("unexpected pre-begin admission result: {error:?}"),
                };
                owner_finished_tx
                    .send(admitted)
                    .expect("test observes pre-begin owner completion");
            })
            .await
            .expect("retained owner completes independently");
        });

        owner_started_rx
            .await
            .expect("retained owner started before begin");
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        service.begin_shutdown();
        allow_begin_tx.send(()).expect("release pre-begin barrier");
        assert!(!owner_finished_rx.await.expect("owner reports admission"));
        service
            .wait_for_shutdown()
            .await
            .expect("shutdown sees no admitted card operation");
        assert!(
            service.current().is_none(),
            "no durable operation was committed"
        );
    }

    #[tokio::test]
    async fn shutdown_module_finalizer_waits_for_accepted_esim_owner_drain() {
        let service = Arc::new(Service::default());
        let permit = service.begin().expect("admit accepted eSIM operation");
        let operation_id = permit.id.clone();
        service.begin_shutdown();

        let module_service = service.clone();
        let (finalizer_started_tx, mut finalizer_started_rx) = tokio::sync::oneshot::channel();
        let module = async move {
            let mut report = crate::shutdown::Report::default();
            report.check(
                "eSIM operation drain",
                module_service.wait_for_shutdown().await,
            );
            // This signal stands for the module radio final receipt and must
            // remain behind the exact accepted eSIM owner's terminal write.
            let _ = finalizer_started_tx.send(());
            report
        };
        let shutdown = tokio::spawn(crate::shutdown::coordinate(
            crate::shutdown::Report::default(),
            async { crate::shutdown::Report::default() },
            module,
            async { crate::shutdown::Report::default() },
            async { crate::shutdown::Report::default() },
            async { crate::shutdown::Report::default() },
        ));

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(finalizer_started_rx.try_recv().is_err());
        assert!(
            !shutdown.is_finished(),
            "shutdown must retain the accepted eSIM owner"
        );

        service.finish(&operation_id, Ok(DownloadResult::default()));
        drop(permit);
        finalizer_started_rx
            .await
            .expect("module finalizer starts after eSIM drain");
        let report = shutdown.await.expect("shutdown coordinator remains alive");
        assert!(report.finish().is_ok());
        assert_eq!(service.current().unwrap().state, "succeeded");
    }

    #[test]
    fn profile_mutations_share_busy_journal_and_reconciliation_admission() {
        let service = Service::default();
        let owner = service.begin().expect("existing operation owns gate");
        let denied = acquire_profile_mutation_gate(&service).expect_err("busy gate");
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        service.finish(&owner.id, Ok(DownloadResult::default()));
        drop(owner);
        let admitted = acquire_profile_mutation_gate(&service).expect("gate released");
        drop(admitted);

        service.journal_faulted.store(true, Ordering::Release);
        let denied = acquire_profile_mutation_gate(&service).expect_err("faulted journal");
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            !service.operation_gate.load(Ordering::Acquire),
            "failed admission must release its provisional gate"
        );
        service.journal_faulted.store(false, Ordering::Release);
        drop(acquire_profile_mutation_gate(&service).expect("recovered gate"));

        let fixture = JournalFixture::new();
        let original = Service::open(&fixture.0).unwrap();
        let in_flight = original.begin().unwrap();
        assert!(original.update(&in_flight.id, "running", "install", "installing", 80));
        drop(in_flight);
        drop(original);
        let recovered = Service::open(&fixture.0).unwrap();
        let denied = acquire_profile_mutation_gate(&recovered)
            .expect_err("unreconciled previous card write");
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        assert!(
            !recovered.operation_gate.load(Ordering::Acquire),
            "reconciliation denial must not leave the operation gate held"
        );
    }

    #[tokio::test]
    async fn denied_profile_mutation_never_reaches_fake_apdu_and_admitted_work_holds_gate() {
        let service = Arc::new(Service::default());
        let apdu_calls = Arc::new(AtomicU64::new(0));
        let owner = service.begin().expect("a prior eSIM write owns the gate");
        let denied_calls = apdu_calls.clone();
        let denied = with_profile_mutation_gate(&service, || async move {
            denied_calls.fetch_add(1, Ordering::SeqCst);
            json_response(StatusCode::OK, &json!({"apdu":"must not run"}))
        })
        .await;
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        assert_eq!(apdu_calls.load(Ordering::SeqCst), 0);

        service.finish(&owner.id, Ok(DownloadResult::default()));
        drop(owner);
        service.journal_faulted.store(true, Ordering::Release);
        let denied_calls = apdu_calls.clone();
        let denied = with_profile_mutation_gate(&service, || async move {
            denied_calls.fetch_add(1, Ordering::SeqCst);
            json_response(StatusCode::OK, &json!({"apdu":"must not run"}))
        })
        .await;
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(apdu_calls.load(Ordering::SeqCst), 0);
        service.journal_faulted.store(false, Ordering::Release);

        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let operation_service = service.clone();
        let operation_calls = apdu_calls.clone();
        let operation = tokio::spawn(async move {
            with_profile_mutation_gate(&operation_service, || async move {
                entered.send(()).unwrap();
                release_rx.await.unwrap();
                operation_calls.fetch_add(1, Ordering::SeqCst);
                json_response(StatusCode::OK, &json!({"apdu":"completed"}))
            })
            .await
        });
        entered_rx.await.expect("admitted mutation starts");
        let concurrent = acquire_profile_mutation_gate(&service)
            .expect_err("permit remains held through the physical operation");
        assert_eq!(concurrent.status(), StatusCode::CONFLICT);
        release.send(()).unwrap();
        assert_eq!(operation.await.unwrap().status(), StatusCode::OK);
        assert_eq!(apdu_calls.load(Ordering::SeqCst), 1);
        drop(acquire_profile_mutation_gate(&service).expect("permit released after completion"));
    }

    #[tokio::test]
    async fn cancelling_profile_mutation_waiter_keeps_retained_apdu_owner_and_gate() {
        let service = Arc::new(Service::default());
        let apdu_calls = Arc::new(AtomicU64::new(0));
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let (completed, completed_rx) = tokio::sync::oneshot::channel();
        let task_service = service.clone();
        let task_calls = apdu_calls.clone();
        let waiter = tokio::spawn(async move {
            with_profile_mutation_gate(&task_service, || async move {
                entered.send(()).unwrap();
                release_rx.await.unwrap();
                task_calls.fetch_add(1, Ordering::SeqCst);
                completed.send(()).unwrap();
                json_response(StatusCode::OK, &json!({"apdu":"completed"}))
            })
            .await
        });
        entered_rx.await.expect("retained card work was accepted");
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        let busy = acquire_profile_mutation_gate(&service)
            .expect_err("HTTP waiter cancellation must not release the operation gate");
        assert_eq!(busy.status(), StatusCode::CONFLICT);
        assert_eq!(apdu_calls.load(Ordering::SeqCst), 0);

        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), completed_rx)
            .await
            .expect("retained physical operation completes")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(permit) = acquire_profile_mutation_gate(&service) {
                    drop(permit);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("operation gate releases after the retained owner finishes");
        assert_eq!(apdu_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reader_reconciliation_keeps_unknown_on_partial_or_replaced_card() {
        let fixture = JournalFixture::new();
        let initial = Service::open(&fixture.0).unwrap();
        let permit = initial.begin().unwrap();
        assert!(initial.update(&permit.id, "running", "install", "installing", 80));
        drop(permit);
        drop(initial);
        let operations = Service::open(&fixture.0).unwrap();
        let gate = crate::reader_reservation::Gate::default();
        gate.test_seed_selection("Reader A", 1);
        let mut overview = ReaderOverview {
            eids: vec![json!({"eid":"synthetic-eid"})],
            profiles: vec![json!({"read_state":"error", "profiles":[]})],
            product: None,
            card_identities: Vec::new(),
        };
        let original = fs::read(fixture.0.join("reader-esim-operation.json")).unwrap();
        assert_eq!(
            reconcile_reader_overview(&operations, &gate, "Reader A", 1, &overview),
            Ok(true)
        );
        assert!(matches!(
            operations.begin(),
            Err(BeginError::ReconciliationRequired)
        ));
        assert_eq!(
            fs::read(fixture.0.join("reader-esim-operation.json")).unwrap(),
            original
        );
        overview.profiles[0]["read_state"] = json!("ready");
        gate.test_seed_selection("Reader A", 2);
        assert_eq!(
            reconcile_reader_overview(&operations, &gate, "Reader A", 1, &overview),
            Err("reader_generation_changed")
        );
        assert!(matches!(
            operations.begin(),
            Err(BeginError::ReconciliationRequired)
        ));
        assert_eq!(
            fs::read(fixture.0.join("reader-esim-operation.json")).unwrap(),
            original
        );
        assert_eq!(
            reconcile_reader_overview(&operations, &gate, "Reader A", 2, &overview),
            Ok(true)
        );
        assert!(operations.current().unwrap().recoverable);
        drop(operations);
        let recovered = Service::open(&fixture.0).unwrap();
        assert!(
            recovered.begin().is_ok(),
            "complete reconciliation must be durable"
        );
    }

    #[test]
    fn complete_other_card_overview_cannot_reconcile_live_reader_operation() {
        let fixture = JournalFixture::new();
        let operations = Service::open(&fixture.0).unwrap();
        let permit = operations.begin().unwrap();
        assert!(operations.bind_reader_owner(&permit.id, "Reader A", 7));
        assert!(operations.update(&permit.id, "running", "install", "installing", 80));
        drop(permit);
        assert!(!operations.current().unwrap().recoverable);
        let original = fs::read(fixture.0.join("reader-esim-operation.json")).unwrap();
        let overview = ReaderOverview {
            eids: vec![json!({"eid":"other-card-eid"})],
            profiles: vec![json!({"read_state":"ready", "profiles":[]})],
            product: None,
            card_identities: Vec::new(),
        };
        let gate = crate::reader_reservation::Gate::default();
        gate.test_seed_selection("Reader B", 8);
        assert_eq!(
            reconcile_reader_overview(&operations, &gate, "Reader B", 8, &overview),
            Ok(true)
        );
        assert!(!operations.current().unwrap().recoverable);
        assert!(matches!(
            operations.begin(),
            Err(BeginError::ReconciliationRequired)
        ));
        assert_eq!(
            fs::read(fixture.0.join("reader-esim-operation.json")).unwrap(),
            original
        );

        gate.test_seed_selection("Reader A", 9);
        assert_eq!(
            reconcile_reader_overview(&operations, &gate, "Reader A", 9, &overview),
            Ok(true)
        );
        assert!(!operations.current().unwrap().recoverable);
    }

    #[test]
    fn complete_owner_card_overview_reconciles_same_reader_generation() {
        let fixture = JournalFixture::new();
        let operations = Service::open(&fixture.0).unwrap();
        let permit = operations.begin().unwrap();
        assert!(operations.bind_reader_owner(&permit.id, "Reader A", 7));
        assert!(operations.update(&permit.id, "running", "install", "installing", 80));
        drop(permit);
        let overview = ReaderOverview {
            eids: vec![json!({"eid":"owner-card-eid"})],
            profiles: vec![json!({"read_state":"ready", "profiles":[]})],
            product: None,
            card_identities: Vec::new(),
        };
        let gate = crate::reader_reservation::Gate::default();
        gate.test_seed_selection("Reader A", 7);
        assert_eq!(
            reconcile_reader_overview(&operations, &gate, "Reader A", 7, &overview),
            Ok(true)
        );
        assert!(operations.current().unwrap().recoverable);
        assert!(operations.begin().is_ok());
    }

    #[test]
    fn armed_card_proof_survives_restart_and_rejects_other_card_even_same_generation() {
        let fixture = JournalFixture::new();
        let aid = KNOWN_ISDR_APPLICATION_AIDS[0];
        let eid_a = [0x12u8; 16];
        let eid_b = [0x34u8; 16];
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        assert!(service.bind_reader_owner(&permit.id, "Reader A", 7));
        service.arm_card_proof(&permit.id, &eid_a, aid).unwrap();
        drop(permit);
        drop(service);

        let journal_path = fixture.0.join("reader-esim-operation.json");
        let original = fs::read(&journal_path).unwrap();
        let journal_text = String::from_utf8(original.clone()).unwrap();
        assert!(!journal_text.contains(&encode_hex_lower(&eid_a)));
        assert!(!journal_text.contains(&encode_hex_lower(aid)));
        assert!(!journal_text.contains("key_hex"));

        let recovered = Service::open(&fixture.0).unwrap();
        assert!(!recovered.current().unwrap().recoverable);
        assert!(recovered.reconcile_after_overview_refresh(
            "Reader A",
            7,
            &[(eid_b.to_vec(), aid.to_vec())]
        ));
        assert!(!recovered.current().unwrap().recoverable);
        let mut wrong_aid = aid.to_vec();
        wrong_aid[0] ^= 1;
        assert!(recovered.reconcile_after_overview_refresh(
            "Reader A",
            7,
            &[(eid_a.to_vec(), wrong_aid)]
        ));
        assert!(!recovered.current().unwrap().recoverable);
        assert_eq!(fs::read(&journal_path).unwrap(), original);
        assert!(matches!(
            recovered.begin(),
            Err(BeginError::ReconciliationRequired)
        ));

        // The durable proof, rather than reader name or process generation,
        // permits the exact card to be recognized in another reader.
        assert!(recovered.reconcile_after_overview_refresh(
            "Reader C",
            99,
            &[(eid_a.to_vec(), aid.to_vec())]
        ));
        assert!(recovered.current().unwrap().recoverable);
        assert!(recovered.begin().is_ok());
    }

    #[test]
    fn armed_journal_key_loss_or_corruption_fails_closed_without_regeneration() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin_switch("89882110000000000001").unwrap();
        service
            .arm_card_proof(&permit.id, &[0x12; 16], KNOWN_ISDR_APPLICATION_AIDS[0])
            .unwrap();
        drop(permit);
        drop(service);
        let journal_path = fixture.0.join("reader-esim-operation.json");
        let original = fs::read(&journal_path).unwrap();
        let key_path = fixture.0.join("reader-esim-card-proof-key.json");
        fs::remove_file(&key_path).unwrap();
        assert!(Service::open(&fixture.0).is_err());
        assert!(!key_path.exists(), "missing key must not be replaced");
        assert_eq!(fs::read(&journal_path).unwrap(), original);

        ArchiveStore::new(&key_path)
            .replace(vec![json!({"schema_version":1,"key_hex":"broken"})])
            .unwrap();
        assert!(Service::open(&fixture.0).is_err());
        assert_eq!(fs::read(&journal_path).unwrap(), original);
    }

    #[test]
    fn existing_versioned_journal_rejects_missing_empty_or_null_key() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        drop(permit);
        drop(service);
        let journal_path = fixture.0.join("reader-esim-operation.json");
        let original = fs::read(&journal_path).unwrap();
        let key_path = fixture.0.join("reader-esim-card-proof-key.json");
        fs::remove_file(&key_path).unwrap();
        assert!(Service::open(&fixture.0).is_err());
        assert!(!key_path.exists(), "missing key must not be regenerated");
        for corrupt in [b"".as_slice(), b"null".as_slice(), b"[]".as_slice()] {
            fs::write(&key_path, corrupt).unwrap();
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(Service::open(&fixture.0).is_err());
            assert_eq!(fs::read(&key_path).unwrap(), corrupt);
            assert_eq!(fs::read(&journal_path).unwrap(), original);
        }
    }

    #[test]
    fn present_empty_or_null_operation_journal_fails_closed() {
        for corrupt in [
            b"".as_slice(),
            b"null".as_slice(),
            b" \n null \t".as_slice(),
            b"[]".as_slice(),
            b"{}".as_slice(),
            b"false".as_slice(),
            b"0".as_slice(),
            b"[{}]".as_slice(),
            b"[null]".as_slice(),
        ] {
            let fixture = JournalFixture::new();
            let service = Service::open(&fixture.0).unwrap();
            let permit = service.begin().unwrap();
            drop(permit);
            drop(service);
            let journal_path = fixture.0.join("reader-esim-operation.json");
            fs::write(&journal_path, corrupt).unwrap();
            assert!(Service::open(&fixture.0).is_err());
            assert_eq!(fs::read(&journal_path).unwrap(), corrupt);
        }
    }

    #[test]
    fn proof_write_failure_stops_before_card_mutation() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        let journal_path = fixture.0.join("reader-esim-operation.json");
        fs::remove_file(&journal_path).unwrap();
        fs::create_dir(&journal_path).unwrap();
        assert!(
            service
                .arm_card_proof(&permit.id, &[0x12; 16], KNOWN_ISDR_APPLICATION_AIDS[0])
                .is_err()
        );
        assert!(service.journal_faulted.load(Ordering::Acquire));
        let state = service.state.lock().unwrap();
        assert!(matches!(state.as_ref().unwrap().proof, CardProof::NotArmed));
        drop(state);
        drop(permit);
    }

    #[test]
    fn legacy_ownerless_unknown_requires_exact_manual_acknowledgement() {
        let fixture = JournalFixture::new();
        let initial = Service::open(&fixture.0).unwrap();
        let permit = initial.begin().unwrap();
        drop(permit);
        let legacy = initial.current().unwrap();
        drop(initial);
        let journal_path = fixture.0.join("reader-esim-operation.json");
        ArchiveStore::new(&journal_path)
            .replace(vec![serde_json::to_value(&legacy).unwrap()])
            .unwrap();
        let recovered = Service::open(&fixture.0).unwrap();
        assert!(recovered.manual_ack_available());
        assert!(recovered.reconcile_after_overview_refresh(
            "Reader A",
            1,
            &[([0x12; 16].to_vec(), KNOWN_ISDR_APPLICATION_AIDS[0].to_vec())]
        ));
        assert!(!recovered.current().unwrap().recoverable);
        let request = AcknowledgeLegacyRequest {
            operation_id: legacy.id.clone(),
            expected_updated_at: legacy.updated_at.clone(),
            expected_state: "failed".into(),
            expected_error_code: "esim_operation_interrupted".into(),
            legacy_manual_ack: true,
        };
        let before = fs::read(&journal_path).unwrap();
        assert!(matches!(
            recovered.acknowledge_legacy(AcknowledgeLegacyRequest {
                expected_updated_at: "stale".into(),
                ..request.clone()
            }),
            Err(AcknowledgeError::Conflict)
        ));
        assert_eq!(fs::read(&journal_path).unwrap(), before);
        assert!(recovered.acknowledge_legacy(request).unwrap().recoverable);
        assert!(!recovered.manual_ack_available());
        assert!(recovered.begin().is_ok());
    }

    #[test]
    fn terminal_journal_state_survives_service_restart() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        let operation_id = permit.id.clone();
        service.finish(&operation_id, Ok(DownloadResult::default()));
        drop(permit);
        drop(service);

        let recovered = Service::open(&fixture.0).unwrap();
        let snapshot = recovered.current().unwrap();
        assert_eq!(snapshot.id, operation_id);
        assert_eq!(snapshot.state, "succeeded");
        assert_eq!(snapshot.step.as_deref(), Some("done"));
    }

    #[test]
    fn only_one_reader_profile_download_can_be_active() {
        let service = Service::default();
        let _permit = service.begin().unwrap();
        assert!(service.begin().is_err());
    }

    #[test]
    fn reader_download_gate_stays_held_until_owner_cleanup_finishes() {
        let service = Service::default();
        let permit = service.begin().unwrap();
        let id = permit.id.clone();
        let operation_gate = permit._operation_gate;

        // The operation journal can become terminal before the detached owner
        // completes lease release and its final reader refresh.
        service.finish(&id, Ok(DownloadResult::default()));
        assert!(matches!(
            service.begin_module_operation(),
            Err(ModuleOperationAdmissionError::Busy)
        ));

        drop(operation_gate);
        assert!(service.begin_module_operation().is_ok());
    }

    #[test]
    fn module_mutation_and_reader_download_or_switch_share_one_operation_gate() {
        let service = Service::default();
        let module_permit = service.begin_module_operation().unwrap();
        assert!(service.active());
        assert!(matches!(service.begin(), Err(BeginError::Busy)));
        assert!(matches!(
            service.begin_switch("8985200014631959072"),
            Err(BeginError::Busy)
        ));
        assert!(matches!(
            service.begin_module_operation(),
            Err(ModuleOperationAdmissionError::Busy)
        ));
        drop(module_permit);

        let download = service.begin().unwrap();
        assert!(matches!(
            service.begin_module_operation(),
            Err(ModuleOperationAdmissionError::Busy)
        ));
        let download_id = download.id.clone();
        service.finish(&download_id, Ok(DownloadResult::default()));
        drop(download);

        let switch = service.begin_switch("8985200014631959072").unwrap();
        assert!(matches!(
            service.begin_module_operation(),
            Err(ModuleOperationAdmissionError::Busy)
        ));
        service.finish_switch(&switch.id, Err(DownloadFailure::Cancelled));
        drop(switch);

        assert!(!service.active());
        assert!(service.begin_module_operation().is_ok());
    }

    #[test]
    fn module_download_request_rejects_unknown_fields_and_keeps_legacy_fields() {
        let valid: DownloadRequest = serde_json::from_value(json!({
            "smdp": "example.smdp.test",
            "matching_id": "match-1",
            "confirmation_code": "confirm-1",
            "aid": "A0000005591010FFFFFFFF8900000100",
            "imei": "353837410000006"
        }))
        .unwrap();
        assert_eq!(valid.smdp, "example.smdp.test");
        assert!(
            serde_json::from_value::<DownloadRequest>(json!({
                "smdp": "example.smdp.test",
                "unexpected": true
            }))
            .is_err()
        );
    }

    #[test]
    fn parse_download_request_clears_sensitive_input_fields() {
        let (activation, _, imei, matching_id, confirmation) = prepare_request(DownloadRequest {
            client_request_id: String::new(),
            smdp: "https://smdp.example".into(),
            matching_id: "secret-match".into(),
            confirmation_code: "secret-code".into(),
            aid: "A0000005591010".into(),
            imei: "".into(),
        })
        .unwrap();
        assert_eq!(activation.server_address(), "smdp.example");
        assert_eq!(imei.as_str(), "35383741");
        assert_eq!(matching_id.as_str(), "secret-match");
        assert_eq!(confirmation.as_str(), "secret-code");
    }

    #[test]
    fn transport_generic_download_keeps_public_failure_projection_offline() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).expect("open isolated journal");
        let permit = service.begin().expect("start isolated operation");
        let (activation, aid, imei, matching_id, confirmation_code) =
            prepare_request(DownloadRequest {
                client_request_id: String::new(),
                smdp: "smdp.example".into(),
                matching_id: "fixture-match".into(),
                confirmation_code: String::new(),
                aid: "A0000005591010".into(),
                imei: String::new(),
            })
            .expect("valid fixture request");
        let mut transport = FailingDownloadTransport::default();

        let result = run_download_on_transport(
            &mut transport,
            activation,
            &imei,
            aid,
            &matching_id,
            &confirmation_code,
            permit.confirmation,
            permit.cancelled,
            &service,
            &permit.id,
        );

        assert_eq!(transport.commands.len(), 1);
        assert!(matches!(
            &result,
            Err(DownloadFailure::Failed { code, message, .. })
                if code == "download_failed" && message == "无法读取目标 eUICC 信息"
        ));
        service.finish(&permit.id, result);
        let snapshot = service.current().expect("record the failure");
        assert_eq!(snapshot.state, "failed");
        assert_eq!(snapshot.error_code.as_deref(), Some("download_failed"));
        assert!(snapshot.recoverable);
        drop(service);
        let recovered = Service::open(&fixture.0).expect("reload isolated journal");
        let snapshot = recovered.current().expect("recover the failure state");
        assert_eq!(snapshot.state, "failed");
        assert_eq!(snapshot.error_code.as_deref(), Some("download_failed"));
        assert!(snapshot.recoverable);
    }

    fn scripted_eid_response(eid: [u8; 16]) -> Vec<u8> {
        let mut response = vec![0xbf, 0x3e, 0x12, 0x5a, 0x10];
        response.extend_from_slice(&eid);
        response.extend_from_slice(&[0x90, 0x00]);
        response
    }

    fn install_probe_transport(eid: [u8; 16]) -> FakeHealthTransport {
        FakeHealthTransport {
            responses: VecDeque::from(vec![
                vec![0x01, 0x90, 0x00], // MANAGE CHANNEL
                vec![0x90, 0x00],       // SELECT ISD-R
                scripted_eid_response(eid),
                vec![0x90, 0x00], // CLOSE CHANNEL
            ]),
            commands: Vec::new(),
        }
    }

    #[test]
    fn switch_proof_persist_failure_stops_before_enable_profile_apdu() {
        fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
            let mut encoded = Vec::with_capacity(tag.len() + value.len() + 1);
            encoded.extend_from_slice(tag);
            encoded.push(value.len() as u8);
            encoded.extend_from_slice(value);
            encoded
        }

        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let iccid = "8985200014631959072";
        let permit = service.begin_switch(iccid).unwrap();
        let mut profile_fields = tlv(&[0x5a], &encode_iccid(iccid).unwrap());
        profile_fields.extend_from_slice(&tlv(&[0x9f, 0x70], &[0x00])); // disabled
        let profile = tlv(&[0xe3], &profile_fields);
        let mut list = tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profile));
        list.extend_from_slice(&[0x90, 0x00]);
        let mut transport = FakeHealthTransport {
            responses: VecDeque::from(vec![
                vec![0x01, 0x90, 0x00],
                vec![0x90, 0x00],
                list,
                scripted_eid_response([0x12; 16]),
                vec![0x90, 0x00], // Close logical channel after proof failure.
            ]),
            commands: Vec::new(),
        };
        let journal_path = fixture.0.join("reader-esim-operation.json");
        fs::remove_file(&journal_path).unwrap();
        fs::create_dir(&journal_path).unwrap();
        let result = switch_profile_on_transport_with_proof(
            &mut transport,
            iccid,
            Some(KNOWN_ISDR_APPLICATION_AIDS[0]),
            Arc::new(AtomicBool::new(false)),
            Some((&service, &permit.id)),
        );
        assert!(matches!(
            result,
            Err(SwitchFailure::Operation(message))
                if message == "switch recovery proof could not be persisted"
        ));
        assert_eq!(transport.commands.len(), 5);
        assert!(
            transport
                .commands
                .iter()
                .any(|command| command.windows(2).any(|tag| tag == [0xbf, 0x2d]))
        );
        assert!(
            transport
                .commands
                .iter()
                .any(|command| command.windows(2).any(|tag| tag == [0xbf, 0x3e]))
        );
        assert!(
            transport
                .commands
                .iter()
                .all(|command| !command.windows(2).any(|tag| tag == [0xbf, 0x31]))
        );
        assert!(matches!(
            service.state.lock().unwrap().as_ref().unwrap().proof,
            CardProof::NotArmed
        ));
    }

    #[test]
    fn install_channel_card_swap_stops_before_authenticate_or_load() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        let (activation, aid, imei, _, _) = prepare_request(DownloadRequest {
            client_request_id: String::new(),
            smdp: "smdp.example".into(),
            matching_id: "fixture-match".into(),
            confirmation_code: String::new(),
            aid: encode_hex_upper(KNOWN_ISDR_APPLICATION_AIDS[0]),
            imei: String::new(),
        })
        .unwrap();
        let mut transport = install_probe_transport([0x34; 16]);
        let mut observer = ReaderDownloadObserver {
            service: &service,
            id: &permit.id,
            confirmation: permit.confirmation,
            cancelled: permit.cancelled,
            matching_id: "fixture-match",
            confirmation_code: "",
            stage: "init_auth",
        };
        let result = install_profile(
            &mut transport,
            aid.as_deref().unwrap(),
            &format_eid(&[0x12; 16]).unwrap(),
            &activation,
            &imei,
            None,
            &mut observer,
        );
        assert!(matches!(
            result,
            Err(DownloadFailure::Failed { code, .. }) if code == "esim_card_identity_changed"
        ));
        assert_eq!(
            transport.commands.len(),
            4,
            "no authentication or package APDU"
        );
        assert!(matches!(
            service.state.lock().unwrap().as_ref().unwrap().proof,
            CardProof::NotArmed
        ));
    }

    #[test]
    fn install_channel_proof_persist_failure_stops_before_authenticate_or_load() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let permit = service.begin().unwrap();
        let (activation, aid, imei, _, _) = prepare_request(DownloadRequest {
            client_request_id: String::new(),
            smdp: "smdp.example".into(),
            matching_id: "fixture-match".into(),
            confirmation_code: String::new(),
            aid: encode_hex_upper(KNOWN_ISDR_APPLICATION_AIDS[0]),
            imei: String::new(),
        })
        .unwrap();
        let journal_path = fixture.0.join("reader-esim-operation.json");
        fs::remove_file(&journal_path).unwrap();
        fs::create_dir(&journal_path).unwrap();
        let mut transport = install_probe_transport([0x12; 16]);
        let mut observer = ReaderDownloadObserver {
            service: &service,
            id: &permit.id,
            confirmation: permit.confirmation,
            cancelled: permit.cancelled,
            matching_id: "fixture-match",
            confirmation_code: "",
            stage: "init_auth",
        };
        let result = install_profile(
            &mut transport,
            aid.as_deref().unwrap(),
            &format_eid(&[0x12; 16]).unwrap(),
            &activation,
            &imei,
            None,
            &mut observer,
        );
        assert!(matches!(
            result,
            Err(DownloadFailure::Failed { code, .. }) if code == "esim_operation_journal_failed"
        ));
        assert_eq!(
            transport.commands.len(),
            4,
            "no authentication or package APDU"
        );
        assert!(matches!(
            service.state.lock().unwrap().as_ref().unwrap().proof,
            CardProof::NotArmed
        ));
    }

    #[test]
    fn ble_download_bridge_binds_apdu_to_reader_and_preserves_failure_contract() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).expect("open isolated journal");
        let permit = service.begin().expect("start isolated operation");
        let (activation, aid, imei, matching_id, confirmation_code) =
            prepare_request(DownloadRequest {
                client_request_id: String::new(),
                smdp: "smdp.example".into(),
                matching_id: "fixture-match".into(),
                confirmation_code: "fixture-confirmation".into(),
                aid: "A0000005591010".into(),
                imei: String::new(),
            })
            .expect("valid fixture request");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = std::env::temp_dir().join(format!(
            "switch-ble-download-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).expect("bind fake BLE bridge");
        let reader = "BLE:12345678-1234-1234-1234-123456789abc";
        let token = "offline-bridge-token-0123456789abcdef";
        let expected_reader = reader.to_owned();
        let expected_token = token.to_owned();
        let bridge = thread::spawn(move || {
            for action in ["status", "apdu"] {
                let (mut stream, _) = listener.accept().expect("accept bridge action");
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .expect("read bridge request");
                let request: Value = serde_json::from_str(&line).expect("decode request");
                assert_eq!(request["token"], expected_token);
                assert_eq!(request["action"], action);
                if action == "status" {
                    assert!(request.get("reader").is_none());
                    stream
                        .write_all(
                            format!(
                                "{{\"ok\":true,\"reader\":{},\"state\":\"connected\",\"operation_tracking\":true,\"bridge_id\":\"fixture-bridge\"}}\n",
                                serde_json::to_string(&expected_reader).unwrap()
                            )
                            .as_bytes(),
                        )
                        .unwrap();
                } else {
                    assert_eq!(request["reader"], expected_reader);
                    assert!(!request["hex"].as_str().unwrap().is_empty());
                    stream
                        .write_all(
                            ble_terminal_reply(
                                &request,
                                json!({"ok":false,"error_code":"fixture_apdu_failure"}),
                            )
                            .as_bytes(),
                        )
                        .unwrap();
                }
            }
        });
        let mut transport = crate::reader_ble_apdu::BleApduTransport::connect_for_test(
            Some(&socket),
            Some(token),
            reader,
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .expect("bind fake bridge to selected BLE reader");

        let result = run_download_on_transport(
            &mut transport,
            activation,
            &imei,
            aid,
            &matching_id,
            &confirmation_code,
            permit.confirmation,
            permit.cancelled,
            &service,
            &permit.id,
        );
        bridge.join().expect("complete fake BLE bridge exchange");
        fs::remove_file(&socket).unwrap();

        assert!(matches!(
            &result,
            Err(DownloadFailure::Failed { code, message, diagnostic })
                if code == "download_failed"
                    && message == "无法读取目标 eUICC 信息"
                    && diagnostic.contains("fixture_apdu_failure")
                    && !diagnostic.contains("fixture-match")
                    && !diagnostic.contains("fixture-confirmation")
        ));
        service.finish(&permit.id, result);
        let snapshot = service.current().expect("record the failure");
        assert_eq!(snapshot.state, "failed");
        assert_eq!(snapshot.error_code.as_deref(), Some("download_failed"));
        assert!(snapshot.recoverable);
    }

    #[test]
    fn deletion_notification_requires_new_sequence_matching_iccid_and_delete_event() {
        let target = [0x89, 0x98, 0x21];
        let notifications = [
            NotificationInfo {
                sequence_number: 8,
                event: 3,
                iccid: target.to_vec(),
                ..NotificationInfo::default()
            },
            NotificationInfo {
                sequence_number: 10,
                event: 1,
                iccid: target.to_vec(),
                ..NotificationInfo::default()
            },
            NotificationInfo {
                sequence_number: 11,
                event: 3,
                iccid: vec![0x89, 0x98, 0x22],
                ..NotificationInfo::default()
            },
            NotificationInfo {
                sequence_number: 12,
                event: 3,
                iccid: target.to_vec(),
                ..NotificationInfo::default()
            },
        ];

        assert_eq!(max_notification_sequence(&notifications), 12);
        assert_eq!(
            find_delete_notification(&notifications, 8, &target),
            Some(12)
        );
        assert_eq!(find_delete_notification(&notifications, 12, &target), None);
    }

    #[test]
    fn delete_response_preserves_warning_codes_and_space_delta_semantics() {
        assert_eq!(
            deletion_space_delta(Some(1024), Some(2048))
                .map(|delta| (delta.direction, delta.bytes)),
            Some(("released".into(), 1024))
        );
        assert!(deletion_space_delta(Some(2048), Some(1024)).is_none());
        assert!(deletion_space_delta(None, Some(4096)).is_none());
        assert!(deletion_space_delta(Some(1), Some(u64::MAX)).is_none());

        let result = DeleteProfileResult {
            warning: Some("Profile 已删除，但删除通知发送未完全确认".into()),
            warning_code: Some("delete_notification_handle_failed".into()),
            space_delta: None,
        };
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json["warning_code"], "delete_notification_handle_failed");
        assert!(json.get("space_delta").is_none());
    }

    #[test]
    fn reader_post_write_finalization_refresh_warning_is_journaled() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).expect("open isolated journal");
        let permit = service.begin().expect("begin isolated operation");
        let mut result = Ok(DownloadResult::default());

        apply_reader_finalization(&mut result, Err("reader_lifecycle_unavailable"));
        service.finish(&permit.id, result);

        let snapshot = service.current().expect("record finalization warning");
        assert_eq!(snapshot.state, "warning");
        assert_eq!(
            snapshot.message.as_deref(),
            Some("Profile 已安装，但部分收尾状态尚未确认")
        );
        let warning = snapshot.result.expect("include warning result");
        assert!(
            warning
                .warning
                .as_deref()
                .unwrap()
                .contains("读卡器状态刷新未确认")
        );
        assert_eq!(
            warning.warning_code.as_deref(),
            Some("reader_profile_refresh_unconfirmed")
        );

        drop(service);
        let recovered = Service::open(&fixture.0).expect("recover isolated journal");
        let recovered = recovered.current().expect("recover warning state");
        assert_eq!(recovered.state, "warning");
        assert_eq!(
            recovered.result.unwrap().warning_code.as_deref(),
            Some("reader_profile_refresh_unconfirmed")
        );
    }

    #[test]
    fn reader_post_write_finalization_preserves_primary_download_failure() {
        let mut result = Err(DownloadFailure::Failed {
            code: "download_failed".into(),
            message: "下载失败".into(),
            diagnostic: "card operation failed".into(),
        });

        apply_reader_finalization(&mut result, Err("reader_refresh_unconfirmed"));

        assert!(matches!(
            result,
            Err(DownloadFailure::Failed { code, diagnostic, .. })
                if code == "download_failed"
                    && diagnostic.contains("reader_refresh_unconfirmed")
        ));
    }

    #[test]
    fn reader_post_write_finalization_does_not_report_unconfirmed_cancel_as_clean() {
        let mut result = Err(DownloadFailure::Cancelled);

        apply_reader_finalization(&mut result, Err("reader_session_lease_release_failed"));

        assert!(matches!(
            result,
            Err(DownloadFailure::Failed { code, message, .. })
                if code == "reader_session_lease_release_failed"
                    && message.contains("读卡器最终状态未确认")
        ));
    }

    #[test]
    fn delete_request_rejects_invalid_iccid_and_aid_before_card_access() {
        assert_eq!(
            validate_delete_request(DeleteProfileRequest {
                client_request_id: String::new(),
                iccid: String::new(),
                aid: String::new(),
            })
            .unwrap_err()
            .0,
            "invalid_iccid"
        );
        assert_eq!(
            validate_delete_request(DeleteProfileRequest {
                client_request_id: String::new(),
                iccid: "89882110000000000002".into(),
                aid: "A0001".into(),
            })
            .unwrap_err()
            .0,
            "invalid_aid_hex"
        );
    }

    #[test]
    fn switch_request_validates_iccid_and_hex_aid_before_card_access() {
        let (iccid, aid) = validate_switch_request(SwitchProfileRequest {
            client_request_id: String::new(),
            iccid: " 8985200014631959072 ".into(),
            aid: " A0000005591010 ".into(),
        })
        .unwrap();
        assert_eq!(iccid, "8985200014631959072");
        assert_eq!(aid.unwrap(), [0xa0, 0, 0, 5, 0x59, 0x10, 0x10]);

        assert_eq!(
            validate_switch_request(SwitchProfileRequest {
                client_request_id: String::new(),
                iccid: "not-an-iccid".into(),
                aid: String::new(),
            })
            .unwrap_err()
            .0,
            "invalid_iccid"
        );
        assert_eq!(
            validate_switch_request(SwitchProfileRequest {
                client_request_id: String::new(),
                iccid: "8985200014631959072".into(),
                aid: "A0001".into(),
            })
            .unwrap_err()
            .0,
            "invalid_aid_hex"
        );
    }

    #[test]
    fn profile_switch_retries_cat_busy_three_times_then_accepts_enable() {
        fn response(result: u8) -> Vec<u8> {
            vec![0xbf, 0x31, 0x03, 0x80, 0x01, result, 0x90, 0x00]
        }

        let fake = FakeSwitchCard {
            responses: VecDeque::from([response(5), response(5), response(5), response(0)]),
            commands: 0,
        };
        let mut card = Euicc::new(fake, 1, READER_MSS).unwrap();
        let iccid = encode_iccid("8985200014631959072").unwrap();
        let mut waits = 0;
        enable_profile_with_retry_and_wait(
            &mut card,
            ProfileIdentifier::Iccid(&iccid),
            &AtomicBool::new(false),
            || waits += 1,
        )
        .unwrap();

        let fake = card.into_inner();
        assert_eq!(fake.commands, 4);
        assert_eq!(waits, 3);
        assert!(fake.responses.is_empty());
    }

    #[test]
    fn profile_switch_stops_retrying_when_reader_lease_is_lost() {
        fn cat_busy_response() -> Vec<u8> {
            vec![0xbf, 0x31, 0x03, 0x80, 0x01, 5, 0x90, 0x00]
        }

        let fake = FakeSwitchCard {
            responses: VecDeque::from([cat_busy_response()]),
            commands: 0,
        };
        let mut card = Euicc::new(fake, 1, READER_MSS).unwrap();
        let iccid = encode_iccid("8985200014631959072").unwrap();
        let cancelled = AtomicBool::new(false);
        let error = enable_profile_with_retry_and_wait(
            &mut card,
            ProfileIdentifier::Iccid(&iccid),
            &cancelled,
            || cancelled.store(true, Ordering::Release),
        )
        .unwrap_err();

        assert!(matches!(error, SwitchFailure::Operation(_)));
        assert_eq!(card.into_inner().commands, 1);
    }

    // Preserve the single-payload regression fixtures against the same batch
    // implementation now used by production; no separate retry engine.
    fn retry_notification_delivery(
        cancelled: &AtomicBool,
        mut deliver: impl FnMut() -> Result<(), String>,
        remove: impl FnMut() -> Result<(), String>,
        wait: impl FnMut(Duration),
    ) -> Result<(), RetryNotificationFailure> {
        notifications::deliver_pending(
            cancelled,
            &[proletarius_euicc::PendingNotification {
                sequence_number: 1,
                address: String::new(),
                encoded: Vec::new(),
            }],
            |_| deliver(),
            remove,
            wait,
        )
    }

    #[test]
    fn notification_retry_repeats_delivery_then_removes_only_after_success() {
        let cancelled = AtomicBool::new(false);
        let mut delivery_calls = 0;
        let mut removal_calls = 0;
        let mut waits = Vec::new();
        retry_notification_delivery(
            &cancelled,
            || {
                delivery_calls += 1;
                if delivery_calls < 4 {
                    Err("temporary delivery failure".into())
                } else {
                    Ok(())
                }
            },
            || {
                removal_calls += 1;
                Ok(())
            },
            |wait| waits.push(wait),
        )
        .unwrap();
        assert_eq!(delivery_calls, 4);
        assert_eq!(removal_calls, 1);
        assert_eq!(
            waits,
            [
                Duration::from_millis(300),
                Duration::from_millis(600),
                Duration::from_millis(1200),
            ]
        );
    }

    #[test]
    fn notification_retry_never_removes_after_delivery_failure() {
        let cancelled = AtomicBool::new(false);
        let mut delivery_calls = 0;
        let mut removal_calls = 0;
        let mut waits = 0;
        let error = retry_notification_delivery(
            &cancelled,
            || {
                delivery_calls += 1;
                Err("delivery failed".into())
            },
            || {
                removal_calls += 1;
                Ok(())
            },
            |_| waits += 1,
        )
        .unwrap_err();
        assert!(matches!(error, RetryNotificationFailure::Failed(_)));
        assert_eq!(delivery_calls, 4);
        assert_eq!(removal_calls, 0);
        assert_eq!(waits, 3);
    }

    #[test]
    fn notification_retry_treats_post_delivery_card_confirmation_failure_as_uncertain() {
        let cancelled = AtomicBool::new(false);
        let mut removal_calls = 0;
        let mut waits = 0;
        let error = retry_notification_delivery(
            &cancelled,
            || Ok(()),
            || {
                removal_calls += 1;
                Err("card confirmation failed".into())
            },
            |_| waits += 1,
        )
        .unwrap_err();
        assert!(matches!(error, RetryNotificationFailure::Uncertain(_)));
        assert_eq!(removal_calls, 4);
        assert_eq!(waits, 3);
    }

    #[test]
    fn notification_confirmation_is_idempotent_when_card_already_removed_entry() {
        let result: Result<(), EuiccError<String>> = Err(EuiccError::NotificationOperation {
            operation: proletarius_euicc::NotificationOperation::Remove,
            result: proletarius_euicc::NotificationResult::NotFound,
        });
        assert_eq!(notification_removal_action(result), Ok(()));
    }

    #[test]
    fn notification_retry_preserves_existing_success_response_contract() {
        let response = serde_json::to_value(RetryNotificationResponse {
            message: "运营商通知发送并确认完成".into(),
            sequence_number: 11,
        })
        .unwrap();
        assert_eq!(
            response,
            json!({
                "message": "运营商通知发送并确认完成",
                "sequence_number": 11
            })
        );
    }

    #[test]
    fn notification_retry_only_treats_explicit_admission_rejections_as_conflicts() {
        assert!(is_reader_lease_conflict(
            &proletarius_domain::BackendFailure::ReaderAdmissionRejected {
                code: "reader_maintenance_busy"
            }
        ));
        assert!(is_reader_lease_conflict(
            &proletarius_domain::BackendFailure::ReaderAdmissionRejected { code: "card_busy" }
        ));
        assert!(is_reader_lease_conflict(
            &proletarius_domain::BackendFailure::GoBackendUnavailable(
                "reader native lease: card_busy".into()
            )
        ));
        assert!(!is_reader_lease_conflict(
            &proletarius_domain::BackendFailure::GoBackendUnavailable("connect timeout".into())
        ));
    }

    #[test]
    fn notification_retry_cancellation_during_delivery_never_retries_or_removes() {
        let cancelled = AtomicBool::new(false);
        let mut delivery_calls = 0;
        let mut removal_calls = 0;
        let error = retry_notification_delivery(
            &cancelled,
            || {
                delivery_calls += 1;
                cancelled.store(true, Ordering::Release);
                Err("connection interrupted".into())
            },
            || {
                removal_calls += 1;
                Ok(())
            },
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(error, RetryNotificationFailure::Uncertain(_)));
        assert_eq!(delivery_calls, 1);
        assert_eq!(removal_calls, 0);
    }

    #[test]
    fn interrupted_profile_switch_is_unknown_until_overview_reconciliation() {
        let fixture = JournalFixture::new();
        let service = Service::open(&fixture.0).unwrap();
        let operation_permit = service.begin_switch("8985200014631959072").unwrap();
        let operation_id = operation_permit.id.clone();
        let snapshot = service.current().unwrap();
        assert_eq!(snapshot.kind, "switch_profile");
        assert_eq!(
            snapshot.target_iccid.as_deref(),
            Some("8985200014631959072")
        );
        drop(operation_permit);
        drop(service);

        let recovered = Service::open(&fixture.0).unwrap();
        let snapshot = recovered.current().unwrap();
        assert_eq!(snapshot.id, operation_id);
        assert_eq!(snapshot.state, "failed");
        assert_eq!(snapshot.step.as_deref(), Some("interrupted"));
        assert_eq!(
            snapshot.error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
        assert!(!snapshot.recoverable);
        assert!(snapshot.target_iccid.is_none());
        assert!(matches!(
            recovered.begin_switch("8985200014631959072"),
            Err(BeginError::ReconciliationRequired)
        ));
        assert!(recovered.reconcile_after_overview_refresh("Reader A", 1, &[]));
        let reconciled = recovered.current().unwrap();
        assert!(reconciled.recoverable);
        assert!(
            reconciled
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("切换")
        );

        let journal = fs::read_to_string(fixture.0.join("reader-esim-operation.json")).unwrap();
        assert!(!journal.contains("8985200014631959072"));
        assert!(journal.contains("esim_operation_interrupted"));
    }

    #[test]
    fn delete_notice_is_removed_only_after_successful_es9_delivery() {
        let iccid = [0x89, 0x98, 0x21];
        let mut card = FakeDeleteCard {
            notifications: vec![fake_delete_notification(&iccid)],
            fail_retrieve: false,
            removed: 0,
        };
        let network = FakeDeleteNetwork {
            fail_notification: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };

        let warning =
            deliver_delete_notification(&mut card, &network, 13, &iccid, &AtomicBool::new(false));

        assert_eq!(warning, None);
        assert_eq!(card.removed, 1);
        assert_eq!(network.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn delete_notice_failure_never_removes_the_card_record() {
        let iccid = [0x89, 0x98, 0x21];
        let mut card = FakeDeleteCard {
            notifications: vec![fake_delete_notification(&iccid)],
            fail_retrieve: false,
            removed: 0,
        };
        let network = FakeDeleteNetwork {
            fail_notification: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };

        let warning =
            deliver_delete_notification(&mut card, &network, 13, &iccid, &AtomicBool::new(false));

        assert_eq!(
            warning.as_deref(),
            Some("delete_notification_handle_failed")
        );
        assert_eq!(card.removed, 0);
        assert_eq!(network.calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn missing_or_unreadable_delete_notice_keeps_existing_warning_distinction() {
        let iccid = [0x89, 0x98, 0x21];
        let network = FakeDeleteNetwork {
            fail_notification: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut missing_card = FakeDeleteCard {
            notifications: Vec::new(),
            fail_retrieve: false,
            removed: 0,
        };
        assert_eq!(
            deliver_delete_notification(
                &mut missing_card,
                &network,
                13,
                &iccid,
                &AtomicBool::new(false),
            )
            .as_deref(),
            Some("delete_notification_not_observed")
        );

        let mut unreadable_card = FakeDeleteCard {
            notifications: vec![fake_delete_notification(&iccid)],
            fail_retrieve: true,
            removed: 0,
        };
        assert_eq!(
            deliver_delete_notification(
                &mut unreadable_card,
                &network,
                13,
                &iccid,
                &AtomicBool::new(false),
            )
            .as_deref(),
            Some("delete_notification_retrieve_failed")
        );
        assert_eq!(unreadable_card.removed, 0);
        assert_eq!(network.calls.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod switch_failure_tests {
    include!("reader_esim_download/switch_failure_tests.rs");
}
