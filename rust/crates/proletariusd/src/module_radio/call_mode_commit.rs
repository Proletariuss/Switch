//! Configuration commit evidence minted by the actual, executing radio owner.
//! Neither a clone nor a held file descriptor can create a new operation right.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConfigAction {
    Voice(VoiceConfiguration),
    Usb {
        expected: UsbComposition,
        target: UsbComposition,
    },
    AdbAuthorize,
    Reboot,
}
impl ConfigAction {
    fn of(action: &Action) -> Option<Self> {
        match action {
            Action::CallModeVoice { target } => Some(Self::Voice(*target)),
            Action::CallModeUsbCompositionWrite { expected, target } => Some(Self::Usb {
                expected: *expected,
                target: *target,
            }),
            Action::CallModeAdbAuthorize => Some(Self::AdbAuthorize),
            Action::Reboot => Some(Self::Reboot),
            _ => None,
        }
    }
    fn request(input: &RadioAction) -> Option<Self> {
        match input.kind.as_str() {
            "call_mode_voice" => input.voice_target.map(Self::Voice),
            "call_mode_usb_composition_write" => Some(Self::Usb {
                expected: input.usb_expected?,
                target: input.usb_target?,
            }),
            "call_mode_adb_authorize" => Some(Self::AdbAuthorize),
            "reboot" => Some(Self::Reboot),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct CallModeCommitProof {
    owner: Weak<Inner>,
    root: crate::daemon_lifetime::RootProof,
    lock: Arc<crate::call_mode_backups::OperationFileLock>,
    operation: Weak<crate::module_recovery::OperationGuard>,
    right: crate::module_recovery::OperationRight,
    input: RadioAction,
    action: ConfigAction,
    epoch: u64,
    binding: Binding,
    failed: Arc<AtomicBool>,
    reconnect: Option<crate::usb_at::CallModeUsbAnchor>,
    restore_required: bool,
    restore_journal: Option<crate::call_mode_restore_journal::Journal>,
}
impl std::fmt::Debug for CallModeCommitProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallModeCommitProof")
            .finish_non_exhaustive()
    }
}
fn unavailable() -> Failure {
    Failure::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "call_mode_operation_lock_unavailable",
        "无法取得通话模式操作锁",
    )
}

impl CallModeCommitProof {
    pub(crate) fn restore_journal(
        &self,
    ) -> Result<Option<crate::call_mode_restore_journal::Journal>, Failure> {
        if self.restore_required && self.restore_journal.is_none() {
            return Err(restore_journal_failure());
        }
        if let Some(journal) = &self.restore_journal {
            journal
                .validate_root(&self.root)
                .map_err(|_| restore_journal_failure())?;
        }
        Ok(self.restore_journal.clone())
    }
    pub(crate) fn prepare_restore_write(
        &self,
        kind: crate::call_mode_restore_journal::MutationKind,
    ) -> Result<
        Option<(
            crate::call_mode_restore_journal::Mutation,
            crate::call_mode_restore_journal::PreparedWrite,
        )>,
        Failure,
    > {
        use crate::call_mode_restore_journal::{Mutation, MutationKind};
        let action = match &self.action {
            ConfigAction::Voice(target) => Action::CallModeVoice { target: *target },
            ConfigAction::Usb { expected, target } => Action::CallModeUsbCompositionWrite {
                expected: *expected,
                target: *target,
            },
            ConfigAction::Reboot => Action::Reboot,
            ConfigAction::AdbAuthorize => Action::CallModeAdbAuthorize,
        };
        self.validate(&action)?;
        let suffix = match (&self.action, &kind) {
            (ConfigAction::Voice(target), MutationKind::Volte { disabled })
                if target.volte_disabled == *disabled =>
            {
                "volte"
            }
            (ConfigAction::Voice(target), MutationKind::Ims { target: ims })
                if target.ims == *ims =>
            {
                "ims"
            }
            (
                ConfigAction::Usb { expected, target },
                MutationKind::UsbComposition {
                    expected: e,
                    target: t,
                },
            ) if expected == e && target == t => "usb",
            (ConfigAction::Reboot, MutationKind::Reboot) => "reboot",
            (ConfigAction::AdbAuthorize, MutationKind::AdbAuthorize) => "adb",
            _ => return Err(Failure::stale()),
        };
        let Some(journal) = self.restore_journal()? else {
            return Ok(None);
        };
        let mutation = Mutation {
            operation_id: format!("{}-{suffix}", self.input.operation_id),
            kind,
        };
        let prepared = journal
            .prepare_step(mutation.clone())
            .map_err(|_| restore_journal_failure())?;
        if matches!(mutation.kind, MutationKind::Reboot) {
            let inner = self.owner.upgrade().ok_or_else(Failure::stale)?;
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            self.check_state(&state, inner._availability.module_recovery())?;
            let owner = state.reservation.as_mut().ok_or_else(Failure::stale)?;
            owner.restore_reboot = Some(prepared.clone());
            owner.restore_reboot_verified = false;
        }
        Ok(Some((mutation, prepared)))
    }
    pub(crate) fn validate_for_usb_owner(
        &self,
        actual: &crate::usb_at::Owner,
    ) -> Result<(), Failure> {
        self.check_actor(|inner| inner.usb.same_instance(actual))
    }
    pub(crate) fn validate_for_modem_actor(
        &self,
        actual: &crate::modem_actor::ModemActor,
    ) -> Result<(), Failure> {
        self.check_actor(|inner| inner.modem.same_instance(actual))
    }
    fn check_actor(&self, matches: impl FnOnce(&Inner) -> bool) -> Result<(), Failure> {
        if self.failed.load(Ordering::Acquire)
            || !self.owner.upgrade().is_some_and(|inner| matches(&inner))
        {
            self.failed.store(true, Ordering::Release);
            return Err(Failure::stale());
        }
        Ok(())
    }
    /// Physical owners must compare their own Gate as well as their session.
    /// Serial session counters alone are not identities across distinct actors.
    pub(crate) fn validate_for_gate(
        &self,
        gate: &crate::module_recovery::Gate,
    ) -> Result<(), Failure> {
        if self.failed.load(Ordering::Acquire)
            || !self.right.is_active_for(gate, &self.input.producer_id)
        {
            self.failed.store(true, Ordering::Release);
            return Err(Failure::stale());
        }
        Ok(())
    }
    pub(crate) fn validate(&self, action: &Action) -> Result<(), Failure> {
        let result = self.check(action);
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    pub(crate) fn validate_for_binding(
        &self,
        action: &Action,
        binding: &Binding,
    ) -> Result<(), Failure> {
        if &self.binding != binding {
            self.failed.store(true, Ordering::Release);
            return Err(Failure::stale());
        }
        self.validate(action)
    }
    fn check(&self, action: &Action) -> Result<(), Failure> {
        if self.failed.load(Ordering::Acquire)
            || ConfigAction::of(action).as_ref() != Some(&self.action)
        {
            return Err(Failure::stale());
        }
        let inner = self.owner.upgrade().ok_or_else(Failure::stale)?;
        let gate = inner._availability.module_recovery();
        {
            let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            self.check_state(&state, gate)?;
        }
        // Never acquire the asynchronous service gate from a physical port
        // callback. Filesystem metadata runs outside the radio state mutex.
        let current = gate
            .root_proof_for(&inner.data_root)
            .map_err(|()| unavailable())?;
        if !self.root.same_authority(&current) {
            return Err(unavailable());
        }
        self.root
            .with_root(&inner.data_root, |_| ())
            .map_err(|_| unavailable())?;
        self.lock.validate().map_err(|_| unavailable())?;
        self.restore_journal()?;
        if matches!(self.binding, Binding::Usb { .. }) {
            self.reconnect
                .as_ref()
                .ok_or_else(unavailable)?
                .validate_for_write(action)
                .map_err(Failure::from)?;
        }
        // Retirement/release may have happened during the filesystem checks.
        let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check_state(&state, gate)?;
        if self.failed.load(Ordering::Acquire) {
            return Err(Failure::stale());
        }
        Ok(())
    }
    fn check_state(
        &self,
        state: &State,
        gate: &crate::module_recovery::Gate,
    ) -> Result<(), Failure> {
        if state.epoch != self.epoch
            || state.producer != self.input.producer_id
            || state.retiring
            || state.stopping
            || state.jobs == 0
            || state.binding.as_ref() != Some(&self.binding)
            || state.observed.uncertain
            || state.active_shared_voice_pins != 0
            || !self.right.is_active_for(gate, &self.input.producer_id)
        {
            return Err(Failure::stale());
        }
        let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
        if owner.restore_journal_required != self.restore_required
            || self.restore_required
                && (self.restore_journal.is_none() || owner.restore_journal.is_none())
            || owner
                .restore_journal
                .as_ref()
                .zip(self.restore_journal.as_ref())
                .is_some_and(|(a, b)| !a.same_instance(b))
            || owner.restore_journal.is_some() != self.restore_journal.is_some()
        {
            return Err(restore_journal_failure());
        }
        if owner.purpose != Purpose::CallMode
            || owner.identity.token != self.input.token
            || owner.identity.sequence != self.input.sequence
            || owner.unknown
            || owner.active_voice_pins != 0
            || !self.operation.ptr_eq(&Arc::downgrade(&owner.operation))
            || !owner
                .operation_lock
                .as_ref()
                .is_some_and(|lock| Arc::ptr_eq(lock, &self.lock))
            || matches!(self.binding, Binding::Usb { .. })
                && !owner
                    .reconnect
                    .as_ref()
                    .zip(self.reconnect.as_ref())
                    .is_some_and(|(a, b)| a.same_instance(b))
            || !state
                .executing_operation
                .as_ref()
                .is_some_and(|operation| self.operation.ptr_eq(&Arc::downgrade(operation)))
            || !owner
                .attempts
                .get(&self.input.operation_id)
                .is_some_and(|attempt| attempt.input == self.input && attempt.result.is_none())
        {
            return Err(Failure::stale());
        }
        Ok(())
    }
}

impl Service {
    pub(super) fn validate_restore_attachment(&self) -> Result<(), Failure> {
        let (required, journal) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            (
                owner.restore_journal_required,
                owner.restore_journal.clone(),
            )
        };
        if required {
            let journal = journal.ok_or_else(restore_journal_failure)?;
            let root = self
                .module_recovery()
                .root_proof_for(&self.0.data_root)
                .map_err(|_| restore_journal_failure())?;
            journal
                .validate_root(&root)
                .map_err(|_| restore_journal_failure())?;
        }
        Ok(())
    }
    pub(super) fn mint_call_mode_commit(
        &self,
        input: &RadioAction,
        action: &Action,
        epoch: u64,
    ) -> Result<CallModeCommitProof, Failure> {
        let action = ConfigAction::of(action).ok_or_else(Failure::invalid)?;
        if input.purpose != Purpose::CallMode
            || ConfigAction::request(input).as_ref() != Some(&action)
        {
            return Err(Failure::stale());
        }
        let root = self
            .module_recovery()
            .root_proof_for(&self.0.data_root)
            .map_err(|()| unavailable())?;
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
        let proof = CallModeCommitProof {
            owner: Arc::downgrade(&self.0),
            root,
            lock: owner.operation_lock.clone().ok_or_else(unavailable)?,
            operation: Arc::downgrade(&owner.operation),
            right: owner.operation.right(),
            input: input.clone(),
            action,
            epoch,
            binding: state.binding.clone().ok_or_else(Failure::unavailable)?,
            failed: Arc::new(AtomicBool::new(false)),
            reconnect: owner.reconnect.clone(),
            restore_required: owner.restore_journal_required,
            restore_journal: owner.restore_journal.clone(),
        };
        proof.check_state(&state, self.module_recovery())?;
        drop(state);
        proof.lock.validate().map_err(|_| unavailable())?;
        Ok(proof)
    }
    /// Observe only an actual pending owner job in actor tests. This cannot
    /// create a reservation, replace its Guard, or bypass the production mint.
    #[cfg(test)]
    pub(crate) fn pending_call_mode_commit_for_tests(
        &self,
        action: &Action,
    ) -> Result<CallModeCommitProof, Failure> {
        let (input, epoch) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let reservation = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            let input = reservation
                .attempts
                .values()
                .find(|attempt| {
                    attempt.result.is_none()
                        && ConfigAction::request(&attempt.input) == ConfigAction::of(action)
                })
                .ok_or_else(Failure::stale)?
                .input
                .clone();
            (input, state.epoch)
        };
        self.mint_call_mode_commit(&input, action, epoch)
    }
}

#[cfg(test)]
#[path = "call_mode_commit_tests.rs"]
mod tests;
