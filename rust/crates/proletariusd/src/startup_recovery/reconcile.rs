//! A new finite compensation attempt. Historical journals are immutable inputs;
//! only this independently durable ledger may record new physical work.
pub(crate) mod backup_lifecycle;
#[cfg(test)]
mod crash_tests;
#[cfg(test)]
mod finite_io_tests;
pub(crate) mod ledger;
mod storage;
#[cfg(test)]
pub(crate) mod tests;

use super::{Coordinator, evidence};
use crate::call_mode_backups::{
    OperationFileLock, OperationNamespaceWitness, RecoveryLockKind,
    acquire_bound_existing_recovery_lock,
};
use crate::call_mode_restore_journal::{Intent, RecoveryJournalCapture};
use crate::daemon_lifetime::RootProof;
use crate::module_radio::CallAdmission;
use crate::module_radio_transport::{UsbComposition, VoiceConfiguration};
use crate::module_recovery::{Gate, ReconciliationTicket};
use crate::usb_at_transport::{DurableUsbBinding, TransportError};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

pub(crate) const POLICY: &str = "startup-reconcile-v1";
pub(crate) const SUCCESSOR_POLICY: &str = "controlled-successor-at23-v1";
const BUDGET: Duration = Duration::from_secs(180);

#[derive(Debug)]
pub(crate) struct PhysicalFailure {
    code: &'static str,
    cleanup_unconfirmed: bool,
}
impl PhysicalFailure {
    fn pending() -> Self {
        Self {
            code: "startup_reconciliation_pending",
            cleanup_unconfirmed: false,
        }
    }
    fn cleanup() -> Self {
        Self {
            code: "startup_reconciliation_cleanup_unconfirmed",
            cleanup_unconfirmed: true,
        }
    }
    fn native(error: TransportError) -> Self {
        Self {
            code: error.code,
            cleanup_unconfirmed: matches!(
                error.code,
                "usb_at_close_unconfirmed"
                    | "startup_native_callback_panicked"
                    | "startup_voice_cleanup_unconfirmed"
            ),
        }
    }
    pub(crate) fn cleanup_unconfirmed(&self) -> bool {
        self.cleanup_unconfirmed
    }
}
impl std::fmt::Display for PhysicalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for PhysicalFailure {}
impl From<io::Error> for PhysicalFailure {
    fn from(_: io::Error) -> Self {
        Self::pending()
    }
}
impl From<ledger::Error> for PhysicalFailure {
    fn from(_: ledger::Error) -> Self {
        Self::pending()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ReconciliationMutation {
    Usb {
        expected: UsbComposition,
        target: UsbComposition,
    },
    Volte {
        expected: u8,
        target: u8,
    },
    Ims {
        expected: u8,
        target: u8,
    },
    Reboot,
    ManagedVoiceCleanup,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubmissionOutcome {
    NotSubmitted,
    KnownRejected,
    Acknowledged,
    SubmittedUnknown,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ReconciliationReadback {
    Usb {
        actual: UsbComposition,
    },
    Voice {
        actual: VoiceConfiguration,
    },
    RebootSuccessor {
        usb: UsbComposition,
        voice: VoiceConfiguration,
    },
}

struct Identity {
    gate: Gate,
    ticket: ReconciliationTicket,
    root: PathBuf,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    active: AtomicBool,
    input: HistoricalInput,
    locks: Mutex<Locks>,
    ledger: Mutex<ledger::Writer>,
}
struct HistoricalInput {
    tree: evidence::EvidenceTree,
    journal: RecoveryJournalCapture,
    backup: crate::call_mode_backups::CapturedNativeBackup,
    manifest_sha256: String,
}

/// Weak logical correlation only. It cannot create a native Device or extend
/// the original finite owner's lifetime.
#[derive(Clone)]
pub(crate) struct ReconciliationSeal(Weak<Identity>);
impl ReconciliationSeal {
    pub(crate) fn matches(&self, scope: &ReconciliationScope<'_>) -> bool {
        self.0.ptr_eq(&Arc::downgrade(&scope.owner.identity))
    }
}

pub(crate) struct PreparedReconciliationStep {
    seal: ReconciliationSeal,
    ordinal: u64,
    mutation: ReconciliationMutation,
}
impl PreparedReconciliationStep {
    pub(crate) fn check(&self) -> Result<(), TransportError> {
        let identity = self.seal.0.upgrade().ok_or_else(revoked)?;
        identity.check()?;
        identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .validate_step(self.ordinal, &self.mutation)
            .map_err(|_| revoked())
    }
    pub(crate) fn admission(&self) -> CallAdmission {
        let seal = self.seal.clone();
        let ordinal = self.ordinal;
        let mutation = self.mutation.clone();
        Arc::new(move || {
            seal.0.upgrade().is_some_and(|identity| {
                identity.check().is_ok()
                    && identity
                        .ledger
                        .lock()
                        .is_ok_and(|writer| writer.validate_step(ordinal, &mutation).is_ok())
            })
        })
    }
    pub(crate) fn kind(&self) -> &ReconciliationMutation {
        &self.mutation
    }
    pub(crate) fn ordinal(&self) -> u64 {
        self.ordinal
    }
    pub(crate) fn seal(&self) -> &ReconciliationSeal {
        &self.seal
    }
}

pub(crate) struct ReconciliationScope<'a> {
    owner: &'a ReconciliationOwner,
}
impl ReconciliationScope<'_> {
    pub(crate) fn binding(&self) -> &DurableUsbBinding {
        self.original_intent()
            .recovery_binding
            .as_ref()
            .expect("approved native binding")
    }
    pub(crate) fn expected_imei(&self) -> &str {
        &self.original_intent().module_imei
    }
    pub(crate) fn target_usb(&self) -> UsbComposition {
        self.owner.identity.input.backup.backup().usb
    }
    pub(crate) fn target_voice(&self) -> Option<VoiceConfiguration> {
        self.owner.identity.input.backup.backup().voice
    }
    pub(crate) fn original_voice_binding(
        &self,
    ) -> Option<&crate::call_mode_adb::DurableVoiceBinding> {
        self.original_intent().voice_recovery_binding.as_ref()
    }
    pub(crate) fn original_intent(&self) -> &Intent {
        self.owner.identity.input.journal.intent()
    }
    pub(crate) fn original_manifest_sha256(&self) -> &str {
        &self.owner.identity.input.manifest_sha256
    }
    pub(crate) fn root(&self) -> &Path {
        &self.owner.identity.root
    }
    pub(crate) fn root_proof(&self) -> &RootProof {
        self.owner.identity.ticket.root_proof()
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.owner.identity.deadline
    }
    pub(crate) fn check(&self) -> Result<(), TransportError> {
        self.owner.identity.check()
    }
    pub(crate) fn seal(&self) -> ReconciliationSeal {
        ReconciliationSeal(Arc::downgrade(&self.owner.identity))
    }
    pub(crate) fn admission(&self) -> CallAdmission {
        let identity = Arc::downgrade(&self.owner.identity);
        Arc::new(move || {
            identity
                .upgrade()
                .is_some_and(|identity| identity.check().is_ok())
        })
    }
    pub(crate) fn observe_current(
        &self,
        usb: UsbComposition,
        voice: VoiceConfiguration,
    ) -> Result<(), TransportError> {
        self.check()?;
        self.owner
            .identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .observe(usb, voice)
            .map_err(|_| revoked())
    }
    pub(crate) fn prepare(
        &self,
        mutation: ReconciliationMutation,
    ) -> Result<PreparedReconciliationStep, TransportError> {
        self.check()?;
        let ordinal = self
            .owner
            .identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .prepare(mutation.clone())
            .map_err(|_| revoked())?;
        Ok(PreparedReconciliationStep {
            seal: self.seal(),
            ordinal,
            mutation,
        })
    }
    pub(crate) fn record_outcome(
        &self,
        step: &PreparedReconciliationStep,
        outcome: SubmissionOutcome,
    ) -> Result<(), TransportError> {
        if !step.seal.matches(self) {
            return Err(revoked());
        }
        // A submitted result remains recordable after cancellation; it cannot
        // authorize another write. Root/ledger validation still applies.
        self.owner
            .identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .outcome(step.ordinal, &step.mutation, outcome)
            .map_err(|_| revoked())
    }
    pub(crate) fn record_readback(
        &self,
        step: &PreparedReconciliationStep,
        readback: ReconciliationReadback,
    ) -> Result<(), TransportError> {
        if !step.seal.matches(self) {
            return Err(revoked());
        }
        self.owner
            .identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .readback(step.ordinal, &step.mutation, readback)
            .map_err(|_| revoked())
    }
}
fn revoked() -> TransportError {
    TransportError {
        code: "startup_reconciliation_revoked",
        message: "startup reconciliation scope is unavailable".into(),
        response: String::new(),
        uncertain: false,
    }
}

impl PreparedReconciliationStep {
    pub(crate) fn cleanup_check(&self) -> Result<(), TransportError> {
        if !matches!(self.mutation, ReconciliationMutation::ManagedVoiceCleanup) {
            return Err(revoked());
        }
        self.seal.0.upgrade().ok_or_else(revoked)?.check_cleanup()
    }
    pub(crate) fn cleanup_admission(&self) -> CallAdmission {
        let seal = self.seal.clone();
        let allowed = matches!(self.mutation, ReconciliationMutation::ManagedVoiceCleanup);
        Arc::new(move || allowed && seal.0.upgrade().is_some_and(|i| i.check_cleanup().is_ok()))
    }
}
impl ReconciliationScope<'_> {
    pub(crate) fn record_voice_settled(
        &self,
        proof: &crate::call_mode_adb::startup_recovery::VoiceSettledProof,
    ) -> Result<(), TransportError> {
        self.check()?;
        proof.validate_for(self)?;
        if !proof.seal().matches(self) {
            return Err(revoked());
        }
        let value = serde_json::to_value(proof.evidence()).map_err(|_| revoked())?;
        self.owner
            .identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .voice_settled(value)
            .map_err(|_| revoked())
    }
    pub(crate) fn record_successor_voice(
        &self,
        proof: &crate::call_mode_adb::startup_recovery::VoiceSettledProof,
    ) -> Result<(), TransportError> {
        self.check()?;
        proof.validate_for(self)?;
        if !proof.seal().matches(self) || !proof.has_successor_observation() {
            return Err(revoked());
        }
        self.owner
            .identity
            .ledger
            .lock()
            .map_err(|_| revoked())?
            .successor_voice(serde_json::to_value(proof.evidence()).map_err(|_| revoked())?)
            .map_err(|_| revoked())
    }
}
struct Locks {
    operation: Option<OperationFileLock>,
    adb: Option<OperationFileLock>,
}
impl Locks {
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
    fn release(&mut self) -> Result<[OperationNamespaceWitness; 2], PhysicalFailure> {
        let checked = |lock: OperationFileLock| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.release()))
                .unwrap_or_else(|_| Err(io::Error::other("checked unlock panicked")))
        };
        let adb = self.adb.take().map(checked);
        let operation = self.operation.take().map(checked);
        match (operation, adb) {
            (Some(Ok(op)), Some(Ok(adb))) => Ok([op, adb]),
            _ => Err(PhysicalFailure::cleanup()),
        }
    }
    fn partial_release(&mut self) -> Result<(), PhysicalFailure> {
        let checked = |lock: OperationFileLock| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lock.release()))
                .unwrap_or_else(|_| Err(io::Error::other("checked unlock panicked")))
        };
        let adb = self.adb.take().map(checked);
        let operation = self.operation.take().map(checked);
        if adb.is_some_and(|r| r.is_err()) || operation.is_some_and(|r| r.is_err()) {
            Err(PhysicalFailure::cleanup())
        } else {
            Ok(())
        }
    }
}
impl Drop for Locks {
    fn drop(&mut self) {
        if self.operation.is_some() || self.adb.is_some() {
            let _ = self.partial_release();
        }
    }
}
impl Identity {
    fn cheap(&self, allow_cancelled: bool) -> io::Result<()> {
        if !self.active.load(Ordering::Acquire)
            || (!allow_cancelled && self.cancelled.load(Ordering::Acquire))
            || Instant::now() >= self.deadline
        {
            return Err(io::Error::other("reconciliation owner ended"));
        }
        self.ticket
            .validate(&self.gate)
            .map_err(|_| io::Error::other("reconciliation lineage changed"))?;
        self.locks
            .lock()
            .map_err(|_| io::Error::other("reconciliation lock state poisoned"))?
            .validate()?;
        self.ticket
            .root_proof()
            .with_root(&self.root, |_| ())
            .map_err(|_| io::Error::other("reconciliation root changed"))
    }
    fn verify_input(&self, allow_cancelled: bool) -> Result<(), TransportError> {
        self.cheap(allow_cancelled).map_err(|_| revoked())?;
        self.input
            .tree
            .verify(&|| self.cheap(allow_cancelled))
            .map_err(|_| revoked())?;
        self.input
            .journal
            .verify_unchanged(self.ticket.root_proof())
            .map_err(|_| revoked())?;
        Ok(())
    }
    fn check(&self) -> Result<(), TransportError> {
        self.verify_input(false)?;
        self.ledger
            .lock()
            .map_err(|_| revoked())?
            .validate()
            .map_err(|_| revoked())
    }
    fn check_cleanup(&self) -> Result<(), TransportError> {
        self.verify_input(true)
    }
}
struct ReconciliationOwner {
    identity: Arc<Identity>,
    native_attempted: bool,
}
impl ReconciliationOwner {
    fn begin(
        gate: &Gate,
        root: &Path,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, PhysicalFailure> {
        let ticket = gate
            .begin_reconciliation(root)
            .map_err(|_| PhysicalFailure::pending())?;
        let deadline = Instant::now() + BUDGET;
        let operation = acquire_bound_existing_recovery_lock(
            root,
            ticket.root_proof(),
            RecoveryLockKind::Operation,
            false,
        )?;
        let mut locks = Locks {
            operation: Some(operation),
            adb: None,
        };
        let result = (|| {
            ticket
                .validate(gate)
                .map_err(|_| PhysicalFailure::pending())?;
            if cancelled.load(Ordering::Acquire) {
                return Err(PhysicalFailure::pending());
            }
            locks.adb = Some(acquire_bound_existing_recovery_lock(
                root,
                ticket.root_proof(),
                RecoveryLockKind::AdbInterface,
                false,
            )?);
            locks.validate()?;
            let cheap = || -> io::Result<()> {
                ticket
                    .validate(gate)
                    .map_err(|_| io::Error::other("startup lineage changed"))?;
                locks.validate()?;
                if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return Err(io::Error::other("startup cancelled"));
                }
                Ok(())
            };
            let tree = ticket
                .root_proof()
                .with_root(root, |held| {
                    evidence::EvidenceTree::capture_for_reconciliation(held, &cheap)
                })
                .map_err(|_| PhysicalFailure::pending())??;
            let manifest = tree.manifest()?;
            let pristine = &manifest["slots"][".call-mode-pristine-v1"];
            if !pristine.is_null()
                && (pristine["stamp"]["size"] != 0
                    || pristine["stamp"]["links"] != 1
                    || pristine["stamp"]["mode"]
                        .as_u64()
                        .is_none_or(|m| m & nix::libc::S_IFMT as u64 != nix::libc::S_IFREG as u64))
            {
                return Err(PhysicalFailure::pending());
            }
            if !manifest["slots"][crate::module_recovery::LEGACY_EXECUTION_PENDING].is_null() {
                return Err(PhysicalFailure::pending());
            }
            let journal =
                crate::call_mode_restore_journal::capture_for_recovery(root, ticket.root_proof())
                    .map_err(|_| PhysicalFailure::pending())?
                    .ok_or_else(PhysicalFailure::pending)?;
            if journal.summary().receipt_present {
                return Err(PhysicalFailure::pending());
            }
            let intent = journal.intent();
            if intent.recovery_binding.is_none()
                || intent.module_imei.len() != 15
                || !intent.module_imei.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(PhysicalFailure::pending());
            }
            let (backup_id, backup_sha) = journal.backup_evidence();
            let backup = tokio::runtime::Handle::current()
                .block_on(crate::call_mode_backups::capture_native_backup(
                    root,
                    backup_id,
                    ticket.root_proof(),
                ))
                .map_err(|_| PhysicalFailure::pending())?;
            if backup.sha256() != backup_sha
                || backup.backup().module.imei != intent.module_imei
                || backup.backup().usb != intent.target_usb
                || backup.backup().voice != intent.target_voice
            {
                return Err(PhysicalFailure::pending());
            }
            cheap()?;
            tree.verify(&cheap)?;
            journal
                .verify_unchanged(ticket.root_proof())
                .map_err(|_| PhysicalFailure::pending())?;
            let writer = ledger::Writer::create(
                root,
                ticket.root_proof(),
                ticket.attempt_id(),
                &manifest,
                &journal,
                &backup,
            )?;
            let manifest_sha256 = storage::digest(
                &serde_json::to_vec(&manifest).map_err(|_| PhysicalFailure::pending())?,
            );
            Ok::<_, PhysicalFailure>((
                HistoricalInput {
                    tree,
                    journal,
                    backup,
                    manifest_sha256,
                },
                writer,
            ))
        })();
        let (input, writer) = match result {
            Ok(v) => v,
            Err(e) => {
                let cleanup = locks.partial_release();
                return Err(if cleanup.is_err() {
                    PhysicalFailure::cleanup()
                } else {
                    e
                });
            }
        };
        let owner = Self {
            identity: Arc::new(Identity {
                gate: gate.clone(),
                ticket,
                root: root.into(),
                deadline,
                cancelled,
                active: AtomicBool::new(true),
                input,
                locks: Mutex::new(locks),
                ledger: Mutex::new(writer),
            }),
            native_attempted: false,
        };
        Ok(owner)
    }
    fn release(&mut self) -> Result<[OperationNamespaceWitness; 2], PhysicalFailure> {
        self.identity.active.store(false, Ordering::Release);
        let (mut locks, poisoned) = match self.identity.locks.lock() {
            Ok(v) => (v, false),
            Err(e) => (e.into_inner(), true),
        };
        let result = locks.release();
        if poisoned {
            Err(PhysicalFailure::cleanup())
        } else {
            result
        }
    }
    fn run(
        &mut self,
        native: impl FnOnce(
            &ReconciliationScope<'_>,
        ) -> Result<
            crate::usb_at_transport::recovery::reconcile::NativeCompletion,
            TransportError,
        >,
    ) -> Result<(), PhysicalFailure> {
        if self.native_attempted {
            return Err(PhysicalFailure::pending());
        }
        self.identity.check().map_err(PhysicalFailure::native)?;
        self.native_attempted = true;
        let scope = ReconciliationScope { owner: self };
        let completion = native(&scope).map_err(PhysicalFailure::native)?;
        if !completion.seal().matches(&scope)
            || completion.identity().imei != scope.expected_imei()
            || completion.usb() != scope.target_usb()
            || !ledger::same_voice(scope.target_voice(), completion.voice())
            || completion.close_disposition()
                != crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped
        {
            return Err(PhysicalFailure::cleanup());
        }
        completion
            .voice_settled()
            .validate_for(&scope)
            .map_err(PhysicalFailure::native)?;
        self.identity.check().map_err(PhysicalFailure::native)?;
        self.identity
            .ledger
            .lock()
            .map_err(|_| PhysicalFailure::cleanup())?
            .native_closed(completion.usb(), completion.voice())?;
        Ok(())
    }
}
impl Drop for ReconciliationOwner {
    fn drop(&mut self) {
        self.identity.active.store(false, Ordering::Release);
        let mut locks = self
            .identity
            .locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if locks.operation.is_some() || locks.adb.is_some() {
            let _ = locks.partial_release();
        }
    }
}
struct SettledWorker {
    identity: Arc<Identity>,
    witnesses: [OperationNamespaceWitness; 2],
}
fn blocking_run(
    gate: Gate,
    root: PathBuf,
    cancelled: Arc<AtomicBool>,
) -> Result<SettledWorker, PhysicalFailure> {
    blocking_run_with_native(
        gate,
        root,
        cancelled,
        crate::usb_at_transport::recovery::reconcile::reconcile,
    )
}
fn blocking_run_with_native(
    gate: Gate,
    root: PathBuf,
    cancelled: Arc<AtomicBool>,
    native: impl FnOnce(
        &ReconciliationScope<'_>,
    ) -> Result<
        crate::usb_at_transport::recovery::reconcile::NativeCompletion,
        TransportError,
    >,
) -> Result<SettledWorker, PhysicalFailure> {
    let mut owner = ReconciliationOwner::begin(&gate, &root, cancelled)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.run(native)))
        .unwrap_or_else(|_| Err(PhysicalFailure::cleanup()));
    let release = owner.release();
    match (result, release) {
        (Ok(()), Ok(witnesses)) => Ok(SettledWorker {
            identity: owner.identity.clone(),
            witnesses,
        }),
        (Err(e), Ok(_)) => Err(e),
        (_, Err(e)) => Err(e),
    }
}
/// No data constructor exists. This token is produced only after the blocking
/// worker actually joined and the exact original directories were published.
pub(crate) struct CompletedReconciliation {
    ticket: ReconciliationTicket,
    root: PathBuf,
    witnesses: [OperationNamespaceWitness; 2],
    publication_id: String,
    receipt_sha256: String,
}
impl std::fmt::Debug for CompletedReconciliation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletedReconciliation")
            .finish_non_exhaustive()
    }
}
impl CompletedReconciliation {
    pub(crate) fn ticket(&self) -> &ReconciliationTicket {
        &self.ticket
    }
    pub(crate) fn receipt_sha256(&self) -> &str {
        &self.receipt_sha256
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn verify_for_gate(&self, proof: &RootProof) -> Result<(), PhysicalFailure> {
        if !proof.same_authority(self.ticket().root_proof()) {
            return Err(PhysicalFailure::pending());
        }
        for witness in &self.witnesses {
            witness.validate()?;
        }
        proof
            .with_root(self.root(), |held| -> Result<(), PhysicalFailure> {
                match ledger::inspect_startup(held)? {
                    ledger::RecoveryClassification::PublishedCoverage(p)
                        if p.attempt_id() == self.publication_id
                            && p.receipt_sha256() == self.receipt_sha256 =>
                    {
                        Ok(())
                    }
                    _ => Err(PhysicalFailure::pending()),
                }
            })
            .map_err(|_| PhysicalFailure::pending())?
    }
    pub(crate) fn startup_logging(&self) -> serde_json::Value {
        serde_json::json!({"state":"startup_reconciled","policy":POLICY,"physical_settled":true,"original_bytes_preserved":true,"receipt_sha256":self.receipt_sha256})
    }
}
impl SettledWorker {
    fn finalize(self) -> Result<CompletedReconciliation, PhysicalFailure> {
        // This method is called only by the async side after a successful
        // JoinHandle await; no blocking worker can self-certify its own join.
        for witness in &self.witnesses {
            witness.validate()?;
        }
        let identity = &self.identity;
        if identity.active.load(Ordering::Acquire) {
            return Err(PhysicalFailure::cleanup());
        }
        identity.input.tree.verify(&|| Ok(()))?;
        let mut writer = identity
            .ledger
            .lock()
            .map_err(|_| PhysicalFailure::cleanup())?;
        writer.store.append(ledger::Event::HostSettled)?;
        let physical = writer.store.append(ledger::Event::PhysicalSettled)?;
        let original = identity.input.tree.manifest()?;
        let receipt_sha256 = ledger::finish_publication(
            &identity.root,
            identity.ticket.root_proof(),
            &mut writer.store,
            &original,
            &physical,
        )?;
        let archive = writer.store.archive_parent()?;
        identity.input.tree.verify_retired(&archive, &|| Ok(()))?;
        drop(writer);
        let identity = Arc::try_unwrap(self.identity).map_err(|_| PhysicalFailure::cleanup())?;
        let publication_id = identity.ticket.attempt_id().to_owned();
        let completed = CompletedReconciliation {
            ticket: identity.ticket,
            root: identity.root,
            witnesses: self.witnesses,
            publication_id,
            receipt_sha256,
        };
        completed.verify_for_gate(completed.ticket().root_proof())?;
        Ok(completed)
    }
}
struct CancelWaiter(Arc<AtomicBool>);
impl Drop for CancelWaiter {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release)
    }
}
impl Coordinator {
    pub(crate) async fn reconcile_with_cancel(
        gate: &Gate,
        root: &Path,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<CompletedReconciliation>, PhysicalFailure> {
        if !gate.is_quarantined() {
            return Ok(None);
        }
        let candidate = gate
            .capture_physical_read_lineage(root)
            .map_err(|_| PhysicalFailure::pending())?
            .root_proof()
            .with_root(root, ledger::metadata_resume_candidate)
            .map_err(|_| PhysicalFailure::pending())??;
        if let Some(candidate) = candidate {
            let ticket = gate
                .begin_reconciliation(root)
                .map_err(|_| PhysicalFailure::pending())?;
            let path = root.to_owned();
            let _waiter = CancelWaiter(cancelled.clone());
            let joined =
                tokio::task::spawn_blocking(move || metadata_worker(ticket, path, candidate))
                    .await
                    .map_err(|_| PhysicalFailure::cleanup())??;
            return Ok(Some(joined.finalize()?));
        }
        // No Context or lock creation for an absent historical journal.
        let exists =
            std::fs::symlink_metadata(root.join(crate::call_mode_restore_journal::ACTIVE_NAME));
        if matches!(exists,Err(ref e)if e.kind()==io::ErrorKind::NotFound) {
            return Ok(None);
        }
        let _waiter = CancelWaiter(cancelled.clone());
        let gate = gate.clone();
        let root = root.to_owned();
        let settled = tokio::task::spawn_blocking(move || blocking_run(gate, root, cancelled))
            .await
            .map_err(|_| PhysicalFailure::cleanup())??;
        // Receipt and archival are exclusively metadata work after real join.
        Ok(Some(settled.finalize()?))
    }
}

struct MetadataWorker {
    ticket: ReconciliationTicket,
    root: PathBuf,
    store: storage::Store,
    original: serde_json::Value,
    physical_sha: String,
    witnesses: [OperationNamespaceWitness; 2],
}
fn metadata_worker(
    ticket: ReconciliationTicket,
    root: PathBuf,
    candidate: ledger::MetadataResume,
) -> Result<MetadataWorker, PhysicalFailure> {
    let operation = acquire_bound_existing_recovery_lock(
        &root,
        ticket.root_proof(),
        RecoveryLockKind::Operation,
        false,
    )?;
    let mut locks = Locks {
        operation: Some(operation),
        adb: None,
    };
    let expected_id = candidate.attempt.id.clone();
    let expected_physical = candidate.physical_sha.clone();
    let outcome = (|| {
        locks.adb = Some(acquire_bound_existing_recovery_lock(
            &root,
            ticket.root_proof(),
            RecoveryLockKind::AdbInterface,
            false,
        )?);
        locks.validate()?;
        // The first candidate was only a pre-lock policy observation. Re-read
        // its actual immutable chain and both placements under both real locks.
        let fresh = ticket
            .root_proof()
            .with_root(&root, ledger::metadata_resume_candidate)
            .map_err(|_| PhysicalFailure::pending())??
            .ok_or_else(PhysicalFailure::pending)?;
        if fresh.attempt.id != expected_id || fresh.physical_sha != expected_physical {
            return Err(PhysicalFailure::pending());
        }
        let original = fresh.attempt.original.clone();
        let store = storage::Store::resume(&root, ticket.root_proof(), fresh.attempt)?;
        Ok::<_, PhysicalFailure>((original, store))
    })();
    let (original, store) = match outcome {
        Ok(v) => v,
        Err(e) => {
            return Err(if locks.partial_release().is_err() {
                PhysicalFailure::cleanup()
            } else {
                e
            });
        }
    };
    let released = locks.release();
    let (original, store, witnesses) = match (Ok::<_, PhysicalFailure>((original, store)), released)
    {
        (Ok((o, s)), Ok(w)) => (o, s, w),
        (Err(e), Ok(_)) => return Err(e),
        (_, Err(e)) => return Err(e),
    };
    Ok(MetadataWorker {
        ticket,
        root,
        store,
        original,
        physical_sha: expected_physical,
        witnesses,
    })
}
impl MetadataWorker {
    fn finalize(mut self) -> Result<CompletedReconciliation, PhysicalFailure> {
        for witness in &self.witnesses {
            witness.validate()?;
        }
        let receipt_sha256 = ledger::finish_publication(
            &self.root,
            self.ticket.root_proof(),
            &mut self.store,
            &self.original,
            &self.physical_sha,
        )?;
        let publication_id = self.store.id().to_owned();
        let completion = CompletedReconciliation {
            ticket: self.ticket,
            root: self.root,
            witnesses: self.witnesses,
            publication_id,
            receipt_sha256,
        };
        completion.verify_for_gate(completion.ticket().root_proof())?;
        Ok(completion)
    }
}
