//! Best-effort, allowlisted support telemetry. Never a business/audit ledger.
//! No user text, request bodies, query strings, identities, credentials or paths.
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use nix::errno::Errno;
use nix::fcntl::{OFlag, openat, renameat};
use nix::sys::stat::{Mode, mkdirat};
use nix::unistd::{UnlinkatFlags, geteuid, unlinkat};
use serde::{Deserialize, Serialize};

pub(crate) mod module;
pub(crate) mod recovery;
mod live;

pub(crate) const PATH: &str = "/api/diagnostics/support";
const DIRECTORY: &str = "support-diagnostics";
const NAMES: [&str; 4] = [
    "events.0.jsonl",
    "events.1.jsonl",
    "events.2.jsonl",
    "events.3.jsonl",
];
const QUEUE: usize = 256;
const RECORD_BYTES: usize = 4096;
const SEGMENT_BYTES: usize = 2 * 1024 * 1024;
const EXPORT_BYTES: usize = 2 * 1024 * 1024;
const EXPORT_EVENTS: usize = 4096;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u8,
    runtime_id: String,
    sequence: u64,
    time_ms: u64,
    event: Event,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Lifecycle {
        phase: String,
    },
    Api {
        route: String,
        method: String,
        status: u16,
        code: String,
        duration_ms: u64,
        correlation_id: String,
    },
    SafeEvent {
        detail: crate::safe_events::Event,
    },
    Module {
        detail: module::Event,
    },
}

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
    write_failures: AtomicU64,
}
struct Shared {
    live: Mutex<live::Buffer>,
    live_missed: AtomicU64,
    root: PathBuf,
    runtime_id: String,
    sequence: AtomicU64,
    counters: Counters,
    available: AtomicBool,
    exporting: AtomicBool,
    recovery: Mutex<recovery::Observation>,
}
struct Recorder {
    sender: mpsc::SyncSender<Record>,
    shared: Arc<Shared>,
}
static RECORDER: OnceLock<Recorder> = OnceLock::new();

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn hex_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn status_code(status: u16) -> &'static str {
    match status {
        0 => "request_cancelled",
        100..=399 => "http_success",
        400..=499 => "http_client_error",
        _ => "http_server_error",
    }
}
fn method(value: &str) -> &'static str {
    match value {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}
fn valid(record: &Record) -> bool {
    if record.schema_version != 1 || !hex_id(&record.runtime_id) || record.sequence == 0 {
        return false;
    }
    match &record.event {
        Event::Lifecycle { phase } => matches!(
            phase.as_str(),
            "startup" | "shutdown_started" | "shutdown_complete" | "shutdown_failed"
        ),
        Event::Api {
            route,
            method: verb,
            status,
            code,
            correlation_id,
            ..
        } => {
            (route == "unmatched"
                || crate::public_http::diagnostic_route(route) == Some(route.as_str()))
                && route != PATH
                && method(verb) == verb.as_str()
                && (*status == 0 || (100..=599).contains(status))
                && code == status_code(*status)
                && hex_id(correlation_id)
        }
        Event::SafeEvent { detail } => crate::safe_events::valid_persisted_event(detail),
        Event::Module { .. } => true, // Every field is a closed enum or bounded integer.
    }
}
impl Recorder {
    fn emit(&self, event: Event) {
        let record = Record {
            schema_version: 1,
            runtime_id: self.shared.runtime_id.clone(),
            sequence: self
                .shared
                .sequence
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1),
            time_ms: now_ms(),
            event,
        };
        if !valid(&record) {
            self.shared.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        live::record(&self.shared, &record);
        if self.sender.try_send(record).is_err() {
            self.shared.counters.dropped.fetch_add(1, Ordering::Relaxed);
        } else {
            self.shared
                .counters
                .accepted
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Called only after the daemon acquired its lifetime root lease. File work is
/// confined to a detached thread; no flush/join is added to runtime shutdown.
pub(crate) fn initialize(root: &Path) {
    if RECORDER.get().is_some() {
        return;
    }
    let Ok(mut runtime_id) = crate::multistandby_store::random_token() else {
        return;
    };
    runtime_id.truncate(32);
    if !hex_id(&runtime_id) {
        return;
    }
    let shared = Arc::new(Shared {
        root: root.to_path_buf(),
        runtime_id,
        sequence: AtomicU64::new(0),
        counters: Counters::default(),
        available: AtomicBool::new(false),
        exporting: AtomicBool::new(false),
        recovery: Mutex::new(recovery::Observation::default()),
        live: Mutex::new(live::Buffer::default()),
        live_missed: AtomicU64::new(0),
    });
    let (sender, receiver) = mpsc::sync_channel(QUEUE);
    let worker = shared.clone();
    if std::thread::Builder::new()
        .name("support-diagnostics".into())
        .spawn(move || {
            let mut storage = Storage::open(&worker.root, SEGMENT_BYTES);
            for record in receiver {
                write_record(&mut storage, &worker, &record);
            }
        })
        .is_err()
    {
        shared
            .counters
            .write_failures
            .fetch_add(1, Ordering::Relaxed);
    }
    let _ = RECORDER.set(Recorder { sender, shared });
    lifecycle("startup");
}

fn write_record(storage: &mut io::Result<Storage>, shared: &Shared, record: &Record) {
    let result = match storage {
        Ok(storage) => storage.append(record),
        Err(_) => Err(invalid()),
    };
    if result.is_ok() {
        shared.available.store(true, Ordering::Relaxed);
        shared.counters.written.fetch_add(1, Ordering::Relaxed);
    } else {
        shared.available.store(false, Ordering::Relaxed);
        shared
            .counters
            .write_failures
            .fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn lifecycle(phase: &'static str) {
    if let Some(recorder) = RECORDER.get() {
        recorder.emit(Event::Lifecycle {
            phase: phase.into(),
        });
    }
}
pub(crate) fn safe_event(detail: crate::safe_events::Event) {
    if let Some(recorder) = RECORDER.get() {
        recorder.emit(Event::SafeEvent { detail });
    }
}

pub(crate) struct ApiSpan {
    recorder: &'static Recorder,
    route: &'static str,
    method: &'static str,
    correlation_id: String,
    started: Instant,
    finished: bool,
}
pub(crate) fn api_span(verb: &str, path: &str, supplied_id: Option<&str>) -> Option<ApiSpan> {
    if path == PATH || !path.starts_with("/api/") || path.starts_with("/api/internal") {
        return None;
    }
    let recorder = RECORDER.get()?;
    let correlation_id = match supplied_id.filter(|value| hex_id(value)) {
        Some(value) => value.to_owned(),
        None => {
            let mut value = crate::multistandby_store::random_token().ok()?;
            value.truncate(32);
            if !hex_id(&value) {
                return None;
            }
            value
        }
    };
    Some(ApiSpan {
        recorder,
        route: crate::public_http::diagnostic_route(path).unwrap_or("unmatched"),
        method: method(verb),
        correlation_id,
        started: Instant::now(),
        finished: false,
    })
}
impl ApiSpan {
    fn record(&self, status: u16) {
        self.recorder.emit(Event::Api {
            route: self.route.into(),
            method: self.method.into(),
            status,
            code: status_code(status).into(),
            duration_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            correlation_id: self.correlation_id.clone(),
        });
    }
    pub(crate) fn finish(mut self, status: u16) {
        self.finished = true;
        self.record(status);
    }
}
impl Drop for ApiSpan {
    fn drop(&mut self) {
        if !self.finished {
            self.record(0);
        }
    }
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}
fn directory_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK
}
fn open_root(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(invalid());
    }
    let mut directory = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = File::from(
                    openat(&directory, name, directory_flags(), Mode::empty())
                        .map_err(io::Error::from)?,
                );
            }
            _ => return Err(invalid()),
        }
    }
    Ok(directory)
}
fn open_directory(root: &Path, create: bool) -> io::Result<File> {
    let root = open_root(root)?;
    if create {
        match mkdirat(&root, DIRECTORY, Mode::S_IRWXU) {
            Ok(()) | Err(Errno::EEXIST) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let directory = File::from(
        openat(&root, DIRECTORY, directory_flags(), Mode::empty()).map_err(io::Error::from)?,
    );
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(invalid());
    }
    Ok(directory)
}
fn open_segment(directory: &File, name: &str, write: bool) -> io::Result<File> {
    let access = if write {
        OFlag::O_WRONLY | OFlag::O_APPEND | OFlag::O_CREAT
    } else {
        OFlag::O_RDONLY
    };
    let file = File::from(
        openat(
            directory,
            name,
            access | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(io::Error::from)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(invalid());
    }
    Ok(file)
}

struct Storage {
    directory: File,
    limit: usize,
}
impl Storage {
    fn open(root: &Path, limit: usize) -> io::Result<Self> {
        Ok(Self {
            directory: open_directory(root, true)?,
            limit,
        })
    }
    fn append(&mut self, record: &Record) -> io::Result<()> {
        if !valid(record) {
            return Err(invalid());
        }
        let mut bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() > RECORD_BYTES || bytes.len() > self.limit {
            return Err(invalid());
        }
        // Inspect every rotation before touching any entry; no symlink, hardlink,
        // special file, permissive file, or oversized segment can be adopted.
        for name in NAMES {
            match open_segment(&self.directory, name, false) {
                Ok(file) if file.metadata()?.len() <= self.limit as u64 => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(invalid()),
            }
        }
        let mut current = open_segment(&self.directory, NAMES[0], true)?;
        if current.metadata()?.len() + bytes.len() as u64 > self.limit as u64 {
            drop(current);
            match unlinkat(&self.directory, NAMES[3], UnlinkatFlags::NoRemoveDir) {
                Ok(()) | Err(Errno::ENOENT) => {}
                Err(error) => return Err(error.into()),
            }
            for index in (0..3).rev() {
                match renameat(
                    &self.directory,
                    NAMES[index],
                    &self.directory,
                    NAMES[index + 1],
                ) {
                    Ok(()) | Err(Errno::ENOENT) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            current = open_segment(&self.directory, NAMES[0], true)?;
        }
        current.write_all(&bytes)
    }
}

fn snapshot(shared: &Shared) -> serde_json::Value {
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    let mut bytes_used = 0usize;
    let mut invalid_records = 0u64;
    let mut read_failures = 0u64;
    let mut truncated = false;
    match open_directory(&shared.root, false) {
        Ok(directory) => {
            'segments: for name in NAMES {
                let file = match open_segment(&directory, name, false) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        read_failures += 1;
                        continue;
                    }
                };
                let mut bytes = Vec::new();
                if file
                    .take((SEGMENT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .is_err()
                    || bytes.len() > SEGMENT_BYTES
                {
                    read_failures += 1;
                    continue;
                }
                let mut lines = bytes.split_inclusive(|byte| *byte == b'\n').rev();
                for line in &mut lines {
                    if line.len() > RECORD_BYTES || !line.ends_with(b"\n") {
                        invalid_records += 1;
                        continue;
                    }
                    let record = match serde_json::from_slice::<Record>(line) {
                        Ok(record) if valid(&record) => record,
                        _ => {
                            invalid_records += 1;
                            continue;
                        }
                    };
                    if seen.contains(&(record.runtime_id.clone(), record.sequence)) {
                        continue;
                    }
                    if events.len() >= EXPORT_EVENTS
                        || bytes_used + line.len() > EXPORT_BYTES - 32768
                    {
                        truncated = true;
                        break 'segments;
                    }
                    bytes_used += line.len();
                    seen.insert((record.runtime_id.clone(), record.sequence));
                    events.push(record);
                }
            }
        }
        Err(_) => read_failures += 1,
    }
    events.reverse(); // Chronological display of the most recent bounded window.
    let counters = &shared.counters;
    let dropped = counters.dropped.load(Ordering::Relaxed);
    let failures = counters.write_failures.load(Ordering::Relaxed);
    let mut warnings = vec![
        "best_effort_not_lossless",
        "pending_queue_not_flushed",
        "snapshot_not_atomic",
        "current_runtime_counters",
    ];
    if truncated {
        warnings.push("export_truncated");
    }
    if invalid_records != 0 {
        warnings.push("invalid_records_skipped");
    }
    if read_failures != 0 {
        warnings.push("read_failed");
    }
    if dropped != 0 {
        warnings.push("records_dropped");
    }
    if failures != 0 {
        warnings.push("write_failed");
    }
    if !shared.available.load(Ordering::Relaxed) {
        warnings.push("writer_unavailable");
    }
    serde_json::json!({
        "schema_version":1, "generated_at_ms":now_ms(),
        "version":crate::APP_RELEASE_VERSION, "build":crate::APP_BUILD_NUMBER,
        "source_digest":crate::SOURCE_DIGEST, "runtime_id":shared.runtime_id,
        "limits":{"queue_capacity":QUEUE,"max_record_bytes":RECORD_BYTES,
            "segment_bytes":SEGMENT_BYTES,"segments":NAMES.len(),
            "export_bytes":EXPORT_BYTES,"max_events":EXPORT_EVENTS},
        "counters":{"accepted":counters.accepted.load(Ordering::Relaxed),
            "written":counters.written.load(Ordering::Relaxed),"dropped":dropped,
            "write_failures":failures,"invalid_records":invalid_records,"read_failures":read_failures},
        "module_recovery":shared.recovery.lock().unwrap_or_else(|error| error.into_inner()).snapshot(),
        "events":events,"warnings":warnings
    })
}
struct ExportPermit(Arc<Shared>);
impl Drop for ExportPermit {
    fn drop(&mut self) {
        self.0.exporting.store(false, Ordering::Release);
    }
}
fn unavailable() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &serde_json::json!({
            "code":"support_snapshot_unavailable","error":"Support snapshot unavailable","retryable":true
        }),
    )
}
type ExportWork = Box<dyn FnOnce() + Send + 'static>;

fn spawn_export(work: ExportWork) -> io::Result<()> {
    std::thread::Builder::new()
        .name("support-export".into())
        .spawn(work)
        .map(|_| ())
}

/// A single detached reader survives request cancellation without admitting a
/// second reader. Its bounded response wait never joins a stalled filesystem.
pub(crate) async fn handle() -> Response<Full<Bytes>> {
    let Some(recorder) = RECORDER.get() else {
        return unavailable();
    };
    export_snapshot(
        recorder.shared.clone(),
        Duration::from_secs(3),
        |shared| serde_json::to_vec(&snapshot(shared)).ok(),
        spawn_export,
    )
    .await
}


/// The live view is bounded current-process memory only. It never reads the
/// rotating export files or waits for a writer, device or recovery owner.
pub(crate) fn handle_live() -> Response<Full<Bytes>> {
    let Some(recorder) = RECORDER.get() else {
        return unavailable();
    };
    live::response(&recorder.shared)
}

// Injectable effects keep admission, thread ownership, timeout, response limits
// and permit retirement identical in the production path and deterministic tests.
async fn export_snapshot<ReadSnapshot, Spawn>(
    shared: Arc<Shared>,
    wait: Duration,
    read_snapshot: ReadSnapshot,
    spawn: Spawn,
) -> Response<Full<Bytes>>
where
    ReadSnapshot: FnOnce(&Shared) -> Option<Vec<u8>> + Send + 'static,
    Spawn: FnOnce(ExportWork) -> io::Result<()>,
{
    if shared
        .exporting
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return unavailable();
    }
    let permit = ExportPermit(shared);
    let (send, receive) = tokio::sync::oneshot::channel();
    if spawn(Box::new(move || {
        let bytes = read_snapshot(&permit.0).filter(|bytes| bytes.len() <= EXPORT_BYTES);
        let _ = send.send(bytes);
        drop(permit);
    }))
    .is_err()
    {
        return unavailable();
    }
    match tokio::time::timeout(wait, receive).await {
        Ok(Ok(Some(bytes))) => Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::CACHE_CONTROL, "no-store")
            .body(Full::new(Bytes::from(bytes)))
            .expect("static support response"),
        _ => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "switch-support-{}",
                crate::multistandby_store::random_token().unwrap()
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn shared(root: &Root) -> Arc<Shared> {
        Arc::new(Shared {
            root: root.0.clone(),
            runtime_id: "a".repeat(32),
            sequence: AtomicU64::new(0),
            counters: Counters::default(),
            available: AtomicBool::new(true),
            exporting: AtomicBool::new(false),
            recovery: Mutex::new(recovery::Observation::default()),
        live: Mutex::new(live::Buffer::default()),
        live_missed: AtomicU64::new(0),
        })
    }
    fn record(sequence: u64) -> Record {
        Record {
            schema_version: 1,
            runtime_id: "a".repeat(32),
            sequence,
            time_ms: 1,
            event: Event::Lifecycle {
                phase: "startup".into(),
            },
        }
    }
    #[test]
    fn rotates_with_hard_bounds_preserves_restart_and_private_permissions() {
        let root = Root::new();
        let mut storage = Storage::open(&root.0, 512).unwrap();
        for sequence in 1..=30 {
            storage.append(&record(sequence)).unwrap();
        }
        drop(storage);
        let mut restarted = Storage::open(&root.0, 512).unwrap();
        let mut next = record(1);
        next.runtime_id = "b".repeat(32);
        restarted.append(&next).unwrap();
        let directory = root.0.join(DIRECTORY);
        assert_eq!(
            std::fs::metadata(&directory).unwrap().mode() & 0o7777,
            0o700
        );
        let mut total = 0;
        for name in NAMES {
            let metadata = std::fs::metadata(directory.join(name)).unwrap();
            assert!(metadata.len() <= 512);
            assert_eq!(metadata.mode() & 0o7777, 0o600);
            total += metadata.len();
        }
        assert!(total <= 4 * 512);
        let value = snapshot(&shared(&root));
        let events = value["events"].as_array().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event["runtime_id"] == "a".repeat(32))
        );
        assert!(
            events
                .iter()
                .any(|event| event["runtime_id"] == "b".repeat(32))
        );
    }
    #[test]
    fn recovery_survives_rotation_queue_failure_and_absent_module_events() {
        let root = Root::new();
        let shared = shared(&root);
        let gate = crate::module_recovery::Gate::quarantined_for_tests();
        shared.recovery.lock().unwrap().bind(&gate);
        let mut storage = Storage::open(&root.0, 512).unwrap();
        for sequence in 1..=40 {
            storage.append(&record(sequence)).unwrap();
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let recorder = Recorder {
            sender,
            shared: shared.clone(),
        };
        recorder.emit(Event::Lifecycle {
            phase: "startup".into(),
        });
        assert_eq!(shared.counters.dropped.load(Ordering::Relaxed), 1);
        let value = snapshot(&shared);
        assert_eq!(
            value["module_recovery"]["state"],
            "legacy_recovery_required"
        );
        assert_eq!(
            value["module_recovery"]["reason_codes"],
            serde_json::json!(["legacy_call_mode_lock"])
        );
        assert_eq!(
            value["module_recovery"]["startup"]["cleanup"],
            "not_observed"
        );
        assert_eq!(
            value["module_recovery"]["historical_recovery_confirmed"],
            false
        );
        let events = value["events"].as_array().unwrap();
        assert!(!events.is_empty());
        assert!(events.len() < 40);
        assert!(
            events
                .iter()
                .all(|event| event["event"]["kind"] != "module")
        );
        assert!(!value.to_string().contains(root.0.to_str().unwrap()));
    }

    #[test]
    fn recovery_is_outside_export_event_limit_and_observes_later_revocation() {
        let root = Root::new();
        let shared = shared(&root);
        let gate = crate::module_recovery::Gate::default();
        shared.recovery.lock().unwrap().bind(&gate);
        let mut storage = Storage::open(&root.0, SEGMENT_BYTES).unwrap();
        for sequence in 1..=(EXPORT_EVENTS as u64 + 1) {
            storage.append(&record(sequence)).unwrap();
        }
        let value = snapshot(&shared);
        assert_eq!(value["events"].as_array().unwrap().len(), EXPORT_EVENTS);
        assert_eq!(value["module_recovery"]["state"], "open");
        assert!(
            value["warnings"]
                .as_array()
                .unwrap()
                .contains(&"export_truncated".into())
        );
        gate.revoke_for_tests();
        let value = snapshot(&shared);
        assert_eq!(value["events"].as_array().unwrap().len(), EXPORT_EVENTS);
        assert_eq!(
            value["module_recovery"]["reason_codes"],
            serde_json::json!(["legacy_execution_pending"])
        );
        assert_eq!(
            value["module_recovery"]["state"],
            "legacy_recovery_required"
        );
    }

    #[test]
    fn corruption_and_private_data_are_never_exported() {
        let root = Root::new();
        let mut storage = Storage::open(&root.0, SEGMENT_BYTES).unwrap();
        storage.append(&record(1)).unwrap();
        let file = root.0.join(DIRECTORY).join(NAMES[0]);
        let mut file = std::fs::OpenOptions::new().append(true).open(file).unwrap();
        file.write_all(b"{\"token\":\"secret-phone-123\"}\nmalformed\n")
            .unwrap();
        let mut bad = record(2);
        bad.event = Event::Lifecycle {
            phase: "secret-phone-123".into(),
        };
        assert!(storage.append(&bad).is_err());
        let value = snapshot(&shared(&root));
        assert_eq!(value["events"].as_array().unwrap().len(), 1);
        assert_eq!(value["counters"]["invalid_records"], 2);
        assert!(!value.to_string().contains("secret-phone"));
        assert!(!value.to_string().contains(root.0.to_str().unwrap()));
    }
    #[test]
    fn rejects_symlinks_hardlinks_special_files_and_permissive_entries() {
        let root = Root::new();
        let target = root.0.join("outside");
        std::fs::write(&target, b"untouched").unwrap();
        symlink(&root.0, root.0.join(DIRECTORY)).unwrap();
        assert!(Storage::open(&root.0, SEGMENT_BYTES).is_err());
        std::fs::remove_file(root.0.join(DIRECTORY)).unwrap();
        let mut storage = Storage::open(&root.0, SEGMENT_BYTES).unwrap();
        let active = root.0.join(DIRECTORY).join(NAMES[0]);
        symlink(&target, &active).unwrap();
        assert!(storage.append(&record(1)).is_err());
        std::fs::remove_file(&active).unwrap();
        std::fs::hard_link(&target, &active).unwrap();
        assert!(storage.append(&record(1)).is_err());
        std::fs::remove_file(&active).unwrap();
        nix::unistd::mkfifo(&active, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        assert!(storage.append(&record(1)).is_err());
        std::fs::remove_file(&active).unwrap();
        std::fs::write(&active, b"").unwrap();
        std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(storage.append(&record(1)).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    }
    #[test]
    fn write_failure_only_changes_observability_counters() {
        let root = Root::new();
        let shared = shared(&root);
        let mut storage = Err(invalid());
        write_record(&mut storage, &shared, &record(1));
        assert_eq!(shared.counters.write_failures.load(Ordering::Relaxed), 1);
        assert_eq!(shared.counters.written.load(Ordering::Relaxed), 0);
        assert!(!shared.available.load(Ordering::Relaxed));
        let mut storage = Storage::open(&root.0, SEGMENT_BYTES);
        write_record(&mut storage, &shared, &record(2));
        assert_eq!(shared.counters.written.load(Ordering::Relaxed), 1);
        assert!(shared.available.load(Ordering::Relaxed));
    }
    #[test]
    fn queue_saturation_and_disconnect_are_nonblocking_and_counted() {
        let root = Root::new();
        let (sender, receiver) = mpsc::sync_channel(QUEUE);
        let recorder = Recorder {
            sender,
            shared: shared(&root),
        };
        for _ in 0..QUEUE + 9 {
            recorder.emit(Event::Lifecycle {
                phase: "startup".into(),
            });
        }
        assert_eq!(
            recorder.shared.counters.accepted.load(Ordering::Relaxed),
            QUEUE as u64
        );
        assert_eq!(recorder.shared.counters.dropped.load(Ordering::Relaxed), 9);
        drop(receiver);
        recorder.emit(Event::Lifecycle {
            phase: "startup".into(),
        });
        assert_eq!(recorder.shared.counters.dropped.load(Ordering::Relaxed), 10);
    }
    #[test]
    fn dynamic_routes_and_error_fields_use_only_allowlisted_values() {
        assert_eq!(
            crate::public_http::diagnostic_route("/api/v2/sms-export/private-secret"),
            Some("/api/v2/sms-export/{token}")
        );
        let mut row = record(1);
        row.event = Event::Api {
            route: "/api/v2/sms-export/private-secret".into(),
            method: "GET".into(),
            status: 400,
            code: "http_client_error".into(),
            duration_ms: 2,
            correlation_id: "b".repeat(32),
        };
        assert!(!valid(&row));
        if let Event::Api { route, .. } = &mut row.event {
            *route = "/api/v2/sms-export/{token}".into();
        }
        assert!(valid(&row));
        if let Event::Api { code, .. } = &mut row.event {
            *code = "raw-password".into();
        }
        assert!(!valid(&row));
    }
    #[test]
    fn export_limit_and_oversized_records_remain_bounded() {
        let root = Root::new();
        let mut storage = Storage::open(&root.0, SEGMENT_BYTES).unwrap();
        for sequence in 1..=EXPORT_EVENTS as u64 + 10 {
            storage.append(&record(sequence)).unwrap();
        }
        let value = snapshot(&shared(&root));
        assert_eq!(value["events"].as_array().unwrap().len(), EXPORT_EVENTS);
        assert!(
            value["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "export_truncated")
        );
        assert_eq!(
            value["events"].as_array().unwrap().last().unwrap()["sequence"],
            EXPORT_EVENTS as u64 + 10
        );
        assert!(serde_json::to_vec(&value).unwrap().len() <= EXPORT_BYTES);
        let file = open_segment(&storage.directory, NAMES[0], true).unwrap();
        let mut file = file;
        file.write_all(&vec![b'x'; RECORD_BYTES + 1]).unwrap();
        file.write_all(b"\n").unwrap();
        // A fresh bounded export cannot leak any malformed payload.
        assert!(!snapshot(&shared(&root)).to_string().contains("xxxx"));
    }
    async fn wait_for_export_retirement(shared: &Shared) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while shared.exporting.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("released reader must retire its permit");
    }

    async fn assert_export_available(shared: Arc<Shared>) {
        let response = export_snapshot(
            shared.clone(),
            Duration::from_secs(1),
            |_| Some(b"{}".to_vec()),
            spawn_export,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[hyper::header::CACHE_CONTROL], "no-store");
        wait_for_export_retirement(&shared).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_export_timeout_or_cancellation_keeps_singleflight_until_reader_retires() {
        for cancel in [false, true] {
            let root = Root::new();
            let shared = shared(&root);
            let (started_send, started_receive) = tokio::sync::oneshot::channel();
            let (release_send, release_receive) = mpsc::channel::<()>();
            let waiter = tokio::spawn(export_snapshot(
                shared.clone(),
                if cancel {
                    Duration::from_secs(30)
                } else {
                    Duration::from_millis(25)
                },
                move |_| {
                    let _ = started_send.send(());
                    // A finite emergency bound prevents a broken test from
                    // retaining an OS thread forever; normal release is explicit.
                    release_receive
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    Some(b"{}".to_vec())
                },
                spawn_export,
            ));
            tokio::time::timeout(Duration::from_secs(2), started_receive)
                .await
                .unwrap()
                .unwrap();
            assert!(shared.exporting.load(Ordering::Acquire));
            if cancel {
                waiter.abort();
                assert!(waiter.await.unwrap_err().is_cancelled());
            } else {
                let response = tokio::time::timeout(Duration::from_secs(2), waiter)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
            // The HTTP waiter is gone, while the actual reader is still held.
            assert!(shared.exporting.load(Ordering::Acquire));
            let second = export_snapshot(
                shared.clone(),
                Duration::from_secs(1),
                |_| panic!("singleflight admitted a second read"),
                |_| panic!("singleflight spawned a second reader"),
            )
            .await;
            assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
            release_send.send(()).unwrap();
            wait_for_export_retirement(&shared).await;
            assert_export_available(shared).await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn export_spawn_failure_releases_singleflight_without_running_reader() {
        let root = Root::new();
        let shared = shared(&root);
        let response = export_snapshot(
            shared.clone(),
            Duration::from_secs(1),
            |_| panic!("failed spawn must not run a reader"),
            |work| {
                drop(work);
                Err(io::Error::other("isolated thread creation failure"))
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!shared.exporting.load(Ordering::Acquire));
        assert_export_available(shared).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_or_oversized_export_releases_singleflight() {
        let root = Root::new();
        let shared = shared(&root);
        for payload in [None, Some(vec![b'x'; EXPORT_BYTES + 1])] {
            let response = export_snapshot(
                shared.clone(),
                Duration::from_secs(1),
                move |_| payload,
                spawn_export,
            )
            .await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            wait_for_export_retirement(&shared).await;
        }
        assert_export_available(shared).await;
    }
}
