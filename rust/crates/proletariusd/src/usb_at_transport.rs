//! Blocking USB AT transport. One owner holds the claimed handle for its entire
//! lifetime; submitted writes are never retried. The actor must run these calls
//! off the async executor and wait for the real call before dropping the owner.
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

mod reconnect;
pub(crate) mod recovery;
#[cfg(test)]
mod restore_journal_tests;
pub(crate) use reconnect::{DurableUsbBinding, ReenumerationAnchor, open_exact, open_successor};

const MAX_BYTES: usize = 1024 * 1024;
const READ_BYTES: usize = 512;
const MAX_DRAIN_TRANSFERS: usize = 4096;
const MIN_TIMEOUT: Duration = Duration::from_millis(1);
const DRAIN_TIMEOUT: Duration = Duration::from_millis(80);
const READ_TIMEOUT: Duration = Duration::from_millis(900);
const PROBE_TIMEOUT: Duration = Duration::from_millis(900);
const ESC_TIMEOUT: Duration = Duration::from_millis(300);
const DEVICE_IDS: [(u16, u16); 2] = [(0x2ca3, 0x4006), (0x2c7c, 0x0125)];
const DJI_QDC507: (u16, u16) = DEVICE_IDS[0];

fn supports_usb_at_interface(model: (u16, u16), interface: u8) -> bool {
    matches!(interface, 2 | 3) || (model == DJI_QDC507 && matches!(interface, 4 | 5))
}

fn usb_at_endpoint_pair_matches(
    model: (u16, u16),
    interface: u8,
    endpoint_in: u8,
    endpoint_out: u8,
) -> bool {
    if model != DJI_QDC507 {
        return true;
    }
    match interface {
        4 => endpoint_in == 0x86 && endpoint_out == 0x04,
        5 => endpoint_in == 0x88 && endpoint_out == 0x05,
        _ => true,
    }
}

fn usb_at_interface_priority(model: (u16, u16), interface: u8) -> u8 {
    if model == DJI_QDC507 {
        match interface {
            // In the QDC507 RNDIS composition, these are its AT0/AT1 pair.
            4 => 0,
            5 => 1,
            2 => 2,
            3 => 3,
            _ => 4,
        }
    } else {
        match interface {
            2 => 0,
            3 => 1,
            _ => 2,
        }
    }
}

/// Observation of host resource retirement, never restored module settings,
/// SET_INTERFACE acknowledgement for a disconnected target, or recovery success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseDisposition {
    ReleasedAndHandleDropped,
    DisconnectedAndHandleDropped,
}

pub(crate) type LineCapture = Arc<dyn Fn(&str) + Send + Sync>;

/// An opaque reference to the USB connection that owns the opened AT handle.
///
/// This is not a port, address, VID/PID or serial-number selector. Native
/// capture retains the handle's actual libusb Device and Context, so another
/// device with identical descriptors cannot satisfy it. It remains usable
/// after the AT handle is dropped, but does not prove continued presence: the
/// consumer must open this exact Device and fail if it disappeared.
///
/// A USB composition change/reboot/re-enumeration ends this binding. A new
/// connection requires independently verified identity; neither the same port
/// nor an unverified serial string grants permission to substitute a Device.
#[derive(Clone)]
pub(crate) struct PhysicalTarget {
    connection: PhysicalConnection,
}
#[derive(Clone)]
enum PhysicalConnection {
    #[cfg(target_os = "macos")]
    Native(rusb::Device<rusb::Context>),
    // An ownership-only fixture, never USB identity evidence. No pointer,
    // Context or physical device is manufactured for actor lease tests.
    #[cfg(test)]
    Synthetic(Arc<()>),
}
impl fmt::Debug for PhysicalTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Do not emit descriptors, topology, serials or pointer values.
        f.debug_struct("PhysicalTarget").finish_non_exhaustive()
    }
}
impl PhysicalTarget {
    pub(crate) fn same_connection(&self, other: &Self) -> bool {
        match (&self.connection, &other.connection) {
            #[cfg(target_os = "macos")]
            (PhysicalConnection::Native(a), PhysicalConnection::Native(b)) => {
                a.as_raw() == b.as_raw()
            }
            #[cfg(test)]
            (PhysicalConnection::Synthetic(a), PhysicalConnection::Synthetic(b)) => {
                Arc::ptr_eq(a, b)
            }
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }
    #[cfg(test)]
    pub(crate) fn synthetic_for_tests() -> Self {
        Self {
            connection: PhysicalConnection::Synthetic(Arc::new(())),
        }
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_device(&self) -> Result<rusb::Device<rusb::Context>, TransportError> {
        match &self.connection {
            PhysicalConnection::Native(device) => Ok(device.clone()),
            #[cfg(test)]
            PhysicalConnection::Synthetic(_) => Err(target_unconfirmed()),
        }
    }
}

pub(crate) fn close_unconfirmed() -> TransportError {
    TransportError::new(
        "usb_at_close_unconfirmed",
        "USB AT interface release or handle close is unconfirmed",
        &[],
        true,
    )
}

#[cfg(test)]
pub(crate) fn no_device_for_tests() -> TransportError {
    TransportError::new(
        "usb_at_not_found",
        "No supported USB AT device was found",
        &[],
        false,
    )
}

fn target_unconfirmed() -> TransportError {
    TransportError::new(
        "usb_at_target_unconfirmed",
        "The opened AT transport cannot prove its native USB connection",
        &[],
        false,
    )
}

pub(crate) trait Transport: Send {
    /// Consume the actual transport and confirm interface release and handle
    /// close. Destructor completion alone is never checked release evidence.
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        drop(self);
        Err(close_unconfirmed())
    }

    /// Preflight only the actually opened device. Missing identity cannot be
    /// repaired by discovery or by sending AT to an unrelated candidate.
    fn capture_reenumeration_anchor(
        &mut self,
        _deadline: Instant,
        _guard: crate::module_radio::CallAdmission,
    ) -> Result<ReenumerationAnchor, TransportError> {
        Err(target_unconfirmed())
    }
    /// Capture only the already opened transport's connection. Implementations
    /// without native evidence must fail, never enumerate a replacement.
    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
        Err(target_unconfirmed())
    }
    fn command_with_guard(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<String, TransportError> {
        if !guard() {
            return Err(TransportError::new(
                "module_call_admission_changed",
                "Native transaction admission changed before submission",
                &[],
                false,
            ));
        }
        self.command(command, follow_up, timeout, max_bytes)
    }
    fn set_line_capture(&mut self, _capture: LineCapture) {}
    fn poll_call_capture(&mut self, _timeout: Duration) -> Result<(), TransportError> {
        Ok(())
    }
    fn command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<String, TransportError>;
    /// A status plan must not continue after an unfinished command. This
    /// default validates the normalized response contract used by adapters;
    /// NativeTransport overrides it to retain actual wire-terminal evidence.
    fn status_command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<String, TransportError> {
        let response = self.command(command, None, timeout, max_bytes)?;
        if response
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .next_back()
            .and_then(|line| terminal_line(line.as_bytes()))
            .is_none()
        {
            return Err(incomplete_status_response(response.as_bytes()));
        }
        Ok(response)
    }
    fn description(&self) -> String;
}

/// `message` is safe for diagnostics; it never includes command, payload,
/// response bytes or card identity. `response` is for the private caller only.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TransportError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) response: String,
    /// Some command bytes may have reached the device. This is not permission
    /// to replay the operation, even when no response has arrived.
    pub(crate) uncertain: bool,
}
impl fmt::Debug for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportError")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("response_bytes", &self.response.len())
            .field("uncertain", &self.uncertain)
            .finish()
    }
}
impl TransportError {
    fn new(code: &'static str, message: &'static str, response: &[u8], uncertain: bool) -> Self {
        Self {
            code,
            message: message.into(),
            response: normalize_response(response),
            uncertain,
        }
    }
}
impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for TransportError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IoError {
    Timeout,
    Disconnected,
    Other,
}

/// All concrete transfers receive a nonzero, finite millisecond timeout.
/// `now` can be overridden by fake I/O to exercise deadlines without sleeping.
trait Io: Send {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        drop(self);
        Err(close_unconfirmed())
    }

    fn capture_reenumeration_anchor(
        &mut self,
        _deadline: Instant,
        _guard: crate::module_radio::CallAdmission,
    ) -> Result<ReenumerationAnchor, TransportError> {
        Err(target_unconfirmed())
    }
    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
        Err(target_unconfirmed())
    }
    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, IoError>;
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, IoError>;
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub(crate) struct NativeTransport {
    // Field order is intentional: close/release the concrete handle before
    // returning the process-wide admission lease to a future opener.
    io: Box<dyn Io>,
    description: String,
    _owner: Option<OwnerLease>,
    call_capture: Option<LineCapture>,
    capture_line: Vec<u8>,
    command_guard: Option<crate::module_radio::CallAdmission>,
}
impl NativeTransport {
    /// Tests inject only physical byte transfers. Framing, drain, finite
    /// admission callbacks and the command Plan remain production code.
    #[cfg(test)]
    pub(crate) fn scripted_for_tests(
        target: PhysicalTarget,
        read: Box<dyn FnMut(&mut [u8], Duration) -> Result<usize, ()> + Send>,
        write: Box<dyn FnMut(&[u8], Duration) -> Result<usize, ()> + Send>,
    ) -> Self {
        struct ScriptedIo {
            target: PhysicalTarget,
            read: Box<dyn FnMut(&mut [u8], Duration) -> Result<usize, ()> + Send>,
            write: Box<dyn FnMut(&[u8], Duration) -> Result<usize, ()> + Send>,
        }
        impl Io for ScriptedIo {
            fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
                drop(self);
                Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped) // explicit successful release in this byte-I/O fixture
            }

            fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
                Ok(self.target.clone())
            }
            fn capture_reenumeration_anchor(
                &mut self,
                deadline: Instant,
                guard: crate::module_radio::CallAdmission,
            ) -> Result<ReenumerationAnchor, TransportError> {
                reconnect::check_finite(deadline, &guard)?;
                Ok(ReenumerationAnchor::synthetic_for_tests(
                    self.target.clone(),
                    (0x2c7c, 0x0125),
                ))
            }
            fn read(&mut self, output: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
                (self.read)(output, timeout).map_err(|()| IoError::Timeout)
            }
            fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, IoError> {
                (self.write)(bytes, timeout).map_err(|()| IoError::Other)
            }
        }
        Self::new(
            Box::new(ScriptedIo {
                target,
                read,
                write,
            }),
            "ownership-only scripted byte transport".into(),
            None,
        )
    }
    fn new(io: Box<dyn Io>, description: String, owner: Option<OwnerLease>) -> Self {
        Self {
            io,
            description,
            _owner: owner,
            call_capture: None,
            capture_line: Vec::new(),
            command_guard: None,
        }
    }

    fn capture_input(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if self.capture_line.len() >= MAX_BYTES {
                self.capture_line.clear();
            }
            self.capture_line.push(*byte);
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.capture_line);
                if let Some(capture) = &self.call_capture {
                    capture(&String::from_utf8_lossy(&line));
                }
            }
        }
    }
    fn remaining(&self, deadline: Instant, ceiling: Duration) -> Option<Duration> {
        let remaining = deadline.saturating_duration_since(self.io.now());
        if remaining < MIN_TIMEOUT {
            return None;
        }
        // rusb converts milliseconds to libusb's unsigned 32-bit timeout. Cap
        // rather than allowing an oversized duration to wrap to infinite zero.
        Some(
            remaining
                .min(ceiling)
                .min(Duration::from_millis(u32::MAX as u64)),
        )
    }

    fn drain(&mut self, deadline: Instant, max_bytes: usize) -> Result<(), TransportError> {
        let mut discarded = 0usize;
        let mut buffer = [0u8; READ_BYTES];
        for _ in 0..MAX_DRAIN_TRANSFERS {
            let timeout = self.remaining(deadline, DRAIN_TIMEOUT).ok_or_else(|| {
                TransportError::new(
                    "usb_at_timeout",
                    "USB AT deadline expired while draining input",
                    &[],
                    false,
                )
            })?;
            match self.io.read(&mut buffer, timeout) {
                Err(IoError::Timeout) => return Ok(()),
                Err(IoError::Disconnected) => {
                    return Err(TransportError::new(
                        "usb_at_disconnected",
                        "USB AT device disconnected while draining input",
                        &[],
                        false,
                    ));
                }
                Err(IoError::Other) => {
                    return Err(TransportError::new(
                        "usb_at_drain_failed",
                        "USB AT input drain failed",
                        &[],
                        false,
                    ));
                }
                Ok(length) if length > buffer.len() => {
                    return Err(TransportError::new(
                        "usb_at_drain_failed",
                        "USB AT input transfer length is invalid",
                        &[],
                        false,
                    ));
                }
                Ok(length) => {
                    if length > max_bytes.saturating_sub(discarded) {
                        return Err(TransportError::new(
                            "usb_at_response_too_large",
                            "USB AT input drain exceeds its byte limit",
                            &[],
                            false,
                        ));
                    }
                    self.capture_input(&buffer[..length]);
                    discarded += length;
                }
            }
        }
        Err(TransportError::new(
            "usb_at_drain_failed",
            "USB AT input drain did not become quiet",
            &[],
            false,
        ))
    }

    fn write_once(
        &mut self,
        data: &[u8],
        deadline: Instant,
        response: &[u8],
        already_submitted: bool,
    ) -> Result<(), TransportError> {
        let timeout = self.remaining(deadline, Duration::MAX).ok_or_else(|| {
            TransportError::new(
                "usb_at_timeout",
                "USB AT deadline expired before write",
                response,
                already_submitted,
            )
        })?;
        // The drain, deadline calculation and prompt wait may execute code or
        // block after admission. Check immediately before this actual payload
        // write, including a prompt follow-up. An accepted editor's ESC below
        // is settlement, so it deliberately does not pass through this gate.
        if self.command_guard.as_ref().is_some_and(|guard| !guard()) {
            return Err(TransportError::new(
                "module_call_admission_changed",
                "Native transaction admission changed before submission",
                response,
                already_submitted,
            ));
        }
        match self.io.write(data, timeout) {
            Ok(length) if length == data.len() => Ok(()),
            Ok(_) => Err(TransportError::new(
                "usb_at_short_write",
                "USB AT write was incomplete; submission outcome is uncertain",
                response,
                true,
            )),
            Err(IoError::Disconnected) => Err(TransportError::new(
                "usb_at_disconnected",
                "USB AT device disconnected during write; submission outcome is uncertain",
                response,
                true,
            )),
            Err(IoError::Timeout) => Err(TransportError::new(
                "usb_at_timeout",
                "USB AT write timed out; submission outcome is uncertain",
                response,
                true,
            )),
            Err(IoError::Other) => Err(TransportError::new(
                "usb_at_write_failed",
                "USB AT write failed; submission outcome is uncertain",
                response,
                true,
            )),
        }
    }

    fn execute_command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
        require_terminal: bool,
    ) -> Result<String, TransportError> {
        let command = command.trim();
        if !command
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AT"))
            || command.len() >= MAX_BYTES
        {
            return Err(TransportError::new(
                "usb_at_invalid_command",
                "USB AT command must start with AT and fit within its byte limit",
                &[],
                false,
            ));
        }
        if follow_up.is_some_and(|bytes| bytes.is_empty() || bytes.len() > MAX_BYTES) {
            return Err(TransportError::new(
                "usb_at_invalid_payload",
                "USB AT follow-up must be nonempty and fit within its byte limit",
                &[],
                false,
            ));
        }
        let max_bytes = if max_bytes == 0 { MAX_BYTES } else { max_bytes };
        if max_bytes > MAX_BYTES {
            return Err(TransportError::new(
                "usb_at_invalid_limit",
                "USB AT response limit exceeds the hard maximum",
                &[],
                false,
            ));
        }
        let timeout = if timeout.is_zero() {
            Duration::from_secs(if follow_up.is_some() { 30 } else { 3 })
        } else {
            timeout
        };
        if timeout < MIN_TIMEOUT {
            return Err(TransportError::new(
                "usb_at_invalid_timeout",
                "USB AT timeout must be at least one millisecond",
                &[],
                false,
            ));
        }
        let deadline = self.io.now().checked_add(timeout).ok_or_else(|| {
            TransportError::new(
                "usb_at_invalid_timeout",
                "USB AT timeout exceeds the monotonic clock range",
                &[],
                false,
            )
        })?;
        self.drain(deadline, max_bytes)?;
        let mut payload = Zeroizing::new(Vec::with_capacity(command.len() + 1));
        payload.extend_from_slice(command.as_bytes());
        payload.push(b'\r');
        let result = self
            .write_once(&payload, deadline, &[], false)
            .and_then(|()| self.collect(follow_up, deadline, max_bytes, require_terminal));
        if follow_up.is_some() && result.as_ref().is_err_and(|error| error.uncertain) {
            // Even if the prompt itself was lost, the editor may already be
            // active. Make exactly one best-effort ESC transfer, never replay
            // the AT command or message body. Cleanup adds at most 300 ms to
            // the requested deadline; failure cannot make the outcome certain.
            let _ = self.io.write(&[0x1b], ESC_TIMEOUT);
        }
        result
    }
    fn collect(
        &mut self,
        follow_up: Option<&[u8]>,
        deadline: Instant,
        max_bytes: usize,
        require_terminal: bool,
    ) -> Result<String, TransportError> {
        let mut response = Vec::new();
        let mut buffer = [0u8; READ_BYTES];
        let mut prompted = false;
        while let Some(timeout) = self.remaining(deadline, READ_TIMEOUT) {
            let length = match self.io.read(&mut buffer, timeout) {
                Ok(length) if length <= buffer.len() => length,
                Ok(_) => {
                    return Err(TransportError::new(
                        "usb_at_read_failed",
                        "USB AT input transfer length is invalid",
                        &response,
                        true,
                    ));
                }
                Err(IoError::Timeout) => continue,
                Err(IoError::Disconnected) => {
                    return Err(TransportError::new(
                        "usb_at_disconnected",
                        "USB AT device disconnected before command completion",
                        &response,
                        true,
                    ));
                }
                Err(IoError::Other) => {
                    return Err(TransportError::new(
                        "usb_at_read_failed",
                        "USB AT response read failed",
                        &response,
                        true,
                    ));
                }
            };
            if length == 0 {
                continue;
            }
            if length > max_bytes.saturating_sub(response.len()) {
                return Err(TransportError::new(
                    "usb_at_response_too_large",
                    "USB AT response exceeds its byte limit",
                    &response,
                    true,
                ));
            }
            self.capture_input(&buffer[..length]);
            response.extend_from_slice(&buffer[..length]);
            let terminal = response_terminal(&response);
            if let Some(payload) = follow_up {
                if !prompted {
                    // A confirmed modem rejection is a complete response, not
                    // transport failure; never send a payload after rejection.
                    if let Some((end, false)) = terminal {
                        return Ok(normalize_response(&response[..end]));
                    }
                    if !response_has_prompt(&response) {
                        continue;
                    }
                    self.write_once(payload, deadline, &response, true)?;
                    prompted = true;
                    continue;
                }
            }
            if let Some((end, _)) = terminal {
                // The entire transfer already reached capture_input, including
                // trailing URCs. Only this command's framed reply is returned.
                return Ok(normalize_response(&response[..end]));
            }
        }
        if require_terminal && !response.is_empty() {
            return Err(incomplete_status_response(&response));
        }
        if follow_up.is_none() && !response.is_empty() {
            // Compatibility: a normal command may return partial data at its
            // deadline. Callers requiring complete OK must still check it.
            return Ok(normalize_response(&response));
        }
        Err(TransportError::new(
            "usb_at_timeout",
            "USB AT command timed out before completion",
            &response,
            true,
        ))
    }
}
impl Transport for NativeTransport {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        let Self { io, _owner, .. } = *self;
        // The consuming I/O implementation closes its real handle before
        // returning. Keep the admission lease through error and unwind too.
        let result = io.checked_close();
        drop(_owner);
        result
    }

    fn capture_reenumeration_anchor(
        &mut self,
        deadline: Instant,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<ReenumerationAnchor, TransportError> {
        reconnect::check_finite(deadline, &guard)?;
        self.io.capture_reenumeration_anchor(deadline, guard)
    }
    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
        self.io.physical_target()
    }
    fn command_with_guard(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<String, TransportError> {
        self.command_guard = Some(guard);
        let result = self.command(command, follow_up, timeout, max_bytes);
        self.command_guard = None;
        result
    }
    fn set_line_capture(&mut self, capture: LineCapture) {
        self.call_capture = Some(capture);
    }
    fn poll_call_capture(&mut self, timeout: Duration) -> Result<(), TransportError> {
        let mut buffer = [0_u8; READ_BYTES];
        match self.io.read(&mut buffer, timeout.max(MIN_TIMEOUT)) {
            Ok(length) if length <= buffer.len() => {
                self.capture_input(&buffer[..length]);
                Ok(())
            }
            Err(IoError::Timeout) => Ok(()),
            _ => Err(TransportError::new(
                "usb_at_disconnected",
                "USB AT observation transport failed",
                &[],
                false,
            )),
        }
    }
    fn command(
        &mut self,
        command: &str,
        follow_up: Option<&[u8]>,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<String, TransportError> {
        self.execute_command(command, follow_up, timeout, max_bytes, false)
    }
    fn status_command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<String, TransportError> {
        self.execute_command(command, None, timeout, max_bytes, true)
    }
    fn description(&self) -> String {
        self.description.clone()
    }
}

fn terminal_line(line: &[u8]) -> Option<bool> {
    let line = std::str::from_utf8(line).ok()?.trim();
    if line == "OK" {
        return Some(true);
    }
    let upper = line.to_ascii_uppercase();
    if upper == "ERROR"
        || ["+CME ERROR:", "+CMS ERROR:"].iter().any(|prefix| {
            upper
                .strip_prefix(*prefix)
                .is_some_and(|detail| !detail.trim().is_empty())
        })
    {
        return Some(false);
    }
    None
}

fn response_terminal(response: &[u8]) -> Option<(usize, bool)> {
    let mut end = 0;
    for line in response.split_inclusive(|byte| *byte == b'\n') {
        end += line.len();
        // USB frame boundaries are not AT line boundaries. In particular a
        // fragment such as "+CME ERROR:" cannot complete the command.
        if line.last() == Some(&b'\n') {
            if let Some(ok) = terminal_line(line) {
                return Some((end, ok));
            }
        }
    }
    None
}

fn incomplete_status_response(response: &[u8]) -> TransportError {
    TransportError::new(
        "usb_at_status_incomplete_response",
        "USB AT status response has no complete terminal line",
        response,
        true,
    )
}
fn response_is_error(response: &[u8]) -> bool {
    let normalized = String::from_utf8_lossy(response)
        .replace("\r\n", "\n")
        .to_ascii_uppercase();
    normalized.contains("\nERROR\n")
        || normalized.ends_with("\nERROR")
        || normalized.contains("+CME ERROR:")
        || normalized.contains("+CMS ERROR:")
}
fn response_has_prompt(response: &[u8]) -> bool {
    String::from_utf8_lossy(response)
        .trim_end_matches([' ', '\t', '\r', '\n'])
        .ends_with('>')
}
fn probe_succeeded(response: &str) -> bool {
    let normalized = response.trim().replace("\r\n", "\n");
    !response_is_error(response.as_bytes()) && (normalized == "OK" || normalized.ends_with("\nOK"))
}
fn normalize_response(response: &[u8]) -> String {
    let text = String::from_utf8_lossy(response).replace("\r\r\n", "\r\n");
    text.trim()
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\r\n")
}

static USB_AT_OWNER: AtomicBool = AtomicBool::new(false);
#[derive(Clone)]
struct OwnerLease {
    _inner: Arc<OwnerLeaseInner>,
}
struct OwnerLeaseInner;
impl OwnerLease {
    fn acquire() -> Result<Self, TransportError> {
        USB_AT_OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                TransportError::new(
                    "usb_at_owner_busy",
                    "USB AT transport already has an owner",
                    &[],
                    false,
                )
            })?;
        Ok(Self {
            _inner: Arc::new(OwnerLeaseInner),
        })
    }
}
impl Drop for OwnerLeaseInner {
    fn drop(&mut self) {
        USB_AT_OWNER.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Candidate {
    interface: u8,
    alternate: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}
fn candidate(
    model: (u16, u16),
    interface: u8,
    alternate: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    endpoints: impl IntoIterator<Item = (u8, bool)>,
) -> Option<Candidate> {
    // Do not probe diagnostics, NMEA or QMI. These transfers can wedge macOS
    // IOUSBLib and cannot be made safe by a userspace timeout alone. QDC507
    // RNDIS mode moves its vendor-serial AT pair to interfaces 4/5; ECM/MBIM
    // network interfaces use different descriptors and are excluded here.
    if !supports_usb_at_interface(model, interface) || (class, subclass, protocol) != (0xff, 0, 0) {
        return None;
    }
    let bulk: Vec<_> = endpoints
        .into_iter()
        .filter(|(_, is_bulk)| *is_bulk)
        .map(|(address, _)| address)
        .collect();
    if bulk.len() != 2
        || bulk
            .iter()
            .any(|address| address & 0x0f == 0 || address & 0x70 != 0)
    {
        return None;
    }
    let incoming: Vec<_> = bulk
        .iter()
        .copied()
        .filter(|address| address & 0x80 != 0)
        .collect();
    let outgoing: Vec<_> = bulk
        .iter()
        .copied()
        .filter(|address| address & 0x80 == 0)
        .collect();
    if incoming.len() != 1 || outgoing.len() != 1 {
        return None;
    }
    let endpoint_in = incoming[0];
    let endpoint_out = outgoing[0];
    if !usb_at_endpoint_pair_matches(model, interface, endpoint_in, endpoint_out) {
        return None;
    }
    Some(Candidate {
        interface,
        alternate,
        endpoint_in,
        endpoint_out,
    })
}
fn probe(mut transport: NativeTransport) -> Result<NativeTransport, TransportError> {
    match transport.command("AT", None, PROBE_TIMEOUT, 4096) {
        Ok(response) if probe_succeeded(&response) => Ok(transport),
        // A failed probe may contain identifiers or unsolicited SMS. Never
        // return that response in an open/probe error or diagnostic message.
        _ => {
            Box::new(transport).checked_close()?;
            Err(TransportError::new(
                "usb_at_probe_failed",
                "USB AT interface did not confirm its probe",
                &[],
                false,
            ))
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use rusb::{Context, DeviceHandle, TransferType, UsbContext};

    pub(super) struct LibusbIo {
        pub(super) handle: DeviceHandle<Context>,
        pub(super) candidate: Candidate,
        pub(super) claimed: bool,
    }
    impl Drop for LibusbIo {
        fn drop(&mut self) {
            if self.claimed {
                let _ = self.handle.release_interface(self.candidate.interface);
            }
        }
    }
    fn io_error(error: rusb::Error) -> IoError {
        match error {
            rusb::Error::Timeout => IoError::Timeout,
            rusb::Error::NoDevice => IoError::Disconnected,
            _ => IoError::Other,
        }
    }
    impl Io for LibusbIo {
        fn checked_close(mut self: Box<Self>) -> Result<CloseDisposition, TransportError> {
            let released = self.handle.release_interface(self.candidate.interface);
            self.claimed = false; // no destructor retry can change this outcome
            drop(self); // actual libusb_close before returning even on error
            match released {
                Ok(()) => Ok(CloseDisposition::ReleasedAndHandleDropped),
                Err(rusb::Error::NoDevice) => Ok(CloseDisposition::DisconnectedAndHandleDropped),
                Err(_) => Err(close_unconfirmed()),
            }
        }

        fn capture_reenumeration_anchor(
            &mut self,
            deadline: Instant,
            guard: crate::module_radio::CallAdmission,
        ) -> Result<ReenumerationAnchor, TransportError> {
            reconnect::capture_native(&self.handle, self.candidate, deadline, guard)
        }
        fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
            // rusb obtains this Device from libusb_get_device(handle), retains
            // its refcount and clones the owning Context. No enumeration,
            // descriptor/serial reads or topology-based substitution occurs.
            Ok(PhysicalTarget {
                connection: PhysicalConnection::Native(self.handle.device()),
            })
        }
        fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
            self.handle
                .read_bulk(self.candidate.endpoint_in, buffer, timeout)
                .map_err(io_error)
        }
        fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, IoError> {
            self.handle
                .write_bulk(self.candidate.endpoint_out, data, timeout)
                .map_err(io_error)
        }
    }
    pub(super) fn open() -> Result<Box<dyn Transport>, TransportError> {
        let owner = OwnerLease::acquire()?;
        // Keep the libusb context stable across AT close/reopen. A retained
        // Device reference can then identify that same connection object;
        // a disconnected/re-enumerated device is never matched by address.
        // Initialization failures remain retryable on a subsequent open.
        static CONTEXT: std::sync::Mutex<Option<Context>> = std::sync::Mutex::new(None);
        let context = {
            let mut context = CONTEXT.lock().unwrap_or_else(|e| e.into_inner());
            if context.is_none() {
                *context = Some(Context::new().map_err(|_| {
                    TransportError::new(
                        "usb_at_open_failed",
                        "USB AT context initialization failed",
                        &[],
                        false,
                    )
                })?);
            }
            context.as_ref().expect("initialized context").clone()
        };
        let devices = context.devices().map_err(|_| {
            TransportError::new(
                "usb_at_open_failed",
                "USB AT device enumeration failed",
                &[],
                false,
            )
        })?;
        let mut last_error = None;
        for (vendor, product) in DEVICE_IDS {
            for device in devices.iter() {
                let Ok(descriptor) = device.device_descriptor() else {
                    continue;
                };
                if descriptor.vendor_id() != vendor || descriptor.product_id() != product {
                    continue;
                }
                let config = match device.active_config_descriptor() {
                    Ok(config) => config,
                    Err(_) => {
                        last_error = Some(TransportError::new(
                            "usb_at_open_failed",
                            "USB AT active configuration could not be read",
                            &[],
                            false,
                        ));
                        continue;
                    }
                };
                let mut candidates = Vec::new();
                for interface in config.interfaces() {
                    for alt in interface.descriptors() {
                        if let Some(value) = candidate(
                            (vendor, product),
                            alt.interface_number(),
                            alt.setting_number(),
                            alt.class_code(),
                            alt.sub_class_code(),
                            alt.protocol_code(),
                            alt.endpoint_descriptors()
                                .map(|ep| (ep.address(), ep.transfer_type() == TransferType::Bulk)),
                        ) {
                            candidates.push(value);
                        }
                    }
                }
                candidates.sort_by_key(|candidate| {
                    usb_at_interface_priority((vendor, product), candidate.interface)
                });
                if candidates.is_empty() {
                    last_error = Some(TransportError::new(
                        "usb_at_not_found",
                        "No supported USB AT bulk interface was found",
                        &[],
                        false,
                    ));
                }
                for candidate in candidates {
                    let handle = match device.open() {
                        Ok(handle) => handle,
                        Err(_) => {
                            last_error = Some(TransportError::new(
                                "usb_at_open_failed",
                                "USB AT device could not be opened",
                                &[],
                                false,
                            ));
                            continue;
                        }
                    };
                    if handle.claim_interface(candidate.interface).is_err() {
                        last_error = Some(TransportError::new(
                            "usb_at_open_failed",
                            "USB AT interface could not be claimed",
                            &[],
                            false,
                        ));
                        continue;
                    }
                    let io = LibusbIo {
                        handle,
                        candidate,
                        claimed: true,
                    };
                    if candidate.alternate != 0
                        && io
                            .handle
                            .set_alternate_setting(candidate.interface, candidate.alternate)
                            .is_err()
                    {
                        Box::new(io).checked_close()?;
                        last_error = Some(TransportError::new(
                            "usb_at_open_failed",
                            "USB AT alternate setting could not be selected",
                            &[],
                            false,
                        ));
                        continue;
                    }
                    let description = format!(
                        "USB AT · {vendor:04x}:{product:04x} interface {} out 0x{:02x} in 0x{:02x}",
                        candidate.interface, candidate.endpoint_out, candidate.endpoint_in
                    );
                    match probe(NativeTransport::new(
                        Box::new(io),
                        description,
                        Some(owner.clone()),
                    )) {
                        Ok(transport) => return Ok(Box::new(transport)),
                        Err(error) if error.code == "usb_at_close_unconfirmed" => {
                            return Err(error);
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            TransportError::new(
                "usb_at_not_found",
                "No supported USB AT device was found",
                &[],
                false,
            )
        }))
    }
}

pub(crate) fn open() -> Result<Box<dyn Transport>, TransportError> {
    #[cfg(target_os = "macos")]
    {
        macos::open()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(TransportError::new(
            "usb_at_unsupported",
            "Native USB AT transport is supported only on macOS",
            &[],
            false,
        ))
    }
}

#[cfg(test)]
mod tests {
    include!("usb_at_transport/tests.rs");
}
