//! Bounded APDU client for the app-owned CoreBluetooth bridge.
//!
//! The BLE bridge remains a narrow Swift system-framework adapter. Rust owns
//! the LPA/APDU sequence and validates the connected reader before submitting
//! commands; the Go reader-session lease serializes this work with other card
//! operations.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use proletarius_sim::ApduTransport;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

const MAX_FRAME_BYTES: usize = 160_000;
const MAX_APDU_BYTES: usize = 65_535;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
// A socket deadline is only a waiting deadline, never evidence that the card
// operation ended. Ambiguous submissions retain their worker and poll the
// bridge's exact operation receipt until actual physical completion.
const APDU_TIMEOUT: Duration = Duration::from_secs(135);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Serialize)]
struct Request<'a> {
    token: &'a str,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reader: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hex: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bridge_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_id: Option<&'a str>,
}

#[derive(Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    reader: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    hex: String,
    #[serde(default, rename = "errorCode", alias = "error_code")]
    error_code: String,
    #[serde(default)]
    bridge_id: String,
    #[serde(default)]
    operation_id: String,
    #[serde(default)]
    operation_tracking: bool,
    #[serde(default)]
    terminal: bool,
    #[serde(default)]
    connection_id: String,
    #[serde(default)]
    close_operation_id: String,
    #[serde(default)]
    closed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionBinding {
    pub(crate) reader: String,
    pub(crate) bridge_id: String,
    pub(crate) connection_id: String,
    pub(crate) close_operation_id: String,
}
#[derive(Clone)]
pub(crate) struct CloseProof(ConnectionBinding);
impl CloseProof {
    pub(crate) fn binding(&self) -> &ConnectionBinding {
        &self.0
    }
}
/// Polls only the captured close operation. Unknown, replaced, logical idle and
/// failed socket replies are never physical-close evidence.
pub(crate) fn confirmed_close(
    socket: &Path,
    token: &str,
    binding: &ConnectionBinding,
) -> Result<Option<CloseProof>, String> {
    let request = Request {
        token,
        action: "operation-status",
        reader: None,
        hex: None,
        bridge_id: Some(&binding.bridge_id),
        operation_id: Some(&binding.close_operation_id),
        connection_id: Some(&binding.connection_id),
    };
    let response = exchange_wire(socket, &request, SOCKET_TIMEOUT)
        .map_err(|_| "BLE close receipt unavailable".to_owned())?;
    Ok((response.ok
        && response.terminal
        && response.closed
        && response.reader == binding.reader
        && response.bridge_id == binding.bridge_id
        && response.connection_id == binding.connection_id
        && response.operation_id == binding.close_operation_id)
        .then(|| CloseProof(binding.clone())))
}

// Native LPA/AKA and compatibility RPCs share the same in-flight slot. The
// blocking I/O worker owns it until a matching Swift terminal receipt arrives,
// even when its HTTP caller has disconnected. Keep independent fixture/bridge
// sockets independent, and discard inactive weak entries.
fn io_slot(socket: &Path) -> Result<Arc<Semaphore>, String> {
    static SLOTS: OnceLock<Mutex<HashMap<PathBuf, Weak<Semaphore>>>> = OnceLock::new();
    let mut slots = SLOTS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "BLE transport owner unavailable".to_owned())?;
    slots.retain(|_, value| value.strong_count() > 0);
    if let Some(slot) = slots.get(socket).and_then(Weak::upgrade) {
        return Ok(slot);
    }
    let slot = Arc::new(Semaphore::new(1));
    slots.insert(socket.to_owned(), Arc::downgrade(&slot));
    Ok(slot)
}

#[derive(Debug)]
pub(crate) struct BleIoPermit {
    socket: PathBuf,
    _permit: OwnedSemaphorePermit,
    physical_worker: Option<(String, crate::reader_reservation::BleIoWorkerPermit)>,
}

impl BleIoPermit {
    /// Compatibility requests carry the producer captured by their Go
    /// transport. Validate it and register physical ownership in one Gate
    /// critical section; a separate producer check would race confirmed exit.
    pub(crate) fn bind_go_reader(
        &mut self,
        reader: &str,
        producer: &str,
        gate: Arc<crate::reader_reservation::Gate>,
    ) -> Result<(), &'static str> {
        if self.physical_worker.is_some() {
            return Err("reader_binding_already_present");
        }
        let worker = gate.track_go_ble_io(reader, producer)?;
        self.physical_worker = Some((reader.to_owned(), worker));
        Ok(())
    }

    pub(crate) fn bind_reader(
        &mut self,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
    ) -> Result<(), String> {
        if let Some((bound_reader, _)) = &self.physical_worker {
            return if bound_reader == reader {
                Ok(())
            } else {
                Err("BLE transport physical reservation mismatch".into())
            };
        }
        let worker = gate.track_ble_io(reader).map_err(str::to_owned)?;
        self.physical_worker = Some((reader.to_owned(), worker));
        Ok(())
    }
}

pub(crate) fn acquire_io_permit(socket: &Path) -> Result<BleIoPermit, String> {
    let permit = io_slot(socket)?
        .try_acquire_owned()
        .map_err(|_| "BLE transport is busy; operation was not submitted".to_owned())?;
    Ok(BleIoPermit {
        socket: socket.to_owned(),
        _permit: permit,
        physical_worker: None,
    })
}

#[derive(Debug)]
pub(crate) struct BleApduTransport {
    socket: PathBuf,
    token: Zeroizing<String>,
    reader: String,
    bridge_id: String,
    connection: Option<ConnectionBinding>,
    timing: IoTiming,
    _permit: BleIoPermit,
}

#[derive(Clone, Copy, Debug)]
struct IoTiming {
    response: Duration,
    apdu: Duration,
    poll: Duration,
}

impl Default for IoTiming {
    fn default() -> Self {
        Self {
            response: SOCKET_TIMEOUT,
            apdu: APDU_TIMEOUT,
            poll: POLL_INTERVAL,
        }
    }
}

enum ExchangeError {
    NotSubmitted(String),
    Unconfirmed,
}

impl BleApduTransport {
    pub(crate) fn connect(
        socket: Option<&Path>,
        token: Option<&str>,
        reader: &str,
    ) -> Result<Self, String> {
        let socket = socket
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| "BLE bridge is not configured".to_owned())?;
        let permit = acquire_io_permit(socket)?;
        Self::connect_reserved(socket, token, reader, permit)
    }

    pub(crate) fn connect_reserved(
        socket: &Path,
        token: Option<&str>,
        reader: &str,
        permit: BleIoPermit,
    ) -> Result<Self, String> {
        Self::connect_reserved_with_gate(
            socket,
            token,
            reader,
            permit,
            crate::reader_reservation::gate(),
        )
    }

    pub(crate) fn connect_reserved_with_gate(
        socket: &Path,
        token: Option<&str>,
        reader: &str,
        mut permit: BleIoPermit,
        gate: Arc<crate::reader_reservation::Gate>,
    ) -> Result<Self, String> {
        if permit.socket != socket {
            return Err("BLE transport reservation mismatch".into());
        }
        let token = token
            .filter(|token| token.len() >= 32)
            .ok_or_else(|| "BLE bridge is not configured".to_owned())?;
        if !valid_ble_reader(reader) {
            return Err("invalid BLE reader identity".into());
        }
        let expected_connection = gate.ble_connection_binding(reader);
        permit.bind_reader(reader, gate)?;
        let mut transport = Self {
            socket: socket.to_owned(),
            token: Zeroizing::new(token.to_owned()),
            reader: reader.to_owned(),
            bridge_id: String::new(),
            connection: None,
            timing: IoTiming::default(),
            _permit: permit,
        };
        let response = transport.request("status", None)?;
        if !response.ok {
            return Err(bridge_error(&response));
        }
        if response.reader != transport.reader
            || !response.state.trim().eq_ignore_ascii_case("connected")
        {
            return Err("BLE bridge reader changed or is not connected".into());
        }
        if !response.operation_tracking || !valid_bridge_id(&response.bridge_id) {
            return Err(
                "BLE bridge does not support physical operation completion tracking".into(),
            );
        }
        if valid_bridge_id(&response.connection_id) && valid_bridge_id(&response.close_operation_id)
        {
            transport.connection = Some(ConnectionBinding {
                reader: reader.into(),
                bridge_id: response.bridge_id.clone(),
                connection_id: response.connection_id,
                close_operation_id: response.close_operation_id,
            });
        }
        if expected_connection.is_some() && expected_connection != transport.connection {
            return Err("BLE bridge connection changed; operation was not submitted".into());
        }
        transport.bridge_id = response.bridge_id;
        Ok(transport)
    }

    #[cfg(test)]
    pub(crate) fn connect_for_test(
        socket: Option<&Path>,
        token: Option<&str>,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
    ) -> Result<Self, String> {
        let socket = socket.ok_or("BLE bridge is not configured")?;
        Self::connect_reserved_with_gate(socket, token, reader, acquire_io_permit(socket)?, gate)
    }

    pub(crate) fn connection_binding(&self) -> Option<ConnectionBinding> {
        self.connection.clone()
    }

    fn request(&self, action: &'static str, apdu_hex: Option<&str>) -> Result<Response, String> {
        let mutating = matches!(action, "apdu" | "reset");
        let operation_id = mutating
            .then(crate::multistandby_store::random_token)
            .transpose()
            .map_err(|_| "BLE operation identity unavailable".to_owned())?;
        let request = Request {
            token: &self.token,
            action,
            reader: mutating.then_some(self.reader.as_str()),
            hex: apdu_hex,
            bridge_id: mutating.then_some(self.bridge_id.as_str()),
            operation_id: operation_id.as_deref(),
            connection_id: self.connection.as_ref().map(|c| c.connection_id.as_str()),
        };
        let timeout = if mutating {
            self.timing.apdu
        } else {
            self.timing.response
        };
        let result = self.exchange(&request, timeout);
        let Some(operation_id) = operation_id.as_deref() else {
            return result.map_err(|error| match error {
                ExchangeError::NotSubmitted(error) => error,
                ExchangeError::Unconfirmed => "BLE bridge status response is unconfirmed".into(),
            });
        };
        match result {
            Ok(response) if self.is_terminal(&response, operation_id) => return Ok(response),
            Err(ExchangeError::NotSubmitted(error)) => return Err(error),
            _ => {}
        }
        // This loop runs in the already admitted blocking worker. Dropping the
        // caller's future cannot drop its IO slot or Gate physical marker. It
        // never submits a second APDU, creates another worker, or uses TTL as
        // release evidence. A new bridge epoch/unknown ID is not completion.
        let query = Request {
            token: &self.token,
            action: "operation-status",
            reader: None,
            hex: None,
            bridge_id: Some(&self.bridge_id),
            operation_id: Some(operation_id),
            connection_id: self.connection.as_ref().map(|c| c.connection_id.as_str()),
        };
        loop {
            std::thread::sleep(self.timing.poll);
            if let Ok(response) = self.exchange(&query, self.timing.response)
                && self.is_terminal(&response, operation_id)
            {
                return Ok(response);
            }
        }
    }

    fn is_terminal(&self, response: &Response, operation_id: &str) -> bool {
        response.terminal
            && response.bridge_id == self.bridge_id
            && response.operation_id == operation_id
            && self
                .connection
                .as_ref()
                .is_none_or(|c| response.connection_id == c.connection_id)
            && !matches!(
                response.error_code.as_str(),
                "ble_operation_pending"
                    | "ble_operation_unknown"
                    | "ble_operation_conflict"
                    | "ble_bridge_changed"
            )
    }

    fn exchange(
        &self,
        request: &Request<'_>,
        timeout: Duration,
    ) -> Result<Response, ExchangeError> {
        exchange_wire(&self.socket, request, timeout)
    }

    pub(crate) fn reset(&self) -> Result<(), String> {
        let response = self.request("reset", None)?;
        if response.ok {
            Ok(())
        } else {
            Err(bridge_error(&response))
        }
    }
}

impl ApduTransport for BleApduTransport {
    type Error = String;

    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        if command.is_empty() || command.len() > MAX_APDU_BYTES {
            return Err("invalid BLE APDU length".into());
        }
        let mut encoded = String::with_capacity(command.len() * 2);
        for byte in command {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        let response = self.request("apdu", Some(&encoded))?;
        if !response.ok {
            return Err(bridge_error(&response));
        }
        decode_hex(&response.hex)
    }
}

fn exchange_wire(
    socket: &Path,
    request: &Request<'_>,
    timeout: Duration,
) -> Result<Response, ExchangeError> {
    // Finish serialization before any possible socket write, so only this
    // pre-dispatch path can be classified as definitely not submitted.
    let mut bytes =
        Zeroizing::new(serde_json::to_vec(request).map_err(|_| {
            ExchangeError::NotSubmitted("BLE bridge request encoding failed".into())
        })?);
    bytes.push(b'\n');
    let mut stream = connect_socket(socket, Duration::from_secs(3))
        .map_err(|error| ExchangeError::NotSubmitted(format!("BLE bridge unavailable: {error}")))?;
    stream.set_read_timeout(Some(timeout)).map_err(|error| {
        ExchangeError::NotSubmitted(format!("BLE bridge timeout setup: {error}"))
    })?;
    stream.set_write_timeout(Some(timeout)).map_err(|error| {
        ExchangeError::NotSubmitted(format!("BLE bridge timeout setup: {error}"))
    })?;
    stream
        .write_all(&bytes)
        .map_err(|_| ExchangeError::Unconfirmed)?;
    let mut line = Vec::with_capacity(512);
    let read = BufReader::new(stream)
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|_| ExchangeError::Unconfirmed)?;
    if read == 0 || read > MAX_FRAME_BYTES || line.last() != Some(&b'\n') {
        return Err(ExchangeError::Unconfirmed);
    }
    line.pop();
    serde_json::from_slice(&line).map_err(|_| ExchangeError::Unconfirmed)
}

fn connect_socket(path: &Path, timeout: Duration) -> std::io::Result<UnixStream> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.connect_timeout(&socket2::SockAddr::unix(path)?, timeout)?;
    Ok(socket.into())
}

fn bridge_error(response: &Response) -> String {
    if response.error_code.is_empty() {
        "BLE bridge: ble_unknown_error".into()
    } else {
        format!("BLE bridge: {}", response.error_code)
    }
}

fn valid_bridge_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn valid_ble_reader(reader: &str) -> bool {
    let Some(id) = reader.strip_prefix("BLE:") else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.is_empty() || value.len() % 2 != 0 || value.len() / 2 > MAX_APDU_BYTES {
        return Err("BLE bridge response hex has an invalid length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])
                .ok_or_else(|| "BLE bridge response hex is invalid".to_owned())?;
            let low = hex_nibble(pair[1])
                .ok_or_else(|| "BLE bridge response hex is invalid".to_owned())?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn socket_path() -> PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "switch-ble-apdu-{}-{unique}-{sequence}.sock",
            std::process::id()
        ))
    }

    fn serve_one(listener: &UnixListener, response: &'static str) -> String {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&request).unwrap();
        let mut response: serde_json::Value = serde_json::from_str(response).unwrap();
        response["bridge_id"] = serde_json::json!("fixture-bridge");
        if parsed["action"] == "status" {
            response["operation_tracking"] = serde_json::json!(true);
        } else {
            response["operation_id"] = parsed["operation_id"].clone();
            response["terminal"] = serde_json::json!(true);
        }
        writeln!(stream, "{response}").unwrap();
        request
    }

    #[test]
    fn connected_bridge_receives_exact_apdu_once_and_decodes_response() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let worker = thread::spawn(move || {
            let status = serve_one(
                &listener,
                "{\"ok\":true,\"reader\":\"BLE:12345678-1234-1234-1234-123456789abc\",\"state\":\"connected\"}\n",
            );
            let apdu = serve_one(&listener, "{\"ok\":true,\"hex\":\"9000\"}\n");
            (status, apdu)
        });
        let mut transport = BleApduTransport::connect_for_test(
            Some(&path),
            Some(&"a".repeat(32)),
            "BLE:12345678-1234-1234-1234-123456789abc",
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap();
        assert_eq!(
            transport.transmit(&[0x00, 0xa4, 0x04, 0x00]),
            Ok(vec![0x90, 0x00])
        );
        let (status, apdu) = worker.join().unwrap();
        assert!(status.contains("\"action\":\"status\""));
        assert!(apdu.contains("\"action\":\"apdu\""));
        assert!(apdu.contains("\"reader\":\"BLE:12345678-1234-1234-1234-123456789abc\""));
        assert!(apdu.contains("\"hex\":\"00a40400\""));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mismatched_status_reader_is_rejected_before_apdu() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let worker = thread::spawn(move || {
            let request = serve_one(
                &listener,
                "{\"ok\":true,\"reader\":\"BLE:aaaaaaaa-1234-1234-1234-123456789abc\",\"state\":\"connected\"}\n",
            );
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
            request
        });
        let error = BleApduTransport::connect_for_test(
            Some(&path),
            Some(&"a".repeat(32)),
            "BLE:12345678-1234-1234-1234-123456789abc",
            Arc::new(crate::reader_reservation::Gate::default()),
        )
        .unwrap_err();
        assert!(error.contains("changed or is not connected"));
        assert!(worker.join().unwrap().contains("\"action\":\"status\""));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn selected_reader_with_replaced_bridge_connection_is_rejected_before_apdu() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let worker = thread::spawn(move || {
            let request = serve_one(
                &listener,
                "{\"ok\":true,\"reader\":\"BLE:12345678-1234-1234-1234-123456789abc\",\"state\":\"connected\",\"connection_id\":\"replacement\",\"close_operation_id\":\"replacement-close\"}\n",
            );
            listener.set_nonblocking(true).unwrap();
            assert!(
                listener.accept().is_err(),
                "stale binding must send no APDU"
            );
            request
        });
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        gate.test_seed_ble_selection(
            READER,
            1,
            Some(ConnectionBinding {
                reader: READER.into(),
                bridge_id: "fixture-bridge".into(),
                connection_id: "original".into(),
                close_operation_id: "original-close".into(),
            }),
        );
        let error =
            BleApduTransport::connect_for_test(Some(&path), Some(TOKEN), READER, gate).unwrap_err();
        assert!(error.contains("connection changed"), "{error}");
        assert!(worker.join().unwrap().contains("\"action\":\"status\""));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_unbounded_and_malformed_apdu_frames() {
        assert!(decode_hex("").is_err());
        assert!(decode_hex("0").is_err());
        assert!(decode_hex("0g").is_err());
        assert!(decode_hex(&"aa".repeat(MAX_APDU_BYTES + 1)).is_err());
        assert!(!valid_ble_reader("BLE:../reader"));
    }

    #[test]
    fn saturated_bridge_backlog_does_not_block_connect_indefinitely() {
        let path = socket_path();
        let listener =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        listener
            .bind(&socket2::SockAddr::unix(&path).unwrap())
            .unwrap();
        listener.listen(1).unwrap();
        let start = std::time::Instant::now();
        let mut clients = Vec::new();
        let mut saturated = false;
        for _ in 0..32 {
            match connect_socket(&path, Duration::from_millis(20)) {
                Ok(client) => clients.push(client),
                Err(_) => {
                    saturated = true;
                    break;
                }
            }
        }
        assert!(saturated, "fixture did not fill the socket backlog");
        assert!(start.elapsed() < Duration::from_secs(2));
        drop(clients);
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
    const READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";
    const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn accept_request(listener: &UnixListener) -> (UnixStream, serde_json::Value) {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "bridge fixture timed out"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("bridge accept: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).unwrap();
        (stream, serde_json::from_str(&line).unwrap())
    }

    fn respond(mut stream: UnixStream, value: serde_json::Value) {
        writeln!(stream, "{value}").unwrap();
    }

    fn status() -> serde_json::Value {
        serde_json::json!({"ok":true,"reader":READER,"state":"connected",
            "bridge_id":"fixture-bridge","operation_tracking":true})
    }

    fn terminal(request: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({"ok":true,"hex":"9000","terminal":true,
            "bridge_id":request["bridge_id"],"operation_id":request["operation_id"]})
    }

    #[test]
    fn untracked_bridge_is_rejected_without_card_submission() {
        for capability in [serde_json::Value::Null, serde_json::json!(false)] {
            let path = socket_path();
            let listener = UnixListener::bind(&path).unwrap();
            let socket = path.clone();
            let gate = Arc::new(crate::reader_reservation::Gate::default());
            let owner = gate.clone();
            let worker = thread::spawn(move || {
                BleApduTransport::connect_for_test(Some(&socket), Some(TOKEN), READER, owner)
            });
            let (stream, request) = accept_request(&listener);
            assert_eq!(request["action"], "status");
            let mut reply = status();
            if capability.is_null() {
                reply.as_object_mut().unwrap().remove("operation_tracking");
            } else {
                reply["operation_tracking"] = capability;
            }
            respond(stream, reply);
            assert!(
                worker
                    .join()
                    .unwrap()
                    .unwrap_err()
                    .contains("completion tracking")
            );
            assert!(listener.accept().is_err());
            assert!(acquire_io_permit(&path).is_ok());
            assert!(gate.admit(READER).is_ok());
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn uncertain_submissions_poll_exact_receipt_and_retain_slot_and_gate_until_terminal() {
        for fault in [
            "eof",
            "malformed",
            "truncated",
            "oversized",
            "timeout",
            "pending",
            "wrong_epoch",
            "wrong_id",
        ] {
            let path = socket_path();
            let listener = UnixListener::bind(&path).unwrap();
            let socket = path.clone();
            let gate = Arc::new(crate::reader_reservation::Gate::default());
            let owner = gate.clone();
            let worker = thread::spawn(move || {
                let mut transport =
                    BleApduTransport::connect_for_test(Some(&socket), Some(TOKEN), READER, owner)
                        .unwrap();
                transport.timing = IoTiming {
                    response: Duration::from_millis(50),
                    apdu: Duration::from_millis(30),
                    poll: Duration::from_millis(2),
                };
                transport.transmit(&[0, 0xa4, 0, 0])
            });
            let (stream, _) = accept_request(&listener);
            respond(stream, status());
            let (mut stream, submission) = accept_request(&listener);
            assert_eq!(submission["action"], "apdu");
            assert_eq!(submission["reader"], READER);
            assert_eq!(submission["bridge_id"], "fixture-bridge");
            let id = submission["operation_id"].as_str().unwrap();
            assert_eq!(id.len(), 64);
            assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
            let mut held_stream = None;
            match fault {
                "eof" => drop(stream),
                "malformed" => {
                    stream.write_all(b"not json\n").unwrap();
                }
                "truncated" => {
                    stream.write_all(b"{\"ok\":true}").unwrap();
                }
                "oversized" => {
                    let _ = stream.write_all(&vec![b'x'; MAX_FRAME_BYTES + 1]);
                }
                "timeout" => {
                    held_stream = Some(stream);
                }
                _ => {
                    let mut response = terminal(&submission);
                    match fault {
                        "pending" => response["terminal"] = serde_json::json!(false),
                        "wrong_epoch" => response["bridge_id"] = serde_json::json!("new-bridge"),
                        "wrong_id" => {
                            response["operation_id"] = serde_json::json!("other-operation")
                        }
                        _ => unreachable!(),
                    }
                    respond(stream, response);
                }
            }
            // Even malformed completion claims and unknown receipts cannot
            // release either admission fence. Every following frame is read-only.
            for invalid in [
                "pending",
                "unknown",
                "wrong_epoch",
                "wrong_id",
                "missing_terminal",
                "missing_id",
                "missing_epoch",
                "contradictory_unknown",
                "malformed",
            ] {
                let (mut stream, query) = accept_request(&listener);
                drop(held_stream.take());
                assert_eq!(query["action"], "operation-status", "{fault}/{invalid}");
                assert_eq!(query["operation_id"], submission["operation_id"]);
                assert_eq!(query["bridge_id"], submission["bridge_id"]);
                assert!(query.get("hex").is_none());
                assert!(!worker.is_finished(), "{fault}/{invalid}");
                assert!(acquire_io_permit(&path).is_err(), "{fault}/{invalid}");
                assert!(gate.admit(READER).is_err(), "{fault}/{invalid}");
                if invalid == "malformed" {
                    stream.write_all(b"{}\n").unwrap();
                    continue;
                }
                let mut response = terminal(&submission);
                match invalid {
                    "pending" | "unknown" => {
                        response["terminal"] = serde_json::json!(false);
                        response["ok"] = serde_json::json!(false);
                        response["errorCode"] =
                            serde_json::json!(format!("ble_operation_{invalid}"));
                    }
                    "contradictory_unknown" => {
                        response["errorCode"] = serde_json::json!("ble_operation_unknown")
                    }
                    "wrong_epoch" => response["bridge_id"] = serde_json::json!("new-bridge"),
                    "wrong_id" => response["operation_id"] = serde_json::json!("other-operation"),
                    "missing_terminal" => {
                        response.as_object_mut().unwrap().remove("terminal");
                    }
                    "missing_id" => {
                        response.as_object_mut().unwrap().remove("operation_id");
                    }
                    "missing_epoch" => {
                        response.as_object_mut().unwrap().remove("bridge_id");
                    }
                    _ => unreachable!(),
                }
                respond(stream, response);
            }
            let (stream, query) = accept_request(&listener);
            assert_eq!(query["action"], "operation-status");
            respond(stream, terminal(&submission));
            assert_eq!(worker.join().unwrap(), Ok(vec![0x90, 0x00]), "{fault}");
            assert!(acquire_io_permit(&path).is_ok());
            assert!(gate.admit(READER).is_ok());
            assert!(listener.accept().is_err());
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn reset_recovers_terminal_error_without_replaying_the_reset() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let socket = path.clone();
        let worker = thread::spawn(move || {
            let mut transport = BleApduTransport::connect_for_test(
                Some(&socket),
                Some(TOKEN),
                READER,
                Arc::new(crate::reader_reservation::Gate::default()),
            )
            .unwrap();
            transport.timing.poll = Duration::from_millis(2);
            transport.reset()
        });
        let (stream, _) = accept_request(&listener);
        respond(stream, status());
        let (stream, request) = accept_request(&listener);
        assert_eq!(request["action"], "reset");
        drop(stream);
        let (stream, query) = accept_request(&listener);
        assert_eq!(query["action"], "operation-status");
        let mut response = terminal(&request);
        response["ok"] = serde_json::json!(false);
        response["errorCode"] = serde_json::json!("bluetooth_disconnected");
        respond(stream, response);
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            "BLE bridge: bluetooth_disconnected"
        );
        assert!(listener.accept().is_err());
        assert!(acquire_io_permit(&path).is_ok());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn bound_io_permit_blocks_gate_before_dispatch_and_rejects_reader_rebinding() {
        let path = socket_path();
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        let mut permit = acquire_io_permit(&path).unwrap();
        permit.bind_reader(READER, gate.clone()).unwrap();
        permit.bind_reader(READER, gate.clone()).unwrap();
        assert!(gate.admit(READER).is_err());
        assert!(
            permit
                .bind_reader("BLE:aaaaaaaa-1234-1234-1234-123456789abc", gate.clone())
                .is_err()
        );
        drop(permit);
        assert!(gate.admit(READER).is_ok());
    }
}

#[cfg(test)]
pub(crate) mod close_tests;
