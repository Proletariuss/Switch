//! A single startup reconciliation stays in the original Gate authority.
use super::*;

/// A live logical claim, never a physical device permit. The reconciliation
/// owner must still hold the original kernel locks, immutable inputs, bounded
/// policy and exact native/voice session through real resource settlement.
pub(crate) struct ReconciliationTicket {
    authority: std::sync::Weak<Authority>,
    proof: crate::daemon_lifetime::RootProof,
    root: PathBuf,
    attempt_id: String,
    epoch: u64,
    consumed: AtomicBool,
}
impl std::fmt::Debug for ReconciliationTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconciliationTicket")
            .field("epoch", &self.epoch)
            .field("consumed", &self.consumed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
impl ReconciliationTicket {
    pub(crate) fn validate(&self, gate: &Gate) -> Result<(), ()> {
        let authority = self.authority.upgrade().ok_or(())?;
        if !Arc::ptr_eq(&authority, &gate.0)
            || self.consumed.load(Ordering::Acquire)
            || !authority.reconciliation_started.load(Ordering::Acquire)
            || authority.reconciliation_epoch.load(Ordering::Acquire) != self.epoch
            || authority.startup.is_none()
            || authority.startup_reconciled.get().is_some()
            || authority.revoked.get().is_some()
            || !authority
                .root_proof
                .as_ref()
                .is_some_and(|p| p.same_authority(&self.proof))
        {
            return Err(());
        }
        self.proof.with_root(&self.root, |_| ()).map_err(|_| ())
    }
    pub(crate) fn root_proof(&self) -> &crate::daemon_lifetime::RootProof {
        &self.proof
    }
    pub(crate) fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
}
impl Drop for ReconciliationTicket {
    fn drop(&mut self) {
        if self.consumed.load(Ordering::Acquire) {
            return;
        }
        let Some(authority) = self.authority.upgrade() else {
            return;
        };
        // Dropping a logical ticket cannot certify native/voice/host cleanup
        // or authorize a new attempt in this Gate. The worker retains it until
        // actual work ends; an incomplete attempt remains quarantined.
        let _admission = authority
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = authority.revoked.set(Status {
            state: CODE,
            reason_codes: vec![Reason::ReconciliationPending],
            historical_recovery_confirmed: false,
        });
    }
}

impl Gate {
    pub(crate) fn begin_reconciliation(&self, root: &Path) -> Result<ReconciliationTicket, ()> {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_quarantined()
            || self.0.revoked.get().is_some()
            || self.0.restore_in_progress.load(Ordering::Acquire)
            || self.0.reconciliation_started.load(Ordering::Acquire)
        {
            return Err(());
        }
        let epoch = self.0.reconciliation_epoch.load(Ordering::Acquire);
        epoch.checked_add(1).ok_or(())?;
        let proof = self.0.root_proof.as_ref().ok_or(())?;
        proof.with_root(root, |_| ()).map_err(|_| ())?;
        let attempt_id = crate::multistandby_store::random_token().map_err(|_| ())?;
        self.0.reconciliation_started.store(true, Ordering::Release);
        Ok(ReconciliationTicket {
            authority: Arc::downgrade(&self.0),
            proof: proof.clone(),
            root: root.into(),
            attempt_id,
            epoch,
            consumed: AtomicBool::new(false),
        })
    }

    /// Only an opaque completion produced after actual worker join, checked
    /// resource release and durable publication can advance this same Gate.
    pub(crate) fn consume_reconciliation(
        &self,
        completion: &crate::startup_recovery::reconcile::CompletedReconciliation,
    ) -> Result<(), ()> {
        let ticket = completion.ticket();
        ticket.validate(self)?;
        completion
            .verify_for_gate(ticket.root_proof())
            .map_err(|_| ())?;
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        ticket.validate(self)?;
        // This is bounded filesystem verification, without a Gate callback or
        // native I/O. No current-process normal producer starts before commit.
        completion
            .verify_for_gate(ticket.root_proof())
            .map_err(|_| ())?;
        if completion.root() != ticket.root {
            return Err(());
        }
        // Publication covers only its sealed slots. Recheck the complete
        // original evidence policy before activation so unrelated legacy,
        // unreadable or invalid-marker evidence cannot be waived by a valid
        // completion for a different slot. This reader has no Gate callback.
        let reasons = ticket
            .root_proof()
            .with_root(&ticket.root, |directory| {
                evidence::inspect(
                    directory,
                    &crate::call_mode_runtime_manifest::manifest().files,
                )
            })
            .map_err(|_| ())?;
        if !reasons.is_empty() {
            return Err(());
        }
        let next = ticket.epoch.checked_add(1).ok_or(())?;
        self.0
            .reconciliation_epoch
            .compare_exchange(ticket.epoch, next, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ())?;
        self.0.startup_reconciled.set(()).map_err(|_| ())?;
        ticket.consumed.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn root() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/iteration-592-gate-ticket")
            .join(crate::snapshot::new_instance_id());
        fs::create_dir_all(&root).unwrap();
        root
    }
    fn pending(root: &Path) -> (crate::daemon_lifetime::Lease, Gate) {
        let lease = crate::daemon_lifetime::Lease::acquire(root).unwrap();
        fs::write(root.join("call-mode-operation.lock"), b"").unwrap();
        let gate = Gate::inspect_with_lifetime(&lease);
        assert!(gate.is_quarantined());
        (lease, gate)
    }
    #[test]
    fn ticket_requires_same_actual_quarantined_gate_and_cannot_admit_normal_work() {
        let root = root();
        let (lease, gate) = pending(&root);
        assert!(Gate::default().begin_reconciliation(&root).is_err());
        assert!(Gate::inspect(&root).begin_reconciliation(&root).is_err());
        let ticket = gate.begin_reconciliation(&root).unwrap();
        assert_eq!(ticket.epoch(), 0);
        assert_eq!(ticket.attempt_id().len(), 64);
        assert!(ticket.validate(&gate.clone()).is_ok());
        assert!(
            ticket
                .validate(&Gate::inspect_with_lifetime(&lease))
                .is_err()
        );
        assert!(gate.begin_reconciliation(&root).is_err());
        assert!(gate.admit_operation("normal-producer").is_err());
        drop(ticket);
        assert!(gate.is_quarantined());
        assert!(gate.begin_reconciliation(&root).is_err());
        drop(lease);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn path_replacement_and_new_lifetime_cannot_rebind_an_unconsumed_ticket() {
        let root = root();
        let (lease, gate) = pending(&root);
        let ticket = gate.begin_reconciliation(&root).unwrap();
        let old = root.with_extension("original");
        fs::rename(&root, &old).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(ticket.validate(&gate).is_err());
        fs::remove_dir(&root).unwrap();
        fs::rename(&old, &root).unwrap();
        assert!(ticket.validate(&gate).is_ok());
        drop(lease);
        assert!(ticket.validate(&gate).is_err());
        let replacement = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        assert!(ticket.validate(&gate).is_err());
        assert!(
            ticket
                .validate(&Gate::inspect_with_lifetime(&replacement))
                .is_err()
        );
        drop(ticket);
        drop(replacement);
        fs::remove_dir_all(root).unwrap();
    }
}

 
/// A startup current-state read claim, deliberately distinct from a recovery
/// ticket: it never records historical compensation or creates durable permits.
pub(crate) struct LegacyCurrentClaim {
    authority: std::sync::Weak<Authority>,
    proof: crate::daemon_lifetime::RootProof,
    root: PathBuf,
    epoch: u64,
    consumed: AtomicBool,
}
impl LegacyCurrentClaim {
    fn common(&self, gate: &Gate) -> Result<Arc<Authority>, ()> {
        let authority = self.authority.upgrade().ok_or(())?;
        if !Arc::ptr_eq(&authority, &gate.0) || authority.revoked.get().is_some()
            || !authority.reconciliation_started.load(Ordering::Acquire)
            || !authority.root_proof.as_ref().is_some_and(|p| p.same_authority(&self.proof)) {
            return Err(());
        }
        self.proof.with_root(&self.root, |_| ()).map_err(|_| ())?;
        Ok(authority)
    }
    pub(crate) fn validate(&self, gate: &Gate) -> Result<(), ()> {
        let a = self.common(gate)?;
        if self.consumed.load(Ordering::Acquire) || a.startup.is_none()
            || a.startup_reconciled.get().is_some()
            || a.reconciliation_epoch.load(Ordering::Acquire) != self.epoch { return Err(()); }
        Ok(())
    }
    pub(crate) fn validate_activated(&self, gate: &Gate) -> Result<(), ()> {
        let a = self.common(gate)?;
        if !self.consumed.load(Ordering::Acquire) || a.startup_reconciled.get().is_none()
            || a.reconciliation_epoch.load(Ordering::Acquire) != self.epoch.checked_add(1).ok_or(())?
            || gate.is_admission_revoked() { return Err(()); }
        Ok(())
    }
    pub(crate) fn root_proof(&self) -> &crate::daemon_lifetime::RootProof { &self.proof }
    pub(crate) fn revoke(&self, gate: &Gate) {
        let Some(a) = self.authority.upgrade() else { return; };
        if !Arc::ptr_eq(&a, &gate.0) { return; }
        let _admission = a.admission.lock().unwrap_or_else(|e| e.into_inner());
        let _ = a.revoked.set(Status { state: CODE,
            reason_codes: vec![Reason::ReconciliationPending], historical_recovery_confirmed: false });
    }
}
impl Gate {
    pub(crate) fn legacy_current_eligible(&self) -> bool {
        self.is_quarantined() && self.0.revoked.get().is_none()
            && !self.0.restore_in_progress.load(Ordering::Acquire)
            && !self.0.reconciliation_started.load(Ordering::Acquire)
            && self.0.startup.as_ref().is_some_and(|s| !s.reason_codes.is_empty()
                && s.reason_codes.iter().all(|reason| matches!(reason,
                    Reason::LegacyCallModeLock | Reason::LegacyAdbInterfaceLock
                    | Reason::LegacyLockProvenancePending | Reason::ModuleBackupPresent
                    | Reason::LegacyVoiceRuntime | Reason::ReconciliationPending)))
    }
    pub(crate) fn begin_legacy_current(&self, root: &Path) -> Result<LegacyCurrentClaim, ()> {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        if !self.legacy_current_eligible() { return Err(()); }
        let epoch = self.0.reconciliation_epoch.load(Ordering::Acquire);
        epoch.checked_add(1).ok_or(())?;
        let proof = self.0.root_proof.as_ref().ok_or(())?;
        proof.with_root(root, |_| ()).map_err(|_| ())?;
        self.0.reconciliation_started.store(true, Ordering::Release);
        Ok(LegacyCurrentClaim { authority: Arc::downgrade(&self.0), proof: proof.clone(),
            root: root.to_owned(), epoch, consumed: AtomicBool::new(false) })
    }
    pub(crate) fn consume_legacy_current(
        &self, completed: crate::startup_recovery::legacy_current::Completed,
    ) -> Result<crate::startup_recovery::legacy_current::Seed, ()> {
        completed.verify(self)?;
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        completed.verify(self)?;
        let claim = completed.claim();
        let next = claim.epoch.checked_add(1).ok_or(())?;
        self.0.reconciliation_epoch.compare_exchange(claim.epoch, next, Ordering::AcqRel, Ordering::Acquire).map_err(|_| ())?;
        self.0.startup_reconciled.set(()).map_err(|_| ())?;
        claim.consumed.store(true, Ordering::Release);
        // No file is removed or recategorized and history remains unconfirmed.
        // A fresh lifetime must perform the complete actual-device read again.
        Ok(completed.into_seed())
    }
}
