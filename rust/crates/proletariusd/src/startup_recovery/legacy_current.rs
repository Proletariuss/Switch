//! A startup-only current baseline for bounded, journal-free legacy data.
//! No historical completion is asserted and no device settings are written.
use super::evidence::EvidenceTree;
use crate::call_mode_backups::{
    OperationFileLock, OperationNamespaceWitness, RecoveryLockKind, acquire_bound_recovery_lock,
};
use crate::module_radio_transport::UsbComposition;
use crate::module_recovery::{Gate, LegacyCurrentClaim};
use crate::usb_at_transport::{Transport, TransportError};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(30);
const ABSENT: &[&str] = &[
    crate::module_recovery::LEGACY_EXECUTION_PENDING,
    crate::call_mode_restore_journal::ACTIVE_NAME,
    crate::call_mode_restore_journal::ARCHIVE_NAME,
    "voice-control",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Failure {
    Ineligible,
    EvidenceChanged,
    Busy,
    Cancelled,
    Timeout,
    DeviceUnconfirmed,
    ReadbackUnconfirmed,
    HelperUnconfirmed,
    CleanupUnconfirmed,
    WorkerFailed,
}
impl Failure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Ineligible => "legacy_current_ineligible",
            Self::EvidenceChanged => "legacy_current_evidence_changed",
            Self::Busy => "legacy_current_busy",
            Self::Cancelled => "legacy_current_cancelled",
            Self::Timeout => "legacy_current_timeout",
            Self::DeviceUnconfirmed => "legacy_current_device_unconfirmed",
            Self::ReadbackUnconfirmed => "legacy_current_readback_unconfirmed",
            Self::HelperUnconfirmed => "legacy_current_helper_unconfirmed",
            Self::CleanupUnconfirmed => "legacy_current_cleanup_unconfirmed",
            Self::WorkerFailed => "legacy_current_worker_failed",
        }
    }
    fn retained(self) -> crate::support_diagnostics::recovery::LegacyCurrent {
        use crate::support_diagnostics::recovery::LegacyCurrent;
        match self {
            Self::Ineligible => LegacyCurrent::Ineligible,
            Self::EvidenceChanged => LegacyCurrent::EvidenceChanged,
            Self::Busy => LegacyCurrent::Busy,
            Self::Cancelled => LegacyCurrent::Cancelled,
            Self::Timeout => LegacyCurrent::Timeout,
            Self::DeviceUnconfirmed => LegacyCurrent::DeviceUnconfirmed,
            Self::ReadbackUnconfirmed => LegacyCurrent::ReadbackUnconfirmed,
            Self::HelperUnconfirmed => LegacyCurrent::HelperUnconfirmed,
            Self::CleanupUnconfirmed => LegacyCurrent::CleanupUnconfirmed,
            Self::WorkerFailed => LegacyCurrent::WorkerFailed,
        }
    }
    fn from_transport(error: &TransportError) -> Self {
        if error.code == Self::WorkerFailed.code() {
            return Self::WorkerFailed;
        }
        if error.uncertain {
            return Self::CleanupUnconfirmed;
        }
        match error.code {
            "legacy_current_ineligible" => Self::Ineligible,
            "legacy_current_evidence_changed" => Self::EvidenceChanged,
            "legacy_current_busy" => Self::Busy,
            "legacy_current_cancelled" => Self::Cancelled,
            "legacy_current_timeout" => Self::Timeout,
            "legacy_current_device_unconfirmed" => Self::DeviceUnconfirmed,
            "legacy_current_helper_unconfirmed" => Self::HelperUnconfirmed,
            _ => Self::ReadbackUnconfirmed,
        }
    }
    fn record(self) {
        crate::support_diagnostics::recovery::legacy_current(self.retained());
        use crate::support_diagnostics::module::{
            self as diagnostic, Code, ResultKind, Stage, Transport,
        };
        let code = match self {
            Self::Ineligible => Code::PolicyUnavailable,
            Self::EvidenceChanged => Code::Stale,
            Self::Busy => Code::Busy,
            Self::Cancelled => Code::Stopped,
            Self::Timeout => Code::Timeout,
            Self::DeviceUnconfirmed => Code::BindingMissing,
            Self::ReadbackUnconfirmed => Code::OutcomeUnknown,
            Self::HelperUnconfirmed => Code::CallUnconfirmed,
            Self::CleanupUnconfirmed => Code::CleanupUnconfirmed,
            Self::WorkerFailed => Code::WorkerFailed,
        };
        diagnostic::startup(Stage::Startup, ResultKind::Failed, code, Transport::Usb);
    }
    pub(crate) fn cleanup_unconfirmed(self) -> bool {
        matches!(self, Self::CleanupUnconfirmed | Self::WorkerFailed)
    }
}
fn io_failure<T>(_: T) -> Failure {
    Failure::EvidenceChanged
}

fn require_absent(root: &File) -> io::Result<()> {
    use nix::fcntl::AtFlags;
    for name in ABSENT {
        match nix::sys::stat::fstatat(root, *name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Err(nix::errno::Errno::ENOENT) => {}
            _ => {
                return Err(io::Error::other(
                    "legacy current obligations are not absent",
                ));
            }
        }
    }
    Ok(())
}

struct Evidence {
    tree: EvidenceTree,
    imei: Option<zeroize::Zeroizing<String>>,
    targets: Vec<UsbComposition>,
}
impl Evidence {
    #[cfg(test)]
    fn capture(root: &File) -> Result<Self, Failure> {
        Self::capture_checked(root, &|| Ok(()))
    }
    fn capture_checked(root: &File, fence: &impl Fn() -> io::Result<()>) -> Result<Self, Failure> {
        fence().map_err(io_failure)?;
        require_absent(root).map_err(|_| Failure::Ineligible)?;
        let tree = EvidenceTree::capture_legacy_current(root, fence).map_err(io_failure)?;
        let legacy = crate::module_recovery::legacy_locks::legacy_current_source(root)
            .map_err(|_| Failure::Ineligible)?;
        let download = crate::module_recovery::runtime_download::legacy_current_source(root)
            .map_err(|_| Failure::Ineligible)?;
        let data =
            crate::startup_recovery::reconcile::backup_lifecycle::legacy_current_data_only(root)
                .map_err(|_| Failure::Ineligible)?;
        let backups = tree
            .legacy_current_backups()
            .map_err(|_| Failure::Ineligible)?;
        if !tree.has_legacy_current_data() && !legacy && !download && !data {
            return Err(Failure::Ineligible);
        }
        let imei = backups
            .first()
            .map(|first| zeroize::Zeroizing::new(first.module.imei.clone()));
        let mut targets = Vec::new();
        for backup in backups {
            if !imei.as_ref().is_some_and(|id| backup.module.imei == **id) {
                return Err(Failure::Ineligible);
            }
            let target = backup.usb.call_mode_target();
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        tree.verify_legacy_current_runtime()
            .map_err(|_| Failure::Ineligible)?;
        require_absent(root).map_err(io_failure)?;
        tree.verify(fence).map_err(io_failure)?;
        Ok(Self {
            tree,
            imei,
            targets,
        })
    }
    fn verify(&self, root: &File, fence: &impl Fn() -> io::Result<()>) -> io::Result<()> {
        require_absent(root)?;
        self.tree.verify(fence)?;
        require_absent(root)
    }
}

struct Locks {
    operation: Option<OperationFileLock>,
    adb: Option<OperationFileLock>,
}
impl Locks {
    fn acquire(root: &Path, claim: &LegacyCurrentClaim) -> Result<Self, Failure> {
        let mut locks = Self {
            operation: None,
            adb: None,
        };
        for kind in [RecoveryLockKind::Operation, RecoveryLockKind::AdbInterface] {
            let result = acquire_bound_recovery_lock(root, claim.root_proof(), kind, false);
            match result {
                Ok(lock) => match kind {
                    RecoveryLockKind::Operation => locks.operation = Some(lock),
                    RecoveryLockKind::AdbInterface => locks.adb = Some(lock),
                },
                Err(_) => {
                    locks.release()?;
                    return Err(Failure::Busy);
                }
            }
        }
        Ok(locks)
    }
    fn validate(&self) -> io::Result<()> {
        self.operation
            .as_ref()
            .ok_or_else(|| io::Error::other("operation lock ended"))?
            .validate()?;
        self.adb
            .as_ref()
            .ok_or_else(|| io::Error::other("ADB lock ended"))?
            .validate()
    }
    fn release(&mut self) -> Result<Vec<OperationNamespaceWitness>, Failure> {
        let mut witnesses = Vec::new();
        let mut failed = false;
        for lock in [self.adb.take(), self.operation.take()]
            .into_iter()
            .flatten()
        {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.release())) {
                Ok(Ok(witness)) => witnesses.push(witness),
                _ => failed = true,
            }
        }
        if failed {
            Err(Failure::CleanupUnconfirmed)
        } else {
            Ok(witnesses)
        }
    }
}

struct ReadState {
    gate: Gate,
    claim: Arc<LegacyCurrentClaim>,
    root: PathBuf,
    evidence: Arc<Evidence>,
    locks: Mutex<Locks>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    activated: bool,
}
/// Only this coordinator constructs a read capability. It exposes no write plan.
#[derive(Clone)]
pub(crate) struct ReadScope(Arc<ReadState>);
impl ReadScope {
    fn cheap(&self) -> io::Result<()> {
        if self.0.cancelled.load(Ordering::Acquire) || Instant::now() >= self.0.deadline {
            return Err(io::Error::other("legacy current read ended"));
        }
        let valid = if self.0.activated {
            self.0.claim.validate_activated(&self.0.gate)
        } else {
            self.0.claim.validate(&self.0.gate)
        };
        valid.map_err(|_| io::Error::other("legacy current authority ended"))?;
        self.0
            .locks
            .lock()
            .map_err(|_| io::Error::other("legacy locks poisoned"))?
            .validate()
    }
    fn failure(&self) -> Failure {
        if self.0.cancelled.load(Ordering::Acquire) {
            Failure::Cancelled
        } else if Instant::now() >= self.0.deadline {
            Failure::Timeout
        } else {
            Failure::EvidenceChanged
        }
    }
    pub(crate) fn io_fence(&self) -> Result<(), TransportError> {
        self.cheap()
            .and_then(|()| {
                self.0
                    .claim
                    .root_proof()
                    .with_root(&self.0.root, |_| ())
                    .map_err(|_| io::Error::other("legacy root changed"))
            })
            .map_err(|_| Self::error(self.failure()))
    }
    pub(crate) fn check(&self) -> Result<(), TransportError> {
        let result = self.cheap().and_then(|()| {
            self.0
                .claim
                .root_proof()
                .with_root(&self.0.root, |root| {
                    self.0.evidence.verify(root, &|| self.cheap())
                })
                .map_err(|_| io::Error::other("legacy current root changed"))?
        });
        result.map_err(|_| Self::error(self.failure()))
    }
    pub(crate) fn error(failure: Failure) -> TransportError {
        TransportError {
            code: failure.code(),
            message: "Legacy current-state check unconfirmed".into(),
            response: String::new(),
            uncertain: failure.cleanup_unconfirmed(),
        }
    }
    pub(crate) fn admission(&self) -> crate::module_radio::CallAdmission {
        let scope = self.clone();
        Arc::new(move || scope.io_fence().is_ok())
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.0.deadline
    }
    pub(crate) fn expected_imei(&self) -> Option<&str> {
        self.0.evidence.imei.as_ref().map(|id| id.as_str())
    }
    pub(crate) fn accepts_usb(&self, usb: UsbComposition) -> bool {
        usb.valid()
            && (self.0.evidence.targets.is_empty()
                || self.0.evidence.targets.iter().any(|known| {
                    known.vendor_id == usb.vendor_id && known.product_id == usb.product_id
                }))
    }
}

/// An opaque current observation, bound to one original Gate/root and device.
/// It is deliberately not serializable and is never accepted across lifetimes.
pub(crate) struct Completed {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    claim: Arc<LegacyCurrentClaim>,
    root: PathBuf,
    evidence: Arc<Evidence>,
    witnesses: Vec<OperationNamespaceWitness>,
    device: crate::usb_at_transport::legacy_current::VerifiedDevice,
    #[cfg(test)]
    fixture_reopen: Option<
        fn(
            &ReadScope,
            &crate::usb_at_transport::legacy_current::VerifiedDevice,
        ) -> Result<Box<dyn Transport>, TransportError>,
    >,
}
impl Completed {
    pub(crate) fn claim(&self) -> &LegacyCurrentClaim {
        &self.claim
    }
    pub(crate) fn activation_live(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire) && Instant::now() < self.deadline
    }
    pub(crate) fn verify(&self, gate: &Gate) -> Result<(), ()> {
        if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(());
        }
        self.claim.validate(gate)?;
        self.claim
            .root_proof()
            .with_root(&self.root, |root| {
                for witness in &self.witnesses {
                    witness.validate()?;
                }
                self.evidence.verify(root, &|| {
                    if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
                        Err(io::Error::other("current baseline interval ended"))
                    } else {
                        Ok(())
                    }
                })
            })
            .map_err(|_| ())?
            .map_err(|_| ())
    }
    pub(crate) fn into_seed(self) -> Seed {
        Seed {
            deadline: self.deadline,
            cancelled: self.cancelled,
            claim: self.claim,
            root: self.root,
            evidence: self.evidence,
            device: self.device,
            opened: AtomicBool::new(false),
            #[cfg(test)]
            fixture_reopen: self.fixture_reopen,
        }
    }
}

/// The ordinary opener must re-check the same retained physical Device before
/// publishing its session. It cannot enumerate another module or a serial port.
pub(crate) struct Seed {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    opened: AtomicBool,
    claim: Arc<LegacyCurrentClaim>,
    root: PathBuf,
    evidence: Arc<Evidence>,
    device: crate::usb_at_transport::legacy_current::VerifiedDevice,
    #[cfg(test)]
    fixture_reopen: Option<
        fn(
            &ReadScope,
            &crate::usb_at_transport::legacy_current::VerifiedDevice,
        ) -> Result<Box<dyn Transport>, TransportError>,
    >,
}
impl Seed {
    pub(crate) fn open(&self, gate: &Gate) -> Result<Box<dyn Transport>, TransportError> {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.open_inner(gate)))
                .unwrap_or_else(|_| Err(ReadScope::error(Failure::WorkerFailed)));
        if let Err(error) = &result {
            self.claim.revoke(gate);
            Failure::from_transport(error).record();
        } else {
            crate::support_diagnostics::recovery::legacy_current(
                crate::support_diagnostics::recovery::LegacyCurrent::CurrentVerified,
            );
        }
        result
    }
    fn open_inner(&self, gate: &Gate) -> Result<Box<dyn Transport>, TransportError> {
        if self.opened.swap(true, Ordering::AcqRel) {
            self.claim.revoke(gate);
            return Err(ReadScope::error(Failure::DeviceUnconfirmed));
        }
        self.claim
            .validate_activated(gate)
            .map_err(|_| ReadScope::error(Failure::EvidenceChanged))?;
        let locks = Locks::acquire(&self.root, &self.claim).map_err(ReadScope::error)?;
        let scope = ReadScope(Arc::new(ReadState {
            gate: gate.clone(),
            claim: self.claim.clone(),
            root: self.root.clone(),
            evidence: self.evidence.clone(),
            locks: Mutex::new(locks),
            deadline: self.deadline,
            cancelled: self.cancelled.clone(),
            activated: true,
        }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            {
                match self.fixture_reopen {
                    Some(reopen) => reopen(&scope, &self.device),
                    None => crate::usb_at_transport::legacy_current::reopen(&scope, &self.device),
                }
            }
            #[cfg(not(test))]
            {
                crate::usb_at_transport::legacy_current::reopen(&scope, &self.device)
            }
        }))
        .unwrap_or_else(|_| Err(ReadScope::error(Failure::WorkerFailed)));
        let released = scope
            .0
            .locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release();
        match (result, released) {
            (Ok(transport), Ok(_)) => Ok(transport),
            (Ok(transport), Err(_)) => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    transport.checked_close()
                }));
                self.claim.revoke(gate);
                Err(ReadScope::error(Failure::CleanupUnconfirmed))
            }
            (Err(_), Err(_)) => {
                self.claim.revoke(gate);
                Err(ReadScope::error(Failure::CleanupUnconfirmed))
            }
            (Err(error), Ok(_)) => {
                self.claim.revoke(gate);
                Err(error)
            }
        }
    }
}

impl super::Coordinator {
    /// None is policy inapplicability before locks or physical I/O. A failed
    /// applicable check stays isolated; callers must not fall through to I/O.
    pub(crate) async fn inspect_legacy_current(
        gate: &Gate,
        root: &Path,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<Completed>, Failure> {
        use crate::support_diagnostics::recovery::{LegacyCurrent, legacy_current};
        if !gate.legacy_current_eligible() {
            legacy_current(LegacyCurrent::NotApplicable);
            return Ok(None);
        }
        legacy_current(LegacyCurrent::Started);
        use crate::support_diagnostics::module::{
            self as diagnostic, Code, ResultKind, Stage, Transport,
        };
        diagnostic::startup(
            Stage::Startup,
            ResultKind::Started,
            Code::None,
            Transport::Usb,
        );
        let result = Self::inspect_legacy_current_inner(
            gate,
            root,
            cancelled,
            crate::usb_at_transport::legacy_current::inspect,
        )
        .await;
        match &result {
            Ok(Some(_)) => diagnostic::startup(
                Stage::Startup,
                ResultKind::Succeeded,
                Code::None,
                Transport::Usb,
            ),
            Ok(None) => Failure::Ineligible.record(),
            Err(error) => error.record(),
        }
        result
    }
    async fn inspect_legacy_current_inner(
        gate: &Gate,
        root: &Path,
        cancelled: Arc<AtomicBool>,
        inspect: fn(
            &ReadScope,
        ) -> Result<
            crate::usb_at_transport::legacy_current::VerifiedDevice,
            TransportError,
        >,
    ) -> Result<Option<Completed>, Failure> {
        if !gate.legacy_current_eligible() {
            return Ok(None);
        }
        let gate = gate.clone();
        let root = root.to_owned();
        let deadline = Instant::now() + BUDGET;
        tokio::task::spawn_blocking(move || {
            let fence = || {
                if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                    Err(io::Error::other("legacy current interval ended"))
                } else {
                    Ok(())
                }
            };
            let ended = || {
                if cancelled.load(Ordering::Acquire) {
                    Failure::Cancelled
                } else {
                    Failure::Timeout
                }
            };
            fence().map_err(|_| ended())?;
            let proof = gate.root_proof_for(&root).map_err(io_failure)?;
            let eligible = proof
                .with_root(&root, |root| Evidence::capture_checked(root, &fence))
                .map_err(io_failure)?;
            let original = match eligible {
                Ok(value) => value,
                Err(Failure::Ineligible) => return Ok(None),
                Err(error) => return Err(error),
            };
            drop(original);
            fence().map_err(|_| ended())?;
            let claim = Arc::new(gate.begin_legacy_current(&root).map_err(io_failure)?);
            let locks = Locks::acquire(&root, &claim)?;
            let evidence = match claim
                .root_proof()
                .with_root(&root, |root| Evidence::capture_checked(root, &fence))
            {
                Ok(Ok(evidence)) => Arc::new(evidence),
                _ => {
                    let mut locks = locks;
                    locks.release()?;
                    return Err(Failure::EvidenceChanged);
                }
            };
            let scope = ReadScope(Arc::new(ReadState {
                gate,
                claim: claim.clone(),
                root: root.clone(),
                evidence: evidence.clone(),
                locks: Mutex::new(locks),
                deadline,
                cancelled,
                activated: false,
            }));
            let checked =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inspect(&scope)));
            let final_check = scope.check();
            let released = scope
                .0
                .locks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .release();
            let witnesses = released?;
            match &checked {
                Err(_) => return Err(Failure::WorkerFailed),
                Ok(Err(error)) if error.uncertain => return Err(Failure::CleanupUnconfirmed),
                _ => {}
            }
            if final_check.is_err() {
                return Err(scope.failure());
            }
            let device = match checked {
                Ok(Ok(device)) => device,
                Ok(Err(error)) if error.uncertain => return Err(Failure::CleanupUnconfirmed),
                Ok(Err(error)) if error.code == Failure::HelperUnconfirmed.code() => {
                    return Err(Failure::HelperUnconfirmed);
                }
                Ok(Err(error)) if error.code == Failure::Timeout.code() => {
                    return Err(Failure::Timeout);
                }
                Ok(Err(error)) if error.code == Failure::Cancelled.code() => {
                    return Err(Failure::Cancelled);
                }
                Ok(Err(error)) if error.code == Failure::DeviceUnconfirmed.code() => {
                    return Err(Failure::DeviceUnconfirmed);
                }
                Ok(Err(_)) => return Err(Failure::ReadbackUnconfirmed),
                Err(_) => return Err(Failure::WorkerFailed),
            };
            Ok(Some(Completed {
                deadline,
                cancelled: scope.0.cancelled.clone(),
                claim,
                root,
                evidence,
                witnesses,
                device,
                #[cfg(test)]
                fixture_reopen: None,
            }))
        })
        .await
        .map_err(|_| Failure::WorkerFailed)?
    }
}

#[cfg(test)]
mod tests;
