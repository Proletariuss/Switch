//! Retained ordinary VoWiFi intent. Rebuilding never replays application work.
use crate::native_vowifi_supervisor::SessionGeneration;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle, time::Instant};

pub(crate) type DriverFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExpectedIdentity {
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) iccid: String,
    pub(crate) imsi: String,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FrozenRequest {
    pub(crate) proxy: String,
    pub(crate) epdg: String,
    pub(crate) epdg_port: u16,
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) intent_id: u64,
    /// Zero is the explicit initial start; positive numbers are rebuilds.
    pub(crate) number: u32,
    pub(crate) expected: ExpectedIdentity,
    pub(crate) request: FrozenRequest,
}
impl std::fmt::Debug for ExpectedIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpectedIdentity")
            .field("reader_generation", &self.reader_generation)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for FrozenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenRequest").finish_non_exhaustive()
    }
}
impl std::fmt::Debug for Attempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attempt")
            .field("intent_id", &self.intent_id)
            .field("number", &self.number)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    Stopped,
    ReaderChanged,
    StartupFailed {
        error: String,
    },
    /// A typed transport/temporary REGISTER failure; cleanup still gates retry.
    StartupTransient {
        error: String,
    },
    RuntimeLost {
        error: String,
        retryable: bool,
    },
    RegistrationRenewal,
    PlannedReauthentication,
}
pub(crate) struct AttemptExit {
    pub(crate) reason: EndReason,
    /// Includes physical/network/consumer drain AND exact reader lease release.
    pub(crate) cleanup_confirmed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptProgress {
    Starting,
    Registered(SessionGeneration),
    Draining,
}

/// Each method operates on the same captured attempt. The adapter owns actual
/// prepare/session/recovery records; no method may acquire a replacement SIM.
/// Errors are bounded public codes/messages, without SIM or proxy credentials.
pub(crate) trait Driver: Send + Sync + 'static {
    /// Short native snapshot check, without hardware I/O or a Go callback.
    fn check_identity(&self, expected: &ExpectedIdentity) -> Result<(), String>;
    /// Must retain startup/ordinary cleanup until a terminal result, including
    /// when cancellation is signalled. A caller dropping a waiter is not stop.
    fn run(
        self: Arc<Self>,
        attempt: Attempt,
        stop: watch::Receiver<bool>,
        progress: watch::Sender<AttemptProgress>,
    ) -> DriverFuture<AttemptExit>;
    /// Only reconcile this attempt's existing cleanup and lease receipt. Never
    /// repeat prepare, registration, SMS, USSD, or any uncertain protocol write.
    fn await_cleanup(self: Arc<Self>, attempt: Attempt) -> DriverFuture<Result<(), String>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Starting,
    Registered,
    Draining,
    CleanupUnconfirmed,
    Backoff,
    Stopped,
    Failed,
    ReaderChanged,
}
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub(crate) intent_id: u64,
    pub(crate) attempt: u32,
    pub(crate) generation: Option<SessionGeneration>,
    pub(crate) phase: Phase,
    pub(crate) last_error: Option<String>,
    pub(crate) last_end: Option<EndReason>,
    pub(crate) retry_at: Option<Instant>,
    pub(crate) cleanup_confirmed: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Exit {
    pub(crate) snapshot: Snapshot,
}
#[derive(Clone, Copy)]
pub(crate) struct Policy {
    pub(crate) initial_delay: Duration,
    pub(crate) maximum_delay: Duration,
    pub(crate) jitter: Duration,
    pub(crate) identity_poll: Duration,
    pub(crate) cleanup_retry: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(2),
            maximum_delay: Duration::from_secs(16),
            jitter: Duration::ZERO,
            identity_poll: Duration::from_millis(200),
            cleanup_retry: Duration::from_millis(500),
        }
    }
}
impl Policy {
    fn validate(self) -> Result<Self, &'static str> {
        if self.initial_delay.is_zero()
            || self.initial_delay > self.maximum_delay
            || self.maximum_delay > Duration::from_secs(60)
            || self.identity_poll.is_zero()
            || self.identity_poll > Duration::from_secs(5)
            || self.cleanup_retry.is_zero()
            || self.cleanup_retry > Duration::from_secs(5)
        {
            return Err("native_vowifi_intent_policy_invalid");
        }
        Ok(self)
    }
    fn delay(self, attempt: u32) -> Result<Duration, &'static str> {
        let multiplier = 1_u32 << attempt.saturating_sub(1).min(31);
        let base = self
            .initial_delay
            .saturating_mul(multiplier)
            .min(self.maximum_delay);
        let range = self.jitter.min(base / 4).as_nanos().min(u64::MAX as u128) as u64;
        if range == 0 {
            return Ok(base);
        }
        let mut entropy = [0; 8];
        getrandom::fill(&mut entropy).map_err(|_| "native_vowifi_reconnect_entropy_unavailable")?;
        Ok(base - Duration::from_nanos(u64::from_ne_bytes(entropy) % (range + 1)))
    }
}

/// Service owns this handle until finish. UI may clone only its stop/status
/// channels. No abort API is exposed; cancellation of finish preserves the join.
pub(crate) struct Handle {
    stop: watch::Sender<bool>,
    progress: watch::Receiver<Snapshot>,
    task: Option<JoinHandle<Exit>>,
    outcome: Option<Result<Exit, String>>,
}
impl Handle {
    pub(crate) fn stop_sender(&self) -> watch::Sender<bool> {
        self.stop.clone()
    }
    pub(crate) fn request_stop(&self) {
        self.stop.send_replace(true);
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.progress.borrow().clone()
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.progress.clone()
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.outcome.is_some() || self.task.as_ref().is_some_and(JoinHandle::is_finished)
    }
    pub(crate) async fn finish_cancel_safe(&mut self) -> Result<Exit, String> {
        if let Some(outcome) = &self.outcome {
            return outcome.clone();
        }
        let outcome = self
            .task
            .as_mut()
            .expect("intent completion retained")
            .await
            .map_err(|_| "native_vowifi_intent_task_failed".to_owned());
        self.task.take();
        self.outcome = Some(outcome.clone());
        outcome
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

pub(crate) fn spawn(
    driver: Arc<dyn Driver>,
    intent_id: u64,
    expected: ExpectedIdentity,
    request: FrozenRequest,
    policy: Policy,
) -> Result<Handle, &'static str> {
    let policy = policy.validate()?;
    if intent_id == 0
        || expected.reader.is_empty()
        || expected.reader_generation == 0
        || (expected.iccid.is_empty() && expected.imsi.is_empty())
    {
        return Err("native_vowifi_intent_identity_invalid");
    }
    let state = Snapshot {
        intent_id,
        attempt: 0,
        generation: None,
        phase: Phase::Starting,
        last_error: None,
        last_end: None,
        retry_at: None,
        cleanup_confirmed: true,
    };
    let (stop, stopped) = watch::channel(false);
    let (progress, receiver) = watch::channel(state.clone());
    let task = tokio::spawn(run(
        driver,
        Attempt {
            intent_id,
            number: 0,
            expected,
            request,
        },
        policy,
        stopped,
        progress,
        state,
    ));
    Ok(Handle {
        stop,
        progress: receiver,
        task: Some(task),
        outcome: None,
    })
}

fn stopped(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow() || stop.has_changed().is_err()
}
fn publish(progress: &watch::Sender<Snapshot>, state: &Snapshot) {
    progress.send_replace(state.clone());
}
fn check_identity(driver: &dyn Driver, expected: &ExpectedIdentity) -> Result<(), String> {
    // An observer failure while a session is live still owes that worker its
    // cancellation/drain. Do not unwind the runner and detach its cancellation.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        driver.check_identity(expected)
    }))
    .unwrap_or_else(|_| Err("native_vowifi_identity_observer_failed".into()))
}
async fn run(
    driver: Arc<dyn Driver>,
    mut attempt: Attempt,
    policy: Policy,
    mut stop: watch::Receiver<bool>,
    progress: watch::Sender<Snapshot>,
    mut state: Snapshot,
) -> Exit {
    loop {
        if stopped(&stop) {
            state.phase = Phase::Stopped;
            break;
        }
        if let Err(error) = check_identity(driver.as_ref(), &attempt.expected) {
            state.phase = Phase::ReaderChanged;
            state.last_error = Some(error);
            state.last_end = Some(EndReason::ReaderChanged);
            break;
        }
        state.phase = Phase::Starting;
        state.attempt = attempt.number;
        state.generation = None;
        state.retry_at = None;
        state.cleanup_confirmed = false;
        publish(&progress, &state);
        let (cancel, cancelled) = watch::channel(false);
        let (observed, mut observation) = watch::channel(AttemptProgress::Starting);
        let worker_driver = driver.clone();
        let worker_attempt = attempt.clone();
        // Catch a submitted driver panic as unknown cleanup, never by dropping
        // its future or granting admission to the next generation.
        let mut worker =
            tokio::spawn(
                async move { worker_driver.run(worker_attempt, cancelled, observed).await },
            );
        let mut identity_changed = false;
        let mut stopping = false;
        let mut identity_tick = tokio::time::interval(policy.identity_poll);
        identity_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut observe_open = true;
        let exit = loop {
            tokio::select! {
                biased;
                result=&mut worker=>break result.unwrap_or_else(|_|AttemptExit { reason:EndReason::RuntimeLost {error:"native_vowifi_attempt_task_failed".into(),retryable:false},cleanup_confirmed:false }),
                _=stop.changed(),if !stopping=>{
                    if stopped(&stop) { stopping=true;cancel.send_replace(true);state.phase=Phase::Draining;publish(&progress,&state); }
                },
                _=identity_tick.tick(),if !stopping && !identity_changed=>{
                    if let Err(error)=check_identity(driver.as_ref(),&attempt.expected) {identity_changed=true;cancel.send_replace(true);state.phase=Phase::Draining;state.last_error=Some(error);publish(&progress,&state);}
                },
                changed=observation.changed(),if observe_open=>{
                    if changed.is_err() {observe_open=false;continue;}
                    match *observation.borrow_and_update() {
                        AttemptProgress::Registered(generation)=>{state.generation=Some(generation);if !stopping&&!identity_changed {state.phase=Phase::Registered;state.last_error=None;}},
                        AttemptProgress::Draining=>state.phase=Phase::Draining,
                        AttemptProgress::Starting=>{},
                    }
                    publish(&progress,&state);
                },
            }
        };
        // An immediately terminating worker can publish Registered and finish
        // in one scheduler turn. Preserve its actual generation independently
        // of whether the status watcher won select before the completion.
        if let AttemptProgress::Registered(generation) = *observation.borrow_and_update() {
            state.generation = Some(generation);
        }
        state.last_end = Some(if identity_changed {
            EndReason::ReaderChanged
        } else {
            exit.reason.clone()
        });
        state.retry_at = None;
        if let EndReason::StartupFailed { error }
        | EndReason::StartupTransient { error }
        | EndReason::RuntimeLost { error, .. } = &exit.reason
        {
            if !identity_changed {
                state.last_error = Some(error.clone());
            }
        }
        if !exit.cleanup_confirmed {
            state.phase = Phase::CleanupUnconfirmed;
            state
                .last_error
                .get_or_insert_with(|| "native_vowifi_cleanup_unconfirmed".into());
            publish(&progress, &state);
            loop {
                let cleanup_driver = driver.clone();
                let cleanup_attempt = attempt.clone();
                let mut cleanup =
                    tokio::spawn(
                        async move { cleanup_driver.await_cleanup(cleanup_attempt).await },
                    );
                let result = loop {
                    tokio::select! {
                        result=&mut cleanup=>break result.unwrap_or_else(|_|Err("native_vowifi_cleanup_observer_failed".into())),
                        _=stop.changed(),if !stopping=>{if stopped(&stop){stopping=true;cancel.send_replace(true);publish(&progress,&state);}},
                    }
                };
                match result {
                    Ok(()) => break,
                    Err(error) => {
                        state.last_error = Some(error);
                        publish(&progress, &state);
                        tokio::time::sleep(policy.cleanup_retry).await;
                    }
                }
            }
        }
        state.cleanup_confirmed = true;
        if stopping || stopped(&stop) {
            state.phase = Phase::Stopped;
            break;
        }
        if identity_changed || matches!(exit.reason, EndReason::ReaderChanged) {
            state.phase = Phase::ReaderChanged;
            break;
        }
        if let Err(error) = check_identity(driver.as_ref(), &attempt.expected) {
            state.phase = Phase::ReaderChanged;
            state.last_error = Some(error);
            state.last_end = Some(EndReason::ReaderChanged);
            break;
        }
        match exit.reason {
            EndReason::Stopped => {
                state.phase = Phase::Stopped;
                break;
            }
            EndReason::StartupFailed { .. }
            | EndReason::RuntimeLost {
                retryable: false, ..
            } => {
                state.phase = Phase::Failed;
                break;
            }
            EndReason::StartupTransient { .. }
            | EndReason::RegistrationRenewal
            | EndReason::PlannedReauthentication
            | EndReason::RuntimeLost {
                retryable: true, ..
            } => {}
            EndReason::ReaderChanged => unreachable!(),
        }
        let Some(next) = attempt.number.checked_add(1) else {
            state.phase = Phase::Failed;
            state.last_error = Some("native_vowifi_reconnect_attempt_overflow".into());
            break;
        };
        let delay = match policy.delay(next) {
            Ok(delay) => delay,
            Err(error) => {
                state.phase = Phase::Failed;
                state.last_error = Some(error.into());
                break;
            }
        };
        let deadline = Instant::now() + delay;
        state.phase = Phase::Backoff;
        state.retry_at = Some(deadline);
        publish(&progress, &state);
        tokio::select! {
            biased;
            _=stop.changed()=>{if stopped(&stop){state.phase=Phase::Stopped;break;}},
            _=tokio::time::sleep_until(deadline)=>{},
        }
        attempt.number = next;
    }
    state.retry_at = None;
    publish(&progress, &state);
    Exit { snapshot: state }
}

#[cfg(test)]
mod tests;
