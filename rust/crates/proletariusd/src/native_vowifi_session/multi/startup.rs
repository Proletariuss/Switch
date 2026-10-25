use super::*;

#[derive(Debug)]
pub(crate) struct StartFailure {
    pub(crate) code: String,
    pub(crate) cleanup_confirmed: bool,
    pub(crate) retry: RetryClass,
}
impl StartFailure {
    pub(super) async fn release(code: &str, card: SessionCard, retry: RetryClass) -> Self {
        let released = card.release_with_confirmation().await;
        Self {
            code: code.into(),
            cleanup_confirmed: released.is_ok(),
            retry,
        }
    }
}
#[derive(Default)]
pub(super) struct Starts {
    pub(super) stopping: bool,
    pub(super) tasks: Vec<Task>,
    pub(super) failure: Option<String>,
}
impl Starts {
    fn reap_finished(&mut self) {
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Poll, Waker},
        };
        // Join completed workers rather than accumulating detached retry attempts.
        let mut index = 0;
        while index < self.tasks.len() {
            if self.tasks[index].join.is_finished() {
                let mut task = self.tasks.swap_remove(index);
                match Pin::new(&mut task.join).poll(&mut Context::from_waker(Waker::noop())) {
                    Poll::Ready(Ok(Ok(()))) => {}
                    Poll::Ready(Ok(Err(error))) => {
                        self.failure.get_or_insert(error);
                    }
                    Poll::Ready(Err(error)) => {
                        self.failure.get_or_insert(error.to_string());
                    }
                    Poll::Pending => {
                        self.tasks.push(task);
                        index += 1;
                    }
                }
            } else {
                index += 1;
            }
        }
    }
}

pub(super) struct Task {
    pub(super) stop: watch::Sender<bool>,
    pub(super) join: tokio::task::JoinHandle<Result<(), String>>,
}
pub(crate) struct Startup {
    stop: watch::Sender<bool>,
    result: Option<oneshot::Receiver<Result<NativeMultiSession, StartFailure>>>,
    completed: bool,
}
impl Startup {
    pub(crate) fn cancel(&self) {
        self.stop.send_replace(true);
    }
    pub(crate) async fn wait(mut self) -> Result<NativeMultiSession, StartFailure> {
        let result = self.result.take().expect("one startup waiter").await;
        self.completed = true;
        result.unwrap_or_else(|_| {
            Err(StartFailure {
                code: "native multi startup worker lost".into(),
                cleanup_confirmed: false,
                retry: RetryClass::Permanent,
            })
        })
    }
}
impl Drop for Startup {
    fn drop(&mut self) {
        if !self.completed {
            self.cancel();
        }
    }
}

impl Runtime {
    pub(super) fn begin_start_scoped(
        self: &Arc<Self>,
        profile: Profile,
        request: PlanRequest,
        external: watch::Receiver<bool>,
        archive: NativeSmsArchive,
    ) -> Result<Startup, StartFailure> {
        let mut starts = self.starts.lock().unwrap();
        starts.reap_finished();
        if starts.stopping || starts.failure.is_some() || starts.tasks.len() >= 32 {
            return Err(StartFailure {
                code: "native multi owner unavailable".into(),
                cleanup_confirmed: true,
                retry: RetryClass::Stop,
            });
        }
        let (stop, cancelled) = watch::channel(false);
        let (result, received) = oneshot::channel();
        let runtime = self.clone();
        let join = tokio::spawn(async move {
            runtime
                .run_start(profile, request, external, cancelled, archive, result)
                .await
        });
        starts.tasks.push(Task {
            stop: stop.clone(),
            join,
        });
        Ok(Startup {
            stop,
            result: Some(received),
            completed: false,
        })
    }
    async fn run_start(
        self: Arc<Self>,
        profile: Profile,
        request: PlanRequest,
        mut external: watch::Receiver<bool>,
        mut cancelled: watch::Receiver<bool>,
        archive: NativeSmsArchive,
        result: oneshot::Sender<Result<NativeMultiSession, StartFailure>>,
    ) -> Result<(), String> {
        let (stop, signal) = watch::channel(*external.borrow() || *cancelled.borrow());
        let prepare = self.prepare(profile, request, signal, archive);
        tokio::pin!(prepare);
        let prepared = loop {
            tokio::select! {
                biased;
                changed = external.changed(), if !*stop.borrow() => {
                    if changed.is_err() || *external.borrow() { stop.send_replace(true); }
                },
                changed = cancelled.changed(), if !*stop.borrow() => {
                    if changed.is_err() || *cancelled.borrow() { stop.send_replace(true); }
                },
                value = &mut prepare => break value,
            }
        };
        let handle = match prepared {
            Ok(handle) => handle,
            Err(error) => {
                let cleanup = if error.cleanup_confirmed {
                    Ok(())
                } else {
                    Err("native multi preparation cleanup unconfirmed".into())
                };
                let _ = result.send(Err(error));
                return cleanup;
            }
        };
        let (exited, exit) = watch::channel(None);
        let cancelled_before_publish = *stop.borrow()
            || *external.borrow()
            || *cancelled.borrow()
            || external.has_changed().is_err()
            || result.is_closed();
        let unpublished = if cancelled_before_publish {
            handle.request_shutdown();
            Some(result)
        } else {
            let session = NativeMultiSession {
                handle: handle.clone(),
                exit,
            };
            if result.send(Ok(session)).is_err() {
                handle.request_shutdown();
            }
            None
        };
        // The runtime, not a manager/UI future, retains this exact generation's
        // join. Natural failures preserve their termination and retry facts.
        loop {
            if *stop.borrow() || *external.borrow() || *cancelled.borrow() {
                handle.request_shutdown();
            }
            let joined = {
                let mut state = self.owner.lock().unwrap();
                let owner = state
                    .as_mut()
                    .expect("owner retained until startup workers join");
                owner.reap_ordinary_tasks();
                let joined = !owner.ordinary_tasks.contains_key(&handle.generation);
                if joined {
                    owner
                        .ordinary_results
                        .retain(|(generation, _)| *generation != handle.generation);
                }
                joined
            };
            if joined {
                let snapshot = handle.snapshot();
                let cleanup_confirmed = snapshot.cleanup_confirmed && snapshot.lease_released;
                exited.send_replace(Some(SessionExit {
                    snapshot,
                    joined: true,
                    cleanup_confirmed,
                }));
                if let Some(result) = unpublished {
                    let _ = result.send(Err(StartFailure {
                        code: "native multi startup stopped".into(),
                        cleanup_confirmed,
                        retry: RetryClass::Stop,
                    }));
                }
                return if cleanup_confirmed {
                    Ok(())
                } else {
                    Err("native multi session cleanup unconfirmed".into())
                };
            }
            tokio::select! {
                changed=external.changed(), if !*stop.borrow()=>if changed.is_err() || *external.borrow(){stop.send_replace(true);},
                changed=cancelled.changed(), if !*stop.borrow()=>if changed.is_err() || *cancelled.borrow(){stop.send_replace(true);},
                _=tokio::time::sleep(Duration::from_millis(5))=>{},
            }
        }
    }
}

#[cfg(test)]
mod tests;
