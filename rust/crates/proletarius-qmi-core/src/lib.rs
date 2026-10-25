// Copyright (c) 2026 iniwex5
// Modifications copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

pub mod fanout;
pub mod qmap;
mod transport;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use proletarius_qmi_wire::{
    CONTROL_SERVICE, DecodeError, PacketMeta, RESULT_TLV_KIND, Tlv, TlvIter, decode_packet,
    encode_packet, encoded_len,
};

pub use transport::PacketPort;
pub use transport::{RawTransport, Transport};

pub const CTL_GET_CLIENT_ID: u16 = 0x0022;
pub const CTL_RELEASE_CLIENT_ID: u16 = 0x0023;
pub const CTL_SYNC: u16 = 0x0027;
pub const CTL_GET_VERSION_INFO: u16 = 0x0021;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_EVENT_QUEUE: usize = 64;
const MAX_EVENT_QUEUE: usize = 64;
const MAX_IN_FLIGHT: usize = 64;
const MAX_COALESCED_INDICATIONS: usize = 32;
const MAX_CLIENT_IDS: usize = 256;
const MAX_SERVICE_VERSIONS: usize = 64;
const MAX_FRAME_SIZE: usize = u16::MAX as usize + 1;
const READ_BUFFER_SIZE: usize = 4096;
const MAX_RETAINED_PENDING: usize = 16 * 1024;
const MAX_PENDING_SIZE: usize = MAX_FRAME_SIZE + READ_BUFFER_SIZE;
const READER_THREAD_STACK: usize = 128 * 1024;

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub default_request_timeout: Duration,
    pub event_queue_size: usize,
    pub max_in_flight: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            default_request_timeout: DEFAULT_REQUEST_TIMEOUT,
            event_queue_size: DEFAULT_EVENT_QUEUE,
            max_in_flight: 32,
        }
    }
}

impl ClientOptions {
    fn normalized(mut self) -> Self {
        if self.default_request_timeout.is_zero() {
            self.default_request_timeout = DEFAULT_REQUEST_TIMEOUT;
        }
        if self.event_queue_size == 0 {
            self.event_queue_size = DEFAULT_EVENT_QUEUE;
        } else {
            self.event_queue_size = self.event_queue_size.min(MAX_EVENT_QUEUE);
        }
        if self.max_in_flight == 0 {
            self.max_in_flight = 32;
        } else {
            self.max_in_flight = self.max_in_flight.min(MAX_IN_FLIGHT);
        }
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedPacket {
    pub meta: PacketMeta,
    pub frame_kind: proletarius_qmi_wire::FrameKind,
    tlv_bytes: Arc<[u8]>,
}

impl OwnedPacket {
    fn decode(data: &[u8]) -> Result<Self, ClientError> {
        let packet = decode_packet(data).map_err(ClientError::Wire)?;
        Ok(Self {
            meta: packet.meta,
            frame_kind: packet.frame_kind,
            tlv_bytes: Arc::from(packet.tlv_bytes()),
        })
    }

    #[must_use]
    pub fn tlv_bytes(&self) -> &[u8] {
        &self.tlv_bytes
    }

    #[must_use]
    pub fn tlvs(&self) -> TlvIter<'_> {
        TlvIter::new(&self.tlv_bytes)
    }

    #[must_use]
    pub fn find_tlv(&self, kind: u8) -> Option<&[u8]> {
        self.tlvs()
            .find(|tlv| tlv.kind == kind)
            .map(|tlv| tlv.value)
    }

    #[must_use]
    pub const fn is_indication(&self) -> bool {
        if self.meta.service == CONTROL_SERVICE {
            self.meta.message_control_flags & 0x02 != 0
        } else {
            self.meta.message_control_flags & 0x04 != 0
        }
    }

    pub fn check_result(&self) -> Result<(), ClientError> {
        let value = self
            .find_tlv(RESULT_TLV_KIND)
            .ok_or(ClientError::MissingResult)?;
        if value.len() < 4 {
            return Err(ClientError::MalformedResult);
        }
        let result = u16::from_le_bytes([value[0], value[1]]);
        let error = u16::from_le_bytes([value[2], value[3]]);
        if result != 0 {
            return Err(ClientError::Qmi {
                service: self.meta.service,
                message: self.meta.message_id,
                result,
                error,
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Wire(DecodeError),
    Encode(proletarius_qmi_wire::EncodeError),
    Timeout,
    Busy,
    Closed,
    MissingResult,
    MalformedResult,
    OutputTooSmall,
    InvalidResponse(&'static str),
    Qmi {
        service: u16,
        message: u16,
        result: u16,
        error: u16,
    },
}

impl ClientError {
    /// Whether the request failed because the underlying QMI transport can no
    /// longer carry requests. Protocol, modem and timeout failures are not
    /// transport loss and must not restart the manager.
    #[must_use]
    pub fn is_transport_lost(&self) -> bool {
        match self {
            Self::Io(error) => matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::UnexpectedEof
            ),
            Self::Closed => true,
            _ => false,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "QMI transport: {error}"),
            Self::Wire(error) => write!(f, "QMI wire: {error}"),
            Self::Encode(error) => write!(f, "QMI encode: {error}"),
            Self::Timeout => f.write_str("QMI request timed out"),
            Self::Busy => f.write_str("QMI request queue is full"),
            Self::Closed => f.write_str("QMI client is closed"),
            Self::MissingResult => f.write_str("QMI response has no result TLV"),
            Self::MalformedResult => f.write_str("QMI result TLV is malformed"),
            Self::OutputTooSmall => f.write_str("QMI output buffer is too small"),
            Self::InvalidResponse(reason) => write!(f, "QMI response is invalid: {reason}"),
            Self::Qmi {
                service,
                message,
                result,
                error,
            } => write!(
                f,
                "QMI service 0x{service:04x} message 0x{message:04x} failed: result 0x{result:04x}, error 0x{error:04x}"
            ),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Wire(error) => Some(error),
            Self::Encode(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClientStats {
    pub unmatched_responses: u64,
    pub parse_errors: u64,
    pub dropped_indications: u64,
    pub coalesced_indications: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceVersion {
    pub service: u8,
    pub major: u16,
    pub minor: u16,
}

struct Inner {
    transport: Box<dyn Transport>,
    writes: Mutex<()>,
    transport_shutdown: Mutex<Option<CloseResult>>,
    final_close: Mutex<Option<CloseResult>>,
    transactions: Mutex<HashMap<u64, SyncSender<Result<OwnedPacket, ClientError>>>>,
    client_ids: Mutex<HashSet<(u16, u8)>>,
    events_tx: Mutex<Option<SyncSender<OwnedPacket>>>,
    events_rx: Mutex<Option<Receiver<OwnedPacket>>>,
    coalesced: Mutex<BTreeMap<u64, OwnedPacket>>,
    closed: AtomicBool,
    next_tx: AtomicU32,
    next_ctl_tx: AtomicU32,
    unmatched_responses: AtomicU64,
    parse_errors: AtomicU64,
    dropped_indications: AtomicU64,
    coalesced_indications: AtomicU64,
    reader: Mutex<Option<JoinHandle<()>>>,
    options: ClientOptions,
}

// io::Error is not Clone. Preserve the I/O classification and diagnostic for
// every caller without replaying transport shutdown after an uncertain result.
#[derive(Clone)]
struct CloseFailure {
    kind: io::ErrorKind,
    message: String,
    raw_os_error: Option<i32>,
}

impl CloseFailure {
    fn from_io(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
            raw_os_error: error.raw_os_error(),
        }
    }

    fn into_client_error(self) -> ClientError {
        ClientError::Io(match self.raw_os_error {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(self.kind, self.message),
        })
    }
}

type CloseResult = Result<(), CloseFailure>;

type TransactionResult = Result<OwnedPacket, ClientError>;

struct TransactionReservation {
    transaction_id: u16,
    key: u64,
    receiver: Receiver<TransactionResult>,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    /// A deadline this handle's requests expire by, when its opener needs one.
    ///
    /// The connection, client ids and event stream stay shared; only the
    /// default request timeout changes, so a compound operation can bound every
    /// exchange it makes without the service wrappers growing a parallel API.
    default_timeout: Option<Duration>,
}

#[derive(Clone)]
pub struct ServiceClient {
    client: Client,
    service: u16,
    client_id: u8,
    closed: Arc<AtomicBool>,
}

impl ServiceClient {
    pub fn open(client: &Client, service: u16) -> Result<Self, ClientError> {
        let client_id = client.allocate_client_id(service)?;
        Ok(Self {
            client: client.clone(),
            service,
            client_id,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub const fn service(&self) -> u16 {
        self.service
    }

    #[must_use]
    pub const fn client_id(&self) -> u8 {
        self.client_id
    }

    pub fn request(&self, message_id: u16, tlvs: &[Tlv<'_>]) -> Result<OwnedPacket, ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::Closed);
        }
        self.client
            .send_request(self.service, self.client_id, message_id, tlvs)
    }

    pub fn request_with_timeout(
        &self,
        message_id: u16,
        tlvs: &[Tlv<'_>],
        timeout: Duration,
    ) -> Result<OwnedPacket, ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::Closed);
        }
        self.client.send_request_with_timeout(
            self.service,
            self.client_id,
            message_id,
            tlvs,
            timeout,
        )
    }

    pub fn close(&self) -> Result<(), ClientError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Err(error) = self.client.release_client_id(self.service, self.client_id) {
            self.closed.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for ServiceClient {
    fn drop(&mut self) {
        if Arc::strong_count(&self.closed) != 1 {
            return;
        }
        if self.client.is_closed() {
            self.closed.store(true, Ordering::Release);
            return;
        }
        self.close().ok();
    }
}

impl Client {
    /// Build a client on a transport the caller already owns.
    ///
    /// There is deliberately no "open this device path for me" constructor: a
    /// QMI control device is a single serial resource, so a second reader
    /// would steal the responses and indications of whoever owns it. The
    /// platform-specific owner opens its transport and passes it through this
    /// constructor; this client never opens a second device path.
    pub fn from_transport(
        transport: Box<dyn Transport>,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        let options = options.normalized();
        let (events_tx, events_rx) = mpsc::sync_channel(options.event_queue_size);
        let inner = Arc::new(Inner {
            transport,
            writes: Mutex::new(()),
            transport_shutdown: Mutex::new(None),
            final_close: Mutex::new(None),
            transactions: Mutex::new(HashMap::new()),
            client_ids: Mutex::new(HashSet::new()),
            events_tx: Mutex::new(Some(events_tx)),
            events_rx: Mutex::new(Some(events_rx)),
            coalesced: Mutex::new(BTreeMap::new()),
            closed: AtomicBool::new(false),
            next_tx: AtomicU32::new(0),
            next_ctl_tx: AtomicU32::new(0),
            unmatched_responses: AtomicU64::new(0),
            parse_errors: AtomicU64::new(0),
            dropped_indications: AtomicU64::new(0),
            coalesced_indications: AtomicU64::new(0),
            reader: Mutex::new(None),
            options,
        });
        let weak = Arc::downgrade(&inner);
        let reader = thread::Builder::new()
            .name("switch-qmi-reader".to_owned())
            .stack_size(READER_THREAD_STACK)
            .spawn(move || reader_loop(weak))?;
        *lock(&inner.reader) = Some(reader);
        Ok(Self {
            inner,
            default_timeout: None,
        })
    }

    /// The same channel with a different default request timeout.
    ///
    /// Compound operations - a PDN bring-up, a data-plane takeover, entering
    /// VoWiFi low power - must be finished by a deadline, and every request
    /// they make has to respect it. The timeout is a client option, so a
    /// bounded handle carries the deadline to the service wrappers that
    /// already exist instead of each of them growing a second entry point.
    #[must_use]
    pub fn with_default_timeout(&self, timeout: Duration) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            default_timeout: Some(timeout),
        }
    }

    pub fn take_events(&self) -> Option<Receiver<OwnedPacket>> {
        lock(&self.inner.events_rx).take()
    }

    /// The module's data function, when this client's transport also owns it.
    ///
    /// A transport that reaches the module through something other than the
    /// pipes it claims - a character device on a host with a netdev - has no
    /// port to hand out.
    #[must_use]
    pub fn packet_port(&self, mux_id: u8) -> Option<std::sync::Arc<dyn PacketPort>> {
        self.inner.transport.packet_port(mux_id)
    }

    /// The module's data function when it carries plain IP.
    ///
    /// A firmware that does not aggregate hands whole packets to the bulk
    /// pair; a transport that reaches the module some other way answers `None`.
    #[must_use]
    pub fn native_packet_port(&self) -> Option<std::sync::Arc<dyn PacketPort>> {
        self.inner.transport.native_packet_port()
    }

    #[must_use]
    pub fn stats(&self) -> ClientStats {
        ClientStats {
            unmatched_responses: self.inner.unmatched_responses.load(Ordering::Relaxed),
            parse_errors: self.inner.parse_errors.load(Ordering::Relaxed),
            dropped_indications: self.inner.dropped_indications.load(Ordering::Relaxed),
            coalesced_indications: self.inner.coalesced_indications.load(Ordering::Relaxed),
        }
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    pub fn send_request(
        &self,
        service: u16,
        client_id: u8,
        message_id: u16,
        tlvs: &[Tlv<'_>],
    ) -> Result<OwnedPacket, ClientError> {
        self.send_request_with_timeout(
            service,
            client_id,
            message_id,
            tlvs,
            self.default_timeout
                .unwrap_or(self.inner.options.default_request_timeout),
        )
    }

    pub fn send_request_with_timeout(
        &self,
        service: u16,
        client_id: u8,
        message_id: u16,
        tlvs: &[Tlv<'_>],
        timeout: Duration,
    ) -> Result<OwnedPacket, ClientError> {
        if self.is_closed() {
            return Err(ClientError::Closed);
        }
        let reservation = reserve_transaction(&self.inner, service, client_id)?;
        let started = Instant::now();
        let transaction_id = reservation.transaction_id;
        let key = reservation.key;
        let meta = PacketMeta::request(service, client_id, transaction_id, message_id);
        let size = match encoded_len(meta, tlvs) {
            Ok(size) => size,
            Err(error) => {
                lock(&self.inner.transactions).remove(&key);
                return Err(ClientError::Encode(error));
            }
        };
        let mut request = vec![0u8; size];
        if let Err(error) = encode_packet(meta, tlvs, &mut request) {
            lock(&self.inner.transactions).remove(&key);
            return Err(ClientError::Encode(error));
        }

        let write_result = {
            let _write = lock(&self.inner.writes);
            self.inner.transport.write_all_timeout(&request, timeout)
        };
        if let Err(error) = write_result {
            lock(&self.inner.transactions).remove(&key);
            return Err(ClientError::Io(error));
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            lock(&self.inner.transactions).remove(&key);
            return Err(ClientError::Timeout);
        }
        match reservation.receiver.recv_timeout(remaining) {
            Ok(response) => response,
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.inner.transactions).remove(&key);
                Err(ClientError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(ClientError::Closed),
        }
    }

    pub fn sync(&self) -> Result<(), ClientError> {
        let response = self.send_request(CONTROL_SERVICE, 0, CTL_SYNC, &[])?;
        response.check_result()
    }

    pub fn service_versions(&self) -> Result<Vec<ServiceVersion>, ClientError> {
        let response = self.send_request(CONTROL_SERVICE, 0, CTL_GET_VERSION_INFO, &[])?;
        response.check_result()?;
        let value = response
            .find_tlv(0x01)
            .ok_or(ClientError::InvalidResponse("missing service-version TLV"))?;
        let count = service_version_count(value)?;
        let mut output = Vec::with_capacity(count);
        for entry in value[1..].chunks_exact(5).take(count) {
            output.push(ServiceVersion {
                service: entry[0],
                major: u16::from_le_bytes([entry[1], entry[2]]),
                minor: u16::from_le_bytes([entry[3], entry[4]]),
            });
        }
        Ok(output)
    }

    /// Query service versions without allocating a result vector.
    ///
    /// The response is validated in full before the caller's buffer is
    /// touched.  This is used by diagnostics and startup discovery where a
    /// fixed 64-entry array is already available.
    pub fn service_versions_into(
        &self,
        output: &mut [ServiceVersion],
    ) -> Result<usize, ClientError> {
        let response = self.send_request(CONTROL_SERVICE, 0, CTL_GET_VERSION_INFO, &[])?;
        response.check_result()?;
        let value = response
            .find_tlv(0x01)
            .ok_or(ClientError::InvalidResponse("missing service-version TLV"))?;
        let count = service_version_count(value)?;
        if output.len() < count {
            return Err(ClientError::OutputTooSmall);
        }
        for (slot, entry) in output
            .iter_mut()
            .zip(value[1..].chunks_exact(5).take(count))
        {
            *slot = ServiceVersion {
                service: entry[0],
                major: u16::from_le_bytes([entry[1], entry[2]]),
                minor: u16::from_le_bytes([entry[3], entry[4]]),
            };
        }
        Ok(count)
    }

    pub fn allocate_client_id(&self, service: u16) -> Result<u8, ClientError> {
        // A response can race with shutdown after the request has left the
        // transport.  Do not repopulate the client-ID registry after close;
        // otherwise a late response retains stale modem state until the last
        // `Client` clone is dropped.
        if self.is_closed() {
            return Err(ClientError::Closed);
        }
        if lock(&self.inner.client_ids).len() >= MAX_CLIENT_IDS {
            return Err(ClientError::Busy);
        }
        let mut service_bytes = [0_u8; 2];
        let service_len = if service <= u16::from(u8::MAX) {
            service_bytes[0] = service as u8;
            1
        } else {
            service_bytes.copy_from_slice(&service.to_le_bytes());
            2
        };
        let response = self.send_request(
            CONTROL_SERVICE,
            0,
            CTL_GET_CLIENT_ID,
            &[Tlv::new(0x01, &service_bytes[..service_len])],
        )?;
        response.check_result()?;
        let value = response
            .find_tlv(0x01)
            .ok_or(ClientError::InvalidResponse("missing client-ID TLV"))?;
        let client_id = match value.len() {
            2 => value[1],
            3 => value[2],
            _ => return Err(ClientError::InvalidResponse("invalid client-ID TLV")),
        };
        let mut client_ids = lock(&self.inner.client_ids);
        if self.is_closed() {
            return Err(ClientError::Closed);
        }
        if !client_ids.contains(&(service, client_id)) && client_ids.len() >= MAX_CLIENT_IDS {
            drop(client_ids);
            let _ = self.release_client_id(service, client_id);
            return Err(ClientError::Busy);
        }
        client_ids.insert((service, client_id));
        Ok(client_id)
    }

    pub fn release_client_id(&self, service: u16, client_id: u8) -> Result<(), ClientError> {
        let mut service_bytes = [0_u8; 3];
        let service_len = if service <= u16::from(u8::MAX) {
            service_bytes[..2].copy_from_slice(&[service as u8, client_id]);
            2
        } else {
            service_bytes[..2].copy_from_slice(&service.to_le_bytes());
            service_bytes[2] = client_id;
            3
        };
        let response = self.send_request(
            CONTROL_SERVICE,
            0,
            CTL_RELEASE_CLIENT_ID,
            &[Tlv::new(0x01, &service_bytes[..service_len])],
        )?;
        response.check_result()?;
        lock(&self.inner.client_ids).remove(&(service, client_id));
        Ok(())
    }

    pub fn close(&self) -> Result<(), ClientError> {
        let mut final_close = lock(&self.inner.final_close);
        if let Some(result) = &*final_close {
            return result.clone().map_err(CloseFailure::into_client_error);
        }
        *final_close = Some(Err(CloseFailure {
            kind: io::ErrorKind::Other,
            message: "QMI final close did not reach a confirmed result".to_owned(),
            raw_os_error: None,
        }));
        self.inner.closed.store(true, Ordering::Release);
        // Admission closure alone is not a completion receipt. This also
        // recovers a shutdown error retained by a fatal reader exit.
        let shutdown_result = shutdown_transport(&self.inner);
        fail_transactions(&self.inner);
        lock(&self.inner.events_tx).take();
        // Drop an unclaimed receiver as part of shutdown.  Otherwise packets
        // already buffered in the bounded channel remain live for as long as
        // a cloned `Client` is retained, which is common during recovery.
        lock(&self.inner.events_rx).take();
        lock(&self.inner.coalesced).clear();
        lock(&self.inner.client_ids).clear();
        let reader = lock(&self.inner.reader).take();
        let join_failed = reader.is_some_and(|reader| reader.join().is_err());
        let result = match (shutdown_result, join_failed) {
            (Ok(()), false) => Ok(()),
            (Err(error), false) => Err(error),
            (Ok(()), true) => Err(CloseFailure {
                kind: io::ErrorKind::Other,
                message: "QMI reader thread panicked".to_owned(),
                raw_os_error: None,
            }),
            (Err(mut error), true) => {
                error.message.push_str("; QMI reader thread panicked");
                // A composite error needs both diagnostics, while retaining
                // the transport classification for existing callers.
                error.raw_os_error = None;
                Err(error)
            }
        };
        *final_close = Some(result.clone());
        result.map_err(CloseFailure::into_client_error)
    }
}

fn shutdown_transport(inner: &Inner) -> CloseResult {
    let mut receipt = lock(&inner.transport_shutdown);
    if let Some(result) = &*receipt {
        return result.clone();
    }
    *receipt = Some(Err(CloseFailure {
        kind: io::ErrorKind::Other,
        message: "QMI transport shutdown did not reach a confirmed result".to_owned(),
        raw_os_error: None,
    }));
    let _write = lock(&inner.writes);
    // This boundary owns transport teardown. A panic is an unknown physical
    // result, but must not bypass the caller's retained reader-thread join.
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.transport.shutdown()))
            .map_err(|_| CloseFailure {
                kind: io::ErrorKind::Other,
                message: "QMI transport shutdown panicked; physical release unconfirmed".to_owned(),
                raw_os_error: None,
            })
            .and_then(|result| result.map_err(CloseFailure::from_io));
    *receipt = Some(result.clone());
    result
}

impl Drop for Client {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            let _ = self.close();
        }
    }
}

fn next_nonzero(counter: &AtomicU32, mask: u32) -> u16 {
    loop {
        let value = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1) & mask;
        if value != 0 {
            return value as u16;
        }
    }
}

fn transaction_key(service: u16, client_id: u8, transaction_id: u16) -> u64 {
    (u64::from(service) << 24) | (u64::from(client_id) << 16) | u64::from(transaction_id)
}

fn reserve_transaction(
    inner: &Inner,
    service: u16,
    client_id: u8,
) -> Result<TransactionReservation, ClientError> {
    let mut transactions = lock(&inner.transactions);
    if inner.closed.load(Ordering::Acquire) {
        return Err(ClientError::Closed);
    }
    if transactions.len() >= inner.options.max_in_flight {
        return Err(ClientError::Busy);
    }
    let counter = if service == CONTROL_SERVICE {
        &inner.next_ctl_tx
    } else {
        &inner.next_tx
    };
    let mask = if service == CONTROL_SERVICE {
        0xff
    } else {
        0xffff
    };
    for _ in 0..mask {
        let transaction_id = next_nonzero(counter, mask);
        let key = transaction_key(service, client_id, transaction_id);
        if let std::collections::hash_map::Entry::Vacant(entry) = transactions.entry(key) {
            let (sender, receiver) = mpsc::sync_channel(1);
            entry.insert(sender);
            return Ok(TransactionReservation {
                transaction_id,
                key,
                receiver,
            });
        }
    }
    Err(ClientError::Busy)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fail_transactions(inner: &Inner) {
    let mut transactions = lock(&inner.transactions);
    for (_, sender) in transactions.drain() {
        let _ = sender.send(Err(ClientError::Closed));
    }
}

fn reader_loop(weak: Weak<Inner>) {
    let mut pending = Vec::with_capacity(READ_BUFFER_SIZE);
    let mut consumed = 0;
    let mut read_buffer = [0u8; READ_BUFFER_SIZE];
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.closed.load(Ordering::Acquire) {
            return;
        }
        flush_coalesced(&inner);
        let read_result = inner.transport.read(&mut read_buffer);
        match read_result {
            Ok(0) => thread::sleep(Duration::from_millis(2)),
            Ok(read) => {
                compact_pending(&mut pending, &mut consumed, read);
                if pending.len().saturating_add(read) > MAX_PENDING_SIZE {
                    pending.clear();
                    consumed = 0;
                    inner.parse_errors.fetch_add(1, Ordering::Relaxed);
                }
                pending.extend_from_slice(&read_buffer[..read]);
                process_pending(&inner, &pending, &mut consumed);
                compact_pending(&mut pending, &mut consumed, 0);
                flush_coalesced(&inner);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) if inner.closed.load(Ordering::Acquire) => return,
            Err(error) => {
                inner.closed.store(true, Ordering::Release);
                inner.parse_errors.fetch_add(1, Ordering::Relaxed);
                fail_transactions(&inner);
                lock(&inner.events_tx).take();
                let _ = shutdown_transport(&inner);
                let _ = error;
                return;
            }
        }
    }
}

fn process_pending(inner: &Arc<Inner>, pending: &[u8], consumed: &mut usize) {
    loop {
        let available = &pending[*consumed..];
        if available.len() < 3 {
            return;
        }
        if available[0] != 0x01 && available[0] != 0x02 {
            *consumed += 1;
            inner.parse_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let frame_len = usize::from(u16::from_le_bytes([available[1], available[2]])) + 1;
        if !(6..=MAX_FRAME_SIZE).contains(&frame_len) {
            *consumed += 1;
            inner.parse_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if available.len() < frame_len {
            return;
        }
        let packet = OwnedPacket::decode(&available[..frame_len]);
        *consumed += frame_len;
        match packet {
            Ok(packet) if packet.is_indication() => {
                let events = lock(&inner.events_tx);
                let Some(events) = events.as_ref() else {
                    return;
                };
                match events.try_send(packet) {
                    Ok(()) => {}
                    Err(TrySendError::Full(packet)) => {
                        if let Some(key) = coalescing_key(&packet) {
                            store_coalesced(inner, key, packet);
                        } else {
                            inner.dropped_indications.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            Ok(packet) => {
                let key = transaction_key(
                    packet.meta.service,
                    packet.meta.client_id,
                    packet.meta.transaction_id,
                );
                let sender = lock(&inner.transactions).remove(&key);
                match sender {
                    Some(sender) => {
                        let _ = sender.send(Ok(packet));
                    }
                    None => {
                        inner.unmatched_responses.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(_) => {
                inner.parse_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn coalescing_key(packet: &OwnedPacket) -> Option<u64> {
    let service = packet.meta.service;
    let message = packet.meta.message_id;
    let coalescible = matches!(
        (service, message),
        (CONTROL_SERVICE, 0x0024)
            | (1, 0x0022)
            | (3, 0x0002)
            | (3, 0x0024)
            | (3, 0x003a)
            | (3, 0x004c)
            | (3, 0x0051)
            | (3, 0x0068)
            | (3, 0x0085)
            | (5, 0x004b)
    );
    coalescible.then_some((u64::from(service) << 16) | u64::from(message))
}

fn store_coalesced(inner: &Inner, key: u64, packet: OwnedPacket) {
    let mut events = lock(&inner.coalesced);
    if !events.contains_key(&key)
        && events.len() >= MAX_COALESCED_INDICATIONS
        && let Some(oldest) = events.keys().next().copied()
    {
        events.remove(&oldest);
    }
    events.insert(key, packet);
    inner.coalesced_indications.fetch_add(1, Ordering::Relaxed);
}

fn flush_coalesced(inner: &Arc<Inner>) {
    let mut events_tx = lock(&inner.events_tx);
    let Some(sender) = events_tx.as_ref() else {
        lock(&inner.coalesced).clear();
        return;
    };
    loop {
        let Some((key, packet)) = lock(&inner.coalesced)
            .iter()
            .next()
            .map(|(&key, packet)| (key, packet.clone()))
        else {
            return;
        };
        match sender.try_send(packet) {
            Ok(()) => {
                lock(&inner.coalesced).remove(&key);
            }
            Err(TrySendError::Full(_)) => return,
            Err(TrySendError::Disconnected(_)) => {
                events_tx.take();
                lock(&inner.coalesced).clear();
                return;
            }
        }
    }
}

fn compact_pending(pending: &mut Vec<u8>, consumed: &mut usize, incoming: usize) {
    if *consumed == 0 {
        return;
    }
    if *consumed == pending.len() {
        pending.clear();
        if pending.capacity() > MAX_RETAINED_PENDING {
            pending.shrink_to(READ_BUFFER_SIZE);
        }
        *consumed = 0;
        return;
    }
    if *consumed >= READ_BUFFER_SIZE
        || pending
            .capacity()
            .saturating_sub(pending.len())
            .lt(&incoming)
    {
        let remaining = pending.len() - *consumed;
        pending.copy_within(*consumed.., 0);
        pending.truncate(remaining);
        *consumed = 0;
    }
}

fn service_version_count(value: &[u8]) -> Result<usize, ClientError> {
    let (&count, entries) = value
        .split_first()
        .ok_or(ClientError::InvalidResponse("empty service-version TLV"))?;
    let count = usize::from(count);
    if count > MAX_SERVICE_VERSIONS {
        return Err(ClientError::InvalidResponse(
            "service-version list exceeds limit",
        ));
    }
    if entries.len() < count.saturating_mul(5) {
        return Err(ClientError::InvalidResponse(
            "truncated service-version list",
        ));
    }
    Ok(count)
}
