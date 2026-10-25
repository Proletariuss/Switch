//! Coordinator lifetime primitive shared by Service, HTTP admission and shutdown.
//! Reserve before captures or spawn, then put every coordinator-owned
//! Runtime/future inside the named envelope.
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use tokio::sync::Notify;

const CLOSED: usize = 1usize << (usize::BITS - 1);
const COUNT: usize = CLOSED - 1;
const CANCELLED: u8 = 1;
const REPORTED_UNCERTAIN: u8 = 2;
const DESTRUCTOR_UNWIND: u8 = 4;
const ACCOUNTING_UNCERTAIN: u8 = 8;

#[derive(Clone)]
pub(crate) struct CoordinatorOwners(Arc<State>);
struct State {
    word: AtomicUsize,
    maximum: usize,
    faults: AtomicU8,
    changed: Notify,
    accepted: AtomicUsize,
    full_refusals: AtomicUsize,
    closed_refusals: AtomicUsize,
    unavailable_refusals: AtomicUsize,
    peak: AtomicUsize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Full,
    Closed,
    Unavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidLimit;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) pending: usize,
    pub(crate) maximum: usize,
    pub(crate) closed: bool,
    pub(crate) faults: u8,
    pub(crate) accepted: usize,
    pub(crate) full_refusals: usize,
    pub(crate) closed_refusals: usize,
    pub(crate) unavailable_refusals: usize,
    pub(crate) peak: usize,
}
fn increment(counter: &AtomicUsize) {
    let _ = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        Some(n.saturating_add(1))
    });
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Finish {
    Clean,
    Uncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainError {
    AdmissionOpen,
    Unconfirmed(u8),
}
impl std::fmt::Display for DrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AdmissionOpen => f.write_str("network coordinator admission remains open"),
            Self::Unconfirmed(_) => f.write_str("network coordinator completion unconfirmed"),
        }
    }
}

impl CoordinatorOwners {
    pub(crate) fn new(maximum: NonZeroUsize) -> Result<Self, InvalidLimit> {
        if maximum.get() > COUNT {
            return Err(InvalidLimit);
        }
        Ok(Self(Arc::new(State {
            word: AtomicUsize::new(0),
            maximum: maximum.get(),
            faults: AtomicU8::new(0),
            changed: Notify::new(),
            accepted: AtomicUsize::new(0),
            full_refusals: AtomicUsize::new(0),
            closed_refusals: AtomicUsize::new(0),
            unavailable_refusals: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })))
    }
    // Synchronous: closed flag + enrollment are one linearization point.
    // No caller future, factory, Runtime, or waiter is accepted on refusal.
    pub(crate) fn try_reserve(&self) -> Result<CoordinatorTicket, Refusal> {
        if self.0.faults.load(Ordering::Acquire) & ACCOUNTING_UNCERTAIN != 0 {
            increment(&self.0.unavailable_refusals);
            return Err(Refusal::Unavailable);
        }
        let previous = self
            .0
            .word
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (word & CLOSED == 0 && word & COUNT < self.0.maximum).then(|| word + 1)
            })
            .map_err(|word| {
                if word & CLOSED != 0 {
                    increment(&self.0.closed_refusals);
                    Refusal::Closed
                } else {
                    increment(&self.0.full_refusals);
                    Refusal::Full
                }
            })?;
        increment(&self.0.accepted);
        self.0
            .peak
            .fetch_max((previous & COUNT) + 1, Ordering::AcqRel);
        Ok(CoordinatorTicket {
            state: Some(self.0.clone()),
        })
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        let word = self.0.word.load(Ordering::Acquire);
        Snapshot {
            pending: word & COUNT,
            maximum: self.0.maximum,
            closed: word & CLOSED != 0,
            faults: self.0.faults.load(Ordering::Acquire),
            accepted: self.0.accepted.load(Ordering::Acquire),
            full_refusals: self.0.full_refusals.load(Ordering::Acquire),
            closed_refusals: self.0.closed_refusals.load(Ordering::Acquire),
            unavailable_refusals: self.0.unavailable_refusals.load(Ordering::Acquire),
            peak: self.0.peak.load(Ordering::Acquire),
        }
    }
    pub(crate) fn begin_shutdown(&self) {
        self.0.word.fetch_or(CLOSED, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
    pub(crate) async fn finish_pending(&self) -> Result<(), DrainError> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let word = self.0.word.load(Ordering::Acquire);
            if word & CLOSED == 0 {
                return Err(DrainError::AdmissionOpen);
            }
            if word & COUNT == 0 {
                let faults = self.0.faults.load(Ordering::Acquire);
                return if faults == 0 {
                    Ok(())
                } else {
                    Err(DrainError::Unconfirmed(faults))
                };
            }
            changed.await;
        }
    }
}
impl State {
    fn retire(&self, faults: u8) {
        self.faults.fetch_or(faults, Ordering::AcqRel);
        if self
            .word
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (word & COUNT != 0).then(|| word - 1)
            })
            .is_err()
        {
            self.faults.fetch_or(ACCOUNTING_UNCERTAIN, Ordering::AcqRel);
        }
        self.changed.notify_waiters();
    }
}

#[must_use = "transfer this reservation to the named coordinator before spawn"]
pub(crate) struct CoordinatorTicket {
    state: Option<Arc<State>>,
}
impl CoordinatorTicket {
    // Build this envelope synchronously, before returning any outer future.
    // No overload failure occurs after accepting the caller's future.
    pub(crate) fn into_runner<F>(mut self, future: F) -> OwnedCoordinator<F>
    where
        F: Future<Output = Finish> + Send + 'static,
    {
        OwnedCoordinator {
            inner: Some(Box::pin(future)),
            witness: Witness {
                state: self.state.take(),
                started: false,
                report: None,
            },
        }
    }
}
impl Drop for CoordinatorTicket {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // A bare unused ticket never accepted a caller payload.
            state.retire(if std::thread::panicking() {
                DESTRUCTOR_UNWIND
            } else {
                0
            });
        }
    }
}

// Declaration order is an unwind fallback. The final witness runs after the
// inner future and all Runtime/resource captures, not after a JoinHandle drop.
pub(crate) struct OwnedCoordinator<F> {
    inner: Option<Pin<Box<F>>>,
    witness: Witness,
}
impl<F> Future for OwnedCoordinator<F>
where
    F: Future<Output = Finish>,
{
    type Output = Finish;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Finish> {
        let this = self.get_mut();
        this.witness.started = true;
        match this
            .inner
            .as_mut()
            .expect("coordinator polled before destruction")
            .as_mut()
            .poll(cx)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(report) => {
                this.witness.report = Some(report);
                Poll::Ready(report)
            }
        }
    }
}
impl<F> Drop for OwnedCoordinator<F> {
    fn drop(&mut self) {
        drop(self.inner.take());
    }
}
struct Witness {
    state: Option<Arc<State>>,
    started: bool,
    report: Option<Finish>,
}
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let mut faults = 0;
            if std::thread::panicking() {
                faults |= DESTRUCTOR_UNWIND;
            }
            if self.started && self.report.is_none() {
                faults |= CANCELLED;
            }
            if self.report == Some(Finish::Uncertain) {
                faults |= REPORTED_UNCERTAIN;
            }
            state.retire(faults);
        }
    }
}

#[cfg(test)]
#[path = "coordinator_owner_tests.rs"]
mod tests;
