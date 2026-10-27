//! Native cellular call authority. Every state transition and facts admission
//! shares one short lock; physical commands and durable writes run elsewhere.
use crate::call_facts::{NativeFact, NativeFactAction, NativeSink};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Ring,
    Clip(String),
    Connect,
    Hangup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DialTicket {
    pub(crate) boundary: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub(crate) display: Value,
    pub(crate) revision: u64,
    pub(crate) boundary: u64,
    pub(crate) desired_active: bool,
    pub(crate) pending_dial: bool,
    pub(crate) safe_idle: bool,
    pub(crate) applied: bool,
    pub(crate) history_error: Option<String>,
}

struct PendingDial {
    ticket: DialTicket,
    connected_at: Option<OffsetDateTime>,
}
struct Inner {
    display: Value,
    revision: u64,
    boundary: u64,
    desired_active: bool,
    safe_idle: bool,
    pending: Option<PendingDial>,
    ring_at: Option<OffsetDateTime>,
    ring_number: String,
    owner_id: String,
    owner_epoch: u64,
    retired: bool,
}

#[derive(Clone)]
pub(crate) struct State {
    inner: Arc<Mutex<Inner>>,
    sink: NativeSink,
}

impl State {
    pub(crate) fn new(sink: NativeSink) -> Self {
        Self {
            sink,
            inner: Arc::new(Mutex::new(Inner {
                display: display("unknown", ""),
                revision: 0,
                boundary: 0,
                desired_active: false,
                safe_idle: false,
                pending: None,
                ring_at: None,
                ring_number: String::new(),
                owner_id: "module-cs".into(),
                owner_epoch: 1,
                retired: false,
            })),
        }
    }

    pub(crate) fn sink(&self) -> NativeSink {
        self.sink.clone()
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.project(
            &self.inner.lock().unwrap_or_else(|error| error.into_inner()),
            false,
        )
    }
    pub(crate) fn query_revision(&self) -> u64 {
        self.snapshot().revision
    }
    pub(crate) fn invalidate_observation(&self) -> Snapshot {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired {
            return self.project(&state, false);
        }
        state.revision += 1;
        state.boundary += 1;
        state.pending = None;
        state.ring_at = None;
        state.ring_number.clear();
        state.display = display("unknown", "");
        state.safe_idle = false;
        // Losing a physical observation is not evidence that its existing
        // call ended. Keep protection and its archive until a true terminal.
        self.project(&state, true)
    }
    pub(crate) fn revision_current(&self, revision: u64) -> bool {
        let state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        !state.retired && state.revision == revision
    }
    pub(crate) fn dial_current(&self, ticket: DialTicket) -> bool {
        let state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        !state.retired
            && state.boundary == ticket.boundary
            && state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.ticket == ticket)
    }

    pub(crate) fn start_owner(
        &self,
        owner_id: String,
        epoch: u64,
        at: OffsetDateTime,
    ) -> Result<(), String> {
        if owner_id.trim().is_empty()
            || owner_id.len() > 96
            || owner_id.chars().any(char::is_control)
            || epoch == 0
        {
            return Err("invalid native call owner".into());
        }
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired {
            return Err("native call state retired".into());
        }
        if state.owner_id == owner_id && state.owner_epoch == epoch {
            return Ok(());
        }
        if epoch <= state.owner_epoch {
            return Err("native call owner epoch did not advance".into());
        }
        self.push(&state, at, NativeFactAction::Retire);
        state.owner_id = owner_id;
        state.owner_epoch = epoch;
        state.revision += 1;
        state.boundary += 1;
        state.pending = None;
        state.ring_at = None;
        state.ring_number.clear();
        state.display = display("unknown", "");
        state.desired_active = false;
        state.safe_idle = false;
        Ok(())
    }

    pub(crate) fn apply_event(&self, event: Event, at: OffsetDateTime) -> Snapshot {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired {
            return self.project(&state, false);
        }
        state.revision += 1;
        match event {
            Event::Ring => {
                state.boundary += 1;
                state.pending = None;
                state.ring_at = Some(at);
                state.display = display("incoming", &state.ring_number);
                state.desired_active = true;
                state.safe_idle = false;
                self.push(
                    &state,
                    at,
                    NativeFactAction::Begin {
                        incoming: true,
                        number: state.ring_number.clone(),
                    },
                );
            }
            Event::Clip(number) => {
                if !number.is_empty() {
                    state.ring_number = number.clone();
                    if live(&state.display) {
                        state.display["number"] = json!(number);
                    }
                    self.push(&state, at, NativeFactAction::Number(number));
                }
            }
            Event::Connect => {
                if let Some(pending) = &mut state.pending {
                    if pending.connected_at.is_none() {
                        pending.connected_at = Some(at);
                    }
                } else {
                    self.push(&state, at, NativeFactAction::Answered);
                }
                state.ring_at = None;
                let number = number(&state.display);
                state.display = display("active", &number);
                state.desired_active = true;
                state.safe_idle = false;
            }
            Event::Hangup => {
                state.boundary += 1;
                state.pending = None;
                state.ring_at = None;
                state.ring_number.clear();
                state.display = display("idle", "");
                state.desired_active = false;
                state.safe_idle = true;
                self.push(&state, at, NativeFactAction::End);
            }
        }
        self.project(&state, true)
    }

    pub(crate) fn apply_query(
        &self,
        revision: u64,
        observed: Value,
        safe_idle: bool,
        at: OffsetDateTime,
    ) -> Snapshot {
        self.apply_query_inner(revision, observed, safe_idle, false, at)
    }

    pub(crate) fn apply_unconfirmed_hangup_query(
        &self,
        revision: u64,
        observed: Value,
        at: OffsetDateTime,
    ) -> Snapshot {
        self.apply_query_inner(revision, observed, false, true, at)
    }

    fn apply_query_inner(
        &self,
        revision: u64,
        mut observed: Value,
        safe_idle: bool,
        retain_hangup_protection: bool,
        at: OffsetDateTime,
    ) -> Snapshot {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired || state.revision != revision {
            return self.project(&state, false);
        }
        let observed_state = observed["state"].as_str().unwrap_or("unknown");
        if matches!(observed_state, "idle" | "unknown") && fresh_ring(&state, at) {
            observed = display("incoming", &state.ring_number);
        }
        let kind = observed["state"].as_str().unwrap_or("unknown").to_owned();
        state.revision += 1;
        state.safe_idle = kind == "idle" && safe_idle && state.pending.is_none();
        if retain_hangup_protection && !state.safe_idle {
            // An acknowledged ATH without terminal evidence must retain its
            // lease even if no earlier query established a live call. The
            // revision check above keeps newer RING, terminal and binding
            // observations authoritative.
            state.desired_active = true;
        }
        if live(&observed) && state.pending.is_none() {
            state.desired_active = true;
            self.push(
                &state,
                at,
                NativeFactAction::Snapshot {
                    state: kind,
                    number: number(&observed),
                },
            );
        } else if state.safe_idle {
            state.desired_active = false;
            state.ring_at = None;
            state.ring_number.clear();
            self.push(
                &state,
                at,
                NativeFactAction::Snapshot {
                    state: "idle".into(),
                    number: String::new(),
                },
            );
        }
        // A query while ATD is pending cannot synthesize a begin or end. Keep
        // its provisional UI and early CONNECT until that command completes.
        if state.pending.is_none() {
            state.display = observed;
        }
        self.project(&state, true)
    }

    pub(crate) fn begin_dial(&self, _at: OffsetDateTime) -> Option<DialTicket> {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired || !state.safe_idle || state.desired_active || state.pending.is_some() {
            return None;
        }
        state.boundary += 1;
        state.revision += 1;
        let ticket = DialTicket {
            boundary: state.boundary,
        };
        state.pending = Some(PendingDial {
            ticket,
            connected_at: None,
        });
        state.safe_idle = false;
        state.desired_active = true;
        state.display = display("dialing", "");
        Some(ticket)
    }

    pub(crate) fn complete_dial(
        &self,
        ticket: DialTicket,
        number: &str,
        at: OffsetDateTime,
    ) -> Snapshot {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired
            || state.boundary != ticket.boundary
            || !state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.ticket == ticket)
        {
            return self.project(&state, false);
        }
        let pending = state.pending.take().expect("checked pending dial");
        let mut facts = vec![self.fact(
            &state,
            at,
            NativeFactAction::Begin {
                incoming: false,
                number: number.into(),
            },
        )];
        if let Some(at) = pending.connected_at {
            facts.push(self.fact(&state, at, NativeFactAction::Answered));
        }
        let _ = self.sink.try_push_batch(facts);
        state.boundary += 1;
        state.revision += 1;
        state.display = display(
            if pending.connected_at.is_some() {
                "active"
            } else {
                "dialing"
            },
            number,
        );
        state.desired_active = true;
        state.safe_idle = false;
        self.project(&state, true)
    }

    pub(crate) fn cancel_dial(&self, ticket: DialTicket) -> Snapshot {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired
            || state.boundary != ticket.boundary
            || !state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.ticket == ticket)
        {
            return self.project(&state, false);
        }
        state.pending = None;
        state.boundary += 1;
        state.revision += 1;
        // A failed ATD needs a fresh CLCC reconciliation. Do not fabricate an
        // idle result or release protection based on that command's failure.
        state.display = display("unknown", "");
        state.safe_idle = false;
        self.project(&state, true)
    }

    pub(crate) fn answer(&self, revision: u64, at: OffsetDateTime) -> Snapshot {
        self.command_result(revision, false, at)
    }
    pub(crate) fn end(&self, revision: u64, at: OffsetDateTime) -> Snapshot {
        self.command_result(revision, true, at)
    }
    fn command_result(&self, revision: u64, end: bool, at: OffsetDateTime) -> Snapshot {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired || state.revision != revision {
            return self.project(&state, false);
        }
        state.revision += 1;
        state.ring_at = None;
        if end {
            state.boundary += 1;
            state.pending = None;
            state.ring_number.clear();
            state.display = display("idle", "");
            state.desired_active = false;
            state.safe_idle = true;
            self.push(&state, at, NativeFactAction::End);
        } else {
            let number = number(&state.display);
            state.display = display("active", &number);
            state.desired_active = true;
            state.safe_idle = false;
            self.push(&state, at, NativeFactAction::Answered);
        }
        self.project(&state, true)
    }

    pub(crate) fn retire(&self, at: OffsetDateTime) -> Result<(), String> {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if state.retired {
            return Ok(());
        }
        state.retired = true;
        state.revision += 1;
        state.boundary += 1;
        state.pending = None;
        self.sink.retire(at)
    }
    fn fact(&self, state: &Inner, at: OffsetDateTime, action: NativeFactAction) -> NativeFact {
        NativeFact {
            owner_id: state.owner_id.clone(),
            owner_epoch: state.owner_epoch,
            at,
            action,
        }
    }
    fn push(&self, state: &Inner, at: OffsetDateTime, action: NativeFactAction) {
        let _ = self.sink.try_push(self.fact(state, at, action));
    }
    fn project(&self, state: &Inner, applied: bool) -> Snapshot {
        let history_error = self.sink.history_error();
        let mut display = state.display.clone();
        if let Some(error) = &history_error {
            display["history_error"] = json!(error);
        }
        Snapshot {
            display,
            revision: state.revision,
            boundary: state.boundary,
            desired_active: state.desired_active,
            pending_dial: state.pending.is_some(),
            safe_idle: state.safe_idle,
            applied,
            history_error,
        }
    }
}

fn display(state: &str, number: &str) -> Value {
    let mut display =
        json!({"state":state,"incoming":state == "incoming","active":state == "active"});
    if !number.is_empty() {
        display["number"] = json!(number);
    }
    display
}
fn number(value: &Value) -> String {
    value["number"].as_str().unwrap_or_default().into()
}
fn live(value: &Value) -> bool {
    matches!(
        value["state"].as_str(),
        Some("incoming" | "dialing" | "alerting" | "active")
    )
}
fn fresh_ring(state: &Inner, at: OffsetDateTime) -> bool {
    state.ring_at.is_some_and(|ring| {
        (at - ring).whole_nanoseconds() >= 0 && (at - ring).whole_nanoseconds() < 10_000_000_000
    })
}

#[cfg(test)]
mod tests;
