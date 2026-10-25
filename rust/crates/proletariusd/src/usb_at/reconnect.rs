//! Operation-bound normal USB reconnect. A candidate is private until verified.
use super::*;
use crate::module_radio::CallModeRebindProof;
use crate::module_radio_transport::{Action, Binding, CallModeModuleIdentity, Plan, Reply};
use std::sync::Weak;

#[cfg(test)]
pub(crate) type TestOpener = Arc<
    dyn Fn(
            &usb_at_transport::PhysicalTarget,
            Option<&usb_at_transport::ReenumerationAnchor>,
            Instant,
            crate::module_radio::CallAdmission,
        ) -> Result<Box<dyn Transport>, TransportError>
        + Send
        + Sync,
>;

fn open_candidate(
    inner: &Inner,
    anchor: &Anchor,
    successor: bool,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
) -> Result<Box<dyn Transport>, TransportError> {
    #[cfg(test)]
    if let Some(opener) = inner.reconnect_opener.lock().unwrap().clone() {
        return opener(
            &anchor.source,
            successor.then_some(&anchor.transport),
            deadline,
            guard,
        );
    }
    #[cfg(not(test))]
    let _ = inner;
    if successor {
        usb_at_transport::open_successor(&anchor.transport, deadline, guard)
    } else {
        usb_at_transport::open_exact(&anchor.source, deadline, guard)
    }
}

#[derive(Clone)]
pub(crate) struct CallModeUsbAnchor(Arc<Anchor>);
pub(super) struct Anchor {
    owner: Weak<Inner>,
    // Every clone shares debt from its exact unpublished/native-open attempt.
    // A later independent anchor starts without this old operation's debt.
    close_debt: Mutex<Option<Failure>>,
    proof: CallModeRebindProof,
    producer: String,
    epoch: u64,
    source: usb_at_transport::PhysicalTarget,
    transport: usb_at_transport::ReenumerationAnchor,
    identity: CallModeModuleIdentity,
}
pub(super) struct Connection {
    anchor: Weak<Anchor>,
    session: String,
    generation: u64,
    reopening: bool,
    reboot_confirmed: bool,
    target: usb_at_transport::PhysicalTarget,
    identity: CallModeModuleIdentity,
}
impl Connection {
    pub(super) fn note_closed(&mut self, session: &str, generation: u64) {
        if !session.is_empty() && self.session == session {
            self.generation = generation;
            self.reopening = true;
        }
    }
}
impl Anchor {
    pub(super) fn note_close_failure(&self, error: Failure) {
        let mut debt = self.close_debt.lock().unwrap_or_else(|e| e.into_inner());
        if debt.is_none() {
            *debt = Some(error);
        }
    }
}

impl std::fmt::Debug for CallModeUsbAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallModeUsbAnchor").finish_non_exhaustive()
    }
}
pub(crate) struct ConfirmedUsbRebind {
    anchor: CallModeUsbAnchor,
    session: String,
    identity: CallModeModuleIdentity,
    successor: bool,
}
fn unavailable() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "call_mode_reconnect_unconfirmed",
        "原模块连接的有限重连能力未确认，未采用其他USB设备",
    )
}
impl CallModeUsbAnchor {
    /// Export data from this still-current original native connection. A
    /// decoded data object cannot be used to construct this owner-bound anchor.
    pub(crate) fn durable_binding(
        &self,
        proof: &CallModeRebindProof,
    ) -> Result<Option<usb_at_transport::DurableUsbBinding>, Failure> {
        if !self.0.proof.same_operation(proof) {
            return Err(Failure::stale());
        }
        proof.validate().map_err(|_| unavailable())?;
        let owner = self.0.owner.upgrade().ok_or_else(Failure::stale)?;
        {
            let state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
            self.check(&state, false)?;
            if !state
                .reconnect
                .as_ref()
                .is_some_and(|slot| slot.target.same_connection(&self.0.source))
            {
                return Err(Failure::stale());
            }
        }
        let binding = self.0.transport.durable_binding().map_err(Failure::from)?;
        proof.validate().map_err(|_| unavailable())?;
        // Neither proof validation nor data construction holds the USB mutex.
        let state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check(&state, false)?;
        if !state
            .reconnect
            .as_ref()
            .is_some_and(|slot| slot.target.same_connection(&self.0.source))
        {
            return Err(Failure::stale());
        }
        Ok(binding)
    }
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn validate_for_write(&self, action: &Action) -> Result<(), Failure> {
        let owner = self.0.owner.upgrade().ok_or_else(Failure::stale)?;
        let state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check(&state, false)?;
        if let Action::CallModeUsbCompositionWrite { expected, target } = action {
            // QCFG's module IDs are not the outside USB descriptor. Flags-only
            // changes keep those module IDs; no unaudited identity transition.
            if (expected.vendor_id, expected.product_id) != (target.vendor_id, target.product_id) {
                return Err(Failure::new(
                    StatusCode::CONFLICT,
                    "call_mode_usb_transition_unsupported",
                    "所选USB目标不属于原实际设备已审核的同VID/PID重枚举范围，未写入配置",
                ));
            }
        }
        if matches!(action, Action::Reboot)
            && state
                .reconnect
                .as_ref()
                .is_some_and(|slot| slot.reboot_confirmed)
        {
            return Err(unavailable());
        }
        Ok(())
    }
    fn check(&self, state: &State, allow_closed: bool) -> Result<(), Failure> {
        if let Some(error) = &*self.0.close_debt.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(error.clone());
        }
        check(state, &self.0.producer)?;
        check_epoch(state, self.0.epoch)?;
        if !self
            .0
            .proof
            .right()
            .is_active_for(&state.module_recovery, &self.0.producer)
        {
            return Err(unavailable());
        }
        let slot = state.reconnect.as_ref().ok_or_else(unavailable)?;
        if let Some(error) = close_failure_for(state, &self.0.producer, &slot.session) {
            return Err(error);
        }
        if let Binding::Usb {
            producer_id,
            session_id,
        } = self.0.proof.binding()
        {
            if let Some(error) = close_failure_for(state, producer_id, session_id) {
                return Err(error);
            }
        }
        if !slot.anchor.ptr_eq(&Arc::downgrade(&self.0))
            || slot.generation != state.identity_generation
        {
            return Err(Failure::stale());
        }
        if slot.reopening {
            if !allow_closed || !state.session.is_empty() {
                return Err(Failure::stale());
            }
        } else if state.session.is_empty() || slot.session != state.session {
            return Err(Failure::stale());
        }
        if state.lease.is_some() || !state.inhibits.is_empty() || state.radio_inhibit.is_some() {
            return Err(Failure::busy());
        }
        Ok(())
    }
}
impl ConfirmedUsbRebind {
    pub(crate) fn binding(&self) -> Binding {
        Binding::Usb {
            producer_id: self.anchor.0.producer.clone(),
            session_id: self.session.clone(),
        }
    }
    pub(crate) fn identity(&self) -> &CallModeModuleIdentity {
        &self.identity
    }
    pub(crate) fn successor_confirmed(&self) -> bool {
        self.successor
    }
    pub(crate) fn validate(&self) -> Result<(), Failure> {
        self.anchor
            .validate_for_write(&Action::CallModeIdentityRead)?;
        let owner = self.anchor.0.owner.upgrade().ok_or_else(Failure::stale)?;
        if owner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session
            != self.session
        {
            return Err(Failure::stale());
        }
        Ok(())
    }
}

fn identity_on(
    transport: &mut dyn Transport,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> Result<CallModeModuleIdentity, Failure> {
    let mut plan = Plan::new_with_cancelled(
        Action::CallModeIdentityRead,
        deadline.saturating_duration_since(Instant::now()),
        cancelled,
    );
    while !plan.done() {
        if !guard() || Instant::now() >= deadline {
            return Err(unavailable());
        }
        let Some(step) = plan.next() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if step.write {
            return Err(unavailable());
        }
        let reply = match transport.command_with_guard(
            &step.command,
            None,
            step.timeout
                .min(deadline.saturating_duration_since(Instant::now())),
            crate::module_radio_transport::RESPONSE_LIMIT,
            guard.clone(),
        ) {
            Ok(response) => Reply::response(response),
            Err(error) => Reply::error(&Failure::from(error)),
        };
        plan.accept(reply);
    }
    // ATI is advisory, but the Plan still marks a broken/unaligned firmware
    // stream for retirement even when the strict IMEI query succeeded.
    if plan.retire_transport() {
        return Err(unavailable());
    }
    let outcome = plan.finish();
    if !guard() || !outcome.complete || !outcome.verified || outcome.uncertain {
        return Err(unavailable());
    }
    outcome
        .module_identity
        .filter(|identity| {
            identity.imei.len() == 15 && identity.imei.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(unavailable)
}

impl Owner {
    pub(crate) async fn capture_call_mode_reconnect(
        &self,
        proof: CallModeRebindProof,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<CallModeUsbAnchor, Failure> {
        proof
            .validate_for_usb_owner(self)
            .map_err(|_| unavailable())?;
        let Binding::Usb {
            producer_id,
            session_id,
        } = proof.binding().clone()
        else {
            return Err(Failure::invalid());
        };
        let pin = proof.pin_worker().map_err(|_| unavailable())?;
        let permit = self
            .admit_with_right(
                &producer_id,
                Some(&session_id),
                "",
                deadline,
                Some(&proof.right()),
            )
            .await?;
        let inner = self.inner.clone();
        self.run(permit, move |_, epoch| {
            let _pin = pin;
            let guard_inner = inner.clone();
            let guard_proof = proof.clone();
            let guard_producer = producer_id.clone();
            let guard_session = session_id.clone();
            let identity_cancelled = cancelled.clone();
            let guard: crate::module_radio::CallAdmission = Arc::new(move || {
                !cancelled.load(std::sync::atomic::Ordering::Acquire)
                    && Instant::now() < deadline
                    && guard_proof.validate().is_ok()
                    && check_radio_fence(&guard_inner, epoch, &guard_producer, &guard_session, None)
                        .is_ok()
            });
            if !guard() {
                return Err(unavailable());
            }
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            let transport = port.as_mut().ok_or_else(Failure::stale)?;
            let source = transport.physical_target().map_err(Failure::from)?;
            let captured = transport
                .capture_reenumeration_anchor(deadline, guard.clone())
                .map_err(Failure::from)?;
            if !captured.same_source(&source) || !guard() {
                return Err(unavailable());
            }
            let identity = match identity_on(
                transport.as_mut(),
                deadline,
                guard.clone(),
                identity_cancelled,
            ) {
                Ok(identity) => identity,
                Err(error) => {
                    native_module::close_port(&inner, &mut port)?;
                    detached(&inner, epoch);
                    return Err(error);
                }
            };
            if !guard() {
                return Err(unavailable());
            }
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            check_epoch(&state, epoch)?;
            if state.session != session_id || state.producer != producer_id {
                return Err(Failure::stale());
            }
            if state
                .reconnect
                .as_ref()
                .and_then(|slot| slot.anchor.upgrade())
                .is_some_and(|anchor| {
                    anchor
                        .proof
                        .right()
                        .is_active_for(&state.module_recovery, &producer_id)
                })
            {
                return Err(Failure::busy());
            }
            let anchor = CallModeUsbAnchor(Arc::new(Anchor {
                owner: Arc::downgrade(&inner),
                close_debt: Mutex::new(None),
                proof,
                producer: producer_id,
                epoch,
                source,
                transport: captured,
                identity,
            }));
            state.reconnect = Some(Connection {
                anchor: Arc::downgrade(&anchor.0),
                session: session_id,
                generation: state.identity_generation,
                reopening: false,
                reboot_confirmed: false,
                target: anchor.0.source.clone(),
                identity: anchor.0.identity.clone(),
            });
            Ok(anchor)
        })
        .await
    }

    pub(crate) async fn reconnect_call_mode(
        &self,
        anchor: CallModeUsbAnchor,
        proof: CallModeRebindProof,
        expected_imei: &str,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ConfirmedUsbRebind, Failure> {
        proof
            .validate_for_usb_owner(self)
            .map_err(|_| unavailable())?;
        if !anchor.0.proof.same_operation(&proof) || anchor.0.identity.imei != expected_imei {
            return Err(unavailable());
        }
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            anchor.check(&state, true)?;
        }
        let producer = anchor.0.producer.clone();
        let pin = proof.pin_worker().map_err(|_| unavailable())?;
        let permit = self
            .admit_with_right(&producer, None, "", deadline, Some(&proof.right()))
            .await?;
        let inner = self.inner.clone();
        self.run(permit, move |_, epoch| {
            let _pin = pin;
            proof.validate().map_err(|_| unavailable())?;
            let mut port = inner.port.lock().unwrap_or_else(|e| e.into_inner());
            {
                let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                anchor.check(&state, true)?;
                if !state.session.is_empty() {
                    let target = port
                        .as_ref()
                        .ok_or_else(Failure::stale)?
                        .physical_target()
                        .map_err(Failure::from)?;
                    let slot = state.reconnect.as_ref().ok_or_else(unavailable)?;
                    if !slot.target.same_connection(&target) {
                        return Err(Failure::stale());
                    }
                    if slot.reboot_confirmed && proof.successor_required() {
                        return Ok(ConfirmedUsbRebind {
                            anchor: anchor.clone(),
                            session: state.session.clone(),
                            identity: slot.identity.clone(),
                            successor: true,
                        });
                    }
                } else if port.is_some() {
                    return Err(Failure::stale());
                }
            }
            if port.is_some() {
                native_module::close_port(&inner, &mut port)?;
                detached(&inner, epoch);
                let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let generation = state.identity_generation;
                let slot = state.reconnect.as_mut().ok_or_else(unavailable)?;
                slot.generation = generation;
                slot.reopening = true;
            }
            let guard_inner = inner.clone();
            let guard_proof = proof.clone();
            let guard_anchor = anchor.clone();
            let identity_cancelled = cancelled.clone();
            let guard: crate::module_radio::CallAdmission = Arc::new(move || {
                !cancelled.load(std::sync::atomic::Ordering::Acquire)
                    && Instant::now() < deadline
                    && guard_proof.validate().is_ok()
                    && guard_anchor
                        .check(
                            &guard_inner.state.lock().unwrap_or_else(|e| e.into_inner()),
                            true,
                        )
                        .is_ok()
            });
            if !guard() {
                return Err(unavailable());
            }
            // No generic factory: one exact Device or one bounded, sticky,
            // policy-verified successor selection attempt from the original anchor.
            let successor = proof.successor_required();
            let mut attempt = begin_port_attempt(&inner, epoch)?;
            attempt.reconnect = Some(Arc::downgrade(&anchor.0));
            let mut candidate = pending_port(&inner, attempt, || {
                open_candidate(&inner, &anchor.0, successor, deadline, guard.clone())
            })?;
            let target = candidate.physical_target().map_err(Failure::from)?;
            if !successor && !anchor.0.source.same_connection(&target) {
                return Err(unavailable());
            }
            let identity = identity_on(
                candidate.as_mut(),
                deadline,
                guard.clone(),
                identity_cancelled,
            )?;
            if identity.imei != anchor.0.identity.imei || !guard() {
                return Err(unavailable());
            }
            let description = candidate.description();
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            anchor.check(&state, true)?;
            if port.is_some() || !state.session.is_empty() {
                return Err(Failure::stale());
            }
            let generation = state
                .identity_generation
                .checked_add(1)
                .ok_or_else(Failure::stale)?;
            let session = crate::snapshot::new_instance_id();
            install_call_capture(&inner, candidate.as_mut(), &producer, &session);
            state.identity_generation = generation;
            state.module_identity = None;
            state.native_sms_identity = None;
            state.device_observation = None;
            state.description = description;
            state.next_open = None;
            state.session = session.clone();
            let slot = state.reconnect.as_mut().ok_or_else(unavailable)?;
            slot.generation = generation;
            slot.session = session.clone();
            slot.reopening = false;
            slot.reboot_confirmed |= successor;
            slot.target = target;
            slot.identity = identity.clone();
            let (candidate, port_identity) = candidate.commit(session.clone());
            state.port_identity = Some(port_identity);
            *port = Some(candidate);
            Ok(ConfirmedUsbRebind {
                anchor,
                session,
                identity,
                successor,
            })
        })
        .await
    }
}
