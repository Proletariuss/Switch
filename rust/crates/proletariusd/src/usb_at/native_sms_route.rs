//! Native direct-SMS capture and one-shot submission on an exact USB stream.
//! Prepared routes retain identity facts, not a physical lease or permission
//! to write; the durable send ledger remains the only submission grant.
use super::*;

impl Owner {
    pub(crate) fn owns_native_sms_binding(&self, session: &str, generation: u64) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .native_sms_identity
            .as_ref()
            .is_some_and(|v| v.0 == session && v.1 == generation)
    }
    pub(crate) fn native_sms_binding_current(&self, session: &str, generation: u64) -> bool {
        let s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        sms_current(&s, session, generation) && s.lease.is_none() && !s.busy
    }
    pub(crate) async fn capture_native_sms(&self, session: &str) -> Result<(String, u64), Failure> {
        let (producer, epoch, generation, config_sequence) = {
            let s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check(&s, &s.producer)?;
            sms_idle(&s, session)?;
            (
                s.producer.clone(),
                s.epoch,
                s.identity_generation,
                s.config_high_water,
            )
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let permit = self.admit(&producer, Some(session), "", deadline).await?;
        let session = session.to_owned();
        self.run(permit, move |inner, actual_epoch| {
            let fence = || {
                let s = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                check_epoch(&s, epoch)?;
                if actual_epoch != epoch
                    || s.identity_generation != generation
                    || s.config_high_water != config_sequence
                {
                    return Err(Failure::stale());
                }
                sms_idle(&s, &session)
            };
            fence()?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let mut failed = false;
            let mut disproved_identity = false;
            let result = (|| {
                let mut command = |text: &str| {
                    fence()?;
                    let result = port.as_mut().ok_or_else(Failure::stale)?.command(
                        text,
                        None,
                        remaining(deadline)?.min(Duration::from_secs(4)),
                        MAX_BYTES,
                    );
                    failed |= result.is_err();
                    fence()?;
                    result.map_err(Failure::from)
                };
                let cpin = command("AT+CPIN?")?;
                if !complete_ok(&cpin) {
                    disproved_identity = explicit_sim_absent(&cpin);
                    return Err(Failure::stale());
                }
                let pin = cpin
                    .split(['\r', '\n'])
                    .find_map(|line| line.trim().strip_prefix("+CPIN:").map(str::trim));
                if pin != Some("READY") {
                    // A complete explicit PIN/not-ready result disproves the
                    // previous ready identity; a missing/failed reply does not.
                    disproved_identity = pin.is_some();
                    return Err(Failure::stale());
                }
                let response = command("AT+QCCID")?;
                let iccid = crate::usb_at_storage::card_identity(&response).filter(|v| {
                    (10..=20).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit())
                });
                let Some(iccid) = iccid else {
                    disproved_identity = complete_ok(&response) || explicit_sim_absent(&response);
                    return Err(Failure::stale());
                };
                disproved_identity = inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .module_identity
                    .as_ref()
                    .is_some_and(|v| v.iccid.as_str() != iccid);
                let response = command("AT+CIMI")?;
                if !complete_ok(&response) {
                    return Err(Failure::stale());
                }
                let Some(imsi) = response
                    .split(['\r', '\n'])
                    .map(str::trim)
                    .find(|value| valid_module_sim_identifier(value, 5, 20))
                    .map(str::to_owned)
                else {
                    disproved_identity = true;
                    return Err(Failure::stale());
                };
                Ok((iccid, imsi))
            })();
            if failed {
                let _ = native_module::close_port(inner, &mut port);
                detached(inner, epoch);
            }
            if result.is_err() && disproved_identity {
                let mut s = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if s.epoch == epoch
                    && s.session == session
                    && s.identity_generation == generation
                    && s.config_high_water == config_sequence
                {
                    invalidate_identity(&mut s);
                }
            }
            let (iccid, imsi) = result?;
            fence()?;
            let mut s = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check_epoch(&s, epoch)?;
            sms_idle(&s, &session)?;
            if s.identity_generation != generation || s.config_high_water != config_sequence {
                return Err(Failure::stale());
            }
            let changed = s
                .module_identity
                .as_ref()
                .is_some_and(|v| v.iccid.as_str() != iccid || v.imsi.as_str() != imsi);
            if changed {
                s.identity_generation = s
                    .identity_generation
                    .checked_add(1)
                    .ok_or_else(Failure::stopped)?;
            }
            if s.identity_generation == 0 {
                return Err(Failure::stale());
            }
            s.esim_snapshot.identity_observed(&iccid);
            if s.device_observation
                .as_ref()
                .is_some_and(|v| v.status["iccid"].as_str() != Some(iccid.as_str()))
            {
                s.device_observation = None;
            }
            let generation = s.identity_generation;
            s.module_identity = Some(ModuleSimIdentityCache {
                generation,
                iccid: Zeroizing::new(iccid),
                imsi: Zeroizing::new(imsi),
            });
            s.native_sms_identity = Some((session.clone(), generation));
            Ok((session, generation))
        })
        .await
    }
    /// Retain acquisition, every actual CMGS and confirmed release together.
    /// Dropping a caller/HTTP waiter never detaches the cleanup from that I/O.
    pub(crate) async fn submit_native_sms(
        &self,
        session: String,
        generation: u64,
        recipient: String,
        message: String,
    ) -> Result<SmsSubmitOutcome, Failure> {
        let owner = self.clone();
        tokio::spawn(async move {
            owner
                .submit_native_sms_inner(session, generation, recipient, message)
                .await
        })
        .await
        .map_err(|_| Failure::stopped())?
    }
    async fn submit_native_sms_inner(
        &self,
        session: String,
        generation: u64,
        recipient: String,
        message: String,
    ) -> Result<SmsSubmitOutcome, Failure> {
        let (producer, sequence) = {
            let s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !sms_current(&s, &session, generation) {
                return Err(Failure::stale());
            }
            sms_idle(&s, &session)?;
            (
                s.producer.clone(),
                s.lease_high_water
                    .checked_add(1)
                    .ok_or_else(Failure::stopped)?,
            )
        };
        let lease_id = crate::snapshot::new_instance_id();
        self.acquire(LeaseRequest {
            producer_id: producer.clone(),
            session_id: session.clone(),
            lease_id: lease_id.clone(),
            lease_seq: sequence,
            purpose: Purpose::Sim,
        })
        .await?;
        let mut lease = LeaseCleanup {
            owner: self.clone(),
            release: Some(ReleaseRequest {
                producer_id: producer,
                session_id: session.clone(),
                lease_id,
                lease_seq: sequence,
            }),
        };
        let result = if self.has_active_sim_identity_binding(&session, generation) {
            self.submit_active_sms(recipient, message, generation, false)
                .await
        } else {
            Err(Failure::stale())
        };
        if result
            .as_ref()
            .is_err_and(|error| error.code == "usb_sms_card_changed")
        {
            let mut s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.session == session && s.identity_generation == generation {
                invalidate_identity(&mut s);
            }
        }
        lease.finish().await?;
        result
    }
}
// Only call after an actual identity observation, with an exact captured
// session/generation fence. Zero is a permanently unusable exhausted generation.
pub(super) fn invalidate_identity(s: &mut State) {
    s.identity_generation = s.identity_generation.checked_add(1).unwrap_or(0);
    s.module_identity = None;
    s.native_sms_identity = None;
    s.device_observation = None;
}
pub(super) fn explicit_sim_absent(response: &str) -> bool {
    response.split(['\r', '\n']).any(|line| {
        line.trim()
            .strip_prefix("+CME ERROR:")
            .is_some_and(|value| {
                let value = value.trim();
                value == "10" || value.eq_ignore_ascii_case("SIM not inserted")
            })
    })
}
fn sms_idle(s: &State, session: &str) -> Result<(), Failure> {
    if s.stopping || s.revoking || s.session != session || session.is_empty() {
        return Err(Failure::stale());
    }
    if s.cellular_suppressed
        || s.native_module_hold.is_some()
        || s.radio_inhibit.is_some()
        || !s.inhibits.is_empty()
        || s.lease.is_some()
    {
        return Err(Failure::busy());
    }
    Ok(())
}
fn sms_current(s: &State, session: &str, generation: u64) -> bool {
    !s.stopping
        && !s.revoking
        && !s.cellular_suppressed
        && !session.is_empty()
        && s.session == session
        && generation > 0
        && s.identity_generation == generation
        && s.native_module_hold.is_none()
        && s.radio_inhibit.is_none()
        && s.inhibits.is_empty()
        && s.module_identity
            .as_ref()
            .is_some_and(|v| v.generation == generation)
}
struct LeaseCleanup {
    owner: Owner,
    release: Option<ReleaseRequest>,
}
impl LeaseCleanup {
    async fn finish(&mut self) -> Result<(), Failure> {
        let request = self.release.take().ok_or_else(Failure::stale)?;
        self.owner
            .release_with_deadline(request, None)
            .await
            .map(|_| ())
    }
}
impl Drop for LeaseCleanup {
    fn drop(&mut self) {
        if let Some(request) = self.release.take() {
            let owner = self.owner.clone();
            tokio::spawn(async move {
                let _ = owner.release_with_deadline(request, None).await;
            });
        }
    }
}

#[cfg(test)]
mod tests;
