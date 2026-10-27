//! In-process call-mode transactions. The coordinator owns the reservation
//! until release acknowledgement; physical jobs retain it through real cleanup.
use super::*;
use crate::module_radio_transport::CallModeModuleIdentity;

#[derive(Clone, Debug)]
pub(crate) struct CallModeReservation {
    pub(crate) producer_id: String,
    pub(crate) token: String,
    pub(crate) sequence: u64,
    pub(super) epoch: u64,
    pub(super) purpose: Purpose,
    pub(super) owned: bool,
}
pub(super) struct RebindRequest {
    pub(super) reservation: CallModeReservation,
    pub(super) expected_imei: String,
}
impl CallModeReservation {
    fn request(&self, action: &str) -> ReservationRequest {
        ReservationRequest {
            producer_id: self.producer_id.clone(),
            action: action.into(),
            purpose: self.purpose,
            token: self.token.clone(),
            sequence: self.sequence,
        }
    }
}
impl Service {
    /// Observe the original opened USB connection before restore effects. The
    /// result is journal data, not permission to open a serialized target.
    pub(crate) async fn capture_restore_physical_binding(
        &self,
        lease: &CallModeReservation,
    ) -> Result<Option<crate::usb_at_transport::DurableUsbBinding>, Failure> {
        self.validate_call_mode_operation_lock(lease)?;
        let value = self
            .submit(Job::CaptureRestorePhysicalBinding(lease.clone()))
            .await?;
        serde_json::from_value(value).map_err(|_| Failure::unavailable())
    }
    pub(super) async fn capture_restore_physical_binding_inner(
        &self,
        lease: CallModeReservation,
        epoch: u64,
    ) -> Result<Value, Failure> {
        let check_capture = |state: &State| -> Result<(), Failure> {
            check_operation(state, &lease)?;
            let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
            if !lease.owned
                || lease.purpose != Purpose::CallMode
                || !owner.restore_journal_required
                || owner.unknown
                || owner.attempts.values().any(|attempt| {
                    matches!(
                        attempt.input.kind.as_str(),
                        "call_mode_voice"
                            | "call_mode_usb_composition_write"
                            | "call_mode_adb_authorize"
                            | "reboot"
                    )
                })
            {
                return Err(Failure::stale());
            }
            Ok(())
        };
        let (binding, anchor, deadline, cancelled) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            check_capture(&state)?;
            (
                state.binding.clone().ok_or_else(Failure::unavailable)?,
                state
                    .reservation
                    .as_ref()
                    .and_then(|owner| owner.reconnect.clone()),
                state.job_deadline.ok_or_else(Failure::unavailable)?,
                state.epoch_cancel.clone(),
            )
        };
        let proof = self.mint_call_mode_rebind_proof(None, epoch)?;
        if let Binding::Serial { serial_session } = binding {
            #[cfg(test)]
            let ownership_substitute = self.0.physical_fixture.lock().unwrap().is_some();
            #[cfg(not(test))]
            let ownership_substitute = false;
            if !ownership_substitute && !self.0.modem.call_binding_current(serial_session) {
                return Err(Failure::stale());
            }
            proof.validate()?;
            return Ok(Value::Null);
        }
        let anchor = match anchor {
            Some(anchor) => anchor,
            None => self
                .0
                .usb
                .capture_call_mode_reconnect(proof.clone(), deadline, cancelled)
                .await
                .map_err(Failure::from)?,
        };
        let observed = anchor.durable_binding(&proof).map_err(Failure::from)?;
        proof.validate()?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        check_capture(&state)?;
        let owner = state.reservation.as_mut().ok_or_else(Failure::stale)?;
        if owner
            .reconnect
            .as_ref()
            .is_some_and(|current| !current.same_instance(&anchor))
        {
            return Err(Failure::stale());
        }
        owner.reconnect = Some(anchor);
        serde_json::to_value(observed).map_err(|_| Failure::unavailable())
    }
    pub(crate) async fn reserve_call_mode_operation(&self) -> Result<CallModeReservation, Failure> {
        self.reserve_native_call_mode(true, false).await
    }
    pub(crate) async fn reserve_call_mode_restore_operation(
        &self,
    ) -> Result<CallModeReservation, Failure> {
        self.reserve_native_call_mode(true, true).await
    }
    pub(crate) async fn reserve_call_mode_read(&self) -> Result<CallModeReservation, Failure> {
        self.reserve_native_call_mode(false, false).await
    }
    async fn reserve_native_call_mode(
        &self,
        operation: bool,
        restore: bool,
    ) -> Result<CallModeReservation, Failure> {
        // Acquisition outlives its waiter. If admission finishes after caller
        // cancellation, cleanup retains the service until release is confirmed.
        let service = self.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let result = service
                .reserve_native_call_mode_inner(operation, restore)
                .await;
            if let Err(result) = send.send(result) {
                if let Ok(lease) = result {
                    // This detached task is the cleanup owner. A failed or
                    // raced release keeps the exact lease until a real ACK or
                    // committed producer-drain proof; it never replays actions.
                    while service.release_call_mode_operation(&lease).await.is_err() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
        receive.await.map_err(|_| Failure::stopped())?
    }
    async fn reserve_native_call_mode_inner(
        &self,
        operation: bool,
        restore: bool,
    ) -> Result<CallModeReservation, Failure> {
        if !operation {
            self.require_new_module_admission()?;
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(reservation) = state.reservation.as_ref() {
                if !matches!(reservation.purpose, Purpose::Call | Purpose::CallMode) {
                    // Copy the actual rejecting owner under the same lock; the
                    // best-effort recorder never runs while holding that lock.
                    let snapshot = diagnostics::Snapshot::capture(&state);
                    drop(state);
                    self.record_module_snapshot(
                        crate::support_diagnostics::module::Stage::Admission,
                        crate::support_diagnostics::module::ResultKind::Deferred,
                        crate::support_diagnostics::module::Code::ReservationConflict,
                        snapshot,
                        Duration::ZERO,
                    );
                    return Err(Failure::busy());
                }
                return Ok(CallModeReservation {
                    producer_id: state.producer.clone(),
                    token: reservation.identity.token.clone(),
                    sequence: reservation.identity.sequence,
                    epoch: state.epoch,
                    purpose: reservation.purpose,
                    owned: false,
                });
            }
        }
        let producer = self
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .producer
            .clone();
        self.submit(Job::CallModeOpen(Producer {
            producer_id: producer,
        }))
        .await?;
        let lease = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let sequence = state
                .waters
                .get(&Purpose::CallMode)
                .map_or(0, |water| water.sequence)
                .checked_add(1)
                .ok_or_else(Failure::stale)?;
            CallModeReservation {
                producer_id: state.producer.clone(),
                token: crate::multistandby_store::random_token().map_err(|_| {
                    Failure::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "call_mode_entropy_unavailable",
                        "无法分配通话模式预约",
                    )
                })?,
                sequence,
                epoch: state.epoch,
                purpose: Purpose::CallMode,
                owned: true,
            }
        };
        let request = lease.request(if operation {
            "acquire_call_mode_operation"
        } else {
            "acquire"
        });
        let value = self
            .submit(if restore {
                Job::RestoreReservation(request)
            } else {
                Job::Reservation(request)
            })
            .await?;
        confirm_reservation_ack(&lease, &value, false)?;
        Ok(lease)
    }
    pub(crate) fn attach_restore_journal(
        &self,
        lease: &CallModeReservation,
        journal: crate::call_mode_restore_journal::Journal,
    ) -> Result<(), Failure> {
        self.validate_call_mode_operation_lock(lease)?;
        let root = self
            .module_recovery()
            .root_proof_for(&self.0.data_root)
            .map_err(|_| restore_journal_failure())?;
        journal
            .validate_root(&root)
            .map_err(|_| restore_journal_failure())?;
        if !journal.matches_reservation(&lease.producer_id, &lease.token, lease.sequence) {
            return Err(Failure::stale());
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        check_operation(&state, lease)?;
        let owner = state.reservation.as_mut().ok_or_else(Failure::stale)?;
        if !owner.restore_journal_required
            || owner.attempts.values().any(|attempt| {
                matches!(
                    attempt.input.kind.as_str(),
                    "call_mode_voice"
                        | "call_mode_usb_composition_write"
                        | "call_mode_adb_authorize"
                        | "reboot"
                )
            })
        {
            return Err(restore_journal_failure());
        }
        match &owner.restore_journal {
            Some(existing) if existing.same_instance(&journal) => Ok(()),
            Some(_) => Err(Failure::stale()),
            None => {
                owner.restore_journal = Some(journal);
                Ok(())
            }
        }
    }
    pub(crate) async fn release_call_mode_operation(
        &self,
        lease: &CallModeReservation,
    ) -> Result<(), Failure> {
        if !lease.owned {
            return Ok(());
        }
        let proof = (
            lease.epoch,
            lease.producer_id.clone(),
            lease.token.clone(),
            lease.sequence,
        );
        loop {
            let (confirmed, current, old_worker_active, reconnect_worker_active) = {
                let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                (
                    state.confirmed_call_mode_releases.contains(&proof),
                    state.epoch == lease.epoch
                        && state.producer == lease.producer_id
                        && !state.retiring,
                    state.retiring && state.epoch != lease.epoch && state.jobs != 0,
                    state.reservation.as_ref().is_some_and(|owner| {
                        owner.identity.token == lease.token
                            && owner.identity.sequence == lease.sequence
                            && owner.active_reconnect_pins != 0
                    }),
                )
            };
            if confirmed {
                return Ok(());
            }
            if reconnect_worker_active {
                self.wait_idle(false).await;
                continue;
            }
            if current {
                let value = self
                    .submit(Job::Reservation(lease.request("release")))
                    .await?;
                confirm_reservation_ack(lease, &value, true)?;
                self.0
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .confirmed_call_mode_releases
                    .insert(proof);
                return Ok(());
            }
            if old_worker_active {
                self.wait_idle(false).await;
                continue;
            }
            // Retirement may have drained its worker but not yet committed its
            // cleanup proof. The caller retains the lease and retries release.
            return Err(Failure::stale());
        }
    }
    pub(crate) fn borrow_call_mode_operation_voice(
        &self,
        lease: &CallModeReservation,
    ) -> Result<CallModeVoiceGuard, Failure> {
        self.fence(lease.epoch)?;
        self.borrow_call_mode_voice_operation(&lease.producer_id, &lease.token, lease.sequence)
    }
    /// Stop only an already accepted voice obligation. The caller supplies
    /// the voice registry's retained right; this method additionally proves
    /// the exact current exclusive radio operation rather than accepting a
    /// cleanup flag or a matching producer alone.
    pub(crate) fn borrow_call_mode_operation_voice_settlement(
        &self,
        lease: &CallModeReservation,
        right: &crate::module_recovery::OperationRight,
    ) -> Result<CallModeVoiceGuard, Failure> {
        if !lease.owned || lease.purpose != Purpose::CallMode {
            return Err(Failure::stale());
        }
        self.borrow_call_mode_voice_operation_scoped(
            &lease.producer_id,
            &lease.token,
            lease.sequence,
            Some(lease.epoch),
            Some(right),
        )
    }
    pub(crate) async fn call_mode_action(
        &self,
        lease: &CallModeReservation,
        action: Action,
    ) -> Result<Outcome, Failure> {
        self.fence(lease.epoch)?;
        if action.mutating() {
            self.validate_call_mode_operation_lock(lease)?;
        }
        let sequence = NEXT_CALL_MODE_READ_OPERATION.fetch_add(1, Ordering::Relaxed);
        if sequence == 0 || sequence == u64::MAX {
            return Err(Failure::stale());
        }
        let mut input = RadioAction {
            producer_id: lease.producer_id.clone(),
            purpose: lease.purpose,
            token: lease.token.clone(),
            sequence: lease.sequence,
            operation_id: format!("rust-call-mode-{sequence}"),
            kind: String::new(),
            target: None,
            command: None,
            timeout_ms: None,
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        };
        input.kind = match action {
            Action::CallModeIdleRead => "call_mode_idle_read",
            Action::CallModeIdentityRead => "call_mode_identity_read",
            Action::CallModeVoiceRead => "call_mode_voice_read",
            Action::CallModeUsbCompositionRead => "call_mode_usb_composition_read",
            Action::CallModeAdbProbe => "call_mode_adb_probe",
            Action::CallModeAdbAuthorize => "call_mode_adb_authorize",
            Action::CallModeVoice { target } => {
                input.voice_target = Some(target);
                "call_mode_voice"
            }
            Action::CallModeUsbCompositionWrite { expected, target } => {
                input.usb_expected = Some(expected);
                input.usb_target = Some(target);
                "call_mode_usb_composition_write"
            }
            Action::Reboot => "reboot",
            _ => return Err(Failure::invalid()),
        }
        .into();
        let value = self.submit(Job::Action(input)).await?;
        serde_json::from_value(value).map_err(|_| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_radio_outcome_unconfirmed",
                "原生模块事务结果未确认",
            )
        })
    }
    /// New writes and success receipts require the original lock namespace.
    /// Accepted cleanup/release retains its held guard even if names change.
    pub(crate) fn validate_call_mode_operation_lock(
        &self,
        lease: &CallModeReservation,
    ) -> Result<(), Failure> {
        self.fence(lease.epoch)?;
        if !lease.owned || lease.purpose != Purpose::CallMode {
            return Err(Failure::stale());
        }
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        check_operation(&state, lease)?;
        state
            .reservation
            .as_ref()
            .and_then(|reservation| reservation.operation_lock.as_ref())
            .ok_or_else(Failure::stale)?
            .validate()
            .map_err(|_| {
                Failure::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "call_mode_operation_lock_unavailable",
                    "无法取得通话模式操作锁",
                )
            })
    }
    pub(crate) fn capture_call_mode_operation_namespace(
        &self,
        lease: &CallModeReservation,
    ) -> Result<crate::call_mode_backups::OperationNamespaceWitness, Failure> {
        self.fence(lease.epoch)?;
        if !lease.owned || lease.purpose != Purpose::CallMode {
            return Err(Failure::stale());
        }
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        check_operation(&state, lease)?;
        state
            .reservation
            .as_ref()
            .and_then(|reservation| reservation.operation_lock.as_ref())
            .ok_or_else(Failure::stale)?
            .namespace_witness()
            .map_err(|_| {
                Failure::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "call_mode_operation_lock_unavailable",
                    "无法取得通话模式操作锁",
                )
            })
    }
    pub(crate) async fn call_mode_ensure_idle(
        &self,
        lease: &CallModeReservation,
    ) -> Result<(), Failure> {
        let outcome = self
            .call_mode_action(lease, Action::CallModeIdleRead)
            .await?;
        if outcome.complete && outcome.verified && !outcome.uncertain {
            Ok(())
        } else {
            Err(outcome_failure(&outcome, "radio_call_state_unconfirmed"))
        }
    }
    pub(crate) async fn call_mode_identity(
        &self,
        lease: &CallModeReservation,
    ) -> Result<CallModeModuleIdentity, Failure> {
        let outcome = self
            .call_mode_action(lease, Action::CallModeIdentityRead)
            .await?;
        confirmed_identity(&outcome)
    }
    /// Retains the same producer, reservation and exclusive operation file
    /// lock while admitting a fresh physical connection. Only reads are sent
    /// to an unproven device; no configuration write is replayed here.
    pub(crate) async fn call_mode_rebind(
        &self,
        lease: &CallModeReservation,
        expected_imei: &str,
    ) -> Result<CallModeModuleIdentity, Failure> {
        if expected_imei.len() != 15 || !expected_imei.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Failure::invalid());
        }
        self.fence(lease.epoch)?;
        let value = self
            .submit(Job::CallModeRebind(RebindRequest {
                reservation: lease.clone(),
                expected_imei: expected_imei.into(),
            }))
            .await?;
        serde_json::from_value(value).map_err(|_| Failure::invalid())
    }
    pub(super) async fn call_mode_open_inner(
        &self,
        input: Producer,
        epoch: u64,
    ) -> Result<Value, Failure> {
        let (old_binding, deadline, cancelled) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.reservation.is_some()
                || state.intent.is_some()
                || state.active_shared_voice_pins != 0
            {
                return Err(Failure::busy());
            }
            (
                state.binding.clone(),
                state.job_deadline.ok_or_else(Failure::unavailable)?,
                state.epoch_cancel.clone(),
            )
        };
        #[cfg(test)]
        if self.0.physical_fixture.lock().unwrap().is_some() && old_binding.is_some() {
            return Ok(json!({"bound":true}));
        }
        let serial = matches!(old_binding, Some(Binding::Serial { .. }))
            || self
                .0
                .usb
                .sms_status_config()
                .is_some_and(|(serial, _)| serial);
        let binding = if serial {
            crate::module_radio_transport::capture_serial_binding(&self.0.modem)
                .await
                .map_err(Failure::from)?
        } else {
            Binding::Usb {
                producer_id: input.producer_id.clone(),
                session_id: self
                    .0
                    .usb
                    .call_mode_radio_reconnect_with_right(
                        &input.producer_id,
                        "",
                        deadline,
                        cancelled,
                        self.current_operation_right()?,
                    )
                    .await
                    .map_err(Failure::from)?,
            }
        };
        self.fence(epoch)?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.producer != input.producer_id
            || state.reservation.is_some()
            || state.intent.is_some()
        {
            return Err(Failure::stale());
        }
        if state.binding.as_ref() != Some(&binding) {
            state.observed = Outcome::default();
        }
        state.binding = Some(binding);
        self.sync_suppression(&state);
        drop(state);
        self.flush_call_capture();
        Ok(json!({"bound":true}))
    }
    pub(super) async fn call_mode_rebind_inner(
        &self,
        input: RebindRequest,
        epoch: u64,
    ) -> Result<Value, Failure> {
        let (old_binding, cancelled, deadline) = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            check_operation(&state, &input.reservation)?;
            (
                state.binding.clone().ok_or_else(Failure::unavailable)?,
                state.epoch_cancel.clone(),
                state.job_deadline.ok_or_else(Failure::unavailable)?,
            )
        };
        #[cfg(test)]
        let fixture = self.0.physical_fixture.lock().unwrap().clone();
        #[cfg(not(test))]
        let fixture: Option<()> = None;
        let mut usb_receipt = None;
        let mut usb_proof = None;
        let candidate = if let Binding::Usb { .. } = &old_binding {
            let proof = self.mint_call_mode_rebind_proof(None, epoch)?;
            let anchor = self
                .0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reservation
                .as_ref()
                .and_then(|owner| owner.reconnect.clone());
            let anchor = match anchor {
                Some(anchor) => anchor,
                None if !proof.successor_required() => {
                    let anchor = self
                        .0
                        .usb
                        .capture_call_mode_reconnect(proof.clone(), deadline, cancelled.clone())
                        .await
                        .map_err(Failure::from)?;
                    proof.validate()?;
                    let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    check_operation(&state, &input.reservation)?;
                    state
                        .reservation
                        .as_mut()
                        .ok_or_else(Failure::stale)?
                        .reconnect = Some(anchor.clone());
                    anchor
                }
                None => return Err(Failure::stale()),
            };
            let receipt = self
                .0
                .usb
                .reconnect_call_mode(
                    anchor,
                    proof.clone(),
                    &input.expected_imei,
                    deadline,
                    cancelled.clone(),
                )
                .await
                .map_err(Failure::from)?;
            receipt.validate().map_err(Failure::from)?;
            let binding = receipt.binding();
            usb_receipt = Some(receipt);
            usb_proof = Some(proof);
            binding
        } else if fixture.is_some() {
            match old_binding {
                Binding::Usb { .. } => return Err(Failure::stale()),
                Binding::Serial { serial_session } => Binding::Serial {
                    serial_session: serial_session + 1,
                },
            }
        } else {
            match old_binding {
                Binding::Usb { .. } => return Err(Failure::stale()),
                Binding::Serial { serial_session } => Binding::Serial {
                    serial_session: self
                        .0
                        .modem
                        .call_mode_radio_reconnect_with_right(
                            serial_session,
                            &input.expected_imei,
                            deadline,
                            cancelled.clone(),
                            self.current_operation_right()?,
                            input.reservation.producer_id.clone(),
                        )
                        .await
                        .map_err(Failure::from)?,
                },
            }
        };
        self.fence(epoch)?;
        #[cfg(test)]
        let fake = self.0.physical_fixture.lock().unwrap().is_some();
        #[cfg(not(test))]
        let fake = false;
        if let Binding::Serial { serial_session } = &candidate {
            if !fake {
                if self.0.modem.radio_binding().await.map_err(Failure::from)? != *serial_session {
                    return Err(Failure::stale());
                }
                // The low-level candidate already passed its bounded CGSN
                // probe. Opening a new native serial session requires its
                // scoped initialization before ordinary typed radio reads;
                // otherwise the actor correctly rejects the identity Plan.
                self.0
                    .modem
                    .call_mode_radio_initialize_with_right(
                        *serial_session,
                        deadline,
                        cancelled.clone(),
                        self.current_operation_right()?,
                        input.reservation.producer_id.clone(),
                    )
                    .await
                    .map_err(Failure::from)?;
                self.fence(epoch)?;
            }
        }
        #[cfg(test)]
        let outcome = if let Some(receipt) = &usb_receipt {
            Outcome {
                complete: true,
                verified: true,
                module_identity: Some(receipt.identity().clone()),
                ..Outcome::default()
            }
        } else if let Some(fixture) = fixture {
            let (send, receive) = oneshot::channel();
            fixture
                .send((Action::CallModeIdentityRead, send))
                .map_err(|_| Failure::unavailable())?;
            receive.await.map_err(|_| Failure::unavailable())??
        } else {
            crate::module_radio_transport::execute_with_budget_and_right(
                &self.0.usb,
                &self.0.modem,
                candidate.clone(),
                Action::CallModeIdentityRead,
                None,
                deadline.saturating_duration_since(Instant::now()),
                cancelled.clone(),
                self.current_operation_right()?,
                input.reservation.producer_id.clone(),
                None,
            )
            .await
            .map_err(Failure::from)?
        };
        #[cfg(not(test))]
        let outcome = if let Some(receipt) = &usb_receipt {
            Outcome {
                complete: true,
                verified: true,
                module_identity: Some(receipt.identity().clone()),
                ..Outcome::default()
            }
        } else {
            crate::module_radio_transport::execute_with_budget_and_right(
                &self.0.usb,
                &self.0.modem,
                candidate.clone(),
                Action::CallModeIdentityRead,
                None,
                deadline.saturating_duration_since(Instant::now()),
                cancelled.clone(),
                self.current_operation_right()?,
                input.reservation.producer_id.clone(),
                None,
            )
            .await
            .map_err(Failure::from)?
        };
        let identity = confirmed_identity(&outcome)?;
        if identity.imei != input.expected_imei {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "call_mode_module_identity_mismatch",
                "模块重连后的 IMEI 与配置前不一致",
            ));
        }
        self.fence(epoch)?;
        if !fake {
            match &candidate {
                Binding::Usb {
                    producer_id,
                    session_id,
                } => self
                    .0
                    .usb
                    .validate_radio_binding(producer_id, session_id)
                    .map_err(Failure::from)?,
                Binding::Serial { serial_session } => {
                    if self.0.modem.radio_binding().await.map_err(Failure::from)? != *serial_session
                    {
                        return Err(Failure::stale());
                    }
                }
            }
        }
        if let Some(proof) = usb_proof {
            proof.validate()?;
        }
        if let Some(receipt) = &usb_receipt {
            receipt.validate().map_err(Failure::from)?;
        }
        if usb_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.successor_confirmed())
        {
            let prepared = {
                let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                check_operation(&state, &input.reservation)?;
                let owner = state.reservation.as_ref().ok_or_else(Failure::stale)?;
                if owner.restore_journal_required && !owner.restore_reboot_verified {
                    Some(
                        owner
                            .restore_reboot
                            .clone()
                            .ok_or_else(restore_journal_failure)?,
                    )
                } else {
                    None
                }
            };
            if let Some(prepared) = prepared {
                // The token was stored by this exact reservation's actual
                // Reboot Prepare, and only the sealed native successor above
                // may refine its original (retained) unknown event.
                prepared
                    .record_reboot_verified_after_successor()
                    .map_err(|_| restore_journal_failure())?;
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                check_operation(&state, &input.reservation)?;
                state
                    .reservation
                    .as_mut()
                    .ok_or_else(Failure::stale)?
                    .restore_reboot_verified = true;
            }
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        check_operation(&state, &input.reservation)?;
        // A fresh stream proves connection identity, not the success of any
        // earlier configuration write. Keep unconfirmed writes fenced; an
        // uncertain reboot alone is settled by the same-device reconnection.
        let reservation = state.reservation.as_mut().ok_or_else(Failure::stale)?;
        let only_reboot_unknown =
            reservation
                .attempts
                .values()
                .all(|attempt| match &attempt.result {
                    Some(Ok(outcome)) if outcome.uncertain => {
                        attempt.input.kind == "reboot"
                            && (usb_receipt.is_none()
                                || reservation.reconnect_reboot.as_deref()
                                    == Some(attempt.input.operation_id.as_str()))
                    }
                    Some(Err(error)) if error.uncertain => {
                        attempt.input.kind == "reboot"
                            && (usb_receipt.is_none()
                                || reservation.reconnect_reboot.as_deref()
                                    == Some(attempt.input.operation_id.as_str()))
                    }
                    _ => true,
                });
        if only_reboot_unknown
            && (!reservation.restore_journal_required || reservation.restore_reboot_verified)
            && usb_receipt
                .as_ref()
                .is_none_or(|receipt| receipt.successor_confirmed())
        {
            reservation.unknown = false;
        }
        let keep_unknown = reservation.unknown;
        state.binding = Some(candidate.clone());
        state.observed = outcome;
        state.observed.uncertain = keep_unknown;
        if let Some(config) = state.config.as_mut() {
            match candidate {
                Binding::Usb { session_id, .. } => {
                    config.transport = "usb".into();
                    config.session_id = session_id;
                }
                Binding::Serial { .. } => {
                    config.transport = "serial".into();
                    config.session_id.clear();
                }
            }
        }
        self.sync_suppression(&state);
        Ok(json!(identity))
    }
}
fn check_operation(state: &State, lease: &CallModeReservation) -> Result<(), Failure> {
    if state.epoch != lease.epoch || state.producer != lease.producer_id || state.retiring {
        return Err(Failure::stale());
    }
    if state.stopping {
        return Err(Failure::stopped());
    }
    let reservation = state.reservation.as_ref().ok_or_else(Failure::stale)?;
    if reservation.purpose != Purpose::CallMode
        || reservation.identity.token != lease.token
        || reservation.identity.sequence != lease.sequence
        || reservation.operation_lock.is_none()
    {
        return Err(Failure::stale());
    }
    if reservation.active_voice_pins != 0
        || reservation.active_reconnect_pins != 0
        || state.active_shared_voice_pins != 0
        || reservation
            .attempts
            .values()
            .any(|attempt| attempt.result.is_none())
    {
        return Err(Failure::busy());
    }
    Ok(())
}
fn confirm_reservation_ack(
    lease: &CallModeReservation,
    value: &Value,
    released: bool,
) -> Result<(), Failure> {
    if value["token"].as_str() != Some(&lease.token)
        || value["sequence"].as_u64() != Some(lease.sequence)
        || value["released"].as_bool() != Some(released)
    {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_radio_ack_unconfirmed",
            "原生模块预约确认无效",
        ));
    }
    Ok(())
}
fn confirmed_identity(outcome: &Outcome) -> Result<CallModeModuleIdentity, Failure> {
    if !outcome.complete || !outcome.verified || outcome.uncertain {
        return Err(outcome_failure(outcome, "call_mode_identity_unconfirmed"));
    }
    outcome
        .module_identity
        .clone()
        .filter(|identity| {
            identity.imei.len() == 15 && identity.imei.bytes().all(|b| b.is_ascii_digit())
        })
        .ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "call_mode_identity_unconfirmed",
                "模块没有返回可识别的 15 位 IMEI",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        service: Service,
        requests: tokio::sync::mpsc::UnboundedReceiver<(
            Action,
            oneshot::Sender<Result<Outcome, Failure>>,
        )>,
        root: PathBuf,
        _lifetime: crate::daemon_lifetime::Lease,
        trace: Arc<crate::usb_at::reconnect_tests::Trace>,
    }
    impl Fixture {
        async fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap()
                .join("build/runtime/module-radio-call-mode-tests")
                .join(crate::snapshot::new_instance_id());
            std::fs::create_dir_all(&root).unwrap();
            let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
            let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
            let service = Service::new(
                root.clone(),
                crate::usb_at::Owner::unavailable_for_tests("native-radio".into())
                    .with_recovery_gate_for_tests(gate.clone()),
                crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap(),
                BackendAvailability::with_module_recovery(gate),
                "native-radio".into(),
            );
            let (send, requests) = tokio::sync::mpsc::unbounded_channel();
            let trace =
                crate::usb_at::reconnect_tests::install_default(&service.0.usb, "original-session");
            *service.0.physical_fixture.lock().unwrap() = Some(send);
            service.0.store.save(Preferences::default()).unwrap();
            service
                .submit(Job::Configure(Configure {
                    producer_id: "native-radio".into(),
                    config_seq: 1,
                    transport: "usb".into(),
                    session_id: "original-session".into(),
                }))
                .await
                .unwrap();
            Self {
                service,
                requests,
                root,
                _lifetime: lifetime,
                trace,
            }
        }
        async fn next(&mut self) -> oneshot::Sender<Result<Outcome, Failure>> {
            let (action, reply) =
                tokio::time::timeout(Duration::from_secs(2), self.requests.recv())
                    .await
                    .unwrap()
                    .unwrap();
            assert!(matches!(action, Action::CallModeIdentityRead));
            reply
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = self.service.0.modem.shutdown() {
                if std::thread::panicking() {
                    crate::diagnostics::write_line(format_args!(
                        "fixture modem shutdown unconfirmed during unwind: {error:?}"
                    ));
                } else {
                    panic!("fixture modem shutdown unconfirmed: {error:?}");
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn identity(imei: &str) -> Outcome {
        Outcome {
            complete: true,
            verified: true,
            module_identity: Some(CallModeModuleIdentity {
                imei: imei.into(),
                firmware: String::new(),
            }),
            ..Outcome::default()
        }
    }
    #[tokio::test]
    async fn restore_binding_capture_uses_actual_owner_and_retains_unbound_fixture_anchor() {
        let mut f = Fixture::new().await;
        let lease = f
            .service
            .reserve_call_mode_restore_operation()
            .await
            .unwrap();
        assert_eq!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .unwrap(),
            None
        );
        assert_eq!(*f.trace.commands.lock().unwrap(), ["AT+CGSN", "ATI"]);
        let anchor = f
            .service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .reconnect
            .clone()
            .unwrap();
        // An ownership-only transport has no durable native binding. Reusing
        // its retained anchor does not probe or call a generic open factory.
        assert_eq!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .unwrap(),
            None
        );
        assert!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .reconnect
                .as_ref()
                .unwrap()
                .same_instance(&anchor)
        );
        assert_eq!(*f.trace.commands.lock().unwrap(), ["AT+CGSN", "ATI"]);
        assert!(f.requests.try_recv().is_err());
        f.service.release_call_mode_operation(&lease).await.unwrap();
        assert!(crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root).is_ok());
    }
    #[tokio::test]
    async fn restore_binding_native_capture_failure_is_an_error_without_fallback_or_write() {
        let mut f = Fixture::new().await;
        let lease = f
            .service
            .reserve_call_mode_restore_operation()
            .await
            .unwrap();
        f.trace.anchor_enabled.store(false, Ordering::Release);
        assert_eq!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .unwrap_err()
                .code,
            "usb_at_target_unconfirmed"
        );
        assert!(f.trace.commands.lock().unwrap().is_empty());
        assert!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .reconnect
                .is_none()
        );
        assert!(f.requests.try_recv().is_err());
        f.service.release_call_mode_operation(&lease).await.unwrap();
    }
    #[tokio::test]
    async fn restore_binding_capture_requires_exact_restore_lease_and_preserves_unknown() {
        let f = Fixture::new().await;
        let normal = f.service.reserve_call_mode_operation().await.unwrap();
        assert!(
            f.service
                .capture_restore_physical_binding(&normal)
                .await
                .is_err()
        );
        assert!(f.trace.commands.lock().unwrap().is_empty());
        f.service
            .release_call_mode_operation(&normal)
            .await
            .unwrap();
        let lease = f
            .service
            .reserve_call_mode_restore_operation()
            .await
            .unwrap();
        let mut foreign = lease.clone();
        foreign.sequence += 1;
        assert!(
            f.service
                .capture_restore_physical_binding(&foreign)
                .await
                .is_err()
        );
        {
            let mut state = f.service.0.state.lock().unwrap();
            state.reservation.as_mut().unwrap().unknown = true;
            state.observed.uncertain = true;
        }
        assert!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .is_err()
        );
        assert!(f.trace.commands.lock().unwrap().is_empty());
        let state = f.service.0.state.lock().unwrap();
        assert!(state.reservation.as_ref().unwrap().unknown && state.observed.uncertain);
        drop(state);
        f.service.release_call_mode_operation(&lease).await.unwrap();
    }
    #[tokio::test]
    async fn restore_binding_capture_retains_original_finite_right_after_live_revoke() {
        let f = Fixture::new().await;
        let lease = f
            .service
            .reserve_call_mode_restore_operation()
            .await
            .unwrap();
        f.service.module_recovery().revoke_for_tests();
        assert_eq!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .unwrap(),
            None
        );
        assert_eq!(*f.trace.commands.lock().unwrap(), ["AT+CGSN", "ATI"]);
        f.service.release_call_mode_operation(&lease).await.unwrap();
        assert!(
            f.service
                .reserve_call_mode_restore_operation()
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn restore_binding_capture_explicit_serial_substitute_is_unbound_not_native() {
        let f = Fixture::new().await;
        let lease = f
            .service
            .reserve_call_mode_restore_operation()
            .await
            .unwrap();
        f.service.0.state.lock().unwrap().binding = Some(Binding::Serial { serial_session: 77 });
        // This fixture explicitly installs physical_fixture. Its invented
        // session is never represented as a native USB observation.
        assert_eq!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .unwrap(),
            None
        );
        assert!(f.trace.commands.lock().unwrap().is_empty());
        *f.service.0.physical_fixture.lock().unwrap() = None;
        // Without the explicit substitute, an absent real serial session is
        // rejected rather than silently reported as an unbound success.
        assert!(
            f.service
                .capture_restore_physical_binding(&lease)
                .await
                .is_err()
        );
        f.service.release_call_mode_operation(&lease).await.unwrap();
    }
    #[tokio::test]
    async fn native_call_mode_rebind_preserves_reservation_and_exclusive_lock() {
        let f = Fixture::new().await;
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let lock = f
            .service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .operation_lock
            .clone()
            .unwrap();
        let lock_owners = Arc::downgrade(&lock);
        let service = f.service.clone();
        let copy = lease.clone();
        let rebind =
            tokio::spawn(async move { service.call_mode_rebind(&copy, "123456789012345").await });
        assert_eq!(rebind.await.unwrap().unwrap().imei, "123456789012345");
        {
            let state = f.service.0.state.lock().unwrap();
            let reservation = state.reservation.as_ref().unwrap();
            assert_eq!(reservation.identity.token, lease.token);
            assert_eq!(reservation.identity.sequence, lease.sequence);
            assert_eq!(state.producer, lease.producer_id);
            assert!(Arc::ptr_eq(
                &lock,
                reservation.operation_lock.as_ref().unwrap()
            ));
            assert!(
                matches!(state.binding.as_ref(),Some(Binding::Usb{session_id,..}) if !session_id.is_empty() && session_id!="original-session")
            );
        }
        assert!(matches!(
            crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root),
            Err(crate::call_mode_backups::OperationFileLockError::Busy)
        ));
        drop(lock);
        let voice = f.service.borrow_call_mode_operation_voice(&lease).unwrap();
        assert_eq!(
            f.service
                .release_call_mode_operation(&lease)
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        drop(voice);
        f.service.release_call_mode_operation(&lease).await.unwrap();
        f.service.release_call_mode_operation(&lease).await.unwrap();
        let remaining_lock_owners = lock_owners.strong_count();
        let released_lock = crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root);
        assert!(
            released_lock.is_ok(),
            "call-mode lock after acknowledged release at {}: {released_lock:?}; Rust lock owners: {remaining_lock_owners}",
            f.root.display()
        );
        drop(released_lock);
    }
    #[tokio::test]
    async fn wrong_device_cannot_replace_binding_or_permit_configuration() {
        let f = Fixture::new().await;
        *f.trace.imei.lock().unwrap() = "987654321098765".into();
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let service = f.service.clone();
        let copy = lease.clone();
        let rebind =
            tokio::spawn(async move { service.call_mode_rebind(&copy, "123456789012345").await });
        assert_eq!(
            rebind.await.unwrap().unwrap_err().code,
            "call_mode_reconnect_unconfirmed"
        );
        assert_eq!(
            f.service.usb_transport_session().as_deref(),
            Some("original-session")
        );
        assert!(f.service.0.state.lock().unwrap().reservation.is_some());
        let mut stale = lease.clone();
        stale.producer_id = "different-producer".into();
        assert_eq!(
            f.service
                .call_mode_rebind(&stale, "123456789012345")
                .await
                .unwrap_err()
                .code,
            "radio_stale_generation"
        );
        assert!(f.requests.is_empty());
        f.service.release_call_mode_operation(&lease).await.unwrap();
    }
    #[tokio::test]
    async fn canceled_rebind_waiter_cannot_release_the_physical_worker() {
        let f = Fixture::new().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let block = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let hook_entered = entered.clone();
        let hook_block = block.clone();
        let once = std::sync::atomic::AtomicBool::new(false);
        *f.trace.before_command.lock().unwrap() = Some(Box::new(move |command| {
            if command == "AT+CGSN" && !once.swap(true, Ordering::AcqRel) {
                hook_entered.notify_one();
                let (lock, changed) = &*hook_block;
                let done = lock.lock().unwrap();
                let (done, timeout) = changed
                    .wait_timeout_while(done, Duration::from_secs(5), |done| !*done)
                    .unwrap();
                assert!(*done && !timeout.timed_out());
            }
        }));
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let service = f.service.clone();
        let copy = lease.clone();
        let rebind =
            tokio::spawn(async move { service.call_mode_rebind(&copy, "123456789012345").await });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        rebind.abort();
        let _ = rebind.await;
        assert_eq!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .active_reconnect_pins,
            1
        );
        assert!(matches!(
            crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root),
            Err(crate::call_mode_backups::OperationFileLockError::Busy)
        ));
        let service = f.service.clone();
        let copy = lease.clone();
        let release = tokio::spawn(async move { service.release_call_mode_operation(&copy).await });
        tokio::task::yield_now().await;
        assert!(!release.is_finished());
        *block.0.lock().unwrap() = true;
        block.1.notify_all();
        release.await.unwrap().unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
    }
    #[tokio::test]
    async fn identity_without_an_accepted_reboot_anchor_never_settles_unknown() {
        for kind in ["reboot", "call_mode_usb_composition_write"] {
            let f = Fixture::new().await;
            let lease = f.service.reserve_call_mode_operation().await.unwrap();
            {
                let mut state = f.service.0.state.lock().unwrap();
                state.observed.uncertain = true;
                let reservation = state.reservation.as_mut().unwrap();
                reservation.unknown = true;
                reservation.attempts.insert(
                    "uncertain-before-rebind".into(),
                    Attempt {
                        input: RadioAction {
                            producer_id: lease.producer_id.clone(),
                            purpose: Purpose::CallMode,
                            token: lease.token.clone(),
                            sequence: lease.sequence,
                            operation_id: "uncertain-before-rebind".into(),
                            kind: kind.into(),
                            target: None,
                            command: None,
                            timeout_ms: None,
                            voice_target: None,
                            usb_expected: None,
                            usb_target: None,
                        },
                        result: Some(Ok(Outcome {
                            submitted: true,
                            uncertain: true,
                            ..Outcome::default()
                        })),
                    },
                );
            }
            let service = f.service.clone();
            let copy = lease.clone();
            let rebind =
                tokio::spawn(
                    async move { service.call_mode_rebind(&copy, "123456789012345").await },
                );
            rebind.await.unwrap().unwrap();
            let state = f.service.0.state.lock().unwrap();
            assert_eq!(state.reservation.as_ref().unwrap().unknown, true);
            assert!(state.observed.uncertain);
            drop(state);
            f.service.release_call_mode_operation(&lease).await.unwrap();
        }
    }
    #[tokio::test]
    async fn old_epoch_release_requires_real_worker_drain_and_matching_retirement_proof() {
        let f = Fixture::new().await;
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let guard = f.service.borrow_call_mode_operation_voice(&lease).unwrap();
        let lock_owners = Arc::downgrade(&guard.operation_lock);
        let service = f.service.clone();
        let retirement =
            tokio::spawn(
                async move { service.retire_producer("next-native-producer".into()).await },
            );
        while !f.service.0.state.lock().unwrap().retiring {
            tokio::task::yield_now().await;
        }
        let service = f.service.clone();
        let copy = lease.clone();
        let release = tokio::spawn(async move { service.release_call_mode_operation(&copy).await });
        tokio::task::yield_now().await;
        assert!(!release.is_finished());
        assert!(!retirement.is_finished());
        assert!(matches!(
            crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root),
            Err(crate::call_mode_backups::OperationFileLockError::Busy)
        ));
        drop(guard);
        retirement.await.unwrap().unwrap();
        let _ = release.await.unwrap();
        f.service.release_call_mode_operation(&lease).await.unwrap();
        f.service.release_call_mode_operation(&lease).await.unwrap();
        let mut fake = lease.clone();
        fake.token = "unproven-token".into();
        assert!(f.service.release_call_mode_operation(&fake).await.is_err());
        let remaining_lock_owners = lock_owners.strong_count();
        let released_lock = crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root);
        assert!(
            released_lock.is_ok(),
            "call-mode lock after retirement release at {}: {released_lock:?}; Rust lock owners: {remaining_lock_owners}",
            f.root.display()
        );
        drop(released_lock);
    }
    #[tokio::test]
    async fn borrowed_call_mode_reader_does_not_release_operation_owner() {
        let f = Fixture::new().await;
        let operation = f.service.reserve_call_mode_operation().await.unwrap();
        let reader = f.service.reserve_call_mode_read().await.unwrap();
        assert_eq!(operation.token, reader.token);
        f.service
            .release_call_mode_operation(&reader)
            .await
            .unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_some());
        f.service
            .release_call_mode_operation(&operation)
            .await
            .unwrap();
    }
}
