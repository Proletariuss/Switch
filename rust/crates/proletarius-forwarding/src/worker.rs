//! Durable provider execution over the Go-compatible forwarding queue.
//!
//! The worker keeps the queue lock only for short snapshots and durable state
//! changes. Provider I/O runs without the queue mutex. Every request is
//! preceded by a persisted unknown-result marker; ambiguous transport outcomes
//! therefore remain paused instead of being replayed after cancellation or a
//! process crash.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use tokio::sync::{Mutex, Notify, watch};

use crate::queue_store::AttemptToken;

use crate::{
    BarkSender, DeliveryDisposition, DeliveryFailure, DeliveryOutcome, DestinationBinding,
    ForwardedSms, ForwardingChannel, ForwardingDelivery, QueueMutationError, QueueStore,
    StateStoreError, TelegramSender, telegram_chunks,
};

mod queue_io;

const RETRY_TICK: Duration = Duration::from_secs(5);
const PRE_SUBMIT_CONNECT_RETRY_LIMIT: i64 = 5;
const MAX_PENDING_RELEASES: usize = 64;

pub trait ForwardingClock: Send + Sync + 'static {
    fn now_rfc3339(&self) -> Result<String, WorkerError>;
    fn local_sms_timestamp(&self, timestamp: &str) -> Result<Option<String>, WorkerError>;
    fn retry_rfc3339(&self, delay: Duration) -> Result<String, WorkerError>;
}

/// Uses the host's actual timezone rules for both historic SMS timestamps and
/// retry boundaries, matching Go's `time.Time.Local()` behavior.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemForwardingClock;

impl ForwardingClock for SystemForwardingClock {
    fn now_rfc3339(&self) -> Result<String, WorkerError> {
        let now = OffsetDateTime::now_utc();
        let offset = UtcOffset::local_offset_at(now).map_err(|_| WorkerError::ClockUnavailable)?;
        now.to_offset(offset)
            .format(&Rfc3339)
            .map_err(|_| WorkerError::ClockUnavailable)
    }

    fn local_sms_timestamp(&self, timestamp: &str) -> Result<Option<String>, WorkerError> {
        if timestamp == "0001-01-01T00:00:00Z" {
            return Ok(None);
        }
        let parsed = OffsetDateTime::parse(timestamp, &Rfc3339)
            .map_err(|_| WorkerError::ClockUnavailable)?;
        let offset =
            UtcOffset::local_offset_at(parsed).map_err(|_| WorkerError::ClockUnavailable)?;
        let local = parsed.to_offset(offset);
        Ok(Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            local.year(),
            u8::from(local.month()),
            local.day(),
            local.hour(),
            local.minute(),
            local.second(),
        )))
    }

    fn retry_rfc3339(&self, delay: Duration) -> Result<String, WorkerError> {
        let delta = time::Duration::try_from(delay).map_err(|_| WorkerError::ClockUnavailable)?;
        let retry_at = OffsetDateTime::now_utc()
            .checked_add(delta)
            .ok_or(WorkerError::ClockUnavailable)?;
        let offset =
            UtcOffset::local_offset_at(retry_at).map_err(|_| WorkerError::ClockUnavailable)?;
        retry_at
            .to_offset(offset)
            .format(&Rfc3339)
            .map_err(|_| WorkerError::ClockUnavailable)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerSummary {
    pub attempted: usize,
    pub delivered: usize,
    pub failed: usize,
    pub uncertain: usize,
    pub skipped: usize,
    pub pending: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerError {
    InvalidConfiguration,
    ClockUnavailable,
    Stopped,
    ReleaseUnconfirmed,
    QueueIoBusy,
    QueueIoUnconfirmed,
    Queue(QueueMutationError),
    StateStore(StateStoreError),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "forwarding_worker_configuration_invalid",
            Self::ClockUnavailable => "forwarding_clock_unavailable",
            Self::Stopped => "forwarding_worker_stopped",
            Self::ReleaseUnconfirmed => "forwarding_release_unconfirmed",
            Self::QueueIoBusy => "forwarding_queue_io_busy",
            Self::QueueIoUnconfirmed => "forwarding_queue_io_unconfirmed",
            Self::Queue(error) => return error.fmt(f),
            Self::StateStore(error) => return error.fmt(f),
        })
    }
}

impl std::error::Error for WorkerError {}

impl From<QueueMutationError> for WorkerError {
    fn from(value: QueueMutationError) -> Self {
        Self::Queue(value)
    }
}

impl From<StateStoreError> for WorkerError {
    fn from(value: StateStoreError) -> Self {
        Self::StateStore(value)
    }
}

pub struct ForwardingWorker<C: ForwardingClock = SystemForwardingClock> {
    queue: Arc<Mutex<QueueStore>>,
    telegram: Option<TelegramSender>,
    telegram_binding: Option<DestinationBinding>,
    bark: Option<BarkSender>,
    bark_binding: Option<DestinationBinding>,
    clock: C,
    flush_gate: Mutex<()>,
    wake: Notify,
    releases: Arc<ReleaseLedger>,
    disk: Arc<queue_io::Owner>,
    #[cfg(test)]
    unknown_release_pause: StdMutex<Option<Arc<ReleaseBarrier>>>,
}

impl<C: ForwardingClock> ForwardingWorker<C> {
    pub fn new(
        queue: QueueStore,
        telegram: Option<TelegramSender>,
        telegram_binding: Option<DestinationBinding>,
        bark: Option<BarkSender>,
        bark_binding: Option<DestinationBinding>,
        clock: C,
    ) -> Result<Self, WorkerError> {
        if telegram.is_some() != telegram_binding.is_some()
            || bark.is_some() != bark_binding.is_some()
        {
            return Err(WorkerError::InvalidConfiguration);
        }
        Ok(Self {
            queue: Arc::new(Mutex::new(queue)),
            telegram,
            telegram_binding,
            bark,
            bark_binding,
            clock,
            flush_gate: Mutex::new(()),
            wake: Notify::new(),
            releases: Arc::new(ReleaseLedger::default()),
            disk: Arc::new(queue_io::Owner::default()),
            #[cfg(test)]
            unknown_release_pause: StdMutex::new(None),
        })
    }

    /// Shared queue handle for the single configured owner to admit SMS or
    /// perform explicit user-approved queue actions.
    pub fn queue(&self) -> &Arc<Mutex<QueueStore>> {
        &self.queue
    }

    /// Wakes the periodic runner after a durable queue admission.
    pub fn notify_new_work(&self) {
        self.wake.notify_one();
    }

    /// Consume one actual Notify permit in explicit offline daemon tests only.
    #[cfg(feature = "queue-fault-tests")]
    #[doc(hidden)]
    pub fn take_new_work_notification_for_test(&self) -> bool {
        let mut notified = std::pin::pin!(self.wake.notified());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        std::future::Future::poll(notified.as_mut(), &mut context).is_ready()
    }

    /// Installs a deterministic disk-owner checkpoint for offline daemon
    /// integration tests. The normal dependency does not enable this feature.
    #[cfg(feature = "queue-fault-tests")]
    #[doc(hidden)]
    pub fn test_disk_hook(&self, call: usize, after: bool, action: impl FnOnce() + Send + 'static) {
        self.disk.hook(call, after, action);
    }

    /// Returns the next disk-owner call index in offline fault fixtures only.
    #[cfg(feature = "queue-fault-tests")]
    #[doc(hidden)]
    pub fn test_disk_call_count(&self) -> usize {
        self.disk.test_call_count()
    }

    /// Returns `(registered commands, sticky uncertainty)` after all three
    /// completion facts have settled in offline fault fixtures only.
    #[cfg(feature = "queue-fault-tests")]
    #[doc(hidden)]
    pub fn test_disk_snapshot(&self) -> (usize, bool) {
        self.disk.test_snapshot()
    }

    /// Sends the fixed, non-sensitive provider test notification without
    /// touching the durable queue.
    pub async fn test_provider(&self, provider: &str) -> Result<(), DeliveryFailure> {
        let message = ForwardedSms {
            sender: "Switch",
            device: "测试",
            timestamp_local: None,
            content: "短信转发测试通知",
        };
        match provider.trim().to_ascii_lowercase().as_str() {
            "telegram" => {
                let sender = self.telegram.as_ref().ok_or_else(|| DeliveryFailure {
                    code: "telegram_not_enabled".to_owned(),
                    disposition: DeliveryDisposition::Permanent,
                    retry_after: None,
                })?;
                for chunk in telegram_chunks(&message) {
                    sender.send_chunk(&chunk).await?;
                }
                Ok(())
            }
            "bark" => {
                self.bark
                    .as_ref()
                    .ok_or_else(|| DeliveryFailure {
                        code: "bark_not_enabled".to_owned(),
                        disposition: DeliveryDisposition::Permanent,
                        retry_after: None,
                    })?
                    .send(&message)
                    .await
            }
            _ => Err(DeliveryFailure {
                code: "unknown_provider".to_owned(),
                disposition: DeliveryDisposition::Permanent,
                retry_after: None,
            }),
        }
    }

    /// Performs one ordered flush. The queue mutex is never held while waiting
    /// on a provider, allowing enqueue/status/actions to keep progressing.
    pub async fn flush_pending(&self) -> Result<WorkerSummary, WorkerError> {
        let _flush = self.flush_gate.lock().await;
        self.releases.ensure_open()?;
        let now = self.clock.now_rfc3339()?;
        let telegram = self.telegram_binding.clone();
        let bark = self.bark_binding.clone();
        let candidates = self
            .disk
            .run(self.queue.clone(), move |queue| {
                Ok(queue.due_deliveries(&now, telegram.as_ref(), bark.as_ref())?)
            })
            .await?;
        let mut summary = WorkerSummary::default();
        for candidate in candidates {
            let result = match candidate.channel() {
                ForwardingChannel::Telegram => self.deliver_telegram(&candidate).await,
                ForwardingChannel::Bark => self.deliver_bark(&candidate).await,
            };
            match result {
                Ok(AttemptResult::Delivered) => {
                    summary.attempted += 1;
                    summary.delivered += 1;
                }
                Ok(AttemptResult::Failed) => {
                    summary.attempted += 1;
                    summary.failed += 1;
                }
                Ok(AttemptResult::Uncertain) => {
                    summary.attempted += 1;
                    summary.uncertain += 1;
                }
                Err(WorkerError::Queue(
                    QueueMutationError::DestinationAlreadyTerminal
                    | QueueMutationError::DestinationNotCurrent
                    | QueueMutationError::DestinationPaused,
                )) => summary.skipped += 1,
                Err(error) => return Err(error),
            }
        }
        summary.pending = self
            .disk
            .run(self.queue.clone(), |queue| Ok(queue.inspect()?.tasks.len()))
            .await?;
        Ok(summary)
    }

    /// Permanently closes new flush admission and observes every registered
    /// release. The configured owner must stop/join its runner first. Taking
    /// the flush gate also waits for any previously admitted library caller.
    /// Cancelling this observer leaves closure, pending work and errors intact.
    /// No queue guard is held while waiting for cleanup completion.
    pub async fn finish_releases(&self) -> Result<(), WorkerError> {
        self.close_admission();
        let _flush = self.flush_gate.lock().await;
        self.disk.close();
        // Orphan begin outputs can enqueue exact-token cleanup on destruction.
        // They must retire before observing the release ledger as empty.
        let disk = self.disk.finish().await;
        let releases = self.releases.finish().await;
        disk.and(releases)
    }

    /// Close new provider work synchronously while retaining accepted owners.
    pub fn close_admission(&self) {
        // Accepted attempts may still checkpoint/finish while the flush gate
        // is occupied. finish_releases closes disk admission after that gate
        // proves no live flush can produce another command.
        self.releases.close();
    }

    /// Provider runner status also verifies on-disk identity; keep that complete
    /// transaction off the async executor just like mutation checkpoints.
    pub async fn inspect_for_bindings(
        &self,
        telegram: Option<DestinationBinding>,
        bark: Option<DestinationBinding>,
    ) -> Result<crate::ForwardingQueueSummary, WorkerError> {
        self.disk
            .run(self.queue.clone(), move |queue| {
                Ok(queue.inspect_for_bindings(telegram.as_ref(), bark.as_ref())?)
            })
            .await
    }

    async fn begin_attempt(
        &self,
        candidate: &ForwardingDelivery,
        channel: ForwardingChannel,
        binding: &DestinationBinding,
        attempted_at: String,
    ) -> Result<AttemptReservation, WorkerError> {
        let task_id = candidate.task_id().to_owned();
        let binding = binding.clone();
        let queue_handle = self.queue.clone();
        let releases = self.releases.clone();
        self.disk
            .run_begin(self.queue.clone(), releases.clone(), move |queue| {
                let token = queue.begin_destination_attempt(
                    &task_id,
                    channel,
                    true,
                    &binding,
                    &attempted_at,
                )?;
                // The ACK owns cleanup before the blocking output can be lost.
                // There is no await/enrollment gap after the durable begin.
                Ok(AttemptReservation::new(
                    queue_handle,
                    &task_id,
                    channel,
                    token,
                    releases,
                ))
            })
            .await
    }

    async fn checkpoint(
        &self,
        candidate: &ForwardingDelivery,
        binding: &DestinationBinding,
        index: usize,
        submitting: bool,
        updated_at: String,
        token: &AttemptToken,
    ) -> Result<(), WorkerError> {
        let task_id = candidate.task_id().to_owned();
        let binding = binding.clone();
        let token = token.clone();
        self.disk
            .run(self.queue.clone(), move |queue| {
                Ok(queue.checkpoint_telegram_chunk(
                    &task_id,
                    true,
                    &binding,
                    index,
                    submitting,
                    &updated_at,
                    &token,
                )?)
            })
            .await
    }

    async fn finish_attempt(
        &self,
        candidate: &ForwardingDelivery,
        channel: ForwardingChannel,
        binding: &DestinationBinding,
        updated_at: String,
        outcome: DeliveryOutcome,
        token: &AttemptToken,
    ) -> Result<(), WorkerError> {
        let task_id = candidate.task_id().to_owned();
        let binding = binding.clone();
        let token = token.clone();
        self.disk
            .run(self.queue.clone(), move |queue| {
                Ok(queue.finish_destination_attempt(
                    &task_id,
                    channel,
                    &binding,
                    &updated_at,
                    &outcome,
                    &token,
                )?)
            })
            .await
    }

    /// Runs until the watch channel is closed or set to `true`. Cancelling an
    /// in-flight HTTP request drops its volatile reservation but leaves the
    /// durable unknown-result marker intact.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        loop {
            // A newly subscribed receiver considers the current value seen;
            // changed() alone does not observe an already-true shutdown.
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                result = self.flush_pending() => { result?; }
            }
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                _ = tokio::time::sleep(RETRY_TICK) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    async fn deliver_telegram(
        &self,
        candidate: &ForwardingDelivery,
    ) -> Result<AttemptResult, WorkerError> {
        self.releases.ensure_open()?;
        let sender = self
            .telegram
            .as_ref()
            .ok_or(WorkerError::InvalidConfiguration)?;
        let binding = self
            .telegram_binding
            .as_ref()
            .ok_or(WorkerError::InvalidConfiguration)?;
        let message = candidate.message();
        let chunks = if let Some(frozen) = candidate.telegram_chunks() {
            frozen.to_vec()
        } else {
            let timestamp_local = self.clock.local_sms_timestamp(message.timestamp())?;
            let chunks = telegram_chunks(&ForwardedSms {
                sender: message.sender(),
                device: message.device(),
                timestamp_local: timestamp_local.as_deref(),
                content: message.content(),
            });
            let task_id = candidate.task_id().to_owned();
            let binding = binding.clone();
            self.disk
                .run(self.queue.clone(), move |queue| {
                    queue.freeze_telegram_progress(&task_id, &binding, &chunks)?;
                    Ok(chunks)
                })
                .await?
        };
        let confirmed = candidate.confirmed_chunks();
        let attempted_at = self.clock.now_rfc3339()?;
        let reservation = self
            .begin_attempt(
                candidate,
                ForwardingChannel::Telegram,
                binding,
                attempted_at,
            )
            .await?;
        let token = &reservation.token;

        for (index, chunk) in chunks.iter().enumerate().skip(confirmed) {
            let now = self.clock.now_rfc3339()?;
            self.checkpoint(candidate, binding, index, true, now, token)
                .await?;
            if let Err(failure) = sender.send_chunk(chunk).await {
                return self
                    .finish_failure(
                        candidate,
                        ForwardingChannel::Telegram,
                        binding,
                        failure,
                        token,
                    )
                    .await;
            }
            let now = self.clock.now_rfc3339()?;
            self.checkpoint(candidate, binding, index + 1, false, now, token)
                .await?;
        }

        let now = self.clock.now_rfc3339()?;
        self.finish_attempt(
            candidate,
            ForwardingChannel::Telegram,
            binding,
            now,
            DeliveryOutcome::Delivered,
            token,
        )
        .await?;
        Ok(AttemptResult::Delivered)
    }

    async fn deliver_bark(
        &self,
        candidate: &ForwardingDelivery,
    ) -> Result<AttemptResult, WorkerError> {
        self.releases.ensure_open()?;
        let sender = self
            .bark
            .as_ref()
            .ok_or(WorkerError::InvalidConfiguration)?;
        let binding = self
            .bark_binding
            .as_ref()
            .ok_or(WorkerError::InvalidConfiguration)?;
        let message = candidate.message();
        let timestamp_local = self.clock.local_sms_timestamp(message.timestamp())?;
        let sms = ForwardedSms {
            sender: message.sender(),
            device: message.device(),
            timestamp_local: timestamp_local.as_deref(),
            content: message.content(),
        };
        let attempted_at = self.clock.now_rfc3339()?;
        let reservation = self
            .begin_attempt(candidate, ForwardingChannel::Bark, binding, attempted_at)
            .await?;
        let token = &reservation.token;
        if let Err(failure) = sender.send(&sms).await {
            return self
                .finish_failure(candidate, ForwardingChannel::Bark, binding, failure, token)
                .await;
        }
        let now = self.clock.now_rfc3339()?;
        self.finish_attempt(
            candidate,
            ForwardingChannel::Bark,
            binding,
            now,
            DeliveryOutcome::Delivered,
            token,
        )
        .await?;
        Ok(AttemptResult::Delivered)
    }

    async fn finish_failure(
        &self,
        candidate: &ForwardingDelivery,
        channel: ForwardingChannel,
        binding: &DestinationBinding,
        failure: DeliveryFailure,
        token: &AttemptToken,
    ) -> Result<AttemptResult, WorkerError> {
        if failure.disposition == DeliveryDisposition::Unknown {
            // The pre-submit checkpoint is already durable. Releasing only the
            // in-memory reservation preserves the explicit-resolution gate.
            self.queue
                .lock()
                .await
                .release_destination(candidate.task_id(), channel, token);
            #[cfg(test)]
            {
                let pause = self.unknown_release_pause.lock().unwrap().take();
                if let Some(pause) = pause {
                    pause.wait().await;
                }
            }
            return Ok(AttemptResult::Uncertain);
        }
        let attempts = candidate.attempts().saturating_add(1).max(1);
        let connect_retries_exhausted = pre_submit_connect_retry_exhausted(&failure.code, attempts);
        let permanent =
            failure.disposition == DeliveryDisposition::Permanent || connect_retries_exhausted;
        let retry_at = if permanent {
            None
        } else {
            let delay = failure
                .retry_after
                .filter(|value| !value.is_zero())
                .unwrap_or_else(|| retry_backoff(attempts));
            Some(self.clock.retry_rfc3339(delay)?)
        };
        let updated_at = self.clock.now_rfc3339()?;
        let outcome = DeliveryOutcome::Failed {
            error_code: safe_failure_code(if connect_retries_exhausted {
                "delivery_connect_retry_limit_reached"
            } else {
                &failure.code
            }),
            permanent,
            result_unknown: false,
            retry_at,
        };
        self.finish_attempt(candidate, channel, binding, updated_at, outcome, token)
            .await?;
        Ok(AttemptResult::Failed)
    }
}

impl ForwardingWorker<SystemForwardingClock> {
    pub fn system_clock(
        queue: QueueStore,
        telegram: Option<TelegramSender>,
        telegram_binding: Option<DestinationBinding>,
        bark: Option<BarkSender>,
        bark_binding: Option<DestinationBinding>,
    ) -> Result<Self, WorkerError> {
        Self::new(
            queue,
            telegram,
            telegram_binding,
            bark,
            bark_binding,
            SystemForwardingClock,
        )
    }
}

#[derive(Clone, Copy)]
enum AttemptResult {
    Delivered,
    Failed,
    Uncertain,
}

// Only release jobs that can outlive their flush enter this ledger. Registration
// is synchronous before spawn, so joining the producer also observes registration.
#[derive(Default)]
struct ReleaseLedger {
    state: StdMutex<ReleaseState>,
    changed: Notify,
    #[cfg(test)]
    pause: StdMutex<Option<Arc<ReleaseBarrier>>>,
}

#[derive(Default)]
struct ReleaseState {
    closed: bool,
    pending: usize,
    failed: bool,
}

impl ReleaseLedger {
    fn state(&self) -> std::sync::MutexGuard<'_, ReleaseState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.failed = true;
                state
            }
        }
    }

    fn ensure_open(&self) -> Result<(), WorkerError> {
        let state = self.state();
        if state.closed {
            Err(WorkerError::Stopped)
        } else if state.failed {
            Err(WorkerError::ReleaseUnconfirmed)
        } else {
            Ok(())
        }
    }

    fn close(&self) {
        self.state().closed = true;
    }

    fn register(self: &Arc<Self>) -> Option<ReleaseCompletion> {
        let mut state = self.state();
        // Closed admission does not suppress cleanup for an accepted owner.
        // Saturation leaves that owner in place and prevents any later attempt.
        if state.pending >= MAX_PENDING_RELEASES {
            state.failed = true;
            return None;
        }
        state.pending += 1;
        Some(ReleaseCompletion {
            ledger: self.clone(),
            completed: false,
        })
    }

    async fn finish(&self) -> Result<(), WorkerError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state();
                if state.pending == 0 {
                    return if state.failed {
                        Err(WorkerError::ReleaseUnconfirmed)
                    } else {
                        Ok(())
                    };
                }
            }
            changed.await;
        }
    }
}

struct ReleaseCompletion {
    ledger: Arc<ReleaseLedger>,
    completed: bool,
}

impl ReleaseCompletion {
    fn complete(&mut self, success: bool) {
        if self.completed {
            return;
        }
        {
            let mut state = self.ledger.state();
            state.failed |= !success;
            if let Some(pending) = state.pending.checked_sub(1) {
                state.pending = pending;
            } else {
                state.failed = true;
            }
        }
        self.completed = true;
        self.ledger.changed.notify_waiters();
    }
}

impl Drop for ReleaseCompletion {
    fn drop(&mut self) {
        self.complete(false);
    }
}

// Own the queue capture explicitly. On success, panic or cancellation its Arc
// is dropped BEFORE publishing completion, including a never-polled spawned job.
struct ReleaseJob {
    queue: Option<Arc<Mutex<QueueStore>>>,
    task_id: String,
    channel: ForwardingChannel,
    token: AttemptToken,
    completion: ReleaseCompletion,
}

impl ReleaseJob {
    async fn run(mut self) {
        #[cfg(test)]
        {
            let pause = self.completion.ledger.pause.lock().unwrap().take();
            if let Some(pause) = pause {
                pause.wait().await;
            }
        }
        {
            let mut queue = self
                .queue
                .as_ref()
                .expect("release owns queue")
                .lock()
                .await;
            queue.release_destination(&self.task_id, self.channel, &self.token);
        }
        drop(self.queue.take());
        self.completion.complete(true);
    }
}

impl Drop for ReleaseJob {
    fn drop(&mut self) {
        drop(self.queue.take());
        self.completion.complete(false);
    }
}

struct AttemptReservation {
    queue: Arc<Mutex<QueueStore>>,
    task_id: String,
    channel: ForwardingChannel,
    token: AttemptToken,
    releases: Arc<ReleaseLedger>,
}

impl AttemptReservation {
    fn new(
        queue: Arc<Mutex<QueueStore>>,
        task_id: &str,
        channel: ForwardingChannel,
        token: AttemptToken,
        releases: Arc<ReleaseLedger>,
    ) -> Self {
        Self {
            queue,
            task_id: task_id.to_owned(),
            channel,
            token,
            releases,
        }
    }
}

impl Drop for AttemptReservation {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.queue.try_lock() {
            queue.release_destination(&self.task_id, self.channel, &self.token);
            return;
        }
        let Some(completion) = self.releases.register() else {
            return;
        };
        let job = ReleaseJob {
            queue: Some(self.queue.clone()),
            task_id: self.task_id.clone(),
            channel: self.channel,
            token: self.token.clone(),
            completion,
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(job.run());
        }
        // Without a runtime the owned job drops here and records a sticky
        // unconfirmed result; it never reports successful release.
    }
}

#[cfg(test)]
#[derive(Default)]
struct ReleaseBarrier {
    entered: Notify,
    proceed: Notify,
}

#[cfg(test)]
impl ReleaseBarrier {
    async fn wait(&self) {
        self.entered.notify_one();
        self.proceed.notified().await;
    }
}

fn retry_backoff(attempt: i64) -> Duration {
    let exponent = attempt.saturating_sub(1).clamp(0, 10) as u32;
    Duration::from_secs((5_u64.saturating_mul(1_u64 << exponent)).min(3_600))
}

fn pre_submit_connect_retry_exhausted(code: &str, attempts: i64) -> bool {
    code.ends_with("_connect_failed") && attempts >= PRE_SUBMIT_CONNECT_RETRY_LIMIT
}

fn safe_failure_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 96
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        code.to_owned()
    } else {
        "delivery_failed".to_owned()
    }
}

impl<C: ForwardingClock> std::fmt::Debug for ForwardingWorker<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForwardingWorker([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use reqwest::Url;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::{EnqueueOptions, ForwardingMessage, ProxyMode, StateKey, StateLimits};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn pre_submit_connect_errors_stop_after_a_bounded_retry_budget() {
        assert!(!pre_submit_connect_retry_exhausted(
            "telegram_connect_failed",
            4
        ));
        assert!(pre_submit_connect_retry_exhausted(
            "telegram_connect_failed",
            5
        ));
        assert!(pre_submit_connect_retry_exhausted("bark_connect_failed", 8));
        assert!(!pre_submit_connect_retry_exhausted(
            "telegram_http_429",
            100
        ));
        assert!(!pre_submit_connect_retry_exhausted(
            "telegram_network_error",
            100
        ));
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .expect("Switch root from manifest")
                .join("build/tmp/forwarding-worker-tests");
            fs::create_dir_all(&root).unwrap();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for _ in 0..64 {
                let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = root.join(format!("{}-{nanos}-{sequence}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create worker test root: {error}"),
                }
            }
            panic!("could not create worker test root")
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn key() -> StateKey {
        StateKey::from_slice(&(0..32).map(|value| value as u8).collect::<Vec<_>>()).unwrap()
    }

    async fn loopback_response(response: &'static [u8]) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).await;
            stream.write_all(response).await.unwrap();
        });
        Url::parse(&format!("http://{address}")).unwrap()
    }

    async fn loopback_hanging_request() -> (Url, Arc<Notify>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let received = Arc::new(Notify::new());
        let observed = received.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut block = [0_u8; 2048];
                let count = stream.read(&mut block).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&block[..count]);
                assert!(request.len() <= 16 * 1024);
                let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            observed.notify_one();
            std::future::pending::<()>().await;
        });
        (
            Url::parse(&format!("http://{address}")).unwrap(),
            received,
            server,
        )
    }

    async fn loopback_gated_response() -> (
        Url,
        Arc<Notify>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<bool>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let received = Arc::new(Notify::new());
        let observed = received.clone();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut block = [0_u8; 2048];
                let count = stream.read(&mut block).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&block[..count]);
                assert!(request.len() <= 16 * 1024);
                let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            observed.notify_one();
            let _ = released.await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 23\r\nConnection: close\r\n\r\n{\"ok\":true,\"result\":{}}",
                )
                .await
                .unwrap();
            drop(stream);

            // A second accepted connection proves that two workers submitted
            // the same task. The queue should have archived the first result
            // before the serialized second flush can select due work.
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_ok()
        });
        (
            Url::parse(&format!("http://{address}")).unwrap(),
            received,
            release,
            server,
        )
    }

    fn fixed_clock() -> impl ForwardingClock {
        struct FixedClock;
        impl ForwardingClock for FixedClock {
            fn now_rfc3339(&self) -> Result<String, WorkerError> {
                Ok("2026-01-02T03:04:08Z".to_owned())
            }

            fn local_sms_timestamp(&self, timestamp: &str) -> Result<Option<String>, WorkerError> {
                if timestamp == "0001-01-01T00:00:00Z" {
                    Ok(None)
                } else {
                    Ok(Some("2026-01-02 03:04:05".to_owned()))
                }
            }

            fn retry_rfc3339(&self, _delay: Duration) -> Result<String, WorkerError> {
                Ok("2026-01-02T03:05:08Z".to_owned())
            }
        }
        FixedClock
    }

    fn immediate_retry_clock() -> impl ForwardingClock {
        struct ImmediateRetryClock;
        impl ForwardingClock for ImmediateRetryClock {
            fn now_rfc3339(&self) -> Result<String, WorkerError> {
                Ok("2026-01-02T03:04:08Z".to_owned())
            }

            fn local_sms_timestamp(&self, _timestamp: &str) -> Result<Option<String>, WorkerError> {
                Ok(None)
            }

            fn retry_rfc3339(&self, _delay: Duration) -> Result<String, WorkerError> {
                Ok("2026-01-02T03:04:08Z".to_owned())
            }
        }
        ImmediateRetryClock
    }

    fn enqueue_one(
        store: &mut QueueStore,
        telegram: Option<&DestinationBinding>,
        bark: Option<&DestinationBinding>,
    ) -> String {
        let disabled_telegram = DestinationBinding::telegram(
            &key(),
            "synthetic-disabled-token",
            "synthetic-disabled-chat",
        );
        let disabled_bark = DestinationBinding::from_go_identity(
            &key(),
            "bark\0https://api.day.app\0synthetic-device",
        );
        let options = EnqueueOptions::new(
            telegram.is_some(),
            telegram.unwrap_or(&disabled_telegram),
            bark.is_some(),
            bark.unwrap_or(&disabled_bark),
        );
        store
            .enqueue_and_persist(
                &[ForwardingMessage::new(
                    "worker-fixture",
                    "incoming",
                    "65075",
                    "synthetic payload",
                    "2026-01-02T03:04:05Z",
                    "fixture reader",
                )
                .unwrap()],
                "2026-01-02T03:04:06Z",
                &options,
            )
            .unwrap();
        store.inspect().unwrap().tasks[0].id.clone()
    }

    #[tokio::test]
    async fn worker_freezes_sends_confirms_and_archives_telegram_payload() {
        let root = TestRoot::new();
        let state_key = key();
        let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let response = loopback_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 23\r\nConnection: close\r\n\r\n{\"ok\":true,\"result\":{}}",
        )
        .await;
        let sender = TelegramSender::with_endpoint(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::Direct,
            response,
        )
        .unwrap();
        let path = root.0.join("queue.swf1");
        let mut queue = QueueStore::open(&path, state_key, StateLimits::default()).unwrap();
        enqueue_one(&mut queue, Some(&binding), None);
        let worker = ForwardingWorker::new(
            queue,
            Some(sender),
            Some(binding),
            None,
            None,
            fixed_clock(),
        )
        .unwrap();

        let summary = worker.flush_pending().await.unwrap();
        assert_eq!(summary.attempted, 1);
        assert_eq!(summary.delivered, 1);
        assert_eq!(summary.pending, 0);
        let second = worker.flush_pending().await.unwrap();
        assert_eq!(second.attempted, 0);
        assert_eq!(second.pending, 0);
    }

    #[tokio::test]
    async fn worker_persists_known_retry_delay_and_keeps_bark_queue_recoverable() {
        let root = TestRoot::new();
        let state_key = key();
        let response = loopback_response(
            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Type: application/json\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"code\":429}",
        )
        .await;
        let binding =
            DestinationBinding::bark(&state_key, response.as_str(), "synthetic-device").unwrap();
        let sender = BarkSender::with_timeout(
            response.as_str(),
            "synthetic-device",
            None,
            None,
            None,
            Duration::from_secs(15),
        )
        .unwrap();
        let mut queue =
            QueueStore::open(root.0.join("queue.swf1"), state_key, StateLimits::default()).unwrap();
        enqueue_one(&mut queue, None, Some(&binding));
        let worker = ForwardingWorker::new(
            queue,
            None,
            None,
            Some(sender),
            Some(binding),
            fixed_clock(),
        )
        .unwrap();
        let summary = worker.flush_pending().await.unwrap();
        assert_eq!(summary.attempted, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.uncertain, 0);
        let diagnostic = worker.queue.lock().await.inspect().unwrap();
        assert_eq!(diagnostic.tasks[0].bark.attempts, 1);
        assert!(!diagnostic.tasks[0].bark.result_unknown);
        assert!(!diagnostic.tasks[0].bark.permanent_failure);
        let second = worker.flush_pending().await.unwrap();
        assert_eq!(second.attempted, 0);
    }

    #[tokio::test]
    async fn worker_persists_and_stops_after_five_pre_submit_connect_failures() {
        let root = TestRoot::new();
        let state_key = key();
        let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let sender = TelegramSender::with_endpoint(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::Direct,
            Url::parse(&format!("http://{address}")).unwrap(),
        )
        .unwrap();
        let mut queue =
            QueueStore::open(root.0.join("queue.swf1"), state_key, StateLimits::default()).unwrap();
        enqueue_one(&mut queue, Some(&binding), None);
        let worker = ForwardingWorker::new(
            queue,
            Some(sender),
            Some(binding),
            None,
            None,
            immediate_retry_clock(),
        )
        .unwrap();

        for attempt in 1..=PRE_SUBMIT_CONNECT_RETRY_LIMIT {
            let summary = worker.flush_pending().await.unwrap();
            assert_eq!(summary.attempted, 1, "attempt {attempt}");
        }
        let diagnostic = worker.queue.lock().await.inspect().unwrap();
        let delivery = &diagnostic.tasks[0].telegram;
        assert_eq!(delivery.attempts, PRE_SUBMIT_CONNECT_RETRY_LIMIT);
        assert!(delivery.permanent_failure);
        assert!(!delivery.result_unknown);
        assert_eq!(
            delivery.error_code.as_deref(),
            Some("delivery_connect_retry_limit_reached")
        );
        assert_eq!(delivery.next_attempt_at, None);
        assert_eq!(worker.flush_pending().await.unwrap().attempted, 0);
    }

    #[tokio::test]
    async fn worker_pauses_ambiguous_provider_result_without_replaying() {
        let root = TestRoot::new();
        let state_key = key();
        let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let endpoint = loopback_response(b"not-an-http-response").await;
        let sender = TelegramSender::with_endpoint(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::Direct,
            endpoint,
        )
        .unwrap();
        let mut queue =
            QueueStore::open(root.0.join("queue.swf1"), state_key, StateLimits::default()).unwrap();
        enqueue_one(&mut queue, Some(&binding), None);
        let worker = ForwardingWorker::new(
            queue,
            Some(sender),
            Some(binding),
            None,
            None,
            fixed_clock(),
        )
        .unwrap();

        let first = worker.flush_pending().await.unwrap();
        assert_eq!(first.attempted, 1);
        assert_eq!(first.uncertain, 1);
        let diagnostic = worker.queue.lock().await.inspect().unwrap();
        assert!(diagnostic.tasks[0].telegram.result_unknown);

        let retry = worker.flush_pending().await.unwrap();
        assert_eq!(retry.attempted, 0);
        assert_eq!(retry.pending, 1);
    }

    #[tokio::test]
    async fn cancelling_an_in_flight_submit_keeps_durable_unknown_gate() {
        let root = TestRoot::new();
        let state_key = key();
        let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let (endpoint, received, server) = loopback_hanging_request().await;
        let sender = TelegramSender::with_endpoint(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::Direct,
            endpoint,
        )
        .unwrap();
        let mut queue =
            QueueStore::open(root.0.join("queue.swf1"), state_key, StateLimits::default()).unwrap();
        enqueue_one(&mut queue, Some(&binding), None);
        let worker = Arc::new(
            ForwardingWorker::new(
                queue,
                Some(sender),
                Some(binding),
                None,
                None,
                fixed_clock(),
            )
            .unwrap(),
        );

        let submitting = {
            let worker = worker.clone();
            tokio::spawn(async move { worker.flush_pending().await })
        };
        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .unwrap();
        submitting.abort();
        let _ = submitting.await;
        let diagnostic = worker.queue.lock().await.inspect().unwrap();
        assert!(diagnostic.tasks[0].telegram.result_unknown);
        let next_scan = worker.flush_pending().await.unwrap();
        assert_eq!(next_scan.attempted, 0);

        worker.finish_releases().await.unwrap();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn restart_after_ambiguous_submit_never_replays_without_explicit_retry() {
        let root = TestRoot::new();
        let path = root.0.join("queue.swf1");
        let state_key = key();
        let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            for attempt in 1..=2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut block = [0_u8; 2048];
                    let count = stream.read(&mut block).await.unwrap();
                    assert!(count > 0, "client closed before submitting a full request");
                    request.extend_from_slice(&block[..count]);
                    assert!(request.len() <= 16 * 1024);
                    let Some(header_end) =
                        request.windows(4).position(|value| value == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                request_tx.send(attempt).unwrap();

                if attempt == 1 {
                    // The provider has received the request, but its outcome is
                    // lost when the first worker is cancelled, simulating the
                    // process-crash window after external submission.
                    let mut block = [0_u8; 1024];
                    while stream.read(&mut block).await.unwrap_or(0) > 0 {}
                    continue;
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 23\r\nConnection: close\r\n\r\n{\"ok\":true,\"result\":{}}",
                    )
                    .await
                    .unwrap();
            }
        });
        let endpoint = Url::parse(&format!("http://{address}")).unwrap();
        let mut queue = QueueStore::open(&path, state_key, StateLimits::default()).unwrap();
        let task_id = enqueue_one(&mut queue, Some(&binding), None);
        let first_worker = Arc::new(
            ForwardingWorker::new(
                queue,
                Some(
                    TelegramSender::with_endpoint(
                        "synthetic-token",
                        "synthetic-chat",
                        ProxyMode::Direct,
                        endpoint.clone(),
                    )
                    .unwrap(),
                ),
                Some(binding.clone()),
                None,
                None,
                fixed_clock(),
            )
            .unwrap(),
        );

        let submitting = {
            let worker = first_worker.clone();
            tokio::spawn(async move { worker.flush_pending().await })
        };
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), requests.recv())
                .await
                .unwrap(),
            Some(1)
        );
        submitting.abort();
        let _ = submitting.await;
        let before_restart = first_worker.queue().lock().await.inspect().unwrap();
        assert!(before_restart.tasks[0].telegram.result_unknown);
        first_worker.finish_releases().await.unwrap();
        drop(first_worker);

        // Reopen the durable SWF1 file as a fresh process would. Startup/flush
        // must preserve the unresolved marker and perform no provider request.
        let queue = QueueStore::open(&path, key(), StateLimits::default()).unwrap();
        let recovered = queue.inspect().unwrap();
        assert_eq!(recovered.tasks[0].id, task_id);
        assert!(recovered.tasks[0].telegram.result_unknown);
        assert_eq!(recovered.tasks[0].telegram.attempts, 1);
        let second_worker = Arc::new(
            ForwardingWorker::new(
                queue,
                Some(
                    TelegramSender::with_endpoint(
                        "synthetic-token",
                        "synthetic-chat",
                        ProxyMode::Direct,
                        endpoint,
                    )
                    .unwrap(),
                ),
                Some(binding.clone()),
                None,
                None,
                fixed_clock(),
            )
            .unwrap(),
        );
        let startup = second_worker.flush_pending().await.unwrap();
        assert_eq!(startup.attempted, 0);
        assert_eq!(startup.pending, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), requests.recv())
                .await
                .is_err()
        );

        // Only an explicit destination retry clears the durable ambiguity gate.
        second_worker
            .queue()
            .lock()
            .await
            .retry_destination(
                &task_id,
                ForwardingChannel::Telegram,
                true,
                &binding,
                "2026-01-02T03:04:09Z",
            )
            .unwrap();
        let retry = second_worker.flush_pending().await.unwrap();
        assert_eq!(retry.attempted, 1);
        assert_eq!(retry.delivered, 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), requests.recv())
                .await
                .unwrap(),
            Some(2)
        );
        assert!(
            second_worker
                .queue()
                .lock()
                .await
                .inspect()
                .unwrap()
                .tasks
                .is_empty()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn slow_provider_io_keeps_queue_responsive_and_serializes_duplicate_workers() {
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/go-worker-concurrency.json"))
                .unwrap();
        let root = TestRoot::new();
        let state_key = key();
        let binding = DestinationBinding::telegram(&state_key, "synthetic-token", "synthetic-chat");
        let (endpoint, received, release, server) = loopback_gated_response().await;
        let sender = TelegramSender::with_endpoint(
            "synthetic-token",
            "synthetic-chat",
            ProxyMode::Direct,
            endpoint,
        )
        .unwrap();
        let disabled_bark = DestinationBinding::from_go_identity(
            &state_key,
            "bark\0https://api.day.app\0synthetic-device",
        );
        let mut queue =
            QueueStore::open(root.0.join("queue.swf1"), state_key, StateLimits::default()).unwrap();
        let options = EnqueueOptions::new(true, &binding, false, &disabled_bark);
        queue
            .enqueue_and_persist(
                &[ForwardingMessage::new(
                    "slow-provider-task",
                    "incoming",
                    "65075",
                    "synthetic body",
                    "2026-01-02T03:04:05Z",
                    "fixture reader",
                )
                .unwrap()],
                "2026-01-02T03:04:06Z",
                &options,
            )
            .unwrap();
        let task_id = queue.inspect().unwrap().tasks[0].id.clone();
        assert!(!task_id.is_empty());
        let worker = Arc::new(
            ForwardingWorker::new(
                queue,
                Some(sender),
                Some(binding.clone()),
                None,
                None,
                fixed_clock(),
            )
            .unwrap(),
        );

        let first = {
            let worker = worker.clone();
            tokio::spawn(async move { worker.flush_pending().await.unwrap() })
        };
        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .unwrap();

        // While HTTP is blocked, status projection, an unrelated outbound
        // admission, and the explicit in-flight guard must all complete.
        let (summary, admission, cancel) = tokio::time::timeout(Duration::from_secs(1), async {
            let mut queue = worker.queue().lock().await;
            let summary = queue.inspect().unwrap();
            let current = summary
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .unwrap();
            let in_flight = current.telegram.in_flight;
            let admission = queue
                .enqueue_and_persist(
                    &[ForwardingMessage::new(
                        "outbound-during-send",
                        "outbound",
                        "65075",
                        "outgoing control message",
                        "2026-01-02T03:04:07Z",
                        "fixture reader",
                    )
                    .unwrap()],
                    "2026-01-02T03:04:08Z",
                    &options,
                )
                .unwrap();
            let cancel = queue.cancel_destination(
                &task_id,
                ForwardingChannel::Telegram,
                "2026-01-02T03:04:09Z",
            );
            (in_flight, admission, cancel)
        })
        .await
        .expect("provider I/O held the queue lock");
        assert!(
            summary,
            "in-flight provider submission was omitted from status"
        );
        assert_eq!(
            admission.queued,
            expected["outbound_admission_queued"].as_u64().unwrap() as usize
        );
        assert_eq!(cancel.unwrap_err(), QueueMutationError::InFlight);

        let second = {
            let worker = worker.clone();
            tokio::spawn(async move { worker.flush_pending().await.unwrap() })
        };
        release.send(()).unwrap();
        let first = first.await.unwrap();
        let second = second.await.unwrap();
        let duplicate_request = server.await.unwrap();
        assert_eq!(
            first.attempted,
            expected["first_flush_attempted"].as_u64().unwrap() as usize
        );
        assert_eq!(
            first.delivered,
            expected["first_flush_delivered"].as_u64().unwrap() as usize
        );
        assert_eq!(
            second.attempted,
            expected["second_flush_attempted"].as_u64().unwrap() as usize
        );
        assert_eq!(
            second.pending,
            expected["final_pending"].as_u64().unwrap() as usize
        );
        assert_eq!(
            if duplicate_request { 2 } else { 1 },
            expected["provider_requests"].as_u64().unwrap() as usize
        );
        assert!(
            !duplicate_request,
            "serialized second flush duplicated delivery"
        );
        assert!(
            worker
                .queue()
                .lock()
                .await
                .inspect()
                .unwrap()
                .tasks
                .is_empty()
        );
    }
}

#[cfg(test)]
#[path = "worker/attempt_tests.rs"]
mod attempt_tests;
