//! Non-wire authority retaining one physical lease across a card workflow.
use super::*;

pub(super) struct Retention {
    borrower: u64,
    recovery_until: Option<Instant>,
}

/// This guard does not create or renew a lease. Fresh authority can be revoked
/// independently while its already-started recovery and physical cleanup drain.
pub(crate) struct CardWorkflow {
    owner: Owner,
    worker: Arc<Worker>,
    binding: Binding,
    borrower: u64,
    operation: Mutex<()>,
}

impl Owner {
    pub(crate) fn retain_card_workflow(&self, binding: &Binding) -> Result<CardWorkflow, Failure> {
        self.check_admission()?;
        binding.validate_for_reader(&binding.reader)?;
        let worker = self.worker(&binding.producer_id, &binding.reader)?;
        let borrower = {
            let mut state = worker.state.lock().unwrap();
            check_live(&mut state, binding)?;
            if state.native_borrow.is_some()
                || state.wire_cache.iter().any(|entry| entry.result.is_none())
            {
                return Err(Failure::busy());
            }
            state.native_sequence = state
                .native_sequence
                .checked_add(1)
                .ok_or_else(Failure::stale)?;
            let borrower = state.native_sequence;
            state.native_borrow = Some(borrower);
            state.card_workflow = Some(Retention {
                borrower,
                recovery_until: None,
            });
            borrower
        };
        Ok(CardWorkflow {
            owner: self.clone(),
            worker,
            binding: binding.clone(),
            borrower,
            operation: Mutex::new(()),
        })
    }
}

impl CardWorkflow {
    /// Admission for fresh work, including AUTH: recovery never revives it.
    pub(crate) fn checkpoint(&self) -> Result<(), Failure> {
        self.owner.check_admission()?;
        let mut state = self.worker.state.lock().unwrap();
        identity(&state, &self.binding, Some(self.borrower))?;
        check_live(&mut state, &self.binding)
    }

    /// Arm once immediately before submitting ENABLE. This cannot renew an
    /// expired/retired lease or extend a previously armed recovery deadline.
    pub(crate) fn recovery_started(&self, budget: Duration) -> Result<(), Failure> {
        if budget.is_zero() || budget > Duration::from_secs(45) {
            return Err(Failure::invalid());
        }
        self.owner.check_admission()?;
        let mut state = self.worker.state.lock().unwrap();
        identity(&state, &self.binding, Some(self.borrower))?;
        check_live(&mut state, &self.binding)?;
        let retained = state.card_workflow.as_mut().ok_or_else(Failure::stale)?;
        if retained.recovery_until.is_some() {
            return Err(Failure::stale());
        }
        retained.recovery_until = Some(Instant::now() + budget);
        Ok(())
    }

    pub(crate) fn with_transaction<R>(
        &self,
        operation: impl FnOnce(&mut NativeTransport) -> R,
    ) -> Result<R, Failure> {
        let _operation = self.operation.lock().unwrap();
        self.admit()?;
        let mut transport = NativeTransport {
            owner: self.owner.clone(),
            worker: self.worker.clone(),
            binding: self.binding.clone(),
            borrower: self.borrower,
            temporary: false,
            // Only CardWorkflow::drop owns the persistent native borrower.
            finished: true,
        };
        Ok(operation(&mut transport))
    }

    pub(crate) fn reset(&self, action: ResetAction) -> Result<Value, Failure> {
        let _operation = self.operation.lock().unwrap();
        self.admit()?;
        if action == ResetAction::Reset && !is_estk(&self.binding.reader) {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "reader_pcsc_reset_unsupported",
                "Power cycling is only supported for ESTKme readers",
            ));
        }
        let (reply, receive) = mpsc::sync_channel(1);
        {
            let mut state = self.worker.state.lock().unwrap();
            live(&mut state, &self.binding, Some(self.borrower))?;
            self.worker
                .sender
                .try_send(Work::Reset {
                    binding: self.binding.clone(),
                    native: Some(self.borrower),
                    action,
                    sequence: 0,
                    id: String::new(),
                    reply,
                })
                .map_err(|_| Failure::busy())?;
        }
        receive.recv().unwrap_or_else(|_| Err(Failure::unknown()))
    }

    fn admit(&self) -> Result<(), Failure> {
        let admission = self.owner.check_admission();
        let mut state = self.worker.state.lock().unwrap();
        if admission.is_err() && !recovering(&state, &self.binding, Some(self.borrower)) {
            admission?;
        }
        live(&mut state, &self.binding, Some(self.borrower))
    }
}

impl Drop for CardWorkflow {
    fn drop(&mut self) {
        let mut state = self.worker.state.lock().unwrap();
        if state
            .card_workflow
            .as_ref()
            .is_some_and(|retained| retained.borrower == self.borrower)
        {
            state.card_workflow = None;
            if state.native_borrow == Some(self.borrower) {
                state.native_borrow = None;
            }
        }
        drop(state);
        self.worker.changed.notify_all();
        let _ = self.worker.sender.try_send(Work::Wake);
    }
}

pub(super) fn identity(
    state: &ReaderState,
    binding: &Binding,
    native: Option<u64>,
) -> Result<(), Failure> {
    if native.is_some()
        && state.native_borrow == native
        && state
            .card_workflow
            .as_ref()
            .is_some_and(|retained| Some(retained.borrower) == native)
        && state.producer == binding.producer_id
        && state.session == binding.session_id
        && state
            .lease
            .as_ref()
            .is_some_and(|lease| lease.binding == *binding && lease.acquired)
    {
        Ok(())
    } else {
        Err(Failure::stale())
    }
}

pub(super) fn recovering(state: &ReaderState, binding: &Binding, native: Option<u64>) -> bool {
    identity(state, binding, native).is_ok()
        && state
            .card_workflow
            .as_ref()
            .and_then(|retained| retained.recovery_until)
            .is_some_and(|until| Instant::now() < until)
}

pub(super) fn live(
    state: &mut ReaderState,
    binding: &Binding,
    native: Option<u64>,
) -> Result<(), Failure> {
    if identity(state, binding, native).is_ok() {
        expire(state);
        if let Some(error) = &state.close_error {
            return Err(error.clone());
        }
        if let Some(until) = state
            .card_workflow
            .as_ref()
            .and_then(|retained| retained.recovery_until)
        {
            return if Instant::now() < until {
                Ok(())
            } else {
                Err(Failure::expired())
            };
        }
    }
    check_live(state, binding)
}

// A failed reconnect still belongs to the retained workflow. It can request
// its bounded ESTK reset fallback; outsiders cannot use this recovery queue.
pub(super) fn wait_reset(
    worker: &Worker,
    receiver: &mpsc::Receiver<Work>,
    binding: &Binding,
    failure: Failure,
) -> Option<(ResetAction, Option<u64>, u64, String, Reply<Value>)> {
    loop {
        if worker.state.lock().unwrap().card_workflow.is_none() {
            return None;
        }
        let work = match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(work) => work,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return None,
        };
        match work {
            Work::Reset {
                binding: requested,
                native,
                action,
                sequence,
                id,
                reply,
            } => {
                let valid = {
                    let mut state = worker.state.lock().unwrap();
                    if requested != *binding || identity(&state, binding, native).is_err() {
                        Err(Failure::stale())
                    } else {
                        live(&mut state, binding, native)
                    }
                };
                match valid {
                    Ok(()) => return Some((action, native, sequence, id, reply)),
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Work::Transmit { reply, .. } => {
                let _ = reply.send(Err(failure.clone()));
            }
            Work::Acquire { reply, .. } => {
                let _ = reply.send(Err(Failure::busy()));
            }
            Work::Wake => {}
        }
    }
}
