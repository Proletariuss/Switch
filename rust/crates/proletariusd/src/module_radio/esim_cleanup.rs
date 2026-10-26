//! Retain eSIM release until the exact physical owner has stopped borrowing it.
use super::*;

struct ReleaseObligation {
    inner: Arc<Inner>,
    epoch: u64,
    identity: Identity,
}
impl Drop for ReleaseObligation {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state
            .esim_release_pending
            .as_ref()
            .is_some_and(|(epoch, identity)| *epoch == self.epoch && *identity == self.identity)
        {
            state.esim_release_pending = None;
        }
        state.jobs -= 1;
        drop(state);
        self.inner.finished.notify_waiters();
    }
}

impl Service {
    /// Call only after the real AT/APDU worker and its logical-channel cleanup
    /// have returned. This retains the release obligation, never the HTTP waiter.
    pub(crate) async fn release_esim(&self, reservation: EsimReservation) -> Result<(), Failure> {
        let identity = Identity {
            token: reservation.token.clone(),
            sequence: reservation.sequence,
        };
        {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            self.validate_esim_release(&state, &reservation, &identity)?;
            if state.esim_release_pending.is_some() {
                return Err(Failure::busy());
            }
            // Exactly one non-clone native reservation can own this obligation.
            // Register before spawning so finalization/retirement must drain it.
            state.esim_release_pending = Some((reservation.epoch, identity.clone()));
            state.jobs += 1;
        }
        let obligation = ReleaseObligation {
            inner: self.0.clone(),
            epoch: reservation.epoch,
            identity: identity.clone(),
        };
        let service = self.clone();
        tokio::spawn(async move {
            let _obligation = obligation;
            service.finish_esim_release(reservation, identity).await
        })
        .await
        .map_err(|_| Failure::stopped())?
    }

    fn validate_esim_release(
        &self,
        state: &State,
        reservation: &EsimReservation,
        identity: &Identity,
    ) -> Result<(), Failure> {
        self.require_module_admission()?;
        if state.finalized {
            return Err(Failure::stopped());
        }
        if state.retiring
            || state.epoch != reservation.epoch
            || state.producer != reservation.producer_id
        {
            return Err(Failure::stale());
        }
        let owner = state
            .reservation
            .as_ref()
            .filter(|owner| owner.purpose == Purpose::Esim && owner.identity == *identity)
            .ok_or_else(Failure::stale)?;
        if !owner
            .operation
            .right()
            .is_active_for(self.module_recovery(), &reservation.producer_id)
        {
            return Err(Self::admission_failure());
        }
        Ok(())
    }

    async fn finish_esim_release(
        &self,
        reservation: EsimReservation,
        identity: Identity,
    ) -> Result<(), Failure> {
        loop {
            // Register before checking pins to avoid missing their final drop.
            let finished = self.0.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            // This one retained cleanup has no queue-admission/30s timeout:
            // saturation cannot discard the only token that can settle it.
            let gate = self.0.gate.lock().await;
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                self.validate_esim_release(&state, &reservation, &identity)?;
                let owner = state.reservation.as_ref().unwrap();
                if state.active_shared_voice_pins == 0
                    && owner.active_voice_pins == 0
                    && owner.active_reconnect_pins == 0
                {
                    // Same transition as an exact Reservation(release), under
                    // the actor gate and the same lock as identity validation.
                    state.reservation = None;
                    state.waters.insert(
                        Purpose::Esim,
                        Water {
                            sequence: identity.sequence,
                            token: identity.token.clone(),
                            released: true,
                        },
                    );
                    self.0.wake.notify_one();
                    return Ok(());
                }
            }
            drop(gate);
            finished.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn esim_release_pending_for_test(&self) -> bool {
        self.0.state.lock().unwrap().esim_release_pending.is_some()
    }
}

#[cfg(test)]
mod tests;
