//! Bounded provider disk commands with completion tied to real resources.
//!
//! A dropped observer can abort queued work, but cannot retire a started
//! command, its queue guard, or an unconsumed output containing attempt rights.
use super::{ReleaseLedger, WorkerError};
use crate::QueueStore;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{Mutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

const CAPACITY: usize = 64;
const JOB: u8 = 1;
const OUTPUT: u8 = 2;
const OBSERVER: u8 = 4;

pub(super) struct Owner {
    state: StdMutex<State>,
    changed: Notify,
    slots: Arc<Semaphore>,
    #[cfg(any(test, feature = "queue-fault-tests"))]
    hooks: StdMutex<Vec<TestHook>>,
    #[cfg(any(test, feature = "queue-fault-tests"))]
    calls: std::sync::atomic::AtomicUsize,
}
#[derive(Default)]
struct State {
    closed: bool,
    pending: usize,
    unconfirmed: bool,
}
impl Default for Owner {
    fn default() -> Self {
        Self {
            state: StdMutex::new(State::default()),
            changed: Notify::new(),
            slots: Arc::new(Semaphore::new(1)),
            #[cfg(any(test, feature = "queue-fault-tests"))]
            hooks: StdMutex::new(Vec::new()),
            #[cfg(any(test, feature = "queue-fault-tests"))]
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}
impl Owner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| {
            let mut state = error.into_inner();
            state.unconfirmed = true;
            state
        })
    }
    pub(super) fn close(&self) {
        self.state().closed = true;
    }
    fn register(self: &Arc<Self>) -> Result<Arc<Completion>, WorkerError> {
        let mut state = self.state();
        if state.unconfirmed {
            return Err(WorkerError::QueueIoUnconfirmed);
        }
        if state.closed {
            return Err(WorkerError::Stopped);
        }
        if state.pending >= CAPACITY {
            return Err(WorkerError::QueueIoBusy);
        }
        state.pending += 1;
        Ok(Arc::new(Completion {
            owner: self.clone(),
            state: StdMutex::new(Facts::default()),
        }))
    }
    pub(super) async fn finish(&self) -> Result<(), WorkerError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state();
                if state.pending == 0 {
                    return if state.unconfirmed {
                        Err(WorkerError::QueueIoUnconfirmed)
                    } else {
                        Ok(())
                    };
                }
            }
            changed.await;
        }
    }
    #[cfg(any(test, feature = "queue-fault-tests"))]
    pub(super) fn hook(&self, call: usize, after: bool, action: impl FnOnce() + Send + 'static) {
        self.hooks
            .lock()
            .unwrap()
            .push((call, after, Box::new(action)));
    }
    #[cfg(any(test, feature = "queue-fault-tests"))]
    fn checkpoint(&self, call: usize, after: bool) {
        let action = {
            let mut hooks = self.hooks.lock().unwrap();
            hooks
                .iter()
                .position(|(n, phase, _)| *n == call && *phase == after)
                .map(|index| hooks.remove(index).2)
        };
        if let Some(action) = action {
            action();
        }
    }

    #[cfg(feature = "queue-fault-tests")]
    pub(super) fn test_call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(feature = "queue-fault-tests")]
    pub(super) fn test_snapshot(&self) -> (usize, bool) {
        let state = self.state();
        (state.pending, state.unconfirmed)
    }
    /// Only this entry point can classify a close-triggered refusal as proven
    /// pre-mutation: the owner checks admission before invoking the operation.
    pub(super) fn run_begin<F, T>(
        self: &Arc<Self>,
        queue: Arc<Mutex<QueueStore>>,
        admission: Arc<ReleaseLedger>,
        operation: F,
    ) -> impl Future<Output = Result<T, WorkerError>> + Send + 'static + use<F, T>
    where
        F: FnOnce(&mut QueueStore) -> Result<T, WorkerError> + Send + 'static,
        T: Send + 'static,
    {
        self.run_checked(queue, Some(admission), operation)
    }

    // Register before returning the future; even never-polled preparations own
    // their captures and completion. No detached producer enrollment gap.
    pub(super) fn run<F, T>(
        self: &Arc<Self>,
        queue: Arc<Mutex<QueueStore>>,
        operation: F,
    ) -> impl Future<Output = Result<T, WorkerError>> + Send + 'static + use<F, T>
    where
        F: FnOnce(&mut QueueStore) -> Result<T, WorkerError> + Send + 'static,
        T: Send + 'static,
    {
        self.run_checked(queue, None, operation)
    }

    fn run_checked<F, T>(
        self: &Arc<Self>,
        queue: Arc<Mutex<QueueStore>>,
        admission: Option<Arc<ReleaseLedger>>,
        operation: F,
    ) -> impl Future<Output = Result<T, WorkerError>> + Send + 'static + use<F, T>
    where
        F: FnOnce(&mut QueueStore) -> Result<T, WorkerError> + Send + 'static,
        T: Send + 'static,
    {
        let registration = self.register();
        let slots = self.slots.clone();
        let mut request = Request {
            locking: Some(Box::pin(queue.lock_owned())),
            guard: None,
            operation: Some(operation),
            admission,
            running: None,
            observer: registration.as_ref().ok().map(|completion| Observer {
                completion: completion.clone(),
                live: false,
            }),
            witness: registration.as_ref().ok().map(|completion| JobWitness {
                completion: completion.clone(),
                started: false,
                output_created: false,
            }),
        };
        async move {
            match registration {
                Ok(_) => (),
                Err(error) => {
                    drop(request);
                    return Err(error);
                }
            };
            request.guard = Some(request.locking.as_mut().unwrap().as_mut().await);
            drop(request.locking.take());
            let permit = slots
                .acquire_owned()
                .await
                .expect("provider disk pool stays open");
            let command = Command {
                guard: request.guard.take(),
                operation: request.operation.take(),
                admission: request.admission.take(),
                permit: Some(permit),
                witness: request.witness.take().expect("registered preparation"),
            };
            // Spawn synchronously before the next await. The actual command
            // destructor, rather than JoinHandle lifetime, publishes JOB.
            let handle = tokio::task::spawn_blocking(move || command.execute());
            let abort = AbortQueued(handle.abort_handle());
            request.running = Some(Box::pin(async move {
                let _abort = abort;
                handle.await
            }));
            let result = match request.running.as_mut().unwrap().as_mut().await {
                Ok(mut output) => output.result.take().expect("one provider disk result"),
                Err(_) => Err(WorkerError::QueueIoUnconfirmed),
            };
            request.observer.as_mut().unwrap().live = true;
            drop(request);
            result
        }
    }
}
#[derive(Default)]
struct Facts {
    done: u8,
    settled: bool,
    live: bool,
    error: bool,
}
struct Completion {
    owner: Arc<Owner>,
    state: StdMutex<Facts>,
}
impl Completion {
    fn mark(&self, fact: u8, live: bool, error: bool) {
        let mut state = self.state.lock().unwrap_or_else(|poison| {
            self.owner.state().unconfirmed = true;
            poison.into_inner()
        });
        state.done |= fact;
        state.live |= live;
        state.error |= error;
        if state.done != (JOB | OUTPUT | OBSERVER) || state.settled {
            return;
        }
        state.settled = true;
        let mut owner = self.owner.state();
        if let Some(n) = owner.pending.checked_sub(1) {
            owner.pending = n;
        } else {
            owner.unconfirmed = true;
        }
        if state.error && !state.live {
            // No live observer received this failure. Retain an authoritative
            // fence so a replacement cannot turn a lost error into success.
            owner.unconfirmed = true;
        }
        drop(owner);
        drop(state);
        self.owner.changed.notify_waiters();
    }
}
struct Observer {
    completion: Arc<Completion>,
    live: bool,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.completion.mark(OBSERVER, self.live, false);
    }
}
struct AbortQueued(tokio::task::AbortHandle);
impl Drop for AbortQueued {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Command<F> {
    guard: Option<OwnedMutexGuard<QueueStore>>,
    operation: Option<F>,
    admission: Option<Arc<ReleaseLedger>>,
    permit: Option<OwnedSemaphorePermit>,
    witness: JobWitness,
}
impl<F> Command<F> {
    fn execute<T>(mut self) -> Output<T>
    where
        F: FnOnce(&mut QueueStore) -> Result<T, WorkerError>,
    {
        self.witness.started = true;
        #[cfg(any(test, feature = "queue-fault-tests"))]
        let call = self
            .witness
            .completion
            .owner
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        #[cfg(any(test, feature = "queue-fault-tests"))]
        self.witness.completion.owner.checkpoint(call, false);
        let admission = self
            .admission
            .as_ref()
            .map_or(Ok(()), |admission| admission.ensure_open());
        let (result, error) = match admission {
            // The owner itself proves the operation has never been invoked.
            // A missing observer for this specific refusal cannot hide a write.
            Err(WorkerError::Stopped) => (Err(WorkerError::Stopped), false),
            Err(error) => (Err(error), true),
            Ok(()) => {
                let result = self.operation.take().expect("one disk operation")(
                    self.guard.as_mut().expect("owned disk guard"),
                );
                let error = result.is_err();
                (result, error)
            }
        };
        let output = Output {
            result: Some(result),
            _witness: OutputWitness {
                completion: self.witness.completion.clone(),
                error,
            },
        };
        self.witness.output_created = true;
        #[cfg(any(test, feature = "queue-fault-tests"))]
        self.witness.completion.owner.checkpoint(call, true);
        drop(self);
        output
    }
}
impl<F> Drop for Command<F> {
    fn drop(&mut self) {
        if self.witness.started && (!self.witness.output_created || std::thread::panicking()) {
            if let Some(queue) = self.guard.as_mut() {
                queue.require_reopen();
            }
            self.witness.completion.owner.state().unconfirmed = true;
        }
        drop(self.guard.take());
        drop(self.operation.take());
        drop(self.admission.take());
        drop(self.permit.take());
    }
}
struct JobWitness {
    completion: Arc<Completion>,
    started: bool,
    output_created: bool,
}
impl Drop for JobWitness {
    fn drop(&mut self) {
        if std::thread::panicking() || (self.started && !self.output_created) {
            self.completion.owner.state().unconfirmed = true;
        }
        self.completion.mark(
            JOB | if self.output_created { 0 } else { OUTPUT },
            false,
            false,
        );
    }
}
struct Output<T> {
    result: Option<Result<T, WorkerError>>,
    _witness: OutputWitness,
}
impl<T> Drop for Output<T> {
    fn drop(&mut self) {
        // An unconsumed BeginAck drops its exact AttemptReservation here,
        // registering release before OUTPUT can make a drain complete.
        drop(self.result.take());
    }
}
struct OutputWitness {
    completion: Arc<Completion>,
    error: bool,
}
impl Drop for OutputWitness {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.completion.owner.state().unconfirmed = true;
        }
        self.completion.mark(OUTPUT, false, self.error);
    }
}
type Locking = Pin<Box<dyn Future<Output = OwnedMutexGuard<QueueStore>> + Send>>;
type Running<T> = Pin<Box<dyn Future<Output = Result<Output<T>, tokio::task::JoinError>> + Send>>;
struct Request<F, T> {
    running: Option<Running<T>>,
    locking: Option<Locking>,
    guard: Option<OwnedMutexGuard<QueueStore>>,
    operation: Option<F>,
    admission: Option<Arc<ReleaseLedger>>,
    observer: Option<Observer>,
    witness: Option<JobWitness>,
}
impl<F, T> Drop for Request<F, T> {
    fn drop(&mut self) {
        drop(self.running.take());
        drop(self.locking.take());
        drop(self.guard.take());
        drop(self.operation.take());
        drop(self.admission.take());
        drop(self.observer.take());
        // The final preparation witness also runs if an input destructor
        // unwinds. Submitted commands own that witness instead.
        drop(self.witness.take());
    }
}

#[cfg(any(test, feature = "queue-fault-tests"))]
type TestHook = (usize, bool, Box<dyn FnOnce() + Send>);
#[cfg(test)]
#[path = "queue_io_tests.rs"]
mod tests;
