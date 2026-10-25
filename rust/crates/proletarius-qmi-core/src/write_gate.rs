//! Deadline-aware serialization of control writes. Teardown still waits for the
//! actual active writer; a request timeout never interrupts submitted I/O.
use crate::{ClientError, lock, remaining};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Instant;

#[derive(Default)]
pub(crate) struct WriteGate {
    state: Mutex<State>,
    changed: Condvar,
}
#[derive(Default)]
struct State {
    held: bool,
    closed: bool,
}
pub(crate) struct WriteGuard<'a>(&'a WriteGate);
impl WriteGate {
    pub(crate) fn request(&self, deadline: Instant) -> Result<WriteGuard<'_>, ClientError> {
        let mut state = lock(&self.state);
        loop {
            if state.closed {
                return Err(ClientError::Closed);
            }
            let budget = remaining(deadline)?;
            if !state.held {
                state.held = true;
                return Ok(WriteGuard(self));
            }
            state = self
                .changed
                .wait_timeout(state, budget)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub(crate) fn close_admission(&self) {
        lock(&self.state).closed = true;
        self.changed.notify_all();
    }

    pub(crate) fn shutdown(&self) -> WriteGuard<'_> {
        let mut state = lock(&self.state);
        while state.held {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.held = true;
        WriteGuard(self)
    }
}
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).held = false;
        self.0.changed.notify_all();
    }
}
