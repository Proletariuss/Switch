//! Native startup owns the physical session independently of compatibility workers.
use super::*;

impl Owner {
    /// Open a USB AT candidate before committing the module's transport choice.
    /// The same serialized permit installs native ownership with the resulting
    /// session, so another caller cannot replace the handle between probe and
    /// selection. A definite no-device result leaves the owner unclaimed and
    /// lets module startup rescan newly enumerated serial ports.
    pub(crate) async fn open_native_candidate(&self) -> Result<String, Failure> {
        let producer = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, &state.producer)?;
            state.producer.clone()
        };
        let permit = self
            .admit(&producer, None, "", Instant::now() + MAX_WAIT)
            .await?;
        self.run(permit, move |inner, epoch| {
            {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                check_epoch(&state, epoch)?;
                if let Some((_, error)) = state.close_failures.first() {
                    return Err(error.clone());
                }
                if state.native_managed
                    || !state.session.is_empty()
                    || state.lease.is_some()
                    || state.sms.is_some()
                {
                    return Err(Failure::busy());
                }
            }
            ensure_open(inner, epoch)?;
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check_epoch(&state, epoch)?;
            if state.session.is_empty() {
                return Err(Failure::stale());
            }
            state.native_managed = true;
            Ok(state.session.clone())
        })
        .await
    }

    pub(crate) fn native_managed(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .native_managed
    }

    pub(crate) fn claim_native_management(&self) -> Result<(), Failure> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, &state.producer)?;
        if state.native_managed {
            return Ok(());
        }
        // Taking ownership is new admission. Finishing or releasing a session
        // that was already accepted uses the existing owner paths instead.
        let _admission = new_io_authority(&state, &state.producer)?;
        if !state.session.is_empty() || state.busy || state.lease.is_some() || state.sms.is_some() {
            return Err(Failure::busy());
        }
        state.native_managed = true;
        Ok(())
    }

    pub(crate) async fn open_native_session(&self) -> Result<String, Failure> {
        let producer = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&state, &state.producer)?;
            if !state.native_managed {
                return Err(Failure::invalid());
            }
            state.producer.clone()
        };
        let value = self
            .open(
                OpenRequest {
                    producer_id: producer,
                    lease_id: String::new(),
                },
                false,
            )
            .await?;
        value
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(Failure::stale)
    }

    pub(crate) fn native_sms_ready(&self) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.native_managed
            && !state.stopping
            && !state.revoking
            && !state.session.is_empty()
            && state.native_sms_ready_session.as_deref() == Some(state.session.as_str())
    }

    /// Called only after RF bootstrap has acknowledged this exact session.
    pub(crate) fn commit_native_sms_ready(&self, session: &str) -> Result<(), Failure> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, &state.producer)?;
        if !state.native_managed || session.is_empty() || state.session != session {
            return Err(Failure::stale());
        }
        if state.native_sms_ready_session.as_deref() == Some(session) {
            return Ok(());
        }
        let next_config = state
            .config_high_water
            .checked_add(1)
            .ok_or_else(Failure::stale)?;
        let next_observation = state
            .sms_observation_generation
            .checked_add(1)
            .ok_or_else(Failure::stale)?;
        state.sms = Some(SmsConfig {
            serial: false,
            adopt: state.adopt,
            interval_ms: 8000,
        });
        state.config_high_water = next_config;
        state.sms_observation_generation = next_observation;
        state.native_sms_ready_session = Some(session.to_owned());
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(())
    }
}
