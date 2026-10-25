//! Read-only, bounded SMS exports. Each source is pinned at its own atomic-file
//! generation. No notification, forwarding, unread or history cursor is changed.
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_store::{ArchiveStore, StoreError};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const PREFIX: &str = "/api/v2/sms-export";
const CHUNK: usize = 256 * 1024;
const MAX_RECORD: u64 = 1024 * 1024;
const MAX_OUTPUT: u64 = 128 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(600);
const MAX_EXPORTS: usize = 4;
const MAX_MESSAGE_KEYS: usize = 1_000_000;
const MAX_TIME: u64 = 253_402_300_799_999;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Source {
    #[default]
    All,
    Device,
    Nwait,
}
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Privacy {
    #[default]
    Masked,
    Full,
}
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    #[serde(default)]
    source: Source,
    #[serde(default)]
    privacy: Privacy,
    device: Option<String>,
    profile_id: Option<String>,
    subscription_id: Option<String>,
    peer: Option<String>,
    from_ms: Option<u64>,
    to_ms: Option<u64>,
}
impl Options {
    fn validate(&self) -> Result<(), StoreError> {
        if self
            .device
            .as_deref()
            .is_some_and(|s| !matches!(s, "dji" | "reader"))
            || self
                .profile_id
                .as_ref()
                .is_some_and(|s| !valid_text(s, 128))
            || self.peer.as_ref().is_some_and(|s| !valid_text(s, 256))
            || self.subscription_id.as_ref().is_some_and(|s| {
                s.len() != 69
                    || !s.starts_with("sub1_")
                    || !s[5..]
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || self.from_ms.is_some_and(|v| v > MAX_TIME)
            || self.to_ms.is_some_and(|v| v > MAX_TIME)
            || matches!((self.from_ms,self.to_ms), (Some(a),Some(b)) if a>b)
        {
            return Err(failure("sms_export_invalid_request"));
        }
        Ok(())
    }
}
fn valid_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.trim() == s && !s.chars().any(char::is_control)
}
fn failure(code: &str) -> StoreError {
    StoreError::Configuration(code.into())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

struct Entry {
    file: File,
    bytes: u64,
    deadline: Instant,
    _slot: Slot,
}
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Default)]
struct Exports {
    entries: Mutex<BTreeMap<String, Entry>>,
    slots: Arc<AtomicUsize>,
}
impl Exports {
    fn cleanup(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|_, e| Instant::now() < e.deadline);
        }
    }
    fn slot(&self) -> Result<Slot, StoreError> {
        self.cleanup();
        self.slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_EXPORTS).then_some(n + 1)
            })
            .map_err(|_| failure("sms_export_busy"))?;
        Ok(Slot(self.slots.clone()))
    }
    #[cfg(test)]
    fn prepare(&self, root: &Path, options: Options) -> Result<Value, StoreError> {
        let nwait = crate::multistandby_store::Store::open(root)?;
        self.prepare_guarded(&nwait, options)
    }
    fn prepare_guarded(
        &self,
        nwait: &crate::multistandby_store::Store,
        options: Options,
    ) -> Result<Value, StoreError> {
        options.validate()?;
        let slot = self.slot()?;
        let token = crate::multistandby_store::random_token()?;
        let (file, count) = build_guarded(nwait, &options, &token)?;
        let bytes = file.metadata().map_err(StoreError::Io)?.len();
        let expires = now_ms() + TTL.as_millis() as u64;
        self.entries
            .lock()
            .map_err(|_| failure("sms_export_failed"))?
            .insert(
                token.clone(),
                Entry {
                    file,
                    bytes,
                    deadline: Instant::now() + TTL,
                    _slot: slot,
                },
            );
        Ok(
            json!({"schema_version":1,"token":token,"total_bytes":bytes,"message_count":count,"chunk_bytes":CHUNK,"expires_at_ms":expires}),
        )
    }
    fn chunk(&self, token: &str, offset: u64) -> Result<Vec<u8>, StoreError> {
        self.cleanup();
        let entries = self
            .entries
            .lock()
            .map_err(|_| failure("sms_export_failed"))?;
        let entry = entries
            .get(token)
            .ok_or_else(|| failure("sms_export_expired"))?;
        if offset > entry.bytes {
            return Err(failure("sms_export_invalid_request"));
        }
        let mut bytes = vec![0; (entry.bytes - offset).min(CHUNK as u64) as usize];
        let file = entry.file.try_clone().map_err(StoreError::Io)?;
        // Keep registry/TTL/deletion independent of a slow filesystem read.
        // An admitted chunk owns its descriptor even if deletion races it.
        drop(entries);
        let mut read = 0;
        while read < bytes.len() {
            let n = file
                .read_at(&mut bytes[read..], offset + read as u64)
                .map_err(StoreError::Io)?;
            if n == 0 {
                return Err(failure("sms_export_failed"));
            }
            read += n;
        }
        Ok(bytes)
    }
    fn delete(&self, token: &str) -> Result<(), StoreError> {
        self.entries
            .lock()
            .map_err(|_| failure("sms_export_failed"))?
            .remove(token);
        Ok(())
    }
}
fn schedule_cleanup() {
    static START: Once = Once::new();
    START.call_once(|| {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                exports().cleanup();
            }
        });
    });
}
fn exports() -> &'static Exports {
    static EXPORTS: OnceLock<Exports> = OnceLock::new();
    EXPORTS.get_or_init(Exports::default)
}
pub(crate) fn is_route(path: &str) -> bool {
    path == PREFIX || path.starts_with(&format!("{PREFIX}/"))
}
fn valid_token(s: &str) -> bool {
    s.len() >= 32
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    nwait: crate::multistandby_store::Store,
) -> Response<Full<Bytes>> {
    let path = request.uri().path().to_owned();
    if request.method() == Method::POST && path == PREFIX {
        if request.uri().query().is_some() {
            return error(failure("sms_export_invalid_request"));
        }
        let bytes = match crate::http_server::collect_body(request.into_body(), 4096).await {
            Ok(bytes) => bytes,
            Err(crate::http_server::BodyReadError::Timeout) => {
                return crate::http_server::body_timeout_response();
            }
            Err(_) => return error(failure("sms_export_invalid_request")),
        };
        let options: Options = match serde_json::from_slice(&bytes) {
            Ok(o) => o,
            Err(_) => return error(failure("sms_export_invalid_request")),
        };
        if let Err(e) = options.validate() {
            return error(e);
        }
        if options.source != Source::Device {
            if let Some(response) = crate::sms_store_health::rejection(nwait.status()) {
                return response;
            }
        }
        // Start the single reaper before spawning the blocking preparation.
        // A disconnected/cancelled request may never observe its completed token.
        schedule_cleanup();
        let result = crate::archive_io::run(move || {
            if options.source != Source::Device && nwait.status().restart_required {
                return Err(crate::sms_store_health::unavailable());
            }
            exports().prepare_guarded(&nwait, options)
        })
        .await;
        return match result {
            Ok(value) => crate::json_response(StatusCode::OK, &value),
            Err(e) => error(e),
        };
    }
    let Some(token) = path
        .strip_prefix(&format!("{PREFIX}/"))
        .filter(|s| valid_token(s))
    else {
        return error(failure("sms_export_invalid_request"));
    };
    let token = token.to_owned();
    if request.method() == Method::GET {
        let offset = request
            .uri()
            .query()
            .and_then(|q| q.strip_prefix("offset="))
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u64>().ok());
        let Some(offset) = offset else {
            return error(failure("sms_export_invalid_request"));
        };
        return match crate::archive_io::run(move || exports().chunk(&token, offset)).await {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/octet-stream")
                .header("Cache-Control", "no-store")
                .body(Full::new(Bytes::from(bytes)))
                .expect("fixed response"),
            Err(e) => error(e),
        };
    }
    if request.method() == Method::DELETE && request.uri().query().is_none() {
        return match exports().delete(&token) {
            Ok(()) => crate::json_response(StatusCode::OK, &json!({"deleted":true})),
            Err(e) => error(e),
        };
    }
    error(failure("sms_export_invalid_request"))
}
fn error(error: StoreError) -> Response<Full<Bytes>> {
    let code = match &error {
        StoreError::Configuration(c)
            if matches!(
                c.as_str(),
                "sms_export_invalid_request"
                    | "sms_export_busy"
                    | "sms_export_expired"
                    | "sms_store_unavailable"
            ) =>
        {
            c.as_str()
        }
        _ => "sms_export_failed",
    };
    let (status, message) = match code {
        "sms_export_invalid_request" => (StatusCode::BAD_REQUEST, "短信导出参数无效"),
        "sms_export_busy" => (
            StatusCode::TOO_MANY_REQUESTS,
            "已有多个短信导出，请稍后重试",
        ),
        "sms_export_expired" => (StatusCode::GONE, "短信导出已过期，请重新导出"),
        "sms_store_unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "短信归档不可用，未生成导出；请保留原记录并检查诊断",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "短信导出失败，未生成完整文件",
        ),
    };
    crate::json_response(
        status,
        &json!({"code":code,"error":message,"retryable":false}),
    )
}

struct Input {
    source: &'static str,
    file: Option<File>,
    cut: u64,
    limit: u64,
    health: Option<crate::multistandby_store::Store>,
}
#[cfg(test)]
fn pin(root: &Path, source: &'static str) -> Result<Input, StoreError> {
    pin_guarded(&crate::multistandby_store::Store::open(root)?, source)
}
fn pin_guarded(
    nwait: &crate::multistandby_store::Store,
    source: &'static str,
) -> Result<Input, StoreError> {
    let (file, limit, health) = if source == "device" {
        let limit = 8 * 1024 * 1024;
        let archive =
            ArchiveStore::with_limits(nwait.data_root().join("sms-archive.json"), limit, 1000)?;
        (archive.snapshot_file()?, limit, None)
    } else {
        (
            nwait.snapshot_file()?,
            64 * 1024 * 1024,
            Some(nwait.clone()),
        )
    };
    Ok(Input {
        source,
        file,
        cut: now_ms(),
        limit,
        health,
    })
}
fn temporary(root: &Path, token: &str) -> Result<File, StoreError> {
    let dir = root.join("sms-export-tmp");
    match fs::symlink_metadata(&dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
            return Err(failure("sms_export_failed"));
        }
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&dir).map_err(StoreError::Io)?
        }
        Err(e) => return Err(StoreError::Io(e)),
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(StoreError::Io)?;
    let path = dir.join(token);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(StoreError::Io)?;
    // Anonymous descriptor has no recoverable plaintext path after a crash.
    fs::remove_file(&path).map_err(StoreError::Io)?;
    Ok(file)
}
#[cfg(test)]
fn build(root: &Path, options: &Options, token: &str) -> Result<(File, u64), StoreError> {
    build_guarded(
        &crate::multistandby_store::Store::open(root)?,
        options,
        token,
    )
}
fn build_guarded(
    nwait: &crate::multistandby_store::Store,
    options: &Options,
    token: &str,
) -> Result<(File, u64), StoreError> {
    let root = nwait.data_root();
    let mut inputs = Vec::new();
    if options.source != Source::Nwait {
        inputs.push(pin_guarded(nwait, "device")?);
    }
    if options.source != Source::Device {
        inputs.push(pin_guarded(nwait, "nwait")?);
    }
    let file = temporary(root, token)?;
    let mut writer = BufWriter::new(file);
    let meta = json!({"schema_version":1,"privacy":options.privacy,"created_at_ms":now_ms(),"snapshot_consistency":"per_source_atomic_generation","ordering":"source_snapshot_order","source_snapshots":inputs.iter().map(|i|json!({"source":i.source,"captured_at_ms":i.cut})).collect::<Vec<_>>()});
    let mut header = serde_json::to_vec(&meta).map_err(StoreError::Json)?;
    header.pop();
    writer.write_all(&header).map_err(StoreError::Io)?;
    writer
        .write_all(b",\"messages\":[")
        .map_err(StoreError::Io)?;
    let mut sink = Sink {
        writer: &mut writer,
        options,
        count: 0,
        bytes: header.len() as u64 + b",\"messages\":[".len() as u64,
        started: Instant::now(),
        output_failed: false,
    };
    for input in inputs {
        stream(input, &mut sink)?;
    }
    let count = sink.count;
    drop(sink);
    write!(writer, "],\"message_count\":{count}}}").map_err(StoreError::Io)?;
    writer.flush().map_err(StoreError::Io)?;
    let file = writer
        .into_inner()
        .map_err(|e| StoreError::Io(e.into_error()))?;
    file.sync_all().map_err(StoreError::Io)?;
    Ok((file, count))
}
struct Sink<'a, W: Write> {
    writer: &'a mut W,
    options: &'a Options,
    count: u64,
    bytes: u64,
    started: Instant,
    output_failed: bool,
}
impl<W: Write> Sink<'_, W> {
    fn emit(&mut self, source: &str, row: Value) -> Result<(), StoreError> {
        if self.started.elapsed() > Duration::from_secs(30) {
            return Err(failure("sms_export_failed"));
        }
        let obj = row
            .as_object()
            .ok_or_else(|| failure("sms_export_failed"))?;
        if source == "nwait"
            && (obj.get("deleted") == Some(&Value::Bool(true))
                || !matches!(
                    obj.get("direction").and_then(Value::as_str),
                    Some("inbound" | "outbound")
                )
                || matches!(
                    obj.get("state").and_then(Value::as_str),
                    Some("partial" | "ambiguous")
                ))
        {
            return Ok(());
        }
        let field = |name: &str| obj.get(name).and_then(Value::as_str).unwrap_or("");
        let device = match field("device") {
            "dji" => "dji",
            "reader" => "reader",
            _ => "unknown",
        };
        let profile = field("profile_id");
        let subscription = field("subscription_id");
        let peer = field(if source == "device" { "sender" } else { "peer" });
        let body = field(if source == "device" {
            "content"
        } else {
            "body"
        });
        let timestamp = if source == "device" {
            parse_timestamp(field("timestamp"))
        } else {
            obj.get("created_at_ms").and_then(Value::as_u64)
        };
        if source == "device"
            && (self.options.device.as_deref().is_some_and(|f| f != device)
                || self
                    .options
                    .subscription_id
                    .as_deref()
                    .is_some_and(|f| f != subscription))
        {
            return Ok(());
        }
        if source == "nwait"
            && self
                .options
                .profile_id
                .as_deref()
                .is_some_and(|f| f != profile)
        {
            return Ok(());
        }
        if self.options.peer.as_deref().is_some_and(|f| f != peer) {
            return Ok(());
        }
        if self
            .options
            .from_ms
            .is_some_and(|from| timestamp.is_none_or(|at| at < from))
            || self
                .options
                .to_ms
                .is_some_and(|to| timestamp.is_none_or(|at| at > to))
        {
            return Ok(());
        }
        let direction = match field("direction") {
            "in" | "inbound" => "inbound",
            "out" | "outbound" => "outbound",
            _ => "unknown",
        };
        let mut output = json!({"export_index":self.count+1,"source":source,"direction":direction,"timestamp_ms":timestamp});
        let map = output.as_object_mut().expect("fixed object");
        if source == "device" {
            map.insert("device".into(), Value::String(device.into()));
        }
        if source == "nwait" {
            let state = match field("state") {
                s @ ("received"
                | "queued"
                | "submitting"
                | "accepted"
                | "accepted_unconfirmed"
                | "delivered"
                | "failed"
                | "unknown"
                | "cancelled") => s,
                _ => "unknown",
            };
            map.insert("state".into(), Value::String(state.into()));
        }
        if self.options.privacy == Privacy::Full {
            map.insert("peer".into(), Value::String(peer.into()));
            map.insert("body".into(), Value::String(body.into()));
            if source == "nwait" {
                map.insert("profile_id".into(), Value::String(profile.into()));
            }
            if !subscription.is_empty() {
                map.insert("subscription_id".into(), Value::String(subscription.into()));
            }
            if source == "device" {
                map.insert("timestamp".into(), Value::String(field("timestamp").into()));
            }
        } else {
            map.insert("peer".into(), Value::Null);
            map.insert("body".into(), Value::Null);
        }
        let bytes = serde_json::to_vec(&output).map_err(StoreError::Json)?;
        self.bytes = self
            .bytes
            .saturating_add(bytes.len() as u64 + u64::from(self.count > 0));
        // Reserve enough bytes for the final count and object delimiters.
        if self.bytes > MAX_OUTPUT - 64 {
            return Err(failure("sms_export_failed"));
        }
        if self.count > 0 {
            self.writer.write_all(b",").map_err(StoreError::Io)?;
        }
        self.writer.write_all(&bytes).map_err(StoreError::Io)?;
        self.count += 1;
        Ok(())
    }
}

// serde_json's streaming visitor only materializes one bounded message. Other
// snapshot fields are skipped without building their maps/arrays or plaintext.
struct BudgetRead<R> {
    inner: R,
    total: u64,
    remaining: Rc<Cell<u64>>,
    exceeded: Rc<Cell<bool>>,
}
impl<R: Read> Read for BudgetRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.total == 0 || self.remaining.get() == 0 {
            self.exceeded.set(true);
            return Err(std::io::Error::other("export input limit"));
        }
        let limit = buf.len().min(self.total.min(self.remaining.get()) as usize);
        let n = self.inner.read(&mut buf[..limit])?;
        self.total -= n as u64;
        self.remaining.set(self.remaining.get() - n as u64);
        Ok(n)
    }
}
struct RecordSeed<'a, 'b, W: Write> {
    sink: &'a mut Sink<'b, W>,
    source: &'static str,
    budget: Rc<Cell<u64>>,
}
impl<'de, W: Write> DeserializeSeed<'de> for RecordSeed<'_, '_, W> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        self.budget.set(MAX_RECORD);
        let value = Value::deserialize(d);
        self.budget.set(u64::MAX);
        let value = value?;
        if !value.is_object() {
            return Err(de::Error::custom("message is not an object"));
        }
        self.sink.emit(self.source, value).map_err(|error| {
            // A timeout/output limit or failed export destination is not evidence
            // that the source ledger is corrupt. Do not poison its send guard.
            self.sink.output_failed = true;
            de::Error::custom(error)
        })
    }
}
struct SourceSeed<'a, 'b, W: Write> {
    sink: &'a mut Sink<'b, W>,
    source: &'static str,
    budget: Rc<Cell<u64>>,
}
impl<'de, W: Write> DeserializeSeed<'de> for SourceSeed<'_, '_, W> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de, W: Write> Visitor<'de> for SourceSeed<'_, '_, W> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an SMS archive array")
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        if self.source == "nwait" {
            return Err(de::Error::custom("missing N待 snapshot"));
        }
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let mut rows = 0usize;
        loop {
            let got = if self.source == "device" {
                a.next_element_seed(RecordSeed {
                    sink: self.sink,
                    source: self.source,
                    budget: self.budget.clone(),
                })?
            } else {
                a.next_element_seed(SnapshotSeed {
                    sink: self.sink,
                    budget: self.budget.clone(),
                })?
            };
            if got.is_none() {
                break;
            }
            rows += 1;
            if rows > if self.source == "device" { 1000 } else { 1 } {
                return Err(de::Error::custom("archive item limit"));
            }
        }
        if self.source == "nwait" && rows != 1 {
            return Err(de::Error::custom("missing N待 snapshot"));
        }
        Ok(())
    }
}
struct SnapshotSeed<'a, 'b, W: Write> {
    sink: &'a mut Sink<'b, W>,
    budget: Rc<Cell<u64>>,
}
impl<'de, W: Write> DeserializeSeed<'de> for SnapshotSeed<'_, '_, W> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de, W: Write> Visitor<'de> for SnapshotSeed<'_, '_, W> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("one N待 snapshot")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let mut seen = false;
        let mut schema_seen = false;
        let mut revision_seen = false;
        loop {
            self.budget.set(4096);
            let key = a.next_key::<String>();
            self.budget.set(u64::MAX);
            let Some(key) = key? else { break };
            if key == "messages" {
                if seen {
                    return Err(de::Error::custom("duplicate messages"));
                }
                seen = true;
                a.next_value_seed(MessagesSeed {
                    sink: self.sink,
                    budget: self.budget.clone(),
                })?;
            } else if key == "schema_version" {
                if schema_seen || a.next_value::<u64>()? != 1 {
                    return Err(de::Error::custom("invalid snapshot schema"));
                }
                schema_seen = true;
            } else if key == "revision" {
                if revision_seen {
                    return Err(de::Error::custom("duplicate snapshot revision"));
                }
                a.next_value::<u64>()?;
                revision_seen = true;
            } else {
                a.next_value::<IgnoredAny>()?;
            }
        }
        if !revision_seen {
            return Err(de::Error::custom("missing snapshot revision"));
        }
        Ok(())
    }
}
struct MessagesSeed<'a, 'b, W: Write> {
    sink: &'a mut Sink<'b, W>,
    budget: Rc<Cell<u64>>,
}
impl<'de, W: Write> DeserializeSeed<'de> for MessagesSeed<'_, '_, W> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de, W: Write> Visitor<'de> for MessagesSeed<'_, '_, W> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("message map")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        // Retain only unique map keys, never message bodies. Total key bytes
        // are bounded by the 64 MiB source and 4 KiB individual key limits.
        // Do not require canonical sorting: a valid restored JSON may be unordered.
        let mut keys = BTreeSet::new();
        loop {
            self.budget.set(4096);
            let key = a.next_key::<String>();
            self.budget.set(u64::MAX);
            let Some(key) = key? else {
                break;
            };
            if keys.len() >= MAX_MESSAGE_KEYS {
                self.sink.output_failed = true;
                return Err(de::Error::custom("message key limit"));
            }
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate message key"));
            }
            a.next_value_seed(RecordSeed {
                sink: self.sink,
                source: "nwait",
                budget: self.budget.clone(),
            })?;
        }
        Ok(())
    }
}
fn stream<W: Write>(input: Input, sink: &mut Sink<'_, W>) -> Result<(), StoreError> {
    let health = input.health.clone();
    sink.output_failed = false;
    match stream_inner(input, sink) {
        Err(error) if !sink.output_failed && health.is_some() => {
            let error = match error {
                StoreError::Json(ref json) if json.is_io() => StoreError::Io(std::io::Error::new(
                    json.io_error_kind().unwrap_or(std::io::ErrorKind::Other),
                    "snapshot read failed",
                )),
                other => other,
            };
            Err(health.expect("checked health").snapshot_failed(error))
        }
        result => result,
    }
}
fn stream_inner<W: Write>(input: Input, sink: &mut Sink<'_, W>) -> Result<(), StoreError> {
    let Some(file) = input.file else {
        return Ok(());
    };
    if file.metadata().map_err(StoreError::Io)?.len() == 0 {
        return if input.source == "nwait" {
            Err(failure("invalid_store_schema"))
        } else {
            Ok(())
        };
    }
    let budget = Rc::new(Cell::new(u64::MAX));
    let exceeded = Rc::new(Cell::new(false));
    let reader = BudgetRead {
        inner: BufReader::new(file),
        total: input.limit + 1,
        remaining: budget.clone(),
        exceeded: exceeded.clone(),
    };
    let mut d = serde_json::Deserializer::from_reader(reader);
    let result = SourceSeed {
        sink,
        source: input.source,
        budget,
    }
    .deserialize(&mut d)
    .and_then(|()| d.end());
    if exceeded.get() {
        // Export's per-record/key budgets are stricter than the ledger's own
        // schema. Hitting them does not establish a source persistence fault.
        return Err(failure("sms_export_failed"));
    }
    result.map_err(StoreError::Json)
}

// Go's time.Time archive strings are RFC3339/RFC3339Nano. Parse calendar and
// explicit zone rather than comparing strings (offsets may differ).
fn parse_timestamp(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || !s.is_ascii() {
        return None;
    }
    let num = |a: usize, z: usize| {
        let part = s.get(a..z)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<i64>().ok()
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, m, d, h, min, sec) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&m)
        || d < 1
        || d > days[(m - 1) as usize]
        || h > 23
        || min > 59
        || sec > 59
    {
        return None;
    }
    let mut i = 19;
    let mut millis = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            if i - start < 3 {
                millis = millis * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        if i == start {
            return None;
        }
        for _ in (i - start)..3 {
            millis *= 10;
        }
    }
    let zone = if s.get(i..) == Some("Z") {
        0
    } else {
        if b.len() != i + 6 || b[i + 3] != b':' || !matches!(b[i], b'+' | b'-') {
            return None;
        }
        let zh = num(i + 1, i + 3)?;
        let zm = num(i + 4, i + 6)?;
        if zh > 23 || zm > 59 {
            return None;
        }
        (zh * 60 + zm) * 60 * if b[i] == b'+' { 1 } else { -1 }
    };
    let yy = y - if m <= 2 { 1 } else { 0 };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let day = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    u64::try_from((day * 86400 + h * 3600 + min * 60 + sec - zone) * 1000 + millis)
        .ok()
        .filter(|v| *v <= MAX_TIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "switch-sms-export-{}",
                crate::multistandby_store::random_token().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn device(&self) -> ArchiveStore {
            ArchiveStore::new(self.0.join("sms-archive.json"))
        }
        fn nwait(&self) -> ArchiveStore {
            ArchiveStore::with_limits(
                self.0.join("multistandby/store-v1.json"),
                64 * 1024 * 1024,
                1,
            )
            .unwrap()
        }
        fn seed(&self) {
            self.device().replace(vec![json!({"device":"dji","sender":"65075","content":"device private text","timestamp":"2026-10-03T12:00:00+08:00","direction":"in","id":"65075-secret-id","subscription_id":format!("sub1_{}","a".repeat(64))}),json!({"device":"reader","sender":"10086","content":"reader body","timestamp":"2026-10-03T04:00:01Z","direction":"out"})]).unwrap();
            self.nwait().replace(vec![json!({"revision":7,"schema_version":1,"profiles":{"private-profile":{"imsi":"private-imsi"}},"changes":[{"cursor":"private-cursor"}],"messages":{"m1":{"message_id":"m1","profile_id":"private-profile","peer":"65075","body":"nwait private text 😀","direction":"inbound","state":"received","created_at_ms":1791000002000u64},"m2":{"profile_id":"private-profile","peer":"65075","body":"deleted private text","direction":"inbound","deleted":true,"created_at_ms":1791000003000u64},"m3":{"direction":"report","report":"private-report"}}})]).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn read_file(mut file: File) -> Value {
        file.rewind().unwrap();
        serde_json::from_reader(file).unwrap()
    }
    fn opts(value: Value) -> Options {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn export_first_truncated_nwait_must_fail_instead_of_publishing_empty_success() {
        let f = Fixture::new();
        let store = crate::multistandby_store::Store::open(&f.0).unwrap();
        store.recover().unwrap();
        fs::write(f.nwait().path(), b"").unwrap();
        let exports = Exports::default();
        assert!(
            exports
                .prepare_guarded(&store, opts(json!({"source":"nwait"})))
                .is_err()
        );
        assert_eq!(store.status().reason, Some("store_corrupt"));
    }

    #[test]
    fn protected_snapshot_latches_source_errors_after_pinning_without_decoding_whole_store() {
        for damage in ["empty", "json", "read_io"] {
            let f = Fixture::new();
            let store = crate::multistandby_store::Store::open(&f.0).unwrap();
            store.recover().unwrap();
            let mut input = pin_guarded(&store, "nwait").unwrap();
            match damage {
                "empty" => fs::write(f.nwait().path(), b"").unwrap(),
                "json" => fs::write(f.nwait().path(), b"[broken").unwrap(),
                // Unix permits opening a directory but reading it as JSON fails
                // with EISDIR, exercising an actual FD read I/O error.
                _ => input.file = Some(File::open(&f.0).unwrap()),
            }
            let options = Options::default();
            let mut bytes = Vec::new();
            let mut sink = Sink {
                writer: &mut bytes,
                options: &options,
                count: 0,
                bytes: 0,
                started: Instant::now(),
                output_failed: false,
            };
            assert!(
                matches!(stream(input, &mut sink), Err(StoreError::Configuration(c)) if c == "sms_store_unavailable")
            );
            assert_eq!(
                store.status().reason,
                Some(if damage == "read_io" {
                    "store_io_failed"
                } else {
                    "store_corrupt"
                })
            );
            assert!(store.apply("claim", json!({})).is_err());
        }
    }

    #[test]
    fn export_budgets_and_destination_failures_do_not_latch_healthy_nwait_store() {
        struct FullDisk;
        impl Write for FullDisk {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(28))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for policy in [
            "record",
            "source_budget",
            "output",
            "timeout",
            "disk",
            "slots",
        ] {
            let f = Fixture::new();
            let body = if policy == "record" {
                "x".repeat(MAX_RECORD as usize + 5)
            } else {
                "safe".into()
            };
            f.nwait().replace(vec![json!({"schema_version":1,"revision":1,"messages":{"m":{"message_id":"m","direction":"inbound","state":"received","body":body}}})]).unwrap();
            let store = crate::multistandby_store::Store::open(&f.0).unwrap();
            store.recover().unwrap();
            let options = opts(json!({"source":"nwait","privacy":"full"}));
            if policy == "slots" {
                let exports = Exports::default();
                exports.slots.store(MAX_EXPORTS, Ordering::Release);
                assert!(exports.prepare_guarded(&store, options).is_err());
            } else {
                let mut input = pin_guarded(&store, "nwait").unwrap();
                if policy == "source_budget" {
                    input.limit = 2;
                }
                let mut bytes = Vec::new();
                let mut full_disk = FullDisk;
                let writer: &mut dyn Write = if policy == "disk" {
                    &mut full_disk
                } else {
                    &mut bytes
                };
                let mut sink = Sink {
                    writer: &mut { writer },
                    options: &options,
                    count: 0,
                    bytes: if policy == "output" { MAX_OUTPUT } else { 0 },
                    started: if policy == "timeout" {
                        Instant::now() - Duration::from_secs(31)
                    } else {
                        Instant::now()
                    },
                    output_failed: false,
                };
                assert!(stream(input, &mut sink).is_err(), "{policy}");
            }
            assert_eq!(store.status().state, "ready", "{policy}");
            store.recover().unwrap();
        }
    }

    #[test]
    fn masked_projection_hides_payload_identity_extra_fields_and_filter_values() {
        let f = Fixture::new();
        f.seed();
        let before_device = fs::read(f.device().path()).unwrap();
        let before_nwait = fs::read(f.nwait().path()).unwrap();
        let options = opts(json!({"peer":"65075"}));
        let (file, count) = build(&f.0, &options, "test-masked").unwrap();
        assert_eq!(count, 2);
        let value = read_file(file);
        let text = serde_json::to_string(&value).unwrap();
        for secret in [
            "65075",
            "private",
            "sub1_",
            "secret-id",
            "deleted",
            "cursor",
            "imsi",
        ] {
            assert!(!text.contains(secret), "leaked {secret}");
        }
        assert_eq!(value["privacy"], "masked");
        assert_eq!(value["message_count"], 2);
        assert_eq!(value["source_snapshots"].as_array().unwrap().len(), 2);
        for row in value["messages"].as_array().unwrap() {
            assert!(row["peer"].is_null());
            assert!(row["body"].is_null());
            assert!(row.get("profile_id").is_none());
            assert!(row.get("subscription_id").is_none());
        }
        assert_eq!(fs::read(f.device().path()).unwrap(), before_device);
        assert_eq!(fs::read(f.nwait().path()).unwrap(), before_nwait);
        assert_eq!(fs::read_dir(f.0.join("sms-export-tmp")).unwrap().count(), 0);
    }
    #[test]
    fn full_export_filters_sources_subscription_peer_and_closed_time_interval() {
        let f = Fixture::new();
        f.seed();
        let options = opts(
            json!({"source":"device","privacy":"full","device":"dji","subscription_id":format!("sub1_{}","a".repeat(64)),"peer":"65075","from_ms":1791000000000u64,"to_ms":1791000000000u64}),
        );
        options.validate().unwrap();
        let (file, count) = build(&f.0, &options, "test-full").unwrap();
        assert_eq!(count, 1);
        let value = read_file(file);
        let row = &value["messages"][0];
        assert_eq!(row["body"], "device private text");
        assert_eq!(row["peer"], "65075");
        assert_eq!(row["direction"], "inbound");
        assert!(row.get("id").is_none());
        let options = opts(
            json!({"source":"nwait","privacy":"full","profile_id":"private-profile","peer":"65075"}),
        );
        let (file, count) = build(&f.0, &options, "test-nwait").unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            read_file(file)["messages"][0]["body"],
            "nwait private text 😀"
        );
    }
    #[test]
    fn pinned_generation_survives_atomic_replace_without_holding_source_lock() {
        let f = Fixture::new();
        f.seed();
        let input = pin(&f.0, "device").unwrap();
        f.device().replace(vec![json!({"sender":"new-peer","content":"new body","timestamp":"2026-10-03T12:00:00+08:00"})]).unwrap();
        let options = opts(json!({"privacy":"full"}));
        let mut bytes = Vec::new();
        let mut sink = Sink {
            writer: &mut bytes,
            options: &options,
            count: 0,
            bytes: 0,
            started: Instant::now(),
            output_failed: false,
        };
        stream(input, &mut sink).unwrap();
        assert_eq!(sink.count, 2);
        drop(sink);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("device private text"));
        assert!(!text.contains("new body"));
    }
    #[test]
    fn malformed_oversized_and_symlink_sources_do_not_publish_exports() {
        let f = Fixture::new();
        let exports = Exports::default();
        fs::write(f.device().path(), br#"[{"content":"private"}] trailing"#).unwrap();
        assert!(exports.prepare(&f.0, Options::default()).is_err());
        assert_eq!(exports.slots.load(Ordering::Acquire), 0);
        assert!(exports.entries.lock().unwrap().is_empty());
        fs::write(
            f.device().path(),
            format!(
                "[{{\"content\":\"{}\"}}]",
                "x".repeat(MAX_RECORD as usize + 5)
            ),
        )
        .unwrap();
        assert!(exports.prepare(&f.0, Options::default()).is_err());
        fs::remove_file(f.device().path()).unwrap();
        let target = f.0.join("separate.json");
        fs::write(&target, b"[]").unwrap();
        symlink(&target, f.device().path()).unwrap();
        assert!(exports.prepare(&f.0, Options::default()).is_err());
        assert_eq!(fs::read_dir(f.0.join("sms-export-tmp")).unwrap().count(), 0);
    }
    #[test]
    fn corrupted_snapshot_duplicate_map_or_more_than_one_snapshot_fails() {
        let f = Fixture::new();
        fs::create_dir_all(f.0.join("multistandby")).unwrap();
        for input in [
            r#"[{"messages":{},"messages":{}}]"#,
            r#"[{"messages":{}},{"messages":{}}]"#,
            r#"{"messages":{}}"#,
        ] {
            fs::write(f.nwait().path(), input).unwrap();
            assert!(build(&f.0, &Options::default(), "bad-nwait").is_err());
        }
    }
    #[test]
    fn duplicate_message_keys_fail_but_valid_unsorted_snapshots_export_once() {
        let f = Fixture::new();
        fs::create_dir_all(f.0.join("multistandby")).unwrap();
        let row = r#"{"direction":"inbound","body":"text","peer":"65075"}"#;
        fs::write(
            f.nwait().path(),
            format!(r#"[{{"revision":1,"messages":{{"same":{row},"same":{row}}}}}]"#),
        )
        .unwrap();
        assert!(build(&f.0, &Options::default(), "duplicate-key").is_err());
        fs::write(
            f.nwait().path(),
            format!(r#"[{{"revision":1,"messages":{{"z":{row},"a":{row}}}}}]"#),
        )
        .unwrap();
        assert_eq!(
            build(&f.0, &Options::default(), "unsorted-keys").unwrap().1,
            2
        );
    }

    #[test]
    fn bounded_chunks_reassemble_one_private_file_and_cleanup_releases_slots() {
        let f = Fixture::new();
        f.device().replace(vec![json!({"device":"dji","sender":"1","content":"😀".repeat(100_000),"timestamp":"2026-10-03T00:00:00Z"})]).unwrap();
        let exports = Exports::default();
        let metadata = exports
            .prepare(&f.0, opts(json!({"privacy":"full"})))
            .unwrap();
        let token = metadata["token"].as_str().unwrap();
        assert!(valid_token(token));
        let total = metadata["total_bytes"].as_u64().unwrap();
        let mut bytes = Vec::new();
        while (bytes.len() as u64) < total {
            let chunk = exports.chunk(token, bytes.len() as u64).unwrap();
            assert!(chunk.len() <= CHUNK);
            assert!(!chunk.is_empty());
            bytes.extend_from_slice(&chunk);
        }
        assert!(exports.chunk(token, total).unwrap().is_empty());
        assert!(exports.chunk(token, total + 1).is_err());
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["messages"][0]["body"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            100_000
        );
        let entries = exports.entries.lock().unwrap();
        assert_eq!(
            entries[token].file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(entries);
        exports.delete(token).unwrap();
        assert!(exports.chunk(token, 0).is_err());
        assert_eq!(exports.slots.load(Ordering::Acquire), 0);
    }
    #[test]
    fn registry_enforces_four_slots_and_expiration_without_leaking_temporary_paths() {
        let f = Fixture::new();
        let exports = Exports::default();
        let mut tokens = Vec::new();
        for _ in 0..MAX_EXPORTS {
            tokens.push(
                exports.prepare(&f.0, Options::default()).unwrap()["token"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        assert!(
            matches!(exports.prepare(&f.0,Options::default()),Err(StoreError::Configuration(c)) if c=="sms_export_busy")
        );
        exports
            .entries
            .lock()
            .unwrap()
            .get_mut(&tokens[0])
            .unwrap()
            .deadline = Instant::now() - Duration::from_secs(1);
        assert!(exports.chunk(&tokens[0], 0).is_err());
        assert!(exports.prepare(&f.0, Options::default()).is_ok());
        assert_eq!(fs::read_dir(f.0.join("sms-export-tmp")).unwrap().count(), 0);
    }
    #[test]
    fn invalid_options_and_unknown_schema_are_rejected() {
        for value in [
            json!({"from_ms":10,"to_ms":9}),
            json!({"device":"other"}),
            json!({"profile_id":"bad\n"}),
            json!({"subscription_id":"sub1_short"}),
            json!({"to_ms":u64::MAX}),
        ] {
            assert!(opts(value).validate().is_err());
        }
        for value in [
            json!({"source":"other"}),
            json!({"privacy":"unknown"}),
            json!({"path":"/private/source"}),
        ] {
            assert!(serde_json::from_value::<Options>(value).is_err());
        }
    }
    #[test]
    fn timestamp_timezone_fraction_and_calendar_validation() {
        assert_eq!(
            parse_timestamp("2026-10-03T12:00:00+08:00"),
            Some(1791000000000)
        );
        assert_eq!(
            parse_timestamp("2026-10-03T04:00:00.123456789Z"),
            Some(1791000000123)
        );
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("1970-01-01T00:00:00.1Z"), Some(100));
        for s in [
            "bad",
            "2026-02-29T00:00:00Z",
            "2026-10-03T24:00:00Z",
            "2026-10-03T-1:00:00Z",
            "2026-10-03T00:00:00.Z",
            "2026-10-03T00:00:00+24:00",
            "2026-10-03T00:00:00Ztrailing",
        ] {
            assert_eq!(parse_timestamp(s), None, "{s}");
        }
        assert!(parse_timestamp("2024-02-29T00:00:00Z").is_some());
    }
    #[test]
    fn large_archive_streams_all_complete_records_without_exporting_fragment_secrets() {
        let f = Fixture::new();
        fs::create_dir_all(f.0.join("multistandby")).unwrap();
        let mut writer = BufWriter::new(File::create(f.nwait().path()).unwrap());
        writer
            .write_all(
                b"[{\"revision\":123,\"profiles\":{\"ignore\":\"secret identity\"},\"messages\":{",
            )
            .unwrap();
        for i in 0..100_000 {
            if i > 0 {
                writer.write_all(b",").unwrap();
            }
            write!(writer,"\"m{i}\":{{\"direction\":\"inbound\",\"state\":\"received\",\"profile_id\":\"private-profile\",\"peer\":\"65075\",\"body\":\"message {i}\",\"created_at_ms\":{}}}",1791000000000u64+i).unwrap();
        }
        writer.write_all(b",\"partial\":{\"direction\":\"inbound\",\"state\":\"partial\",\"body\":\"fragment secret\"}}}]").unwrap();
        writer.flush().unwrap();
        drop(writer);
        let (file, count) = build(&f.0, &Options::default(), "large-stream").unwrap();
        assert_eq!(count, 100_000);
        // Tests may read the output to validate its schema; production only
        // transfers bounded chunks and never builds this whole Value.
        let value = read_file(file);
        assert_eq!(value["message_count"], 100_000);
        assert_eq!(value["messages"].as_array().unwrap().len(), 100_000);
    }
    #[test]
    fn output_write_failure_is_propagated_and_masked_enums_cannot_carry_secrets() {
        struct Fails;
        impl Write for Fails {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let options = Options::default();
        let mut writer = Fails;
        let mut sink = Sink {
            writer: &mut writer,
            options: &options,
            count: 0,
            bytes: 0,
            started: Instant::now(),
            output_failed: false,
        };
        assert!(sink.emit("device", json!({"sender":"secret"})).is_err());
        assert_eq!(sink.count, 0);
        let mut bytes = Vec::new();
        let mut sink = Sink {
            writer: &mut bytes,
            options: &options,
            count: 0,
            bytes: 0,
            started: Instant::now(),
            output_failed: false,
        };
        sink.emit("device",json!({"device":"secret","direction":"secret","sender":"secret","content":"secret","id":"secret"})).unwrap();
        drop(sink);
        assert!(!String::from_utf8(bytes).unwrap().contains("secret"));
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixStream;
    #[tokio::test]
    async fn export_gateway_authentication_and_bounded_chunk_contract() {
        let root = std::env::temp_dir().join(format!(
            "switch-export-ipc-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let metadata = exports().prepare(&root, Options::default()).unwrap();
        let token = metadata["token"].as_str().unwrap();
        for (method, path, authorized, body, want) in [
            (
                Method::POST,
                PREFIX.to_owned(),
                false,
                "{}",
                StatusCode::UNAUTHORIZED,
            ),
            (
                Method::POST,
                PREFIX.to_owned(),
                true,
                "{\"unknown\":1}",
                StatusCode::BAD_REQUEST,
            ),
            (
                Method::GET,
                format!("{PREFIX}/{token}?offset=0"),
                false,
                "",
                StatusCode::UNAUTHORIZED,
            ),
            (
                Method::GET,
                format!("{PREFIX}/{token}?offset=0"),
                true,
                "",
                StatusCode::OK,
            ),
            (
                Method::GET,
                format!("{PREFIX}/{token}?offset=0&offset=1"),
                true,
                "",
                StatusCode::BAD_REQUEST,
            ),
            (
                Method::DELETE,
                format!("{PREFIX}/{token}"),
                true,
                "",
                StatusCode::OK,
            ),
            (
                Method::GET,
                format!("{PREFIX}/{token}?offset=0"),
                true,
                "",
                StatusCode::GONE,
            ),
        ] {
            let (client, server) = UnixStream::pair().unwrap();
            let sms_inbox = crate::message_archive::InboxState::default();
            let state = crate::ServerState {
                call_mode: Default::default(),
                module_call: Default::default(),
                reader_lifecycle: Default::default(),
                multistandby_card: Default::default(),
                multistandby_service: Default::default(),
                app_token: Some("export-fixture".into()),
                internal_token: "internal-fixture".into(),
                multistandby_store: crate::multistandby_store::Store::open(&root).unwrap(),
                multistandby_operations: crate::multistandby_operations::Store::open(&root)
                    .unwrap(),
                device_send_store: crate::device_send_store::Store::open(&root).unwrap(),
                forwarding_handoff_store: crate::forwarding_handoff_store::Store::open(&root)
                    .unwrap(),
                sms_forwarding: crate::sms_forwarding::Service::default(),
                sms_inbox: sms_inbox.clone(),
                usb_sms_receive: crate::test_usb_sms_receiver(
                    sms_inbox.clone(),
                    root.join("sms-archive.json"),
                ),
                call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                call_facts: crate::call_facts::CallFacts::new(
                    proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                    "call-producer-fixture".to_owned(),
                ),
                go_socket_path: root.join("unused.sock"),
                availability: crate::BackendAvailability::new(),
                reader_discovery: Arc::new(Mutex::new(crate::ReaderDiscovery::default())),
                snapshot_instance_id: "test".into(),
                snapshot_revision: Arc::new(crate::AtomicU64::new(0)),
                eap_exchanges: Arc::new(crate::eap_identity::EapExchangeStore::default()),
                reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
                reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
                modem_actor: crate::modem_actor::ModemActor::new().unwrap(),
                qmi_enhancement: crate::qmi_enhancement::QmiEnhancement::default(),
                network_traffic: crate::network_traffic::Service::default(),
                usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
                module_radio: crate::test_module_radio(&root),
                native_vowifi_lifecycle: std::sync::Arc::new(
                    crate::native_vowifi_session::NativeLifecycle::default(),
                ),
                ims_sms_transactions: crate::ims_sms_transactions::Service::default(),
                multi_runtime_report: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::multistandby_runtime_report::Journal::default(),
                )),
            };
            let server = tokio::spawn(crate::serve_connection(server, state));
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost");
            if authorized {
                request = request.header("x-proletarius-app-token", "export-fixture");
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::from(body))).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), want);
            let content_type = response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert!(bytes.len() <= CHUNK);
            if content_type == "application/octet-stream" {
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes).unwrap()["message_count"],
                    0
                );
            }
            drop(sender);
            if let Err(error) = connection.await.unwrap() {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
            if let Err(error) = server.await.unwrap() {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
