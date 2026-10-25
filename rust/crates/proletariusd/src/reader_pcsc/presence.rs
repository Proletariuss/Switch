//! Confirmed PC/SC disappearance for Rust-owned physical sessions. BLE remains
//! with its platform bridge; lifecycle handoff uses the native service.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval, timeout};

use super::{Failure, Owner, closing_session_pending, finish_session_close};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const SCAN_TIMEOUT: Duration = Duration::from_secs(1);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(65);
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(65);
const REQUIRED_MISSES: u8 = 2;

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) struct Session {
    pub(super) producer: String,
    pub(super) reader: String,
    pub(super) id: String,
    pub(super) sequence: u64,
}

impl Owner {
    #[cfg(test)]
    pub(crate) fn test_close_for_presence(
        &self,
        binding: &super::LifecycleBinding,
    ) -> Result<bool, Failure> {
        self.enable_presence_handoff();
        self.close_if_confirmed_missing(&Session {
            producer: binding.producer.clone(),
            reader: binding.reader.clone(),
            id: binding.session_id.clone(),
            sequence: binding.session_sequence,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_ack_presence_loss(&self, binding: &super::LifecycleBinding) {
        self.acknowledge_presence_handoff(&Session {
            producer: binding.producer.clone(),
            reader: binding.reader.clone(),
            id: binding.session_id.clone(),
            sequence: binding.session_sequence,
        });
    }

    /// A scan is useful only for an idle physical session. Never use a
    /// possibly incomplete inventory to interrupt an APDU transaction.
    pub(super) fn idle_pcsc_sessions(&self) -> Vec<Session> {
        let workers = {
            let owner = self.inner.state.lock().unwrap();
            if !owner.active || owner.stopping || owner.admission_closed {
                return Vec::new();
            }
            owner.readers.values().cloned().collect::<Vec<_>>()
        };
        workers
            .iter()
            .filter_map(|worker| {
                let state = worker.state.lock().unwrap();
                if state.session.is_empty()
                    || state.stopping
                    || state.closing
                    || state.close_error.is_some()
                    || state.lease.is_some()
                    || state.native_borrow.is_some()
                {
                    return None;
                }
                Some(Session {
                    producer: state.producer.clone(),
                    reader: worker.reader.clone(),
                    id: state.session.clone(),
                    sequence: state.session_seq,
                })
            })
            .collect()
    }

    /// Atomically recheck the exact session and its idle state before fencing
    /// it. A replacement session, new lease, or new producer makes the old
    /// inventory observation inapplicable. The worker still owns the handle
    /// until its physical close really returns.
    fn close_if_confirmed_missing(&self, expected: &Session) -> Result<bool, Failure> {
        let worker = {
            let mut owner = self.inner.state.lock().unwrap();
            if !owner.active
                || owner.stopping
                || owner.admission_closed
                || owner.producer != expected.producer
            {
                return Ok(false);
            }
            let Some(worker) = owner.readers.get(&expected.reader).cloned() else {
                return Ok(false);
            };
            let mut state = worker.state.lock().unwrap();
            if state.producer != expected.producer
                || state.session != expected.id
                || state.session_seq != expected.sequence
                || state.stopping
                || state.closing
                || state.close_error.is_some()
                || state.lease.is_some()
                || state.native_borrow.is_some()
            {
                return Ok(false);
            }
            if !owner.closed_sessions.contains_key(&expected.reader)
                && owner.closed_sessions.len() >= super::MAX_READERS
            {
                return Err(Failure::busy());
            }
            if owner
                .pending_presence_handoffs
                .contains_key(&expected.reader)
            {
                return Ok(false);
            }
            let high = owner
                .closed_sessions
                .entry(expected.reader.clone())
                .or_default();
            *high = (*high).max(expected.sequence);
            state.session_high = state.session_high.max(expected.sequence);
            if owner.presence_handoff_enabled {
                owner.pending_presence_handoffs.insert(
                    expected.reader.clone(),
                    super::PendingPresenceHandoff {
                        session: expected.clone(),
                        close_confirmed: false,
                    },
                );
            }
            state.closing = true;
            drop(state);
            worker
        };
        let _ = worker.sender.try_send(super::Work::Wake);
        let mut state = worker.state.lock().unwrap();
        while closing_session_pending(&state, &expected.id, expected.sequence) {
            state = worker.changed.wait(state).unwrap();
        }
        let completed = finish_session_close(&mut state, &expected.id, expected.sequence);
        drop(state);
        completed?;
        let mut owner = self.inner.state.lock().unwrap();
        if let Some(pending) = owner.pending_presence_handoffs.get_mut(&expected.reader) {
            if pending.session == *expected {
                pending.close_confirmed = true;
            }
        }
        Ok(true)
    }
}

pub(crate) type NativeHandoff = Arc<
    dyn Fn(
            super::LifecycleBinding,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;
pub(crate) struct PresenceMonitor {
    owner: Owner,
    misses: HashMap<Session, u8>,
    handoff: Option<NativeHandoff>,
    // Production never waits for a slow physical close inside its scan loop;
    // the worker retains the owner and publishes the eventual close result.
    nonblocking_closes: bool,
}

impl PresenceMonitor {
    pub(crate) fn new(owner: Owner) -> Self {
        Self {
            owner,
            misses: HashMap::new(),
            handoff: None,
            nonblocking_closes: false,
        }
    }

    pub(crate) fn with_native_handoff(owner: Owner, handoff: NativeHandoff) -> Self {
        owner.enable_presence_handoff();
        Self {
            owner,
            misses: HashMap::new(),
            handoff: Some(handoff),
            nonblocking_closes: true,
        }
    }
    pub(crate) async fn run(mut self) {
        let mut ticker = interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut callbacks = JoinSet::new();
        // The task ID lets a failed callback task release its retry slot even
        // when it cannot return the session identity in its normal result.
        let mut in_flight = HashMap::new();
        loop {
            ticker.tick().await;
            while let Some(result) = callbacks.try_join_next_with_id() {
                match result {
                    Ok((id, (session, true))) => {
                        if in_flight.get(&session) == Some(&id) {
                            in_flight.remove(&session);
                        }
                        self.owner.acknowledge_presence_handoff(&session);
                    }
                    Ok((id, (session, false))) => {
                        if in_flight.get(&session) == Some(&id) {
                            in_flight.remove(&session);
                        }
                        crate::diagnostics::write_line(format_args!(
                            "switchd-rs: PC/SC business teardown unconfirmed"
                        ));
                    }
                    Err(error) => {
                        in_flight.retain(|_, id| *id != error.id());
                        crate::diagnostics::write_line(format_args!(
                            "switchd-rs: PC/SC business callback task failed"
                        ));
                    }
                }
            }
            self.poll_with(|| async {
                match timeout(SCAN_TIMEOUT, crate::reader_inventory::scan()).await {
                    Ok(Ok(Ok(readers))) => {
                        Ok(readers.into_iter().map(|reader| reader.name).collect())
                    }
                    _ => Err(()),
                }
            })
            .await;
            if let Some(handoff) = &self.handoff {
                for session in self.owner.confirmed_presence_handoffs() {
                    // One in-flight callback per exact physical session. A
                    // slow Go teardown cannot pause scans of other readers
                    // or cause concurrent duplicate retries of itself.
                    if in_flight.len() >= super::MAX_READERS || in_flight.contains_key(&session) {
                        continue;
                    }
                    let handoff = handoff.clone();
                    let task = callbacks.spawn(send_handoff(handoff, session.clone()));
                    in_flight.insert(session, task.id());
                }
            }
        }
    }

    pub(super) async fn flush_confirmed_handoffs(&self) {
        let Some(handoff) = &self.handoff else {
            return;
        };
        let mut requests = JoinSet::new();
        for session in self.owner.confirmed_presence_handoffs() {
            let handoff = handoff.clone();
            requests.spawn(send_handoff(handoff, session));
        }
        while let Some(result) = requests.join_next().await {
            match result {
                Ok((session, true)) => self.owner.acknowledge_presence_handoff(&session),
                Ok((_, false)) | Err(_) => {
                    // The old business generation may still own a live IMS
                    // session. Keep physical admission fenced and retry.
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: PC/SC business teardown unconfirmed"
                    ));
                }
            }
        }
    }

    /// Injectable scanner keeps the absence policy and owner close race
    /// testable without a reader. A failed or timed-out scan is not a miss.
    pub(super) async fn poll_with<S, F>(&mut self, scan: S)
    where
        S: FnOnce() -> F,
        F: Future<Output = Result<Vec<String>, ()>>,
    {
        let before = self.owner.idle_pcsc_sessions();
        if before.is_empty() {
            self.misses.clear();
            return;
        }
        let Ok(readers) = scan().await else {
            self.misses.clear();
            return;
        };
        let current = self
            .owner
            .idle_pcsc_sessions()
            .into_iter()
            .collect::<HashSet<_>>();
        let readers = readers.into_iter().collect::<HashSet<_>>();
        self.misses.retain(|session, _| current.contains(session));
        for session in before {
            if !current.contains(&session) {
                continue;
            }
            if readers.contains(&session.reader) {
                self.misses.remove(&session);
                continue;
            }
            let misses = self.misses.entry(session.clone()).or_default();
            *misses = misses.saturating_add(1);
            if *misses < REQUIRED_MISSES {
                continue;
            }
            self.misses.remove(&session);
            let owner = self.owner.clone();
            let pool = self.owner.inner.control_pool.clone();
            // Retain the result receiver across an observation timeout. A
            // physical close still in progress must eventually publish its
            // confirmed handoff instead of disappearing with a timed-out poll.
            let mut close = tokio::spawn(async move {
                let result = pool
                    .run(move || owner.close_if_confirmed_missing(&session))
                    .await;
                if let Ok(Err(error)) = &result {
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: PC/SC disappearance close unconfirmed: {}",
                        error.code
                    ));
                }
                result
            });
            if self.nonblocking_closes {
                // Dropping this JoinHandle detaches, rather than aborts, the
                // close task. The fenced owner stays alive and marks its
                // pending handoff only when physical close truly succeeds.
                continue;
            }
            let result = timeout(CLOSE_TIMEOUT, &mut close).await;
            match result {
                Ok(Ok(Ok(Err(_)))) => {}
                Err(_) => crate::diagnostics::write_line(format_args!(
                    "switchd-rs: PC/SC disappearance close still in progress"
                )),
                _ => {}
            }
        }
    }
}

async fn send_handoff(handoff: NativeHandoff, session: Session) -> (Session, bool) {
    let expected = super::LifecycleBinding {
        producer: session.producer.clone(),
        reader: session.reader.clone(),
        session_id: session.id.clone(),
        session_sequence: session.sequence,
    };
    // A task timeout never acknowledges a handoff. The captured native
    // lifecycle future must retain any cleanup it dispatched independently.
    let confirmed = matches!(
        timeout(HANDOFF_TIMEOUT, handoff(expected)).await,
        Ok(Ok(()))
    );
    (session, confirmed)
}
