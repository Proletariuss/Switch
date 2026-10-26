//! Closed-vocabulary module lifecycle evidence. This is best-effort telemetry,
//! not an ownership ledger. Callers pass copied facts after releasing owner locks.
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const WINDOW: Duration = Duration::from_secs(30);
const STAGES: usize = 17;
const RESULTS: usize = 5;
const SLOTS: usize = STAGES * RESULTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    Startup,
    SerialProbe,
    UsbOpen,
    TransportClaim,
    InboxBind,
    SessionOpen,
    SerialInitialize,
    RadioConfigure,
    RadioBootstrap,
    EsimWarmup,
    MetadataRefresh,
    SmsReady,
    ReservationAcquire,
    ReservationRelease,
    Admission,
    Physical,
    Shutdown,
}
impl Stage {
    fn index(self) -> usize {
        self as usize
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResultKind {
    Started,
    Succeeded,
    Failed,
    Deferred,
    WaiterEnded,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Transport {
    Unknown,
    Serial,
    Usb,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Owner {
    None,
    Unknown,
    Esim,
    ModuleProfileNotes,
    DeviceSms,
    Call,
    CallMode,
    Reboot,
    Diagnostic,
    Vowifi,
    SharedVoice,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Code {
    None,
    Busy,
    NoDevice,
    Timeout,
    Stale,
    Stopped,
    Invalid,
    TransportFailed,
    PolicyUnavailable,
    ArchiveUnavailable,
    InboxUnavailable,
    WorkerFailed,
    CleanupUnconfirmed,
    InitializationIncomplete,
    AdmissionRevoked,
    ReservationConflict,
    SharedVoicePin,
    VoicePin,
    ReconnectPin,
    QueueFull,
    GateTimeout,
    BindingMissing,
    OutcomeUnknown,
    Other,
}
impl Code {
    /// Never persist caller text. Unknown/new error codes remain a fixed value.
    pub(crate) fn from_error(value: &str) -> Self {
        match value {
            "usb_at_not_found" | "at_port_not_found" => Self::NoDevice,
            "module_radio_busy"
            | "usb_at_busy"
            | "serial_command_busy"
            | "serial_actor_busy"
            | "serial_initialization_busy"
            | "serial_card_busy" => Self::Busy,
            "usb_at_wait_timeout"
            | "usb_at_timeout"
            | "radio_deadline"
            | "radio_plan_timeout"
            | "serial_command_timeout" => Self::Timeout,
            "module_startup_warm_cleanup_unconfirmed"
            | "usb_at_close_unconfirmed"
            | "module_esim_release_unconfirmed"
            | "module_call_mode_read_release_unconfirmed" => Self::CleanupUnconfirmed,
            "module_startup_worker_failed" | "usb_at_worker_failed" => Self::WorkerFailed,
            "module_startup_archive_unavailable" => Self::ArchiveUnavailable,
            "module_startup_inbox_unavailable" => Self::InboxUnavailable,
            "native_serial_initialization_incomplete" => Self::InitializationIncomplete,
            "flight_mode_state_read_failed" | "flight_mode_state_write_failed" => {
                Self::PolicyUnavailable
            }
            "radio_outcome_unknown" | "module_radio_outcome_unconfirmed" => Self::OutcomeUnknown,
            "radio_stale_generation" | "usb_at_stale_session" | "serial_card_binding_changed" => {
                Self::Stale
            }
            "backend_stopping" | "usb_at_stopping" => Self::Stopped,
            "invalid_radio_request" | "usb_at_invalid_request" => Self::Invalid,
            "legacy_recovery_required" | "module_recovery_required" => Self::AdmissionRevoked,
            "usb_at_unavailable"
            | "usb_at_io"
            | "usb_at_disconnected"
            | "flight_mode_read_failed"
            | "serial_command_failed" => Self::TransportFailed,
            _ => Self::Other,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerFacts {
    pub(crate) owner: Owner,
    pub(crate) shared_voice_pins: u16,
    pub(crate) voice_pins: u16,
    pub(crate) reconnect_pins: u16,
    pub(crate) queued_jobs: u16,
    pub(crate) outcome_unknown: bool,
}
impl Default for OwnerFacts {
    fn default() -> Self {
        Self {
            owner: Owner::Unknown,
            shared_voice_pins: 0,
            voice_pins: 0,
            reconnect_pins: 0,
            queued_jobs: 0,
            outcome_unknown: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub(crate) stage: Stage,
    pub(crate) result: ResultKind,
    pub(crate) code: Code,
    pub(crate) transport: Transport,
    pub(crate) owner: OwnerFacts,
    pub(crate) duration_ms: u64,
    pub(crate) suppressed_repeats: u32,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature {
    code: Code,
    transport: Transport,
    owner: OwnerFacts,
}
#[derive(Clone, Copy)]
struct Stamp {
    signature: Signature,
    emitted: Instant,
    repeats: u32,
}
struct Limiter {
    slots: [Option<Stamp>; SLOTS],
    terminal: [Option<ResultKind>; STAGES],
}
impl Default for Limiter {
    fn default() -> Self {
        Self {
            slots: [None; SLOTS],
            terminal: [None; STAGES],
        }
    }
}
impl Limiter {
    fn observe(&mut self, mut event: Event, now: Instant) -> Option<Event> {
        let stage = event.stage.index();
        let terminal = matches!(event.result, ResultKind::Succeeded | ResultKind::Failed);
        let changed = terminal && self.terminal[stage] != Some(event.result);
        if terminal {
            self.terminal[stage] = Some(event.result);
        }
        let signature = Signature {
            code: event.code,
            transport: event.transport,
            owner: event.owner,
        };
        let slot = &mut self.slots[stage * RESULTS + event.result as usize];
        if let Some(previous) = slot {
            if previous.signature == signature
                && !changed
                && now.saturating_duration_since(previous.emitted) < WINDOW
            {
                previous.repeats = previous.repeats.saturating_add(1);
                return None;
            }
            event.suppressed_repeats = previous.repeats;
        }
        *slot = Some(Stamp {
            signature,
            emitted: now,
            repeats: 0,
        });
        Some(event)
    }
}
static LIMITER: OnceLock<Mutex<Limiter>> = OnceLock::new();

/// Fixed-size, nonblocking deduplication; no identities, tokens, paths, commands
/// or free-form messages are accepted. Routine unchanged polling stays silent.
pub(crate) fn record(
    stage: Stage,
    result: ResultKind,
    code: Code,
    transport: Transport,
    owner: OwnerFacts,
    duration: Duration,
) {
    let Some(recorder) = super::RECORDER.get() else {
        return;
    };
    let limiter = LIMITER.get_or_init(|| Mutex::new(Limiter::default()));
    let event = {
        let Ok(mut limiter) = limiter.try_lock() else {
            recorder
                .shared
                .counters
                .dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        };
        limiter.observe(
            Event {
                stage,
                result,
                code,
                transport,
                owner,
                duration_ms: duration.as_millis().min(u64::MAX as u128) as u64,
                suppressed_repeats: 0,
            },
            Instant::now(),
        )
    };
    if let Some(detail) = event {
        recorder.emit(super::Event::Module { detail });
    }
}
pub(crate) fn startup(stage: Stage, result: ResultKind, code: Code, transport: Transport) {
    record(
        stage,
        result,
        code,
        transport,
        OwnerFacts::default(),
        Duration::ZERO,
    );
}
struct Step {
    stage: Stage,
    transport: Transport,
    start: Instant,
    finished: bool,
}
impl Step {
    fn new(stage: Stage, transport: Transport) -> Self {
        startup(stage, ResultKind::Started, Code::None, transport);
        Self {
            stage,
            transport,
            start: Instant::now(),
            finished: false,
        }
    }
    fn finish(&mut self, result: ResultKind, code: Code) {
        self.finished = true;
        record(
            self.stage,
            result,
            code,
            self.transport,
            OwnerFacts::default(),
            self.start.elapsed(),
        );
    }
}
impl Drop for Step {
    fn drop(&mut self) {
        if !self.finished {
            // A waiter ending is not a physical cleanup receipt.
            record(
                self.stage,
                ResultKind::WaiterEnded,
                Code::None,
                self.transport,
                OwnerFacts::default(),
                self.start.elapsed(),
            );
        }
    }
}
pub(crate) async fn startup_step<T, E>(
    stage: Stage,
    transport: Transport,
    work: impl Future<Output = Result<T, E>>,
    code: fn(&E) -> &'static str,
) -> Result<T, E> {
    let mut step = Step::new(stage, transport);
    let result = work.await;
    match &result {
        Ok(_) => step.finish(ResultKind::Succeeded, Code::None),
        Err(error) => step.finish(ResultKind::Failed, Code::from_error(code(error))),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(result: ResultKind, code: Code) -> Event {
        Event {
            stage: Stage::RadioBootstrap,
            result,
            code,
            transport: Transport::Usb,
            owner: OwnerFacts::default(),
            duration_ms: 12,
            suppressed_repeats: 0,
        }
    }
    #[test]
    fn closed_dto_rejects_unknown_fields_enums_and_sensitive_text() {
        let event = event(ResultKind::Failed, Code::Busy);
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(
            serde_json::from_value::<Event>(value.clone()).unwrap(),
            event
        );
        for field in ["command", "imei", "iccid", "payload", "path", "token"] {
            let mut changed = value.clone();
            changed
                .as_object_mut()
                .unwrap()
                .insert(field.into(), "private".into());
            assert!(serde_json::from_value::<Event>(changed).is_err());
        }
        let mut changed = value.clone();
        changed["code"] = "AT+CGSN private".into();
        assert!(serde_json::from_value::<Event>(changed).is_err());
        let mut changed = value;
        changed["owner"]["token"] = "private".into();
        assert!(serde_json::from_value::<Event>(changed).is_err());
        assert_eq!(Code::from_error("AT+CGSN private"), Code::Other);
        assert_eq!(Code::from_error("/Users/private/secret"), Code::Other);
    }
    #[test]
    fn retries_are_bounded_but_recovery_is_immediate() {
        let mut limiter = Limiter::default();
        let now = Instant::now();
        assert!(
            limiter
                .observe(event(ResultKind::Started, Code::None), now)
                .is_some()
        );
        assert!(
            limiter
                .observe(event(ResultKind::Failed, Code::Busy), now)
                .is_some()
        );
        for n in 1..15 {
            let at = now + Duration::from_secs(n * 2);
            assert!(
                limiter
                    .observe(event(ResultKind::Started, Code::None), at)
                    .is_none()
            );
            assert!(
                limiter
                    .observe(event(ResultKind::Failed, Code::Busy), at)
                    .is_none()
            );
        }
        let repeated = limiter
            .observe(event(ResultKind::Failed, Code::Busy), now + WINDOW)
            .unwrap();
        assert_eq!(repeated.suppressed_repeats, 14);
        assert!(
            limiter
                .observe(event(ResultKind::Succeeded, Code::None), now + WINDOW)
                .is_some()
        );
        assert!(
            limiter
                .observe(event(ResultKind::Failed, Code::Busy), now + WINDOW)
                .is_some()
        );
        assert_eq!(limiter.slots.len(), STAGES * RESULTS);
    }
    #[test]
    fn changed_owner_or_pin_reason_is_visible() {
        let mut limiter = Limiter::default();
        let now = Instant::now();
        let mut sample = event(ResultKind::Deferred, Code::ReservationConflict);
        sample.stage = Stage::ReservationRelease;
        sample.owner.owner = Owner::Esim;
        assert!(limiter.observe(sample, now).is_some());
        assert!(limiter.observe(sample, now).is_none());
        sample.code = Code::SharedVoicePin;
        sample.owner.shared_voice_pins = 1;
        assert!(limiter.observe(sample, now).is_some());
    }
    #[tokio::test]
    async fn wrapper_preserves_results_and_never_transforms_failure() {
        let result = startup_step(
            Stage::RadioBootstrap,
            Transport::Usb,
            async { Err::<(), _>("unchanged") },
            |error| *error,
        )
        .await;
        assert_eq!(result, Err("unchanged"));
        let result = startup_step(
            Stage::SmsReady,
            Transport::Usb,
            async { Ok::<_, &'static str>(17) },
            |error| *error,
        )
        .await;
        assert_eq!(result, Ok(17));
    }
}
