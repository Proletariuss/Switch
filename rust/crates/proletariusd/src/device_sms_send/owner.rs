//! One bounded admission owns the complete device-SMS task tree, including
//! gaps with no archive job and outputs not yet consumed by their observer.
//! Closing admission never aborts accepted work or converts panic into release.
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::{sync::Notify, task::JoinHandle};

const LIMIT: usize = 16;
#[derive(Clone, Default)]
pub(crate) struct Owner(Arc<Inner>);
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    changed: Notify,
}
#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
    unconfirmed: bool,
}
#[derive(Clone)]
pub(super) struct Job(Arc<Admission>);
struct Admission {
    owner: Owner,
}

impl Owner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|error| {
            let mut state = error.into_inner();
            state.unconfirmed = true;
            state
        })
    }
    pub(super) fn admit(&self) -> Result<Job, &'static str> {
        let mut state = self.state();
        if state.closed {
            return Err("backend_stopping");
        }
        if state.unconfirmed {
            return Err("device_sms_owner_unconfirmed");
        }
        if state.active >= LIMIT {
            return Err("device_sms_owner_busy");
        }
        state.active += 1;
        Ok(Job(Arc::new(Admission {
            owner: self.clone(),
        })))
    }
    pub(crate) fn begin_shutdown(&self) {
        self.state().closed = true;
        self.0.changed.notify_waiters();
    }
    /// Cancellation leaves all registration and sticky uncertainty intact.
    /// Call only after close; zero queued archive jobs is never this receipt.
    pub(crate) async fn finish_pending(&self) -> Result<(), String> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state();
                if !state.closed {
                    return Err("device_sms_admission_not_closed".into());
                }
                if state.active == 0 {
                    return if state.unconfirmed {
                        Err("device_sms_owner_unconfirmed".into())
                    } else {
                        Ok(())
                    };
                }
            }
            changed.await;
        }
    }
    fn fail(&self) {
        self.state().unconfirmed = true;
        self.0.changed.notify_waiters();
    }
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> (usize, bool, bool) {
        let state = self.state();
        (state.active, state.closed, state.unconfirmed)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        let mut state = self.owner.state();
        if std::thread::panicking() {
            state.unconfirmed = true;
        }
        match state.active.checked_sub(1) {
            Some(active) => state.active = active,
            None => state.unconfirmed = true,
        }
        drop(state);
        self.owner.0.changed.notify_waiters();
    }
}

struct TaskGuard {
    job: Job,
    complete: bool,
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.job.0.owner.fail();
        }
    }
}
// Named envelopes impose resource-before-ticket destruction, including a task
// aborted before its first poll and a future/output destructor that unwinds.
struct Task<F> {
    work: Option<Pin<Box<F>>>,
    guard: TaskGuard,
}
impl<F> Drop for Task<F> {
    fn drop(&mut self) {
        drop(self.work.take());
    }
}
pub(super) struct Output<T> {
    value: Option<T>,
    job: Job,
}
impl<T> Output<T> {
    // Production call sites transfer only JSON, Binding and unit DTOs. A
    // capability-bearing payload must retain this envelope instead of using
    // this transfer boundary. No current caller transfers a physical guard.
    pub(super) fn into_inner(mut self) -> T {
        self.value.take().expect("owned SMS output")
    }
}
impl<T> Drop for Output<T> {
    fn drop(&mut self) {
        let mut guard = TaskGuard {
            job: self.job.clone(),
            complete: false,
        };
        drop(self.value.take());
        guard.complete = true;
    }
}
impl Job {
    pub(super) fn spawn<F, T>(&self, work: F) -> JoinHandle<Output<T>>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let mut task = Task {
            work: Some(Box::pin(work)),
            guard: TaskGuard {
                job: self.clone(),
                complete: false,
            },
        };
        tokio::spawn(async move {
            let value = task.work.as_mut().expect("owned SMS future").as_mut().await;
            drop(task.work.take());
            let output = Output {
                value: Some(value),
                job: task.guard.job.clone(),
            };
            task.guard.complete = true;
            drop(task);
            output
        })
    }
}

#[cfg(test)]
mod tests;
