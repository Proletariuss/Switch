//! SMS collection authority belongs to the physical owner and its committed
//! configuration, independently of the compatibility process's availability.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SmsPollBinding {
    producer: String,
    epoch: u64,
    config_seq: u64,
    revision: u64,
    config: SmsConfig,
    ready_session: Option<String>,
    observed_session: String,
    observed_identity_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SmsObservationToken {
    binding: SmsPollBinding,
    session: String,
    identity_generation: u64,
}

pub(crate) struct SmsObservation {
    pub(crate) value: Value,
    pub(crate) token: SmsObservationToken,
}

impl SmsPollBinding {
    pub(crate) fn observation_token(&self) -> SmsObservationToken {
        SmsObservationToken {
            binding: self.clone(),
            session: self.observed_session.clone(),
            identity_generation: self.observed_identity_generation,
        }
    }
    pub(super) fn check(&self, state: &State) -> Result<(), Failure> {
        check_epoch(state, self.epoch)?;
        if state.producer != self.producer
            || state.config_high_water != self.config_seq
            || state.sms_observation_generation != self.revision
            || state.sms != Some(self.config)
            || (state.native_managed
                && (self.ready_session.as_deref() != Some(state.session.as_str())
                    || state.native_sms_ready_session.as_deref() != Some(state.session.as_str())
                    || state.identity_generation != self.observed_identity_generation
                    || state.session.is_empty()))
        {
            return Err(Failure::stale());
        }
        Ok(())
    }
}

impl SmsObservationToken {
    pub(super) fn check(&self, state: &State) -> Result<(), Failure> {
        self.binding.check(state)?;
        if state.session != self.session || state.identity_generation != self.identity_generation {
            return Err(Failure::stale());
        }
        Ok(())
    }
}

impl Owner {
    pub(crate) fn sms_poll_binding(
        &self,
        producer: &str,
    ) -> Result<Option<SmsPollBinding>, Failure> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        check(&state, producer)?;
        let Some(config) = state.sms else {
            return Ok(None);
        };
        if state.native_managed
            && (state.session.is_empty()
                || state.native_sms_ready_session.as_deref() != Some(state.session.as_str()))
        {
            return Ok(None);
        }
        Ok(Some(SmsPollBinding {
            producer: producer.to_owned(),
            epoch: state.epoch,
            config_seq: state.config_high_water,
            revision: state.sms_observation_generation,
            config,
            ready_session: state.native_managed.then(|| state.session.clone()),
            observed_session: state.session.clone(),
            observed_identity_generation: state.identity_generation,
        }))
    }

    pub(crate) fn with_current_sms_observation<R>(
        &self,
        token: &SmsObservationToken,
        publish: impl FnOnce() -> R,
    ) -> Result<R, Failure> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        token.check(&state)?;
        Ok(publish())
    }

    pub(crate) fn sms_observation_current(&self, token: &SmsObservationToken) -> bool {
        self.with_current_sms_observation(token, || ()).is_ok()
    }

    pub(crate) async fn snapshot_bound(
        &self,
        binding: &SmsPollBinding,
    ) -> Result<SmsObservation, Failure> {
        let observe = |state: &State, value| SmsObservation {
            value,
            token: SmsObservationToken {
                binding: binding.clone(),
                session: state.session.clone(),
                identity_generation: state.identity_generation,
            },
        };
        let permit = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            binding.check(&state)?;
            if state.session != binding.observed_session
                || state.identity_generation != binding.observed_identity_generation
            {
                return Err(Failure::stale());
            }
            if binding.config.serial {
                return Ok(observe(
                    &state,
                    raw_snapshot(
                        &binding.producer,
                        "serial",
                        Some(binding.config),
                        "",
                        vec![],
                    ),
                ));
            }
            if state.busy
                || state.lease.is_some()
                || !state.inhibits.is_empty()
                || state.radio_inhibit.is_some()
            {
                return Ok(observe(
                    &state,
                    raw_snapshot(&binding.producer, "busy", Some(binding.config), "", vec![]),
                ));
            }
            let authority = new_io_authority(&state, &binding.producer)?;
            state.busy = true;
            state.busy_lease.clear();
            IoPermit {
                inner: self.inner.clone(),
                epoch: state.epoch,
                _authority: authority,
            }
        };
        let binding = binding.clone();
        self.run(permit, move |inner, epoch| {
            {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                binding.check(&state)?;
            }
            if let Err(error) = ensure_open(inner, epoch) {
                if error.code != "usb_at_not_found" {
                    return Err(error);
                }
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                binding.check(&state)?;
                return Ok(SmsObservation {
                    value: raw_snapshot(
                        &binding.producer,
                        "absent",
                        Some(binding.config),
                        "",
                        vec![],
                    ),
                    token: SmsObservationToken {
                        binding,
                        session: state.session.clone(),
                        identity_generation: state.identity_generation,
                    },
                });
            }
            let token = {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                binding.check(&state)?;
                SmsObservationToken {
                    binding: binding.clone(),
                    session: state.session.clone(),
                    identity_generation: state.identity_generation,
                }
            };
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let result = read_snapshot(
                inner,
                epoch,
                &token,
                port.as_mut().ok_or_else(Failure::stale)?.as_mut(),
                Instant::now() + MAX_WAIT,
            );
            if match &result {
                Ok((_, detached, _)) => *detached,
                Err(error) => matches!(
                    error.code,
                    "usb_at_disconnected"
                        | "usb_at_timeout"
                        | "usb_at_read_failed"
                        | "usb_at_write_failed"
                        | "usb_at_short_write"
                        | "usb_at_response_too_large"
                        | "usb_at_drain_failed"
                        | "usb_sms_snapshot_incomplete_response"
                ),
            } {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            let (storages, _, card_identity) = result?;
            // Keep completed per-storage diagnostics in the compatibility
            // snapshot even when this read retired its transport. The native
            // receiver separately validates this token before any adoption or
            // preview publication; an old physical result grants no authority.
            check_epoch(
                &inner.state.lock().unwrap_or_else(|e| e.into_inner()),
                epoch,
            )?;
            let mut value = raw_snapshot(
                &binding.producer,
                "usb",
                Some(binding.config),
                &token.session,
                storages,
            );
            value["card_identity"] = json!(card_identity);
            Ok(SmsObservation { value, token })
        })
        .await
    }
}

pub(super) fn capture(
    inner: &Inner,
    binding: &SmsPollBinding,
) -> Result<SmsObservationToken, Failure> {
    let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
    binding.check(&state)?;
    Ok(SmsObservationToken {
        binding: binding.clone(),
        session: state.session.clone(),
        identity_generation: state.identity_generation,
    })
}

pub(super) fn check_active(
    inner: &Inner,
    token: Option<&SmsObservationToken>,
    phase: crate::usb_at_storage::Phase,
) -> Result<(), Failure> {
    if phase == crate::usb_at_storage::Phase::Active {
        if let Some(token) = token {
            token.check(&inner.state.lock().unwrap_or_else(|e| e.into_inner()))?;
        }
    }
    Ok(())
}
