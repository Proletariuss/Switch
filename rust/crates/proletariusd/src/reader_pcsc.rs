//! The process-wide PC/SC handle and complete-transaction owner.
//!
//! A dedicated bounded worker owns each physical handle. Its lexical PC/SC
//! transaction spans the lease, including native protocol closures and Go
//! logical channels. Cancellation revokes admission immediately, but release
//! does not report completion until submitted I/O and native borrowers exit.

mod card_workflow;
mod lifecycle_binding;
pub(crate) use card_workflow::CardWorkflow;
mod presence;

pub(crate) use lifecycle_binding::LifecycleBinding;
pub(crate) use presence::PresenceMonitor;

use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Condvar, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use proletarius_reader::session::{PcscSession, SessionError};
use proletarius_sim::ApduTransport;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(test)]
pub(crate) const PREFIX: &str = "/api/internal/reader-pcsc/";
const MAX_READERS: usize = 16;
const QUEUE: usize = 32;
const CACHE: usize = 32;
const MAX_TTL: u64 = 600_000;
const REVIVE_COOLDOWN: Duration = Duration::from_secs(5);
const WAIT: Duration = Duration::from_secs(65);
static NEXT_NATIVE: AtomicU64 = AtomicU64::new(1);

fn next_native_sequence() -> Result<u64, Failure> {
    NEXT_NATIVE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map_err(|_| Failure::stale())
}

#[derive(Clone)]
pub(crate) struct Failure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) submitted: bool,
    pub(crate) uncertain: bool,
}
impl Failure {
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            submitted: false,
            uncertain: false,
        }
    }
    fn invalid() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "reader_pcsc_invalid_request",
            "PC/SC request is invalid",
        )
    }
    fn stale() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "reader_pcsc_stale_session",
            "PC/SC producer, session or lease changed",
        )
    }
    fn busy() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_pcsc_busy",
            "PC/SC transaction is busy",
        )
    }
    fn stopped() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "reader_pcsc_stopping",
            "PC/SC owner is stopping",
        )
    }
    fn expired() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "reader_pcsc_lease_expired",
            "PC/SC lease has expired",
        )
    }
    fn recovery_cooldown(remaining: Duration) -> Self {
        let whole_seconds = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0));
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "reader_pcsc_recovery_cooldown",
            message: format!("读卡器复位冷却中，请约 {whole_seconds} 秒后重试"),
            submitted: false,
            uncertain: false,
        }
    }
    fn unknown() -> Self {
        let mut value = Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "reader_pcsc_outcome_unknown",
            "PC/SC submitted operation did not complete within its budget",
        );
        value.submitted = true;
        value.uncertain = true;
        value
    }
    fn from_session(error: SessionError, submitted: bool) -> Self {
        let mut result = if error.is_card_absent() {
            Self::new(
                StatusCode::CONFLICT,
                "reader_pcsc_card_absent",
                "No card is available in the selected reader",
            )
        } else if error.is_reader_unavailable() {
            Self::new(
                StatusCode::NOT_FOUND,
                "reader_pcsc_not_found",
                "Selected PC/SC reader is unavailable",
            )
        } else if error.is_busy() {
            Self::busy()
        } else if error.is_unresponsive() {
            Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "reader_pcsc_unresponsive_card",
                "The selected card is unresponsive",
            )
        } else {
            Self::new(
                StatusCode::BAD_GATEWAY,
                "reader_pcsc_io_failed",
                "PC/SC operation failed",
            )
        };
        result.submitted = submitted;
        result.uncertain = submitted;
        result
    }
    pub(crate) fn response(self) -> Response<Full<Bytes>> {
        crate::json_response(
            self.status,
            &json!({"code":self.code,"error":self.message,"submitted":self.submitted,"uncertain":self.uncertain,"retryable":false}),
        )
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Failure")
            .field("code", &self.code)
            .field("submitted", &self.submitted)
            .field("uncertain", &self.uncertain)
            .finish()
    }
}
impl std::error::Error for Failure {}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub(crate) producer_id: String,
    pub(crate) reader: String,
    pub(crate) session_id: String,
    pub(crate) lease_id: String,
    pub(crate) lease_seq: u64,
}
impl Binding {
    pub(crate) fn validate_for_reader(&self, reader: &str) -> Result<(), Failure> {
        if self.reader != reader
            || !valid_reader(reader)
            || !valid_token(&self.producer_id)
            || !valid_token(&self.session_id)
            || !valid_token(&self.lease_id)
            || self.lease_seq == 0
        {
            Err(Failure::invalid())
        } else {
            Ok(())
        }
    }
}
fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}
fn valid_reader(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

fn revival_cooldown_remaining(last_revive: Instant, now: Instant) -> Option<Duration> {
    let remaining = REVIVE_COOLDOWN.saturating_sub(now.saturating_duration_since(last_revive));
    (!remaining.is_zero()).then_some(remaining)
}

/// Object-safe, injectable physical factory. No unsafe lifetime extension is
/// used: the worker's command loop runs inside the real transaction callback.
pub(crate) trait Physical: Send {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = Failure>),
    ) -> Result<(), Failure>;
    fn close(&mut self) -> Result<(), Failure>;
}
type Factory = Arc<dyn Fn(&str) -> Result<Box<dyn Physical>, Failure> + Send + Sync>;
type Enumerator = Arc<dyn Fn() -> Result<Vec<String>, Failure> + Send + Sync>;
type Resetter = Arc<dyn Fn(&str, bool) -> Result<Box<dyn Physical>, Failure> + Send + Sync>;
struct RealPhysical(PcscSession);
impl Physical for RealPhysical {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = Failure>),
    ) -> Result<(), Failure> {
        struct Adapter<'a, 'b>(&'a mut proletarius_reader::session::PcscTransaction<'b>);
        impl ApduTransport for Adapter<'_, '_> {
            type Error = Failure;
            fn transmit(&mut self, c: &[u8]) -> Result<Vec<u8>, Failure> {
                self.0
                    .transmit(c)
                    .map_err(|e| Failure::from_session(e, true))
            }
        }
        let mut transaction = self
            .0
            .transaction()
            .map_err(|e| Failure::from_session(e, false))?;
        body(&mut Adapter(&mut transaction));
        transaction
            .end()
            .map_err(|e| Failure::from_session(e, false))
    }
    fn close(&mut self) -> Result<(), Failure> {
        self.0.close().map_err(|e| Failure::from_session(e, false))
    }
}

#[derive(Clone)]
pub(crate) struct Owner {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<OwnerState>,
    lifecycle: Mutex<()>,
    final_retirement: tokio::sync::Mutex<FinalRetirement>,
    factory: Factory,
    enumerate: Enumerator,
    reset: Resetter,
    normal_pool: crate::blocking_pool::BlockingPool,
    control_pool: crate::blocking_pool::BlockingPool,
}
#[derive(Default)]
struct FinalRetirement {
    task: Option<tokio::task::JoinHandle<Result<(), Failure>>>,
    confirmed: bool,
}
struct OwnerState {
    producer: String,
    active: bool,
    stopping: bool,
    admission_closed: bool,
    final_shutdown: bool,
    presence_handoff_enabled: bool,
    pending_presence_handoffs: HashMap<String, PendingPresenceHandoff>,
    closed_sessions: HashMap<String, u64>,
    retired_operation_leases: HashMap<String, u64>,
    readers: HashMap<String, Arc<Worker>>,
}
struct PendingPresenceHandoff {
    session: presence::Session,
    close_confirmed: bool,
}
struct Worker {
    reader: String,
    state: Mutex<ReaderState>,
    changed: Condvar,
    sender: mpsc::SyncSender<Work>,
    thread: Mutex<Option<JoinHandle<()>>>,
    quarantined: Mutex<Option<Box<dyn Physical>>>,
}
struct ReaderState {
    producer: String,
    session: String,
    session_seq: u64,
    session_high: u64,
    lease: Option<Lease>,
    lease_high: u64,
    last_terminal: Option<(Binding, &'static str)>,
    stopping: bool,
    closing: bool,
    close_error: Option<Failure>,
    port_open: bool,
    native_borrow: Option<u64>,
    card_workflow: Option<card_workflow::Retention>,
    native_sequence: u64,
    wire_high: u64,
    wire_cache: VecDeque<Cached>,
}
struct Lease {
    internal: bool,
    binding: Binding,
    deadline: Instant,
    acquired: bool,
    ending: Option<&'static str>,
}
#[derive(Clone)]
struct Cached {
    sequence: u64,
    id: String,
    fingerprint: Vec<u8>,
    result: Option<Result<Value, Failure>>,
}
type Reply<T> = mpsc::SyncSender<Result<T, Failure>>;
enum Work {
    Acquire {
        binding: Binding,
        reply: Reply<Value>,
        admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    },
    Transmit {
        binding: Binding,
        native: Option<u64>,
        command: Vec<u8>,
        wire: Option<(u64, String)>,
        reply: Reply<Vec<u8>>,
    },
    Reset {
        binding: Binding,
        native: Option<u64>,
        action: ResetAction,
        sequence: u64,
        id: String,
        reply: Reply<Value>,
    },
    Wake,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResetAction {
    Reset,
    Reopen,
}
enum ExitTransaction {
    Released,
    Reset(ResetAction, Option<u64>, u64, String, Reply<Value>),
    Failed(Failure),
}

impl Owner {
    pub(crate) fn new(producer: String) -> Self {
        Self::create(
            producer,
            Arc::new(|reader| {
                PcscSession::connect(reader)
                    .map(|v| Box::new(RealPhysical(v)) as Box<dyn Physical>)
                    .map_err(|e| Failure::from_session(e, false))
            }),
            Arc::new(|| PcscSession::reader_names().map_err(|e| Failure::from_session(e, false))),
            Arc::new(|reader, power| {
                PcscSession::connect_for_reset(reader, power)
                    .map(|v| Box::new(RealPhysical(v)) as Box<dyn Physical>)
                    .map_err(|e| Failure::from_session(e, false))
            }),
        )
    }
    fn create(producer: String, factory: Factory, enumerate: Enumerator, reset: Resetter) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(OwnerState {
                    producer,
                    active: true,
                    stopping: false,
                    admission_closed: false,
                    final_shutdown: false,
                    presence_handoff_enabled: false,
                    pending_presence_handoffs: HashMap::new(),
                    closed_sessions: HashMap::new(),
                    retired_operation_leases: HashMap::new(),
                    readers: HashMap::new(),
                }),
                lifecycle: Mutex::new(()),
                final_retirement: tokio::sync::Mutex::new(FinalRetirement::default()),
                factory,
                enumerate,
                reset,
                normal_pool: crate::blocking_pool::BlockingPool::new(8),
                control_pool: crate::blocking_pool::BlockingPool::new(4),
            }),
        }
    }
    #[cfg(test)]
    pub(crate) fn with_factory(
        producer: String,
        factory: Factory,
        enumerate: Enumerator,
        reset: Resetter,
    ) -> Self {
        Self::create(producer, factory, enumerate, reset)
    }
    #[cfg(test)]
    pub(crate) fn test_open_acquire(
        &self,
        binding: &Binding,
        session_sequence: u64,
    ) -> Result<(), Failure> {
        self.open(
            &binding.producer_id,
            &binding.reader,
            &binding.session_id,
            session_sequence,
        )?;
        self.acquire(binding, Duration::from_secs(120))?;
        Ok(())
    }
    /// Native lifecycle entry point. The returned identity belongs to this
    /// physical owner, independently of a compatibility process restart.
    pub(crate) fn native_open(&self, reader: &str) -> Result<LifecycleBinding, Failure> {
        let producer = self.inner.state.lock().unwrap().producer.clone();
        if let Ok(binding) = self.with_live_lifecycle_binding(&producer, reader, Clone::clone) {
            return Ok(binding);
        }
        let sequence = next_native_sequence()?;
        if sequence == 0 || sequence == u64::MAX {
            return Err(Failure::stale());
        }
        let session = format!("native-session-{sequence}");
        self.open_inner(&producer, reader, &session, sequence, true)?;
        self.with_live_lifecycle_binding(&producer, reader, Clone::clone)
    }
    pub(crate) fn native_close(&self, session: &LifecycleBinding) -> Result<Value, Failure> {
        self.close(
            &session.producer,
            &session.reader,
            &session.session_id,
            session.session_sequence,
            None,
        )
    }
    /// Construct the cleanup authority before acquisition can reach a worker.
    pub(crate) fn native_pin_binding(
        &self,
        session: &LifecycleBinding,
        pin: &crate::reader_reservation::NativePinBinding,
    ) -> Result<Binding, Failure> {
        if session.reader != pin.reader || session.producer != pin.producer_id {
            return Err(Failure::stale());
        }
        self.with_live_lifecycle_binding(&session.producer, &session.reader, |live| {
            if live != session {
                return Err(Failure::stale());
            }
            Ok(Binding {
                producer_id: session.producer.clone(),
                reader: session.reader.clone(),
                session_id: session.session_id.clone(),
                lease_id: pin.lease_id.clone(),
                lease_seq: pin.lease_sequence,
            })
        })?
    }
    pub(crate) fn native_acquire_binding(
        &self,
        binding: &Binding,
        duration: Duration,
        admission: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Value, Failure> {
        self.acquire_inner_guarded(binding, duration, true, Some(admission))
    }
    pub(crate) fn native_reset(
        &self,
        binding: &Binding,
        action: ResetAction,
    ) -> Result<Value, Failure> {
        let worker = self.worker(&binding.producer_id, &binding.reader)?;
        let sequence = {
            let state = worker.state.lock().unwrap();
            state.wire_high.checked_add(1).ok_or_else(Failure::stale)?
        };
        let id = format!("native-reset-{}", next_native_sequence()?);
        self.reset(binding, action, sequence, id)
    }
    /// A lifecycle worker may borrow only its captured session. Never silently
    /// open or attach to a replacement while metadata preparation is running.
    pub(crate) fn native_work_on_session<T>(
        &self,
        session: &LifecycleBinding,
        operation: impl FnOnce(&mut NativeTransport) -> T,
    ) -> Result<T, Failure> {
        self.with_live_lifecycle_binding(&session.producer, &session.reader, |live| {
            if live == session {
                Ok(())
            } else {
                Err(Failure::stale())
            }
        })??;
        let sequence = next_native_sequence()?;
        let binding = Binding {
            producer_id: session.producer.clone(),
            reader: session.reader.clone(),
            session_id: session.session_id.clone(),
            lease_id: format!("native-work-{sequence}"),
            lease_seq: sequence,
        };
        let acquired = self.acquire_inner(&binding, Duration::from_millis(MAX_TTL), true);
        let result = match acquired {
            Ok(_) => self.with_transaction(Some(&binding), &binding.reader, operation),
            Err(error) => Err(error),
        };
        let cleanup = self.release(&binding);
        let confirmed = self
            .status(&binding)
            .is_ok_and(|value| value.get("terminal").and_then(Value::as_bool) == Some(true));
        if !confirmed {
            let mut error = cleanup.err().unwrap_or_else(Failure::unknown);
            error.submitted = true;
            error.uncertain = true;
            return Err(error);
        }
        // An uncertain protocol result remains uncertain; confirmed physical
        // cleanup only permits retiring its admission, never replaying I/O.
        result
    }
    fn check_producer(&self, producer: &str) -> Result<(), Failure> {
        let state = self.inner.state.lock().unwrap();
        if state.producer != producer {
            Err(Failure::stale())
        } else {
            Ok(())
        }
    }
    fn check_admission(&self) -> Result<(), Failure> {
        let state = self.inner.state.lock().unwrap();
        if state.admission_closed || state.stopping || !state.active {
            Err(Failure::stopped())
        } else {
            Ok(())
        }
    }
    pub(crate) fn begin_shutdown(&self) {
        self.inner.state.lock().unwrap().admission_closed = true;
    }
    fn enable_presence_handoff(&self) {
        self.inner.state.lock().unwrap().presence_handoff_enabled = true;
    }
    fn confirmed_presence_handoffs(&self) -> Vec<presence::Session> {
        self.inner
            .state
            .lock()
            .unwrap()
            .pending_presence_handoffs
            .values()
            .filter(|pending| pending.close_confirmed)
            .map(|pending| pending.session.clone())
            .collect()
    }
    fn acknowledge_presence_handoff(&self, session: &presence::Session) {
        let mut state = self.inner.state.lock().unwrap();
        if state
            .pending_presence_handoffs
            .get(&session.reader)
            .is_some_and(|pending| pending.close_confirmed && pending.session == *session)
        {
            state.pending_presence_handoffs.remove(&session.reader);
        }
    }
    fn worker(&self, producer: &str, reader: &str) -> Result<Arc<Worker>, Failure> {
        self.check_producer(producer)?;
        self.inner
            .state
            .lock()
            .unwrap()
            .readers
            .get(reader)
            .cloned()
            .ok_or_else(Failure::stale)
    }
    fn open(
        &self,
        producer: &str,
        reader: &str,
        session: &str,
        sequence: u64,
    ) -> Result<Value, Failure> {
        self.open_inner(producer, reader, session, sequence, false)
    }
    fn open_inner(
        &self,
        producer: &str,
        reader: &str,
        session: &str,
        sequence: u64,
        internal: bool,
    ) -> Result<Value, Failure> {
        if !valid_token(producer)
            || !valid_reader(reader)
            || !valid_token(session)
            || (!internal && sequence == 0)
        {
            return Err(Failure::invalid());
        }
        self.check_admission()?;
        self.check_producer(producer)?;
        // Do not enumerate or connect again for a known idempotent open.
        if let Ok(worker) = self.worker(producer, reader) {
            let state = worker.state.lock().unwrap();
            if state.session == session
                && state.session_seq == sequence
                && !state.closing
                && !state.stopping
            {
                return Ok(json!({"open":true,"reader":reader,"session_id":session}));
            }
        }
        if !(self.inner.enumerate)()?.iter().any(|name| name == reader) {
            return Err(Failure::new(
                StatusCode::NOT_FOUND,
                "reader_pcsc_not_found",
                "Selected PC/SC reader is unavailable",
            ));
        }
        let worker = {
            let mut state = self.inner.state.lock().unwrap();
            if state.stopping
                || state.admission_closed
                || !state.active
                || state.producer != producer
            {
                return Err(Failure::stale());
            }
            if !internal
                && state
                    .closed_sessions
                    .get(reader)
                    .is_some_and(|high| sequence <= *high)
            {
                return Err(Failure::stale());
            }
            if state.pending_presence_handoffs.contains_key(reader) {
                return Err(Failure::busy());
            }
            if let Some(worker) = state.readers.get(reader) {
                worker.clone()
            } else {
                if state.readers.len() >= MAX_READERS {
                    return Err(Failure::busy());
                }
                let (sender, receiver) = mpsc::sync_channel(QUEUE);
                let worker = Arc::new(Worker {
                    reader: reader.into(),
                    state: Mutex::new(ReaderState {
                        producer: producer.into(),
                        session: String::new(),
                        session_seq: 0,
                        session_high: 0,
                        lease: None,
                        lease_high: 0,
                        last_terminal: None,
                        stopping: false,
                        closing: false,
                        close_error: None,
                        port_open: false,
                        native_borrow: None,
                        card_workflow: None,
                        native_sequence: 0,
                        wire_high: 0,
                        wire_cache: VecDeque::new(),
                    }),
                    changed: Condvar::new(),
                    sender,
                    thread: Mutex::new(None),
                    quarantined: Mutex::new(None),
                });
                let thread_worker = Arc::downgrade(&worker);
                let inner = Arc::downgrade(&self.inner);
                *worker.thread.lock().unwrap() = Some(
                    thread::Builder::new()
                        .name("switch-pcsc".into())
                        .spawn(move || run_worker(thread_worker, receiver, inner))
                        .map_err(|_| Failure::busy())?,
                );
                state.readers.insert(reader.into(), worker.clone());
                worker
            }
        };
        let mut state = worker.state.lock().unwrap();
        if state.stopping || state.producer != producer {
            return Err(Failure::stale());
        }
        if let Some(error) = &state.close_error {
            return Err(error.clone());
        }
        if state.session == session && state.session_seq == sequence && !state.closing {
            return Ok(json!({"open":true,"reader":reader,"session_id":session}));
        }
        if !internal && sequence <= state.session_high {
            return Err(Failure::stale());
        }
        if state.lease.is_some() || state.native_borrow.is_some() || state.closing {
            return Err(Failure::busy());
        }
        if state.port_open {
            state.closing = true;
            drop(state);
            let _ = worker.sender.try_send(Work::Wake);
            state = worker.state.lock().unwrap();
            while state.closing {
                state = worker.changed.wait(state).unwrap();
            }
        }
        drop(state);
        self.publish_session(&worker, producer, reader, session, sequence, internal)
    }
    fn publish_session(
        &self,
        worker: &Arc<Worker>,
        producer: &str,
        reader: &str,
        session: &str,
        sequence: u64,
        internal: bool,
    ) -> Result<Value, Failure> {
        // Enumeration and the old handle's close may block. Publish under the
        // same owner lock as close tombstones, then revalidate the reader: a
        // native borrower or another open may have won while we waited.
        let owner = self.inner.state.lock().unwrap();
        if owner.stopping
            || owner.admission_closed
            || !owner.active
            || owner.producer != producer
            || !owner
                .readers
                .get(reader)
                .is_some_and(|current| Arc::ptr_eq(current, worker))
            || (!internal
                && owner
                    .closed_sessions
                    .get(reader)
                    .is_some_and(|high| sequence <= *high))
            || owner.pending_presence_handoffs.contains_key(reader)
        {
            return Err(Failure::stale());
        }
        let mut state = worker.state.lock().unwrap();
        if state.stopping || state.producer != producer {
            return Err(Failure::stale());
        }
        if let Some(error) = &state.close_error {
            return Err(error.clone());
        }
        if state.session == session && state.session_seq == sequence && !state.closing {
            return Ok(json!({"open":true,"reader":reader,"session_id":session}));
        }
        if !internal && sequence <= state.session_high {
            return Err(Failure::stale());
        }
        if state.lease.is_some()
            || state.native_borrow.is_some()
            || state.closing
            || state.port_open
        {
            return Err(Failure::busy());
        }
        state.session = session.into();
        state.session_seq = sequence;
        if !internal {
            state.session_high = sequence;
        }
        state.wire_high = 0;
        state.wire_cache.clear();
        Ok(json!({"open":true,"reader":reader,"session_id":session}))
    }
    fn acquire(&self, binding: &Binding, duration: Duration) -> Result<Value, Failure> {
        self.acquire_inner(binding, duration, false)
    }
    fn acquire_inner(
        &self,
        binding: &Binding,
        duration: Duration,
        internal: bool,
    ) -> Result<Value, Failure> {
        self.acquire_inner_guarded(binding, duration, internal, None)
    }
    fn acquire_inner_guarded(
        &self,
        binding: &Binding,
        duration: Duration,
        internal: bool,
        admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Value, Failure> {
        self.check_admission()?;
        binding.validate_for_reader(&binding.reader)?;
        // Keep the owner fence and worker publication in one lock order.
        // A retained transaction may cancel a queued acquire before it starts.
        let owner = self.inner.state.lock().unwrap();
        if owner.producer != binding.producer_id
            || !owner.active
            || owner.stopping
            || owner.admission_closed
        {
            return Err(Failure::stale());
        }
        let worker = owner
            .readers
            .get(&binding.reader)
            .cloned()
            .ok_or_else(Failure::stale)?;
        let (reply, receive) = mpsc::sync_channel(1);
        {
            let mut state = worker.state.lock().unwrap();
            if !internal
                && owner
                    .retired_operation_leases
                    .get(&binding.reader)
                    .is_some_and(|high| binding.lease_seq <= *high)
            {
                return Err(Failure::stale());
            }
            drop(owner);
            check_session(&state, binding)?;
            expire(&mut state);
            if let Some(lease) = &state.lease {
                if lease.binding == *binding && lease.ending.is_none() && lease.acquired {
                    return Ok(ack(binding));
                }
                return Err(Failure::busy());
            }
            if !internal && binding.lease_seq <= state.lease_high {
                return Err(Failure::stale());
            }
            state.lease = Some(Lease {
                internal,
                binding: binding.clone(),
                deadline: Instant::now() + duration,
                acquired: false,
                ending: None,
            });
            state.wire_high = 0;
            state.wire_cache.clear();
            if worker
                .sender
                .try_send(Work::Acquire {
                    binding: binding.clone(),
                    reply,
                    admission,
                })
                .is_err()
            {
                state.lease = None;
                if !internal {
                    state.lease_high = state.lease_high.max(binding.lease_seq);
                }
                return Err(Failure::busy());
            }
        }
        receive.recv().unwrap_or_else(|_| Err(Failure::stopped()))
    }
    pub(crate) fn renew(&self, binding: &Binding, duration: Duration) -> Result<Value, Failure> {
        self.check_admission()?;
        let worker = self.worker(&binding.producer_id, &binding.reader)?;
        let mut state = worker.state.lock().unwrap();
        check_live(&mut state, binding)?;
        state.lease.as_mut().unwrap().deadline = Instant::now() + duration;
        Ok(ack(binding))
    }
    pub(crate) fn status(&self, binding: &Binding) -> Result<Value, Failure> {
        binding.validate_for_reader(&binding.reader)?;
        let worker = match self.worker(&binding.producer_id, &binding.reader) {
            Ok(v) => v,
            Err(e) if e.code == "reader_pcsc_stale_session" => {
                let owner = self.inner.state.lock().unwrap();
                // No worker is not evidence that a queued open/acquire for
                // the current producer has been cancelled.
                return Ok(
                    if owner.producer == binding.producer_id && owner.active && !owner.stopping {
                        status_value(binding, "closing", false)
                    } else {
                        status_value(binding, "stale", true)
                    },
                );
            }
            Err(e) => return Err(e),
        };
        let mut state = worker.state.lock().unwrap();
        expire(&mut state);
        let value = if state.close_error.is_some() {
            status_value(binding, "closing", false)
        } else if let Some(lease) = &state.lease {
            if lease.binding == *binding {
                status_value(
                    binding,
                    if lease.ending.is_some() || state.closing {
                        "closing"
                    } else {
                        "live"
                    },
                    false,
                )
            } else {
                status_value(
                    binding,
                    if binding.lease_seq <= state.lease_high {
                        "released"
                    } else {
                        "closing"
                    },
                    binding.lease_seq <= state.lease_high,
                )
            }
        } else if let Some((old, kind)) = &state.last_terminal {
            if old == binding {
                status_value(binding, kind, true)
            } else if binding.lease_seq > state.lease_high {
                status_value(binding, "closing", false)
            } else {
                status_value(binding, "released", true)
            }
        } else if binding.lease_seq > state.lease_high {
            // A not-yet-started acquire may still carry this token. Only an
            // explicit release tombstone can prove it cannot become live.
            status_value(binding, "closing", false)
        } else {
            status_value(binding, "released", true)
        };
        drop(state);
        let _ = worker.sender.try_send(Work::Wake);
        Ok(value)
    }
    /// Retire a registered adapter lease even if its acquire never reached the
    /// physical queue. The owner-level watermark survives absent workers and
    /// is checked atomically when every later external acquire is published.
    pub(crate) fn retire_operation_binding(&self, binding: &Binding) -> Result<bool, Failure> {
        binding.validate_for_reader(&binding.reader)?;
        let mut owner = self.inner.state.lock().unwrap();
        // Producer replacement is published only after worker join and close.
        if owner.producer != binding.producer_id {
            return Ok(true);
        }
        if !owner.retired_operation_leases.contains_key(&binding.reader)
            && owner.retired_operation_leases.len() >= MAX_READERS
        {
            return Err(Failure::busy());
        }
        let high = owner
            .retired_operation_leases
            .entry(binding.reader.clone())
            .or_default();
        *high = (*high).max(binding.lease_seq);
        let Some(worker) = owner.readers.get(&binding.reader).cloned() else {
            return Ok(true);
        };
        let mut state = worker.state.lock().unwrap();
        drop(owner);
        if state.close_error.is_some() {
            return Ok(false);
        }
        state.lease_high = state.lease_high.max(binding.lease_seq);
        let pending = if let Some(lease) = state
            .lease
            .as_mut()
            .filter(|lease| lease.binding == *binding)
        {
            lease.ending.get_or_insert("released");
            true
        } else {
            false
        };
        drop(state);
        if pending {
            worker.changed.notify_all();
            let _ = worker.sender.try_send(Work::Wake);
        }
        Ok(!pending)
    }
    pub(crate) fn release(&self, binding: &Binding) -> Result<Value, Failure> {
        binding.validate_for_reader(&binding.reader)?;
        let worker = self.worker(&binding.producer_id, &binding.reader)?;
        let mut state = worker.state.lock().unwrap();
        check_session(&state, binding)?;
        if !state
            .lease
            .as_ref()
            .is_some_and(|l| l.binding == *binding && l.internal)
        {
            state.lease_high = state.lease_high.max(binding.lease_seq);
        }
        if let Some(lease) = &mut state.lease {
            if lease.binding == *binding {
                lease.ending.get_or_insert("released");
            } else {
                return Ok(ack(binding));
            }
        }
        drop(state);
        worker.changed.notify_all();
        let _ = worker.sender.try_send(Work::Wake);
        state = worker.state.lock().unwrap();
        while state
            .lease
            .as_ref()
            .is_some_and(|lease| lease.binding == *binding)
        {
            state = worker.changed.wait(state).unwrap();
        }
        if let Some(error) = &state.close_error {
            return Err(error.clone());
        }
        Ok(ack(binding))
    }
    pub(crate) fn close(
        &self,
        producer: &str,
        reader: &str,
        session: &str,
        sequence: u64,
        binding: Option<&Binding>,
    ) -> Result<Value, Failure> {
        if !valid_token(producer) || !valid_reader(reader) || !valid_token(session) || sequence == 0
        {
            return Err(Failure::invalid());
        }
        let worker = self.record_session_close(producer, reader, sequence)?;
        let empty = json!({"accepted":true,"session_id":session,"lease_id":binding.map(|b|b.lease_id.as_str()).unwrap_or(""),"lease_seq":binding.map(|b|b.lease_seq).unwrap_or(0)});
        let Some(worker) = worker else {
            return Ok(empty);
        };
        let mut state = worker.state.lock().unwrap();
        if state.session != session || state.session_seq != sequence {
            return Ok(empty);
        }
        if let Some(lease) = &mut state.lease {
            if binding != Some(&lease.binding) {
                return Err(Failure::busy());
            }
            lease.ending.get_or_insert("released");
        }
        state.closing = true;
        state.session_high = state.session_high.max(sequence);
        drop(state);
        let _ = worker.sender.try_send(Work::Wake);
        state = worker.state.lock().unwrap();
        while closing_session_pending(&state, session, sequence) {
            state = worker.changed.wait(state).unwrap();
        }
        finish_session_close(&mut state, session, sequence)?;
        Ok(
            json!({"accepted":true,"session_id":session,"lease_id":binding.map(|b|b.lease_id.as_str()).unwrap_or(""),"lease_seq":binding.map(|b|b.lease_seq).unwrap_or(0)}),
        )
    }
    fn record_session_close(
        &self,
        producer: &str,
        reader: &str,
        sequence: u64,
    ) -> Result<Option<Arc<Worker>>, Failure> {
        let mut state = self.inner.state.lock().unwrap();
        // Producer retirement can finish after request validation. Its fence
        // and the tombstone mutation must belong to the same critical section.
        if state.producer != producer {
            return Err(Failure::stale());
        }
        if !state.closed_sessions.contains_key(reader) && state.closed_sessions.len() >= MAX_READERS
        {
            return Err(Failure::busy());
        }
        let high = state.closed_sessions.entry(reader.into()).or_default();
        *high = (*high).max(sequence);
        Ok(state.readers.get(reader).cloned())
    }
    fn transmit(
        &self,
        binding: &Binding,
        native: Option<u64>,
        command: Vec<u8>,
        wire: Option<(u64, String)>,
    ) -> Result<Vec<u8>, Failure> {
        let admission = self.check_admission();
        if !(4..=65544).contains(&command.len()) {
            return Err(Failure::invalid());
        }
        let worker = self.worker(&binding.producer_id, &binding.reader)?;
        let (reply, receive) = mpsc::sync_channel(1);
        {
            let mut state = worker.state.lock().unwrap();
            if admission.is_err() && !card_workflow::recovering(&state, binding, native) {
                admission?;
            }
            card_workflow::live(&mut state, binding, native)?;
            if state.native_borrow != native {
                return Err(Failure::busy());
            }
            if let Some((seq, id)) = &wire {
                if let Some(value) = admit_wire(&mut state, *seq, id, &command)? {
                    return decode_response(&value);
                }
            }
            if worker
                .sender
                .try_send(Work::Transmit {
                    binding: binding.clone(),
                    native,
                    command,
                    wire: wire.clone(),
                    reply,
                })
                .is_err()
            {
                if let Some((seq, id)) = &wire {
                    cache_result(&mut state, *seq, id, Err(Failure::busy()));
                }
                return Err(Failure::busy());
            }
        }
        receive.recv().unwrap_or_else(|_| Err(Failure::unknown()))
    }
    fn reset(
        &self,
        binding: &Binding,
        action: ResetAction,
        seq: u64,
        id: String,
    ) -> Result<Value, Failure> {
        self.check_admission()?;
        if action == ResetAction::Reset && !is_estk(&binding.reader) {
            return Err(Failure::new(
                StatusCode::CONFLICT,
                "reader_pcsc_reset_unsupported",
                "Power cycling is only supported for ESTKme readers",
            ));
        }
        let worker = self.worker(&binding.producer_id, &binding.reader)?;
        let (reply, receive) = mpsc::sync_channel(1);
        {
            let mut state = worker.state.lock().unwrap();
            check_live(&mut state, binding)?;
            if state.native_borrow.is_some() {
                return Err(Failure::busy());
            }
            if state.wire_cache.iter().any(|entry| entry.result.is_none()) {
                return Err(Failure::busy());
            }
            let fingerprint = if action == ResetAction::Reset {
                b"reset".as_slice()
            } else {
                b"reopen".as_slice()
            };
            if let Some(value) = admit_wire(&mut state, seq, &id, fingerprint)? {
                return Ok(value);
            }
            if worker
                .sender
                .try_send(Work::Reset {
                    binding: binding.clone(),
                    action,
                    native: None,
                    sequence: seq,
                    id: id.clone(),
                    reply,
                })
                .is_err()
            {
                cache_result(&mut state, seq, &id, Err(Failure::busy()));
                return Err(Failure::busy());
            }
        }
        receive.recv().unwrap_or_else(|_| Err(Failure::unknown()))
    }

    /// Native protocol code borrows the same complete PC/SC transaction. The
    /// closure's lifetime is a separately tracked admission, so neither a
    /// cancelled HTTP waiter nor a lease timer can release its physical owner.
    pub(crate) fn with_transaction<R, F>(
        &self,
        binding: Option<&Binding>,
        reader: &str,
        operation: F,
    ) -> Result<R, Failure>
    where
        F: FnOnce(&mut NativeTransport) -> R,
    {
        self.check_admission()?;
        if !valid_reader(reader) {
            return Err(Failure::invalid());
        }
        let owned;
        let temporary = binding.is_none();
        let binding = if let Some(binding) = binding {
            binding.validate_for_reader(reader)?;
            binding
        } else {
            let producer = {
                let state = self.inner.state.lock().unwrap();
                if state.stopping || !state.active {
                    return Err(Failure::stopped());
                }
                state.producer.clone()
            };
            let worker = self.worker(&producer, reader).ok();
            let session = worker.as_ref().and_then(|w| {
                let s = w.state.lock().unwrap();
                if !s.session.is_empty() && !s.closing {
                    Some(s.session.clone())
                } else {
                    None
                }
            });
            let session = if let Some(session) = session {
                session
            } else {
                let n = next_native_sequence()?;
                let session = format!("native-session-{n}");
                self.open_inner(&producer, reader, &session, 0, true)?;
                session
            };
            let worker = self.worker(&producer, reader)?;
            let sequence = {
                let s = worker.state.lock().unwrap();
                if s.lease.is_some() {
                    return Err(Failure::busy());
                }
                next_native_sequence()?
            };
            owned = Binding {
                producer_id: producer,
                reader: reader.into(),
                session_id: session,
                lease_id: format!("native-lease-{}", next_native_sequence()?),
                lease_seq: sequence,
            };
            self.acquire_inner(&owned, Duration::from_millis(MAX_TTL), true)?;
            &owned
        };
        let worker = self.worker(&binding.producer_id, reader)?;
        let borrower = {
            let mut state = worker.state.lock().unwrap();
            check_live(&mut state, binding)?;
            if state.native_borrow.is_some() {
                return Err(Failure::busy());
            }
            if state.wire_cache.iter().any(|entry| entry.result.is_none()) {
                return Err(Failure::busy());
            }
            state.native_sequence = state
                .native_sequence
                .checked_add(1)
                .ok_or_else(Failure::stale)?;
            let id = state.native_sequence;
            state.native_borrow = Some(id);
            id
        };
        let mut transport = NativeTransport {
            owner: self.clone(),
            worker,
            binding: binding.clone(),
            borrower,
            temporary,
            finished: false,
        };
        let result = operation(&mut transport);
        let observed = {
            let mut state = transport.worker.state.lock().unwrap();
            check_live(&mut state, binding)
        };
        let cleanup = transport.finish();
        observed?;
        cleanup?;
        Ok(result)
    }
    fn retire_blocking(
        &self,
        producer: Option<&str>,
        next: Option<String>,
        shutdown: bool,
    ) -> Result<(), Failure> {
        let _lifecycle = self.inner.lifecycle.lock().unwrap();
        let workers = {
            let mut state = self.inner.state.lock().unwrap();
            if producer.is_some_and(|p| p != state.producer) {
                return Ok(());
            }
            state.active = false;
            state.stopping = true;
            state.readers.values().cloned().collect::<Vec<_>>()
        };
        for worker in &workers {
            let mut state = worker.state.lock().unwrap();
            state.stopping = true;
            state.closing = true;
            if let Some(lease) = &mut state.lease {
                lease.ending.get_or_insert("released");
            }
            drop(state);
            let _ = worker.sender.try_send(Work::Wake);
        }
        for worker in &workers {
            if let Some(thread) = worker.thread.lock().unwrap().take() {
                if thread.join().is_err() {
                    worker.state.lock().unwrap().close_error = Some(Failure::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "reader_pcsc_worker_failed",
                        "PC/SC worker did not exit normally",
                    ));
                }
            }
            // A failed disconnect remains owned after the worker exits. A
            // later cleanup may confirm it; replacement never opens beside it.
            let mut quarantined = worker.quarantined.lock().unwrap();
            if let Some(physical) = quarantined.as_mut() {
                match physical.close() {
                    Ok(()) => {
                        quarantined.take();
                        let mut state = worker.state.lock().unwrap();
                        state.port_open = false;
                        state.close_error = None;
                    }
                    Err(error) => worker.state.lock().unwrap().close_error = Some(error),
                }
            }
        }
        let failure = workers
            .iter()
            .find_map(|w| w.state.lock().unwrap().close_error.clone());
        let mut state = self.inner.state.lock().unwrap();
        if let Some(error) = failure {
            return Err(error);
        }
        state.readers.clear();
        state.closed_sessions.clear();
        state.retired_operation_leases.clear();
        if let Some(next) = next {
            state.producer = next;
            state.active = true;
            state.stopping = false;
            state.admission_closed = false;
        } else {
            state.stopping = shutdown;
        }
        Ok(())
    }
    pub(crate) async fn replace_producer(&self, producer: String) -> Result<(), Failure> {
        let owner = self.clone();
        match tokio::time::timeout(
            WAIT,
            tokio::task::spawn_blocking(move || owner.retire_blocking(None, None, true)),
        )
        .await
        {
            Ok(Ok(result)) => result?,
            _ => return Err(Failure::unknown()),
        }
        // Publication stays in the waiter. A timeout or cancellation cannot
        // enable a replacement producer after the caller already gave up.
        let mut state = self.inner.state.lock().unwrap();
        if state.final_shutdown {
            return Err(Failure::stopped());
        }
        // replace_producer is called only after the Go child has exited and
        // its bounded physical retirement completed. That verified exit is
        // the alternative to a live Go cleanup acknowledgement.
        state.pending_presence_handoffs.clear();
        state.producer = producer;
        state.active = true;
        state.stopping = false;
        state.admission_closed = false;
        Ok(())
    }
    /// A lost pin reply may hide a physical lease binding. Producer death
    /// alone is insufficient: retirement must have joined every physical
    /// worker and successfully closed quarantined handles. Replacement is
    /// published only after that proof; failed or timed-out retirement keeps
    /// the old producer and reader records here.
    pub(crate) fn producer_physically_idle(&self, producer: &str) -> bool {
        if !valid_token(producer) {
            return false;
        }
        let state = self.inner.state.lock().unwrap();
        state.producer != producer || (!state.active && state.readers.is_empty())
    }
    pub(crate) async fn shutdown(&self) -> Result<(), Failure> {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.admission_closed = true;
            state.final_shutdown = true;
        }
        let mut retirement = self.inner.final_retirement.lock().await;
        if retirement.confirmed {
            return Ok(());
        }
        if retirement.task.is_none() {
            let owner = self.clone();
            retirement.task = Some(tokio::task::spawn_blocking(move || {
                owner.retire_blocking(None, None, true)
            }));
        }
        // Borrow the retained handle: cancelling any waiter drops only its
        // mutex guard. Another waiter observes the same actual retirement.
        // Final cleanup has no request deadline and never aborts physical I/O.
        let result = retirement
            .task
            .as_mut()
            .expect("retirement task installed")
            .await
            .map_err(|_| Failure::unknown())
            .and_then(|result| result);
        retirement.task = None;
        retirement.confirmed = result.is_ok();
        // Failure keeps the owner fence/quarantine; a later call may retry
        // cleanup but cannot publish a replacement or reuse a success receipt.
        result
    }
}

pub(crate) struct NativeTransport {
    owner: Owner,
    worker: Arc<Worker>,
    binding: Binding,
    borrower: u64,
    temporary: bool,
    finished: bool,
}
impl ApduTransport for NativeTransport {
    type Error = Failure;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Failure> {
        self.owner
            .transmit(&self.binding, Some(self.borrower), command.to_vec(), None)
    }
}
impl Drop for NativeTransport {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
impl NativeTransport {
    fn finish(&mut self) -> Result<(), Failure> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let mut state = self.worker.state.lock().unwrap();
        if state.native_borrow == Some(self.borrower) {
            state.native_borrow = None;
        }
        drop(state);
        self.worker.changed.notify_all();
        let _ = self.worker.sender.try_send(Work::Wake);
        if self.temporary {
            self.owner.release(&self.binding)?;
        }
        Ok(())
    }
}

fn closing_session_pending(state: &ReaderState, session: &str, sequence: u64) -> bool {
    state.session == session
        && state.session_seq == sequence
        && (state.closing || state.lease.is_some())
}

fn finish_session_close(
    state: &mut ReaderState,
    session: &str,
    sequence: u64,
) -> Result<(), Failure> {
    // A completed physical close wakes all waiters. A newer session may be
    // published before this older waiter runs; its state belongs to its owner.
    if state.session != session || state.session_seq != sequence {
        return Ok(());
    }
    if let Some(error) = &state.close_error {
        return Err(error.clone());
    }
    state.session.clear();
    Ok(())
}

fn check_session(state: &ReaderState, binding: &Binding) -> Result<(), Failure> {
    if state.close_error.is_some() {
        return Err(state.close_error.as_ref().unwrap().clone());
    }
    if state.stopping
        || state.producer != binding.producer_id
        || state.session != binding.session_id
        || state.closing
    {
        Err(Failure::stale())
    } else {
        Ok(())
    }
}
fn expire(state: &mut ReaderState) {
    if let Some(lease) = &mut state.lease {
        if lease.deadline <= Instant::now() {
            lease.ending.get_or_insert("expired");
        }
    }
}
fn check_live(state: &mut ReaderState, binding: &Binding) -> Result<(), Failure> {
    check_session(state, binding)?;
    expire(state);
    match &state.lease {
        Some(lease) if lease.binding == *binding && lease.acquired && lease.ending.is_none() => {
            Ok(())
        }
        Some(lease) if lease.binding == *binding && lease.ending == Some("expired") => {
            Err(Failure::expired())
        }
        _ => Err(Failure::stale()),
    }
}
fn ack(binding: &Binding) -> Value {
    json!({"accepted":true,"session_id":binding.session_id,"lease_id":binding.lease_id,"lease_seq":binding.lease_seq})
}
fn status_value(binding: &Binding, state: &str, terminal: bool) -> Value {
    json!({"state":state,"terminal":terminal,"session_id":binding.session_id,"lease_id":binding.lease_id,"lease_seq":binding.lease_seq})
}
fn is_estk(reader: &str) -> bool {
    reader.to_ascii_lowercase().contains("estkme")
}
fn admit_wire(
    state: &mut ReaderState,
    seq: u64,
    id: &str,
    fingerprint: &[u8],
) -> Result<Option<Value>, Failure> {
    if seq == 0 || !valid_token(id) {
        return Err(Failure::invalid());
    }
    if let Some(cached) = state.wire_cache.iter().find(|c| c.sequence == seq) {
        if cached.id != id || cached.fingerprint != fingerprint {
            return Err(Failure::stale());
        }
        return cached
            .result
            .clone()
            .map(|r| r.map(Some))
            .unwrap_or_else(|| Err(Failure::busy()));
    }
    if seq <= state.wire_high || state.wire_cache.iter().any(|c| c.id == id) {
        return Err(Failure::stale());
    }
    state.wire_high = seq;
    while state.wire_cache.len() >= CACHE {
        if state.wire_cache.front().is_some_and(|c| c.result.is_none()) {
            return Err(Failure::busy());
        }
        state.wire_cache.pop_front();
    }
    state.wire_cache.push_back(Cached {
        sequence: seq,
        id: id.into(),
        fingerprint: fingerprint.into(),
        result: None,
    });
    Ok(None)
}
fn cache_result(state: &mut ReaderState, seq: u64, id: &str, result: Result<Value, Failure>) {
    if let Some(cached) = state
        .wire_cache
        .iter_mut()
        .find(|c| c.sequence == seq && c.id == id)
    {
        cached.result = Some(result);
    }
}
fn decode_response(value: &Value) -> Result<Vec<u8>, Failure> {
    value
        .get("response_hex")
        .and_then(Value::as_str)
        .ok_or_else(Failure::invalid)
        .and_then(decode_hex)
}
fn decode_hex(value: &str) -> Result<Vec<u8>, Failure> {
    if value.len() % 2 != 0 || value.len() > 131096 {
        return Err(Failure::invalid());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let h = (p[0] as char).to_digit(16).ok_or_else(Failure::invalid)?;
            let l = (p[1] as char).to_digit(16).ok_or_else(Failure::invalid)?;
            Ok(((h << 4) | l) as u8)
        })
        .collect()
}
fn encode_hex(value: &[u8]) -> String {
    const H: &[u8] = b"0123456789ABCDEF";
    let mut result = String::with_capacity(value.len() * 2);
    for byte in value {
        result.push(H[(byte >> 4) as usize] as char);
        result.push(H[(byte & 15) as usize] as char);
    }
    result
}

fn finish_lease(worker: &Worker) {
    let mut state = worker.state.lock().unwrap();
    if let Some(lease) = state.lease.take() {
        if !lease.internal {
            state.lease_high = state.lease_high.max(lease.binding.lease_seq);
        }
        state.last_terminal = Some((lease.binding, lease.ending.unwrap_or("released")));
    }
    state.wire_cache.clear();
    worker.changed.notify_all();
}
fn close_port(worker: &Worker, port: &mut Option<Box<dyn Physical>>) -> Result<(), Failure> {
    let result = if let Some(physical) = port.as_mut() {
        physical.close()
    } else {
        Ok(())
    };
    let mut state = worker.state.lock().unwrap();
    if state.card_workflow.is_none() {
        state.closing = false;
    }
    match result {
        Ok(()) => {
            port.take();
            state.port_open = false;
            state.close_error = None;
            worker.changed.notify_all();
            Ok(())
        }
        Err(error) => {
            state.port_open = true;
            state.close_error = Some(error.clone());
            worker.changed.notify_all();
            Err(error)
        }
    }
}
fn reset_port(
    worker: &Worker,
    inner: &Inner,
    port: &mut Option<Box<dyn Physical>>,
    power: bool,
) -> Result<(), Failure> {
    close_port(worker, port)?;
    *port = Some((inner.reset)(&worker.reader, power)?);
    worker.state.lock().unwrap().port_open = true;
    close_port(worker, port)
}

fn run_worker(worker_ref: Weak<Worker>, receiver: mpsc::Receiver<Work>, inner_ref: Weak<Inner>) {
    let mut port: Option<Box<dyn Physical>> = None;
    let mut last_revive: Option<Instant> = None;
    loop {
        let Some(worker) = worker_ref.upgrade() else {
            break;
        };
        let Some(inner) = inner_ref.upgrade() else {
            break;
        };
        {
            let state = worker.state.lock().unwrap();
            if (state.stopping || state.closing)
                && state.lease.is_none()
                && state.native_borrow.is_none()
            {
                let stopping = state.stopping;
                drop(state);
                let _ = close_port(&worker, &mut port);
                if stopping {
                    break;
                }
            }
        }
        let work = match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(work) => work,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        match work {
            Work::Acquire {
                binding,
                reply,
                admission,
            } => {
                let admitted = || admission.as_ref().is_none_or(|guard| guard());
                let admitted_before_open = admitted();
                let allowed = {
                    let mut state = worker.state.lock().unwrap();
                    expire(&mut state);
                    check_session(&state, &binding).is_ok()
                        && state
                            .lease
                            .as_ref()
                            .is_some_and(|l| l.binding == binding && l.ending.is_none())
                };
                if !allowed || !admitted_before_open {
                    let _ = reply.send(Err(Failure::stale()));
                    finish_lease(&worker);
                    continue;
                }
                if port.is_none() {
                    let mut opened = (inner.factory)(&worker.reader);
                    let card_is_unresponsive = opened
                        .as_ref()
                        .is_err_and(|error| error.code == "reader_pcsc_unresponsive_card");
                    if card_is_unresponsive {
                        let remaining = last_revive
                            .and_then(|at| revival_cooldown_remaining(at, Instant::now()));
                        if let Some(remaining) = remaining {
                            opened = Err(Failure::recovery_cooldown(remaining));
                        } else if admitted() {
                            last_revive = Some(Instant::now());
                            if reset_port(&worker, &inner, &mut port, false).is_ok() {
                                thread::sleep(Duration::from_millis(300));
                                let allowed = {
                                    let mut s = worker.state.lock().unwrap();
                                    expire(&mut s);
                                    s.lease
                                        .as_ref()
                                        .is_some_and(|l| l.binding == binding && l.ending.is_none())
                                        && !s.stopping
                                };
                                if allowed && admitted() {
                                    opened = (inner.factory)(&worker.reader);
                                }
                            }
                        }
                    }
                    match opened {
                        Ok(value) => {
                            port = Some(value);
                            worker.state.lock().unwrap().port_open = true;
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            finish_lease(&worker);
                            continue;
                        }
                    }
                }
                let mut acquire_reply = Some(reply);
                let mut reset_reply: Option<(Option<u64>, u64, String, Reply<Value>)> = None;
                'leased: loop {
                    // A slow connect/reset may have completed after release,
                    // expiry or producer retirement revoked the reservation.
                    // Do not begin another OS transaction in that case.
                    let permitted = {
                        let mut state = worker.state.lock().unwrap();
                        expire(&mut state);
                        card_workflow::recovering(&state, &binding, state.native_borrow)
                            || !state.stopping
                                && !state.closing
                                && state.close_error.is_none()
                                && state.lease.as_ref().is_some_and(|lease| {
                                    lease.binding == binding && lease.ending.is_none()
                                })
                    };
                    // Only initial acquisition is bound to Preparing. Once
                    // ACKed, reset/reopen remains under the active fixed lease.
                    let initially_admitted = acquire_reply.is_none() || admitted();
                    if !permitted || !initially_admitted {
                        if let Some(reply) = acquire_reply.take() {
                            let _ = reply.send(Err(Failure::stale()));
                        }
                        if let Some((_native, seq, id, reply)) = reset_reply.take() {
                            let error = Failure::unknown();
                            cache_result(
                                &mut worker.state.lock().unwrap(),
                                seq,
                                &id,
                                Err(error.clone()),
                            );
                            let _ = reply.send(Err(error));
                        }
                        break;
                    }
                    let mut outcome = ExitTransaction::Released;
                    let result=port.as_mut().unwrap().transaction(&mut |transport|{
                        {let mut state=worker.state.lock().unwrap();expire(&mut state);if !card_workflow::recovering(&state,&binding,state.native_borrow)&&(state.stopping||state.closing||state.lease.as_ref().is_none_or(|l|l.binding!=binding||l.ending.is_some())){if let Some(reply)=acquire_reply.take(){let _=reply.send(Err(Failure::stale()));}return;}state.lease.as_mut().unwrap().acquired=true;}
                        if let Some(reply)=acquire_reply.take(){let _=reply.send(Ok(ack(&binding)));}
                        if let Some((_native,seq,id,reply))=reset_reply.take(){let value=ack(&binding);cache_result(&mut worker.state.lock().unwrap(),seq,&id,Ok(value.clone()));let _=reply.send(Ok(value));}
                        loop{
                            {let mut state=worker.state.lock().unwrap();expire(&mut state);let ending=state.stopping||state.closing||state.lease.as_ref().is_none_or(|l|l.binding!=binding||l.ending.is_some());if ending&&state.native_borrow.is_none(){break;}}
                            let work=match receiver.recv_timeout(Duration::from_millis(25)){Ok(work)=>work,Err(mpsc::RecvTimeoutError::Timeout)=>continue,Err(_)=>break};
                            match work{
                                Work::Wake=>{},
                                Work::Acquire{reply,..}=>{let _=reply.send(Err(Failure::busy()));},
                                Work::Transmit{binding:request,native,command,wire,reply}=>{
                                    let allowed={let mut state=worker.state.lock().unwrap();card_workflow::live(&mut state,&request,native).and_then(|_|if state.native_borrow==native{Ok(())}else{Err(Failure::busy())})};
                                    let result=allowed.and_then(|_|{let result=transport.transmit(&command);let mut state=worker.state.lock().unwrap();if card_workflow::identity(&state,&request,native).is_err()&&check_live(&mut state,&request).is_err(){return Err(Failure::unknown());}result});
                                    let fatal=result.as_ref().err().is_some_and(|e|e.submitted||e.uncertain);
                                    if let Some((seq,id))=&wire{let value=result.as_ref().map(|bytes|json!({"response_hex":encode_hex(bytes),"session_id":request.session_id,"operation_id":id,"operation_seq":seq})).map_err(Clone::clone);cache_result(&mut worker.state.lock().unwrap(),*seq,id,value);}
                                    let error=result.as_ref().err().cloned();let _=reply.send(result);
                                    if fatal{let mut state=worker.state.lock().unwrap();if let Some(lease)=&mut state.lease{lease.ending.get_or_insert("released");}drop(state);outcome=ExitTransaction::Failed(error.unwrap());}
                                    // A borrower may still be unwinding its protocol closure.
                                    if fatal && !card_workflow::recovering(&worker.state.lock().unwrap(),&request,native){loop{let state=worker.state.lock().unwrap();if state.native_borrow.is_none(){break;}drop(worker.changed.wait(state).unwrap());}break;}
                                },
                                Work::Reset{binding:request,native,action,sequence,id,reply}=>{
                                    let valid={let mut state=worker.state.lock().unwrap();card_workflow::live(&mut state,&request,native).and_then(|_|if state.native_borrow==native{Ok(())}else{Err(Failure::busy())})};
                                    if let Err(error)=valid{cache_result(&mut worker.state.lock().unwrap(),sequence,&id,Err(error.clone()));let _=reply.send(Err(error));}else{outcome=ExitTransaction::Reset(action,native,sequence,id,reply);break;}
                                },
                            }
                        }
                    });
                    if let Err(error) = result {
                        if let Some(reply) = acquire_reply.take() {
                            let _ = reply.send(Err(error.clone()));
                        }
                        if let Some((_native, seq, id, reply)) = reset_reply.take() {
                            cache_result(
                                &mut worker.state.lock().unwrap(),
                                seq,
                                &id,
                                Err(error.clone()),
                            );
                            let _ = reply.send(Err(error.clone()));
                        }
                        if !matches!(outcome, ExitTransaction::Reset(_, Some(_), ..)) {
                            outcome = ExitTransaction::Failed(error);
                        }
                    }
                    loop {
                        match outcome {
                            ExitTransaction::Reset(
                                mut action,
                                mut native,
                                mut seq,
                                mut id,
                                mut reply,
                            ) => loop {
                                let result: Result<(), Failure> = (|| {
                                    close_port(&worker, &mut port)?;
                                    let mut state = worker.state.lock().unwrap();
                                    card_workflow::live(&mut state, &binding, native)?;
                                    drop(state);
                                    if action == ResetAction::Reset {
                                        reset_port(&worker, &inner, &mut port, true)?;
                                    }
                                    let mut state = worker.state.lock().unwrap();
                                    card_workflow::live(&mut state, &binding, native)?;
                                    drop(state);
                                    port = Some((inner.factory)(&worker.reader)?);
                                    worker.state.lock().unwrap().port_open = true;
                                    Ok(())
                                })(
                                );
                                match result {
                                    Ok(()) => {
                                        reset_reply = Some((native, seq, id, reply));
                                        continue 'leased;
                                    }
                                    Err(mut error) => {
                                        error.submitted = true;
                                        error.uncertain = true;
                                        cache_result(
                                            &mut worker.state.lock().unwrap(),
                                            seq,
                                            &id,
                                            Err(error.clone()),
                                        );
                                        let _ = reply.send(Err(error.clone()));
                                        let Some(next) = card_workflow::wait_reset(
                                            &worker, &receiver, &binding, error,
                                        ) else {
                                            break 'leased;
                                        };
                                        (action, native, seq, id, reply) = next;
                                    }
                                }
                            },
                            ExitTransaction::Failed(error) => {
                                if let Some((action, native, seq, id, reply)) =
                                    card_workflow::wait_reset(&worker, &receiver, &binding, error)
                                {
                                    outcome =
                                        ExitTransaction::Reset(action, native, seq, id, reply);
                                    continue;
                                }
                                let _ = close_port(&worker, &mut port);
                                break 'leased;
                            }
                            ExitTransaction::Released => break 'leased,
                        }
                    }
                }
                // A deadline/retirement can win between reconnect and entry
                // into its transaction. Unblock the reset caller before waiting
                // for its retained workflow guard to leave.
                if let Some((_native, seq, id, reply)) = reset_reply.take() {
                    let error = Failure::unknown();
                    cache_result(
                        &mut worker.state.lock().unwrap(),
                        seq,
                        &id,
                        Err(error.clone()),
                    );
                    let _ = reply.send(Err(error));
                }
                {
                    let mut state = worker.state.lock().unwrap();
                    while state.native_borrow.is_some() {
                        state = worker.changed.wait(state).unwrap();
                    }
                }
                let closing = {
                    let state = worker.state.lock().unwrap();
                    state.closing
                        || state.stopping
                        || is_estk(&worker.reader)
                        || state
                            .lease
                            .as_ref()
                            .is_some_and(|l| l.ending == Some("expired"))
                };
                if closing {
                    let _ = close_port(&worker, &mut port);
                }
                finish_lease(&worker);
            }
            Work::Transmit { reply, .. } => {
                let _ = reply.send(Err(Failure::stale()));
            }
            Work::Reset { reply, .. } => {
                let _ = reply.send(Err(Failure::stale()));
            }
            Work::Wake => {}
        }
    }
    if let Some(worker) = worker_ref.upgrade() {
        if close_port(&worker, &mut port).is_err() {
            *worker.quarantined.lock().unwrap() = port.take();
        }
        finish_lease(&worker);
    } else if let Some(mut physical) = port.take() {
        // The last Owner can disappear without an explicit async shutdown.
        // There is no receipt consumer left, but the physical handle must still
        // receive its best-effort close before the worker exits.
        let _ = physical.close();
    }
}

/// Borrow an explicitly admitted workflow without trying to acquire a second
/// ordinary reader reservation. The permit lives in the physical closure.
pub(crate) async fn run_card_work<F, T>(
    owner: Owner,
    gate: Arc<crate::reader_reservation::Gate>,
    reader: &str,
    transaction: Option<&crate::reader_reservation::CardTransaction>,
    pcsc: Option<&Binding>,
    operation: F,
    deadline: Duration,
) -> Result<T, crate::reader_task::ReaderTaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    use crate::reader_task::ReaderTaskError;
    match (transaction, pcsc) {
        (None, None) => crate::reader_task::run_with_gate(gate, reader, operation, deadline).await,
        (Some(transaction), Some(binding)) => {
            binding
                .validate_for_reader(reader)
                .map_err(|_| ReaderTaskError::Busy)?;
            let permit = gate
                .track_card_io(transaction, &binding.producer_id, reader)
                .map_err(|_| ReaderTaskError::Busy)?;
            gate.register_card_pcsc(transaction, binding)
                .map_err(|_| ReaderTaskError::Busy)?;
            tokio::time::timeout(
                deadline,
                crate::reader_task::run_in_pool(&owner.inner.normal_pool, Some(permit), operation),
            )
            .await
            .map_err(|_| ReaderTaskError::Timeout)?
        }
        _ => Err(ReaderTaskError::Busy),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod legacy_wire_oracle;
#[cfg(test)]
pub(crate) use legacy_wire_oracle::handle as handle_legacy_wire;
