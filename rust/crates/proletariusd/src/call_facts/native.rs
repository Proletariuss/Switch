//! A bounded, ordered native call lane. Admission does no archive I/O; the
//! retained head is the exact observation retried until durable confirmation.
use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Condvar,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::sync::Notify;

mod history;
pub(crate) use history::NativeHistory;

#[derive(Clone, Debug)]
pub(crate) enum NativeFactAction {
    Begin {
        incoming: bool,
        number: String,
    },
    Number(String),
    Answered,
    End,
    Snapshot {
        state: String,
        number: String,
    },
    Retire,
    Ims {
        incoming: bool,
        number: String,
        call_id: String,
        state: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct NativeFact {
    pub(crate) owner_id: String,
    pub(crate) owner_epoch: u64,
    pub(crate) at: OffsetDateTime,
    pub(crate) action: NativeFactAction,
}

impl NativeFact {
    fn observation(
        &self,
        source: Source,
        producer: &str,
        sequence: u64,
    ) -> Result<Observation, String> {
        let (action, direction, number, state, call_id) = match &self.action {
            NativeFactAction::Begin { incoming, number } => (
                Action::Begin,
                Some(if *incoming {
                    Direction::Incoming
                } else {
                    Direction::Outgoing
                }),
                Some(number.clone()),
                None,
                None,
            ),
            NativeFactAction::Number(number) => {
                (Action::Number, None, Some(number.clone()), None, None)
            }
            NativeFactAction::Answered => (Action::Answered, None, None, None, None),
            NativeFactAction::End => (Action::End, None, None, None, None),
            NativeFactAction::Retire => (Action::Retire, None, None, None, None),
            NativeFactAction::Snapshot { state, number } => (
                Action::Snapshot,
                None,
                Some(number.clone()),
                Some(state.clone()),
                None,
            ),
            NativeFactAction::Ims {
                incoming,
                number,
                call_id,
                state,
            } => (
                Action::Ims,
                Some(if *incoming {
                    Direction::Incoming
                } else {
                    Direction::Outgoing
                }),
                Some(number.clone()),
                Some(state.clone()),
                Some(call_id.clone()),
            ),
        };
        let observation = Observation {
            producer_id: producer.into(),
            source,
            sequence,
            owner_id: self.owner_id.clone(),
            owner_epoch: self.owner_epoch,
            observed_at: self
                .at
                .format(&Rfc3339)
                .map_err(|error| error.to_string())?,
            action,
            direction,
            number,
            state,
            call_id,
        };
        observation.validate().map_err(|error| error.message)?;
        Ok(observation)
    }
}

struct Queue {
    facts: VecDeque<Observation>,
    sequence: u64,
    owner: (String, u64),
    closed: bool,
    admission_failed: bool,
    finished: bool,
}
struct Lane {
    facts: CallFacts,
    source: Source,
    producer: String,
    capacity: usize,
    queue: Mutex<Queue>,
    changed: Condvar,
    finished: Notify,
    #[cfg(test)]
    before_worker_exit: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
struct Owner {
    lane: Arc<Lane>,
    worker: Arc<WorkerJoin>,
}
enum JoinState {
    Pending(thread::JoinHandle<()>),
    Joining,
    Joined(Result<(), String>),
}
struct WorkerJoin {
    state: Mutex<JoinState>,
    completed: Notify,
    retired: AtomicBool,
}
impl Drop for Owner {
    fn drop(&mut self) {
        // A forgotten lifecycle drain still retains and processes admitted
        // facts. Drop is not an observed terminal, so it cannot invent one.
        if let Ok(mut queue) = self.lane.queue.lock() {
            queue.closed = true;
            self.lane.changed.notify_one();
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeSink(Arc<Owner>);

impl NativeSink {
    pub(crate) fn start(
        facts: CallFacts,
        producer_id: String,
        capacity: usize,
    ) -> Result<Self, String> {
        Self::start_source(facts, Source::ModuleCs, producer_id, capacity)
    }

    pub(crate) fn start_reader_ims(
        facts: CallFacts,
        producer_id: String,
        capacity: usize,
    ) -> Result<Self, String> {
        Self::start_source(facts, Source::ReaderIms, producer_id, capacity)
    }

    pub(crate) fn start_module_ims(
        facts: CallFacts,
        producer_id: String,
        capacity: usize,
    ) -> Result<Self, String> {
        Self::start_source(facts, Source::ModuleIms, producer_id, capacity)
    }

    fn start_source(
        facts: CallFacts,
        source: Source,
        producer_id: String,
        capacity: usize,
    ) -> Result<Self, String> {
        if capacity == 0 {
            return Err("native call queue capacity must be positive".into());
        }
        facts.start_native_source(source, &producer_id)?;
        let lane = Arc::new(Lane {
            facts,
            source,
            producer: producer_id,
            capacity,
            queue: Mutex::new(Queue {
                facts: VecDeque::new(),
                sequence: 0,
                owner: (source.name().into(), 1),
                closed: false,
                admission_failed: false,
                finished: false,
            }),
            changed: Condvar::new(),
            finished: Notify::new(),
            #[cfg(test)]
            before_worker_exit: Mutex::new(None),
        });
        let worker_lane = lane.clone();
        let worker = match thread::Builder::new()
            .name("native-call-facts".into())
            .spawn(move || {
                run(worker_lane.clone());
                #[cfg(test)]
                {
                    let hook = worker_lane.before_worker_exit.lock().unwrap().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
            }) {
            Ok(worker) => worker,
            Err(error) => {
                lane.facts
                    .release_native_source(lane.source, &lane.producer);
                return Err(error.to_string());
            }
        };
        Ok(Self(Arc::new(Owner {
            lane,
            worker: Arc::new(WorkerJoin {
                state: Mutex::new(JoinState::Pending(worker)),
                completed: Notify::new(),
                retired: AtomicBool::new(false),
            }),
        })))
    }

    pub(crate) fn try_push(&self, fact: NativeFact) -> Result<(), String> {
        self.try_push_batch(vec![fact])
    }

    pub(crate) fn try_push_batch(&self, facts: Vec<NativeFact>) -> Result<(), String> {
        let lane = &self.0.lane;
        let mut queue = lane
            .queue
            .lock()
            .map_err(|_| "native call queue poisoned")?;
        let fail = |error: &str| {
            lane.facts
                .latch_error(lane.source, &lane.producer, error.into());
            error.to_owned()
        };
        if queue.closed || queue.admission_failed || queue.finished {
            return Err(fail(if lane.source == Source::ModuleCs {
                "模块通话历史观察已停止接收"
            } else {
                "读卡器通话历史观察已停止接收"
            }));
        }
        if facts.len() > lane.capacity.saturating_sub(queue.facts.len()) {
            queue.admission_failed = true;
            return Err(fail(if lane.source == Source::ModuleCs {
                "模块通话历史观察队列已满"
            } else {
                "读卡器通话历史观察队列已满"
            }));
        }
        let mut sequence = queue.sequence;
        let mut prepared = Vec::with_capacity(facts.len());
        for fact in &facts {
            sequence = sequence.checked_add(1).ok_or_else(|| {
                fail(if lane.source == Source::ModuleCs {
                    "模块通话事实序号已耗尽"
                } else {
                    "读卡器通话事实序号已耗尽"
                })
            })?;
            match fact.observation(lane.source, &lane.producer, sequence) {
                Ok(observation) => prepared.push(observation),
                Err(error) => {
                    queue.admission_failed = true;
                    return Err(fail(&error));
                }
            }
        }
        if let Some(last) = prepared.last() {
            queue.owner = (last.owner_id.clone(), last.owner_epoch);
        }
        queue.sequence = sequence;
        queue.facts.extend(prepared);
        lane.changed.notify_one();
        Ok(())
    }

    /// One reserved tail slot remains available after ordinary saturation.
    /// Closing never evicts or rewrites a queued terminal observation.
    pub(crate) fn retire(&self, at: OffsetDateTime) -> Result<(), String> {
        let lane = &self.0.lane;
        let mut queue = lane
            .queue
            .lock()
            .map_err(|_| "native call queue poisoned")?;
        if queue.closed {
            return Ok(());
        }
        queue.closed = true;
        // Even a rejected terminal must wake the real worker to finish its
        // already admitted queue; closing admission is not an exit proof.
        lane.changed.notify_one();
        let sequence = queue
            .sequence
            .checked_add(1)
            .ok_or("native call sequence exhausted")?;
        let observation = NativeFact {
            owner_id: queue.owner.0.clone(),
            owner_epoch: queue.owner.1,
            at,
            action: NativeFactAction::Retire,
        }
        .observation(lane.source, &lane.producer, sequence)?;
        queue.sequence = sequence;
        queue.facts.push_back(observation);
        lane.changed.notify_one();
        Ok(())
    }

    pub(crate) fn history_error(&self) -> Option<String> {
        self.0.lane.facts.native_history_error(self.0.lane.source)
    }

    /// Physical thread retirement is independent of archive delivery success.
    /// Neither a finished queue nor another waiter's cancelled drain proves it.
    pub(crate) fn worker_retired(&self) -> bool {
        self.0.worker.retired.load(Ordering::Acquire)
    }

    /// Cancellation of the waiter cannot cancel an archive write or lose its
    /// frozen head. The retained worker keeps retrying storage failures.
    pub(crate) async fn drain(&self) -> Result<(), String> {
        let completion = &self.0.worker;
        {
            let mut state = completion
                .state
                .lock()
                .map_err(|_| "native call worker poisoned")?;
            if matches!(*state, JoinState::Pending(_)) {
                let JoinState::Pending(worker) = std::mem::replace(&mut *state, JoinState::Joining)
                else {
                    unreachable!()
                };
                // Retain only join state, not Owner: if the last sink is dropped,
                // its Drop must still close admission and wake the real worker.
                let completion = completion.clone();
                tokio::task::spawn_blocking(move || {
                    let result = worker
                        .join()
                        .map_err(|_| "native call worker panicked".to_owned());
                    let mut state = completion.state.lock().unwrap_or_else(|e| e.into_inner());
                    *state = JoinState::Joined(result);
                    completion.retired.store(true, Ordering::Release);
                    drop(state);
                    completion.completed.notify_waiters();
                });
            }
        }
        loop {
            let notified = completion.completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let result = match &*completion
                .state
                .lock()
                .map_err(|_| "native call worker poisoned")?
            {
                JoinState::Joined(result) => Some(result.clone()),
                _ => None,
            };
            if let Some(result) = result {
                result?;
                break;
            }
            notified.await;
        }
        self.history_error().map_or(Ok(()), Err)
    }
}

fn run(lane: Arc<Lane>) {
    let mut retry = Duration::from_millis(100);
    loop {
        let head = {
            let mut queue = lane.queue.lock().unwrap_or_else(|error| error.into_inner());
            while queue.facts.is_empty() && !queue.closed {
                queue = lane
                    .changed
                    .wait(queue)
                    .unwrap_or_else(|error| error.into_inner());
            }
            match queue.facts.front().cloned() {
                Some(head) => head,
                None => {
                    lane.facts
                        .release_native_source(lane.source, &lane.producer);
                    queue.finished = true;
                    lane.finished.notify_waiters();
                    return;
                }
            }
        };
        match lane.facts.observe(head.clone(), false) {
            Ok(_) => {
                lane.facts
                    .native_delivery_error(lane.source, &lane.producer, None);
                let mut queue = lane.queue.lock().unwrap_or_else(|error| error.into_inner());
                debug_assert_eq!(queue.facts.front(), Some(&head));
                queue.facts.pop_front();
                retry = Duration::from_millis(100);
            }
            Err(error) => {
                if error.retryable {
                    lane.facts.native_delivery_error(
                        lane.source,
                        &lane.producer,
                        Some(error.message.clone()),
                    );
                } else {
                    lane.facts
                        .latch_error(lane.source, &lane.producer, error.message.clone());
                }
                let mut queue = lane.queue.lock().unwrap_or_else(|error| error.into_inner());
                if !error.retryable {
                    queue.admission_failed = true;
                    queue.finished = true;
                    lane.finished.notify_waiters();
                    return; // Head remains retained, never acknowledged or skipped.
                }
                // Admission notifications must not turn a failed fsync into a
                // tight loop. Wait against elapsed time, always retrying head.
                let until = std::time::Instant::now() + retry;
                while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
                    queue = lane
                        .changed
                        .wait_timeout(queue, left)
                        .unwrap_or_else(|error| error.into_inner())
                        .0;
                }
                retry = (retry * 2).min(Duration::from_secs(2));
            }
        }
    }
}

#[cfg(test)]
mod tests;
