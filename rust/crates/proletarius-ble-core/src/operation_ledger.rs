//! Physical BLE completion receipts. The FFI owns only metadata and digests;
//! raw APDUs enter through a transient request and are never kept in entries.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct ResultValue {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Fingerprint {
    action: String,
    reader: Option<String>,
    hex: Option<String>,
    connection_id: Option<String>,
}

impl Fingerprint {
    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for value in [
            &Some(&self.action),
            &self.reader.as_ref(),
            &self.hex.as_ref(),
            &self.connection_id.as_ref(),
        ] {
            match value {
                Some(value) => {
                    hasher.update([1]);
                    hasher.update(format!("{}:", value.len()).as_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0]),
            }
        }
        hasher.finalize().into()
    }
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    Admit {
        operation_id: String,
        bridge_id: String,
        fingerprint: Fingerprint,
        rejection: Option<ResultValue>,
    },
    Begin {
        operation_id: String,
        incarnation: u64,
        ledger_nonce: String,
    },
    Finish {
        operation_id: String,
        incarnation: u64,
        ledger_nonce: String,
        result: ResultValue,
    },
    RegisterConnection {
        reader: String,
        connection_id: String,
        operation_id: String,
    },
    ConfirmDisconnected {
        reader: String,
        connection_id: String,
        operation_id: String,
    },
    Status {
        operation_id: String,
        bridge_id: String,
    },
}

#[derive(Serialize)]
struct Response {
    dispatch: bool,
    accepted: bool,
    incarnation: Option<u64>,
    ledger_nonce: Option<String>,
    snapshot: Option<Snapshot>,
}

impl Response {
    fn accepted(value: bool) -> Self {
        Self {
            dispatch: false,
            accepted: value,
            incarnation: None,
            ledger_nonce: None,
            snapshot: None,
        }
    }
    fn snapshot(value: Snapshot) -> Self {
        Self {
            dispatch: false,
            accepted: false,
            incarnation: None,
            ledger_nonce: None,
            snapshot: Some(value),
        }
    }
}

#[derive(Debug, Serialize)]
struct Snapshot {
    operation_id: String,
    bridge_id: String,
    terminal: bool,
    result: ResultValue,
    connection_id: Option<String>,
    reader: Option<String>,
    closed: Option<bool>,
}

#[derive(Debug)]
enum Phase {
    Queued,
    Running,
    Terminal(ResultValue),
}

#[derive(Debug)]
struct Entry {
    fingerprint: [u8; 32],
    incarnation: u64,
    phase: Phase,
    connection_id: Option<String>,
    reader: Option<String>,
    connection_receipt: bool,
    disconnected: bool,
}

struct Ledger {
    bridge_id: String,
    ledger_nonce: String,
    capacity: usize,
    next_incarnation: u64,
    entries: HashMap<String, Entry>,
    terminal_order: VecDeque<String>,
    pending_id: Option<String>,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Ledger {
    fn new(capacity: usize, bridge_id: String) -> Option<Self> {
        if capacity == 0 {
            return None;
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).ok()?;
        let ledger_nonce = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Some(Self {
            bridge_id,
            ledger_nonce,
            capacity,
            next_incarnation: 1,
            entries: HashMap::new(),
            terminal_order: VecDeque::new(),
            pending_id: None,
        })
    }

    fn incarnation(&mut self) -> Option<u64> {
        let value = self.next_incarnation;
        self.next_incarnation = value.checked_add(1)?;
        Some(value)
    }

    fn unconfirmed(&self, operation_id: String, code: &str) -> Snapshot {
        Snapshot {
            operation_id,
            bridge_id: self.bridge_id.clone(),
            terminal: false,
            result: ResultValue {
                ok: false,
                hex: None,
                error_code: Some(code.into()),
            },
            connection_id: None,
            reader: None,
            closed: None,
        }
    }

    fn snapshot(&self, operation_id: &str, entry: &Entry) -> Snapshot {
        match &entry.phase {
            Phase::Queued | Phase::Running => {
                self.unconfirmed(operation_id.into(), "ble_operation_pending")
            }
            Phase::Terminal(result) => Snapshot {
                operation_id: operation_id.into(),
                bridge_id: self.bridge_id.clone(),
                terminal: true,
                result: result.clone(),
                connection_id: entry.connection_id.clone(),
                reader: entry.reader.clone(),
                closed: entry.connection_receipt.then_some(true),
            },
        }
    }

    fn room(&mut self) -> bool {
        if self.entries.len() < self.capacity {
            return true;
        }
        if let Some(id) = self.terminal_order.pop_front() {
            self.entries.remove(&id);
            true
        } else {
            false
        }
    }

    fn admit(
        &mut self,
        operation_id: String,
        bridge_id: String,
        fp: Fingerprint,
        rejection: Option<ResultValue>,
    ) -> Response {
        // Digest before any branch, and drop the request's raw hex on return.
        let digest = fp.digest();
        if bridge_id != self.bridge_id {
            return Response::snapshot(self.unconfirmed(operation_id, "ble_bridge_changed"));
        }
        if !valid_id(&operation_id) {
            return Response::snapshot(self.unconfirmed(operation_id, "ble_operation_unknown"));
        }
        if let Some(entry) = self.entries.get(&operation_id) {
            return Response::snapshot(if entry.fingerprint == digest {
                self.snapshot(&operation_id, entry)
            } else {
                self.unconfirmed(operation_id, "ble_operation_conflict")
            });
        }
        if !self.room() {
            return Response::snapshot(self.unconfirmed(operation_id, "ble_operation_unknown"));
        }
        let Some(incarnation) = self.incarnation() else {
            return Response::snapshot(self.unconfirmed(operation_id, "ble_operation_unknown"));
        };
        let failure = rejection.or_else(|| {
            self.pending_id.as_ref().map(|_| ResultValue {
                ok: false,
                hex: None,
                error_code: Some("ble_busy".into()),
            })
        });
        if let Some(result) = failure {
            self.entries.insert(
                operation_id.clone(),
                Entry {
                    fingerprint: digest,
                    incarnation,
                    phase: Phase::Terminal(result),
                    connection_id: fp.connection_id,
                    reader: fp.reader,
                    connection_receipt: false,
                    disconnected: false,
                },
            );
            self.terminal_order.push_back(operation_id.clone());
            return Response::snapshot(self.snapshot(&operation_id, &self.entries[&operation_id]));
        }
        let connection_id = fp.connection_id.or_else(|| {
            self.entries
                .values()
                .find(|entry| {
                    entry.connection_receipt
                        && !entry.disconnected
                        && (fp.reader.is_none() || entry.reader == fp.reader)
                })
                .and_then(|entry| entry.connection_id.clone())
        });
        self.entries.insert(
            operation_id.clone(),
            Entry {
                fingerprint: digest,
                incarnation,
                phase: Phase::Queued,
                connection_id,
                reader: fp.reader,
                connection_receipt: false,
                disconnected: false,
            },
        );
        self.pending_id = Some(operation_id);
        Response {
            dispatch: true,
            accepted: true,
            incarnation: Some(incarnation),
            ledger_nonce: Some(self.ledger_nonce.clone()),
            snapshot: None,
        }
    }

    fn begin(&mut self, operation_id: &str, incarnation: u64, ledger_nonce: &str) -> bool {
        if ledger_nonce != self.ledger_nonce {
            return false;
        }
        let Some(entry) = self.entries.get_mut(operation_id) else {
            return false;
        };
        if entry.incarnation != incarnation || !matches!(entry.phase, Phase::Queued) {
            return false;
        }
        entry.phase = Phase::Running;
        true
    }

    fn finish(
        &mut self,
        operation_id: &str,
        incarnation: u64,
        ledger_nonce: &str,
        result: ResultValue,
    ) -> bool {
        if ledger_nonce != self.ledger_nonce {
            return false;
        }
        let Some(entry) = self.entries.get_mut(operation_id) else {
            return false;
        };
        if entry.incarnation != incarnation || !matches!(entry.phase, Phase::Running) {
            return false;
        }
        entry.phase = Phase::Terminal(result);
        self.terminal_order.push_back(operation_id.into());
        if self.pending_id.as_deref() == Some(operation_id) {
            self.pending_id = None;
        }
        self.finish_closed_connections();
        true
    }

    fn register_connection(
        &mut self,
        reader: String,
        connection_id: String,
        operation_id: String,
    ) -> bool {
        if !valid_id(&connection_id) || !valid_id(&operation_id) {
            return false;
        }
        let digest = Fingerprint {
            action: "connection-close".into(),
            reader: Some(reader.clone()),
            hex: None,
            connection_id: Some(connection_id.clone()),
        }
        .digest();
        if let Some(entry) = self.entries.get(&operation_id) {
            return entry.connection_receipt && entry.fingerprint == digest;
        }
        if !self.room() {
            return false;
        }
        let Some(incarnation) = self.incarnation() else {
            return false;
        };
        self.entries.insert(
            operation_id,
            Entry {
                fingerprint: digest,
                incarnation,
                phase: Phase::Running,
                connection_id: Some(connection_id),
                reader: Some(reader),
                connection_receipt: true,
                disconnected: false,
            },
        );
        true
    }

    fn confirm_disconnected(&mut self, reader: &str, connection_id: &str, operation_id: &str) {
        let Some(entry) = self.entries.get_mut(operation_id) else {
            return;
        };
        if !entry.connection_receipt
            || entry.connection_id.as_deref() != Some(connection_id)
            || entry.reader.as_deref() != Some(reader)
            || !matches!(entry.phase, Phase::Running)
        {
            return;
        }
        entry.disconnected = true;
        self.finish_closed_connections();
    }

    fn finish_closed_connections(&mut self) {
        let eligible: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, connection)| {
                connection.connection_receipt
                    && connection.disconnected
                    && matches!(connection.phase, Phase::Running)
                    && !self.entries.values().any(|job| {
                        !job.connection_receipt
                            && (job.connection_id.is_none()
                                || job.connection_id == connection.connection_id)
                            && !matches!(job.phase, Phase::Terminal(_))
                    })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in eligible {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.phase = Phase::Terminal(ResultValue {
                    ok: true,
                    hex: None,
                    error_code: None,
                });
                self.terminal_order.push_back(id);
            }
        }
    }

    fn status(&self, operation_id: String, bridge_id: String) -> Snapshot {
        if bridge_id != self.bridge_id {
            return self.unconfirmed(operation_id, "ble_bridge_changed");
        }
        self.entries.get(&operation_id).map_or_else(
            || self.unconfirmed(operation_id.clone(), "ble_operation_unknown"),
            |entry| self.snapshot(&operation_id, entry),
        )
    }

    fn execute(&mut self, command: Command) -> Response {
        match command {
            Command::Admit {
                operation_id,
                bridge_id,
                fingerprint,
                rejection,
            } => self.admit(operation_id, bridge_id, fingerprint, rejection),
            Command::Begin {
                operation_id,
                incarnation,
                ledger_nonce,
            } => Response::accepted(self.begin(&operation_id, incarnation, &ledger_nonce)),
            Command::Finish {
                operation_id,
                incarnation,
                ledger_nonce,
                result,
            } => Response::accepted(self.finish(&operation_id, incarnation, &ledger_nonce, result)),
            Command::RegisterConnection {
                reader,
                connection_id,
                operation_id,
            } => Response::accepted(self.register_connection(reader, connection_id, operation_id)),
            Command::ConfirmDisconnected {
                reader,
                connection_id,
                operation_id,
            } => {
                self.confirm_disconnected(&reader, &connection_id, &operation_id);
                Response::accepted(true)
            }
            Command::Status {
                operation_id,
                bridge_id,
            } => Response::snapshot(self.status(operation_id, bridge_id)),
        }
    }
}

/// Callers must pass the handle returned by `new`; it must not be used after free.
/// Panics never unwind into Swift. A null result is interpreted as failure closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ledger_new(
    capacity: usize,
    bridge_id: *const c_char,
) -> *mut c_void {
    catch_unwind(|| {
        if bridge_id.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: The caller supplies a valid NUL-terminated string.
        let Some(bridge_id) = (unsafe { CStr::from_ptr(bridge_id) }).to_str().ok() else {
            return std::ptr::null_mut();
        };
        Ledger::new(capacity, bridge_id.into()).map_or(std::ptr::null_mut(), |ledger| {
            Box::into_raw(Box::new(Mutex::new(ledger))).cast()
        })
    })
    .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ledger_call(
    handle: *mut c_void,
    request: *const u8,
    length: usize,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || request.is_null() || length > 1_048_576 {
            return std::ptr::null_mut();
        }
        // SAFETY: The caller owns a live handle and readable request buffer.
        let ledger = unsafe { &*handle.cast::<Mutex<Ledger>>() };
        let bytes = unsafe { std::slice::from_raw_parts(request, length) };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return std::ptr::null_mut();
        };
        let Ok(mut ledger) = ledger.lock() else {
            return std::ptr::null_mut();
        };
        let Ok(json) = serde_json::to_vec(&ledger.execute(command)) else {
            return std::ptr::null_mut();
        };
        CString::new(json).map_or(std::ptr::null_mut(), CString::into_raw)
    }))
    .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ledger_string_free(value: *mut c_char) {
    if !value.is_null() {
        let _ = catch_unwind(|| {
            // SAFETY: The pointer came from `switch_ble_ledger_call` exactly once.
            drop(unsafe { CString::from_raw(value) });
        });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_ble_ledger_free(handle: *mut c_void) {
    if !handle.is_null() {
        let _ = catch_unwind(|| {
            // SAFETY: The pointer came from `switch_ble_ledger_new` exactly once.
            drop(unsafe { Box::from_raw(handle.cast::<Mutex<Ledger>>()) });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fp(connection_id: Option<&str>) -> Fingerprint {
        Fingerprint {
            action: "apdu".into(),
            reader: Some("BLE:reader".into()),
            hex: Some("00840000".into()),
            connection_id: connection_id.map(str::to_string),
        }
    }
    fn good() -> ResultValue {
        ResultValue {
            ok: true,
            hex: Some("9000".into()),
            error_code: None,
        }
    }
    #[derive(Debug, PartialEq, Eq)]
    struct Ticket {
        incarnation: u64,
        ledger_nonce: String,
    }
    fn ticket(reply: Response) -> Ticket {
        assert!(reply.dispatch);
        Ticket {
            incarnation: reply.incarnation.unwrap(),
            ledger_nonce: reply.ledger_nonce.unwrap(),
        }
    }
    fn begin(ledger: &mut Ledger, id: &str, ticket: &Ticket) -> bool {
        ledger.begin(id, ticket.incarnation, &ticket.ledger_nonce)
    }
    fn finish(ledger: &mut Ledger, id: &str, ticket: &Ticket, result: ResultValue) -> bool {
        ledger.finish(id, ticket.incarnation, &ticket.ledger_nonce, result)
    }
    #[test]
    fn dedupe_fence_eviction_and_stale_incarnation() {
        let mut ledger = Ledger::new(2, "epoch".into()).unwrap();
        let first = ticket(ledger.admit("same".into(), "epoch".into(), fp(None), None));
        assert_eq!(
            ledger
                .admit("same".into(), "epoch".into(), fp(None), None)
                .snapshot
                .unwrap()
                .result
                .error_code
                .as_deref(),
            Some("ble_operation_pending")
        );
        let mut changed = fp(None);
        changed.hex = Some("00A40000".into());
        assert_eq!(
            ledger
                .admit("same".into(), "epoch".into(), changed, None)
                .snapshot
                .unwrap()
                .result
                .error_code
                .as_deref(),
            Some("ble_operation_conflict")
        );
        assert!(begin(&mut ledger, "same", &first));
        assert!(!begin(&mut ledger, "same", &first));
        assert_eq!(
            ledger
                .admit("busy".into(), "epoch".into(), fp(None), None)
                .snapshot
                .unwrap()
                .result
                .error_code
                .as_deref(),
            Some("ble_busy")
        );
        assert!(finish(&mut ledger, "same", &first, good()));
        assert!(!finish(&mut ledger, "same", &first, good()));
        ledger.admit("evict".into(), "epoch".into(), fp(None), Some(good()));
        ledger.admit("evict-again".into(), "epoch".into(), fp(None), Some(good()));
        assert_eq!(
            ledger
                .status("same".into(), "epoch".into())
                .result
                .error_code
                .as_deref(),
            Some("ble_operation_unknown")
        );
        let second = ticket(ledger.admit("same".into(), "epoch".into(), fp(None), None));
        assert_ne!(first, second);
        assert!(!begin(&mut ledger, "same", &first));
        assert!(!finish(&mut ledger, "same", &first, good()));
        assert!(begin(&mut ledger, "same", &second));
        assert_eq!(
            ledger
                .status("same".into(), "old-epoch".into())
                .result
                .error_code
                .as_deref(),
            Some("ble_bridge_changed")
        );
    }
    #[test]
    fn close_waits_for_exact_disconnect_and_jobs() {
        let mut ledger = Ledger::new(8, "epoch".into()).unwrap();
        assert!(ledger.register_connection("BLE:reader".into(), "conn-a".into(), "close-a".into()));
        let job = ticket(ledger.admit("job".into(), "epoch".into(), fp(Some("conn-a")), None));
        begin(&mut ledger, "job", &job);
        ledger.confirm_disconnected("BLE:reader", "conn-b", "close-a");
        assert!(!ledger.status("close-a".into(), "epoch".into()).terminal);
        ledger.confirm_disconnected("BLE:reader", "conn-a", "close-a");
        assert!(!ledger.status("close-a".into(), "epoch".into()).terminal);
        finish(&mut ledger, "job", &job, good());
        assert_eq!(
            ledger.status("close-a".into(), "epoch".into()).closed,
            Some(true)
        );
        assert!(!ledger.register_connection(
            "BLE:reader".into(),
            "conn-b".into(),
            "close-a".into()
        ));
    }

    #[test]
    fn ticket_cannot_cross_ledgers_with_the_same_public_bridge_id() {
        let mut old = Ledger::new(1, "reused-bridge-id".into()).unwrap();
        let old_ticket =
            ticket(old.admit("same-id".into(), "reused-bridge-id".into(), fp(None), None));
        let mut replacement = Ledger::new(1, "reused-bridge-id".into()).unwrap();
        let new_ticket =
            ticket(replacement.admit("same-id".into(), "reused-bridge-id".into(), fp(None), None));
        assert_eq!(old_ticket.incarnation, new_ticket.incarnation);
        assert_ne!(old_ticket.ledger_nonce, new_ticket.ledger_nonce);
        assert!(!begin(&mut replacement, "same-id", &old_ticket));
        assert!(begin(&mut replacement, "same-id", &new_ticket));
        assert!(!finish(&mut replacement, "same-id", &old_ticket, good()));
        assert!(
            !replacement
                .status("same-id".into(), "reused-bridge-id".into())
                .terminal
        );
        assert!(finish(&mut replacement, "same-id", &new_ticket, good()));
    }

    #[test]
    fn ffi_rejects_bad_input_and_poisoned_ledger_without_unwinding() {
        let bridge_id = CString::new("epoch").unwrap();
        assert!(unsafe { switch_ble_ledger_new(1, std::ptr::null()) }.is_null());
        assert!(unsafe { switch_ble_ledger_new(0, bridge_id.as_ptr()) }.is_null());
        let invalid_utf8 = [0xff_u8, 0];
        assert!(unsafe { switch_ble_ledger_new(1, invalid_utf8.as_ptr().cast()) }.is_null());
        let handle = unsafe { switch_ble_ledger_new(1, bridge_id.as_ptr()) };
        assert!(!handle.is_null());
        let request = br#"{"command":"status","operation_id":"missing","bridge_id":"epoch"}"#;
        assert!(
            unsafe {
                switch_ble_ledger_call(std::ptr::null_mut(), request.as_ptr(), request.len())
            }
            .is_null()
        );
        assert!(
            unsafe { switch_ble_ledger_call(handle, std::ptr::null(), request.len()) }.is_null()
        );
        assert!(unsafe { switch_ble_ledger_call(handle, request.as_ptr(), 1_048_577) }.is_null());
        assert!(unsafe { switch_ble_ledger_call(handle, b"{".as_ptr(), 1) }.is_null());
        let address = handle as usize;
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(move || {
                    let handle = address as *mut c_void;
                    for _ in 0..32 {
                        let output = unsafe {
                            switch_ble_ledger_call(handle, request.as_ptr(), request.len())
                        };
                        assert!(!output.is_null());
                        let response: serde_json::Value =
                            serde_json::from_slice(unsafe { CStr::from_ptr(output).to_bytes() })
                                .unwrap();
                        assert_eq!(
                            response["snapshot"]["result"]["error_code"],
                            "ble_operation_unknown"
                        );
                        unsafe { switch_ble_ledger_string_free(output) };
                    }
                });
            }
        });
        let poisoned = std::thread::spawn(move || {
            let ledger = unsafe { &*(address as *mut Mutex<Ledger>) };
            let _guard = ledger.lock().unwrap();
            panic!("test poisoned mutex");
        });
        assert!(poisoned.join().is_err());
        assert!(
            unsafe { switch_ble_ledger_call(handle, request.as_ptr(), request.len()) }.is_null()
        );
        unsafe { switch_ble_ledger_free(handle) };
    }
}
