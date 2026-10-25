//! File locking and atomic archive writes must not occupy async I/O workers.
//! A registration retires only after job captures, its observer, and its output
//! have settled. This is an I/O lifetime proof, not a business dispatch grant.
use proletarius_store::StoreError;
use std::fmt::{self, Write};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tokio::sync::Notify;

const ARCHIVE_WORKERS: usize = 8;
const MAX_ORPHAN_ERRORS: usize = 64;
const MAX_ERROR_BYTES: usize = 2_048;
const JOB_DONE: u8 = 1;
const OBSERVER_DONE: u8 = 2;
const OUTPUT_DONE: u8 = 4;
const ALL_DONE: u8 = JOB_DONE | OBSERVER_DONE | OUTPUT_DONE;
static EXECUTOR: OnceLock<Executor> = OnceLock::new();

// Both entry points are synchronous: even a never-polled future already owns
// a named request and a registered, explicitly ordered input destructor.
pub(crate) fn run<T, F>(
    operation: F,
) -> impl Future<Output = Result<T, StoreError>> + Send + 'static + use<T, F>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    EXECUTOR
        .get_or_init(|| Executor::new(ARCHIVE_WORKERS))
        .run(operation)
}

/// Keep execution alive after caller cancellation without marking an inner
/// detached task as the result observer. The original caller owns Waiter; the
/// channel retains IoAck/OutputWitness until that caller consumes or drops it.
/// Genuine unobserved failures therefore retire into this same sticky ledger.
pub(crate) fn run_durable<T, F, G>(
    operation: F,
    retained: G,
) -> impl Future<Output = Result<T, StoreError>> + Send + 'static + use<T, F, G>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    G: Send + 'static,
{
    EXECUTOR
        .get_or_init(|| Executor::new(ARCHIVE_WORKERS))
        .run_durable(operation, retained)
}

/// Intentional fault injection uses the same executor lifecycle with a private
/// ledger, so a synthetic panic cannot poison unrelated daemon-wide drains.
#[cfg(test)]
pub(crate) struct IsolatedTestExecutor(Executor);
#[cfg(test)]
impl IsolatedTestExecutor {
    pub(crate) fn new() -> Self {
        Self(Executor::new(1))
    }
    pub(crate) fn run<T, F>(
        &self,
        operation: F,
    ) -> impl Future<Output = Result<T, StoreError>> + Send + 'static + use<T, F>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    {
        self.0.run(operation)
    }
    pub(crate) fn run_durable<T, F, G>(
        &self,
        operation: F,
        retained: G,
    ) -> impl Future<Output = Result<T, StoreError>> + Send + 'static + use<T, F, G>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StoreError> + Send + 'static,
        G: Send + 'static,
    {
        self.0.run_durable(operation, retained)
    }
    pub(crate) async fn finish_pending(&self) -> Result<(), String> {
        self.0.ledger.finish_pending().await
    }
}

/// Observe registered work after external producers stop. This does not close
/// the eight-slot resource pool or prevent explicitly awaited final flushes.
/// Delivered business errors are clean; orphan errors remain repeatably visible.
/// Unwind/accounting uncertainty is sticky even if the caller saw an error.
pub(crate) async fn finish_pending() -> Result<(), String> {
    match EXECUTOR.get() {
        Some(executor) => executor.ledger.finish_pending().await,
        None => Ok(()),
    }
}

struct Executor {
    pool: crate::blocking_pool::BlockingPool,
    ledger: Arc<Ledger>,
    #[cfg(test)]
    last: Mutex<std::sync::Weak<Completion>>,
}
impl Executor {
    fn new(concurrency: usize) -> Self {
        Self {
            pool: crate::blocking_pool::BlockingPool::new(concurrency),
            ledger: Arc::new(Ledger::default()),
            #[cfg(test)]
            last: Mutex::new(std::sync::Weak::new()),
        }
    }

    fn run<T, F>(
        &self,
        operation: F,
    ) -> impl Future<Output = Result<T, StoreError>> + Send + 'static + use<T, F>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    {
        let mut request = Request::new(self.pool.clone(), self.ledger.clone(), operation);
        #[cfg(test)]
        {
            *self.last.lock().unwrap() = Arc::downgrade(&request.completion);
        }
        async move {
            if !request.completion.armed.load(Ordering::Acquire) {
                // Admission failed; dispose F before returning the refusal.
                drop(request.job.take());
                return Err(worker_error("archive registration unavailable"));
            }
            let job = request.job.take().expect("request owns its job");
            let pool = request.pool.clone();
            request.running = Some(Box::pin(async move {
                // The only user payload capture is the named Job. A queued
                // abort drops its F before JobWitness publishes job_done.
                pool.run(move || job.execute()).await
            }));
            let result = request.running.as_mut().expect("submitted job").await;
            drop(request.running.take());
            let result = match result {
                Ok(ack) => ack.into_result(),
                Err(_) => Err(worker_error("archive worker failed")),
            };
            // No await follows consumption. Request's synchronous Drop records
            // live delivery in the same poll that transfers this result.
            request.waiter.as_mut().expect("request observer").live = true;
            result
        }
    }

    fn run_durable<T, F, G>(
        &self,
        operation: F,
        retained: G,
    ) -> impl Future<Output = Result<T, StoreError>> + Send + 'static + use<T, F, G>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StoreError> + Send + 'static,
        G: Send + 'static,
    {
        // Registration is synchronous and uses the same retained Completion
        // as normal archive I/O; there is no second owner/drain enrollment gap.
        let mut request: Request<F, T> =
            Request::new(self.pool.clone(), self.ledger.clone(), operation);
        #[cfg(test)]
        {
            *self.last.lock().unwrap() = Arc::downgrade(&request.completion);
        }
        // The owner travels with Job, then IoAck, and is destroyed before
        // the corresponding final witness. A finished blocking closure alone
        // cannot free quota while its unobserved output still exists.
        request.job.as_mut().expect("request owns its job").retained = Some(Box::new(retained));
        let runtime = tokio::runtime::Handle::try_current().ok();
        let unavailable = if runtime.is_none() {
            "archive durable runtime unavailable"
        } else {
            "archive registration unavailable"
        };
        let receiver = if request.completion.armed.load(Ordering::Acquire) {
            runtime.map(|runtime| {
                let mut job = request.job.take().expect("request owns its job");
                job.redacted_error = true;
                let pool = request.pool.clone();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                // Dropping this task handle detaches only execution. Never
                // consume IoAck here: send success can still precede a later
                // receiver cancellation, and its output witness must survive.
                #[cfg(test)]
                let completion = request.completion.clone();
                runtime.spawn(async move {
                    let result = pool.run(move || job.execute()).await;
                    let sent = sender.send(result);
                    #[cfg(test)]
                    if sent.is_ok() {
                        completion.forwarded.store(true, Ordering::Release);
                    }
                    drop(sent);
                });
                receiver
            })
        } else {
            None
        };
        if receiver.is_none() {
            // No runtime/registration means no worker may use the input.
            // Dispose captures before the caller's observer can retire.
            drop(request.job.take());
        }
        async move {
            let result = match receiver {
                Some(receiver) => match receiver.await {
                    Ok(Ok(ack)) => ack.into_result(),
                    Ok(Err(_)) | Err(_) => Err(worker_error("archive durable worker failed")),
                },
                None => Err(worker_error(unavailable)),
            };
            // This is the actual caller acknowledgement, in the same poll as
            // returning Result. Cancellation before this point leaves Waiter
            // orphaned; IoAck destruction then retains real errors for drains.
            request.waiter.as_mut().expect("request observer").live = true;
            result
        }
    }

    #[cfg(test)]
    fn completion(&self) -> Arc<Completion> {
        self.last.lock().unwrap().upgrade().unwrap()
    }
}

fn worker_error(message: &'static str) -> StoreError {
    StoreError::Io(std::io::Error::other(message))
}

type PoolFuture<T> =
    Pin<Box<dyn Future<Output = Result<IoAck<T>, tokio::task::JoinError>> + Send + 'static>>;

// Declaration order is also the unwind fallback order. Custom Drop takes each
// payload before dropping it; if it panics, remaining fields still unwind in
// this order, with the observer last among the resource owners.
struct Request<F, T> {
    running: Option<PoolFuture<T>>,
    job: Option<Job<F>>,
    waiter: Option<Waiter>,
    completion: Arc<Completion>,
    pool: crate::blocking_pool::BlockingPool,
}
impl<F, T> Request<F, T> {
    fn new(pool: crate::blocking_pool::BlockingPool, ledger: Arc<Ledger>, operation: F) -> Self {
        // Allocate all envelopes before checked enrollment. The ticket starts
        // unarmed: a refused registration must never decrement someone else.
        let completion = Arc::new(Completion::new(ledger));
        let request = Self {
            running: None,
            job: Some(Job {
                operation: Some(operation),
                redacted_error: false,
                retained: None,
                witness: JobWitness {
                    completion: completion.clone(),
                    started: false,
                    returned: false,
                    output_created: false,
                },
            }),
            waiter: Some(Waiter {
                completion: completion.clone(),
                live: false,
            }),
            completion,
            pool,
        };
        request.completion.arm();
        request
    }
}
impl<F, T> Drop for Request<F, T> {
    fn drop(&mut self) {
        drop(self.running.take());
        drop(self.job.take());
        drop(self.waiter.take());
    }
}

struct Job<F> {
    operation: Option<F>,
    redacted_error: bool,
    retained: Option<Box<dyn Send>>,
    // Last field is the final owned-lifetime witness, including F::Drop unwind.
    witness: JobWitness,
}
impl<F> Job<F> {
    fn execute<T>(mut self) -> IoAck<T>
    where
        F: FnOnce() -> Result<T, StoreError>,
    {
        self.witness.started = true;
        let result = self.operation.take().expect("one archive operation")();
        if let Err(error) = &result {
            let (message, valid) = if self.redacted_error {
                // No path, caller identity, body or arbitrary Display text is
                // retained by the durable owner. The live caller gets the
                // original StoreError; drains keep a fixed bounded diagnostic.
                ("durable archive persistence failed".to_owned(), true)
            } else {
                bounded_error(error)
            };
            if !valid {
                self.witness.completion.ledger.mark_unconfirmed();
            }
            self.witness.completion.set_error(message);
        }
        // An owning ACK exists before job_done. Output was Unresolved from
        // enrollment, so cancelled observers cannot retire an unconsumed T.
        let ack = IoAck {
            result: Some(result),
            retained: self.retained.take(),
            _witness: OutputWitness {
                completion: self.witness.completion.clone(),
            },
        };
        self.witness.output_created = true;
        self.witness.returned = true;
        drop(self);
        ack
    }
}
impl<F> Drop for Job<F> {
    fn drop(&mut self) {
        drop(self.operation.take());
        drop(self.retained.take());
    }
}
struct JobWitness {
    completion: Arc<Completion>,
    started: bool,
    returned: bool,
    output_created: bool,
}
impl Drop for JobWitness {
    fn drop(&mut self) {
        if std::thread::panicking() || (self.started && !self.returned) {
            self.completion.ledger.mark_unconfirmed();
        }
        // Only this last witness can prove no output will ever be produced.
        if !self.output_created {
            self.completion.mark(OUTPUT_DONE);
        }
        self.completion.mark(JOB_DONE);
    }
}

struct IoAck<T> {
    result: Option<Result<T, StoreError>>,
    retained: Option<Box<dyn Send>>,
    // After normal payload Drop or its normal Rust unwind field cleanup.
    _witness: OutputWitness,
}
impl<T> IoAck<T> {
    fn into_result(mut self) -> Result<T, StoreError> {
        self.result.take().expect("one live result consumption")
    }
}
impl<T> Drop for IoAck<T> {
    fn drop(&mut self) {
        drop(self.result.take());
        drop(self.retained.take());
    }
}
struct OutputWitness {
    completion: Arc<Completion>,
}
impl Drop for OutputWitness {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.completion.ledger.mark_unconfirmed();
        }
        self.completion.mark(OUTPUT_DONE);
    }
}

struct Waiter {
    completion: Arc<Completion>,
    live: bool,
}
impl Drop for Waiter {
    fn drop(&mut self) {
        self.completion
            .observer
            .store(if self.live { 1 } else { 2 }, Ordering::Release);
        self.completion.mark(OBSERVER_DONE);
    }
}

struct Ledger {
    // Actual count is independent of all diagnostic mutexes. Poison cannot
    // make a still-running job or held ready output disappear from this count.
    pending: AtomicUsize,
    accounting_failed: AtomicBool,
    unconfirmed: AtomicBool,
    diagnostics: Mutex<Diagnostics>,
    changed: Notify,
}
struct Diagnostics {
    orphan_errors: Vec<String>,
    omitted_errors: usize,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            pending: AtomicUsize::new(0),
            accounting_failed: AtomicBool::new(false),
            unconfirmed: AtomicBool::new(false),
            diagnostics: Mutex::new(Diagnostics {
                orphan_errors: Vec::with_capacity(MAX_ORPHAN_ERRORS),
                omitted_errors: 0,
            }),
            changed: Notify::new(),
        }
    }
}
impl Ledger {
    fn mark_unconfirmed(&self) {
        self.unconfirmed.store(true, Ordering::Release);
    }

    fn diagnostics(&self) -> MutexGuard<'_, Diagnostics> {
        self.diagnostics.lock().unwrap_or_else(|error| {
            self.mark_unconfirmed();
            error.into_inner()
        })
    }

    fn retire(&self, error: Option<String>) {
        if let Some(error) = error {
            let mut diagnostics = self.diagnostics();
            if diagnostics.orphan_errors.len() < MAX_ORPHAN_ERRORS {
                diagnostics.orphan_errors.push(error);
            } else {
                diagnostics.omitted_errors = diagnostics.omitted_errors.saturating_add(1);
            }
        }
        if self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            })
            .is_err()
        {
            self.accounting_failed.store(true, Ordering::Release);
            self.mark_unconfirmed();
        }
        self.changed.notify_waiters();
    }

    async fn finish_pending(&self) -> Result<(), String> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.pending.load(Ordering::Acquire) == 0 {
                let diagnostics = self.diagnostics();
                let mut errors = Vec::new();
                if self.unconfirmed.load(Ordering::Acquire) {
                    errors.push(
                        "archive completion unconfirmed: archive worker panicked, resource destruction or accounting uncertain"
                            .to_owned(),
                    );
                }
                if !diagnostics.orphan_errors.is_empty() || diagnostics.omitted_errors != 0 {
                    let mut orphan = diagnostics.orphan_errors.join("; ");
                    if diagnostics.omitted_errors != 0 {
                        let qualifier = if diagnostics.omitted_errors == usize::MAX {
                            "at least "
                        } else {
                            ""
                        };
                        write!(
                            orphan,
                            "; {qualifier}{} further orphan archive failures",
                            diagnostics.omitted_errors
                        )
                        .expect("String formatting");
                    }
                    errors.push(format!("undelivered archive worker failure(s): {orphan}"));
                }
                return if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                };
            }
            changed.await;
        }
    }
}

struct Completion {
    ledger: Arc<Ledger>,
    #[cfg(test)]
    forwarded: AtomicBool,
    armed: AtomicBool,
    facts: AtomicU8,
    // 0 unresolved, 1 transferred live, 2 observer cancelled.
    observer: AtomicU8,
    settled: AtomicBool,
    error: Mutex<Option<String>>,
}
impl Completion {
    fn new(ledger: Arc<Ledger>) -> Self {
        Self {
            ledger,
            #[cfg(test)]
            forwarded: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            facts: AtomicU8::new(0),
            observer: AtomicU8::new(0),
            settled: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }

    fn arm(&self) {
        if self.ledger.accounting_failed.load(Ordering::Acquire) {
            return;
        }
        if self
            .ledger
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .is_err()
        {
            self.ledger.accounting_failed.store(true, Ordering::Release);
            self.ledger.mark_unconfirmed();
            return;
        }
        // No allocation, await, lock or user code after count commit before
        // the preconstructed request receives its armed ticket.
        self.armed.store(true, Ordering::Release);
    }

    fn error(&self) -> MutexGuard<'_, Option<String>> {
        self.error.lock().unwrap_or_else(|error| {
            self.ledger.mark_unconfirmed();
            error.into_inner()
        })
    }

    fn set_error(&self, message: String) {
        *self.error() = Some(message);
    }

    fn mark(&self, fact: u8) {
        self.facts.fetch_or(fact, Ordering::AcqRel);
        if !self.armed.load(Ordering::Acquire)
            || self.facts.load(Ordering::Acquire) != ALL_DONE
            || self
                .settled
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        // Recover diagnostic poison without discarding an independently known
        // lifecycle fact. Error recording precedes the final count decrement.
        let error = self.error().take();
        self.ledger.retire(
            (self.observer.load(Ordering::Acquire) == 2)
                .then_some(error)
                .flatten(),
        );
    }
}

// Bound formatting itself, rather than allocating the entire Display text and
// truncating afterwards. This changes only retained shutdown diagnostics.
fn bounded_error(error: &StoreError) -> (String, bool) {
    const SUFFIX: &str = "...[truncated]";
    struct Bounded {
        text: String,
        truncated: bool,
    }
    impl Write for Bounded {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            let capacity = MAX_ERROR_BYTES - SUFFIX.len() - self.text.len();
            let mut count = text.len().min(capacity);
            while !text.is_char_boundary(count) {
                count -= 1;
            }
            self.text.push_str(&text[..count]);
            self.truncated |= count != text.len();
            Ok(())
        }
    }
    let mut output = Bounded {
        text: String::with_capacity(MAX_ERROR_BYTES),
        truncated: false,
    };
    let valid = write!(&mut output, "{error}").is_ok();
    if output.truncated {
        output.text.push_str(SUFFIX);
    }
    (output.text, valid)
}

#[cfg(test)]
mod tests;
