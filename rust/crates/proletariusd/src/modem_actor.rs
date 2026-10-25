//! Single-owner serial port actor used by the supervised Go AT compatibility
//! loop while modem command/URC semantics are migrated to Rust.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use zeroize::Zeroize;

mod admission;
mod call_state_projection;
mod discovery;
mod initialize;
mod native_serial;
mod radio;
mod serial_card;
mod serial_module;
pub(crate) use initialize::InitializationResult as NativeSerialInitialization;
use native_serial::*;
pub(crate) use native_serial::{
    NativeSerialMetadata, NativeSerialObservation, NativeSerialSession,
};
pub(crate) use serial_module::SerialModuleHold;
use serial_module::*;
mod serial_sms;
use serial_card::*;
pub(crate) use serial_card::{SerialCardBinding, parse_serial_esim_health_responses};
mod sms_ingest;
mod sms_receive;
pub(crate) use serial_sms::{SerialSmsBinding, SerialSmsObservation};
#[cfg(test)]
mod serial_sms_tests;
use initialize::*;
use radio::*;
use sms_ingest::*;
use sms_receive::*;

pub(crate) const PREFIX: &str = "/api/internal/modem/";
pub(crate) const PROJECT_CALL_STATE_PATH: &str = "/api/internal/modem/project-call-state";
const CALL_COMMAND_PATH: &str = "/api/internal/modem/call-command";
const MAX_PORT_NAME_BYTES: usize = 4096;
const MAX_AT_DISCOVERY_CANDIDATES: usize = 32;
const AT_DISCOVERY_BAUD_RATE: u32 = 115_200;
const AT_DISCOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const AT_DISCOVERY_READ_TIMEOUT: Duration = Duration::from_millis(80);
const AT_DISCOVERY_MAX_RESPONSE_BYTES: usize = 4096;
const MAX_READ_BYTES: usize = 8192;
const MAX_WRITE_BYTES: usize = 8192;
const MAX_BUFFERED_READ_BYTES: usize = 64 * 1024;
const SERIAL_READ_CHUNK_BYTES: usize = 4096;
const SERIAL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SERIAL_READ_TIMEOUT: Duration = Duration::from_millis(10);
const ACTOR_QUEUE_CAPACITY: usize = 32;
const HIGH_COMMAND_QUEUE_CAPACITY: usize = 5;
const NORMAL_COMMAND_QUEUE_CAPACITY: usize = 10;
const COMMAND_TIMEOUT_WATCHDOG_THRESHOLD: usize = 5;
const RPC_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const SMS_DELETE_STEP_TIMEOUT: Duration = Duration::from_secs(5);
const SMS_PDU_MODE_TIMEOUT: Duration = Duration::from_secs(3);
const SMS_SUBMIT_STEP_TIMEOUT: Duration = Duration::from_secs(20);
const SMS_SUBMIT_SEGMENT_GAP: Duration = Duration::from_millis(500);
const MAX_COMMAND_REPLY_BYTES: usize = 64 * 1024;
const MAX_COMMAND_REPLY_LINES: usize = 1_024;
const MAX_SMS_SUBMIT_BYTES: usize = 64 * 1024;
const MAX_SMS_SUBMIT_REQUEST_BYTES: usize = 400 * 1024;
pub(crate) const MAX_CALL_STATE_PROJECTION_REQUEST_BYTES: usize = 68 * 1024;
type PortOpener = Box<
    dyn Fn(String, u32) -> Result<Box<dyn serialport::SerialPort>, ActorError> + Send + 'static,
>;

#[derive(Clone)]
pub(crate) struct ModemActor {
    compatibility_rpc: bool,
    client: Arc<ActorClient>,
    receive_worker: Arc<ReceiveWorker>,
}

pub(crate) enum ManualSmsScanRequest {
    Scheduled,
    SerialUnavailable,
    Rejected(&'static str),
}

enum Command {
    // Only a finite admitted radio operation can carry this private envelope.
    ScopedRadio {
        command: Box<Command>,
        right: crate::module_recovery::OperationRight,
        producer: String,
    },
    LegacyGate(Box<dyn FnOnce(&mut ActorState, &PortOpener) + Send>),
    Native(NativeRequest),
    Card(CardRequest),
    SerialSmsObservation {
        reply: SyncSender<Result<SerialSmsObservation, ActorError>>,
    },
    SerialSmsBindingCurrent {
        binding: SerialSmsBinding,
        reply: SyncSender<Result<bool, ActorError>>,
    },
    CacheSerialSmsPhone {
        binding: SerialSmsBinding,
        phone: String,
        reply: SyncSender<Result<(), ActorError>>,
    },
    CallModeRadioInitialize {
        serial_session: u64,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        reply: SyncSender<Result<InitializationResult, ActorError>>,
    },
    FencedClose {
        serial_session: u64,
        reply: SyncSender<Result<(), ActorError>>,
    },
    CallModeRadioReconnect {
        serial_session: u64,
        expected_imei: String,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        reply: SyncSender<Result<u64, ActorError>>,
    },
    RadioBinding {
        reply: SyncSender<Result<u64, ActorError>>,
    },
    Radio {
        module_token: Option<String>,
        serial_session: u64,
        action: crate::module_radio_transport::Action,
        deadline: Instant,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        commit_proof: Option<crate::module_radio::CallModeCommitProof>,
        reply: SyncSender<Result<crate::module_radio_transport::Outcome, ActorError>>,
    },
    Open {
        path: String,
        baud_rate: u32,
        initialization_required: bool,
        reply: SyncSender<Result<u64, ActorError>>,
    },
    DiscoverATPort {
        // None enumerates inside this retained actor command. This keeps a
        // subsequent Close behind discovery even if its observer is dropped.
        candidates: Option<Vec<String>>,
        reply: SyncSender<Result<DiscoveredATPort, ActorError>>,
    },
    Initialize {
        reply: SyncSender<Result<InitializationResult, ActorError>>,
    },
    Read {
        maximum: usize,
        timeout: Duration,
        reply: SyncSender<Result<SerialFrame, ActorError>>,
    },
    Write {
        data: Vec<u8>,
        reply: SyncSender<Result<usize, ActorError>>,
    },
    Execute {
        command: String,
        timeout: Duration,
        high_priority: bool,
        follow_up: Option<Vec<u8>>,
        expected_sms_generation: Option<u64>,
        reply: SyncSender<Result<CommandResult, ActorError>>,
    },
    SubmitSms {
        tpdus: Vec<Vec<u8>>,
        binding: Option<SerialSmsBinding>,
        expected_sms_generation: Option<u64>,
        reply: SyncSender<Result<SmsSubmitResult, ActorError>>,
    },
    DeleteArchivedSms {
        index: u32,
        pdu: String,
        expected_sms_generation: u64,
        receive_fence: Option<ReceiveFence>,
        reply: SyncSender<Result<StoredSmsDeleteResult, ActorError>>,
    },
    ReadStoredSms {
        storage: Option<String>,
        index: Option<u32>,
        expected_sms_generation: u64,
        receive_fence: Option<ReceiveFence>,
        reply: SyncSender<Result<StoredSmsReadResult, ActorError>>,
    },
    ConfirmSmsGeneration {
        expected_sms_generation: u64,
        receive_fence: Option<ReceiveFence>,
        reply: SyncSender<Result<(), ActorError>>,
    },
    ReadSmsGeneration {
        reply: SyncSender<Result<u64, ActorError>>,
    },
    Close {
        reply: SyncSender<Result<(), ActorError>>,
    },
    Finalize {
        reply: SyncSender<Result<(), ActorError>>,
    },
    ConfigureSmsReceive {
        enabled: bool,
        reply: SyncSender<Result<(), ActorError>>,
    },
    ScanSmsReceive {
        reply: SyncSender<Result<(), ActorError>>,
    },
    NextSmsReceive {
        reply: SyncSender<Result<Option<ReceiveWork>, ActorError>>,
    },
    FinishSmsReceive {
        id: u64,
        reply: SyncSender<Result<(), ActorError>>,
    },
}

struct PendingRead {
    maximum: usize,
    deadline: Instant,
    reply: SyncSender<Result<SerialFrame, ActorError>>,
}

// The bounded command/reply lifecycle follows Celmux's AT session design
// (`crates/at/src/session.rs`), adapted to Switch's existing serial actor,
// frame classifier and Go compatibility callbacks.
struct PendingCommand {
    deadline: Instant,
    started: Instant,
    lines: Vec<String>,
    response_bytes: usize,
    overflow: bool,
    follow_up: Option<Vec<u8>>,
    expected_sms_generation: Option<u64>,
    high_priority: bool,
    completion: CommandCompletion,
}

struct QueuedCommand {
    bytes: Vec<u8>,
    timeout: Duration,
    follow_up: Option<Vec<u8>>,
    expected_sms_generation: Option<u64>,
    high_priority: bool,
    completion: CommandCompletion,
}

enum CommandCompletion {
    NativeQuery(NativeQueryCompletion),
    Card(CardCompletion),
    Radio(RadioTransaction),
    Initialize(InitializationTransaction),
    Execute(SyncSender<Result<CommandResult, ActorError>>),
    SubmitSms(SmsSubmitTransaction),
    DeleteArchivedSms(StoredSmsDeleteTransaction),
    ReadStoredSms(StoredSmsReadTransaction),
}

#[derive(Debug, Serialize)]
pub(crate) struct SmsSubmitResult {
    pub(crate) accepted_segments: usize,
    pub(crate) complete: bool,
}

struct SmsSubmitTransaction {
    binding: Option<SerialSmsBinding>,
    tpdus: Vec<Vec<u8>>,
    next_segment: usize,
    accepted_segments: usize,
    expected_sms_generation: u64,
    step: SmsSubmitStep,
    reply: SyncSender<Result<SmsSubmitResult, ActorError>>,
}

#[derive(Clone, Copy)]
enum SmsSubmitStep {
    PduMode,
    Segment,
}

struct DeferredSmsSubmit {
    transaction: SmsSubmitTransaction,
    ready_at: Instant,
}

struct StoredSmsDeleteTransaction {
    index: u32,
    pdu: String,
    expected_sms_generation: u64,
    receive_fence: Option<ReceiveFence>,
    step: StoredSmsDeleteStep,
    reply: SyncSender<Result<StoredSmsDeleteResult, ActorError>>,
}

#[derive(Clone, Copy)]
enum StoredSmsDeleteStep {
    VerifyStorage,
    VerifyReadback,
    Delete,
}

struct StoredSmsReadTransaction {
    requested_storage: Option<String>,
    index: Option<u32>,
    expected_sms_generation: u64,
    receive_fence: Option<ReceiveFence>,
    previous_storages: Vec<String>,
    selected_storage: String,
    step: StoredSmsReadStep,
    response_lines: Vec<String>,
    primary_error: Option<ActorError>,
    reply: SyncSender<Result<StoredSmsReadResult, ActorError>>,
}

#[derive(Clone, Copy)]
enum StoredSmsReadStep {
    QueryStorage,
    SelectStorage,
    ConfirmStorage,
    Read,
    RestoreStorage,
    ConfirmRestore,
}

#[derive(Debug, Serialize)]
struct StoredSmsReadResult {
    storage: String,
    lines: Vec<String>,
    storage_restored: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct DiscoveredATPort {
    pub(crate) port: String,
}

#[derive(Debug, Serialize)]
struct StoredSmsDeleteResult {
    deleted: bool,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct CommandResult {
    lines: Vec<String>,
    result: &'static str,
    terminal: Option<String>,
    elapsed_ms: u128,
    timeout_streak: usize,
    timeout_threshold: usize,
    watchdog_tripped: bool,
}

#[derive(Debug, Serialize)]
struct TypedQueryResult {
    query: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_state: Option<call_state_projection::CallState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    string_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bool_value: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rssi: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dbm: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    int_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registration_valid: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reg_status: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lac: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cell_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rsrp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rsrq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sinr: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duplex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    band: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<u32>,
}

impl TypedQueryResult {
    fn empty(query: &'static str) -> Self {
        Self {
            query,
            call_state: None,
            string_value: None,
            bool_value: None,
            rssi: None,
            dbm: None,
            int_value: None,
            registration_valid: None,
            reg_status: None,
            lac: None,
            cell_id: None,
            rsrp: None,
            rsrq: None,
            sinr: None,
            duplex: None,
            band: None,
            channel: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct DeviceInfoSnapshotResult {
    imei: String,
    firmware: String,
    sim_inserted: bool,
    imsi: String,
    phone_number: String,
    iccid: String,
    operator: String,
    reg_status: i64,
    reg_status_text: String,
    lac: String,
    cell_id: String,
    signal_dbm: i64,
    signal_rsrp: i64,
    signal_rsrq: i64,
    apn: String,
    ims_status: i64,
    network_mode: String,
    network_duplex: String,
    usbnet_mode: i64,
    cellular_suppressed: bool,
}

impl DeviceInfoSnapshotResult {
    fn empty(cellular_suppressed: bool) -> Self {
        let mut result = Self {
            imei: String::new(),
            firmware: String::new(),
            sim_inserted: false,
            imsi: String::new(),
            phone_number: String::new(),
            iccid: String::new(),
            operator: String::new(),
            reg_status: 0,
            reg_status_text: String::new(),
            lac: String::new(),
            cell_id: String::new(),
            signal_dbm: -999,
            signal_rsrp: 0,
            signal_rsrq: 0,
            apn: String::new(),
            ims_status: 0,
            network_mode: String::new(),
            network_duplex: String::new(),
            usbnet_mode: -1,
            cellular_suppressed,
        };
        if cellular_suppressed {
            result.reg_status_text = "飞行模式 · 蜂窝射频关闭".to_owned();
            result.network_mode = "飞行模式".to_owned();
        }
        result
    }
}

#[derive(Debug)]
struct SerialFrame {
    data: Vec<u8>,
    is_urc: bool,
    pure_async_urc: bool,
    sms_generation: u64,
    sim_event: Option<SimEvent>,
    sim_actions: Option<SimEventActions>,
    sms_event: Option<SmsStorageEvent>,
    sms_ingest_owned: bool,
    call_event_owned: bool,
    ussd_event: Option<UssdEvent>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SimEvent {
    Ready,
    PinState { state: String },
    Status { inserted: Option<bool> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SimEventActions {
    notify_ready: bool,
    notify_status: bool,
}

impl SimEvent {
    fn actions(&self) -> SimEventActions {
        match self {
            Self::Ready => SimEventActions {
                notify_ready: true,
                notify_status: false,
            },
            Self::PinState { state } => SimEventActions {
                notify_ready: state == "READY",
                notify_status: true,
            },
            Self::Status { .. } => SimEventActions {
                notify_ready: false,
                notify_status: true,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SmsStorageEvent {
    storage: String,
    index: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct UssdEvent {
    status: i32,
    text: Option<String>,
    dcs: Option<i32>,
    decoded_text: Option<String>,
}

impl SerialFrame {
    fn new(data: Vec<u8>) -> Self {
        let (is_urc, pure_async_urc) = classify_frame(&data);
        let sim_event = parse_sim_event(&data);
        let sim_actions = sim_event.as_ref().map(SimEvent::actions);
        let sms_event = parse_sms_storage_event(&data);
        let ussd_event = parse_ussd_event(&data);
        Self {
            data,
            is_urc,
            pure_async_urc,
            sms_generation: 0,
            sim_event,
            sim_actions,
            sms_event,
            sms_ingest_owned: false,
            call_event_owned: false,
            ussd_event,
        }
    }
}

fn parse_ussd_event(frame: &[u8]) -> Option<UssdEvent> {
    let decoded = String::from_utf8_lossy(frame);
    let rest = decoded.trim().strip_prefix("+CUSD:")?.trim();
    let fields = parse_at_csv_fields(rest)?;
    let status = fields.first()?.trim().parse::<i32>().ok()?;
    let text = fields.get(1).cloned();
    let dcs = fields
        .get(2)
        .and_then(|value| value.trim().parse::<i32>().ok());
    let decoded_text = text
        .as_deref()
        .map(|value| proletarius_sms::ussd::decode_text(value, dcs.unwrap_or(-1)));
    Some(UssdEvent {
        status,
        text,
        dcs,
        decoded_text,
    })
}

// Parse modem CSV fields without splitting commas inside quoted USSD text.
// Doubled quotes and backslash-escaped quotes are accepted as literal quotes.
fn parse_at_csv_fields(input: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = input.chars().peekable();
    let mut quoted = false;
    let mut quote_closed = false;
    let mut was_quoted = false;

    while let Some(ch) = chars.next() {
        if quoted {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => {
                    quoted = false;
                    quote_closed = true;
                }
                '\\' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                _ => field.push(ch),
            }
            continue;
        }

        if quote_closed {
            if ch.is_whitespace() {
                continue;
            }
            if ch == ',' {
                fields.push(field.clone());
                field.clear();
                quote_closed = false;
                was_quoted = false;
                continue;
            }
            return None;
        }

        match ch {
            '"' if field.trim().is_empty() => {
                field.clear();
                quoted = true;
                was_quoted = true;
            }
            '"' => return None,
            ',' => {
                fields.push(if was_quoted {
                    field.clone()
                } else {
                    field.trim().to_owned()
                });
                field.clear();
                was_quoted = false;
            }
            _ => field.push(ch),
        }
    }
    if quoted {
        return None;
    }
    fields.push(if was_quoted {
        field
    } else {
        field.trim().to_owned()
    });
    Some(fields)
}

fn parse_sms_storage_event(frame: &[u8]) -> Option<SmsStorageEvent> {
    let decoded = String::from_utf8_lossy(frame);
    let rest = decoded.trim().strip_prefix("+CMTI:")?.trim();
    let mut fields = rest.split(',');
    let storage = fields.next()?.trim().trim_matches('"');
    let index = fields.next()?.trim();
    if storage.is_empty() || index.is_empty() {
        return None;
    }
    Some(SmsStorageEvent {
        storage: storage.to_owned(),
        index: index.to_owned(),
    })
}

fn parse_sim_event(frame: &[u8]) -> Option<SimEvent> {
    let decoded = String::from_utf8_lossy(frame);
    let line = decoded.trim();
    if line == "RDY" {
        return Some(SimEvent::Ready);
    }
    if line == "+CPIN" {
        return Some(SimEvent::PinState {
            state: String::new(),
        });
    }
    if let Some(state) = line.strip_prefix("+CPIN:") {
        return Some(SimEvent::PinState {
            state: state.trim().trim_matches('"').to_owned(),
        });
    }
    if line == "+QSIMSTAT" {
        return Some(SimEvent::Status { inserted: None });
    }
    if let Some(rest) = line.strip_prefix("+QSIMSTAT:") {
        let fields: Vec<_> = rest.split(',').collect();
        let inserted = fields
            .get(1)
            .and_then(|value| parse_legacy_integer(value))
            .filter(|value| *value >= 0)
            .map(|value| value == 1);
        return Some(SimEvent::Status { inserted });
    }
    None
}

fn parse_legacy_integer(value: &str) -> Option<i64> {
    let value = value.trim_start();
    let (digits, sign) = match value.as_bytes().first() {
        Some(b'+') => (&value[1..], 1_i64),
        Some(b'-') => (&value[1..], -1_i64),
        _ => (value, 1_i64),
    };
    let length = digits.bytes().take_while(u8::is_ascii_digit).count();
    if length == 0 {
        return None;
    }
    digits[..length].parse::<i64>().ok()?.checked_mul(sign)
}

fn classify_frame(frame: &[u8]) -> (bool, bool) {
    let decoded = String::from_utf8_lossy(frame);
    let line = decoded.trim();
    if line.is_empty()
        || ["+CSIM:", "+CGLA:", "+CCHO:", "+CMGR:", "+CMGS:", "+QENG:"]
            .iter()
            .any(|prefix| line.starts_with(prefix))
    {
        return (false, false);
    }

    let is_urc = matches!(line.chars().next(), Some('+' | '^' | '$'))
        || matches!(
            line,
            "RING"
                | "RDY"
                | "SMS Ready"
                | "Call Ready"
                | "NORMAL POWER DOWN"
                | "NO CARRIER"
                | "BUSY"
                | "NO ANSWER"
        );
    if !is_urc {
        return (false, false);
    }

    let key = urc_key(line);
    let pure_async_urc = matches!(
        key,
        "+CUSD" | "+CMTI" | "RING" | "+CLIP" | "+QSIMSTAT" | "+QSTKURC" | "+QPCMV"
    );
    (true, pure_async_urc)
}

fn urc_key(line: &str) -> &str {
    if matches!(line.chars().next(), Some('+' | '^' | '$')) {
        if let Some(index) = line.find(':').filter(|index| *index > 0) {
            return &line[..index];
        }
        if let Some(index) = line
            .find(|character| character == ' ' || character == ',')
            .filter(|index| *index > 0)
        {
            return &line[..index];
        }
        return line;
    }
    if matches!(
        line,
        "NO CARRIER" | "NO ANSWER" | "SMS Ready" | "Call Ready" | "NORMAL POWER DOWN"
    ) {
        return line;
    }
    line.find(' ').map_or(line, |index| &line[..index])
}

#[derive(Clone, Debug)]
pub(crate) struct ActorError {
    code: &'static str,
    message: String,
}

impl ActorError {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new("serial_not_open", message)
    }

    #[cfg(test)]
    pub(crate) fn fixture_open_failed() -> Self {
        Self::new(
            "serial_open_failed",
            "fixture serial port could not be opened",
        )
    }
}

impl ModemActor {
    pub(crate) fn new() -> io::Result<Self> {
        Self::with_port_opener(open_serial_port)
    }

    pub(crate) fn new_with_module_recovery(gate: crate::module_recovery::Gate) -> io::Result<Self> {
        Self::with_port_opener_and_recovery(open_serial_port, gate)
    }

    pub(crate) fn ensure_sms_receive_worker(&self, store: proletarius_store::ArchiveStore) {
        self.ensure_receive_worker(store);
    }

    pub(crate) fn request_manual_sms_scan(&self) -> ManualSmsScanRequest {
        match self.scan_sms_receive() {
            Ok(()) => ManualSmsScanRequest::Scheduled,
            Err(error) if error.code == "serial_not_open" => {
                ManualSmsScanRequest::SerialUnavailable
            }
            Err(error) => ManualSmsScanRequest::Rejected(error.code),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_port_opener(
        opener: impl Fn(String, u32) -> Result<Box<dyn serialport::SerialPort>, ActorError>
        + Send
        + 'static,
    ) -> io::Result<Self> {
        Self::with_port_opener(opener)
    }

    #[cfg(test)]
    pub(crate) fn with_test_port_opener_and_module_recovery(
        opener: impl Fn(String, u32) -> Result<Box<dyn serialport::SerialPort>, ActorError>
        + Send
        + 'static,
        gate: crate::module_recovery::Gate,
    ) -> io::Result<Self> {
        Self::with_port_opener_and_recovery(opener, gate)
    }

    fn with_port_opener(
        opener: impl Fn(String, u32) -> Result<Box<dyn serialport::SerialPort>, ActorError>
        + Send
        + 'static,
    ) -> io::Result<Self> {
        Self::with_port_opener_and_recovery(opener, Default::default())
    }

    fn with_port_opener_and_recovery(
        opener: impl Fn(String, u32) -> Result<Box<dyn serialport::SerialPort>, ActorError>
        + Send
        + 'static,
        module_recovery: crate::module_recovery::Gate,
    ) -> io::Result<Self> {
        let actor_recovery = module_recovery.clone();
        let owner_epoch = serial_sms::new_owner_epoch()?;
        let (sender, receiver) = mpsc::sync_channel(ACTOR_QUEUE_CAPACITY);
        let wake = Arc::new(tokio::sync::Notify::new());
        let actor_wake = wake.clone();
        let cellular_suppressed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let actor_suppressed = cellular_suppressed.clone();
        let call_capture = Arc::new(std::sync::Mutex::new(None));
        let actor_capture = call_capture.clone();
        let live_serial_session = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let actor_session = live_serial_session.clone();
        let card_shared = Arc::new(CardShared::default());
        let actor_card_shared = card_shared.clone();
        let native_shared = Arc::new(NativeShared::default());
        let actor_native_shared = native_shared.clone();
        let actor_thread = thread::Builder::new()
            .name("switch-modem-actor".to_owned())
            .spawn(move || {
                run_actor(
                    receiver,
                    Box::new(opener),
                    actor_wake,
                    actor_suppressed,
                    actor_capture,
                    actor_session,
                    owner_epoch,
                    actor_card_shared,
                    actor_native_shared,
                    actor_recovery,
                )
            })?;
        Ok(Self {
            compatibility_rpc: false,
            client: Arc::new(ActorClient {
                module_recovery,
                sender,
                final_close: std::sync::Mutex::new(None),
                actor_thread: std::sync::Mutex::new(Some(actor_thread)),
                wake,
                cellular_suppressed,
                call_capture,
                live_serial_session,
                card_shared,
                native_shared,
                module_esim_cache: Arc::new(crate::module_esim_transport::SerialCache::default()),
            }),
            receive_worker: Arc::new(ReceiveWorker::default()),
        })
    }

    pub(crate) fn call_binding_current(&self, session: u64) -> bool {
        session != 0
            && self
                .client
                .live_serial_session
                .load(std::sync::atomic::Ordering::Acquire)
                == session
    }
    pub(crate) fn set_call_capture(&self, capture: crate::module_radio::CallCapture) {
        *self
            .client
            .call_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(capture);
    }
    pub(crate) fn set_cellular_suppressed(&self, suppressed: bool) {
        // A seqlock token closes both policy ABA and the bool/revision write gap.
        let shared = &self.client.native_shared;
        let _write = shared
            .policy_write
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.cellular_suppressed() != suppressed {
            let revision = shared
                .policy_revision
                .load(std::sync::atomic::Ordering::Acquire);
            shared.policy_revision.store(
                revision.saturating_add(1),
                std::sync::atomic::Ordering::Release,
            );
            self.client
                .cellular_suppressed
                .store(suppressed, std::sync::atomic::Ordering::Release);
            shared.policy_revision.store(
                revision.saturating_add(2),
                std::sync::atomic::Ordering::Release,
            );
        }
    }

    pub(crate) fn cellular_policy_revision(&self) -> u64 {
        self.client
            .native_shared
            .policy_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn submit<T: Send + 'static>(
        &self,
        command: impl FnOnce(SyncSender<Result<T, ActorError>>) -> Command,
        timeout: Duration,
    ) -> Result<T, ActorError> {
        if self.client.module_recovery.is_quarantined() {
            return Err(ActorError::new(
                crate::module_recovery::CODE,
                crate::module_recovery::MESSAGE,
            ));
        }
        let (reply, result) = mpsc::sync_channel(1);
        let command = command(reply.clone());
        let command = if self.compatibility_rpc {
            Command::LegacyGate(Box::new(move |state, opener| {
                if state.native.managed() {
                    let _ = reply.send(Err(native_owned()));
                } else {
                    state.process(command, opener);
                }
            }))
        } else {
            command
        };
        self.client
            .sender
            .try_send(command)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    ActorError::new("serial_actor_busy", "serial actor request capacity is full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    ActorError::new("serial_actor_stopped", "serial actor has stopped")
                }
            })?;
        result.recv_timeout(timeout).map_err(|error| match error {
            RecvTimeoutError::Timeout => {
                ActorError::new("serial_actor_timeout", "serial actor request timed out")
            }
            RecvTimeoutError::Disconnected => {
                ActorError::new("serial_actor_stopped", "serial actor has stopped")
            }
        })?
    }

    #[cfg(test)]
    fn open(&self, path: String, baud_rate: u32) -> Result<(), ActorError> {
        self.open_with_initialization(path, baud_rate, false)
            .map(|_| ())
    }

    fn open_with_initialization(
        &self,
        path: String,
        baud_rate: u32,
        initialization_required: bool,
    ) -> Result<u64, ActorError> {
        let path = path.trim().to_owned();
        if path.is_empty()
            || !path.starts_with("/dev/")
            || path.len() > MAX_PORT_NAME_BYTES
            || path.chars().any(char::is_control)
        {
            return Err(ActorError::invalid("invalid serial port path"));
        }
        if !(1200..=4_000_000).contains(&baud_rate) {
            return Err(ActorError::invalid(
                "serial baud rate is outside the supported range",
            ));
        }
        self.submit(
            |reply| Command::Open {
                path,
                baud_rate,
                initialization_required,
                reply,
            },
            RPC_TIMEOUT,
        )
    }

    fn discover_at_port(&self, candidates: Vec<String>) -> Result<DiscoveredATPort, ActorError> {
        if candidates.len() > MAX_AT_DISCOVERY_CANDIDATES
            || candidates.iter().any(|candidate| {
                candidate.trim().is_empty()
                    || candidate.len() > MAX_PORT_NAME_BYTES
                    || !candidate.starts_with("/dev/cu.")
                    || candidate.chars().any(char::is_control)
            })
        {
            return Err(ActorError::invalid("invalid AT port discovery candidates"));
        }
        let scan_timeout = AT_DISCOVERY_PROBE_TIMEOUT
            .saturating_mul(candidates.len() as u32)
            .saturating_add(Duration::from_secs(10));
        self.submit(
            |reply| Command::DiscoverATPort {
                candidates: Some(candidates),
                reply,
            },
            scan_timeout.max(Duration::from_secs(10)),
        )
    }

    fn initialize(&self) -> Result<InitializationResult, ActorError> {
        let queue_wait = MAX_COMMAND_TIMEOUT.saturating_mul(
            (HIGH_COMMAND_QUEUE_CAPACITY + NORMAL_COMMAND_QUEUE_CAPACITY + 1) as u32,
        );
        self.submit(
            |reply| Command::Initialize { reply },
            queue_wait + Duration::from_secs(30),
        )
    }

    fn read(&self, maximum: usize, timeout_ms: u64) -> Result<SerialFrame, ActorError> {
        if maximum == 0 || maximum > MAX_READ_BYTES || !(1..=250).contains(&timeout_ms) {
            return Err(ActorError::invalid("invalid serial read bounds"));
        }
        let timeout = Duration::from_millis(timeout_ms);
        self.submit(
            |reply| Command::Read {
                maximum,
                timeout,
                reply,
            },
            timeout + Duration::from_secs(1),
        )
    }

    fn write(&self, data: Vec<u8>) -> Result<usize, ActorError> {
        if data.is_empty() || data.len() > MAX_WRITE_BYTES {
            return Err(ActorError::invalid("invalid serial write length"));
        }
        self.submit(|reply| Command::Write { data, reply }, RPC_TIMEOUT)
    }

    #[cfg(test)]
    fn execute(&self, command: String, timeout_ms: u64) -> Result<CommandResult, ActorError> {
        self.execute_scheduled(command, timeout_ms, false, None, None)
    }

    fn execute_scheduled(
        &self,
        command: String,
        timeout_ms: u64,
        high_priority: bool,
        follow_up: Option<Vec<u8>>,
        expected_sms_generation: Option<u64>,
    ) -> Result<CommandResult, ActorError> {
        let command = command.trim().to_owned();
        let timeout = Duration::from_millis(timeout_ms);
        if let Some(payload) = follow_up.as_ref() {
            if !valid_sms_follow_up(payload) {
                return Err(ActorError::invalid("invalid interactive SMS payload"));
            }
        }
        if command.is_empty()
            || command.len() + 2 > MAX_WRITE_BYTES
            || command.chars().any(char::is_control)
            || timeout < Duration::from_millis(200)
            || timeout > MAX_COMMAND_TIMEOUT
        {
            return Err(ActorError::invalid("invalid AT command or timeout bounds"));
        }
        let queue_wait = MAX_COMMAND_TIMEOUT.saturating_mul(
            (HIGH_COMMAND_QUEUE_CAPACITY + NORMAL_COMMAND_QUEUE_CAPACITY + 1) as u32,
        );
        self.submit(
            |reply| Command::Execute {
                command,
                timeout,
                high_priority,
                follow_up,
                expected_sms_generation,
                reply,
            },
            timeout + queue_wait + Duration::from_secs(2),
        )
    }

    fn execute_call_command(&self, action: &str, value: &str) -> Result<CommandResult, ActorError> {
        let (_, command, timeout_ms) = call_command_spec(action, value)?;
        self.execute_scheduled(command, timeout_ms, false, None, None)
    }

    fn typed_query(&self, query: &str) -> Result<TypedQueryResult, ActorError> {
        if query == "operator" {
            // Preserve QueryOperator's legacy sequence: request numeric COPS
            // formatting (best-effort), then read the serving operator. The
            // format selector does not change radio/operator selection.
            if let Ok(format) =
                self.execute_scheduled("AT+COPS=3,2".to_owned(), 2_000, false, None, None)
                && format.result == "timeout"
            {
                let _ = self.write(vec![0x1b]);
            }
            let reply = self.execute_scheduled("AT+COPS?".to_owned(), 2_000, false, None, None)?;
            if reply.result != "ok" {
                return Err(ActorError::new(
                    "modem_command_failed",
                    "AT+COPS? did not complete successfully",
                ));
            }
            return Ok(TypedQueryResult {
                string_value: Some(crate::operator_name::resolve_serving_operator_name(
                    &parse_cops_operator(&reply.lines),
                )),
                ..TypedQueryResult::empty("operator")
            });
        }

        let (command, timeout_ms) = match query {
            "call_state" => ("AT+CLCC", 3_000),
            "imei" => ("AT+CGSN", 2_000),
            "firmware" => ("AT+CGMR", 2_000),
            "sim_inserted" => ("AT+QSIMSTAT?", 2_000),
            "imsi" => ("AT+CIMI", 2_000),
            "iccid" => ("AT+QCCID", 2_000),
            "csq" => ("AT+CSQ", 2_000),
            "registration" => ("AT+CREG?", 2_000),
            "serving_cell_lte" => ("AT+QENG=\"servingcell\"", 3_000),
            "apn" => ("AT+CGDCONT?", 2_000),
            "ims_status" => ("AT+QIMS?", 2_000),
            "network_radio" => ("AT+QNWINFO", 2_000),
            "network_mode_fallback" => ("AT+COPS?", 2_000),
            "native_spn" => ("AT+CRSM=176,28486,0,0,17", 2_000),
            "smsc" => ("AT+CSCA?", 2_000),
            "msisdn" => ("AT+CNUM", 2_000),
            "usbnet_mode" => ("AT+QCFG=\"usbnet\"?", 2_000),
            "voice_enabled" => ("AT+QCFG=\"usbcfg\"?", 3_000),
            _ => return Err(ActorError::invalid("unsupported typed modem query")),
        };

        if query == "sim_inserted" {
            let inserted = self
                .execute_scheduled(command.to_owned(), timeout_ms, false, None, None)
                .ok()
                .and_then(|reply| parse_qsimstat_inserted(&reply.lines));
            let inserted = match inserted {
                Some(inserted) => inserted,
                None => {
                    let reply = self.execute_scheduled(
                        "AT+CPIN?".to_owned(),
                        timeout_ms,
                        false,
                        None,
                        None,
                    )?;
                    parse_cpin_inserted(&reply.lines).unwrap_or(false)
                }
            };
            return Ok(TypedQueryResult {
                bool_value: Some(inserted),
                ..TypedQueryResult::empty("sim_inserted")
            });
        }

        let phone_binding = if query == "msisdn" {
            self.capture_serial_sms_binding().ok()
        } else {
            None
        };
        let reply = self.execute_scheduled(command.to_owned(), timeout_ms, false, None, None)?;
        if query == "msisdn" && reply.result == "ok" {
            if let Some(binding) = phone_binding {
                let phone = parse_msisdn(&reply.lines);
                let _ = self.submit(
                    |reply| Command::CacheSerialSmsPhone {
                        binding,
                        phone,
                        reply,
                    },
                    RPC_TIMEOUT,
                );
            }
        }
        if query == "call_state" {
            if reply.result != "ok" {
                return Err(ActorError::new(
                    "modem_command_failed",
                    "AT+CLCC did not complete successfully",
                ));
            }
            return Ok(TypedQueryResult {
                call_state: Some(call_state_projection::project(&reply.lines.join("\r\n"))),
                ..TypedQueryResult::empty("call_state")
            });
        }
        if query == "network_mode_fallback" && reply.result != "ok" {
            return Err(ActorError::new(
                "modem_command_failed",
                "AT+COPS? did not complete successfully",
            ));
        }
        if query == "native_spn" {
            if reply.result != "ok" {
                return Err(ActorError::new(
                    "modem_command_failed",
                    format!(
                        "AT+CRSM=176,28486,0,0,17 did not complete: {}",
                        reply.lines.join("\n")
                    ),
                ));
            }
            let raw = parse_crsm_response(&reply.lines)?;
            let name = decode_ef_spn(&raw)
                .map_err(|message| ActorError::new("sim_spn_invalid", message))?;
            return Ok(TypedQueryResult {
                string_value: Some(name),
                ..TypedQueryResult::empty("native_spn")
            });
        }
        let mut result = TypedQueryResult::empty(match query {
            "imei" => "imei",
            "firmware" => "firmware",
            "imsi" => "imsi",
            "iccid" => "iccid",
            "csq" => "csq",
            "registration" => "registration",
            "serving_cell_lte" => "serving_cell_lte",
            "apn" => "apn",
            "ims_status" => "ims_status",
            "network_radio" => "network_radio",
            "network_mode_fallback" => "network_mode_fallback",
            "native_spn" => unreachable!("native SPN query returned above"),
            "smsc" => "smsc",
            "msisdn" => "msisdn",
            "usbnet_mode" => "usbnet_mode",
            "voice_enabled" => "voice_enabled",
            _ => unreachable!("query was validated before execution"),
        });
        match query {
            "imei" => result.string_value = Some(parse_imei(&reply.lines)),
            "firmware" => result.string_value = Some(parse_firmware(&reply.lines)),
            "imsi" => result.string_value = Some(parse_imsi(&reply.lines)),
            "iccid" => result.string_value = Some(parse_iccid(&reply.lines)),
            "csq" => {
                let (rssi, dbm) = parse_csq(&reply.lines).unwrap_or((0, -999));
                result.rssi = Some(rssi);
                result.dbm = Some(dbm);
            }
            "registration" => {
                let parsed = parse_creg(&reply.lines);
                result.registration_valid = Some(parsed.is_some());
                let (status, lac, cell_id) = parsed.unwrap_or((0, String::new(), String::new()));
                result.reg_status = Some(status);
                result.lac = Some(lac);
                result.cell_id = Some(cell_id);
            }
            "serving_cell_lte" => {
                let (rsrp, rsrq, sinr, duplex, band, channel) = parse_serving_cell_lte(
                    &reply.lines,
                )
                .unwrap_or((0, 0, 0, String::new(), String::new(), 0));
                result.rsrp = Some(rsrp);
                result.rsrq = Some(rsrq);
                result.sinr = Some(sinr);
                result.duplex = Some(duplex);
                result.band = Some(band);
                result.channel = Some(channel);
            }
            "apn" => result.string_value = Some(parse_apn(&reply.lines)),
            "ims_status" => result.int_value = Some(parse_qims(&reply.lines).unwrap_or(0)),
            "network_radio" => {
                let (mode, duplex, band, channel) = parse_qnwinfo_radio(&reply.lines);
                result.string_value = Some(mode);
                result.duplex = Some(duplex);
                result.band = Some(band);
                result.channel = Some(channel);
            }
            "network_mode_fallback" => {
                result.string_value = Some(parse_cops_act(&reply.lines));
            }
            "native_spn" => unreachable!("native SPN query returned above"),
            "smsc" => result.string_value = Some(parse_smsc(&reply.lines)),
            "msisdn" => result.string_value = Some(parse_msisdn(&reply.lines)),
            "usbnet_mode" => result.int_value = Some(parse_usbnet(&reply.lines).unwrap_or(-1)),
            "voice_enabled" => result.bool_value = parse_uac_enabled(&reply.lines),
            _ => unreachable!("query was validated before execution"),
        }
        Ok(result)
    }

    fn device_info_snapshot(&self, suppress_cellular: bool) -> DeviceInfoSnapshotResult {
        let mut result = DeviceInfoSnapshotResult::empty(false);
        let suppress_for_snapshot = suppress_cellular
            || self
                .client
                .cellular_suppressed
                .load(std::sync::atomic::Ordering::Acquire);

        if let Ok(query) = self.typed_query("imei") {
            result.imei = query.string_value.unwrap_or_default();
        }
        if let Ok(query) = self.typed_query("firmware") {
            result.firmware = query.string_value.unwrap_or_default();
        }
        if let Ok(query) = self.typed_query("sim_inserted") {
            result.sim_inserted = query.bool_value.unwrap_or(false);
        }
        if result.sim_inserted {
            if let Ok(query) = self.typed_query("imsi") {
                result.imsi = query.string_value.unwrap_or_default();
            }
            if let Ok(query) = self.typed_query("msisdn") {
                result.phone_number = query.string_value.unwrap_or_default();
            }
        }
        if let Ok(query) = self.typed_query("iccid") {
            result.iccid = query.string_value.unwrap_or_default();
        }

        if !suppress_for_snapshot {
            if let Ok(query) = self.typed_query("operator") {
                result.operator = query.string_value.unwrap_or_default();
            }
            if let Ok(query) = self.typed_query("registration")
                && query.registration_valid == Some(true)
            {
                let status = query.reg_status.unwrap_or_default();
                result.reg_status = status;
                result.reg_status_text = match status {
                    0 => "未注册",
                    1 => "已注册(本地)",
                    2 => "搜索中",
                    3 => "注册被拒",
                    5 => "已注册(漫游)",
                    _ => "未知",
                }
                .to_owned();
                result.lac = query.lac.unwrap_or_default();
                result.cell_id = query.cell_id.unwrap_or_default();
            }
            if let Ok(query) = self.typed_query("csq")
                && let Some(dbm) = query.dbm
                && dbm != -999
            {
                result.signal_dbm = dbm;
            }
            if let Ok(query) = self.typed_query("serving_cell_lte") {
                result.signal_rsrp = query.rsrp.unwrap_or_default();
                result.signal_rsrq = query.rsrq.unwrap_or_default();
            }
        }

        if let Ok(query) = self.typed_query("apn") {
            result.apn = query.string_value.unwrap_or_default();
        }
        if !suppress_for_snapshot {
            if let Ok(query) = self.typed_query("ims_status") {
                result.ims_status = query.int_value.unwrap_or_default();
            }
            if let Ok(query) = self.typed_query("network_radio") {
                result.network_mode = query.string_value.unwrap_or_default();
                result.network_duplex = query.duplex.unwrap_or_default();
            }
        }
        if let Ok(query) = self.typed_query("usbnet_mode") {
            result.usbnet_mode = query.int_value.unwrap_or(-1);
        }

        result.cellular_suppressed = suppress_for_snapshot
            || self
                .client
                .cellular_suppressed
                .load(std::sync::atomic::Ordering::Acquire);
        if result.cellular_suppressed {
            result.operator.clear();
            result.reg_status = 0;
            result.reg_status_text = "飞行模式 · 蜂窝射频关闭".to_owned();
            result.lac.clear();
            result.cell_id.clear();
            result.signal_dbm = -999;
            result.signal_rsrp = 0;
            result.signal_rsrq = 0;
            result.ims_status = 0;
            result.network_mode = "飞行模式".to_owned();
            result.network_duplex.clear();
        }
        result
    }

    pub(crate) fn query_voice_enabled(&self) -> Option<bool> {
        self.typed_query("voice_enabled")
            .ok()
            .and_then(|result| result.bool_value)
    }

    pub(crate) fn submit_sms(
        &self,
        recipient: String,
        text: String,
        encoding: String,
        expected_sms_generation: Option<u64>,
    ) -> Result<SmsSubmitResult, ActorError> {
        self.submit_sms_with_binding(recipient, text, encoding, expected_sms_generation, None)
    }

    fn submit_sms_with_binding(
        &self,
        recipient: String,
        text: String,
        encoding: String,
        expected_sms_generation: Option<u64>,
        binding: Option<SerialSmsBinding>,
    ) -> Result<SmsSubmitResult, ActorError> {
        if recipient.trim().is_empty()
            || recipient.len() > 128
            || text.trim().is_empty()
            || text.len() > MAX_SMS_SUBMIT_BYTES
        {
            return Err(ActorError::invalid(
                "SMS recipient or text is outside supported bounds",
            ));
        }
        let policy = match encoding.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => proletarius_sms::submit::EncodingPolicy::Auto,
            "ucs2" => proletarius_sms::submit::EncodingPolicy::Ucs2,
            "modem" => proletarius_sms::submit::EncodingPolicy::Modem,
            _ => return Err(ActorError::invalid("SMS encoding policy is invalid")),
        };
        let mut random = [0_u8; 1];
        getrandom::fill(&mut random)
            .map_err(|_| ActorError::new("entropy_unavailable", "SMS reference is unavailable"))?;
        let tpdus = proletarius_sms::submit::prepare_with_policy(
            &recipient, &text, random[0], policy, false,
        )
        .map_err(|_| ActorError::invalid("SMS recipient or text cannot be encoded"))?;
        if tpdus.is_empty()
            || tpdus.len() > 255
            || tpdus
                .iter()
                .any(|tpdu| !(8..=232).contains(&tpdu.len()) || tpdu[0] & 0x03 != 0x01)
        {
            return Err(ActorError::invalid(
                "SMS encoder returned an invalid segment list",
            ));
        }
        let timeout = Duration::from_secs(2 * 60 * 60);
        self.submit(
            |reply| Command::SubmitSms {
                tpdus,
                binding,
                expected_sms_generation,
                reply,
            },
            timeout,
        )
    }

    pub(crate) fn current_sms_generation(&self) -> Result<u64, ActorError> {
        self.submit(|reply| Command::ReadSmsGeneration { reply }, RPC_TIMEOUT)
    }

    fn delete_archived_sms(
        &self,
        index: u32,
        pdu: String,
        expected_sms_generation: u64,
    ) -> Result<StoredSmsDeleteResult, ActorError> {
        self.delete_archived_sms_fenced(index, pdu, expected_sms_generation, None)
    }

    fn delete_archived_sms_fenced(
        &self,
        index: u32,
        pdu: String,
        expected_sms_generation: u64,
        receive_fence: Option<ReceiveFence>,
    ) -> Result<StoredSmsDeleteResult, ActorError> {
        if index == 0 || !valid_sms_pdu(&pdu) {
            return Err(ActorError::invalid(
                "archived SMS identity is outside supported bounds",
            ));
        }
        let queue_wait = MAX_COMMAND_TIMEOUT.saturating_mul(
            (HIGH_COMMAND_QUEUE_CAPACITY + NORMAL_COMMAND_QUEUE_CAPACITY + 1) as u32,
        );
        self.submit(
            |reply| Command::DeleteArchivedSms {
                index,
                pdu,
                expected_sms_generation,
                receive_fence,
                reply,
            },
            queue_wait + Duration::from_secs(20),
        )
    }

    fn read_stored_sms(
        &self,
        storage: Option<String>,
        index: Option<u32>,
        expected_sms_generation: u64,
    ) -> Result<StoredSmsReadResult, ActorError> {
        self.read_stored_sms_fenced(storage, index, expected_sms_generation, None)
    }

    fn read_stored_sms_fenced(
        &self,
        storage: Option<String>,
        index: Option<u32>,
        expected_sms_generation: u64,
        receive_fence: Option<ReceiveFence>,
    ) -> Result<StoredSmsReadResult, ActorError> {
        let storage = storage
            .map(|storage| {
                normalize_sms_storage(&storage).ok_or_else(|| {
                    ActorError::invalid("SMS storage must be a bounded alphanumeric identifier")
                })
            })
            .transpose()?;
        if index == Some(0) {
            return Err(ActorError::invalid("SMS storage index must be nonzero"));
        }
        let queue_wait = MAX_COMMAND_TIMEOUT.saturating_mul(
            (HIGH_COMMAND_QUEUE_CAPACITY + NORMAL_COMMAND_QUEUE_CAPACITY + 1) as u32,
        );
        self.submit(
            |reply| Command::ReadStoredSms {
                storage,
                index,
                expected_sms_generation,
                receive_fence,
                reply,
            },
            queue_wait + Duration::from_secs(32),
        )
    }

    fn confirm_sms_generation_fenced(
        &self,
        expected_sms_generation: u64,
        receive_fence: Option<ReceiveFence>,
    ) -> Result<(), ActorError> {
        self.submit(
            |reply| Command::ConfirmSmsGeneration {
                expected_sms_generation,
                receive_fence,
                reply,
            },
            RPC_TIMEOUT,
        )
    }

    fn close(&self) -> Result<(), ActorError> {
        self.submit(|reply| Command::Close { reply }, RPC_TIMEOUT)
    }

    pub(crate) fn shutdown(&self) -> Result<(), ActorError> {
        // Teardown runs on a blocking thread after admission stops. The final
        // command closes the port and terminates the actor; join below is part
        // of the receipt, since a close acknowledgement alone does not prove
        // the owner thread has exited.
        let mut receipt = self.client.final_close.lock().map_err(|_| {
            ActorError::new(
                "serial_shutdown_unconfirmed",
                "final close receipt was poisoned",
            )
        })?;
        if let Some(result) = receipt.as_ref() {
            return result.clone();
        }
        let result = (|| {
            let (reply, result) = mpsc::sync_channel(1);
            self.client
                .sender
                .send(Command::Finalize { reply })
                .map_err(|_| {
                    ActorError::new(
                        "serial_shutdown_unconfirmed",
                        "serial actor stopped before accepting final shutdown",
                    )
                })?;
            result.recv().map_err(|_| {
                ActorError::new(
                    "serial_shutdown_unconfirmed",
                    "serial actor did not acknowledge final shutdown",
                )
            })?
        })();
        let joined = self
            .client
            .actor_thread
            .lock()
            .map_err(|_| {
                ActorError::new(
                    "serial_shutdown_unconfirmed",
                    "serial actor join receipt was poisoned",
                )
            })?
            .take()
            .map(|actor_thread| {
                actor_thread.join().map_err(|_| {
                    ActorError::new(
                        "serial_actor_join_failed",
                        "serial actor thread panicked before clean exit",
                    )
                })
            })
            .unwrap_or(Ok(()));
        let result = result.and(joined);
        // This synchronous owner continues inside the retained blocking task
        // even when an async caller stops waiting. Concurrent/repeated callers
        // reuse this exact close-and-join receipt, including any failure.
        *receipt = Some(result.clone());
        result
    }
}

#[cfg(test)]
#[path = "modem_actor/quarantine_tests.rs"]
mod quarantine_tests;

fn valid_sms_follow_up(payload: &[u8]) -> bool {
    let Some((&terminator, pdu)) = payload.split_last() else {
        return false;
    };
    terminator == 0x1a
        && !pdu.is_empty()
        && pdu.len() % 2 == 0
        && pdu.iter().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_sms_pdu(pdu: &str) -> bool {
    !pdu.is_empty()
        && pdu.len() <= 8192
        && pdu.len() % 2 == 0
        && pdu.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn has_cmgs_ack(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        line.trim()
            .strip_prefix("+CMGS:")
            .and_then(|value| value.trim().parse::<u16>().ok())
            .is_some()
    })
}

fn send_completion_error(completion: CommandCompletion, error: ActorError) {
    match completion {
        CommandCompletion::NativeQuery(completion) => {
            let _ = completion.reply.send(Err(error));
        }
        CommandCompletion::Card(completion) => completion.fail(error),
        CommandCompletion::Radio(transaction) => fail_radio(transaction, error.code),
        CommandCompletion::Initialize(transaction) => {
            let _ = transaction.reply.send(Err(error));
        }
        CommandCompletion::Execute(reply) => {
            let _ = reply.send(Err(error));
        }
        CommandCompletion::SubmitSms(transaction) => {
            let _ = transaction.reply.send(Err(error));
        }
        CommandCompletion::DeleteArchivedSms(transaction) => {
            if matches!(transaction.step, StoredSmsDeleteStep::Delete) {
                // CMGD may already have reached the modem. Disconnect/close
                // cannot establish either deletion or retention of the source.
                let _ = transaction.reply.send(Ok(StoredSmsDeleteResult {
                    deleted: false,
                    reason: "delete_outcome_uncertain",
                }));
            } else {
                let _ = transaction.reply.send(Err(error));
            }
        }
        CommandCompletion::ReadStoredSms(transaction) => {
            let _ = transaction.reply.send(Err(error));
        }
    }
}

fn normalize_sms_storage(storage: &str) -> Option<String> {
    let storage = storage.trim().trim_matches('"');
    if storage.is_empty()
        || storage.len() > 8
        || !storage.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(storage.to_ascii_uppercase())
}

fn sms_storages(lines: &[String]) -> Option<Vec<String>> {
    let line = lines
        .iter()
        .find(|line| line.trim_start().starts_with("+CPMS:"))?;
    let value = line.trim_start().strip_prefix("+CPMS:")?.trim_start();
    let fields = parse_at_csv_fields(value)?;
    let mut storages = Vec::with_capacity(3);
    for index in [0, 3, 6] {
        let storage = fields
            .get(index)
            .and_then(|value| normalize_sms_storage(value))?;
        storages.push(storage);
    }
    Some(storages)
}

fn set_sms_storage_command(storages: &[String]) -> Option<String> {
    if storages.len() != 3
        || storages
            .iter()
            .any(|storage| normalize_sms_storage(storage).is_none())
    {
        return None;
    }
    Some(format!(
        "AT+CPMS=\"{}\",\"{}\",\"{}\"",
        storages[0], storages[1], storages[2]
    ))
}

fn first_sms_storage(lines: &[String]) -> Option<String> {
    let line = lines
        .iter()
        .find(|line| line.trim_start().starts_with("+CPMS:"))?;
    let value = line.trim_start().strip_prefix("+CPMS:")?.trim_start();
    if let Some(quoted) = value.strip_prefix('"') {
        let end = quoted.find('"')?;
        let storage = quoted[..end].trim();
        return (!storage.is_empty()).then(|| storage.to_ascii_uppercase());
    }
    let storage = value.split(',').next()?.trim();
    (!storage.is_empty()).then(|| storage.to_ascii_uppercase())
}

struct ActorState {
    module_recovery: crate::module_recovery::Gate,
    native: NativeState,
    card_shared: Arc<CardShared>,
    card_lease: Option<CardLease>,
    module_hold: Option<SerialModuleHold>,
    card_next_lease: u64,
    card_deferred_closes: Vec<Command>,
    serial_sms: serial_sms::SerialSmsState,
    port: Option<Box<dyn serialport::SerialPort>>,
    current_path: Option<String>,
    last_radio_port: Option<(String, u32)>,
    last_radio_receive_enabled: bool,
    current_baud: u32,
    frames: VecDeque<SerialFrame>,
    queued_bytes: usize,
    line_buffer: Vec<u8>,
    pending_read: Option<PendingRead>,
    pending_command: Option<PendingCommand>,
    pending_high_commands: VecDeque<QueuedCommand>,
    pending_normal_commands: VecDeque<QueuedCommand>,
    deferred_sms_submit: Option<DeferredSmsSubmit>,
    command_events: VecDeque<SerialFrame>,
    disconnected_error: Option<ActorError>,
    sms_generation: u64,
    timeout_streak: usize,
    serial_session: u64,
    sms_receive: ReceiveQueue,
    initialization: InitializationState,
    deferred_radio: Option<RadioTransaction>,
    radio_call_active: bool,
    live_serial_session: Arc<std::sync::atomic::AtomicU64>,
    call_capture: Arc<std::sync::Mutex<Option<crate::module_radio::CallCapture>>>,
    cellular_suppressed: Arc<std::sync::atomic::AtomicBool>,
    finalize_reply: Option<SyncSender<Result<(), ActorError>>>,
}

impl ActorState {
    fn new() -> Self {
        Self {
            module_recovery: crate::module_recovery::Gate::default(),
            native: NativeState::default(),
            card_shared: Arc::new(CardShared::default()),
            card_lease: None,
            module_hold: None,
            card_next_lease: 0,
            card_deferred_closes: Vec::new(),
            serial_sms: Default::default(),
            port: None,
            current_path: None,
            last_radio_port: None,
            last_radio_receive_enabled: false,
            current_baud: 0,
            frames: VecDeque::new(),
            queued_bytes: 0,
            line_buffer: Vec::new(),
            pending_read: None,
            pending_command: None,
            pending_high_commands: VecDeque::new(),
            pending_normal_commands: VecDeque::new(),
            deferred_sms_submit: None,
            command_events: VecDeque::new(),
            disconnected_error: None,
            sms_generation: 0,
            timeout_streak: 0,
            serial_session: 0,
            sms_receive: ReceiveQueue::new(Arc::new(tokio::sync::Notify::new())),
            initialization: InitializationState::default(),
            deferred_radio: None,
            radio_call_active: false,
            live_serial_session: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            call_capture: Arc::new(std::sync::Mutex::new(None)),
            cellular_suppressed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            finalize_reply: None,
        }
    }

    fn cellular_command_allowed(&self, command: &str) -> bool {
        !self
            .cellular_suppressed
            .load(std::sync::atomic::Ordering::Acquire)
            || !crate::module_radio_transport::is_cellular_probe(command)
    }
    fn frame_input(&mut self, bytes: &[u8]) -> Result<(), ActorError> {
        for byte in bytes {
            self.line_buffer.push(*byte);
            if *byte == b'\n' {
                if self.line_buffer.len() > self.card_line_limit() {
                    return Err(ActorError::new(
                        "serial_input_overflow",
                        "serial input line exceeded its byte limit",
                    ));
                }
                let bytes = std::mem::take(&mut self.line_buffer);
                let mut frame = SerialFrame::new(bytes);
                self.observe_radio_call(&String::from_utf8_lossy(&frame.data));
                let capture = self
                    .call_capture
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(capture) = capture {
                    frame.call_event_owned = capture(
                        crate::module_radio_transport::Binding::Serial {
                            serial_session: self.serial_session,
                        },
                        &String::from_utf8_lossy(&frame.data),
                    );
                }
                self.observe_sim_event(frame.sim_event.as_ref());
                frame.sms_generation = self.sms_generation;
                self.observe_native_sms(frame.sms_event.as_ref());
                frame.sms_ingest_owned = self.sms_receive.observe(
                    self.serial_session,
                    self.sms_generation,
                    frame.sms_event.as_ref(),
                )?;
                if !self.native.managed() || self.pending_command.is_some() {
                    self.queued_bytes = self.queued_bytes.saturating_add(frame.data.len());
                    self.frames.push_back(frame);
                }
            } else if self.line_buffer.len() > self.card_line_limit() {
                return Err(ActorError::new(
                    "serial_input_overflow",
                    "serial input line exceeded its byte limit",
                ));
            }
            if self.queued_bytes.saturating_add(self.line_buffer.len()) > MAX_BUFFERED_READ_BYTES {
                return Err(ActorError::new(
                    "serial_input_overflow",
                    "serial input exceeded its bounded line buffer",
                ));
            }
        }

        // The modem prompt is not necessarily terminated by a newline. Match
        // the legacy reader's suffix rule and normalize it to the same bytes.
        if self.line_buffer.ends_with(b"> ") || self.line_buffer.ends_with(b">") {
            let mut frame = SerialFrame::new(b"> ".to_vec());
            frame.sms_generation = self.sms_generation;
            if !self.native.managed() || self.pending_command.is_some() {
                self.frames.push_back(frame);
                self.queued_bytes = self.queued_bytes.saturating_add(2);
            }
            self.line_buffer.clear();
        }
        if self.queued_bytes.saturating_add(self.line_buffer.len()) > MAX_BUFFERED_READ_BYTES {
            return Err(ActorError::new(
                "serial_input_overflow",
                "serial input exceeded its bounded line buffer",
            ));
        }
        Ok(())
    }

    fn observe_sim_event(&mut self, event: Option<&SimEvent>) {
        // Recovery journal commits hold this publication lock. Change the
        // actor-local generation and its shared binding as one indivisible
        // step, so no stale generation can pass a commit-time card fence.
        let card_shared = self.card_shared.clone();
        let mut published = card_shared
            .binding
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.serial_sms.observe_sim(event, self.sms_generation);
        match event {
            Some(SimEvent::Ready | SimEvent::Status { .. }) => {
                self.sms_generation = self.sms_generation.wrapping_add(1);
            }
            Some(SimEvent::PinState { state }) => {
                self.sms_generation = self.sms_generation.wrapping_add(1);
                if state == "READY" {
                    // Switch preserves both the ready/reboot event and the
                    // SIM-status event for +CPIN: READY.
                    self.sms_generation = self.sms_generation.wrapping_add(1);
                }
            }
            None => {}
        }
        *published = self.local_card_binding();
        drop(published);
        self.sms_receive.generation_changed(self.sms_generation);
        self.observe_native_sim(event);
        self.publish_card_binding();
        self.publish_native_facts();
    }

    fn take_frame(&mut self, maximum: usize) -> Option<SerialFrame> {
        let from_command_events = !self.command_events.is_empty();
        let mut frame = if from_command_events {
            self.command_events.pop_front()?
        } else if self.pending_command.is_none() {
            self.frames.pop_front()?
        } else {
            return None;
        };
        let count = maximum.min(frame.data.len());
        let data = frame.data.drain(..count).collect();
        self.queued_bytes = self.queued_bytes.saturating_sub(count);
        let is_urc = frame.is_urc;
        let pure_async_urc = frame.pure_async_urc;
        let sms_generation = frame.sms_generation;
        let sim_event = frame.sim_event.clone();
        let sim_actions = frame.sim_actions.clone();
        let sms_event = frame.sms_event.clone();
        let sms_ingest_owned = frame.sms_ingest_owned;
        let call_event_owned = frame.call_event_owned;
        let ussd_event = frame.ussd_event.clone();
        if !frame.data.is_empty() {
            if from_command_events {
                self.command_events.push_front(frame);
            } else {
                self.frames.push_front(frame);
            }
        }
        Some(SerialFrame {
            data,
            is_urc,
            pure_async_urc,
            sms_generation,
            sim_event,
            sim_actions,
            sms_event,
            sms_ingest_owned,
            call_event_owned,
            ussd_event,
        })
    }

    fn process(&mut self, command: Command, opener: &PortOpener) {
        self.process_inner(command, opener);
        self.publish_card_binding();
        self.observe_native_policy();
        self.publish_native_facts();
    }

    fn process_inner(&mut self, command: Command, opener: &PortOpener) {
        let command = match command {
            Command::Finalize { reply } => {
                self.close_session();
                self.finalize_reply = Some(reply);
                return;
            }
            command => command,
        };
        let Some(command) = self.admit_module_boundary(command) else {
            return;
        };
        let Some(command) = self.admit_card_boundary(command) else {
            return;
        };
        match command {
            Command::ScopedRadio { .. } => unreachable!("scoped envelope is consumed at admission"),
            Command::Finalize { .. } => unreachable!("terminal command consumed before admission"),
            Command::LegacyGate(gate) => gate(self, opener),
            Command::Native(request) => self.process_native_request(request, opener),
            Command::Card(request) => self.process_card_request(request),
            Command::SerialSmsObservation { reply } => {
                let _ = reply.send(Ok(self.serial_sms_observation()));
            }
            Command::SerialSmsBindingCurrent { binding, reply } => {
                let _ = reply.send(Ok(self.serial_sms_binding_current(&binding)));
            }
            Command::CacheSerialSmsPhone {
                binding,
                phone,
                reply,
            } => {
                if self.serial_sms_binding_current(&binding)
                    && phone.len() <= 128
                    && !phone.chars().any(char::is_control)
                {
                    self.serial_sms.phone = phone;
                }
                let _ = reply.send(Ok(()));
            }
            Command::FencedClose {
                serial_session,
                reply,
            } => {
                if serial_session == 0 {
                    let _ = reply.send(Err(ActorError::invalid("serial close session missing")));
                } else {
                    if self.serial_session == serial_session {
                        self.close_session();
                    }
                    let _ = reply.send(Ok(()));
                }
            }
            Command::CallModeRadioInitialize {
                serial_session,
                deadline,
                cancelled,
                reply,
            } => {
                if self.serial_session != serial_session || self.port.is_none() {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_session_changed",
                        "native rebind initialization generation changed",
                    )));
                } else {
                    self.initialization.required = true;
                    self.initialization.call_mode_fence = Some((deadline, cancelled));
                    self.begin_initialization(reply);
                }
            }
            Command::CallModeRadioReconnect {
                serial_session,
                expected_imei,
                deadline,
                cancelled,
                reply,
            } => {
                let result = self.call_mode_radio_reconnect(
                    serial_session,
                    &expected_imei,
                    deadline,
                    &cancelled,
                    opener,
                );
                let _ = reply.send(result);
            }
            Command::DiscoverATPort { candidates, reply } => {
                if self.port.is_some()
                    || self.pending_command.is_some()
                    || !self.pending_high_commands.is_empty()
                    || !self.pending_normal_commands.is_empty()
                    || self.sms_receive.is_active()
                    || self.initialization.running
                {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_resource_busy",
                        "resource busy: AT port discovery requires an idle modem actor",
                    )));
                    return;
                }
                let candidates = match candidates {
                    Some(candidates) => candidates,
                    None => match enumerate_at_port_candidates() {
                        Ok(candidates) => candidates,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            return;
                        }
                    },
                };
                if candidates.is_empty() {
                    let _ = reply.send(Err(ActorError::new(
                        "at_port_not_found",
                        "no Quectel/DJI USB serial ports found; pass -port /dev/cu.* explicitly",
                    )));
                    return;
                }
                let attempted = candidates.join(", ");
                for path in candidates {
                    let Ok(mut port) = opener(path.clone(), AT_DISCOVERY_BAUD_RATE) else {
                        continue;
                    };
                    let found = probe_serial_port_imei(port.as_mut()).is_some();
                    // The reply is physical completion evidence for native
                    // startup. Do not reopen this path or select USB while the
                    // probe handle is still closing (including its destructor).
                    drop(port);
                    if found {
                        let _ = reply.send(Ok(DiscoveredATPort { port: path }));
                        return;
                    }
                }
                let _ = reply.send(Err(ActorError::new(
                    "at_port_not_found",
                    format!("no AT-capable port found among {attempted}"),
                )));
            }
            Command::Open {
                path,
                baud_rate,
                initialization_required,
                reply,
            } => {
                if self.port.is_some() {
                    if self.current_path.as_deref() == Some(path.as_str())
                        && self.current_baud == baud_rate
                        && self.initialization.required == initialization_required
                    {
                        let _ = reply.send(Ok(self.serial_session));
                    } else {
                        let _ = reply.send(Err(ActorError::new(
                            "serial_resource_busy",
                            "resource busy: another serial port is owned",
                        )));
                    }
                    return;
                }
                if self.sms_receive.is_active() {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_resource_busy",
                        "previous SMS receive session is still finishing",
                    )));
                    return;
                }
                if self.initialization.running {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_resource_busy",
                        "resource busy: previous serial initialization is still finishing",
                    )));
                    return;
                }
                let Some(next_session) = self.serial_session.checked_add(1) else {
                    let _ = reply.send(Err(ActorError::unavailable(
                        "serial session identity exhausted",
                    )));
                    return;
                };
                match opener(path.clone(), baud_rate) {
                    Ok(opened) => {
                        self.serial_session = next_session;
                        self.radio_call_active = false;
                        self.initialization = InitializationState {
                            required: initialization_required,
                            ..InitializationState::default()
                        };
                        self.sms_receive.invalidate();
                        self.sms_receive.configured = false;
                        self.port = Some(opened);
                        self.live_serial_session
                            .store(self.serial_session, std::sync::atomic::Ordering::Release);
                        self.last_radio_port = Some((path.clone(), baud_rate));
                        self.current_path = Some(path);
                        self.current_baud = baud_rate;
                        self.frames.clear();
                        self.queued_bytes = 0;
                        self.line_buffer.clear();
                        self.disconnected_error = None;
                        // A new Go Manager starts its observed SMS generation
                        // at zero. Keep that process-local fence aligned when
                        // it reacquires the Rust-owned serial handle.
                        self.sms_generation = 0;
                        self.serial_sms.reset_sim();
                        self.native_session_opened();
                        // A newly opened serial session corresponds to a new
                        // Go compatibility manager, whose legacy watchdog
                        // streak starts at zero.
                        self.timeout_streak = 0;
                        // If the HTTP caller timed out while the device was
                        // opening, do not leave a port owned by a caller that
                        // believes the open failed.
                        if reply.send(Ok(self.serial_session)).is_err() {
                            self.retire_physical_port("serial_disconnected");
                            self.live_serial_session
                                .store(0, std::sync::atomic::Ordering::Release);
                            self.current_path = None;
                            self.current_baud = 0;
                            self.frames.clear();
                            self.queued_bytes = 0;
                            self.line_buffer.clear();
                        }
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Read {
                maximum,
                timeout,
                reply,
            } => {
                if !self.command_events.is_empty()
                    || (self.pending_command.is_none() && !self.frames.is_empty())
                {
                    let frame = self
                        .take_frame(maximum)
                        .expect("a queued serial frame was checked");
                    let _ = reply.send(Ok(frame));
                } else if let Some(error) = self.disconnected_error.take() {
                    let _ = reply.send(Err(error));
                } else if self.port.is_none() {
                    let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
                } else if self.pending_read.is_some() {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_read_busy",
                        "a serial read is already pending",
                    )));
                } else {
                    self.pending_read = Some(PendingRead {
                        maximum,
                        deadline: Instant::now() + timeout,
                        reply,
                    });
                }
            }
            Command::RadioBinding { reply } => {
                let result = if self.port.is_some() {
                    Ok(self.serial_session)
                } else {
                    Err(ActorError::unavailable("serial port is not open"))
                };
                let _ = reply.send(result);
            }
            Command::Radio {
                module_token: _,
                serial_session,
                action,
                deadline,
                cancelled,
                commit_proof,
                reply,
            } => self.begin_radio_with_deadline(
                serial_session,
                action,
                deadline,
                cancelled,
                reply,
                commit_proof,
            ),
            Command::Write { data, reply } => {
                if String::from_utf8_lossy(&data)
                    .split(['\r', '\n'])
                    .any(|line| !self.cellular_command_allowed(line))
                {
                    let _ = reply.send(Err(ActorError::new(
                        "module_flight_mode_blocks_cellular_probe",
                        "Cellular telemetry is suppressed by native radio policy",
                    )));
                    return;
                }
                if String::from_utf8_lossy(&data)
                    .split(['\r', '\n'])
                    .any(|command| {
                        crate::module_radio_transport::is_radio_write(command)
                            || (self
                                .call_capture
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .is_some()
                                && crate::module_radio_transport::is_cellular_call_write(command))
                    })
                {
                    let _ = reply.send(Err(ActorError::new(
                        "radio_authorization_required",
                        "Radio writes require native policy authorization",
                    )));
                    return;
                }
                if self.pending_command.is_some()
                    || !self.pending_high_commands.is_empty()
                    || !self.pending_normal_commands.is_empty()
                    || self.deferred_sms_submit.is_some()
                    || self.deferred_radio.is_some()
                {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_command_busy",
                        "an AT command transaction is in progress",
                    )));
                    return;
                }
                let Some(port) = self.port.as_mut() else {
                    let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
                    return;
                };
                // This compatibility entry point accepts complete AT lines,
                // never fragments that could concatenate across RPCs into an
                // unauthorized CFUN write. ESC is the sole raw control byte.
                if data != [0x1b]
                    && (std::str::from_utf8(&data).is_err()
                        || !data.ends_with(b"\r") && !data.ends_with(b"\n")
                        || std::str::from_utf8(&data).is_ok_and(|text| {
                            text.lines()
                                .filter(|line| !line.trim().is_empty())
                                .any(|line| {
                                    !line
                                        .trim()
                                        .get(..2)
                                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AT"))
                                })
                        }))
                {
                    let _ = reply.send(Err(ActorError::invalid(
                        "raw serial writes require complete AT lines",
                    )));
                    return;
                }
                match port.write_all(&data) {
                    Ok(()) => {
                        let _ = reply.send(Ok(data.len()));
                    }
                    Err(_) => {
                        self.retire_physical_port("serial_disconnected");
                        self.live_serial_session
                            .store(0, std::sync::atomic::Ordering::Release);
                        self.current_path = None;
                        self.current_baud = 0;
                        self.line_buffer.clear();
                        self.disconnected_error = Some(ActorError::new(
                            "serial_disconnected",
                            "device disconnected: serial write failed",
                        ));
                        let _ = reply.send(Err(ActorError::new(
                            "serial_disconnected",
                            "device disconnected: serial write failed",
                        )));
                    }
                }
            }
            Command::Execute {
                command,
                timeout,
                high_priority,
                follow_up,
                expected_sms_generation,
                reply,
            } => {
                if !self.cellular_command_allowed(&command) {
                    let _ = reply.send(Err(ActorError::new(
                        "module_flight_mode_blocks_cellular_probe",
                        "Cellular telemetry is suppressed by native radio policy",
                    )));
                    return;
                }
                if crate::module_radio_transport::is_radio_write(&command)
                    || (self
                        .call_capture
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some()
                        && crate::module_radio_transport::is_cellular_call_write(&command))
                {
                    let _ = reply.send(Err(ActorError::new(
                        "radio_authorization_required",
                        "Radio writes require native policy authorization",
                    )));
                    return;
                }
                if self.port.is_none() {
                    let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
                    return;
                }
                let mut bytes = command.into_bytes();
                bytes.extend_from_slice(b"\r\n");
                let queue = if high_priority {
                    &mut self.pending_high_commands
                } else {
                    &mut self.pending_normal_commands
                };
                let capacity = if high_priority {
                    HIGH_COMMAND_QUEUE_CAPACITY
                } else {
                    NORMAL_COMMAND_QUEUE_CAPACITY
                };
                if queue.len() >= capacity {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_command_queue_full",
                        "serial command priority queue is full",
                    )));
                    return;
                }
                queue.push_back(QueuedCommand {
                    bytes,
                    timeout,
                    follow_up,
                    expected_sms_generation,
                    high_priority,
                    completion: CommandCompletion::Execute(reply),
                });
            }
            Command::SubmitSms {
                tpdus,
                binding,
                expected_sms_generation,
                reply,
            } => {
                if self.port.is_none() {
                    let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
                    return;
                }
                if binding
                    .as_ref()
                    .is_some_and(|binding| !self.serial_sms_binding_current(binding))
                {
                    let _ = reply.send(Err(serial_sms::binding_changed()));
                    return;
                }
                let expected_sms_generation =
                    expected_sms_generation.unwrap_or(self.sms_generation);
                if expected_sms_generation != self.sms_generation {
                    let _ = reply.send(Err(ActorError::new(
                        "sms_generation_changed",
                        "SIM generation changed before SMS submission",
                    )));
                    return;
                }
                if self.pending_high_commands.len() >= HIGH_COMMAND_QUEUE_CAPACITY {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_command_queue_full",
                        "serial command priority queue is full",
                    )));
                    return;
                }
                self.pending_high_commands.push_back(QueuedCommand {
                    bytes: b"AT+CMGF=0\r\n".to_vec(),
                    timeout: SMS_PDU_MODE_TIMEOUT,
                    follow_up: None,
                    expected_sms_generation: Some(expected_sms_generation),
                    high_priority: true,
                    completion: CommandCompletion::SubmitSms(SmsSubmitTransaction {
                        binding,
                        tpdus,
                        next_segment: 0,
                        accepted_segments: 0,
                        expected_sms_generation,
                        step: SmsSubmitStep::PduMode,
                        reply,
                    }),
                });
            }
            Command::DeleteArchivedSms {
                index,
                pdu,
                expected_sms_generation,
                receive_fence,
                reply,
            } => {
                if let Err(error) = self.check_receive_fence(receive_fence) {
                    let _ = reply.send(Err(error));
                    return;
                }
                if self.port.is_none() {
                    let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
                    return;
                }
                if expected_sms_generation != self.sms_generation {
                    let _ = reply.send(Ok(StoredSmsDeleteResult {
                        deleted: false,
                        reason: "generation_changed",
                    }));
                    return;
                }
                let bytes = b"AT+CPMS?\r\n".to_vec();
                let queue = &mut self.pending_normal_commands;
                if queue.len() >= NORMAL_COMMAND_QUEUE_CAPACITY {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_command_queue_full",
                        "serial command priority queue is full",
                    )));
                    return;
                }
                queue.push_back(QueuedCommand {
                    bytes,
                    timeout: SMS_DELETE_STEP_TIMEOUT,
                    follow_up: None,
                    expected_sms_generation: None,
                    high_priority: false,
                    completion: CommandCompletion::DeleteArchivedSms(StoredSmsDeleteTransaction {
                        index,
                        pdu,
                        expected_sms_generation,
                        receive_fence,
                        step: StoredSmsDeleteStep::VerifyStorage,
                        reply,
                    }),
                });
            }
            Command::ReadStoredSms {
                storage,
                index,
                expected_sms_generation,
                receive_fence,
                reply,
            } => {
                if let Err(error) = self.check_receive_fence(receive_fence) {
                    let _ = reply.send(Err(error));
                    return;
                }
                if self.port.is_none() {
                    let _ = reply.send(Err(ActorError::unavailable("serial port is not open")));
                    return;
                }
                if expected_sms_generation != self.sms_generation {
                    let _ = reply.send(Err(ActorError::new(
                        "sms_generation_changed",
                        "SIM generation changed before stored SMS read",
                    )));
                    return;
                }
                let queue = &mut self.pending_normal_commands;
                if queue.len() >= NORMAL_COMMAND_QUEUE_CAPACITY {
                    let _ = reply.send(Err(ActorError::new(
                        "serial_command_queue_full",
                        "serial command priority queue is full",
                    )));
                    return;
                }
                queue.push_back(QueuedCommand {
                    bytes: b"AT+CPMS?\r\n".to_vec(),
                    timeout: Duration::from_secs(3),
                    follow_up: None,
                    expected_sms_generation: None,
                    high_priority: false,
                    completion: CommandCompletion::ReadStoredSms(StoredSmsReadTransaction {
                        requested_storage: storage,
                        index,
                        expected_sms_generation,
                        receive_fence,
                        previous_storages: Vec::new(),
                        selected_storage: String::new(),
                        step: StoredSmsReadStep::QueryStorage,
                        response_lines: Vec::new(),
                        primary_error: None,
                        reply,
                    }),
                });
            }
            Command::ConfirmSmsGeneration {
                expected_sms_generation,
                receive_fence,
                reply,
            } => {
                if let Err(error) = self.check_receive_fence(receive_fence) {
                    let _ = reply.send(Err(error));
                    return;
                }
                let result = if self.port.is_none() {
                    Err(ActorError::unavailable("serial port is not open"))
                } else if expected_sms_generation != self.sms_generation {
                    Err(ActorError::new(
                        "sms_generation_changed",
                        "SIM generation changed before stored SMS archive",
                    ))
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Command::ReadSmsGeneration { reply } => {
                let result = if self.port.is_some() {
                    Ok(self.sms_generation)
                } else {
                    Err(ActorError::unavailable("serial port is not open"))
                };
                let _ = reply.send(result);
            }
            Command::Close { reply } => {
                self.close_session();
                let _ = reply.send(Ok(()));
            }
            Command::ConfigureSmsReceive { enabled, reply } => {
                let result = self.configure_receive(enabled);
                if result.is_ok() {
                    self.last_radio_receive_enabled = enabled;
                }
                let _ = reply.send(result);
            }
            Command::ScanSmsReceive { reply } => {
                let result = self.scan_receive();
                let _ = reply.send(result);
            }
            Command::NextSmsReceive { reply } => {
                let _ = reply.send(Ok(self.next_receive()));
            }
            Command::FinishSmsReceive { id, reply } => {
                self.finish_receive(id);
                let _ = reply.send(Ok(()));
            }
            Command::Initialize { reply } => self.begin_initialization(reply),
        }
    }

    fn close_session(&mut self) {
        self.serial_sms.reset_sim();
        if let Some(transaction) = self.deferred_radio.take() {
            fail_radio(transaction, "serial_not_open");
        }
        self.radio_call_active = false;
        self.cancel_initialization(ActorError::unavailable("serial port was closed"));
        self.sms_receive.invalidate();
        if let Some(mut pending) = self.pending_command.take() {
            scrub_sensitive_pending_response(&mut pending);
            self.fail_completion(
                pending.completion,
                ActorError::unavailable("serial port was closed during an AT command"),
            );
        }
        if let Some(deferred) = self.deferred_sms_submit.take() {
            let _ = deferred.transaction.reply.send(Err(ActorError::unavailable(
                "serial port was closed between SMS segments",
            )));
        }
        self.fail_queued_commands("serial port was closed");
        self.retire_physical_port("serial_closed");
        self.live_serial_session
            .store(0, std::sync::atomic::Ordering::Release);
        self.current_path = None;
        self.current_baud = 0;
        self.frames.clear();
        self.command_events.clear();
        self.queued_bytes = 0;
        self.line_buffer.clear();
        self.disconnected_error = None;
        self.timeout_streak = 0;
        if let Some(pending) = self.pending_read.take() {
            let _ = pending
                .reply
                .send(Err(ActorError::unavailable("serial port was closed")));
        }
    }

    fn fail_completion(&mut self, completion: CommandCompletion, error: ActorError) {
        if let CommandCompletion::Card(completion) = completion {
            self.fail_card_completion(completion);
            return;
        }
        if matches!(&completion, CommandCompletion::Initialize(transaction)
            if transaction.serial_session == self.serial_session)
        {
            self.note_initialization_failure(error.clone());
        }
        send_completion_error(completion, error);
    }

    fn fail_queued_commands(&mut self, reason: &str) {
        let error = || ActorError::unavailable(reason.to_owned());
        let pending: Vec<_> = self
            .pending_high_commands
            .drain(..)
            .chain(self.pending_normal_commands.drain(..))
            .collect();
        for pending in pending {
            self.fail_completion(pending.completion, error());
        }
    }

    fn start_next_command(&mut self) {
        self.poll_initialization();
        if self.pending_command.is_some() {
            return;
        }
        if self.poll_radio() {
            return;
        }
        if let Some(deferred) = self.deferred_sms_submit.take() {
            if Instant::now() < deferred.ready_at {
                self.deferred_sms_submit = Some(deferred);
                return;
            }
            if !self.sms_submit_current(&deferred.transaction) {
                let _ = deferred.transaction.reply.send(Ok(SmsSubmitResult {
                    accepted_segments: deferred.transaction.accepted_segments,
                    complete: false,
                }));
                return;
            }
            self.issue_sms_submit_segment(deferred.transaction);
            return;
        }
        let next = self
            .pending_high_commands
            .pop_front()
            .or_else(|| self.pending_normal_commands.pop_front());
        let Some(next) = next else {
            return;
        };
        if matches!(&next.completion, CommandCompletion::Execute(_))
            && self
                .card_shared
                .managed
                .load(std::sync::atomic::Ordering::Acquire)
            && legacy_card_command(&String::from_utf8_lossy(&next.bytes))
        {
            self.fail_completion(
                next.completion,
                ActorError::new(
                    "serial_esim_native_owner",
                    "Serial card commands require native ownership",
                ),
            );
            return;
        }
        if matches!(&next.completion, CommandCompletion::Execute(_))
            && !self.cellular_command_allowed(&String::from_utf8_lossy(&next.bytes))
        {
            self.fail_completion(
                next.completion,
                ActorError::new(
                    "module_flight_mode_blocks_cellular_probe",
                    "Cellular telemetry is suppressed by native radio policy",
                ),
            );
            return;
        }
        if matches!(&next.completion, CommandCompletion::Execute(_))
            && self
                .call_capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            && crate::module_radio_transport::is_cellular_call_write(&String::from_utf8_lossy(
                &next.bytes,
            ))
        {
            self.fail_completion(
                next.completion,
                ActorError::new(
                    "radio_authorization_required",
                    "Cellular call writes require native call ownership",
                ),
            );
            return;
        }
        let fence = match &next.completion {
            CommandCompletion::ReadStoredSms(transaction) => Some(transaction.receive_fence),
            CommandCompletion::DeleteArchivedSms(transaction) => Some(transaction.receive_fence),
            _ => None,
        };
        if let Some(fence) = fence
            && let Err(error) = self.check_receive_fence(fence)
        {
            self.fail_completion(next.completion, error);
            return;
        }
        if matches!(&next.completion, CommandCompletion::Initialize(transaction)
            if transaction.serial_session != self.serial_session)
        {
            self.fail_completion(
                next.completion,
                ActorError::new(
                    "serial_session_changed",
                    "initialization belongs to a retired serial session",
                ),
            );
            return;
        }
        if matches!(
            &next.completion,
            CommandCompletion::SubmitSms(transaction)
                if !self.sms_submit_current(transaction)
        ) {
            self.fail_completion(
                next.completion,
                ActorError::new(
                    "sms_generation_changed",
                    "SIM generation changed before SMS submission",
                ),
            );
            return;
        }
        if let CommandCompletion::NativeQuery(completion) = &next.completion {
            if !self.native_query_current(completion) {
                self.fail_completion(next.completion, native_stale());
                return;
            }
        }
        if let CommandCompletion::Card(completion) = &next.completion {
            if !self.card_completion_current(completion) {
                self.fail_completion(next.completion, card_stale());
                return;
            }
        }
        let Some(port) = self.port.as_mut() else {
            self.fail_completion(
                next.completion,
                ActorError::unavailable("serial port is not open"),
            );
            return;
        };
        if port.write_all(&next.bytes).is_err() {
            self.retire_physical_port("serial_disconnected");
            self.live_serial_session
                .store(0, std::sync::atomic::Ordering::Release);
            self.current_path = None;
            self.current_baud = 0;
            self.line_buffer.clear();
            let error = ActorError::new(
                "serial_disconnected",
                "device disconnected: serial command write failed",
            );
            self.disconnected_error = Some(error.clone());
            self.fail_completion(next.completion, error);
            self.fail_queued_commands("serial command failed because the device disconnected");
            return;
        }
        // Bytes received before the command belong to the idle event stream.
        self.release_idle_frames();
        self.pending_command = Some(PendingCommand {
            deadline: Instant::now() + next.timeout,
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow: false,
            follow_up: next.follow_up,
            expected_sms_generation: next.expected_sms_generation,
            high_priority: next.high_priority,
            completion: next.completion,
        });
    }

    fn complete_pending_read(&mut self) {
        let Some(pending) = self.pending_read.as_ref() else {
            return;
        };
        if !self.command_events.is_empty()
            || (self.pending_command.is_none() && !self.frames.is_empty())
        {
            let pending = self.pending_read.take().expect("pending read was checked");
            let frame = self
                .take_frame(pending.maximum)
                .expect("a queued serial frame was checked");
            let _ = pending.reply.send(Ok(frame));
        } else if let Some(error) = self.disconnected_error.take() {
            let pending = self.pending_read.take().expect("pending read was checked");
            let _ = pending.reply.send(Err(error));
        } else if Instant::now() >= pending.deadline {
            let pending = self.pending_read.take().expect("pending read was checked");
            let _ = pending.reply.send(Ok(SerialFrame::new(Vec::new())));
        }
    }

    fn complete_pending_command(&mut self) {
        if self.quarantine_invalid_sms_prompt() {
            return;
        }
        if self.pending_command.is_some() {
            if let Some(error) = self.disconnected_error.take() {
                let mut pending = self
                    .pending_command
                    .take()
                    .expect("pending command was checked");
                scrub_sensitive_pending_response(&mut pending);
                self.fail_completion(pending.completion, error);
                return;
            }
        } else {
            return;
        }
        loop {
            let Some(frame) = self.frames.pop_front() else {
                break;
            };
            self.queued_bytes = self.queued_bytes.saturating_sub(frame.data.len());
            let line = String::from_utf8_lossy(&frame.data).trim().to_owned();
            if line.is_empty() {
                continue;
            }

            if frame.is_urc && !self.native.managed() {
                self.queued_bytes = self.queued_bytes.saturating_add(frame.data.len());
                self.command_events.push_back(SerialFrame {
                    data: frame.data.clone(),
                    is_urc: frame.is_urc,
                    pure_async_urc: frame.pure_async_urc,
                    sms_generation: frame.sms_generation,
                    sim_event: frame.sim_event.clone(),
                    sim_actions: frame.sim_actions.clone(),
                    sms_event: frame.sms_event.clone(),
                    sms_ingest_owned: frame.sms_ingest_owned,
                    call_event_owned: frame.call_event_owned,
                    ussd_event: frame.ussd_event.clone(),
                });
            }

            if line.eq_ignore_ascii_case("OK") {
                self.finish_command("ok", None);
                break;
            }
            if line.to_ascii_uppercase().contains("ERROR") {
                self.finish_command("error", Some(line));
                break;
            }
            if line.contains('>') {
                let should_send_follow_up = self
                    .pending_command
                    .as_ref()
                    .is_some_and(|pending| pending.follow_up.is_some());
                if should_send_follow_up {
                    let generation_matches = self.pending_command.as_ref().is_some_and(|pending| {
                        pending
                            .expected_sms_generation
                            .is_none_or(|expected| expected == self.sms_generation)
                    });
                    let binding_matches = self.pending_command.as_ref().is_some_and(|pending| {
                        match &pending.completion {
                            CommandCompletion::SubmitSms(transaction) => {
                                self.sms_submit_current(transaction)
                            }
                            _ => true,
                        }
                    });
                    if !generation_matches || !binding_matches {
                        self.finish_command("generation_changed", None);
                        break;
                    }
                    let follow_up = self
                        .pending_command
                        .as_mut()
                        .and_then(|pending| pending.follow_up.take())
                        .expect("interactive follow-up was checked");
                    let write_result = self.port.as_mut().map(|port| port.write_all(&follow_up));
                    if !matches!(write_result, Some(Ok(()))) {
                        self.retire_physical_port("serial_disconnected");
                        self.live_serial_session
                            .store(0, std::sync::atomic::Ordering::Release);
                        self.current_path = None;
                        self.current_baud = 0;
                        let error = ActorError::new(
                            "serial_disconnected",
                            "device disconnected: interactive SMS payload write failed",
                        );
                        self.disconnected_error = Some(error.clone());
                        if let Some(mut pending) = self.pending_command.take() {
                            scrub_sensitive_pending_response(&mut pending);
                            self.fail_completion(pending.completion, error);
                        }
                        self.fail_queued_commands(
                            "serial command failed because the device disconnected",
                        );
                        break;
                    }
                    continue;
                }
                self.finish_command("prompt", None);
                break;
            }
            if frame.is_urc
                && frame.pure_async_urc
                && !self.native_probe_response(frame.sim_event.as_ref())
            {
                continue;
            }

            let Some(pending) = self.pending_command.as_mut() else {
                break;
            };
            let line_bytes = line.len().saturating_add(1);
            if pending.lines.len() >= MAX_COMMAND_REPLY_LINES
                || pending.response_bytes.saturating_add(line_bytes) > MAX_COMMAND_REPLY_BYTES
            {
                pending.overflow = true;
                continue;
            }
            pending.response_bytes += line_bytes;
            pending.lines.push(line);
        }

        if self
            .pending_command
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
        {
            if let Some(port) = self.port.as_mut() {
                let _ = port.write_all(&[0x1b]);
                let _ = port.clear(serialport::ClearBuffer::Input);
            }
            self.frames.clear();
            self.line_buffer.clear();
            self.queued_bytes = self
                .command_events
                .iter()
                .map(|frame| frame.data.len())
                .sum();
            self.finish_command("timeout", None);
        }
    }

    fn finish_command(&mut self, result: &'static str, terminal: Option<String>) {
        let Some(pending) = self.pending_command.take() else {
            return;
        };
        let lines = if pending.overflow {
            Vec::new()
        } else {
            pending.lines
        };
        let result = if pending.overflow { "overflow" } else { result };
        match result {
            "timeout" if !pending.high_priority => {
                self.timeout_streak = self.timeout_streak.saturating_add(1);
            }
            "ok" | "error" | "prompt" => self.timeout_streak = 0,
            // High-priority timeouts are excluded without clearing the
            // normal-command streak. Overflow and SIM-generation failures
            // also preserve it, matching the previous manager semantics.
            _ => {}
        }
        let timeout_streak = self.timeout_streak;
        let watchdog_tripped = result == "timeout"
            && !pending.high_priority
            && timeout_streak >= COMMAND_TIMEOUT_WATCHDOG_THRESHOLD;
        if watchdog_tripped && matches!(&pending.completion, CommandCompletion::Initialize(_)) {
            let error = ActorError::new(
                "serial_timeout_watchdog",
                "device disconnected: serial initialization timeout watchdog tripped",
            );
            self.fail_completion(pending.completion, error.clone());
            let pending_read = self.pending_read.take();
            self.retire_physical_port("serial_timeout_watchdog");
            self.close_session();
            // Preserve an explicit failure for the compatibility read loop;
            // it still owns the outer reconnect policy during migration.
            self.note_initialization_failure(error.clone());
            let delivered =
                pending_read.is_some_and(|pending| pending.reply.send(Err(error.clone())).is_ok());
            if !delivered {
                self.disconnected_error = Some(error);
            }
            return;
        }
        match pending.completion {
            CommandCompletion::NativeQuery(completion) => {
                self.finish_native_query(completion, result, lines, terminal)
            }
            CommandCompletion::Card(completion) => {
                self.finish_card_command(completion, result, lines, terminal)
            }
            CommandCompletion::Radio(transaction) => {
                self.advance_radio(transaction, result, lines, terminal)
            }
            CommandCompletion::Initialize(transaction) => {
                self.advance_initialization(transaction, result);
            }
            CommandCompletion::Execute(reply) => {
                let _ = reply.send(Ok(CommandResult {
                    lines,
                    result,
                    terminal,
                    elapsed_ms: pending.started.elapsed().as_millis(),
                    timeout_streak,
                    timeout_threshold: COMMAND_TIMEOUT_WATCHDOG_THRESHOLD,
                    watchdog_tripped,
                }));
            }
            CommandCompletion::SubmitSms(transaction) => {
                self.advance_sms_submit(transaction, result, &lines);
            }
            CommandCompletion::DeleteArchivedSms(transaction) => {
                self.advance_stored_sms_delete(transaction, result, &lines);
            }
            CommandCompletion::ReadStoredSms(transaction) => {
                self.advance_stored_sms_read(transaction, result, &lines);
            }
        }
    }

    fn advance_sms_submit(
        &mut self,
        mut transaction: SmsSubmitTransaction,
        result: &'static str,
        lines: &[String],
    ) {
        if matches!(transaction.step, SmsSubmitStep::PduMode)
            && !self.sms_submit_current(&transaction)
        {
            let _ = transaction.reply.send(Ok(SmsSubmitResult {
                accepted_segments: transaction.accepted_segments,
                complete: false,
            }));
            return;
        }
        match transaction.step {
            SmsSubmitStep::PduMode if result == "ok" => {
                transaction.step = SmsSubmitStep::Segment;
                self.issue_sms_submit_segment(transaction);
            }
            SmsSubmitStep::Segment if result == "ok" && has_cmgs_ack(lines) => {
                transaction.accepted_segments += 1;
                // The wire ACK is observed, but after an identity transition
                // it cannot confirm this strict intent as fully completed.
                // Preserve the compatibility path's historical result policy.
                if transaction.binding.is_some() && !self.sms_submit_current(&transaction) {
                    let _ = transaction.reply.send(Ok(SmsSubmitResult {
                        accepted_segments: transaction.accepted_segments,
                        complete: false,
                    }));
                } else if transaction.accepted_segments == transaction.tpdus.len() {
                    let _ = transaction.reply.send(Ok(SmsSubmitResult {
                        accepted_segments: transaction.accepted_segments,
                        complete: true,
                    }));
                } else if !self.sms_submit_current(&transaction) {
                    let _ = transaction.reply.send(Ok(SmsSubmitResult {
                        accepted_segments: transaction.accepted_segments,
                        complete: false,
                    }));
                } else {
                    transaction.next_segment += 1;
                    self.deferred_sms_submit = Some(DeferredSmsSubmit {
                        transaction,
                        ready_at: Instant::now() + SMS_SUBMIT_SEGMENT_GAP,
                    });
                }
            }
            _ => {
                let _ = transaction.reply.send(Ok(SmsSubmitResult {
                    accepted_segments: transaction.accepted_segments,
                    complete: false,
                }));
            }
        }
    }

    fn issue_sms_submit_segment(&mut self, transaction: SmsSubmitTransaction) {
        if !self.sms_submit_current(&transaction) {
            let _ = transaction.reply.send(Ok(SmsSubmitResult {
                accepted_segments: transaction.accepted_segments,
                complete: false,
            }));
            return;
        }
        let Some(tpdu) = transaction.tpdus.get(transaction.next_segment) else {
            let _ = transaction.reply.send(Ok(SmsSubmitResult {
                accepted_segments: transaction.accepted_segments,
                complete: false,
            }));
            return;
        };
        let mut payload = String::with_capacity(3 + tpdu.len() * 2);
        payload.push_str("00");
        use std::fmt::Write as _;
        for byte in tpdu {
            let _ = write!(payload, "{byte:02X}");
        }
        payload.push('\u{001a}');
        let follow_up = payload.into_bytes();

        let command = format!("AT+CMGS={}", tpdu.len());
        let mut bytes = command.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        let Some(port) = self.port.as_mut() else {
            let _ = transaction
                .reply
                .send(Err(ActorError::unavailable("serial port is not open")));
            return;
        };
        if port.write_all(&bytes).is_err() {
            self.retire_physical_port("serial_disconnected");
            self.live_serial_session
                .store(0, std::sync::atomic::Ordering::Release);
            self.current_path = None;
            self.current_baud = 0;
            self.line_buffer.clear();
            let error = ActorError::new(
                "serial_disconnected",
                "device disconnected: SMS submit command write failed",
            );
            self.disconnected_error = Some(error.clone());
            let _ = transaction.reply.send(Err(error));
            self.fail_queued_commands("serial command failed because the device disconnected");
            return;
        }
        self.release_idle_frames();
        self.pending_command = Some(PendingCommand {
            deadline: Instant::now() + SMS_SUBMIT_STEP_TIMEOUT,
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow: false,
            follow_up: Some(follow_up),
            expected_sms_generation: Some(transaction.expected_sms_generation),
            high_priority: true,
            completion: CommandCompletion::SubmitSms(transaction),
        });
    }

    fn advance_stored_sms_delete(
        &mut self,
        mut transaction: StoredSmsDeleteTransaction,
        result: &'static str,
        lines: &[String],
    ) {
        let retain = |transaction: StoredSmsDeleteTransaction, reason| {
            let _ = transaction.reply.send(Ok(StoredSmsDeleteResult {
                deleted: false,
                reason,
            }));
        };
        if self.check_receive_fence(transaction.receive_fence).is_err() {
            let reason = if matches!(transaction.step, StoredSmsDeleteStep::Delete) {
                "delete_outcome_uncertain"
            } else {
                "receive_session_changed"
            };
            retain(transaction, reason);
            return;
        }
        if transaction.expected_sms_generation != self.sms_generation {
            let reason = if matches!(transaction.step, StoredSmsDeleteStep::Delete) {
                "delete_outcome_uncertain"
            } else {
                "generation_changed"
            };
            retain(transaction, reason);
            return;
        }
        if result != "ok" {
            let reason = if matches!(transaction.step, StoredSmsDeleteStep::Delete) {
                "delete_outcome_uncertain"
            } else {
                "modem_command_failed"
            };
            retain(transaction, reason);
            return;
        }
        match transaction.step {
            StoredSmsDeleteStep::VerifyStorage => {
                if first_sms_storage(lines).as_deref() != Some("ME") {
                    retain(transaction, "storage_not_me");
                    return;
                }
                transaction.step = StoredSmsDeleteStep::VerifyReadback;
                let command = format!("AT+CMGR={}", transaction.index);
                self.issue_stored_sms_delete_step(transaction, command);
            }
            StoredSmsDeleteStep::VerifyReadback => {
                let response = lines.join("\r\n");
                let pdu_matches = crate::sms_codec::stored_read_pdu(&response)
                    .is_some_and(|current| current.eq_ignore_ascii_case(&transaction.pdu));
                if !pdu_matches {
                    retain(transaction, "message_changed");
                    return;
                }
                transaction.step = StoredSmsDeleteStep::Delete;
                let command = format!("AT+CMGD={}", transaction.index);
                self.issue_stored_sms_delete_step(transaction, command);
            }
            StoredSmsDeleteStep::Delete => {
                let _ = transaction.reply.send(Ok(StoredSmsDeleteResult {
                    deleted: true,
                    reason: "deleted",
                }));
            }
        }
    }

    fn advance_stored_sms_read(
        &mut self,
        mut transaction: StoredSmsReadTransaction,
        result: &'static str,
        lines: &[String],
    ) {
        if !matches!(
            transaction.step,
            StoredSmsReadStep::RestoreStorage | StoredSmsReadStep::ConfirmRestore
        ) && let Err(error) = self.check_receive_fence(transaction.receive_fence)
        {
            if !transaction.previous_storages.is_empty()
                && self.port.is_some()
                && transaction
                    .receive_fence
                    .is_none_or(|fence| fence.session == self.serial_session)
            {
                self.restore_stored_sms_read_after_error(transaction, error);
            } else {
                let _ = transaction.reply.send(Err(error));
            }
            return;
        }
        if transaction.expected_sms_generation != self.sms_generation {
            let error = ActorError::new(
                "sms_generation_changed",
                "SIM generation changed during stored SMS read",
            );
            if !matches!(
                transaction.step,
                StoredSmsReadStep::RestoreStorage | StoredSmsReadStep::ConfirmRestore
            ) && !transaction.previous_storages.is_empty()
                && transaction.selected_storage != transaction.previous_storages[0]
                && let Some(command) = set_sms_storage_command(&transaction.previous_storages)
            {
                transaction.primary_error = Some(error);
                transaction.step = StoredSmsReadStep::RestoreStorage;
                self.issue_stored_sms_read_step(transaction, command, Duration::from_secs(5));
                return;
            }
            let _ = transaction.reply.send(Err(ActorError::new(
                "sms_generation_changed",
                "SIM generation changed during stored SMS read",
            )));
            return;
        }

        match transaction.step {
            StoredSmsReadStep::QueryStorage => {
                if result != "ok" {
                    let _ = transaction.reply.send(Err(ActorError::new(
                        "sms_storage_query_failed",
                        "could not query the modem SMS storage selection",
                    )));
                    return;
                }
                let Some(previous_storages) = sms_storages(lines) else {
                    let _ = transaction.reply.send(Err(ActorError::new(
                        "sms_storage_query_invalid",
                        "modem returned an invalid SMS storage selection",
                    )));
                    return;
                };
                transaction.selected_storage = transaction
                    .requested_storage
                    .clone()
                    .unwrap_or_else(|| previous_storages[0].clone());
                transaction.previous_storages = previous_storages;
                if transaction.selected_storage != transaction.previous_storages[0] {
                    let Some(command) = set_sms_storage_command(&[
                        transaction.selected_storage.clone(),
                        transaction.selected_storage.clone(),
                        transaction.selected_storage.clone(),
                    ]) else {
                        let _ = transaction
                            .reply
                            .send(Err(ActorError::invalid("requested SMS storage is invalid")));
                        return;
                    };
                    transaction.step = StoredSmsReadStep::SelectStorage;
                    self.issue_stored_sms_read_step(transaction, command, Duration::from_secs(5));
                } else {
                    self.issue_stored_sms_read_command(transaction);
                }
            }
            StoredSmsReadStep::SelectStorage => {
                if result != "ok" {
                    self.restore_stored_sms_read_after_error(
                        transaction,
                        ActorError::new(
                            "sms_storage_select_failed",
                            "modem did not confirm the requested SMS storage",
                        ),
                    );
                    return;
                }
                transaction.step = StoredSmsReadStep::ConfirmStorage;
                self.issue_stored_sms_read_step(
                    transaction,
                    "AT+CPMS?".to_owned(),
                    Duration::from_secs(3),
                );
            }
            StoredSmsReadStep::ConfirmStorage => {
                if result != "ok"
                    || sms_storages(lines)
                        .and_then(|storages| storages.first().cloned())
                        .as_deref()
                        != Some(transaction.selected_storage.as_str())
                {
                    self.restore_stored_sms_read_after_error(
                        transaction,
                        ActorError::new(
                            "sms_storage_select_failed",
                            "modem did not select the requested SMS storage",
                        ),
                    );
                    return;
                }
                self.issue_stored_sms_read_command(transaction);
            }
            StoredSmsReadStep::Read => {
                if result == "ok" {
                    transaction.response_lines = lines.to_vec();
                } else {
                    transaction.primary_error = Some(ActorError::new(
                        "sms_storage_read_failed",
                        "modem could not read the requested SMS storage",
                    ));
                }
                if transaction.selected_storage != transaction.previous_storages[0] {
                    if let Some(command) = set_sms_storage_command(&transaction.previous_storages) {
                        transaction.step = StoredSmsReadStep::RestoreStorage;
                        self.issue_stored_sms_read_step(
                            transaction,
                            command,
                            Duration::from_secs(5),
                        );
                    } else if let Some(error) = transaction.primary_error.take() {
                        let _ = transaction.reply.send(Err(error));
                    } else {
                        self.finish_stored_sms_read(transaction, false);
                    }
                } else if let Some(error) = transaction.primary_error.take() {
                    let _ = transaction.reply.send(Err(error));
                } else {
                    self.finish_stored_sms_read(transaction, true);
                }
            }
            StoredSmsReadStep::RestoreStorage => {
                if result != "ok" {
                    if let Some(error) = transaction.primary_error.take() {
                        let _ = transaction.reply.send(Err(error));
                    } else {
                        self.finish_stored_sms_read(transaction, false);
                    }
                    return;
                }
                transaction.step = StoredSmsReadStep::ConfirmRestore;
                self.issue_stored_sms_read_step(
                    transaction,
                    "AT+CPMS?".to_owned(),
                    Duration::from_secs(3),
                );
            }
            StoredSmsReadStep::ConfirmRestore => {
                let restored = result == "ok"
                    && sms_storages(lines).as_deref()
                        == Some(transaction.previous_storages.as_slice());
                if let Some(error) = transaction.primary_error.take() {
                    let _ = transaction.reply.send(Err(error));
                } else {
                    self.finish_stored_sms_read(transaction, restored);
                }
            }
        }
    }

    fn restore_stored_sms_read_after_error(
        &mut self,
        mut transaction: StoredSmsReadTransaction,
        error: ActorError,
    ) {
        if transaction.selected_storage != transaction.previous_storages[0]
            && let Some(command) = set_sms_storage_command(&transaction.previous_storages)
        {
            transaction.primary_error = Some(error);
            transaction.step = StoredSmsReadStep::RestoreStorage;
            self.issue_stored_sms_read_step(transaction, command, Duration::from_secs(5));
        } else {
            let _ = transaction.reply.send(Err(error));
        }
    }

    fn issue_stored_sms_read_command(&mut self, mut transaction: StoredSmsReadTransaction) {
        let command = transaction.index.map_or_else(
            || "AT+CMGL=4".to_owned(),
            |index| format!("AT+CMGR={index}"),
        );
        let timeout = if transaction.index.is_some() {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(10)
        };
        transaction.step = StoredSmsReadStep::Read;
        self.issue_stored_sms_read_step(transaction, command, timeout);
    }

    fn issue_stored_sms_read_step(
        &mut self,
        transaction: StoredSmsReadTransaction,
        command: String,
        timeout: Duration,
    ) {
        let Some(port) = self.port.as_mut() else {
            let _ = transaction
                .reply
                .send(Err(ActorError::unavailable("serial port is not open")));
            return;
        };
        let mut bytes = command.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        if port.write_all(&bytes).is_err() {
            self.retire_physical_port("serial_disconnected");
            self.live_serial_session
                .store(0, std::sync::atomic::Ordering::Release);
            self.current_path = None;
            self.current_baud = 0;
            self.line_buffer.clear();
            let error = ActorError::new(
                "serial_disconnected",
                "device disconnected: stored SMS read transaction write failed",
            );
            self.disconnected_error = Some(error.clone());
            let _ = transaction.reply.send(Err(error.clone()));
            self.fail_queued_commands("serial command failed because the device disconnected");
            return;
        }
        self.release_idle_frames();
        self.pending_command = Some(PendingCommand {
            deadline: Instant::now() + timeout,
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow: false,
            follow_up: None,
            expected_sms_generation: None,
            high_priority: false,
            completion: CommandCompletion::ReadStoredSms(transaction),
        });
    }

    fn finish_stored_sms_read(
        &self,
        transaction: StoredSmsReadTransaction,
        storage_restored: bool,
    ) {
        let _ = transaction.reply.send(Ok(StoredSmsReadResult {
            storage: transaction.selected_storage,
            lines: transaction.response_lines,
            storage_restored,
        }));
    }

    fn issue_stored_sms_delete_step(
        &mut self,
        transaction: StoredSmsDeleteTransaction,
        command: String,
    ) {
        let mut bytes = command.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        let Some(port) = self.port.as_mut() else {
            let _ = transaction
                .reply
                .send(Err(ActorError::unavailable("serial port is not open")));
            return;
        };
        if port.write_all(&bytes).is_err() {
            self.retire_physical_port("serial_disconnected");
            self.live_serial_session
                .store(0, std::sync::atomic::Ordering::Release);
            self.current_path = None;
            self.current_baud = 0;
            self.line_buffer.clear();
            let error = ActorError::new(
                "serial_disconnected",
                "device disconnected: archived SMS cleanup command write failed",
            );
            self.disconnected_error = Some(error.clone());
            if matches!(transaction.step, StoredSmsDeleteStep::Delete) {
                let _ = transaction.reply.send(Ok(StoredSmsDeleteResult {
                    deleted: false,
                    reason: "delete_outcome_uncertain",
                }));
            } else {
                let _ = transaction.reply.send(Err(error));
            }
            self.fail_queued_commands("serial command failed because the device disconnected");
            return;
        }
        self.release_idle_frames();
        self.pending_command = Some(PendingCommand {
            deadline: Instant::now() + SMS_DELETE_STEP_TIMEOUT,
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow: false,
            follow_up: None,
            expected_sms_generation: None,
            high_priority: false,
            completion: CommandCompletion::DeleteArchivedSms(transaction),
        });
    }

    fn poll_input(&mut self, buffer: &mut [u8]) {
        if self.port.is_none() {
            return;
        }
        let buffered = self.queued_bytes.saturating_add(self.line_buffer.len());
        if buffered >= MAX_BUFFERED_READ_BYTES {
            self.retire_physical_port("serial_input_overflow");
            self.live_serial_session
                .store(0, std::sync::atomic::Ordering::Release);
            self.current_path = None;
            self.current_baud = 0;
            self.line_buffer.clear();
            self.disconnected_error = Some(ActorError::new(
                "serial_input_overflow",
                "serial input exceeded its bounded buffer; port closed",
            ));
            return;
        }
        let available = MAX_BUFFERED_READ_BYTES.saturating_sub(buffered);
        let read_limit = available.min(buffer.len());
        if read_limit == 0 {
            return;
        }
        let result = self
            .port
            .as_mut()
            .expect("open port was checked")
            .read(&mut buffer[..read_limit]);
        match result {
            Ok(count) if count > 0 => {
                if let Err(error) = self.frame_input(&buffer[..count]) {
                    self.retire_physical_port(error.code);
                    self.live_serial_session
                        .store(0, std::sync::atomic::Ordering::Release);
                    self.current_path = None;
                    self.current_baud = 0;
                    self.line_buffer.clear();
                    self.disconnected_error = Some(error);
                }
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                self.retire_physical_port("serial_disconnected");
                self.live_serial_session
                    .store(0, std::sync::atomic::Ordering::Release);
                self.current_path = None;
                self.current_baud = 0;
                self.line_buffer.clear();
                self.disconnected_error = Some(ActorError::new(
                    "serial_disconnected",
                    "device disconnected: serial read failed",
                ));
            }
        }
    }

    fn finish(&mut self) {
        if let Some(transaction) = self.deferred_radio.take() {
            fail_radio(transaction, "serial_not_open");
        }
        self.cancel_initialization(ActorError::unavailable("serial actor stopped"));
        self.sms_receive.invalidate();
        if let Some(pending) = self.pending_read.take() {
            let _ = pending
                .reply
                .send(Err(ActorError::unavailable("serial actor stopped")));
        }
        if let Some(mut pending) = self.pending_command.take() {
            scrub_sensitive_pending_response(&mut pending);
            self.fail_completion(
                pending.completion,
                ActorError::unavailable("serial actor stopped"),
            );
        }
        if let Some(deferred) = self.deferred_sms_submit.take() {
            let _ = deferred.transaction.reply.send(Err(ActorError::unavailable(
                "serial actor stopped between SMS segments",
            )));
        }
        self.fail_queued_commands("serial actor stopped");
        // Dropping the sole handle is the final release path.
        self.retire_physical_port("serial_actor_stopped");
        self.live_serial_session
            .store(0, std::sync::atomic::Ordering::Release);
    }
}

fn scrub_sensitive_pending_response(pending: &mut PendingCommand) {
    if matches!(
        &pending.completion,
        CommandCompletion::Radio(transaction) if transaction.plan.sensitive_in_flight()
    ) || matches!(&pending.completion, CommandCompletion::Card(_))
    {
        for line in &mut pending.lines {
            line.zeroize();
        }
        pending.lines.clear();
    }
}

fn probe_serial_port_imei(port: &mut dyn serialport::SerialPort) -> Option<String> {
    port.set_timeout(AT_DISCOVERY_READ_TIMEOUT).ok()?;
    port.write_all(b"AT\r\n").ok()?;
    thread::sleep(Duration::from_millis(40));
    port.write_all(b"AT+CGSN\r\n").ok()?;

    let deadline = Instant::now() + AT_DISCOVERY_PROBE_TIMEOUT;
    let mut response = Vec::with_capacity(256);
    let mut buffer = [0_u8; 1024];
    while Instant::now() < deadline {
        match port.read(&mut buffer) {
            Ok(0) => continue,
            Ok(count) => {
                if response.len().saturating_add(count) > AT_DISCOVERY_MAX_RESPONSE_BYTES {
                    return None;
                }
                response.extend_from_slice(&buffer[..count]);
                if let Some(imei) = parse_probe_imei(&response) {
                    return Some(imei);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
    parse_probe_imei(&response)
}

fn parse_probe_imei(response: &[u8]) -> Option<String> {
    std::str::from_utf8(response)
        .ok()?
        .split(['\r', '\n'])
        .map(str::trim)
        .find(|line| line.len() == 15 && line.bytes().all(|byte| byte.is_ascii_digit()))
        .map(str::to_owned)
}

fn run_actor(
    receiver: Receiver<Command>,
    opener: PortOpener,
    wake: Arc<tokio::sync::Notify>,
    cellular_suppressed: Arc<std::sync::atomic::AtomicBool>,
    call_capture: Arc<std::sync::Mutex<Option<crate::module_radio::CallCapture>>>,
    live_serial_session: Arc<std::sync::atomic::AtomicU64>,
    owner_epoch: String,
    card_shared: Arc<CardShared>,
    native_shared: Arc<NativeShared>,
    module_recovery: crate::module_recovery::Gate,
) {
    let mut state = ActorState::new();
    state.module_recovery = module_recovery;
    state.serial_sms.owner_epoch = owner_epoch;
    state.card_shared = card_shared;
    state.native.shared = native_shared;
    state.sms_receive = ReceiveQueue::new(wake);
    state.cellular_suppressed = cellular_suppressed;
    state.call_capture = call_capture;
    state.live_serial_session = live_serial_session;
    let mut buffer = [0_u8; SERIAL_READ_CHUNK_BYTES];

    'actor: loop {
        match receiver.recv_timeout(SERIAL_POLL_INTERVAL) {
            Ok(command) => {
                state.process(command, &opener);
                if state.finalize_reply.is_some() {
                    break 'actor;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let mut disconnected = false;
        loop {
            match receiver.try_recv() {
                Ok(command) => {
                    state.process(command, &opener);
                    if state.finalize_reply.is_some() {
                        break 'actor;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            break;
        }
        state.complete_pending_read();
        state.poll_input(&mut buffer);
        state.complete_pending_command();
        if state.native.managed() && state.pending_command.is_none() {
            state.release_idle_frames();
        }
        // A command may have been accepted while the current response was
        // being drained. Admit that bounded backlog before selecting the next
        // transaction so a newly queued high-priority request can pass older
        // normal work.
        loop {
            match receiver.try_recv() {
                Ok(command) => {
                    state.process(command, &opener);
                    if state.finalize_reply.is_some() {
                        break 'actor;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            break;
        }
        state.start_next_command();
        state.publish_card_binding();
        state.observe_native_policy();
        state.publish_native_facts();
        if state.card_lease.is_none() && state.module_hold.is_none() && !state.native.refreshing {
            for command in std::mem::take(&mut state.card_deferred_closes) {
                state.process(command, &opener);
            }
        }
    }
    state.finish();
    if let Some(reply) = state.finalize_reply.take() {
        let _ = reply.send(Ok(()));
    }
}

fn open_serial_port(
    path: String,
    baud_rate: u32,
) -> Result<Box<dyn serialport::SerialPort>, ActorError> {
    serialport::new(path, baud_rate)
        .data_bits(serialport::DataBits::Eight)
        .flow_control(serialport::FlowControl::None)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .timeout(SERIAL_READ_TIMEOUT)
        .open()
        .map_err(|error| {
            if error.kind() == serialport::ErrorKind::NoDevice {
                ActorError::new(
                    "serial_resource_busy",
                    "resource busy or serial device unavailable",
                )
            } else {
                ActorError::new("serial_open_failed", "serial port could not be opened")
            }
        })
}

fn enumerate_at_port_candidates() -> Result<Vec<String>, ActorError> {
    let entries = std::fs::read_dir("/dev")
        .map_err(|_| ActorError::unavailable("serial device directory could not be read"))?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|_| ActorError::unavailable("serial device directory could not be read"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if ["cu.usbmodem", "cu.usbserial", "cu.wchusbserial"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            candidates.push(format!("/dev/{name}"));
        }
    }
    candidates.sort();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(at_port_priority(candidate)));
    candidates.truncate(MAX_AT_DISCOVERY_CANDIDATES);
    Ok(candidates)
}

fn at_port_priority(candidate: &str) -> u8 {
    let candidate = candidate.to_ascii_lowercase();
    if candidate.contains("quectel") || candidate.contains("dji") {
        100
    } else if candidate.contains("usbmodem") {
        80
    } else if candidate.contains("usbserial") {
        60
    } else {
        0
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    port: String,
    baud_rate: u32,
    #[serde(default)]
    sms_receive_owned: bool,
    #[serde(default)]
    initialization_owned: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    maximum: usize,
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    data_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteRequest {
    command: String,
    timeout_ms: u64,
    #[serde(default)]
    high_priority: bool,
    #[serde(default)]
    follow_up: Option<String>,
    #[serde(default)]
    expected_sms_generation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TypedQueryRequest {
    query: String,
    #[serde(default)]
    suppress_cellular: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallCommandRequest {
    action: String,
    #[serde(default)]
    value: String,
}

fn call_command_spec(action: &str, value: &str) -> Result<(&'static str, String, u64), ActorError> {
    match action {
        "dial" => {
            let bytes = value.as_bytes();
            if bytes.is_empty()
                || bytes.len() > 32
                || bytes.iter().enumerate().any(|(index, byte)| {
                    !byte.is_ascii_digit()
                        && *byte != b'*'
                        && *byte != b'#'
                        && !(*byte == b'+' && index == 0)
                })
            {
                return Err(ActorError::invalid("invalid call number"));
            }
            Ok(("dial", format!("ATD{value};"), 10_000))
        }
        "answer" if value.is_empty() => Ok(("answer", "ATA".into(), 10_000)),
        "hangup" if value.is_empty() => Ok(("hangup", "ATH".into(), 10_000)),
        "dtmf"
            if value.len() == 1
                && value
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'*' | b'#')) =>
        {
            Ok(("dtmf", format!("AT+VTS={value}"), 5_000))
        }
        "answer" | "hangup" | "dtmf" => Err(ActorError::invalid("invalid value for call command")),
        _ => Err(ActorError::invalid("unsupported call command")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitSmsRequest {
    recipient: String,
    text: String,
    #[serde(default)]
    encoding: String,
    #[serde(default)]
    expected_sms_generation: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArchivedSmsRequest {
    index: u32,
    pdu: String,
    expected_sms_generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadStoredSmsRequest {
    #[serde(default)]
    storage: Option<String>,
    #[serde(default)]
    index: Option<u32>,
    expected_sms_generation: u64,
}

fn normalized_lines(lines: &[String]) -> impl Iterator<Item = &str> {
    lines.iter().map(|line| line.trim())
}

fn parse_imei(lines: &[String]) -> String {
    normalized_lines(lines)
        .find(|line| line.len() == 15 && line.bytes().all(|byte| byte.is_ascii_digit()))
        .unwrap_or_default()
        .to_owned()
}

fn parse_firmware(lines: &[String]) -> String {
    normalized_lines(lines)
        .find(|line| !line.is_empty() && *line != "OK" && !line.starts_with('+'))
        .unwrap_or_default()
        .to_owned()
}

fn parse_decimal_prefix(value: &str) -> Option<i64> {
    let value = value.trim_start();
    let bytes = value.as_bytes();
    let mut end = usize::from(
        bytes
            .first()
            .is_some_and(|byte| matches!(*byte, b'+' | b'-')),
    );
    let digits_start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    value[..end].parse().ok()
}

fn parse_qsimstat_inserted(lines: &[String]) -> Option<bool> {
    normalized_lines(lines)
        .find(|line| line.starts_with("+QSIMSTAT:"))
        .and_then(|line| line.split(',').nth(1))
        .and_then(parse_decimal_prefix)
        .map(|inserted| inserted == 1)
}

fn parse_cpin_inserted(lines: &[String]) -> Option<bool> {
    let response = lines.join("\n").to_ascii_uppercase();
    if response.contains("READY") {
        Some(true)
    } else if response.contains("NOT INSERTED") {
        Some(false)
    } else {
        None
    }
}

fn parse_imsi(lines: &[String]) -> String {
    normalized_lines(lines)
        .map(|line| line.strip_prefix("+CIMI:").unwrap_or(line).trim())
        .find(|line| {
            (14..=15).contains(&line.len()) && line.bytes().all(|byte| byte.is_ascii_digit())
        })
        .unwrap_or_default()
        .to_owned()
}

fn parse_iccid(lines: &[String]) -> String {
    let Some(line) = normalized_lines(lines).find(|line| line.starts_with("+QCCID:")) else {
        return String::new();
    };
    line.split_once(':')
        .map(|(_, value)| {
            value
                .trim()
                .trim_matches('"')
                .trim_end_matches(['F', 'f'])
                .to_owned()
        })
        .unwrap_or_default()
}

fn parse_cops_operator(lines: &[String]) -> String {
    normalized_lines(lines)
        .find(|line| line.starts_with("+COPS:"))
        .and_then(|line| line.split_once(':').map(|(_, value)| value))
        .and_then(|value| {
            let start = value.find('"')? + 1;
            let end = value[start..].find('"')? + start;
            Some(value[start..end].to_owned())
        })
        .unwrap_or_default()
}

fn parse_cops_act(lines: &[String]) -> String {
    let line = normalized_lines(lines)
        .find(|line| line.starts_with("+COPS:"))
        .or_else(|| normalized_lines(lines).find(|line| line.contains(',')));
    let Some(act) = line
        .and_then(|line| line.rsplit_once(',').map(|(_, value)| value))
        .and_then(parse_decimal_prefix)
    else {
        return String::new();
    };
    match act {
        0 => "GSM".to_owned(),
        2 => "WCDMA".to_owned(),
        7 => "LTE".to_owned(),
        10 => "NR".to_owned(),
        _ => String::new(),
    }
}

fn parse_crsm_response(lines: &[String]) -> Result<Vec<u8>, ActorError> {
    let response = lines.join("\r\n");
    let line = normalized_lines(lines)
        .find(|line| line.starts_with("+CRSM:"))
        .ok_or_else(|| {
            ActorError::new(
                "sim_crsm_invalid",
                format!("invalid CRSM response for EF 28486: {response} (sw1=0, sw2=0)"),
            )
        })?;
    let fields: Vec<_> = line
        .strip_prefix("+CRSM:")
        .unwrap_or_default()
        .splitn(3, ',')
        .collect();
    let sw1 = fields
        .first()
        .and_then(|value| parse_decimal_prefix(value))
        .unwrap_or(0);
    let sw2 = fields
        .get(1)
        .and_then(|value| parse_decimal_prefix(value))
        .unwrap_or(0);
    let hex_data = fields
        .get(2)
        .map(|value| value.trim().trim_matches('"'))
        .unwrap_or_default();
    if fields.len() < 2 || sw1 != 144 || sw2 != 0 || hex_data.is_empty() {
        return Err(ActorError::new(
            "sim_crsm_invalid",
            format!("invalid CRSM response for EF 28486: {response} (sw1={sw1}, sw2={sw2})"),
        ));
    }
    if !hex_data.len().is_multiple_of(2) {
        return Err(ActorError::new(
            "sim_crsm_hex_invalid",
            "EF 28486 hex decode failed: odd-length hexadecimal data",
        ));
    }
    let mut bytes = Vec::with_capacity(hex_data.len() / 2);
    for pair in hex_data.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]);
        let low = hex_nibble(pair[1]);
        match (high, low) {
            (Some(high), Some(low)) => bytes.push((high << 4) | low),
            _ => {
                return Err(ActorError::new(
                    "sim_crsm_hex_invalid",
                    "EF 28486 hex decode failed: invalid hexadecimal data",
                ));
            }
        }
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn decode_ef_spn(data: &[u8]) -> Result<String, String> {
    if data.is_empty() {
        return Err("EF_SPN data empty".into());
    }
    let mut name = if data.len() > 1 { &data[1..] } else { data };
    while name.last().is_some_and(|byte| matches!(byte, 0x00 | 0xff)) {
        name = &name[..name.len() - 1];
    }
    if name.is_empty() {
        return Err("EF_SPN name empty".into());
    }
    let decoded = match name[0] {
        0x80 => decode_spn_ucs2(&name[1..])?,
        0x81 => decode_spn_compressed_ucs2(name, 1)?,
        0x82 => decode_spn_compressed_ucs2(name, 2)?,
        _ if name.iter().all(|byte| (0x20..=0x7e).contains(byte)) => {
            String::from_utf8(name.to_vec()).map_err(|_| "EF_SPN ASCII decode failed")?
        }
        _ => {
            proletarius_sms::alphabet::decode(name).map_err(|_| "EF_SPN GSM 7-bit decode failed")?
        }
    };
    let decoded = decoded.replace('\0', "");
    let decoded = decoded.trim();
    if decoded.is_empty() {
        return Err("EF_SPN name empty".into());
    }
    Ok(decoded.to_owned())
}

fn decode_spn_ucs2(data: &[u8]) -> Result<String, String> {
    if data.is_empty() {
        return Err("UCS2 SPN empty".into());
    }
    if data.len() % 2 != 0 {
        return Err(format!("UCS2 SPN odd length: {}", data.len()));
    }
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    Ok(char::decode_utf16(units)
        .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect())
}

fn decode_spn_compressed_ucs2(data: &[u8], base_bytes: usize) -> Result<String, String> {
    if data.len() < 2 + base_bytes {
        return Err("compressed UCS2 SPN too short".into());
    }
    let count = usize::from(data[1]);
    let payload_start = 2 + base_bytes;
    let payload = &data[payload_start..data.len().min(payload_start + count)];
    let base = if base_bytes == 1 {
        u16::from(data[2]) << 7
    } else {
        u16::from_be_bytes([data[2], data[3]])
    };
    let mut decoded = String::new();
    for &value in payload {
        if value < 0x80 {
            decoded.push_str(
                &proletarius_sms::alphabet::decode(&[value])
                    .map_err(|_| "compressed UCS2 SPN GSM decode failed")?,
            );
        } else {
            decoded.push(
                char::from_u32(u32::from(base.wrapping_add(u16::from(value & 0x7f))))
                    .unwrap_or(char::REPLACEMENT_CHARACTER),
            );
        }
    }
    Ok(decoded)
}

fn parse_csq(lines: &[String]) -> Option<(i64, i64)> {
    let line = normalized_lines(lines).find(|line| line.starts_with("+CSQ:"))?;
    let rssi = parse_decimal_prefix(line.strip_prefix("+CSQ:")?.split(',').next()?)?;
    let dbm = if (0..=31).contains(&rssi) {
        -113 + rssi * 2
    } else {
        -999
    };
    Some((rssi, dbm))
}

fn parse_creg(lines: &[String]) -> Option<(i64, String, String)> {
    let line = normalized_lines(lines).find(|line| line.starts_with("+CREG:"))?;
    let parts: Vec<_> = line.split(',').collect();
    if parts.len() < 2 {
        return None;
    }
    let status = parse_decimal_prefix(parts[1])?;
    let (lac, cell_id) = if parts.len() >= 4 {
        (
            parts[2].trim().trim_matches('"').to_owned(),
            parts[3].trim().trim_matches('"').to_owned(),
        )
    } else {
        (String::new(), String::new())
    };
    Some((status, lac, cell_id))
}

fn quoted_fields(line: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut remainder = line;
    while let Some(start) = remainder.find('"') {
        remainder = &remainder[start + 1..];
        let Some(end) = remainder.find('"') else {
            break;
        };
        fields.push(&remainder[..end]);
        remainder = &remainder[end + 1..];
    }
    fields
}

fn parse_serving_cell_lte(lines: &[String]) -> Option<(i64, i64, i64, String, String, u32)> {
    let line =
        normalized_lines(lines).find(|line| line.starts_with("+QENG:") && line.contains("LTE"))?;
    let parts: Vec<_> = line.split(',').collect();
    if parts.len() < 10 {
        return None;
    }
    let duplex = if parts.len() > 3 {
        parts[3].trim().trim_matches('"').to_owned()
    } else {
        String::new()
    };
    let channel = parts[8].trim().parse::<u32>().unwrap_or(0);
    let raw_band = parts[9].trim();
    let band = if raw_band.is_empty() {
        String::new()
    } else {
        format!("LTE BAND {}", raw_band.trim_matches('"'))
    };
    let tail = &parts[parts.len() - 5..];
    let mut parsed = Vec::with_capacity(5);
    for part in tail {
        parsed.push(parse_decimal_prefix(part)?);
    }
    let rsrp = parsed[0];
    let rsrq = parsed[1];
    let sinr = parsed[3];
    if !(-140..=-40).contains(&rsrp) || !(-30..=0).contains(&rsrq) {
        return None;
    }
    Some((rsrp, rsrq, sinr, duplex, band, channel))
}

fn parse_apn(lines: &[String]) -> String {
    normalized_lines(lines)
        .find(|line| line.starts_with("+CGDCONT:"))
        .map(quoted_fields)
        .and_then(|fields| fields.get(1).copied())
        .unwrap_or_default()
        .to_owned()
}

fn parse_qims(lines: &[String]) -> Option<i64> {
    normalized_lines(lines)
        .find(|line| line.starts_with("+QIMS:"))
        .and_then(|line| line.split_once(':').map(|(_, value)| value))
        .and_then(parse_decimal_prefix)
}

fn parse_qnwinfo_radio(lines: &[String]) -> (String, String, String, u32) {
    let Some(line) = normalized_lines(lines).find(|line| line.starts_with("+QNWINFO:")) else {
        return (String::new(), String::new(), String::new(), 0);
    };
    let fields = quoted_fields(line);
    let Some(raw_mode) = fields.first() else {
        return (String::new(), String::new(), String::new(), 0);
    };
    let (mode, duplex) = match raw_mode.trim() {
        "FDD LTE" => ("LTE", "FDD"),
        "TDD LTE" => ("LTE", "TDD"),
        other => (other, ""),
    };
    let band = fields
        .get(2)
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    let channel = line
        .rsplit_once(',')
        .and_then(|(_, value)| value.trim().parse::<u32>().ok())
        .unwrap_or(0);
    (mode.to_owned(), duplex.to_owned(), band, channel)
}

fn parse_smsc(lines: &[String]) -> String {
    normalized_lines(lines)
        .find(|line| line.starts_with("+CSCA:"))
        .and_then(|line| {
            quoted_fields(line)
                .first()
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_default()
}

fn canonical_phone_candidate(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("FFFFFFFF")
        || value == "00000000000"
        || value.eq_ignore_ascii_case("Own Number")
    {
        return String::new();
    }
    let value = if value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("tel:"))
    {
        value[4..].trim()
    } else {
        value
    };
    let normalized: String = value
        .chars()
        .filter(|character| !matches!(character, ' ' | '-' | '(' | ')'))
        .collect();
    let digits = normalized.strip_prefix('+').unwrap_or(&normalized);
    if digits.len() < 6 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return String::new();
    }
    normalized
}

fn parse_msisdn(lines: &[String]) -> String {
    for line in normalized_lines(lines).filter(|line| line.starts_with("+CNUM:")) {
        for field in quoted_fields(line) {
            let candidate = canonical_phone_candidate(field);
            if !candidate.is_empty() {
                return candidate;
            }
        }
    }
    String::new()
}

fn parse_usbnet(lines: &[String]) -> Option<i64> {
    let line = normalized_lines(lines)
        .find(|line| line.starts_with("+QCFG:") && line.contains("\"usbnet\""))?;
    line.split(',').nth(1).and_then(parse_decimal_prefix)
}

fn parse_uac_enabled(lines: &[String]) -> Option<bool> {
    lines.iter().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let marker = "\"usbcfg\",";
        let start = lower.find(marker)? + marker.len();
        lower[start..]
            .split(',')
            .nth(8)
            .map(|value| value.trim().trim_end_matches('\r') == "1")
    })
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    mut actor: ModemActor,
) -> Response<Full<Bytes>> {
    actor.compatibility_rpc = true;
    if request.method() != Method::POST {
        return error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "serial actor endpoints require POST",
        );
    }
    let path = request.uri().path().to_owned();
    if actor.native_serial_managed() && path != PROJECT_CALL_STATE_PATH {
        return error_response(
            StatusCode::CONFLICT,
            "serial_native_owner",
            "Serial transport is managed by the native owner",
        );
    }
    let body_limit = if path == "/api/internal/modem/submit-sms" {
        MAX_SMS_SUBMIT_REQUEST_BYTES
    } else if path == PROJECT_CALL_STATE_PATH {
        MAX_CALL_STATE_PROJECTION_REQUEST_BYTES
    } else if path == CALL_COMMAND_PATH {
        1_024
    } else {
        12 * 1024
    };
    let body = match crate::http_server::collect_body(request.into_body(), body_limit).await {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return crate::http_server::body_timeout_response();
        }
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "serial actor request exceeds its byte limit",
            );
        }
    };

    if path == PROJECT_CALL_STATE_PATH {
        return call_state_projection::handle(&body);
    }

    if path == CALL_COMMAND_PATH {
        let input: CallCommandRequest = match serde_json::from_slice(&body) {
            Ok(input) => input,
            Err(_) => return invalid_json(),
        };
        return match tokio::task::spawn_blocking(move || {
            actor.execute_call_command(&input.action, &input.value)
        })
        .await
        {
            Ok(Ok(result)) => crate::json_response(StatusCode::OK, &result),
            Ok(Err(error)) => actor_error_response(error),
            Err(_) => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "serial_actor_stopped",
                "serial actor task failed",
            ),
        };
    }

    if path == "/api/internal/modem/ingest-stored-sms" {
        return ingest_stored_sms(&body, actor).await;
    }

    let result = match path.as_str() {
        "/api/internal/modem/discover-at-port" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct DiscoverATPortRequest {}
            if serde_json::from_slice::<DiscoverATPortRequest>(&body).is_err() {
                return invalid_json();
            }
            match tokio::task::spawn_blocking(move || {
                let candidates = enumerate_at_port_candidates()?;
                actor.discover_at_port(candidates)
            })
            .await
            {
                Ok(Ok(discovered)) => Ok(json!(discovered)),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/open" => {
            let input: OpenRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            if input.sms_receive_owned {
                let store = match crate::message_archive::archive() {
                    Ok(store) => store,
                    Err(error) => return crate::message_archive::store_error(error),
                };
                actor.ensure_receive_worker(store);
            }
            let owned = input.sms_receive_owned;
            let initialization_owned = input.initialization_owned;
            match tokio::task::spawn_blocking(move || {
                // The selected generation is acknowledged by the same Open
                // actor command; a later rebind cannot change the proxy token.
                actor.open_with_initialization(input.port, input.baud_rate, initialization_owned)
            })
            .await
            {
                Ok(Ok(serial_session)) => Ok(
                    json!({"opened": true, "serial_session":serial_session,"sms_receive_owned":owned,"initialization_owned":initialization_owned}),
                ),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/initialize" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct InitializeRequest {}
            if serde_json::from_slice::<InitializeRequest>(&body).is_err() {
                return invalid_json();
            }
            match tokio::task::spawn_blocking(move || actor.initialize()).await {
                Ok(Ok(result)) => Ok(json!(result)),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/read" => {
            let input: ReadRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            match tokio::task::spawn_blocking(move || actor.read(input.maximum, input.timeout_ms))
                .await
            {
                Ok(Ok(frame)) => Ok(json!({
                    "data_base64": STANDARD.encode(frame.data),
                    "is_urc": frame.is_urc,
                    "pure_async_urc": frame.pure_async_urc,
                    "sms_generation": frame.sms_generation,
                    "sim_event": frame.sim_event,
                    "sim_actions": frame.sim_actions,
                    "sms_event": frame.sms_event,
                    "sms_ingest_owned":frame.sms_ingest_owned,
                    "call_event_owned":frame.call_event_owned,
                    "ussd_event": frame.ussd_event
                })),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/write" => {
            let input: WriteRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            let data = match STANDARD.decode(input.data_base64.as_bytes()) {
                Ok(data) if !data.is_empty() && data.len() <= MAX_WRITE_BYTES => data,
                _ => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "serial write payload is invalid",
                    );
                }
            };
            match tokio::task::spawn_blocking(move || actor.write(data)).await {
                Ok(Ok(written)) => Ok(json!({"written": written})),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/execute" => {
            let input: ExecuteRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            match tokio::task::spawn_blocking(move || {
                let follow_up = input.follow_up.map(String::into_bytes);
                actor.execute_scheduled(
                    input.command,
                    input.timeout_ms,
                    input.high_priority,
                    follow_up,
                    input.expected_sms_generation,
                )
            })
            .await
            {
                Ok(Ok(reply)) => Ok(json!({
                    "lines": reply.lines,
                    "result": reply.result,
                    "terminal": reply.terminal,
                    "elapsed_ms": reply.elapsed_ms,
                    "timeout_streak": reply.timeout_streak,
                    "timeout_threshold": reply.timeout_threshold,
                    "watchdog_tripped": reply.watchdog_tripped
                })),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/typed-query" => {
            let input: TypedQueryRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            match tokio::task::spawn_blocking(move || {
                if input.query == "device_info_snapshot" {
                    serde_json::to_value(actor.device_info_snapshot(input.suppress_cellular))
                        .map_err(|_| {
                            ActorError::unavailable(
                                "device-info snapshot response could not be encoded",
                            )
                        })
                } else {
                    actor.typed_query(&input.query).and_then(|result| {
                        serde_json::to_value(result).map_err(|_| {
                            ActorError::unavailable("typed query response could not be encoded")
                        })
                    })
                }
            })
            .await
            {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/submit-sms" => {
            let input: SubmitSmsRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            match tokio::task::spawn_blocking(move || {
                actor.submit_sms(
                    input.recipient,
                    input.text,
                    input.encoding,
                    input.expected_sms_generation,
                )
            })
            .await
            {
                Ok(Ok(result)) => Ok(json!({
                    "accepted_segments": result.accepted_segments,
                    "complete": result.complete
                })),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/delete-archived-sms" => {
            let input: DeleteArchivedSmsRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            match tokio::task::spawn_blocking(move || {
                actor.delete_archived_sms(input.index, input.pdu, input.expected_sms_generation)
            })
            .await
            {
                Ok(Ok(result)) => Ok(json!({
                    "deleted": result.deleted,
                    "reason": result.reason
                })),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/read-stored-sms" => {
            let input: ReadStoredSmsRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            match tokio::task::spawn_blocking(move || {
                actor.read_stored_sms(input.storage, input.index, input.expected_sms_generation)
            })
            .await
            {
                Ok(Ok(result)) => Ok(json!({
                    "storage": result.storage,
                    "lines": result.lines,
                    "storage_restored": result.storage_restored
                })),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/close" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct CloseRequest {
                serial_session: u64,
            }
            let input: CloseRequest = match serde_json::from_slice::<CloseRequest>(&body) {
                Ok(input) if input.serial_session != 0 => input,
                _ => return invalid_json(),
            };
            let session = input.serial_session;
            match tokio::task::spawn_blocking(move || {
                actor.submit(
                    |reply| Command::FencedClose {
                        serial_session: session,
                        reply,
                    },
                    RPC_TIMEOUT,
                )
            })
            .await
            {
                Ok(Ok(())) => Ok(json!({"closed":true,"serial_session":session})),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        "/api/internal/modem/sms-receive" => {
            let input: ReceiveControlRequest = match serde_json::from_slice(&body) {
                Ok(input) => input,
                Err(_) => return invalid_json(),
            };
            if !matches!(
                (input.action.as_str(), input.enabled),
                ("configure", Some(_)) | ("scan", None)
            ) {
                return invalid_json();
            }
            let store = match crate::message_archive::archive() {
                Ok(store) => store,
                Err(error) => return crate::message_archive::store_error(error),
            };
            actor.ensure_receive_worker(store);
            match tokio::task::spawn_blocking(move || match input.enabled {
                Some(enabled) => actor.configure_sms_receive(enabled),
                None => actor.scan_sms_receive(),
            })
            .await
            {
                Ok(Ok(())) => Ok(json!({"accepted":true})),
                Ok(Err(error)) => return actor_error_response(error),
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "serial_actor_stopped",
                        "serial actor task failed",
                    );
                }
            }
        }
        _ => {
            return error_response(
                StatusCode::NOT_FOUND,
                "route_not_found",
                "serial actor route not found",
            );
        }
    };

    match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(error) => actor_error_response(error),
    }
}

fn invalid_json() -> Response<Full<Bytes>> {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "invalid JSON request",
    )
}

fn actor_error_response(error: ActorError) -> Response<Full<Bytes>> {
    let status = match error.code {
        "invalid_request" => StatusCode::BAD_REQUEST,
        "serial_resource_busy"
        | "serial_already_open"
        | "serial_read_busy"
        | "serial_command_busy"
        | "serial_command_queue_full"
        | "sms_receive_queue_full"
        | "serial_initialization_busy"
        | "sms_not_ready" => StatusCode::CONFLICT,
        "serial_not_open" | "serial_disconnected" | "serial_input_overflow" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        "sms_generation_changed"
        | "sms_receive_session_changed"
        | "radio_authorization_required"
        | "module_flight_mode_blocks_cellular_probe" => StatusCode::CONFLICT,
        "sms_storage_query_failed"
        | "sms_storage_query_invalid"
        | "sms_storage_select_failed"
        | "sms_storage_read_failed" => StatusCode::BAD_GATEWAY,
        "serial_actor_busy" | "serial_actor_timeout" | "serial_actor_stopped" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error_response(status, error.code, &error.message)
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"code": code, "error": message}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::SerialPort as _;
    use std::sync::Mutex;
    // Legacy transport-only fixtures explicitly permit cellular commands;
    // production actors default to suppressed until native policy confirms it.
    fn test_state() -> ActorState {
        let state = ActorState::new();
        state
            .cellular_suppressed
            .store(false, std::sync::atomic::Ordering::Release);
        state
    }
    fn test_actor() -> io::Result<ModemActor> {
        let actor = ModemActor::new()?;
        actor.set_cellular_suppressed(false);
        Ok(actor)
    }
    fn test_actor_with_opener(
        opener: impl Fn(String, u32) -> Result<Box<dyn serialport::SerialPort>, ActorError>
        + Send
        + 'static,
    ) -> io::Result<ModemActor> {
        let actor = ModemActor::with_port_opener(opener)?;
        actor.set_cellular_suppressed(false);
        Ok(actor)
    }

    fn shutdown_channel_fixture() -> (ModemActor, Receiver<Command>) {
        let mut actor = test_actor_with_opener(|_, _| panic!("no physical port expected")).unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        // Replace only this test client's channel. Dropping its original sender
        // lets the unused real actor finish without ever opening a port.
        Arc::get_mut(&mut actor.client).unwrap().sender = sender;
        (actor, receiver)
    }

    #[test]
    fn final_shutdown_rejects_actor_loss_before_command_acceptance() {
        let (actor, receiver) = shutdown_channel_fixture();
        drop(receiver);
        assert_eq!(
            actor.shutdown().unwrap_err().code,
            "serial_shutdown_unconfirmed"
        );
    }

    #[test]
    fn final_shutdown_rejects_missing_close_acknowledgement() {
        let (actor, receiver) = shutdown_channel_fixture();
        let peer = thread::spawn(move || {
            let Command::Finalize { reply } =
                receiver.recv_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("expected terminal finalization")
            };
            drop(reply);
        });
        assert_eq!(
            actor.shutdown().unwrap_err().code,
            "serial_shutdown_unconfirmed"
        );
        peer.join().unwrap();
    }

    #[test]
    fn final_shutdown_preserves_negative_close_acknowledgement() {
        let (actor, receiver) = shutdown_channel_fixture();
        let peer = thread::spawn(move || {
            let Command::Finalize { reply } =
                receiver.recv_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("expected terminal finalization")
            };
            reply
                .send(Err(ActorError::new(
                    "fixture_close_rejected",
                    "still owned",
                )))
                .unwrap();
        });
        assert_eq!(actor.shutdown().unwrap_err().code, "fixture_close_rejected");
        peer.join().unwrap();
        assert_eq!(actor.shutdown().unwrap_err().code, "fixture_close_rejected");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_shutdown_cancelled_waiter_and_concurrent_call_share_close_receipt() {
        let (actor, receiver) = shutdown_channel_fixture();
        let first_actor = actor.clone();
        let first = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || first_actor.shutdown()).await
        });
        let (reply, receiver) = tokio::task::spawn_blocking(move || {
            let Command::Finalize { reply } =
                receiver.recv_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("expected terminal finalization")
            };
            (reply, receiver)
        })
        .await
        .unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let next_actor = actor.clone();
        let next = tokio::task::spawn_blocking(move || next_actor.shutdown());
        assert!(!next.is_finished());
        reply.send(Ok(())).unwrap();
        next.await.unwrap().unwrap();
        actor.shutdown().unwrap();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn final_shutdown_waits_for_actor_close_and_joins_owner_thread() {
        let actor = test_actor_with_opener(|_, _| panic!("no physical port expected")).unwrap();
        let (entered, started) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        actor
            .client
            .sender
            .send(Command::LegacyGate(Box::new(move |_, _| {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            })))
            .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let (complete, completed) = mpsc::sync_channel(1);
        let closing_actor = actor.clone();
        let closing = thread::spawn(move || complete.send(closing_actor.shutdown()).unwrap());
        assert!(matches!(
            completed.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        completed
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        closing.join().unwrap();
        assert!(actor.client.actor_thread.lock().unwrap().is_none());
        assert_eq!(actor.close().unwrap_err().code, "serial_actor_stopped");
        // A repeated final shutdown reuses the confirmed close-and-join receipt.
        actor.shutdown().unwrap();
    }

    #[test]
    fn at_port_discovery_matches_legacy_candidates_and_priority() {
        assert_eq!(
            at_port_priority("/dev/cu.usbserial-DJI-1"),
            100,
            "vendor-labelled candidates keep the legacy highest priority"
        );
        assert_eq!(at_port_priority("/dev/cu.usbmodem123"), 80);
        assert_eq!(at_port_priority("/dev/cu.wchusbserial1410"), 60);
        assert_eq!(at_port_priority("/dev/cu.fixture"), 0);
        let mut candidates = vec![
            "/dev/cu.wchusbserial1410".to_owned(),
            "/dev/cu.usbmodem123".to_owned(),
            "/dev/cu.usbserial-DJI-1".to_owned(),
        ];
        candidates.sort();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(at_port_priority(candidate)));
        assert_eq!(
            candidates,
            [
                "/dev/cu.usbserial-DJI-1",
                "/dev/cu.usbmodem123",
                "/dev/cu.wchusbserial1410"
            ]
        );
    }

    #[test]
    fn at_port_discovery_probes_only_through_the_rust_actor_owner() {
        let (mut master, slave) = serialport::TTYPort::pair().expect("pseudo-terminal pair");
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let responder = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut observed = Vec::new();
            let mut buffer = [0_u8; 64];
            while Instant::now() < deadline {
                match master.read(&mut buffer) {
                    Ok(count) if count > 0 => {
                        observed.extend_from_slice(&buffer[..count]);
                        if observed
                            .windows(b"AT+CGSN\r\n".len())
                            .any(|window| window == b"AT+CGSN\r\n")
                        {
                            master
                                .write_all(b"AT+CGSN\r\n353936932685355\r\nOK\r\n")
                                .unwrap();
                            // Keep the peer open until discovery has consumed the
                            // reply. Closing a macOS PTY immediately after write
                            // can discard unread bytes under parallel test load.
                            return (observed, master);
                        }
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::TimedOut
                                | io::ErrorKind::WouldBlock
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => panic!("pseudo-terminal read failed: {error}"),
                }
            }
            (observed, master)
        });
        let held_port = Arc::new(Mutex::new(Some(
            Box::new(slave) as Box<dyn serialport::SerialPort>
        )));
        let opener_port = Arc::clone(&held_port);
        let actor = test_actor_with_opener(move |path, baud| {
            assert_eq!(baud, AT_DISCOVERY_BAUD_RATE);
            if path == "/dev/cu.usbmodem-no-at" {
                return Err(ActorError::unavailable(
                    "fixture candidate is not an AT port",
                ));
            }
            assert_eq!(path, "/dev/cu.usbmodem-fixture");
            opener_port
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| ActorError::unavailable("fixture port already consumed"))
        })
        .unwrap();

        let discovered = actor
            .discover_at_port(vec![
                "/dev/cu.usbmodem-no-at".to_owned(),
                "/dev/cu.usbmodem-fixture".to_owned(),
            ])
            .unwrap();
        let (observed, _master) = responder.join().unwrap();
        assert_eq!(discovered.port, "/dev/cu.usbmodem-fixture");
        assert!(observed.windows(4).any(|window| window == b"AT\r\n"));
        assert!(
            observed
                .windows(b"AT+CGSN\r\n".len())
                .any(|window| window == b"AT+CGSN\r\n")
        );
        assert!(held_port.lock().unwrap().is_none());
        actor.shutdown().unwrap();
    }

    #[test]
    fn at_port_discovery_rejects_unbounded_or_non_callout_candidates() {
        let actor = test_actor().unwrap();
        assert_eq!(
            actor.discover_at_port(Vec::new()).unwrap_err().code,
            "at_port_not_found"
        );
        assert_eq!(
            actor
                .discover_at_port(vec!["/dev/tty.usbmodem0".to_owned()])
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            actor
                .discover_at_port(vec![
                    "/dev/cu.usbmodem0".to_owned();
                    MAX_AT_DISCOVERY_CANDIDATES + 1
                ])
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    fn stored_record(
        index: u64,
        content: &str,
        reference: Option<u16>,
        sequence: u8,
        complete: bool,
    ) -> Value {
        let concatenation = reference.map(|reference| {
            json!({
                "reference":reference,
                "wide_reference":false,
                "total":2,
                "sequence":sequence
            })
        });
        json!({
            "index":index,
            "pdu":"AABB",
            "decoded":{
                "kind":"deliver",
                "sender":"+441234567890",
                "text":content,
                "binary_summary":null,
                "complete":complete,
                "concatenation":concatenation,
                "dcs":0,
                "service_center_time":null
            }
        })
    }

    #[test]
    fn rust_receive_selection_archives_only_the_notified_message_group() {
        let records = vec![
            stored_record(7, "", Some(91), 1, false),
            stored_record(8, "hello world", Some(91), 2, true),
            stored_record(9, "another message", None, 0, true),
        ];
        let multipart = stored_sms_candidates(&records, Some(7), "2026-10-05T00:00:00Z");
        assert_eq!(multipart.len(), 1);
        assert_eq!(multipart[0].index, 8);
        assert_eq!(multipart[0].content, "hello world");
        assert!(multipart[0].multipart);

        let single = stored_sms_candidates(&records, Some(9), "2026-10-05T00:00:00Z");
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].index, 9);
        assert!(!single[0].multipart);

        let all = stored_sms_candidates(&records, None, "2026-10-05T00:00:00Z");
        assert_eq!(all.len(), 2);
        assert!(stored_sms_candidates(&records, Some(99), "2026-10-05T00:00:00Z").is_empty());
    }

    #[test]
    fn rust_receive_selection_never_archives_incomplete_or_malformed_multipart() {
        let records = vec![
            stored_record(11, "partial", Some(12), 1, false),
            json!({
                "index":12,
                "pdu":"AABB",
                "decoded":{
                    "kind":"deliver",
                    "sender":"+441234567890",
                    "text":"wrong group",
                    "complete":false,
                    "concatenation":{
                        "reference":13,
                        "wide_reference":false,
                        "total":2,
                        "sequence":2
                    },
                    "dcs":0,
                    "service_center_time":null
                }
            }),
        ];
        assert!(stored_sms_candidates(&records, Some(11), "2026-10-05T00:00:00Z").is_empty());
        assert!(stored_sms_candidates(&records, None, "2026-10-05T00:00:00Z").is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rust_receive_archives_before_verifying_and_deleting_me_source() {
        use serialport::SerialPort as _;
        use std::fs;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        let switch_root = std::path::PathBuf::from(
            std::env::var_os("SWITCH_BUILD_DIR").expect("workspace-env build directory"),
        );
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = switch_root
            .join("tmp")
            .join(format!("rust-sms-receive-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&directory).expect("create in-project test directory");
        let archive_path = directory.join("sms-archive.json");
        let (mut master, slave) = serialport::TTYPort::pair().expect("pseudo-terminal");
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let ports = Arc::new(Mutex::new(VecDeque::from([
            Box::new(slave) as Box<dyn serialport::SerialPort>
        ])));
        let opener_ports = Arc::clone(&ports);
        let actor = test_actor_with_opener(move |_, _| {
            opener_ports
                .lock()
                .expect("pseudo-terminal holder lock")
                .pop_front()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let archive_for_responder = archive_path.clone();
        let (release_master, keep_master_open) = mpsc::channel();
        let responder = thread::spawn(move || {
            let pdu = "0000038188f8000862906000000000020041";
            let commands = [
                (
                    "AT+CPMS?",
                    "\r\n+CPMS: \"ME\",0,10,\"ME\",0,10,\"ME\",0,10\r\nOK\r\n",
                ),
                (
                    "AT+CMGL=4",
                    "+CMGL: 7,0,,17\r\n0000038188f8000862906000000000020041\r\nOK\r\n",
                ),
                (
                    "AT+CPMS?",
                    "\r\n+CPMS: \"ME\",0,10,\"ME\",0,10,\"ME\",0,10\r\nOK\r\n",
                ),
                (
                    "AT+CMGR=7",
                    "+CMGR: 0,,17\r\n0000038188f8000862906000000000020041\r\nOK\r\n",
                ),
                ("AT+CMGD=7", "OK\r\n"),
            ];
            let mut pending = Vec::new();
            for (expected_command, response) in commands {
                let command = loop {
                    if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=end).collect::<Vec<_>>();
                        break String::from_utf8(line)
                            .expect("AT command UTF-8")
                            .trim()
                            .to_owned();
                    }
                    let mut buffer = [0; 128];
                    let count = master.read(&mut buffer).expect("read fake AT command");
                    pending.extend_from_slice(&buffer[..count]);
                };
                assert_eq!(command, expected_command);
                if expected_command == "AT+CMGD=7" {
                    let archived: Value = serde_json::from_slice(
                        &fs::read(&archive_for_responder)
                            .expect("archive must be durable before source deletion"),
                    )
                    .expect("valid archived JSON");
                    assert_eq!(archived[0]["sender"], "888");
                    assert_eq!(archived[0]["content"], "A");
                }
                if expected_command == "AT+CMGR=7" {
                    assert_eq!(pdu, "0000038188f8000862906000000000020041");
                }
                master
                    .write_all(response.as_bytes())
                    .expect("write fake AT reply");
            }
            let _ = keep_master_open.recv_timeout(Duration::from_secs(2));
        });

        let store = proletarius_store::ArchiveStore::new(archive_path.clone());
        let input = serde_json::to_vec(&json!({
            "storage":"ME",
            "index":7,
            "expected_sms_generation":0
        }))
        .unwrap();
        let response = ingest_stored_sms_with_store(&input, actor.clone(), store).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["accepted"], true);
        assert_eq!(value["archived"], 1);
        assert_eq!(value["deleted"], 1);
        assert!(value["storage_restored"].as_bool().unwrap());
        actor.close().unwrap();
        let _ = release_master.send(());
        responder.join().unwrap();
        fs::remove_dir_all(directory).expect("remove in-project test directory");
    }

    fn finish_for_watchdog(
        state: &mut ActorState,
        result: &'static str,
        high_priority: bool,
        overflow: bool,
    ) -> CommandResult {
        let (reply, response) = mpsc::sync_channel(1);
        state.pending_command = Some(PendingCommand {
            deadline: Instant::now() + Duration::from_secs(1),
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow,
            follow_up: None,
            expected_sms_generation: None,
            high_priority,
            completion: CommandCompletion::Execute(reply),
        });
        state.finish_command(result, None);
        response.recv().unwrap().unwrap()
    }

    fn framed_bytes(state: &ActorState) -> Vec<u8> {
        state
            .frames
            .iter()
            .flat_map(|frame| frame.data.iter().copied())
            .collect()
    }

    #[test]
    fn closed_actor_rejects_io_and_close_is_idempotent() {
        let actor = test_actor().unwrap();
        actor.close().unwrap();
        assert_eq!(actor.write(vec![0x41]).unwrap_err().code, "serial_not_open");
        assert_eq!(actor.read(32, 50).unwrap_err().code, "serial_not_open");
        actor.close().unwrap();
    }

    #[test]
    fn open_and_transfer_bounds_are_checked_before_actor_queueing() {
        let actor = test_actor().unwrap();
        assert_eq!(
            actor
                .open("/dev/tty\nAT".to_owned(), 115_200)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            actor
                .open("/tmp/serial".to_owned(), 115_200)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            actor
                .open("/dev/cu.fixture".to_owned(), 0)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(actor.read(0, 20).unwrap_err().code, "invalid_request");
        assert_eq!(
            actor.read(MAX_READ_BYTES + 1, 20).unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(actor.read(16, 251).unwrap_err().code, "invalid_request");
        assert_eq!(actor.write(Vec::new()).unwrap_err().code, "invalid_request");
        assert_eq!(
            actor.write(vec![0; MAX_WRITE_BYTES + 1]).unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(
            actor.execute("AT".to_owned(), 199).unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(
            actor
                .execute("AT\nAT+CSQ".to_owned(), 1_000)
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn typed_query_parsers_match_legacy_modem_formatting() {
        assert_eq!(
            parse_imei(&["AT+CGSN".into(), "123456789012345".into(), "OK".into()]),
            "123456789012345"
        );
        assert_eq!(
            parse_firmware(&["+CGMR: v1".into(), "Modem Build 1.2".into(), "OK".into()]),
            "Modem Build 1.2"
        );
        assert_eq!(
            parse_qsimstat_inserted(&["+QSIMSTAT: 1, 1suffix".into()]),
            Some(true)
        );
        assert_eq!(parse_qsimstat_inserted(&["+QSIMSTAT: 1".into()]), None);
        assert_eq!(
            parse_cpin_inserted(&["+CPIN: NOT INSERTED".into()]),
            Some(false)
        );
        assert_eq!(parse_cpin_inserted(&["+CPIN: READY".into()]), Some(true));
        assert_eq!(
            parse_imsi(&["+CIMI: 234150123456789".into()]),
            "234150123456789"
        );
        assert_eq!(
            parse_iccid(&["+QCCID: \"8901234567890123456Ff\"".into()]),
            "8901234567890123456"
        );
        assert_eq!(parse_csq(&["+CSQ: 17,99".into()]), Some((17, -79)));
        assert_eq!(parse_csq(&["+CSQ: 99,99".into()]), Some((99, -999)));
        assert_eq!(
            parse_cops_operator(&["+COPS: 0,2,\"46011\",7".into()]),
            "46011"
        );
        assert_eq!(parse_cops_operator(&["+COPS: 0,2".into()]), "");
        for (act, expected) in [(0, "GSM"), (2, "WCDMA"), (7, "LTE"), (10, "NR")] {
            assert_eq!(
                parse_cops_act(&[format!("+COPS: 0,0,\"carrier\",{act}")]),
                expected
            );
        }
        assert_eq!(parse_cops_act(&["+COPS: 0,0,\"carrier\",99".into()]), "");
        assert_eq!(parse_cops_act(&["+COPS: malformed".into()]), "");
        assert_eq!(
            decode_ef_spn(&[0, b'C', b'M', b'C', b'C', 0xff]),
            Ok("CMCC".into())
        );
        assert_eq!(
            decode_ef_spn(&[
                0, 0x80, 0x4e, 0x2d, 0x56, 0xfd, 0x79, 0xfb, 0x52, 0xa8, 0xff
            ]),
            Ok("中国移动".into())
        );
        assert_eq!(
            decode_ef_spn(&[0, 0x81, 0x03, 0x9c, 0xad, 0x01, 0x02, 0xff]),
            Ok("中£$".into())
        );
        assert_eq!(
            decode_ef_spn(&[0, 0x82, 0x03, 0x4e, 0x00, 0xad, 0x01, 0x02, 0xff]),
            Ok("中£$".into())
        );
        assert!(decode_ef_spn(&[0, 0xff, 0xff]).is_err());
        assert!(decode_ef_spn(&[0, 0x80, 0x4e]).is_err());
        assert_eq!(
            parse_crsm_response(&["+CRSM: 144,0,\"00434D4343FF\"".into()]).unwrap(),
            [0, b'C', b'M', b'C', b'C', 0xff]
        );
        assert!(parse_crsm_response(&["+CRSM: 105,130,\"\"".into()]).is_err());
        assert_eq!(
            parse_creg(&["+CREG: 2,5,\"1A2B\",\"00AF\"".into()]),
            Some((5, "1A2B".into(), "00AF".into()))
        );
        assert_eq!(parse_creg(&["+CREG: invalid".into()]), None);
        assert_eq!(
            parse_serving_cell_lte(&[
                "+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",460,01,8401A29,132,3740,8,3,3,-95,5992,-75,-8,-50,11,44".into()
            ]),
            Some((-75, -8, 11, "FDD".into(), "LTE BAND 8".into(), 3740))
        );
        assert_eq!(
            parse_apn(&["+CGDCONT: 1,\"IP\",\"internet\"".into()]),
            "internet"
        );
        assert_eq!(parse_qims(&["+QIMS: 1".into()]), Some(1));
        assert_eq!(
            parse_qnwinfo_radio(&["+QNWINFO: \"FDD LTE\",\"46001\",\"LTE BAND 8\",3740".into()]),
            ("LTE".into(), "FDD".into(), "LTE BAND 8".into(), 3740)
        );
        assert_eq!(
            parse_smsc(&["+CSCA: \"+447870002308\",145".into()]),
            "+447870002308"
        );
        assert_eq!(
            parse_msisdn(&[
                "+CNUM: \"Own Number\",\"13800138000\",129".into(),
                "+CNUM: \"Line 1\",\"+8613900139000\",145".into()
            ]),
            "13800138000"
        );
        assert_eq!(parse_usbnet(&["+QCFG: \"usbnet\",0".into()]), Some(0));
        assert_eq!(
            parse_uac_enabled(&["+QCFG: \"usbcfg\",0x2C7C,0x0125,1,1,1,1,1,0,1".into()]),
            Some(true)
        );
        assert_eq!(
            parse_uac_enabled(&["+QCFG: \"USBCFG\",0x2C7C,0x0125,1,1,1,1,1,0,0".into()]),
            Some(false)
        );
        assert_eq!(parse_uac_enabled(&["+QCFG: \"usbcfg\",1".into()]), None);
    }

    #[test]
    fn typed_query_rejects_unlisted_operations_before_serial_io() {
        let actor = test_actor().unwrap();
        assert_eq!(
            actor.typed_query("AT+CMGS").unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(
            actor.typed_query("call_state_extra").unwrap_err().code,
            "invalid_request"
        );
    }

    #[test]
    fn call_command_allowlist_builds_only_valid_voice_commands() {
        assert_eq!(
            call_command_spec("dial", "+85256600731").unwrap(),
            ("dial", "ATD+85256600731;".into(), 10_000)
        );
        assert_eq!(
            call_command_spec("answer", "").unwrap(),
            ("answer", "ATA".into(), 10_000)
        );
        assert_eq!(
            call_command_spec("hangup", "").unwrap(),
            ("hangup", "ATH".into(), 10_000)
        );
        assert_eq!(
            call_command_spec("dtmf", "#").unwrap(),
            ("dtmf", "AT+VTS=#".into(), 5_000)
        );
        for (action, value) in [
            ("dial", ""),
            ("dial", "123;ATH"),
            ("dial", "+1+2"),
            ("dial", "123456789012345678901234567890123"),
            ("answer", "1"),
            ("hangup", "ATH"),
            ("dtmf", "12"),
            ("dtmf", "A"),
            ("arbitrary", "AT+CFUN=0"),
        ] {
            assert!(
                call_command_spec(action, value).is_err(),
                "{action}: {value}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn fixed_voice_commands_run_through_the_rust_serial_owner() {
        use serialport::SerialPort as _;
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().expect("pseudo-terminal");
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let ports = Arc::new(Mutex::new(VecDeque::from([
            Box::new(slave) as Box<dyn serialport::SerialPort>
        ])));
        let opener_ports = Arc::clone(&ports);
        let actor = test_actor_with_opener(move |_, _| {
            opener_ports
                .lock()
                .expect("pseudo-terminal holder lock")
                .pop_front()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let (release_master, keep_master_open) = mpsc::channel();
        let responder = thread::spawn(move || {
            let mut pending = Vec::new();
            for expected in ["ATD+85256600731;", "ATA", "ATH", "AT+VTS=#"] {
                let command = loop {
                    if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=end).collect::<Vec<_>>();
                        break String::from_utf8(line)
                            .expect("AT command UTF-8")
                            .trim()
                            .to_owned();
                    }
                    let mut buffer = [0; 128];
                    let count = master.read(&mut buffer).expect("read fake AT command");
                    pending.extend_from_slice(&buffer[..count]);
                };
                assert_eq!(command, expected);
                master
                    .write_all(b"\r\nOK\r\n")
                    .expect("write fake AT response");
            }
            let _ = keep_master_open.recv_timeout(Duration::from_secs(2));
        });

        for (action, value) in [
            ("dial", "+85256600731"),
            ("answer", ""),
            ("hangup", ""),
            ("dtmf", "#"),
        ] {
            let result = actor.execute_call_command(action, value).unwrap();
            assert_eq!(result.result, "ok", "{action}");
        }
        actor.close().unwrap();
        let _ = release_master.send(());
        responder.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn typed_query_runs_fixed_at_command_through_the_rust_serial_owner() {
        use serialport::SerialPort as _;
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().expect("pseudo-terminal");
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let ports = Arc::new(Mutex::new(VecDeque::from([
            Box::new(slave) as Box<dyn serialport::SerialPort>
        ])));
        let opener_ports = Arc::clone(&ports);
        let actor = test_actor_with_opener(move |_, _| {
            opener_ports
                .lock()
                .expect("pseudo-terminal holder lock")
                .pop_front()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let (release_master, keep_master_open) = mpsc::channel();
        let responder = thread::spawn(move || {
            let mut pending = Vec::new();
            for (expected_command, response) in [
                ("AT+CSQ", b"\r\n+CSQ: 17,99\r\nOK\r\n".as_slice()),
                (
                    "AT+CREG?",
                    b"\r\n+CREG: 2,5,\"1A2B\",\"00AF\"\r\nOK\r\n".as_slice(),
                ),
                ("AT+COPS=3,2", b"\r\nERROR\r\n".as_slice()),
                (
                    "AT+COPS?",
                    b"\r\n+COPS: 0,2,\"46011\",7\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+COPS?",
                    b"\r\n+COPS: 0,0,\"carrier\",10\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+CRSM=176,28486,0,0,17",
                    b"\r\n+CRSM: 144,0,\"00804E2D56FD79FB52A8FFFFFF\"\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+QCFG=\"usbcfg\"?",
                    b"\r\n+QCFG: \"usbcfg\",0x2C7C,0x0125,1,1,1,1,1,0,1\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+CLCC",
                    b"\r\n+CLCC: 1,1,4,0,0,\"+8613800000000\"\r\nOK\r\n".as_slice(),
                ),
            ] {
                let command = loop {
                    if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=end).collect::<Vec<_>>();
                        break String::from_utf8(line)
                            .expect("AT command UTF-8")
                            .trim()
                            .to_owned();
                    }
                    let mut buffer = [0; 128];
                    let count = master.read(&mut buffer).expect("read fake AT command");
                    pending.extend_from_slice(&buffer[..count]);
                };
                assert_eq!(command, expected_command);
                master.write_all(response).expect("write fake AT response");
            }
            let _ = keep_master_open.recv_timeout(Duration::from_secs(2));
        });

        let result = actor.typed_query("csq").unwrap();
        assert_eq!(result.rssi, Some(17));
        assert_eq!(result.dbm, Some(-79));
        let registration = actor.typed_query("registration").unwrap();
        assert_eq!(registration.registration_valid, Some(true));
        assert_eq!(registration.reg_status, Some(5));
        assert_eq!(registration.lac.as_deref(), Some("1A2B"));
        assert_eq!(registration.cell_id.as_deref(), Some("00AF"));
        let operator = actor.typed_query("operator").unwrap();
        assert_eq!(operator.string_value.as_deref(), Some("中国电信"));
        let fallback = actor.typed_query("network_mode_fallback").unwrap();
        assert_eq!(fallback.string_value.as_deref(), Some("NR"));
        let spn = actor.typed_query("native_spn").unwrap();
        assert_eq!(spn.string_value.as_deref(), Some("中国移动"));
        let voice = actor.typed_query("voice_enabled").unwrap();
        assert_eq!(voice.bool_value, Some(true));
        let call = actor.typed_query("call_state").unwrap();
        let call_state = call.call_state.expect("projected CLCC state");
        assert_eq!(call_state.state, "incoming");
        assert_eq!(call_state.number, "+8613800000000");
        assert!(call_state.incoming);
        assert!(!call_state.active);
        actor.close().unwrap();
        let _ = release_master.send(());
        responder.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn device_info_snapshot_aggregates_legacy_order_and_values() {
        use serialport::SerialPort as _;
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().expect("pseudo-terminal");
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let ports = Arc::new(Mutex::new(VecDeque::from([
            Box::new(slave) as Box<dyn serialport::SerialPort>
        ])));
        let opener_ports = Arc::clone(&ports);
        let actor = test_actor_with_opener(move |_, _| {
            opener_ports
                .lock()
                .expect("pseudo-terminal holder lock")
                .pop_front()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();
        actor.set_cellular_suppressed(false);

        let (release_master, keep_master_open) = mpsc::channel();
        let responder = thread::spawn(move || {
            let mut pending = Vec::new();
            for (expected_command, response) in [
                ("AT+CGSN", b"\r\n123456789012345\r\nOK\r\n".as_slice()),
                ("AT+CGMR", b"\r\nbuild-1\r\nOK\r\n".as_slice()),
                ("AT+QSIMSTAT?", b"\r\n+QSIMSTAT: 1,1\r\nOK\r\n".as_slice()),
                ("AT+CPIN?", b"\r\n+CPIN: READY\r\nOK\r\n".as_slice()),
                ("AT+CIMI", b"\r\n234150123456789\r\nOK\r\n".as_slice()),
                ("AT+CNUM", b"\r\n+CNUM: \"\",\"+441234567890\",145\r\nOK\r\n".as_slice()),
                ("AT+QCCID", b"\r\n+QCCID: 8901234567890123456F\r\nOK\r\n".as_slice()),
                ("AT+COPS=3,2", b"\r\nERROR\r\n".as_slice()),
                ("AT+COPS?", b"\r\n+COPS: 0,2,\"23415\",7\r\nOK\r\n".as_slice()),
                ("AT+CREG?", b"\r\n+CREG: 2,1,\"1A2B\",\"00AF\"\r\nOK\r\n".as_slice()),
                ("AT+CSQ", b"\r\n+CSQ: 17,99\r\nOK\r\n".as_slice()),
                ("AT+QENG=\"servingcell\"", b"\r\n+QENG: \"servingcell\",\"NOCONN\",\"LTE\",\"FDD\",460,01,8401A29,132,3740,8,3,3,-95,5992,-75,-8,-50,11,44\r\nOK\r\n".as_slice()),
                ("AT+CGDCONT?", b"\r\n+CGDCONT: 1,\"IP\",\"internet\"\r\nOK\r\n".as_slice()),
                ("AT+QIMS?", b"\r\n+QIMS: 1\r\nOK\r\n".as_slice()),
                ("AT+QNWINFO", b"\r\n+QNWINFO: \"FDD LTE\",\"23415\",\"LTE BAND 8\",3740\r\nOK\r\n".as_slice()),
                ("AT+QCFG=\"usbnet\"?", b"\r\n+QCFG: \"usbnet\",0\r\nOK\r\n".as_slice()),
            ] {
                let command = loop {
                    if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=end).collect::<Vec<_>>();
                        break String::from_utf8(line)
                            .expect("AT command UTF-8")
                            .trim()
                            .to_owned();
                    }
                    let mut buffer = [0; 128];
                    let count = master.read(&mut buffer).expect("read fake AT command");
                    pending.extend_from_slice(&buffer[..count]);
                };
                assert_eq!(command, expected_command);
                master.write_all(response).expect("write fake AT response");
            }
            let _ = keep_master_open.recv_timeout(Duration::from_secs(2));
        });

        let snapshot = actor.device_info_snapshot(false);
        assert_eq!(snapshot.imei, "123456789012345");
        assert_eq!(snapshot.firmware, "build-1");
        assert!(snapshot.sim_inserted);
        assert_eq!(snapshot.imsi, "234150123456789");
        assert_eq!(snapshot.phone_number, "+441234567890");
        assert_eq!(snapshot.iccid, "8901234567890123456");
        assert_eq!(snapshot.operator, "23415");
        assert_eq!(snapshot.reg_status, 1);
        assert_eq!(snapshot.reg_status_text, "已注册(本地)");
        assert_eq!(
            (snapshot.lac.as_str(), snapshot.cell_id.as_str()),
            ("1A2B", "00AF")
        );
        assert_eq!(snapshot.signal_dbm, -79);
        assert_eq!((snapshot.signal_rsrp, snapshot.signal_rsrq), (-75, -8));
        assert_eq!(snapshot.apn, "internet");
        assert_eq!(snapshot.ims_status, 1);
        assert_eq!(
            (
                snapshot.network_mode.as_str(),
                snapshot.network_duplex.as_str()
            ),
            ("LTE", "FDD")
        );
        assert_eq!(snapshot.usbnet_mode, 0);
        assert!(!snapshot.cellular_suppressed);
        actor.close().unwrap();
        let _ = release_master.send(());
        responder.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn device_info_snapshot_skips_cellular_probes_when_suppressed() {
        use serialport::SerialPort as _;
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().expect("pseudo-terminal");
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let ports = Arc::new(Mutex::new(VecDeque::from([
            Box::new(slave) as Box<dyn serialport::SerialPort>
        ])));
        let opener_ports = Arc::clone(&ports);
        let actor = test_actor_with_opener(move |_, _| {
            opener_ports
                .lock()
                .expect("pseudo-terminal holder lock")
                .pop_front()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();
        actor.set_cellular_suppressed(false);

        let (release_master, keep_master_open) = mpsc::channel();
        let responder = thread::spawn(move || {
            let mut pending = Vec::new();
            for (expected_command, response) in [
                ("AT+CGSN", b"\r\n123456789012345\r\nOK\r\n".as_slice()),
                ("AT+CGMR", b"\r\nbuild-1\r\nOK\r\n".as_slice()),
                ("AT+QSIMSTAT?", b"\r\n+QSIMSTAT: 1,1\r\nOK\r\n".as_slice()),
                ("AT+CPIN?", b"\r\n+CPIN: READY\r\nOK\r\n".as_slice()),
                ("AT+CIMI", b"\r\n234150123456789\r\nOK\r\n".as_slice()),
                (
                    "AT+CNUM",
                    b"\r\n+CNUM: \"\",\"+441234567890\",145\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+QCCID",
                    b"\r\n+QCCID: 8901234567890123456F\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+CGDCONT?",
                    b"\r\n+CGDCONT: 1,\"IP\",\"internet\"\r\nOK\r\n".as_slice(),
                ),
                (
                    "AT+QCFG=\"usbnet\"?",
                    b"\r\n+QCFG: \"usbnet\",0\r\nOK\r\n".as_slice(),
                ),
            ] {
                let command = loop {
                    if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=end).collect::<Vec<_>>();
                        break String::from_utf8(line)
                            .expect("AT command UTF-8")
                            .trim()
                            .to_owned();
                    }
                    let mut buffer = [0; 128];
                    let count = master.read(&mut buffer).expect("read fake AT command");
                    pending.extend_from_slice(&buffer[..count]);
                };
                assert_eq!(command, expected_command);
                master.write_all(response).expect("write fake AT response");
            }
            let mut extra = [0; 64];
            if let Ok(count) = master.read(&mut extra) {
                assert_eq!(
                    &extra[..count],
                    b"",
                    "unexpected cellular command after suppression"
                );
            }
            let _ = keep_master_open.recv_timeout(Duration::from_secs(2));
        });

        let snapshot = actor.device_info_snapshot(true);
        assert!(snapshot.cellular_suppressed);
        assert_eq!(snapshot.operator, "");
        assert_eq!(snapshot.signal_dbm, -999);
        assert_eq!(snapshot.reg_status_text, "飞行模式 · 蜂窝射频关闭");
        assert_eq!(snapshot.network_mode, "飞行模式");
        assert_eq!(snapshot.apn, "internet");
        actor.close().unwrap();
        let _ = release_master.send(());
        responder.join().unwrap();
    }

    #[test]
    fn serial_framer_preserves_fragmented_and_coalesced_crlf_lines() {
        let mut state = test_state();
        state.frame_input(b"\r\n+CMTI: 1,").unwrap();
        assert_eq!(framed_bytes(&state), b"\r\n");
        assert_eq!(state.line_buffer, b"+CMTI: 1,");

        state.frame_input(b"2\r\n+CSQ: 17\r\nOK\r\n").unwrap();
        assert_eq!(
            framed_bytes(&state),
            b"\r\n+CMTI: 1,2\r\n+CSQ: 17\r\nOK\r\n"
        );
        let classifications: Vec<_> = state
            .frames
            .iter()
            .map(|frame| (frame.data.as_slice(), frame.is_urc, frame.pure_async_urc))
            .collect();
        assert_eq!(classifications[0], (b"\r\n".as_slice(), false, false));
        assert_eq!(
            classifications[1],
            (b"+CMTI: 1,2\r\n".as_slice(), true, true)
        );
        assert_eq!(
            classifications[2],
            (b"+CSQ: 17\r\n".as_slice(), true, false)
        );
        assert_eq!(classifications[3], (b"OK\r\n".as_slice(), false, false));
        assert!(state.line_buffer.is_empty());
    }

    #[test]
    fn serial_framer_recognizes_prompt_without_a_newline() {
        let mut state = test_state();
        state.frame_input(b"\r\n> ").unwrap();
        assert_eq!(framed_bytes(&state), b"\r\n> ");
        assert!(state.line_buffer.is_empty());
    }

    #[test]
    fn command_timeout_watchdog_matches_priority_reset_and_failure_semantics() {
        let mut state = test_state();
        for streak in 1..=COMMAND_TIMEOUT_WATCHDOG_THRESHOLD {
            let result = finish_for_watchdog(&mut state, "timeout", false, false);
            assert_eq!(result.timeout_streak, streak);
            assert_eq!(result.timeout_threshold, COMMAND_TIMEOUT_WATCHDOG_THRESHOLD);
            assert_eq!(
                result.watchdog_tripped,
                streak >= COMMAND_TIMEOUT_WATCHDOG_THRESHOLD
            );
        }

        // High priority timeouts neither count nor clear normal-command
        // failures, matching the previous Go manager's exclusion rule.
        state.timeout_streak = 2;
        let high_timeout = finish_for_watchdog(&mut state, "timeout", true, false);
        assert_eq!(high_timeout.timeout_streak, 2);
        assert!(!high_timeout.watchdog_tripped);
        assert_eq!(
            finish_for_watchdog(&mut state, "timeout", false, false).timeout_streak,
            3
        );

        for result_kind in ["ok", "error", "prompt"] {
            let result = finish_for_watchdog(&mut state, result_kind, false, false);
            assert_eq!(result.timeout_streak, 0, "{result_kind} must reset streak");
        }

        state.timeout_streak = 3;
        let generation_changed =
            finish_for_watchdog(&mut state, "generation_changed", false, false);
        assert_eq!(generation_changed.timeout_streak, 3);
        let overflow = finish_for_watchdog(&mut state, "timeout", false, true);
        assert_eq!(overflow.result, "overflow");
        assert_eq!(overflow.timeout_streak, 3);
        assert!(!overflow.watchdog_tripped);
    }

    #[test]
    fn serial_frames_match_legacy_urc_and_pure_async_classification() {
        for line in [
            b"+CUSD: 1,\"balance\",15".as_slice(),
            b"+CMTI: \"ME\",7",
            b"RING",
            b"+CLIP: \"+15551234567\",145",
            b"+QSIMSTAT: 1,1",
            b"+QSTKURC: 0,0",
            b"+QPCMV: 1",
        ] {
            assert_eq!(classify_frame(line), (true, true), "{line:?}");
        }
        for line in [
            b"+CSQ: 12,99".as_slice(),
            b"RDY",
            b"SMS Ready",
            b"Call Ready",
            b"NORMAL POWER DOWN",
            b"NO CARRIER",
            b"BUSY",
            b"NO ANSWER",
            b"^FOO: 1",
            b"$FOO,1",
        ] {
            assert_eq!(classify_frame(line), (true, false), "{line:?}");
        }
        for line in [
            b"+CSIM: 144,0".as_slice(),
            b"+CGLA: 144,0",
            b"+CCHO: 1",
            b"+CMGR: 0,,23",
            b"+CMGS: 12",
            b"+QENG: 0",
            b"CONNECT",
            b"OK",
            b"> ",
            b" \r\n ",
        ] {
            assert_eq!(classify_frame(line), (false, false), "{line:?}");
        }
    }

    #[test]
    fn sim_generation_matches_switch_rdy_and_status_invalidation_rules() {
        let mut state = test_state();
        state.frame_input(b"RDY\r\n").unwrap();
        assert_eq!(state.sms_generation, 1);
        state.frame_input(b"+QSIMSTAT: 1,1\r\n").unwrap();
        assert_eq!(state.sms_generation, 2);
        state.frame_input(b"+CPIN: READY\r\n").unwrap();
        assert_eq!(state.sms_generation, 4);
        state.frame_input(b"+CPIN: SIM PIN\r\n").unwrap();
        assert_eq!(state.sms_generation, 5);
    }

    #[test]
    fn sim_event_actions_match_existing_ready_and_status_notifications() {
        assert_eq!(
            SimEvent::Ready.actions(),
            SimEventActions {
                notify_ready: true,
                notify_status: false,
            }
        );
        assert_eq!(
            SimEvent::PinState {
                state: "READY".to_owned()
            }
            .actions(),
            SimEventActions {
                notify_ready: true,
                notify_status: true,
            }
        );
        assert_eq!(
            SimEvent::PinState {
                state: "SIM PIN".to_owned()
            }
            .actions(),
            SimEventActions {
                notify_ready: false,
                notify_status: true,
            }
        );
        assert_eq!(
            SimEvent::Status { inserted: None }.actions(),
            SimEventActions {
                notify_ready: false,
                notify_status: true,
            }
        );

        let frame = SerialFrame::new(b"+CPIN: READY\r\n".to_vec());
        assert_eq!(
            frame.sim_actions,
            Some(SimEventActions {
                notify_ready: true,
                notify_status: true,
            })
        );
    }

    #[test]
    fn serial_frame_parses_valid_cmti_storage_and_index_without_guessing_malformed_lines() {
        let frame = SerialFrame::new(b"\r\n+CMTI: \"ME\", 12,ignored\r\n".to_vec());
        let event = frame.sms_event.expect("valid CMTI event");
        assert_eq!(event.storage, "ME");
        assert_eq!(event.index, "12");

        for malformed in [
            b"+CMTI: ,7".as_slice(),
            b"+CMTI: \"ME\", ",
            b"+CMTI: \"ME\"",
            b"+cmti: \"ME\",7",
        ] {
            assert_eq!(parse_sms_storage_event(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn serial_frame_parses_ussd_csv_without_splitting_quoted_commas() {
        let event = parse_ussd_event(b"+CUSD: 1,\"first, second\",15").expect("valid USSD event");
        assert_eq!(event.status, 1);
        assert_eq!(event.text.as_deref(), Some("first, second"));
        assert_eq!(event.dcs, Some(15));
        assert_eq!(event.decoded_text.as_deref(), Some("first, second"));

        let spaced = parse_ussd_event(b"+CUSD: 1, \" keep spaces \" , 15")
            .expect("spaces around quoted fields are valid");
        assert_eq!(spaced.text.as_deref(), Some(" keep spaces "));

        let escaped =
            parse_ussd_event(b"+CUSD: 0,\"say \\\"hello\\\"\",72").expect("escaped quote is valid");
        assert_eq!(escaped.text.as_deref(), Some("say \"hello\""));
        assert_eq!(escaped.dcs, Some(72));

        let doubled =
            parse_ussd_event(b"+CUSD: 0,\"say \"\"hello\"\"\",72").expect("doubled quote is valid");
        assert_eq!(doubled.text.as_deref(), Some("say \"hello\""));

        assert_eq!(parse_ussd_event(b"+CUSD: nope,\"text\",15"), None);
        assert_eq!(parse_ussd_event(b"+CUSD: 1,\"unterminated,15"), None);
        assert_eq!(
            parse_ussd_event(b"+CUSD: 1"),
            Some(UssdEvent {
                status: 1,
                text: None,
                dcs: None,
                decoded_text: None,
            })
        );
    }

    #[test]
    fn serial_frames_carry_the_generation_observed_when_each_event_arrived() {
        let mut state = test_state();
        state
            .frame_input(b"RDY\r\n+QSIMSTAT: 1,0\r\n+CPIN: READY\r\n")
            .unwrap();

        let frames: Vec<_> = (0..3)
            .map(|_| {
                let frame = state.take_frame(MAX_READ_BYTES).unwrap();
                (
                    String::from_utf8(frame.data).unwrap().trim().to_owned(),
                    frame.sms_generation,
                    frame.sim_event,
                )
            })
            .collect();
        assert_eq!(
            frames
                .iter()
                .map(|(line, generation, _)| (line.clone(), *generation))
                .collect::<Vec<_>>(),
            [
                ("RDY".to_owned(), 1),
                ("+QSIMSTAT: 1,0".to_owned(), 2),
                ("+CPIN: READY".to_owned(), 4)
            ]
        );
        assert!(matches!(frames[0].2, Some(SimEvent::Ready)));
        assert!(matches!(
            frames[1].2,
            Some(SimEvent::Status {
                inserted: Some(false)
            })
        ));
        assert!(matches!(frames[2].2, Some(SimEvent::PinState { ref state }) if state == "READY"));
        assert_eq!(state.sms_generation, 4);
    }

    #[cfg(unix)]
    #[test]
    fn reopening_serial_handle_resets_sms_generation_for_new_manager() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut first_master, first_slave) = serialport::TTYPort::pair().unwrap();
        let (mut second_master, second_slave) = serialport::TTYPort::pair().unwrap();
        first_master.set_timeout(Duration::from_secs(1)).unwrap();
        second_master.set_timeout(Duration::from_secs(1)).unwrap();
        let first_path = first_slave.name().expect("first pseudo-terminal path");
        let second_path = second_slave.name().expect("second pseudo-terminal path");
        let mut queued_ports: VecDeque<Box<dyn serialport::SerialPort>> = VecDeque::new();
        queued_ports.push_back(Box::new(first_slave));
        queued_ports.push_back(Box::new(second_slave));
        let ports: Arc<Mutex<VecDeque<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(queued_ports));
        let opener_ports = Arc::clone(&ports);
        let actor = test_actor_with_opener(move |_, _| {
            opener_ports
                .lock()
                .expect("pseudo-terminal holder lock")
                .pop_front()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();

        actor.open(first_path, 115_200).unwrap();
        first_master.write_all(b"+QSIMSTAT: 1,0\r\n").unwrap();
        let event = actor.read(128, 100).unwrap();
        assert_eq!(event.data, b"+QSIMSTAT: 1,0\r\n");
        actor.close().unwrap();

        actor.open(second_path, 115_200).unwrap();
        let command_actor = actor.clone();
        let command = std::thread::spawn(move || {
            command_actor.execute_scheduled(
                "AT+CMGS=2".to_owned(),
                1_000,
                true,
                Some(b"0011\x1a".to_vec()),
                Some(0),
            )
        });
        let mut sent = [0; 64];
        let count = second_master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CMGS=2\r\n");
        second_master.write_all(b"> ").unwrap();
        let count = second_master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"0011\x1a");
        second_master.write_all(b"+CMGS: 8\r\nOK\r\n").unwrap();
        assert_eq!(command.join().unwrap().unwrap().result, "ok");
        actor.close().unwrap();
    }

    #[test]
    fn serial_framer_rejects_an_unterminated_line_over_its_budget() {
        let mut state = test_state();
        let oversized = vec![b'A'; MAX_READ_BYTES + 1];
        assert_eq!(
            state.frame_input(&oversized).unwrap_err().code,
            "serial_input_overflow"
        );

        let mut terminated = vec![b'A'; MAX_READ_BYTES];
        terminated.push(b'\n');
        assert_eq!(
            test_state().frame_input(&terminated).unwrap_err().code,
            "serial_input_overflow"
        );
    }

    #[test]
    fn stored_sms_storage_and_identity_parsers_fail_closed() {
        assert_eq!(
            first_sms_storage(&["+CPMS: \"ME\",1,50,\"ME\",1,50,\"ME\",1,50".into()]).as_deref(),
            Some("ME")
        );
        assert_eq!(
            first_sms_storage(&["+CPMS: \"SM\",1,50".into()]).as_deref(),
            Some("SM")
        );
        assert_eq!(first_sms_storage(&["OK".into()]), None);
        let stores = sms_storages(&["+CPMS: \"ME\",1,50,\"SM\",2,100,\"ME\",1,50".into()]).unwrap();
        assert_eq!(
            stores,
            vec!["ME".to_owned(), "SM".to_owned(), "ME".to_owned()]
        );
        assert_eq!(
            set_sms_storage_command(&stores).as_deref(),
            Some("AT+CPMS=\"ME\",\"SM\",\"ME\"")
        );
        assert_eq!(sms_storages(&["+CPMS: 1,50,1,50,1,50".into()]), None);
        assert_eq!(normalize_sms_storage("ME;AT+CMGD=1"), None);
        assert!(valid_sms_pdu("001122"));
        assert!(!valid_sms_pdu("00112Z"));
        assert!(!valid_sms_pdu("0"));
    }

    #[test]
    fn archived_sms_cleanup_retains_on_storage_generation_or_pdu_mismatch() {
        let cases = [
            (
                StoredSmsDeleteStep::VerifyStorage,
                4,
                4,
                "ok",
                vec!["+CPMS: \"SM\",1,50".to_owned()],
                "storage_not_me",
            ),
            (
                StoredSmsDeleteStep::VerifyReadback,
                3,
                4,
                "ok",
                vec!["+CMGR: 0,,2".to_owned(), "001122".to_owned()],
                "generation_changed",
            ),
            (
                StoredSmsDeleteStep::VerifyReadback,
                4,
                4,
                "ok",
                vec!["+CMGR: 0,,2".to_owned(), "001124".to_owned()],
                "message_changed",
            ),
            (
                StoredSmsDeleteStep::Delete,
                3,
                4,
                "ok",
                Vec::new(),
                "delete_outcome_uncertain",
            ),
            (
                StoredSmsDeleteStep::Delete,
                4,
                4,
                "error",
                Vec::new(),
                "delete_outcome_uncertain",
            ),
        ];
        for (step, actor_generation, expected_generation, command_result, lines, expected_reason) in
            cases
        {
            let mut state = test_state();
            state.sms_generation = actor_generation;
            let (reply, result) = mpsc::sync_channel(1);
            state.advance_stored_sms_delete(
                StoredSmsDeleteTransaction {
                    index: 7,
                    pdu: "001122".into(),
                    expected_sms_generation: expected_generation,
                    receive_fence: None,
                    step,
                    reply,
                },
                command_result,
                &lines,
            );
            let result = result.recv().unwrap().unwrap();
            assert!(!result.deleted);
            assert_eq!(result.reason, expected_reason);
            assert!(state.pending_command.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn archived_sms_cleanup_is_one_actor_transaction_and_deletes_only_after_checks() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let delete_actor = actor.clone();
        let delete =
            std::thread::spawn(move || delete_actor.delete_archived_sms(7, "001122".into(), 0));
        let mut command = [0; 10];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CPMS?\r\n");

        // A high-priority command arriving mid-transaction must wait until
        // the storage recheck, exact PDU comparison, and CMGD decision finish.
        let queued_actor = actor.clone();
        let queued = std::thread::spawn(move || {
            queued_actor.execute_scheduled("AT+HIGH".into(), 1_000, true, None, None)
        });
        master
            .write_all(b"+CPMS: \"ME\",1,50,\"ME\",1,50,\"ME\",1,50\r\nOK\r\n")
            .unwrap();

        let mut command = [0; 11];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGR=7\r\n");
        master
            .write_all(b"+CMGR: 0,,2\r\n001122\r\nOK\r\n")
            .unwrap();

        let mut command = [0; 11];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGD=7\r\n");
        master.write_all(b"OK\r\n").unwrap();
        let result = delete.join().unwrap().unwrap();
        assert!(result.deleted);
        assert_eq!(result.reason, "deleted");

        let mut command = [0; 9];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+HIGH\r\n");
        master.write_all(b"OK\r\n").unwrap();
        assert_eq!(queued.join().unwrap().unwrap().result, "ok");
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stored_sms_read_restores_selection_after_unconfirmed_switch() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let read_actor = actor.clone();
        let read =
            std::thread::spawn(move || read_actor.read_stored_sms(Some("SM".into()), Some(7), 0));
        let read_command = |master: &mut serialport::TTYPort, expected: &[u8]| {
            let mut command = vec![0; expected.len()];
            master.read_exact(&mut command).unwrap();
            assert_eq!(command, expected);
        };

        read_command(&mut master, b"AT+CPMS?\r\n");
        let queued_actor = actor.clone();
        let queued = std::thread::spawn(move || {
            queued_actor.execute_scheduled("AT+HIGH".into(), 1_000, true, None, None)
        });
        master
            .write_all(b"+CPMS: \"ME\",1,50,\"ME\",1,50,\"ME\",1,50\r\nOK\r\n")
            .unwrap();
        read_command(&mut master, b"AT+CPMS=\"SM\",\"SM\",\"SM\"\r\n");
        master.write_all(b"ERROR\r\n").unwrap();
        read_command(&mut master, b"AT+CPMS=\"ME\",\"ME\",\"ME\"\r\n");
        master.write_all(b"OK\r\n").unwrap();
        read_command(&mut master, b"AT+CPMS?\r\n");
        master
            .write_all(b"+CPMS: \"ME\",1,50,\"ME\",1,50,\"ME\",1,50\r\nOK\r\n")
            .unwrap();

        assert_eq!(
            read.join().unwrap().unwrap_err().code,
            "sms_storage_select_failed"
        );
        read_command(&mut master, b"AT+HIGH\r\n");
        master.write_all(b"OK\r\n").unwrap();
        assert_eq!(queued.join().unwrap().unwrap().result, "ok");
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stored_sms_read_selects_reads_and_restores_storage_without_interleaving() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("no pseudo-terminal remains"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let read_actor = actor.clone();
        let read =
            std::thread::spawn(move || read_actor.read_stored_sms(Some("SM".into()), Some(7), 0));
        let read_command = |master: &mut serialport::TTYPort, expected: &[u8]| {
            let mut command = vec![0; expected.len()];
            master.read_exact(&mut command).unwrap();
            assert_eq!(command, expected);
        };

        read_command(&mut master, b"AT+CPMS?\r\n");
        let queued_actor = actor.clone();
        let queued = std::thread::spawn(move || {
            queued_actor.execute_scheduled("AT+HIGH".into(), 1_000, true, None, None)
        });
        master
            .write_all(b"+CPMS: \"ME\",1,50,\"ME\",1,50,\"ME\",1,50\r\nOK\r\n")
            .unwrap();

        read_command(&mut master, b"AT+CPMS=\"SM\",\"SM\",\"SM\"\r\n");
        master
            .write_all(b"+CPMS: 1,100,1,100,1,100\r\nOK\r\n")
            .unwrap();
        read_command(&mut master, b"AT+CPMS?\r\n");
        master
            .write_all(b"+CPMS: \"SM\",1,100,\"SM\",1,100,\"SM\",1,100\r\nOK\r\n")
            .unwrap();
        read_command(&mut master, b"AT+CMGR=7\r\n");
        master
            .write_all(b"+CMGR: 0,,2\r\n001122\r\nOK\r\n")
            .unwrap();
        read_command(&mut master, b"AT+CPMS=\"ME\",\"ME\",\"ME\"\r\n");
        master
            .write_all(b"+CPMS: 1,50,1,50,1,50\r\nOK\r\n")
            .unwrap();
        read_command(&mut master, b"AT+CPMS?\r\n");
        master
            .write_all(b"+CPMS: \"ME\",1,50,\"ME\",1,50,\"ME\",1,50\r\nOK\r\n")
            .unwrap();

        let result = read.join().unwrap().unwrap();
        assert_eq!(result.storage, "SM");
        assert_eq!(result.lines, ["+CMGR: 0,,2", "001122"]);
        assert!(result.storage_restored);
        read_command(&mut master, b"AT+HIGH\r\n");
        master.write_all(b"OK\r\n").unwrap();
        assert_eq!(queued.join().unwrap().unwrap().result, "ok");
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_open_releases_a_late_serial_handle() {
        use serialport::SerialPort as _;
        use std::sync::{Arc, Mutex};

        let (_master, slave) = serialport::TTYPort::pair().unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let holder: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener_holder = Arc::clone(&holder);
        let opener: PortOpener = Box::new(move |_, _| {
            opener_holder
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        });
        let mut state = test_state();
        let (reply, result) = mpsc::sync_channel(1);
        drop(result);

        state.process(
            Command::Open {
                path,
                baud_rate: 115_200,
                initialization_required: false,
                reply,
            },
            &opener,
        );

        assert!(state.port.is_none());
        assert!(state.current_path.is_none());
        assert_eq!(state.current_baud, 0);
        state.finish();
    }

    #[cfg(unix)]
    #[test]
    fn production_actor_reads_and_writes_one_pseudo_terminal_without_a_device() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(1)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();
        actor.write(b"AT+CSQ\r\n".to_vec()).unwrap();

        let mut sent = [0; 32];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CSQ\r\n");

        master.write_all(b"\r\nOK\r\n").unwrap();
        assert_eq!(actor.read(32, 250).unwrap().data, b"\r\n");
        assert_eq!(actor.read(32, 250).unwrap().data, b"OK\r\n");
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_command_transaction_matches_reply_and_keeps_urcs_on_the_event_stream() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master
            .set_timeout(Duration::from_secs(1))
            .expect("set pseudo-terminal read timeout");
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let command_actor = actor.clone();
        let command = std::thread::spawn(move || command_actor.execute("AT+CSQ".to_owned(), 1_000));
        let mut sent = [0; 32];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CSQ\r\n");
        assert_eq!(
            actor.write(vec![0x1b]).unwrap_err().code,
            "serial_command_busy"
        );

        master
            .write_all(b"+CMTI: \"ME\",7\r\n+CSQ: 17,99\r\nOK\r\n")
            .unwrap();
        let result = command.join().expect("command worker").expect("AT reply");
        assert_eq!(result.result, "ok");
        assert_eq!(result.lines, ["+CSQ: 17,99"]);

        let asynchronous = actor.read(128, 100).expect("async URC");
        assert_eq!(asynchronous.data, b"+CMTI: \"ME\",7\r\n");
        assert!(asynchronous.is_urc);
        assert!(asynchronous.pure_async_urc);
        let response_urc = actor.read(128, 100).expect("response-style URC");
        assert_eq!(response_urc.data, b"+CSQ: 17,99\r\n");
        assert!(response_urc.is_urc);
        assert!(!response_urc.pure_async_urc);
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_command_transaction_returns_prompt_without_waiting_for_a_line_ending() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(1)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let command_actor = actor.clone();
        let command =
            std::thread::spawn(move || command_actor.execute("AT+CMGS".to_owned(), 1_000));
        let mut sent = [0; 32];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CMGS\r\n");
        master.write_all(b"\r\n> ").unwrap();

        let result = command
            .join()
            .expect("command worker")
            .expect("prompt reply");
        assert_eq!(result.result, "prompt");
        assert!(result.lines.is_empty());
        assert_eq!(actor.write(vec![0x1b]).unwrap(), 1);
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_sms_submit_owns_pdu_mode_encoding_prompt_and_final_acceptance() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let submit_actor = actor.clone();
        let submit = std::thread::spawn(move || {
            submit_actor.submit_sms("888".into(), "BAL".into(), "modem".into(), Some(0))
        });
        let mut command = [0; 11];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGF=0\r\n");
        master.write_all(b"OK\r\n").unwrap();

        let mut command = [0; 12];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGS=12\r\n");
        master.write_all(b"> ").unwrap();

        let mut payload = [0; 27];
        master.read_exact(&mut payload).unwrap();
        assert_eq!(&payload, b"000101038188F8000003C22013\x1a");
        master.write_all(b"+CMGS: 42\r\nOK\r\n").unwrap();
        let result = submit.join().unwrap().unwrap();
        assert!(result.complete);
        assert_eq!(result.accepted_segments, 1);
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_sms_submit_mode_failure_and_missing_acceptance_remain_unconfirmed() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let failed_mode_actor = actor.clone();
        let failed_mode = std::thread::spawn(move || {
            failed_mode_actor.submit_sms("888".into(), "BAL".into(), "modem".into(), Some(0))
        });
        let mut command = [0; 11];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGF=0\r\n");
        master.write_all(b"ERROR\r\n").unwrap();
        let result = failed_mode.join().unwrap().unwrap();
        assert!(!result.complete);
        assert_eq!(result.accepted_segments, 0);

        let missing_ack_actor = actor.clone();
        let missing_ack = std::thread::spawn(move || {
            missing_ack_actor.submit_sms("888".into(), "BAL".into(), "modem".into(), Some(0))
        });
        let mut command = [0; 11];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGF=0\r\n");
        master.write_all(b"OK\r\n").unwrap();
        let mut command = [0; 12];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGS=12\r\n");
        master.write_all(b"> ").unwrap();
        let mut payload = [0; 27];
        master.read_exact(&mut payload).unwrap();
        // OK alone is insufficient: without +CMGS the network outcome is unknown.
        master.write_all(b"OK\r\n").unwrap();
        let result = missing_ack.join().unwrap().unwrap();
        assert!(!result.complete);
        assert_eq!(result.accepted_segments, 0);
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_sms_submit_keeps_multipart_segments_atomic_against_other_at_work() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let text = "A".repeat(161);
        let expected = proletarius_sms::submit::prepare_with_policy(
            "888",
            &text,
            1,
            proletarius_sms::submit::EncodingPolicy::Modem,
            false,
        )
        .unwrap();
        assert_eq!(expected.len(), 2);

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let submit_actor = actor.clone();
        let submit = std::thread::spawn(move || {
            submit_actor.submit_sms("888".into(), text, "modem".into(), Some(0))
        });
        let mut command = [0; 11];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+CMGF=0\r\n");
        master.write_all(b"OK\r\n").unwrap();

        let first = format!("AT+CMGS={}\r\n", expected[0].len());
        let mut command = vec![0; first.len()];
        master.read_exact(&mut command).unwrap();
        assert_eq!(command, first.as_bytes());
        master.write_all(b"> ").unwrap();
        let mut first_payload = vec![0; 3 + expected[0].len() * 2];
        master.read_exact(&mut first_payload).unwrap();
        master.write_all(b"+CMGS: 10\r\nOK\r\n").unwrap();

        let queued_actor = actor.clone();
        let queued = std::thread::spawn(move || {
            queued_actor.execute_scheduled("AT+AFTER-SMS".into(), 1_000, true, None, None)
        });
        let second = format!("AT+CMGS={}\r\n", expected[1].len());
        let mut command = vec![0; second.len()];
        master.read_exact(&mut command).unwrap();
        assert_eq!(command, second.as_bytes());
        master.write_all(b"> ").unwrap();
        let mut second_payload = vec![0; 3 + expected[1].len() * 2];
        master.read_exact(&mut second_payload).unwrap();
        master.write_all(b"+CMGS: 11\r\nOK\r\n").unwrap();

        let mut command = [0; 14];
        master.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"AT+AFTER-SMS\r\n");
        master.write_all(b"OK\r\n").unwrap();
        let result = submit.join().unwrap().unwrap();
        assert!(result.complete);
        assert_eq!(result.accepted_segments, 2);
        assert_eq!(queued.join().unwrap().unwrap().result, "ok");
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_sms_submit_rejects_stale_generation_without_sending_at_commands() {
        use serialport::SerialPort as _;
        use std::io::Read;
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_millis(150)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let error = actor
            .submit_sms("888".into(), "BAL".into(), "modem".into(), Some(1))
            .unwrap_err();
        assert_eq!(error.code, "sms_generation_changed");
        let mut command = [0; 16];
        assert!(master.read(&mut command).is_err());
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_command_scheduler_runs_high_priority_before_queued_normal_work() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(1)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let first_actor = actor.clone();
        let first = std::thread::spawn(move || {
            first_actor.execute_scheduled("AT+FIRST".to_owned(), 3_000, false, None, None)
        });
        let mut sent = [0; 64];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+FIRST\r\n");

        let normal_actor = actor.clone();
        let normal = std::thread::spawn(move || {
            normal_actor.execute_scheduled("AT+NORMAL".to_owned(), 3_000, false, None, None)
        });
        std::thread::sleep(Duration::from_millis(30));
        let high_actor = actor.clone();
        let high = std::thread::spawn(move || {
            high_actor.execute_scheduled("AT+HIGH".to_owned(), 3_000, true, None, None)
        });
        std::thread::sleep(Duration::from_millis(30));

        master.write_all(b"OK\r\n").unwrap();
        assert_eq!(first.join().unwrap().unwrap().result, "ok");

        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+HIGH\r\n");
        master.write_all(b"OK\r\n").unwrap();
        assert_eq!(high.join().unwrap().unwrap().result, "ok");

        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+NORMAL\r\n");
        master.write_all(b"OK\r\n").unwrap();
        assert_eq!(normal.join().unwrap().unwrap().result, "ok");
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_sms_transaction_checks_generation_at_prompt_before_writing_payload() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(1)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let command_actor = actor.clone();
        let command = std::thread::spawn(move || {
            command_actor.execute_scheduled(
                "AT+CMGS=2".to_owned(),
                1_000,
                true,
                Some(b"0011\x1a".to_vec()),
                Some(0),
            )
        });
        let mut sent = [0; 64];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CMGS=2\r\n");

        master.write_all(b"+QSIMSTAT: 1,0\r\n> ").unwrap();
        let result = command.join().unwrap().unwrap();
        assert_eq!(result.result, "generation_changed");
        master.set_timeout(Duration::from_millis(100)).unwrap();
        assert!(
            master.read(&mut sent).is_err(),
            "stale SMS payload was written"
        );
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_sms_transaction_writes_payload_and_waits_for_final_ack() {
        use serialport::SerialPort as _;
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(1)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let command_actor = actor.clone();
        let command = std::thread::spawn(move || {
            command_actor.execute_scheduled(
                "AT+CMGS=2".to_owned(),
                1_000,
                true,
                Some(b"0011\x1a".to_vec()),
                Some(0),
            )
        });
        let mut sent = [0; 64];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CMGS=2\r\n");
        master.write_all(b"> ").unwrap();

        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"0011\x1a");
        master.write_all(b"+CMGS: 7\r\nOK\r\n").unwrap();
        let result = command.join().unwrap().unwrap();
        assert_eq!(result.result, "ok");
        assert_eq!(result.lines, ["+CMGS: 7"]);
        actor.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rust_command_timeout_is_bounded_and_reports_without_reopening_port() {
        use serialport::SerialPort as _;
        use std::io::Read;
        use std::sync::{Arc, Mutex};

        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(1)).unwrap();
        let path = slave.name().expect("pseudo-terminal slave path");
        let slave: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>> =
            Arc::new(Mutex::new(Some(Box::new(slave))));
        let opener = Arc::clone(&slave);
        let actor = test_actor_with_opener(move |_, _| {
            opener
                .lock()
                .expect("pseudo-terminal holder lock")
                .take()
                .ok_or_else(|| ActorError::unavailable("pseudo-terminal was already moved"))
        })
        .unwrap();
        actor.open(path, 115_200).unwrap();

        let started = Instant::now();
        let command_actor = actor.clone();
        let command = std::thread::spawn(move || command_actor.execute("AT+CSQ".to_owned(), 200));
        let mut sent = [0; 32];
        let count = master.read(&mut sent).unwrap();
        assert_eq!(&sent[..count], b"AT+CSQ\r\n");
        let result = command
            .join()
            .expect("command worker")
            .expect("timeout result");
        assert_eq!(result.result, "timeout");
        assert_eq!(result.timeout_streak, 1);
        assert_eq!(result.timeout_threshold, COMMAND_TIMEOUT_WATCHDOG_THRESHOLD);
        assert!(!result.watchdog_tripped);
        assert!(started.elapsed() >= Duration::from_millis(190));
        assert_eq!(actor.write(b"\x1b".to_vec()).unwrap(), 1);
        actor.close().unwrap();
    }
}

pub(crate) fn project_call_state(response: &str) -> serde_json::Value {
    serde_json::to_value(call_state_projection::project(response))
        .unwrap_or_else(|_| serde_json::json!({"state":"unknown","incoming":false,"active":false}))
}
