//! Permanent caller identities for public module mutations. Claim is fsynced
//! before entering a dispatcher. An interrupted claim is always unknown and
//! never grants dispatch rights again, including after daemon replacement.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Response, StatusCode};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future, path::Path};

pub(crate) const FILE: &str = "public-radio-mutation-receipts.json";
const LIMIT: usize = 16_384;
// Eight archive workers may be occupied by other stores. Bound accepted
// receipt I/O (including cancelled HTTP observers) rather than retaining an
// unbounded number of tasks waiting for those workers.
const COMMAND_LIMIT: usize = 64;
static COMMAND_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(COMMAND_LIMIT);
const COMMAND_BUSY: &str = "public_mutation_archive_busy";
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u8,
    sequence: u64,
    entries: BTreeMap<String, Receipt>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    sequence: u64,
    fingerprint: String,
    route: String,
    binding: String,
    status: u16,
    body: Value,
}
fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.into())
}
fn load(items: &[Value]) -> Result<Ledger, StoreError> {
    if items.is_empty() {
        return Ok(Ledger {
            version: 1,
            sequence: 0,
            entries: BTreeMap::new(),
        });
    }
    if items.len() != 1 {
        return Err(failure("invalid_public_mutation_schema"));
    }
    let ledger: Ledger = serde_json::from_value(items[0].clone()).map_err(StoreError::Json)?;
    if ledger.version != 1
        || ledger.sequence != ledger.entries.len() as u64
        || ledger
            .entries
            .values()
            .map(|r| r.sequence)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != ledger.entries.len()
        || ledger.entries.len() > LIMIT
        || ledger.entries.iter().any(|(id, r)| {
            r.sequence == 0
                || r.sequence > ledger.sequence
                || !valid_id(id)
                || r.fingerprint.len() != 64
                || !r.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
                || r.route.is_empty()
                || r.binding.is_empty()
                || r.body["mutation_receipt"]["binding"].as_str() != Some(r.binding.as_str())
                || !(200..600).contains(&r.status)
                || r.body
                    .get("mutation_receipt")
                    .and_then(|v| v.get("client_request_id"))
                    .and_then(Value::as_str)
                    != Some(id.as_str())
                || !matches!(
                    r.body["mutation_receipt"]["outcome"].as_str(),
                    Some("completed" | "unknown")
                )
        })
    {
        return Err(failure("invalid_public_mutation_schema"));
    }
    Ok(ledger)
}
fn save(items: &mut Vec<Value>, ledger: &Ledger) -> Result<(), StoreError> {
    *items = vec![serde_json::to_value(ledger).map_err(StoreError::Json)?];
    Ok(())
}
pub(crate) fn valid_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
pub(crate) fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"error":code,"code":code}))
}
/// Identity is removed before the route's deny_unknown_fields DTO is decoded.
/// The original bytes, including this identity, remain the exact fingerprint.
pub(crate) fn split(
    data: &[u8],
    required: bool,
) -> Result<(Option<String>, Vec<u8>), Response<Full<Bytes>>> {
    let mut value: Value = serde_json::from_slice(data)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_public_mutation_request"))?;
    let fields = value
        .as_object_mut()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "invalid_public_mutation_request"))?;
    let id = match fields.remove("client_request_id") {
        None if !required => None,
        Some(Value::String(id)) if valid_id(&id) => Some(id),
        _ => return Err(error(StatusCode::BAD_REQUEST, "client_request_id_required")),
    };
    Ok((id, serde_json::to_vec(&value).expect("JSON value")))
}
fn annotate(body: &mut Value, id: &str, binding: &str, outcome: &str) {
    if !body.is_object() {
        *body = json!({"result":body});
    }
    body["mutation_receipt"] = json!({"client_request_id":id,"binding":binding,"outcome":outcome});
}
fn response(receipt: &Receipt) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::from_u16(receipt.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        &receipt.body,
    )
}

/// Resolve an already admitted identity before route-specific snapshot work.
/// This keeps exact replays independent of current device/card/archive state,
/// while payload drift and ledger corruption fail closed. A `None` result only
/// means this caller identity has not yet been durably claimed; `run` remains
/// the sole claim/dispatch authority and closes the concurrent lookup race.
enum ReplayRead {
    Hit(Receipt),
    Miss,
    Conflict,
}

fn replay_receipt(
    root: &Path,
    route: &str,
    id: &str,
    payload: &[u8],
) -> Result<ReplayRead, StoreError> {
    let store = ArchiveStore::new(root.join(FILE))
        .require_nonempty_ledger()
        .map(crate::sms_store_health::GuardedArchive::from)?;
    let fingerprint =
        proletarius_sms::stable_digest_hex(&[route.as_bytes(), b"\0", payload].concat());
    store.read(|items| {
        let ledger = load(items)?;
        match ledger.entries.get(id) {
            Some(receipt) if receipt.route == route && receipt.fingerprint == fingerprint => {
                Ok(ReplayRead::Hit(receipt.clone()))
            }
            Some(_) => Ok(ReplayRead::Conflict),
            None => Ok(ReplayRead::Miss),
        }
    })
}

fn replay_result(
    result: Result<ReplayRead, StoreError>,
) -> Result<Option<Response<Full<Bytes>>>, Response<Full<Bytes>>> {
    match result {
        Ok(ReplayRead::Hit(receipt)) => Ok(Some(response(&receipt))),
        Ok(ReplayRead::Miss) => Ok(None),
        Ok(ReplayRead::Conflict) => Err(error(
            StatusCode::CONFLICT,
            "public_mutation_request_conflict",
        )),
        Err(StoreError::Configuration(code)) if code == COMMAND_BUSY => {
            Err(error(StatusCode::SERVICE_UNAVAILABLE, &code))
        }
        Err(_) => Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_mutation_store_unavailable",
        )),
    }
}

/// Register the complete locked ledger read before returning the future.
/// Cancellation of its HTTP observer never releases the archive job early.
pub(crate) fn replay_async(
    root: &Path,
    route: &str,
    id: &str,
    payload: &[u8],
) -> impl Future<Output = Result<Option<Response<Full<Bytes>>>, Response<Full<Bytes>>>> + Send + 'static
{
    let valid = valid_id(id);
    let root = root.to_path_buf();
    let route = route.to_owned();
    let id = id.to_owned();
    let payload = payload.to_vec();
    let job = valid.then(|| read_command(move || replay_receipt(&root, &route, &id, &payload)));
    async move {
        match job {
            Some(job) => replay_result(job.await),
            None => Err(error(StatusCode::BAD_REQUEST, "client_request_id_required")),
        }
    }
}

/// Admit before registering with archive_io. One shared token covers both the
/// observer future and actual worker capture, so cancellation cannot release a
/// still-running worker's capacity. All ledger reads/writes share this budget.
fn bounded_archive_io<T, F>(
    operation: F,
    durable: bool,
) -> Result<impl Future<Output = Result<T, StoreError>> + Send + 'static, StoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    let permit = COMMAND_SLOTS
        .try_acquire()
        .map_err(|_| failure(COMMAND_BUSY))?;
    let permit = std::sync::Arc::new(permit);
    let worker_permit = permit.clone();
    let operation = move || {
        let _permit = worker_permit;
        operation()
    };
    let job: std::pin::Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send>> = if durable {
        Box::pin(crate::archive_io::run_durable(operation, permit.clone()))
    } else {
        Box::pin(crate::archive_io::run(operation))
    };
    Ok(async move {
        let _permit = permit;
        job.await
    })
}

/// Only durable I/O is detached. Dispatch retains the caller's borrowed
/// generation/admission guards and remains cancellable. Admission and archive
/// registration occur before spawn, including for a never-polled HTTP observer.
fn durable_command<T, F>(operation: F) -> impl Future<Output = Result<T, StoreError>> + Send
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    let command = bounded_archive_io(
        move || {
            match operation() {
                Ok(value) => Ok(Ok(value)),
                // These refusals never committed a new claim or entered dispatch.
                // Keep them as ordinary business output, not orphan I/O failure.
                Err(StoreError::Configuration(code))
                    if matches!(
                        code.as_str(),
                        "public_mutation_request_conflict"
                            | "public_mutation_store_full"
                            | "public_mutation_binding_unavailable"
                    ) =>
                {
                    Ok(Err(StoreError::Configuration(code)))
                }
                Err(error) => Err(error),
            }
        },
        true,
    );
    async move { command?.await? }
}

/// Reads use the same capacity and registration proof, while retaining their
/// existing cancellation semantics instead of creating unnecessary tasks.
fn read_command<T, F>(operation: F) -> impl Future<Output = Result<T, StoreError>> + Send
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    let job = bounded_archive_io(operation, false);
    async move { job?.await }
}

pub(crate) async fn run<F, Fut>(
    root: &Path,
    route: &str,
    id: &str,
    payload: &[u8],
    binding: String,
    dispatch: F,
) -> Response<Full<Bytes>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Response<Full<Bytes>>>,
{
    if !valid_id(id) {
        return error(StatusCode::BAD_REQUEST, "client_request_id_required");
    }
    // Register the entire constructor/claim operation before detaching its
    // observer. Losing the HTTP waiter never cancels a queued durable write;
    // only the caller that observes a fresh committed claim may dispatch.
    let claim = durable_command({
        let root = root.to_path_buf();
        let route = route.to_owned();
        let id = id.to_owned();
        let fingerprint =
            proletarius_sms::stable_digest_hex(&[route.as_bytes(), b"\0", payload].concat());
        let binding = binding.clone();
        move || {
            let store = crate::sms_store_health::GuardedArchive::from(
                ArchiveStore::new(root.join(FILE)).require_nonempty_ledger()?,
            );
            let claim = store.mutate(|items| {
                let mut ledger = load(items)?;
                if let Some(receipt) = ledger.entries.get(&id) {
                    if receipt.fingerprint != fingerprint || receipt.route != route {
                        return Err(failure("public_mutation_request_conflict"));
                    }
                    return Ok(Some(receipt.clone()));
                }
                if ledger.entries.len() >= LIMIT {
                    return Err(failure("public_mutation_store_full"));
                }
                if binding.is_empty() {
                    return Err(failure("public_mutation_binding_unavailable"));
                }
                let mut body = json!({"error":"操作结果未知，原请求不会重新提交","code":"public_mutation_outcome_unknown"});
                annotate(&mut body, &id, &binding, "unknown");
                ledger.sequence = ledger.sequence.checked_add(1)
                    .ok_or_else(|| failure("public_mutation_sequence_exhausted"))?;
                ledger.entries.insert(id.clone(), Receipt {
                    sequence: ledger.sequence,
                    fingerprint: fingerprint.clone(),
                    route: route.clone(),
                    binding: binding.clone(),
                    status: 409,
                    body,
                });
                save(items, &ledger)?;
                Ok(None)
            })?;
            Ok((store, fingerprint, claim))
        }
    }).await;
    let (store, fingerprint) = match claim {
        Ok((_, _, Some(receipt))) => return response(&receipt),
        Ok((store, fingerprint, None)) => (store, fingerprint),
        Err(StoreError::Configuration(code)) if code == "public_mutation_request_conflict" => {
            return error(StatusCode::CONFLICT, &code);
        }
        Err(StoreError::Configuration(code)) if code == COMMAND_BUSY => {
            return error(StatusCode::SERVICE_UNAVAILABLE, &code);
        }
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_mutation_store_unavailable",
            );
        }
    };
    let result = dispatch().await;
    let status = result.status();
    let bytes = match result.into_body().collect().await {
        Ok(v) => v.to_bytes(),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_mutation_outcome_unknown",
            );
        }
    };
    let mut body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_mutation_outcome_unknown",
            );
        }
    };
    // Error paths can represent lost device replies. Keep the original unknown
    // fence; a new explicit intent is required after independent observation.
    if status.is_server_error()
        || body.get("uncertain").and_then(Value::as_bool) == Some(true)
        || (!status.is_success() && body.get("submitted").and_then(Value::as_bool) == Some(true))
        || matches!(
            body.get("outcome").and_then(Value::as_str),
            Some("unknown" | "uncertain" | "result_unknown")
        )
    {
        let id = id.to_owned();
        return read_command(move || {
            store.read(|items| {
                load(items)?
                    .entries
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| failure("invalid_public_mutation_schema"))
            })
        })
        .await
        .map(|r| response(&r))
        .unwrap_or_else(|source| {
            let code = if matches!(source, StoreError::Configuration(ref code) if code == COMMAND_BUSY) {
                eprintln!("public_mutation_unknown_read_busy_outcome_unknown");
                "public_mutation_unknown_read_busy_outcome_unknown"
            } else {
                "public_mutation_store_unavailable"
            };
            error(StatusCode::SERVICE_UNAVAILABLE, code)
        });
    }
    annotate(&mut body, id, &binding, "completed");
    let receipt = Receipt {
        sequence: 0,
        fingerprint,
        route: route.into(),
        binding,
        status: status.as_u16(),
        body,
    };
    let committed = durable_command({
        let id = id.to_owned();
        let receipt = receipt.clone();
        move || {
            store.mutate(|items| {
                let mut ledger = load(items)?;
                let old = ledger
                    .entries
                    .get(&id)
                    .ok_or_else(|| failure("invalid_public_mutation_schema"))?;
                if old.fingerprint != receipt.fingerprint {
                    return Err(failure("invalid_public_mutation_schema"));
                }
                let mut receipt = receipt.clone();
                receipt.sequence = old.sequence;
                ledger.entries.insert(id, receipt);
                save(items, &ledger)
            })
        }
    })
    .await;
    match committed {
        Ok(()) => response(&receipt),
        Err(StoreError::Configuration(code)) if code == COMMAND_BUSY => {
            // Fixed diagnostic contains no identity, destination or payload.
            // The already committed unknown claim remains the replay fence.
            eprintln!("public_mutation_completion_busy_outcome_unknown");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "public_mutation_completion_busy_outcome_unknown",
            )
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "public_mutation_store_unavailable",
        ),
    }
}

/// Last admitted generation, including an unknown physical start. Callers use
/// the start receipt's request ID as the exact target for a later stop.
pub(crate) fn latest(root: &Path, route: &str) -> Result<Option<(String, String)>, StoreError> {
    let store = crate::sms_store_health::GuardedArchive::from(
        ArchiveStore::new(root.join(FILE)).require_nonempty_ledger()?,
    );
    store.read(|items| {
        Ok(load(items)?
            .entries
            .into_iter()
            .filter(|(_, r)| r.route == route)
            .max_by_key(|(_, r)| r.sequence)
            .map(|(id, r)| (id, r.binding)))
    })
}

/// Keep the exact start-generation lookup under its original archive lock,
/// but wait for disk on the bounded archive worker rather than a Tokio worker.
pub(crate) fn latest_async(
    root: &Path,
    route: &str,
) -> impl Future<Output = Result<Option<(String, String)>, StoreError>> + Send + 'static {
    let root = root.to_path_buf();
    let route = route.to_owned();
    read_command(move || latest(&root, &route))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    fn root() -> std::path::PathBuf {
        let p = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace build dir"),
        )
        .join("runtime/public-radio-receipt-tests")
        .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    async fn value(r: Response<Full<Bytes>>) -> Value {
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    // Every stateful receipt test uses the real process-wide quota and archive
    // executor in its own process. A private file per output stream avoids
    // pipe backpressure while the parent observes the finite child lifetime.
    const CHILD_OUTPUT_LIMIT: usize = 1024 * 1024;
    struct CapturedChildStream {
        bytes: Vec<u8>,
        total_bytes: u64,
        path: std::path::PathBuf,
    }
    impl CapturedChildStream {
        fn read(path: std::path::PathBuf) -> Self {
            use std::io::Read;
            let file = std::fs::File::open(&path).expect("open child output");
            let total_bytes = file.metadata().expect("stat child output").len();
            let mut bytes = Vec::with_capacity(total_bytes.min(CHILD_OUTPUT_LIMIT as u64) as usize);
            file.take(CHILD_OUTPUT_LIMIT as u64)
                .read_to_end(&mut bytes)
                .expect("read bounded child output");
            Self {
                bytes,
                total_bytes,
                path,
            }
        }
        fn truncated(&self) -> bool {
            self.total_bytes > self.bytes.len() as u64
        }
        fn report(&self, stream: &str) {
            eprint!("{}", String::from_utf8_lossy(&self.bytes));
            if self.truncated() {
                eprintln!(
                    "\n[{stream} truncated: retained {} of {} bytes; complete output: {}]",
                    self.bytes.len(),
                    self.total_bytes,
                    self.path.display()
                );
            }
        }
    }
    struct ReceiptChildCapture {
        status: std::process::ExitStatus,
        timed_out: bool,
        stdout: CapturedChildStream,
        stderr: CapturedChildStream,
    }
    fn run_receipt_child(name: &str) -> ReceiptChildCapture {
        let directory = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace build dir"),
        )
        .join("runtime/receipt-child-output")
        .join(format!(
            "{}-{}",
            std::process::id(),
            crate::snapshot::new_instance_id()
        ));
        std::fs::create_dir_all(&directory).expect("create private child output directory");
        let stdout_path = directory.join("stdout.log");
        let stderr_path = directory.join("stderr.log");
        let create = |path: &Path| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .expect("create child output")
        };
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("public_mutation_receipt::tests::{name}"),
                "--nocapture",
                "--test-threads=1",
            ])
            // All workspace/runtime environment is inherited unchanged.
            .env("SWITCH_RECEIPT_OWNER_CHILD", name)
            .stdout(std::process::Stdio::from(create(&stdout_path)))
            .stderr(std::process::Stdio::from(create(&stderr_path)))
            .spawn()
            .expect("spawn isolated receipt test");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut timed_out = false;
        loop {
            if child
                .try_wait()
                .expect("observe isolated receipt test")
                .is_some()
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                timed_out = true;
                child.kill().expect("kill timed-out receipt test");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // No output read is needed to allow exit, kill, or actual wait/reap.
        let status = child.wait().expect("reap isolated receipt test");
        ReceiptChildCapture {
            status,
            timed_out,
            stdout: CapturedChildStream::read(stdout_path),
            stderr: CapturedChildStream::read(stderr_path),
        }
    }
    fn isolated_receipt_case(name: &str) -> bool {
        if std::env::var("SWITCH_RECEIPT_OWNER_CHILD").as_deref() == Ok(name) {
            return false;
        }
        let output = run_receipt_child(name);
        output.stdout.report("stdout");
        output.stderr.report("stderr");
        assert!(
            !output.timed_out,
            "isolated receipt test exceeded finite deadline: {name}"
        );
        assert!(
            output.status.success(),
            "isolated receipt regression failed: {name}; status={}",
            output.status
        );
        true
    }

    #[test]
    fn isolated_child_large_failure_output_is_bounded_and_keeps_original_status() {
        use std::io::Write;
        const NAME: &str =
            "isolated_child_large_failure_output_is_bounded_and_keeps_original_status";
        if std::env::var("SWITCH_RECEIPT_OWNER_CHILD").as_deref() == Ok(NAME) {
            // Each stream exceeds pipe capacity and the parent display budget.
            // The original failure code must survive without a timeout kill.
            let bytes = vec![b'x'; CHILD_OUTPUT_LIMIT * 2];
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&bytes).unwrap();
            stdout.flush().unwrap();
            let mut stderr = std::io::stderr().lock();
            stderr.write_all(&bytes).unwrap();
            stderr.flush().unwrap();
            std::process::exit(23);
        }
        let started = std::time::Instant::now();
        let output = run_receipt_child(NAME);
        assert_eq!(
            output.status.code(),
            Some(23),
            "original failure status was lost"
        );
        assert!(
            !output.timed_out,
            "output backpressure exhausted the child deadline"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "large failing output must finish without waiting for the 60-second timeout"
        );
        for stream in [&output.stdout, &output.stderr] {
            assert!(stream.total_bytes >= (CHILD_OUTPUT_LIMIT * 2) as u64);
            assert_eq!(stream.bytes.len(), CHILD_OUTPUT_LIMIT);
            assert!(stream.truncated());
            assert!(
                stream.path.is_file(),
                "full fault output must remain reviewable"
            );
        }
        eprintln!(
            "bounded child capture: exit=23; stdout={}/{}; stderr={}/{}; truncated=true; elapsed_ms={}",
            output.stdout.bytes.len(),
            output.stdout.total_bytes,
            output.stderr.bytes.len(),
            output.stderr.total_bytes,
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn cancelled_durable_io_error_is_repeatably_visible_without_drain_gap() {
        if isolated_receipt_case(
            "cancelled_durable_io_error_is_repeatably_visible_without_drain_gap",
        ) {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use std::time::Duration;
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let observer = tokio::spawn(durable_command::<(), _>(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
                Err(StoreError::Io(std::io::Error::from_raw_os_error(28)))
            }));
            ready.await.unwrap();
            observer.abort();
            assert!(observer.await.unwrap_err().is_cancelled());
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(25),
                    crate::archive_io::finish_pending()
                )
                .await
                .is_err()
            );
            release.send(()).unwrap();
            let failure =
                tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
                    .await
                    .unwrap()
                    .unwrap_err();
            assert!(failure.contains("durable archive persistence failed"));
            assert_eq!(
                crate::archive_io::finish_pending().await.unwrap_err(),
                failure
            );
            assert_eq!(COMMAND_SLOTS.available_permits(), COMMAND_LIMIT);
        });
    }

    #[test]
    fn cancelled_real_completion_error_remains_visible_and_unknown_never_redispatches() {
        if isolated_receipt_case(
            "cancelled_real_completion_error_remains_visible_and_unknown_never_redispatches",
        ) {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use std::time::Duration;
            let root = root();
            let owned = root.clone();
            let count = Arc::new(AtomicUsize::new(0));
            let n = count.clone();
            let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
            let observer = tokio::spawn(async move {
                run(
                    &owned,
                    "/api/at",
                    "owner-real-error",
                    b"exact",
                    "original".into(),
                    || async {
                        n.fetch_add(1, Ordering::SeqCst);
                        let bytes = std::fs::read(owned.join(FILE)).unwrap();
                        gate_tx.send((bytes, hold_ledger(&owned))).unwrap();
                        crate::json_response(StatusCode::OK, &json!({"physical_effect":true}))
                    },
                )
                .await
            });
            let (original, (release, holder)) = gate_rx.await.unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
            observer.abort();
            assert!(observer.await.unwrap_err().is_cancelled());
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(25),
                    crate::archive_io::finish_pending()
                )
                .await
                .is_err()
            );
            let path = root.join(FILE);
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            release.send(()).unwrap();
            holder.join().unwrap();
            let failure =
                tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
                    .await
                    .unwrap()
                    .unwrap_err();
            assert!(failure.contains("durable archive persistence failed"));
            assert!(!failure.contains("owner-real-error"));
            assert!(!failure.contains(root.to_str().unwrap()));
            assert_eq!(
                crate::archive_io::finish_pending().await.unwrap_err(),
                failure
            );
            let blocked = run(
                &root,
                "/api/at",
                "owner-real-error",
                b"exact",
                "successor".into(),
                || async { panic!("redispatch") },
            )
            .await;
            assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
            std::fs::remove_dir(&path).unwrap();
            std::fs::write(&path, original).unwrap();
            let replay = run(
                &root,
                "/api/at",
                "owner-real-error",
                b"exact",
                "successor".into(),
                || async { panic!("redispatch") },
            )
            .await;
            assert_eq!(replay.status(), StatusCode::CONFLICT);
            assert_eq!(
                value(replay).await["mutation_receipt"]["outcome"],
                "unknown"
            );
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert_eq!(
                crate::archive_io::finish_pending().await.unwrap_err(),
                failure
            );
            assert_eq!(COMMAND_SLOTS.available_permits(), COMMAND_LIMIT);
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_expected_business_refusals_do_not_poison_archive_drain() {
        if isolated_receipt_case("cancelled_expected_business_refusals_do_not_poison_archive_drain")
        {
            return;
        }
        use std::time::Duration;
        for code in [
            "public_mutation_request_conflict",
            "public_mutation_store_full",
            "public_mutation_binding_unavailable",
        ] {
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let observer = tokio::spawn(durable_command::<(), _>(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
                Err(failure(code))
            }));
            ready.await.unwrap();
            observer.abort();
            assert!(observer.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
                .await
                .unwrap()
                .unwrap();
            crate::archive_io::finish_pending().await.unwrap();
            assert_eq!(COMMAND_SLOTS.available_permits(), COMMAND_LIMIT);
        }
    }

    // An independent thread owns the real ArchiveStore lock. Its timeout is a
    // deadlock guard for the baseline implementation, not the success signal.
    fn hold_ledger(root: &Path) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let lock_path = root.join(format!("{FILE}.lock"));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)
                .unwrap();
            lock.lock().unwrap();
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(2));
            drop(lock);
        });
        ready_rx.recv().unwrap();
        (release_tx, holder)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn claim_wait_is_off_executor_and_cancelled_command_never_dispatches() {
        if isolated_receipt_case(
            "claim_wait_is_off_executor_and_cancelled_command_never_dispatches",
        ) {
            return;
        }
        use std::time::{Duration, Instant};
        let root = root();
        let seed = run(
            &root,
            "/api/at",
            "seed-receipt-claim",
            b"seed",
            "module-1".into(),
            || async { crate::json_response(StatusCode::OK, &json!({"ok":true})) },
        )
        .await;
        assert_eq!(seed.status(), StatusCode::OK);
        let (release, holder) = hold_ledger(&root);
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let owned = root.clone();
        let before = Instant::now();
        let observer = tokio::spawn(async move {
            run(
                &owned,
                "/api/at",
                "cancelled-claim-original",
                b"exact",
                "module-1".into(),
                || async {
                    n.fetch_add(1, Ordering::SeqCst);
                    crate::json_response(StatusCode::OK, &json!({"ok":true}))
                },
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            before.elapsed() < Duration::from_millis(250),
            "claim blocked current-thread Tokio"
        );
        assert!(
            !observer.is_finished(),
            "claim bypassed the held archive lock"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "dispatch ran before durable claim"
        );
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                crate::archive_io::finish_pending()
            )
            .await
            .is_err()
        );
        release.send(()).unwrap();
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
            .await
            .unwrap()
            .unwrap();
        let replay = run(
            &root,
            "/api/at",
            "cancelled-claim-original",
            b"exact",
            "module-2".into(),
            || async { panic!("cancelled claim redispatched") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert_eq!(
            value(replay).await["mutation_receipt"]["outcome"],
            "unknown"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn final_receipt_wait_is_off_executor_and_survives_http_cancellation() {
        if isolated_receipt_case(
            "final_receipt_wait_is_off_executor_and_survives_http_cancellation",
        ) {
            return;
        }
        use std::time::{Duration, Instant};
        let root = root();
        let owned = root.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let before = Instant::now();
        let observer = tokio::spawn(async move {
            run(
                &owned,
                "/api/sms/forwarding/queue-action",
                "cancelled-final-original",
                b"exact",
                "queue-1".into(),
                || async {
                    n.fetch_add(1, Ordering::SeqCst);
                    let lock = hold_ledger(&owned);
                    held_tx.send(lock).unwrap();
                    crate::json_response(
                        StatusCode::ACCEPTED,
                        &json!({"accepted":true,"generation":1}),
                    )
                },
            )
            .await
        });
        let (release, holder) = held_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            before.elapsed() < Duration::from_millis(250),
            "final write blocked current-thread Tokio"
        );
        assert!(!observer.is_finished(), "final write bypassed archive lock");
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                crate::archive_io::finish_pending()
            )
            .await
            .is_err()
        );
        release.send(()).unwrap();
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
            .await
            .unwrap()
            .unwrap();
        let replay = run(
            &root,
            "/api/sms/forwarding/queue-action",
            "cancelled-final-original",
            b"exact",
            "replacement".into(),
            || async { panic!("cancelled completion redispatched") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::ACCEPTED);
        let replay = value(replay).await;
        assert_eq!(replay["mutation_receipt"]["outcome"], "completed");
        assert_eq!(replay["generation"], 1);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_final_receipt_waiting_for_worker_permit_still_commits() {
        if isolated_receipt_case("cancelled_final_receipt_waiting_for_worker_permit_still_commits")
        {
            return;
        }
        use std::time::Duration;
        let root = root();
        let owned = root.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let (occupied_tx, occupied_rx) = tokio::sync::oneshot::channel();
        let observer = tokio::spawn(async move {
            run(
                &owned,
                "/api/at",
                "queued-final-original",
                b"exact",
                "module-1".into(),
                || async {
                    n.fetch_add(1, Ordering::SeqCst);
                    // Occupy all eight slots in the real bounded archive pool
                    // after claim, before the terminal receipt is registered.
                    let mut release = Vec::new();
                    let mut blockers = Vec::new();
                    for _ in 0..8 {
                        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                        let (release_tx, release_rx) = std::sync::mpsc::channel();
                        release.push(release_tx);
                        blockers.push(tokio::spawn(crate::archive_io::run(move || {
                            let _ = started_tx.send(());
                            let _ = release_rx.recv_timeout(Duration::from_secs(3));
                            Ok(())
                        })));
                        tokio::time::timeout(Duration::from_secs(1), started_rx)
                            .await
                            .unwrap()
                            .unwrap();
                    }
                    occupied_tx.send((release, blockers)).unwrap();
                    crate::json_response(
                        StatusCode::ACCEPTED,
                        &json!({"accepted":true,"generation":1}),
                    )
                },
            )
            .await
        });
        let (release, blockers) = occupied_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !observer.is_finished(),
            "final receipt did not wait for a worker permit"
        );
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        for sender in release {
            sender.send(()).unwrap();
        }
        for blocker in blockers {
            blocker.await.unwrap().unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
            .await
            .unwrap()
            .unwrap();
        let replay = run(
            &root,
            "/api/at",
            "queued-final-original",
            b"exact",
            "replacement".into(),
            || async { panic!("queued completion redispatched") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::ACCEPTED);
        assert_eq!(
            value(replay).await["mutation_receipt"]["outcome"],
            "completed"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_receipt_admission_refuses_claim_before_registration_and_dispatch() {
        if isolated_receipt_case(
            "saturated_receipt_admission_refuses_claim_before_registration_and_dispatch",
        ) {
            return;
        }
        let root = root();
        let held = COMMAND_SLOTS
            .acquire_many(COMMAND_LIMIT as u32)
            .await
            .unwrap();
        let count = AtomicUsize::new(0);
        let refused = run(
            &root,
            "/api/at",
            "full-claim-original",
            b"exact",
            "module-1".into(),
            || async {
                count.fetch_add(1, Ordering::SeqCst);
                panic!("saturated claim dispatched")
            },
        )
        .await;
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(value(refused).await["code"], COMMAND_BUSY);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(!root.join(FILE).exists());
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::archive_io::finish_pending(),
        )
        .await
        .unwrap()
        .unwrap();
        drop(held);
        let accepted = run(
            &root,
            "/api/at",
            "full-claim-original",
            b"exact",
            "module-1".into(),
            || async {
                count.fetch_add(1, Ordering::SeqCst);
                crate::json_response(StatusCode::OK, &json!({"ok":true}))
            },
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_final_receipt_keeps_durable_unknown_and_exposes_busy_diagnostic() {
        if isolated_receipt_case(
            "saturated_final_receipt_keeps_durable_unknown_and_exposes_busy_diagnostic",
        ) {
            return;
        }
        let root = root();
        let mut held = None;
        let count = AtomicUsize::new(0);
        let refused = run(
            &root,
            "/api/at",
            "full-final-original",
            b"exact",
            "module-1".into(),
            || async {
                count.fetch_add(1, Ordering::SeqCst);
                held = Some(
                    COMMAND_SLOTS
                        .acquire_many(COMMAND_LIMIT as u32)
                        .await
                        .unwrap(),
                );
                crate::json_response(StatusCode::OK, &json!({"ok":true}))
            },
        )
        .await;
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            value(refused).await["code"],
            "public_mutation_completion_busy_outcome_unknown"
        );
        // Reads share the same bounded admission budget; saturation never
        // grants a new dispatch right or rewrites the existing unknown fence.
        let busy = replay_async(&root, "/api/at", "full-final-original", b"exact")
            .await
            .unwrap_err();
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(value(busy).await["code"], COMMAND_BUSY);
        assert!(matches!(latest_async(&root, "/api/at").await,
            Err(StoreError::Configuration(code)) if code == COMMAND_BUSY));
        drop(held);
        let queried = replay_async(&root, "/api/at", "full-final-original", b"exact")
            .await
            .unwrap()
            .expect("durable unknown claim");
        assert_eq!(queried.status(), StatusCode::CONFLICT);
        assert_eq!(
            value(queried).await["mutation_receipt"]["outcome"],
            "unknown"
        );
        let replay = run(
            &root,
            "/api/at",
            "full-final-original",
            b"exact",
            "replacement".into(),
            || async { panic!("saturated final receipt redispatched") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert_eq!(
            value(replay).await["mutation_receipt"]["outcome"],
            "unknown"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_unknown_read_preserves_claim_without_redispatch() {
        if isolated_receipt_case("saturated_unknown_read_preserves_claim_without_redispatch") {
            return;
        }
        let root = root();
        let mut held = None;
        let count = AtomicUsize::new(0);
        let refused = run(
            &root,
            "/api/at",
            "full-unknown-original",
            b"exact",
            "module-1".into(),
            || async {
                count.fetch_add(1, Ordering::SeqCst);
                held = Some(
                    COMMAND_SLOTS
                        .acquire_many(COMMAND_LIMIT as u32)
                        .await
                        .unwrap(),
                );
                crate::json_response(StatusCode::SERVICE_UNAVAILABLE, &json!({"uncertain":true}))
            },
        )
        .await;
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            value(refused).await["code"],
            "public_mutation_unknown_read_busy_outcome_unknown"
        );
        drop(held);
        let replay = run(
            &root,
            "/api/at",
            "full-unknown-original",
            b"exact",
            "replacement".into(),
            || async { panic!("saturated unknown read redispatched") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert_eq!(
            value(replay).await["mutation_receipt"]["outcome"],
            "unknown"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_shutdown_keeps_running_receipt_command_and_permit_until_archive_drain() {
        if isolated_receipt_case(
            "runtime_shutdown_keeps_running_receipt_command_and_permit_until_archive_drain",
        ) {
            return;
        }
        use std::time::Duration;
        let root = root();
        let path = root.join("runtime-shutdown-command.json");
        let owned_path = path.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let command = durable_command(move || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                ArchiveStore::new(owned_path).mutate(|items| {
                    items.push(json!({"completed":true}));
                    Ok(())
                })
            });
            tokio::spawn(command);
            started_rx.await.unwrap();
        });
        runtime.shutdown_timeout(Duration::from_millis(10));
        assert_eq!(
            COMMAND_SLOTS.available_permits(),
            COMMAND_LIMIT - 1,
            "runtime cancellation released a still-running command's permit"
        );
        let observer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        observer.block_on(async {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(25),
                    crate::archive_io::finish_pending()
                )
                .await
                .is_err()
            );
            assert!(!path.exists());
            release_tx.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
                .await
                .unwrap()
                .unwrap();
        });
        assert_eq!(COMMAND_SLOTS.available_permits(), COMMAND_LIMIT);
        assert_eq!(
            ArchiveStore::new(path).load().unwrap(),
            vec![json!({"completed":true})]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertain_receipt_read_wait_is_off_executor_and_keeps_unknown_fence() {
        if isolated_receipt_case(
            "uncertain_receipt_read_wait_is_off_executor_and_keeps_unknown_fence",
        ) {
            return;
        }
        use std::time::{Duration, Instant};
        let root = root();
        let owned = root.clone();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let before = Instant::now();
        let observer = tokio::spawn(async move {
            run(
                &owned,
                "/api/at",
                "uncertain-read-original",
                b"exact",
                "module-1".into(),
                || async {
                    held_tx.send(hold_ledger(&owned)).unwrap();
                    crate::json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &json!({"uncertain":true}),
                    )
                },
            )
            .await
        });
        let (release, holder) = held_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            before.elapsed() < Duration::from_millis(250),
            "unknown-fence read blocked current-thread Tokio"
        );
        assert!(!observer.is_finished());
        release.send(()).unwrap();
        holder.join().unwrap();
        let receipt = observer.await.unwrap();
        assert_eq!(receipt.status(), StatusCode::CONFLICT);
        assert_eq!(
            value(receipt).await["mutation_receipt"]["outcome"],
            "unknown"
        );
        let replay = run(
            &root,
            "/api/at",
            "uncertain-read-original",
            b"exact",
            "replacement".into(),
            || async { panic!("uncertain receipt redispatched") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replay_read_waits_off_runtime_and_cancel_does_not_release_disk_owner() {
        if isolated_receipt_case(
            "replay_read_waits_off_runtime_and_cancel_does_not_release_disk_owner",
        ) {
            return;
        }
        use std::fs::OpenOptions;
        use std::time::Duration;

        let root = root();
        let first = run(
            &root,
            "/api/calls/clear",
            "receipt-read-lock-test",
            b"same request",
            "archive-generation-1".into(),
            || async { crate::json_response(StatusCode::OK, &json!({"ok":true})) },
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let lock_path = root.join(format!("{FILE}.lock"));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)
                .unwrap();
            lock.lock().unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(180));
            drop(lock);
        });
        ready_rx.recv().unwrap();

        let read = replay_async(
            &root,
            "/api/calls/clear",
            "receipt-read-lock-test",
            b"same request",
        );
        let task = tokio::spawn(read);
        let mut ticks = 0;
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            ticks += 1;
        }
        assert!(ticks >= 6);
        assert!(!task.is_finished(), "receipt read bypassed the file lock");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                crate::archive_io::finish_pending()
            )
            .await
            .is_err(),
            "cancelled observer retired a locked archive job"
        );
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
            .await
            .unwrap()
            .unwrap();
        let replay = replay_async(
            &root,
            "/api/calls/clear",
            "receipt-read-lock-test",
            b"same request",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_expected_conflict_does_not_poison_archive_drain() {
        if isolated_receipt_case("cancelled_expected_conflict_does_not_poison_archive_drain") {
            return;
        }
        use std::time::Duration;

        let root = root();
        let first = run(
            &root,
            "/api/calls/clear",
            "receipt-conflict-test",
            b"original",
            "archive-generation-1".into(),
            || async { crate::json_response(StatusCode::OK, &json!({"ok":true})) },
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let owned_root = root.clone();
        let read = crate::archive_io::run(move || {
            let _ = started_tx.send(());
            release_rx.recv().unwrap();
            replay_receipt(
                &owned_root,
                "/api/calls/clear",
                "receipt-conflict-test",
                b"different",
            )
        });
        let observer = tokio::spawn(read);
        started_rx.await.unwrap();
        observer.abort();
        assert!(matches!(observer.await, Err(error) if error.is_cancelled()));
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), crate::archive_io::finish_pending())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            replay_async(
                &root,
                "/api/calls/clear",
                "receipt-conflict-test",
                b"different"
            )
            .await
            .unwrap_err()
            .status(),
            StatusCode::CONFLICT
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn asynchronous_receipt_reads_preserve_replay_conflict_and_latest_generation() {
        if isolated_receipt_case(
            "asynchronous_receipt_reads_preserve_replay_conflict_and_latest_generation",
        ) {
            return;
        }
        let root = root();
        let route = "/api/call/audio/start";
        let first = run(
            &root,
            route,
            "audio-start-original",
            b"first",
            "module-1:audio:audio-start-original".into(),
            || async { crate::json_response(StatusCode::OK, &json!({"generation":1})) },
        )
        .await;
        let expected = value(first).await;
        let replay = replay_async(&root, route, "audio-start-original", b"first")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value(replay).await, expected);
        assert_eq!(
            replay_async(&root, route, "audio-start-original", b"different")
                .await
                .unwrap_err()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            latest_async(&root, route).await.unwrap(),
            Some((
                "audio-start-original".into(),
                "module-1:audio:audio-start-original".into()
            ))
        );
        let before = std::fs::read(root.join(FILE)).unwrap();
        std::fs::remove_file(root.join(FILE)).unwrap();
        assert_eq!(
            replay_async(&root, route, "audio-start-original", b"first")
                .await
                .unwrap_err()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(latest_async(&root, route).await.is_err());
        assert!(!before.is_empty());
        assert!(!root.join(FILE).exists(), "read recreated a lost ledger");
        std::fs::remove_dir_all(root).unwrap();
    }
    const ROUTES: [&str; 14] = [
        "/api/at",
        "/api/module/flight-mode",
        "/api/network/usbnet",
        "/api/esim/module-notes",
        "/api/call-mode/enable",
        "/api/call-mode/restore",
        "/api/call/audio/start",
        "/api/call/audio/stop",
        "/api/sms/forwarding/queue-action",
        "/api/sms/clear",
        "/api/sms/delete-sender",
        "/api/calls/clear",
        "/api/v2/sms-archive/clear",
        "/api/v2/sms-archive/delete-sender",
    ];
    #[tokio::test]
    async fn all_radio_routes_keep_exact_receipt_after_successor_restart_and_payload_conflict() {
        if isolated_receipt_case(
            "all_radio_routes_keep_exact_receipt_after_successor_restart_and_payload_conflict",
        ) {
            return;
        }
        for route in ROUTES {
            let root = root();
            let count = AtomicUsize::new(0);
            let first = run(
                &root,
                route,
                "request-original",
                b"exact payload",
                "module-1:card-1:g1".into(),
                || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    crate::json_response(
                        StatusCode::ACCEPTED,
                        &json!({"accepted":true,"original_generation":1}),
                    )
                },
            )
            .await;
            let first = value(first).await;
            let second = run(
                &root,
                route,
                "request-successor",
                b"later payload",
                "module-2:card-2:g2".into(),
                || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    crate::json_response(StatusCode::OK, &json!({"generation":2}))
                },
            )
            .await;
            assert_eq!(second.status(), StatusCode::OK);
            let replay = value(
                run(
                    &root,
                    route,
                    "request-original",
                    b"exact payload",
                    "replacement-module-3:g3".into(),
                    || async { panic!("old request dispatched to successor") },
                )
                .await,
            )
            .await;
            assert_eq!(first, replay, "{route}");
            assert_eq!(count.load(Ordering::SeqCst), 2);
            let before = std::fs::read(root.join(FILE)).unwrap();
            let conflict = run(
                &root,
                route,
                "request-original",
                b"changed payload",
                "g3".into(),
                || async { panic!("conflict dispatched") },
            )
            .await;
            assert_eq!(conflict.status(), StatusCode::CONFLICT);
            assert_eq!(std::fs::read(root.join(FILE)).unwrap(), before);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn lost_waiter_and_concurrent_claim_are_unknown_and_never_redispatch() {
        if isolated_receipt_case(
            "lost_waiter_and_concurrent_claim_are_unknown_and_never_redispatch",
        ) {
            return;
        }
        let root = root();
        let count = Arc::new(AtomicUsize::new(0));
        let n = count.clone();
        let worker_root = root.clone();
        let worker = tokio::spawn(async move {
            run(
                &worker_root,
                "/api/at",
                "unknown-original",
                b"AT+CFUN=1,1",
                "module-1".into(),
                || async {
                    n.fetch_add(1, Ordering::SeqCst);
                    std::future::pending().await
                },
            )
            .await
        });
        while count.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let replay = run(
            &root,
            "/api/at",
            "unknown-original",
            b"AT+CFUN=1,1",
            "module-2".into(),
            || async { panic!("duplicate claimed") },
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        worker.abort();
        let _ = worker.await;
        let reopened = value(
            run(
                &root,
                "/api/at",
                "unknown-original",
                b"AT+CFUN=1,1",
                "module-3".into(),
                || async { panic!("restart dispatched") },
            )
            .await,
        )
        .await;
        assert_eq!(reopened["mutation_receipt"]["outcome"], "unknown");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn archive_corruption_and_file_or_presence_loss_fail_closed_without_rewrite() {
        if isolated_receipt_case(
            "archive_corruption_and_file_or_presence_loss_fail_closed_without_rewrite",
        ) {
            return;
        }
        for damage in [
            "empty",
            "null",
            "array",
            "malformed",
            "schema",
            "missing",
            "marker",
        ] {
            let root = root();
            let r = run(
                &root,
                "/api/at",
                "kept-original",
                b"mutation",
                "module-1".into(),
                || async { crate::json_response(StatusCode::OK, &json!({"ok":true})) },
            )
            .await;
            assert_eq!(r.status(), StatusCode::OK);
            let path = root.join(FILE);
            match damage {
                "missing" => std::fs::remove_file(&path).unwrap(),
                "marker" => {
                    std::fs::remove_file(root.join(format!(".{FILE}.presence-v1"))).unwrap()
                }
                other => std::fs::write(
                    &path,
                    match other {
                        "empty" => "",
                        "null" => "null",
                        "array" => "[]",
                        "schema" => "[{}]",
                        _ => "{",
                    },
                )
                .unwrap(),
            };
            let before = std::fs::read(&path).ok();
            let r = run(
                &root,
                "/api/at",
                "later-request",
                b"new mutation",
                "module-2".into(),
                || async { panic!("corrupt archive dispatched") },
            )
            .await;
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE, "{damage}");
            assert_eq!(std::fs::read(&path).ok(), before);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn submitted_uncertainty_is_not_reclassified_as_completed_by_http_status() {
        if isolated_receipt_case(
            "submitted_uncertainty_is_not_reclassified_as_completed_by_http_status",
        ) {
            return;
        }
        let root = root();
        let r = run(
            &root,
            "/api/module/flight-mode",
            "uncertain-status-request",
            b"target",
            "module-1".into(),
            || async {
                crate::json_response(
                    StatusCode::CONFLICT,
                    &json!({"uncertain":true,"submitted":true,"error":"late owner change"}),
                )
            },
        )
        .await;
        assert_eq!(value(r).await["mutation_receipt"]["outcome"], "unknown");
        let r = run(
            &root,
            "/api/module/flight-mode",
            "uncertain-status-request",
            b"target",
            "module-2".into(),
            || async { panic!("uncertain receipt dispatched") },
        )
        .await;
        assert_eq!(value(r).await["mutation_receipt"]["outcome"], "unknown");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn strict_identity_rejects_missing_null_type_whitespace_and_oversized_values() {
        for body in [
            json!({}),
            json!({"client_request_id":null}),
            json!({"client_request_id":7}),
            json!({"client_request_id":" short "}),
            json!({"client_request_id":"x".repeat(129)}),
        ] {
            assert!(split(&serde_json::to_vec(&body).unwrap(), true).is_err());
        }
        assert!(
            split(
                br#"{"client_request_id":"valid-request-123","command":"AT+CSIM=4,FFFF"}"#,
                true
            )
            .is_ok()
        );
    }
    #[tokio::test]
    async fn full_ledger_keeps_terminal_identities_and_still_replays_original() {
        if isolated_receipt_case("full_ledger_keeps_terminal_identities_and_still_replays_original")
        {
            return;
        }
        let root = root();
        let _ = run(
            &root,
            "/api/at",
            "old-request-0000",
            b"old",
            "module-1".into(),
            || async { crate::json_response(StatusCode::OK, &json!({"ok":true})) },
        )
        .await;
        let store = ArchiveStore::new(root.join(FILE))
            .require_nonempty_ledger()
            .unwrap();
        store
            .mutate(|items| {
                let mut ledger = load(items)?;
                let original = ledger.entries["old-request-0000"].clone();
                for i in 1..LIMIT {
                    let id = format!("filled-request-{i:05}");
                    let mut r = original.clone();
                    r.sequence = (i + 1) as u64;
                    annotate(&mut r.body, &id, &r.binding, "completed");
                    ledger.entries.insert(id, r);
                }
                ledger.sequence = LIMIT as u64;
                save(items, &ledger)
            })
            .unwrap();
        let before = std::fs::read(root.join(FILE)).unwrap();
        assert_eq!(
            run(
                &root,
                "/api/at",
                "overflow-request",
                b"extra",
                "g2".into(),
                || async { panic!("full ledger dispatched") }
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            run(
                &root,
                "/api/at",
                "old-request-0000",
                b"old",
                "g2".into(),
                || async { panic!("old receipt evicted") }
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(before, std::fs::read(root.join(FILE)).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
