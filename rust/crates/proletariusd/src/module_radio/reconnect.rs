//! A normal finite operation's reconnect authority. This is not startup recovery.
use super::*;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct CallModeRebindProof {
    owner: Weak<Inner>,
    root: crate::daemon_lifetime::RootProof,
    lock: Weak<crate::call_mode_backups::OperationFileLock>,
    operation: Weak<crate::module_recovery::OperationGuard>,
    right: crate::module_recovery::OperationRight,
    epoch: u64,
    producer: String,
    identity: Identity,
    binding: Binding,
    pending: Option<RadioAction>,
    reboot: Option<String>,
    failed: Arc<AtomicBool>,
}
impl std::fmt::Debug for CallModeRebindProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallModeRebindProof")
            .finish_non_exhaustive()
    }
}
fn unavailable() -> Failure {
    Failure::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "call_mode_reconnect_unconfirmed",
        "原通话模式操作的USB重连能力未确认，未提交新的配置",
    )
}
// Only an actual finite blocking worker owns this strong operation/lock pin.
// Proof clones cannot extend either admission or the original kernel lock.
pub(crate) struct ReconnectWorkerPin {
    inner: Arc<Inner>,
    epoch: u64,
    identity: Identity,
    operation: Option<Arc<crate::module_recovery::OperationGuard>>,
    lock: Option<Arc<crate::call_mode_backups::OperationFileLock>>,
}
impl Drop for ReconnectWorkerPin {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.epoch == self.epoch {
            if let Some(owner) = state.reservation.as_mut() {
                if owner.identity == self.identity
                    && self
                        .operation
                        .as_ref()
                        .is_some_and(|operation| Arc::ptr_eq(&owner.operation, operation))
                {
                    owner.active_reconnect_pins -= 1;
                }
            }
        }
        // Drop worker-owned resources before publishing that the worker has
        // drained. The state mutex excludes a concurrent release ACK here.
        drop(self.lock.take());
        drop(self.operation.take());
        state.jobs -= 1;
        drop(state);
        self.inner.finished.notify_waiters();
    }
}
impl CallModeRebindProof {
    pub(crate) fn pin_worker(&self) -> Result<ReconnectWorkerPin, Failure> {
        self.validate()?;
        let inner = self.owner.upgrade().ok_or_else(Failure::stale)?;
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check(&state, inner._availability.module_recovery())?;
        let lock = self.lock.upgrade().ok_or_else(Failure::stale)?;
        let jobs = state.jobs.checked_add(1).ok_or_else(Failure::busy)?;
        let owner = state.reservation.as_mut().ok_or_else(Failure::stale)?;
        owner.active_reconnect_pins = owner
            .active_reconnect_pins
            .checked_add(1)
            .ok_or_else(Failure::busy)?;
        let operation = owner.operation.clone();
        state.jobs = jobs;
        drop(state);
        Ok(ReconnectWorkerPin {
            inner,
            epoch: self.epoch,
            identity: self.identity.clone(),
            operation: Some(operation),
            lock: Some(lock),
        })
    }
    pub(crate) fn right(&self) -> crate::module_recovery::OperationRight {
        self.right.clone()
    }
    pub(crate) fn binding(&self) -> &Binding {
        &self.binding
    }
    pub(crate) fn successor_required(&self) -> bool {
        self.reboot.is_some()
    }
    pub(crate) fn same_operation(&self, other: &Self) -> bool {
        self.owner.ptr_eq(&other.owner)
            && self.operation.ptr_eq(&other.operation)
            && self.identity == other.identity
            && self.epoch == other.epoch
    }
    pub(crate) fn validate_for_usb_owner(
        &self,
        owner: &crate::usb_at::Owner,
    ) -> Result<(), Failure> {
        let inner = self.owner.upgrade().ok_or_else(Failure::stale)?;
        if !inner.usb.same_instance(owner) {
            return Err(Failure::stale());
        }
        self.validate()
    }
    pub(crate) fn validate(&self) -> Result<(), Failure> {
        if self.failed.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let inner = self.owner.upgrade().ok_or_else(Failure::stale)?;
        let gate = inner._availability.module_recovery();
        self.check(&inner.state.lock().unwrap_or_else(|e| e.into_inner()), gate)?;
        let namespace_valid = gate
            .root_proof_for(&inner.data_root)
            .is_ok_and(|current| current.same_authority(&self.root))
            && self.root.with_root(&inner.data_root, |_| ()).is_ok()
            && self
                .lock
                .upgrade()
                .is_some_and(|lock| lock.validate().is_ok());
        if !namespace_valid {
            self.failed.store(true, Ordering::Release);
            return Err(unavailable());
        }
        if self.failed.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        self.check(&inner.state.lock().unwrap_or_else(|e| e.into_inner()), gate)
    }
    fn check(&self, state: &State, gate: &crate::module_recovery::Gate) -> Result<(), Failure> {
        let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
        if state.epoch != self.epoch
            || state.producer != self.producer
            || state.stopping
            || state.retiring
            || state.jobs == 0
            || state.binding.as_ref() != Some(&self.binding)
            || state.active_shared_voice_pins != 0
            || owner.active_voice_pins != 0
            || owner.purpose != Purpose::CallMode
            || owner.identity != self.identity
            || !self.operation.ptr_eq(&Arc::downgrade(&owner.operation))
            || !owner
                .operation_lock
                .as_ref()
                .is_some_and(|lock| self.lock.ptr_eq(&Arc::downgrade(lock)))
            || !state
                .executing_operation
                .as_ref()
                .is_some_and(|op| self.operation.ptr_eq(&Arc::downgrade(op)))
            || !self.right.is_active_for(gate, &self.producer)
        {
            return Err(Failure::stale());
        }
        match &self.pending {
            Some(input)
                if !owner.unknown
                    && owner
                        .attempts
                        .get(&input.operation_id)
                        .is_some_and(|attempt| {
                            attempt.input == *input && attempt.result.is_none()
                        }) =>
            {
                Ok(())
            }
            None if owner
                .attempts
                .values()
                .all(|attempt| attempt.result.is_some())
                && owner.reconnect_reboot == self.reboot =>
            {
                Ok(())
            }
            _ => Err(Failure::stale()),
        }
    }
}
impl Service {
    #[cfg(test)]
    pub(crate) fn pending_call_mode_rebind_for_tests(
        &self,
    ) -> Result<CallModeRebindProof, Failure> {
        let (input, epoch) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            let input = owner
                .attempts
                .values()
                .find(|attempt| attempt.result.is_none())
                .ok_or_else(Failure::stale)?
                .input
                .clone();
            (input, state.epoch)
        };
        self.mint_call_mode_rebind_proof(Some(&input), epoch)
    }
    pub(super) fn mint_call_mode_rebind_proof(
        &self,
        pending: Option<&RadioAction>,
        epoch: u64,
    ) -> Result<CallModeRebindProof, Failure> {
        let root = self
            .module_recovery()
            .root_proof_for(&self.0.data_root)
            .map_err(|_| unavailable())?;
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
        // Preserve the existing call-mode namespace error before a preflight
        // crosses the USB adapter, which deliberately redacts proof internals.
        let lock = owner.operation_lock.as_ref().ok_or_else(|| {
            Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "call_mode_operation_lock_unavailable",
                "无法取得通话模式操作锁",
            )
        })?;
        lock.validate().map_err(|_| {
            Failure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "call_mode_operation_lock_unavailable",
                "无法取得通话模式操作锁",
            )
        })?;
        let proof = CallModeRebindProof {
            owner: Arc::downgrade(&self.0),
            root,
            lock: Arc::downgrade(lock),
            operation: Arc::downgrade(&owner.operation),
            right: owner.operation.right(),
            epoch,
            producer: state.producer.clone(),
            identity: owner.identity.clone(),
            binding: state.binding.clone().ok_or_else(Failure::unavailable)?,
            pending: pending.cloned(),
            reboot: owner.reconnect_reboot.clone(),
            failed: Arc::new(AtomicBool::new(false)),
        };
        proof.check(&state, self.module_recovery())?;
        Ok(proof)
    }
    pub(super) async fn prepare_call_mode_reconnect(
        &self,
        input: &RadioAction,
        action: &Action,
        epoch: u64,
    ) -> Result<(), Failure> {
        let (binding, anchor, deadline, cancelled) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            (
                state.binding.clone().ok_or_else(Failure::unavailable)?,
                owner.reconnect.clone(),
                state.job_deadline.ok_or_else(Failure::unavailable)?,
                state.epoch_cancel.clone(),
            )
        };
        if !matches!(binding, Binding::Usb { .. }) {
            return Ok(());
        }
        let proof = self.mint_call_mode_rebind_proof(Some(input), epoch)?;
        let anchor = match anchor {
            Some(anchor) => anchor,
            None => self
                .0
                .usb
                .capture_call_mode_reconnect(proof.clone(), deadline, cancelled)
                .await
                .map_err(Failure::from)?,
        };
        proof.validate()?;
        anchor.validate_for_write(action).map_err(Failure::from)?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        proof.check(&state, self.module_recovery())?;
        let owner = state.reservation.as_mut().ok_or_else(Failure::stale)?;
        if owner
            .reconnect
            .as_ref()
            .is_some_and(|current| !current.same_instance(&anchor))
        {
            return Err(Failure::stale());
        }
        owner.reconnect = Some(anchor);
        Ok(())
    }
}
