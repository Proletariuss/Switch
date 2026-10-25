//! The original startup metadata inspector remains read-only and has no
//! physical permit. The separate execution child owns finite native reads.
//! Those paths never restore settings or change ordinary admission. The separate
//! sealed reconcile child records new compensation and retires exact originals
//! only after checked physical/host settlement and durable publication.
mod evidence;
pub(crate) mod execution;
pub(crate) mod reconcile;
#[cfg(test)]
mod tests;

use crate::call_mode_backups::{
    OperationFileLock, OperationNamespaceWitness, RecoveryLockKind,
    acquire_bound_existing_recovery_lock,
};
use crate::call_mode_restore_journal::{DiagnosticSummary, RecoveryJournalCapture};
use crate::module_recovery::{Gate, Reason, RecoveryAuthority};
use serde::Serialize;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

const INSPECTION_BUDGET: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct Failure(String);
impl Failure {
    pub(crate) fn code(&self) -> &'static str {
        "startup_recovery_metadata_unavailable"
    }
    fn invalid(message: &str) -> Self {
        Self(message.to_owned())
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failure {}
impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self(format!("startup recovery metadata: {error}"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryScope {
    MetadataInspection,
}

/// Escaping this handle never extends its nonClone owner's lifetime. No
/// physical actor accepts this type, including actors accepted for cleanup.
#[derive(Clone)]
pub(crate) struct RecoveryRight(Weak<OwnerIdentity>);
struct OwnerIdentity {
    authority: Arc<RecoveryAuthority>,
    gate: Gate,
    scope: RecoveryScope,
    deadline: Instant,
    active: AtomicBool,
}
impl RecoveryRight {
    pub(crate) fn validate_for(&self, gate: &Gate, scope: RecoveryScope) -> Result<(), Failure> {
        let owner = self
            .0
            .upgrade()
            .ok_or_else(|| Failure::invalid("metadata owner ended"))?;
        if !owner.active.load(Ordering::Acquire)
            || Instant::now() >= owner.deadline
            || owner.scope != scope
        {
            return Err(Failure::invalid("metadata right ended or out of scope"));
        }
        owner
            .authority
            .validate(gate)
            .map_err(|_| Failure::invalid("foreign or ended metadata authority"))?;
        owner
            .authority
            .validate(&owner.gate)
            .map_err(|_| Failure::invalid("metadata root authority changed"))
    }
}

/// Every fixed startup slot (including absence), all bounded descendants and
/// actual bytes remain pinned. The journal additionally retains its genuine
/// typed Intent/steps/receipt and validates the original complete hash chain.
/// None of these bytes are a persistent physical-device identity.
pub(crate) struct CapturedRecoveryEvidence {
    authority: Arc<RecoveryAuthority>,
    root: PathBuf,
    tree: evidence::EvidenceTree,
    journal: Option<RecoveryJournalCapture>,
}
impl CapturedRecoveryEvidence {
    fn verify_with(&self, gate: &Gate, check: &impl Fn() -> io::Result<()>) -> Result<(), Failure> {
        check()?;
        self.authority
            .validate(gate)
            .map_err(|_| Failure::invalid("captured metadata authority changed"))?;
        self.authority
            .root_proof()
            .with_root(&self.root, |_| self.tree.verify(check))
            .map_err(|error| Failure(error.to_string()))??;
        if let Some(journal) = &self.journal {
            check()?;
            journal
                .verify_unchanged(self.authority.root_proof())
                .map_err(|error| Failure(error.to_string()))?;
        }
        check()?;
        self.authority
            .validate(gate)
            .map_err(|_| Failure::invalid("metadata authority ended during verification"))
    }
}

/// A logical report of a completed read interval, never completion of module
/// recovery. Counts describe captured filesystem nodes/bytes, not devices.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Diagnostic {
    pub(crate) state: &'static str,
    pub(crate) scope: &'static str,
    pub(crate) ordinary_quarantine_preserved: bool,
    pub(crate) recovery_completed: bool,
    pub(crate) reason_codes: Vec<Reason>,
    pub(crate) evidence_nodes: usize,
    pub(crate) evidence_bytes: usize,
    pub(crate) evidence_sha256: Option<String>,
    pub(crate) journal: Option<DiagnosticSummary>,
    pub(crate) journal_evidence_sha256: Option<String>,
}

#[must_use]
pub(crate) struct RecoveryOwner {
    identity: Arc<OwnerIdentity>,
    operation: Option<OperationFileLock>,
    adb: Option<OperationFileLock>,
    captured: Option<CapturedRecoveryEvidence>,
}
impl RecoveryOwner {
    fn right(&self) -> RecoveryRight {
        RecoveryRight(Arc::downgrade(&self.identity))
    }

    fn check(&self, right: &RecoveryRight) -> io::Result<()> {
        if !right.0.ptr_eq(&Arc::downgrade(&self.identity)) {
            return Err(io::Error::other("foreign metadata owner right"));
        }
        right
            .validate_for(&self.identity.gate, RecoveryScope::MetadataInspection)
            .map_err(io::Error::other)?;
        self.operation
            .as_ref()
            .ok_or_else(|| io::Error::other("operation lock released"))?
            .validate()?;
        self.adb
            .as_ref()
            .ok_or_else(|| io::Error::other("ADB lock released"))?
            .validate()
    }

    fn capture(&mut self, root: &Path, right: &RecoveryRight) -> Result<(), Failure> {
        self.check(right)?;
        let proof = self.identity.authority.root_proof();
        let tree = proof
            .with_root(root, |held| {
                evidence::EvidenceTree::capture(held, &|| self.check(right))
            })
            .map_err(|error| Failure(error.to_string()))??;
        self.check(right)?;
        let journal = crate::call_mode_restore_journal::capture_for_recovery(root, proof)
            .map_err(|error| Failure(error.to_string()))?;
        self.check(right)?;
        let captured = CapturedRecoveryEvidence {
            authority: self.identity.authority.clone(),
            root: root.to_owned(),
            tree,
            journal,
        };
        captured.verify_with(&self.identity.gate, &|| self.check(right))?;
        self.captured = Some(captured);
        Ok(())
    }

    fn inspect_metadata(&self, right: &RecoveryRight) -> Result<Diagnostic, Failure> {
        self.check(right)?;
        let captured = self
            .captured
            .as_ref()
            .ok_or_else(|| Failure::invalid("metadata not captured"))?;
        captured.verify_with(&self.identity.gate, &|| self.check(right))?;
        Ok(Diagnostic {
            state: "metadata_inspected",
            scope: "metadata_only",
            ordinary_quarantine_preserved: self.identity.gate.is_quarantined(),
            recovery_completed: false,
            reason_codes: self
                .identity
                .gate
                .status()
                .map(|status| status.reason_codes.clone())
                .unwrap_or_default(),
            evidence_nodes: captured.tree.nodes,
            evidence_bytes: captured.tree.bytes,
            evidence_sha256: Some(captured.tree.sha256.clone()),
            journal: captured
                .journal
                .as_ref()
                .map(RecoveryJournalCapture::summary),
            journal_evidence_sha256: captured
                .journal
                .as_ref()
                .map(|journal| journal.evidence_sha256().to_owned()),
        })
    }

    fn release(mut self, diagnostic: Diagnostic) -> Result<Inspection, Failure> {
        self.identity.active.store(false, Ordering::Release);
        // Release in reverse acquisition order. Always attempt both releases,
        // including when the first namespace validation/close fails.
        let adb = self
            .adb
            .take()
            .ok_or_else(|| Failure::invalid("missing ADB lock"))?
            .release();
        let operation = self
            .operation
            .take()
            .ok_or_else(|| Failure::invalid("missing operation lock"))?
            .release();
        let witnesses = [operation?, adb?];
        let captured = self
            .captured
            .take()
            .ok_or_else(|| Failure::invalid("missing captured evidence"))?;
        let inspection = Inspection {
            diagnostic,
            captured: Some(captured),
            witnesses: Some(witnesses),
        };
        inspection.verify(&self.identity.gate)?;
        Ok(inspection)
    }
}
impl Drop for RecoveryOwner {
    fn drop(&mut self) {
        self.identity.active.store(false, Ordering::Release);
    }
}

/// Retain this at startup and revalidate at shutdown. Only independent
/// read-only evidence and witnesses remain; no live right or kernel lock.
pub(crate) struct Inspection {
    diagnostic: Diagnostic,
    captured: Option<CapturedRecoveryEvidence>,
    witnesses: Option<[OperationNamespaceWitness; 2]>,
}
impl Inspection {
    pub(crate) fn startup_logging(&self) -> &Diagnostic {
        &self.diagnostic
    }

    pub(crate) fn verify(&self, gate: &Gate) -> Result<(), Failure> {
        let Some(captured) = &self.captured else {
            return Ok(());
        };
        let deadline = Instant::now() + INSPECTION_BUDGET;
        let check = || {
            if Instant::now() >= deadline {
                return Err(io::Error::other("metadata verification deadline"));
            }
            captured
                .authority
                .validate(gate)
                .map_err(|_| io::Error::other("metadata Gate/root ended"))?;
            for witness in self
                .witnesses
                .as_ref()
                .ok_or_else(|| io::Error::other("missing released witnesses"))?
            {
                witness.validate()?;
            }
            Ok(())
        };
        captured.verify_with(gate, &check)
    }
}

pub(crate) struct Coordinator;
impl Coordinator {
    pub(crate) fn inspect(gate: &Gate, root: &Path) -> Result<Inspection, Failure> {
        if !gate.is_quarantined() {
            return Ok(Inspection {
                diagnostic: Diagnostic {
                    state: "not_needed",
                    scope: "metadata_only",
                    ordinary_quarantine_preserved: false,
                    recovery_completed: false,
                    reason_codes: Vec::new(),
                    evidence_nodes: 0,
                    evidence_bytes: 0,
                    evidence_sha256: None,
                    journal: None,
                    journal_evidence_sha256: None,
                },
                captured: None,
                witnesses: None,
            });
        }
        let mut owner = Self::begin_inspection(gate, root, Instant::now() + INSPECTION_BUDGET)?;
        let right = owner.right();
        owner.capture(root, &right)?;
        let diagnostic = owner.inspect_metadata(&right)?;
        owner.release(diagnostic)
    }

    fn begin_inspection(
        gate: &Gate,
        root: &Path,
        deadline: Instant,
    ) -> Result<RecoveryOwner, Failure> {
        if !gate.is_quarantined() || Instant::now() >= deadline {
            return Err(Failure::invalid("metadata inspection scope unavailable"));
        }
        let authority =
            Arc::new(gate.capture_recovery_authority(root).map_err(|_| {
                Failure::invalid("startup Gate has no matching live root authority")
            })?);
        authority
            .validate(gate)
            .map_err(|_| Failure::invalid("startup authority changed"))?;
        // Automatic inspection is read-only even when a fixed lock is absent.
        // Inspect both slots before taking either lock. The bound-existing
        // opener also forbids O_CREAT so removal after this check cannot
        // manufacture new startup evidence.
        authority
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
                        return Err(io::Error::other("original recovery lock is unsafe"));
                    }
                }
                Ok::<_, io::Error>(())
            })
            .map_err(|error| Failure(error.to_string()))??;
        let operation = acquire_bound_existing_recovery_lock(
            root,
            authority.root_proof(),
            RecoveryLockKind::Operation,
            false,
        )?;
        if Instant::now() >= deadline {
            return Err(Failure::invalid("metadata lock acquisition deadline"));
        }
        authority
            .validate(gate)
            .map_err(|_| Failure::invalid("startup authority ended after operation lock"))?;
        let adb = acquire_bound_existing_recovery_lock(
            root,
            authority.root_proof(),
            RecoveryLockKind::AdbInterface,
            false,
        )?;
        Ok(RecoveryOwner {
            identity: Arc::new(OwnerIdentity {
                authority,
                gate: gate.clone(),
                scope: RecoveryScope::MetadataInspection,
                deadline,
                active: AtomicBool::new(true),
            }),
            operation: Some(operation),
            adb: Some(adb),
            captured: None,
        })
    }
}
