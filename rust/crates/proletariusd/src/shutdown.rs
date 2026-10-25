//! Final daemon teardown. A reporting failure and a physical release receipt
//! have different meanings: only the latter permits dependent transport close.
use std::future::Future;
use std::time::Duration;

use proletarius_domain::BackendFailure;
use tokio::net::UnixStream;
use tokio::task::JoinSet;

/// Stop archive-producing cleanup requests before the final persistence
/// barrier. Existing handlers retain their permit through the actual result.
#[derive(Clone, Default)]
pub(crate) struct CleanupGate(std::sync::Arc<tokio::sync::RwLock<bool>>);

impl CleanupGate {
    pub(crate) async fn enter(&self) -> Option<tokio::sync::OwnedRwLockReadGuard<bool>> {
        let permit = self.0.clone().read_owned().await;
        if *permit { None } else { Some(permit) }
    }

    pub(crate) async fn close(&self) {
        *self.0.write().await = true;
    }
}

#[derive(Debug, Default)]
pub(crate) struct Report {
    failures: Vec<String>,
}

impl Report {
    pub(crate) fn record(&mut self, owner: &str, error: impl std::fmt::Display) {
        let failure = format!("{owner}: {error}");
        if !self.failures.contains(&failure) {
            crate::diagnostics::write_line(format_args!("switchd-rs shutdown: {failure}"));
            // A permanently failing listener must not grow an unbounded error
            // archive while a physical receipt is still outstanding.
            if self.failures.len() < 128 {
                self.failures.push(failure);
            }
        }
    }

    pub(crate) fn check<E: std::fmt::Display>(&mut self, owner: &str, result: Result<(), E>) {
        if let Err(error) = result {
            self.record(owner, error);
        }
    }

    pub(crate) fn extend(&mut self, other: Self) {
        for failure in other.failures {
            if !self.failures.contains(&failure) {
                self.failures.push(failure);
            }
        }
    }

    pub(crate) fn finish(self) -> Result<(), BackendFailure> {
        if self.failures.is_empty() {
            Ok(())
        } else {
            Err(BackendFailure::Io(format!(
                "native shutdown reported {} failure(s): {}",
                self.failures.len(),
                self.failures.join("; ")
            )))
        }
    }
}

/// The caller supplies an owner-aware receipt/reconciliation operation, never
/// an arbitrary physical write. Cached unknown outcomes remain fenced.
pub(crate) async fn confirmed<F, Fut>(owner: &str, mut receipt: F) -> Report
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Vec<String>, String>>,
{
    let mut report = Report::default();
    loop {
        match receipt().await {
            Ok(errors) => {
                for error in errors {
                    report.record(owner, error);
                }
                return report;
            }
            Err(error) => {
                report.record(owner, error);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// For an operation with no proven reconciliation API, run it once. Failure
/// retains this branch indefinitely; it must not be retried as a new mutation
/// or converted into success merely because its handle has been consumed.
pub(crate) async fn once(owner: &str, result: Result<(), String>) -> Report {
    let mut report = Report::default();
    if let Err(error) = result {
        report.record(owner, error);
        std::future::pending::<()>().await;
    }
    report
}

/// Real production ordering: shared native sessions/card restoration first,
/// then independent module/reader/QMI branches, then their final archive drain.
/// Errors after confirmed release never short-circuit the remaining owners.
pub(crate) async fn coordinate<S, M, R, Q, A>(
    mut report: Report,
    shared: S,
    module: M,
    reader: R,
    qmi: Q,
    archive: A,
) -> Report
where
    S: Future<Output = Report>,
    M: Future<Output = Report>,
    R: Future<Output = Report>,
    Q: Future<Output = Report>,
    A: Future<Output = Report>,
{
    report.extend(shared.await);
    let (module, reader, qmi) = tokio::join!(module, reader, qmi);
    report.extend(module);
    report.extend(reader);
    report.extend(qmi);
    report.extend(archive.await);
    report
}

/// Used by the live accept loop: an accept fault is an exit cause, never an
/// early return that bypasses the coordinator.
pub(crate) fn accepted<T>(
    report: &mut Report,
    listener: &str,
    result: std::io::Result<T>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            report.record(listener, error);
            None
        }
    }
}

/// Keep the owner's future pinned through listener failures, and retry only
/// the failed listener with a bounded rate. Other release paths stay live.
pub(crate) async fn drive_callbacks<T, S, C, I, CA, IA, CF, IF, CH, IH>(
    shutdown: S,
    mut callback_accept: C,
    mut internal_accept: I,
    callback_handler: CH,
    internal_handler: IH,
    limit: usize,
) -> (T, Report)
where
    S: Future<Output = T>,
    C: FnMut() -> CA,
    I: FnMut() -> IA,
    CA: Future<Output = std::io::Result<UnixStream>>,
    IA: Future<Output = std::io::Result<UnixStream>>,
    CH: Fn(UnixStream) -> CF,
    IH: Fn(UnixStream) -> IF,
    CF: Future<Output = ()> + Send + 'static,
    IF: Future<Output = ()> + Send + 'static,
{
    tokio::pin!(shutdown);
    let mut report = Report::default();
    let mut tasks = JoinSet::new();
    let mut callback_retry = tokio::time::Instant::now();
    let mut internal_retry = callback_retry;
    let value = loop {
        tokio::select! {
            biased;
            result = &mut shutdown => break result,
            accepted = async {
                tokio::time::sleep_until(callback_retry).await;
                callback_accept().await
            }, if tasks.len() < limit => {
                match accepted {
                    Ok(stream) => { tasks.spawn(callback_handler(stream)); }
                    Err(error) => {
                        report.record("cleanup callback accept", error);
                        callback_retry = tokio::time::Instant::now() + Duration::from_millis(250);
                    }
                }
            }
            accepted = async {
                tokio::time::sleep_until(internal_retry).await;
                internal_accept().await
            }, if tasks.len() < limit => {
                match accepted {
                    Ok(stream) => { tasks.spawn(internal_handler(stream)); }
                    Err(error) => {
                        report.record("cleanup internal accept", error);
                        internal_retry = tokio::time::Instant::now() + Duration::from_millis(250);
                    }
                }
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = result {
                    report.record("cleanup connection join", error);
                }
            }
        }
    };
    // Request waiters may be cancelled only after the retained owner finishes.
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            report.record("cleanup connection join", error);
        }
    }
    (value, report)
}

#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod tests;
