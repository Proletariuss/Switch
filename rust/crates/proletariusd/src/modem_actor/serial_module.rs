//! Long-lived, actor-owned module SIM capability. No Drop path releases it:
//! network drain and exact RF restoration must precede explicit release.
use super::*;
use crate::module_esim_channel::AtCommandTransport;
use crate::usb_at::Failure;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone)]
pub(crate) struct SerialModuleHold(Arc<ModuleHoldInner>);
struct ModuleHoldInner {
    binding: SerialCardBinding,
    session: NativeSerialSession,
    token: String,
    active: AtomicBool,
    operations: AtomicUsize,
}
impl SerialModuleHold {
    pub(crate) fn binding(&self) -> SerialCardBinding {
        self.0.binding.clone()
    }
    pub(crate) fn session(&self) -> NativeSerialSession {
        self.0.session.clone()
    }
    pub(super) fn token(&self) -> &str {
        &self.0.token
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
pub(super) enum ModuleRequest {
    Retain {
        binding: SerialCardBinding,
        token: String,
        reply: SyncSender<Result<SerialModuleHold, ActorError>>,
    },
    Release {
        hold: SerialModuleHold,
        reply: SyncSender<Result<(), ActorError>>,
    },
}
pub(super) struct Operation(SerialModuleHold);
impl Drop for Operation {
    fn drop(&mut self) {
        self.0.0.operations.fetch_sub(1, Ordering::AcqRel);
    }
}
impl ModemActor {
    pub(super) fn retain_module_radio_operation(
        &self,
        session: u64,
        token: &str,
    ) -> Result<Operation, Failure> {
        let held = self
            .client
            .card_shared
            .module_hold
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let hold = held
            .as_ref()
            .filter(|hold| {
                hold.0.active.load(Ordering::Acquire)
                    && hold.0.binding.serial_session == session
                    && hold.token() == token
            })
            .ok_or_else(|| as_failure(card_stale()))?;
        hold.0.operations.fetch_add(1, Ordering::AcqRel);
        Ok(Operation(hold.clone()))
    }
    pub(crate) fn serial_module_held(&self) -> bool {
        self.client
            .card_shared
            .module_hold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    pub(crate) fn serial_module_hold_matches(&self, serial_session: u64, token: &str) -> bool {
        self.client
            .card_shared
            .module_hold
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|hold| {
                hold.0.active.load(Ordering::Acquire)
                    && hold.0.binding.serial_session == serial_session
                    && hold.token() == token
            })
    }
    pub(crate) fn serial_module_current(&self, hold: &SerialModuleHold) -> bool {
        hold.0.active.load(Ordering::Acquire)
            && self.serial_card_binding_current(&hold.0.binding)
            && self
                .client
                .card_shared
                .module_hold
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|owned| owned.same(hold))
    }
    pub(crate) fn serial_module_closed(&self, hold: &SerialModuleHold) -> bool {
        hold.0.operations.load(Ordering::Acquire) == 0
            && hold
                .0
                .session
                .observation()
                .terminal
                .is_some_and(|terminal| {
                    terminal.physical_closed
                        && terminal.owner_epoch == hold.0.binding.owner_epoch
                        && terminal.serial_session == hold.0.binding.serial_session
                })
    }
    pub(crate) async fn retain_serial_module(
        &self,
        binding: &SerialCardBinding,
        token: &str,
    ) -> Result<SerialModuleHold, Failure> {
        let actor = self.clone();
        let binding = binding.clone();
        let token = token.to_owned();
        tokio::task::spawn_blocking(move || {
            actor
                .card_request(|reply| {
                    CardRequest::Module(ModuleRequest::Retain {
                        binding,
                        token,
                        reply,
                    })
                })
                .map_err(as_failure)
        })
        .await
        .map_err(|_| as_failure(card_stale()))?
    }
    pub(crate) async fn release_serial_module(
        &self,
        hold: &SerialModuleHold,
    ) -> Result<(), Failure> {
        let actor = self.clone();
        let hold = hold.clone();
        tokio::task::spawn_blocking(move || {
            actor
                .card_request(|reply| CardRequest::Module(ModuleRequest::Release { hold, reply }))
                .map_err(as_failure)
        })
        .await
        .map_err(|_| as_failure(card_stale()))?
    }
    pub(crate) async fn sim_identity_transaction<T, F>(
        &self,
        hold: &SerialModuleHold,
        operation: F,
    ) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut dyn AtCommandTransport<Error = Failure>) -> Result<T, Failure>
            + Send
            + 'static,
    {
        // Increment before queueing the worker: even a cancelled observer cannot
        // turn a queued physical operation into a close/settlement receipt.
        let retained = {
            let owned = self
                .client
                .card_shared
                .module_hold
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !hold.0.active.load(Ordering::Acquire)
                || !owned.as_ref().is_some_and(|owned| owned.same(hold))
            {
                return Err(as_failure(card_stale()));
            }
            hold.0.operations.fetch_add(1, Ordering::AcqRel);
            Operation(hold.clone())
        };
        let actor = self.clone();
        let hold = hold.clone();
        tokio::task::spawn_blocking(move || {
            let _retained = retained;
            let permit = actor
                .card_request(|reply| CardRequest::BeginSimIdentity { hold, reply })
                .map_err(as_failure)?;
            let mut transport = SerialCardTransaction {
                actor,
                permit: Some(permit),
            };
            let result = operation(&mut transport);
            let cleanup = transport.finish();
            match (result, cleanup) {
                (_, Err(error)) => Err(error),
                (result, Ok(())) => result,
            }
        })
        .await
        .map_err(|_| {
            let mut failure = as_failure(card_stale());
            failure.uncertain = true;
            failure
        })?
    }
}
impl ActorState {
    fn module_idle(&self) -> bool {
        self.card_lease.is_none()
            && !self.native.refreshing
            && self.pending_command.is_none()
            && self.pending_high_commands.is_empty()
            && self.pending_normal_commands.is_empty()
            && self.deferred_sms_submit.is_none()
            && self.deferred_radio.is_none()
            && !self.initialization.running
            && !self.sms_receive.is_active()
    }
    pub(super) fn process_module_request(&mut self, request: ModuleRequest) {
        match request {
            ModuleRequest::Retain {
                binding,
                token,
                reply,
            } => {
                let result = if let Some(hold) = &self.module_hold {
                    if hold.0.binding == binding && hold.token() == token {
                        Ok(hold.clone())
                    } else {
                        Err(card_busy())
                    }
                } else if !self.native.managed()
                    || token.is_empty()
                    || self.local_card_binding().as_ref() != Some(&binding)
                {
                    Err(card_stale())
                } else if !self.module_idle() {
                    Err(card_busy())
                } else if let Some(session) = self.native.session.clone().filter(|s| {
                    s.owner_epoch == binding.owner_epoch
                        && s.serial_session == binding.serial_session
                }) {
                    let hold = SerialModuleHold(Arc::new(ModuleHoldInner {
                        binding,
                        session,
                        token,
                        active: AtomicBool::new(true),
                        operations: AtomicUsize::new(0),
                    }));
                    self.module_hold = Some(hold.clone());
                    *self
                        .card_shared
                        .module_hold
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(hold.clone());
                    Ok(hold)
                } else {
                    Err(card_stale())
                };
                // Retain even if the observer vanished. The same token can
                // recover the capability and complete explicit cleanup.
                let _ = reply.send(result);
            }
            ModuleRequest::Release { hold, reply } => {
                let mut published = self
                    .card_shared
                    .module_hold
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let result = if !hold.0.active.load(Ordering::Acquire) {
                    Ok(())
                } else if !self
                    .module_hold
                    .as_ref()
                    .is_some_and(|owned| owned.same(&hold))
                {
                    Err(card_stale())
                } else if hold.0.operations.load(Ordering::Acquire) != 0 || !self.module_idle() {
                    Err(card_busy())
                } else {
                    hold.0.active.store(false, Ordering::Release);
                    self.module_hold = None;
                    *published = None;
                    self.wake_sms_receive();
                    Ok(())
                };
                let _ = reply.send(result);
            }
        }
    }
    pub(super) fn begin_sim_identity(
        &mut self,
        hold: SerialModuleHold,
        reply: SyncSender<Result<u64, ActorError>>,
    ) {
        if !hold.0.active.load(Ordering::Acquire)
            || !self
                .module_hold
                .as_ref()
                .is_some_and(|owned| owned.same(&hold))
            || self.local_card_binding().as_ref() != Some(&hold.0.binding)
        {
            let _ = reply.send(Err(card_stale()));
            return;
        }
        if !self.module_idle() {
            let _ = reply.send(Err(card_busy()));
            return;
        }
        let Some(permit) = self.card_next_lease.checked_add(1) else {
            let _ = reply.send(Err(card_stale()));
            return;
        };
        self.card_next_lease = permit;
        self.card_lease = Some(CardLease {
            permit,
            binding: hold.binding(),
            channel: ChannelState::Closed,
            poisoned: false,
            module: Some(hold),
        });
        if reply.send(Ok(permit)).is_err() {
            self.card_lease = None;
        }
    }
}
