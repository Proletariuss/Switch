//! Finite startup-v1 physical reads, separate from metadata inspection.
//! This owner does not restore settings, write a recovery receipt or reopen
//! ordinary admission. Its private policy permits one exact native selection.
use super::{Coordinator, evidence};
use crate::call_mode_backups::{
    OperationFileLock, OperationNamespaceWitness, RecoveryLockKind,
    acquire_bound_existing_recovery_lock,
};
use crate::call_mode_restore_journal::{Event, RecoveryJournalCapture};
use crate::module_radio::CallAdmission;
use crate::module_radio_transport::{CallModeModuleIdentity, UsbComposition, VoiceConfiguration};
use crate::module_recovery::{Gate, PhysicalReadLineage};
use crate::usb_at_transport::recovery::NativeReadback;
use crate::usb_at_transport::{CloseDisposition, DurableUsbBinding, TransportError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

const POLICY: &str = "startup-v1";
const READ_BUDGET: Duration = Duration::from_secs(20);

/// A physical failure never changes metadata-inspection error semantics.
/// Cleanup uncertainty survives redacted startup logging until shutdown.
#[derive(Debug)]
pub(crate) struct PhysicalFailure {
    code: &'static str,
    cleanup_unconfirmed: bool,
}
impl PhysicalFailure {
    fn invalid(_detail: &str) -> Self {
        Self::unavailable()
    }
    fn unavailable() -> Self {
        Self {
            code: "startup_native_inspection_unavailable",
            cleanup_unconfirmed: false,
        }
    }
    fn cleanup_failed() -> Self {
        Self {
            code: "startup_native_cleanup_unconfirmed",
            cleanup_unconfirmed: true,
        }
    }
    fn native(error: TransportError) -> Self {
        Self {
            code: error.code,
            cleanup_unconfirmed: matches!(
                error.code,
                "usb_at_close_unconfirmed" | "startup_native_callback_panicked"
            ),
        }
    }
    pub(crate) fn cleanup_unconfirmed(&self) -> bool {
        self.cleanup_unconfirmed
    }
}
impl std::fmt::Display for PhysicalFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}
impl std::error::Error for PhysicalFailure {}
impl From<io::Error> for PhysicalFailure {
    fn from(_error: io::Error) -> Self {
        Self::unavailable()
    }
}

/// Constructed only from a genuine, fully captured pending typed journal.
/// Its persisted binding is an input to this policy, never itself a permit.
struct StartupReadPolicy {
    binding: DurableUsbBinding,
    expected_imei: zeroize::Zeroizing<String>,
    sha256: String,
}
impl StartupReadPolicy {
    fn from_evidence(evidence: &PhysicalEvidence) -> Result<Option<Self>, PhysicalFailure> {
        let Some(journal) = &evidence.journal else {
            return Ok(None);
        };
        let summary = journal.summary();
        if summary.receipt_present {
            return Ok(None);
        }
        if journal
            .events()
            .filter(|event| matches!(event, Event::Intent { .. }))
            .count()
            != 1
        {
            return Err(PhysicalFailure::invalid(
                "physical read requires one original typed intent",
            ));
        }
        let intent = journal.intent();
        let binding = intent.recovery_binding.as_ref().ok_or_else(|| {
            PhysicalFailure::invalid("pending journal has no complete native recovery binding")
        })?;
        if intent.module_imei.len() != 15
            || !intent.module_imei.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(PhysicalFailure::invalid(
                "pending journal has no exact bounded identity",
            ));
        }
        let binding_bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(binding)
                .map_err(|_| PhysicalFailure::invalid("native policy binding encoding failed"))?,
        );
        let mut digest = Sha256::new();
        for input in [
            POLICY.as_bytes(),
            evidence.tree.sha256.as_bytes(),
            journal.evidence_sha256().as_bytes(),
            binding_bytes.as_slice(),
            intent.module_imei.as_bytes(),
        ] {
            digest.update((input.len() as u64).to_le_bytes());
            digest.update(input);
        }
        Ok(Some(Self {
            binding: binding.clone(),
            expected_imei: zeroize::Zeroizing::new(intent.module_imei.clone()),
            sha256: format!("{:x}", digest.finalize()),
        }))
    }
}

struct PhysicalEvidence {
    tree: evidence::EvidenceTree,
    journal: Option<RecoveryJournalCapture>,
}
impl PhysicalEvidence {
    fn verify(
        &self,
        lineage: &PhysicalReadLineage,
        root: &Path,
        cheap_fence: &impl Fn() -> io::Result<()>,
    ) -> Result<(), PhysicalFailure> {
        cheap_fence()?;
        lineage
            .root_proof()
            .with_root(root, |_| self.tree.verify(cheap_fence))
            .map_err(|_| PhysicalFailure::unavailable())??;
        if let Some(journal) = &self.journal {
            cheap_fence()?;
            journal
                .verify_unchanged(lineage.root_proof())
                .map_err(|_| PhysicalFailure::unavailable())?;
        }
        cheap_fence()?;
        Ok(())
    }
}

struct LockPair {
    operation: Option<OperationFileLock>,
    adb: Option<OperationFileLock>,
}
impl LockPair {
    fn checked_release(lock: OperationFileLock) -> io::Result<OperationNamespaceWitness> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.release()))
            .unwrap_or_else(|_| Err(io::Error::other("checked physical lock release panicked")))
    }

    fn validate(&self) -> io::Result<()> {
        self.operation
            .as_ref()
            .ok_or_else(|| io::Error::other("physical operation lock ended"))?
            .validate()?;
        self.adb
            .as_ref()
            .ok_or_else(|| io::Error::other("physical ADB lock ended"))?
            .validate()
    }

    fn release_partial(&mut self) -> Result<(), PhysicalFailure> {
        let adb = self.adb.take().map(Self::checked_release);
        let operation = self.operation.take().map(Self::checked_release);
        if adb.as_ref().is_some_and(Result::is_err)
            || operation.as_ref().is_some_and(Result::is_err)
        {
            Err(PhysicalFailure::cleanup_failed())
        } else {
            Ok(())
        }
    }

    /// Always attempt both actual releases; namespace loss does not suppress
    /// original-resource cleanup. Never manufacture a success witness.
    fn release(&mut self) -> Result<[OperationNamespaceWitness; 2], PhysicalFailure> {
        let adb = self.adb.take().map(Self::checked_release);
        let operation = self.operation.take().map(Self::checked_release);
        match (operation, adb) {
            (Some(Ok(operation)), Some(Ok(adb))) => Ok([operation, adb]),
            _ => Err(PhysicalFailure::cleanup_failed()),
        }
    }
}

struct ExecutionLiveState {
    lineage: PhysicalReadLineage,
    gate: Gate,
    root: PathBuf,
    deadline: Instant,
    active: AtomicBool,
    cancelled: Arc<AtomicBool>,
    locks: Mutex<LockPair>,
    evidence: OnceLock<PhysicalEvidence>,
    policy: OnceLock<StartupReadPolicy>,
}
impl ExecutionLiveState {
    // Evidence verification calls only this cheap fence. Calling check from
    // inside EvidenceTree::verify would recurse or lock the same mutex twice.
    fn cheap_fence(&self) -> io::Result<()> {
        if !self.active.load(Ordering::Acquire)
            || self.cancelled.load(Ordering::Acquire)
            || Instant::now() >= self.deadline
        {
            return Err(io::Error::other("physical read owner ended or cancelled"));
        }
        self.lineage
            .validate(&self.gate)
            .map_err(|_| io::Error::other("physical read Gate/root authority ended"))?;
        self.locks
            .lock()
            .map_err(|_| io::Error::other("physical recovery lock state poisoned"))?
            .validate()?;
        self.lineage
            .validate(&self.gate)
            .map_err(|_| io::Error::other("physical read authority changed"))?;
        Ok(())
    }

    fn check(&self) -> Result<(), PhysicalFailure> {
        self.cheap_fence()?;
        self.policy
            .get()
            .ok_or_else(|| PhysicalFailure::invalid("physical startup policy is not approved"))?;
        self.evidence
            .get()
            .ok_or_else(|| PhysicalFailure::invalid("physical original input is not captured"))?
            .verify(&self.lineage, &self.root, &|| self.cheap_fence())
    }
}

/// Borrowed, concrete and sealed: raw binding data, ordinary rights and the
/// metadata inspector cannot construct the native entry capability.
pub(crate) struct NativeIdentityScope<'a> {
    owner: &'a ExecutionOwner,
}
impl NativeIdentityScope<'_> {
    pub(crate) fn binding(&self) -> &DurableUsbBinding {
        &self
            .owner
            .state
            .policy
            .get()
            .expect("sealed approved policy")
            .binding
    }
    pub(crate) fn expected_imei(&self) -> &str {
        self.owner
            .state
            .policy
            .get()
            .expect("sealed approved policy")
            .expected_imei
            .as_str()
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.owner.state.deadline
    }
    pub(crate) fn check(&self) -> Result<(), TransportError> {
        self.owner.state.check().map_err(|_| Self::fence_error())
    }
    pub(crate) fn admission(&self) -> CallAdmission {
        let state = Arc::downgrade(&self.owner.state);
        Arc::new(move || state.upgrade().is_some_and(|state| state.check().is_ok()))
    }
    fn fence_error() -> TransportError {
        TransportError {
            code: "startup_physical_read_revoked",
            message: "startup physical read authority ended".into(),
            response: String::new(),
            uncertain: false,
        }
    }
}

#[must_use]
struct ExecutionOwner {
    state: Arc<ExecutionLiveState>,
    native_attempted: bool,
}
impl ExecutionOwner {
    fn begin(
        gate: &Gate,
        root: &Path,
        cancelled: Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<Self, PhysicalFailure> {
        if !gate.is_quarantined() || Instant::now() >= deadline {
            return Err(PhysicalFailure::invalid(
                "physical startup scope unavailable",
            ));
        }
        let lineage = gate.capture_physical_read_lineage(root).map_err(|_| {
            PhysicalFailure::invalid("physical read requires the original live Gate/root")
        })?;
        lineage
            .validate(gate)
            .map_err(|_| PhysicalFailure::invalid("physical read lineage changed"))?;
        // Validate both original slots before acquiring either. The opener is
        // bound-existing and does not create/repair either original lock.
        lineage
            .root_proof()
            .with_root(root, |held| {
                for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
                    let stat = nix::sys::stat::fstatat(
                        held,
                        name,
                        nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
                    )?;
                    if stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG
                        || stat.st_size != 0
                        || stat.st_nlink != 1
                        || stat.st_uid != nix::unistd::geteuid().as_raw()
                        || stat.st_mode as u32 & 0o7133 != 0
                    {
                        return Err(io::Error::other(
                            "original physical recovery lock is unsafe",
                        ));
                    }
                }
                Ok::<_, io::Error>(())
            })
            .map_err(|_| PhysicalFailure::unavailable())??;
        let operation = acquire_bound_existing_recovery_lock(
            root,
            lineage.root_proof(),
            RecoveryLockKind::Operation,
            false,
        )?;
        let mut locks = LockPair {
            operation: Some(operation),
            adb: None,
        };
        // Even second acquisition/deadline/lineage failure explicitly releases
        // the first original resource, with any cleanup failure retained.
        let second = (|| {
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(PhysicalFailure::invalid(
                    "physical recovery acquisition cancelled",
                ));
            }
            lineage.validate(gate).map_err(|_| {
                PhysicalFailure::invalid("physical authority ended after first lock")
            })?;
            locks.adb = Some(acquire_bound_existing_recovery_lock(
                root,
                lineage.root_proof(),
                RecoveryLockKind::AdbInterface,
                false,
            )?);
            Ok::<_, PhysicalFailure>(())
        })();
        if let Err(error) = second {
            let cleanup = locks.release_partial();
            return Err(combine(error, cleanup.err()));
        }
        let mut owner = Self {
            state: Arc::new(ExecutionLiveState {
                lineage,
                gate: gate.clone(),
                root: root.to_owned(),
                deadline,
                active: AtomicBool::new(true),
                cancelled,
                locks: Mutex::new(locks),
                evidence: OnceLock::new(),
                policy: OnceLock::new(),
            }),
            native_attempted: false,
        };
        let captured = owner.capture();
        if let Err(error) = captured {
            let cleanup = owner.release_locks();
            return Err(combine(error, cleanup.err()));
        }
        Ok(owner)
    }

    fn capture(&self) -> Result<(), PhysicalFailure> {
        let state = &self.state;
        state.cheap_fence()?;
        let tree = state
            .lineage
            .root_proof()
            .with_root(&state.root, |held| {
                evidence::EvidenceTree::capture(held, &|| state.cheap_fence())
            })
            .map_err(|_| PhysicalFailure::unavailable())??;
        state.cheap_fence()?;
        let journal = crate::call_mode_restore_journal::capture_for_recovery(
            &state.root,
            state.lineage.root_proof(),
        )
        .map_err(|_| PhysicalFailure::unavailable())?;
        let evidence = PhysicalEvidence { tree, journal };
        evidence.verify(&state.lineage, &state.root, &|| state.cheap_fence())?;
        state
            .evidence
            .set(evidence)
            .map_err(|_| PhysicalFailure::invalid("physical evidence already captured"))?;
        Ok(())
    }

    fn approve(&self) -> Result<bool, PhysicalFailure> {
        self.state.cheap_fence()?;
        let evidence = self.state.evidence.get().expect("captured owner");
        evidence.verify(&self.state.lineage, &self.state.root, &|| {
            self.state.cheap_fence()
        })?;
        let Some(policy) = StartupReadPolicy::from_evidence(evidence)? else {
            return Ok(false);
        };
        self.state
            .policy
            .set(policy)
            .map_err(|_| PhysicalFailure::invalid("physical startup policy already frozen"))?;
        self.state.check()?;
        Ok(true)
    }

    fn read_with(
        &mut self,
        native: impl FnOnce(&NativeIdentityScope<'_>) -> Result<NativeReadback, TransportError>,
    ) -> Result<NativeReadback, PhysicalFailure> {
        if self.native_attempted {
            return Err(PhysicalFailure::invalid(
                "physical selection was already attempted",
            ));
        }
        self.state.check()?;
        // Set before the first native Context/snapshot, including failed open.
        self.native_attempted = true;
        let result = native(&NativeIdentityScope { owner: self });
        // Preserve only a trusted static code and typed cleanup uncertainty;
        // no response, identity or diagnostic payload escapes this boundary.
        result.map_err(PhysicalFailure::native)
    }

    fn release_locks(&mut self) -> Result<[OperationNamespaceWitness; 2], PhysicalFailure> {
        self.state.active.store(false, Ordering::Release);
        let (mut locks, poisoned) = match self.state.locks.lock() {
            Ok(locks) => (locks, false),
            Err(error) => (error.into_inner(), true),
        };
        let released = locks.release();
        if poisoned {
            // Still release both actual original resources, but a poisoned
            // ownership boundary cannot certify successful settlement.
            Err(PhysicalFailure::cleanup_failed())
        } else {
            released
        }
    }
}
impl Drop for ExecutionOwner {
    fn drop(&mut self) {
        self.state.active.store(false, Ordering::Release);
        // Exceptional unwinding is never successful settlement. Still attempt
        // checked releases of every retained original resource.
        let mut locks = self
            .state
            .locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if locks.operation.is_some() || locks.adb.is_some() {
            if let Err(error) = locks.release_partial() {
                crate::diagnostics::write_line(format_args!(
                    "physical read unwind cleanup: {error}"
                ));
            }
        }
    }
}

fn combine(primary: PhysicalFailure, cleanup: Option<PhysicalFailure>) -> PhysicalFailure {
    match cleanup {
        Some(cleanup) => PhysicalFailure {
            cleanup_unconfirmed: primary.cleanup_unconfirmed || cleanup.cleanup_unconfirmed,
            ..primary
        },
        None => primary,
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PhysicalDiagnostic {
    state: &'static str,
    scope: &'static str,
    policy: &'static str,
    ordinary_quarantine_preserved: bool,
    recovery_completed: bool,
    evidence_nodes: usize,
    evidence_bytes: usize,
    evidence_sha256: String,
    journal_evidence_sha256: String,
    policy_sha256: String,
    native_close: &'static str,
}

/// Provisional read-only facts after actual native close, blocking worker end
/// and checked reverse dual release. No live target/right/kernel lock remains.
pub(crate) struct PhysicalInspection {
    diagnostic: PhysicalDiagnostic,
    state: Arc<ExecutionLiveState>,
    readback: NativeReadback,
    witnesses: [OperationNamespaceWitness; 2],
}
impl PhysicalInspection {
    pub(crate) fn startup_logging(&self) -> &PhysicalDiagnostic {
        &self.diagnostic
    }
    pub(crate) fn identity(&self) -> &CallModeModuleIdentity {
        self.readback.identity()
    }
    pub(crate) fn usb_composition(&self) -> UsbComposition {
        self.readback.usb_composition()
    }
    pub(crate) fn voice(&self) -> VoiceConfiguration {
        self.readback.voice()
    }
    pub(crate) fn close_disposition(&self) -> CloseDisposition {
        self.readback.close_disposition()
    }
    pub(crate) fn verify(&self, gate: &Gate) -> Result<(), PhysicalFailure> {
        let deadline = Instant::now() + READ_BUDGET;
        let check = || {
            if Instant::now() >= deadline {
                return Err(io::Error::other(
                    "physical inspection verification deadline",
                ));
            }
            self.state
                .lineage
                .validate(gate)
                .map_err(|_| io::Error::other("physical inspection Gate/root changed"))?;
            if self.state.active.load(Ordering::Acquire) {
                return Err(io::Error::other("physical execution owner is still active"));
            }
            let locks = self
                .state
                .locks
                .lock()
                .map_err(|_| io::Error::other("physical lock state poisoned"))?;
            if locks.operation.is_some() || locks.adb.is_some() {
                return Err(io::Error::other("physical lock is still retained"));
            }
            drop(locks);
            for witness in &self.witnesses {
                witness.validate()?;
            }
            Ok(())
        };
        self.state
            .evidence
            .get()
            .expect("sealed retained evidence")
            .verify(&self.state.lineage, &self.state.root, &check)
    }
}

struct CancelWaiter(Arc<AtomicBool>);
impl Drop for CancelWaiter {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn run_with_native(
    gate: Gate,
    root: PathBuf,
    cancelled: Arc<AtomicBool>,
    native: impl FnOnce(&NativeIdentityScope<'_>) -> Result<NativeReadback, TransportError>,
) -> Result<Option<PhysicalInspection>, PhysicalFailure> {
    let mut owner = ExecutionOwner::begin(&gate, &root, cancelled, Instant::now() + READ_BUDGET)?;
    let result = (|| {
        if !owner.approve()? {
            return Ok(None);
        }
        owner.read_with(native).map(Some)
    })();
    // Native has synchronously consumed/closed its actual target on all normal
    // outcomes. Deactivate only after that return, then release both locks.
    let released = owner.release_locks();
    let readback = match (result, released) {
        (Ok(Some(readback)), Ok(witnesses)) => (readback, witnesses),
        (Ok(None), Ok(_)) => return Ok(None),
        (Err(error), cleanup) => return Err(combine(error, cleanup.err())),
        (Ok(_), Err(error)) => return Err(error),
    };
    let evidence = owner.state.evidence.get().expect("captured physical input");
    let policy = owner.state.policy.get().expect("approved physical input");
    let native_close = match readback.0.close_disposition() {
        CloseDisposition::ReleasedAndHandleDropped => "released_and_handle_dropped",
        CloseDisposition::DisconnectedAndHandleDropped => "disconnected_and_handle_dropped",
    };
    let inspection = PhysicalInspection {
        diagnostic: PhysicalDiagnostic {
            state: "physical_read_inspected",
            scope: "physical_read_only",
            policy: POLICY,
            ordinary_quarantine_preserved: gate.is_quarantined(),
            recovery_completed: false,
            evidence_nodes: evidence.tree.nodes,
            evidence_bytes: evidence.tree.bytes,
            evidence_sha256: evidence.tree.sha256.clone(),
            journal_evidence_sha256: evidence
                .journal
                .as_ref()
                .expect("approved genuine journal")
                .evidence_sha256()
                .to_owned(),
            policy_sha256: policy.sha256.clone(),
            native_close,
        },
        state: owner.state.clone(),
        readback: readback.0,
        witnesses: readback.1,
    };
    inspection.verify(&gate)?;
    Ok(Some(inspection))
}

async fn inspect_with_native(
    gate: &Gate,
    root: &Path,
    cancelled: Arc<AtomicBool>,
    native: impl FnOnce(&NativeIdentityScope<'_>) -> Result<NativeReadback, TransportError>
    + Send
    + 'static,
) -> Result<Option<PhysicalInspection>, PhysicalFailure> {
    if !gate.is_quarantined() {
        return Ok(None);
    }
    let _cancel_waiter = CancelWaiter(cancelled.clone());
    let gate = gate.clone();
    let root = root.to_owned();
    // Dropping the async waiter only requests cancellation. The blocking
    // worker retains its owner/locks through native checked close/release.
    tokio::task::spawn_blocking(move || run_with_native(gate, root, cancelled, native))
        .await
        .map_err(|_| PhysicalFailure::cleanup_failed())?
}

impl Coordinator {
    pub(crate) async fn inspect_native(
        gate: &Gate,
        root: &Path,
    ) -> Result<Option<PhysicalInspection>, PhysicalFailure> {
        Self::inspect_native_with_cancel(gate, root, Arc::new(AtomicBool::new(false))).await
    }

    /// Startup signal/parent monitoring sets this flag and continues awaiting
    /// the real worker, so termination cannot skip checked resource closure.
    pub(crate) async fn inspect_native_with_cancel(
        gate: &Gate,
        root: &Path,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<PhysicalInspection>, PhysicalFailure> {
        inspect_with_native(
            gate,
            root,
            cancelled,
            crate::usb_at_transport::recovery::read_identity_and_state,
        )
        .await
    }
}
