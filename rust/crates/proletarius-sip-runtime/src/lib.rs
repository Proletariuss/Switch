// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Bounded SIP transaction registry for a caller-owned event loop.
//!
//! The runtime creates no thread and performs no network I/O. It owns the
//! client/server state machines and their timer deadlines; the final Rust
//! service executes returned transmit/deliver actions on its transport task.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Instant;

use proletarius_sip::{
    ClientEvent, ClientState, ClientTransaction, Reliability, ServerEvent, ServerState,
    ServerTransaction, TimerConfig, TransactionAction, TransactionActions, TransactionKind,
    TransactionTimer, TransactionTransition,
};

const MAX_TRANSACTIONS: usize = 4096;
const MAX_TIMERS: usize = MAX_TRANSACTIONS * 2;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransactionId(u64);

impl TransactionId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    TransactionCapacity,
    TimerCapacity,
    UnknownTransaction,
    IdExhausted,
    DeadlineOverflow,
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TransactionCapacity => "SIP transaction runtime is full",
            Self::TimerCapacity => "SIP transaction timer table is full",
            Self::UnknownTransaction => "SIP transaction does not exist",
            Self::IdExhausted => "SIP transaction identifiers are exhausted",
            Self::DeadlineOverflow => "SIP transaction timer deadline overflowed",
            Self::Closed => "SIP transaction runtime is closed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TimerKey {
    transaction: TransactionId,
    timer: TransactionTimer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerExpiry {
    pub transaction: TransactionId,
    pub timer: TransactionTimer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScheduledTimer {
    deadline: Instant,
    key: TimerKey,
}

impl Ord for ScheduledTimer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.deadline, self.key).cmp(&(other.deadline, other.key))
    }
}

impl PartialOrd for ScheduledTimer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone)]
pub struct TimerScheduler {
    capacity: usize,
    deadlines: BTreeMap<TimerKey, Instant>,
    ordered: BTreeSet<ScheduledTimer>,
}

impl TimerScheduler {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.clamp(1, MAX_TIMERS),
            deadlines: BTreeMap::new(),
            ordered: BTreeSet::new(),
        }
    }

    pub fn schedule(
        &mut self,
        transaction: TransactionId,
        timer: TransactionTimer,
        deadline: Instant,
    ) -> Result<()> {
        let key = TimerKey { transaction, timer };
        if !self.deadlines.contains_key(&key) && self.deadlines.len() >= self.capacity {
            return Err(Error::TimerCapacity);
        }
        if let Some(previous) = self.deadlines.insert(key, deadline) {
            self.ordered.remove(&ScheduledTimer {
                deadline: previous,
                key,
            });
        }
        self.ordered.insert(ScheduledTimer { deadline, key });
        Ok(())
    }

    pub fn cancel(&mut self, transaction: TransactionId, timer: TransactionTimer) -> bool {
        let key = TimerKey { transaction, timer };
        let Some(deadline) = self.deadlines.remove(&key) else {
            return false;
        };
        self.ordered.remove(&ScheduledTimer { deadline, key });
        true
    }

    pub fn cancel_transaction(&mut self, transaction: TransactionId) -> usize {
        let before = self.deadlines.len();
        self.deadlines
            .retain(|key, _| key.transaction != transaction);
        self.ordered
            .retain(|entry| entry.key.transaction != transaction);
        before - self.deadlines.len()
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.ordered.first().map(|entry| entry.deadline)
    }

    pub fn pop_due(&mut self, now: Instant) -> Option<TimerExpiry> {
        let entry = self.ordered.first().copied()?;
        if entry.deadline > now {
            return None;
        }
        self.ordered.pop_first();
        self.deadlines.remove(&entry.key);
        Some(TimerExpiry {
            transaction: entry.key.transaction,
            timer: entry.key.timer,
        })
    }

    pub fn clear(&mut self) {
        self.deadlines.clear();
        self.ordered.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.deadlines.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deadlines.is_empty()
    }
}

#[derive(Clone)]
enum Machine {
    Client(ClientTransaction),
    Server(ServerTransaction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTransition {
    Client {
        transaction: TransactionId,
        transition: TransactionTransition<ClientState>,
    },
    Server {
        transaction: TransactionId,
        transition: TransactionTransition<ServerState>,
    },
}

#[derive(Clone)]
pub struct TransactionRuntime {
    capacity: usize,
    next_id: u64,
    machines: BTreeMap<TransactionId, Machine>,
    timers: TimerScheduler,
    closed: bool,
}

impl TransactionRuntime {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, MAX_TRANSACTIONS);
        Self {
            capacity,
            next_id: 0,
            machines: BTreeMap::new(),
            timers: TimerScheduler::new(capacity.saturating_mul(2)),
            closed: false,
        }
    }

    pub fn start_client(
        &mut self,
        kind: TransactionKind,
        reliability: Reliability,
        config: TimerConfig,
        now: Instant,
    ) -> Result<(TransactionId, TransactionActions)> {
        self.ensure_open()?;
        self.ensure_capacity()?;
        let id = self.allocate_id()?;
        let mut machine = ClientTransaction::new(kind, reliability, config);
        let actions = machine.start();
        if let Err(error) = self.apply_actions(id, &actions, now) {
            self.timers.cancel_transaction(id);
            return Err(error);
        }
        self.machines.insert(id, Machine::Client(machine));
        Ok((id, actions))
    }

    pub fn start_server(
        &mut self,
        kind: TransactionKind,
        reliability: Reliability,
        config: TimerConfig,
    ) -> Result<TransactionId> {
        self.ensure_open()?;
        self.ensure_capacity()?;
        let id = self.allocate_id()?;
        self.machines.insert(
            id,
            Machine::Server(ServerTransaction::new(kind, reliability, config)),
        );
        Ok(id)
    }

    pub fn client_event(
        &mut self,
        id: TransactionId,
        event: ClientEvent,
        now: Instant,
    ) -> Result<TransactionTransition<ClientState>> {
        self.ensure_open()?;
        let transition = match self.machines.get_mut(&id) {
            Some(Machine::Client(machine)) => machine.on_event(event),
            _ => return Err(Error::UnknownTransaction),
        };
        self.apply_actions(id, &transition.actions, now)?;
        if transition.state == ClientState::Terminated {
            self.remove(id);
        }
        Ok(transition)
    }

    pub fn server_event(
        &mut self,
        id: TransactionId,
        event: ServerEvent,
        now: Instant,
    ) -> Result<TransactionTransition<ServerState>> {
        self.ensure_open()?;
        let transition = match self.machines.get_mut(&id) {
            Some(Machine::Server(machine)) => machine.on_event(event),
            _ => return Err(Error::UnknownTransaction),
        };
        self.apply_actions(id, &transition.actions, now)?;
        if transition.state == ServerState::Terminated {
            self.remove(id);
        }
        Ok(transition)
    }

    pub fn poll_due(&mut self, now: Instant) -> Result<Option<RuntimeTransition>> {
        self.ensure_open()?;
        let Some(expiry) = self.timers.pop_due(now) else {
            return Ok(None);
        };
        let transition = match self.machines.get(&expiry.transaction) {
            Some(Machine::Client(_)) => RuntimeTransition::Client {
                transaction: expiry.transaction,
                transition: self.client_event(
                    expiry.transaction,
                    ClientEvent::Timer(expiry.timer),
                    now,
                )?,
            },
            Some(Machine::Server(_)) => RuntimeTransition::Server {
                transaction: expiry.transaction,
                transition: self.server_event(
                    expiry.transaction,
                    ServerEvent::Timer(expiry.timer),
                    now,
                )?,
            },
            None => return Ok(None),
        };
        Ok(Some(transition))
    }

    pub fn remove(&mut self, id: TransactionId) -> bool {
        self.timers.cancel_transaction(id);
        self.machines.remove(&id).is_some()
    }

    /// Close the caller-owned runtime and release every transaction and timer.
    ///
    /// Shutdown is idempotent. Once closed, no new transition can be applied;
    /// callers must construct a fresh runtime after a transport or account
    /// lifecycle ends. The return value is the number of released machines.
    pub fn close(&mut self) -> usize {
        let released = self.machines.len();
        self.machines.clear();
        self.timers.clear();
        self.closed = true;
        released
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.timers.next_deadline()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.machines.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.machines.is_empty()
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    fn ensure_capacity(&self) -> Result<()> {
        if self.machines.len() >= self.capacity {
            return Err(Error::TransactionCapacity);
        }
        Ok(())
    }

    fn allocate_id(&mut self) -> Result<TransactionId> {
        for _ in 0..=self.capacity {
            self.next_id = self.next_id.checked_add(1).ok_or(Error::IdExhausted)?;
            let id = TransactionId(self.next_id);
            if !self.machines.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(Error::IdExhausted)
    }

    fn apply_actions(
        &mut self,
        id: TransactionId,
        actions: &TransactionActions,
        now: Instant,
    ) -> Result<()> {
        if actions.contains(TransactionAction::Terminate) {
            self.timers.cancel_transaction(id);
            return Ok(());
        }
        for action in actions.iter() {
            if let TransactionAction::Cancel(timer) = action {
                self.timers.cancel(id, timer);
            }
        }
        for action in actions.iter() {
            if let TransactionAction::Schedule(timer, delay) = action {
                let deadline = now.checked_add(delay).ok_or(Error::DeadlineOverflow)?;
                self.timers.schedule(id, timer, deadline)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, RuntimeTransition, TimerScheduler, TransactionId, TransactionRuntime};
    use proletarius_sip::{
        ClientEvent, ClientState, Reliability, TimerConfig, TransactionAction, TransactionKind,
        TransactionTimer,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn timer_reschedule_replaces_the_existing_deadline() {
        let start = Instant::now();
        let transaction = TransactionId(7);
        let mut scheduler = TimerScheduler::new(2);
        scheduler
            .schedule(
                transaction,
                TransactionTimer::A,
                start + Duration::from_millis(20),
            )
            .expect("first deadline");
        scheduler
            .schedule(
                transaction,
                TransactionTimer::A,
                start + Duration::from_millis(10),
            )
            .expect("replacement deadline");

        assert_eq!(scheduler.len(), 1);
        assert_eq!(
            scheduler.next_deadline(),
            Some(start + Duration::from_millis(10))
        );
        assert!(
            scheduler
                .pop_due(start + Duration::from_millis(9))
                .is_none()
        );
        assert_eq!(
            scheduler.pop_due(start + Duration::from_millis(10)),
            Some(super::TimerExpiry {
                transaction,
                timer: TransactionTimer::A,
            })
        );
        assert!(scheduler.is_empty());
    }

    #[test]
    fn client_timer_emits_retransmit_and_provisional_cancels_retransmit_timer() {
        let start = Instant::now();
        let timers = TimerConfig {
            t1: Duration::from_millis(10),
            t2: Duration::from_millis(40),
            t4: Duration::from_millis(50),
        };
        let mut runtime = TransactionRuntime::new(4);
        let (id, initial) = runtime
            .start_client(
                TransactionKind::Invite,
                Reliability::Unreliable,
                timers,
                start,
            )
            .expect("client transaction");
        assert!(initial.contains(TransactionAction::TransmitRequest));
        assert_eq!(
            runtime.next_deadline(),
            Some(start + Duration::from_millis(10))
        );

        let due = runtime
            .poll_due(start + Duration::from_millis(10))
            .expect("poll")
            .expect("retransmit timer");
        let RuntimeTransition::Client {
            transaction,
            transition,
        } = due
        else {
            panic!("expected client transition");
        };
        assert_eq!(transaction, id);
        assert_eq!(transition.state, ClientState::Calling);
        assert!(
            transition
                .actions
                .contains(TransactionAction::TransmitRequest)
        );

        let provisional = runtime
            .client_event(
                id,
                ClientEvent::Provisional,
                start + Duration::from_millis(11),
            )
            .expect("provisional response");
        assert_eq!(provisional.state, ClientState::Proceeding);
        assert!(
            provisional
                .actions
                .contains(TransactionAction::Cancel(TransactionTimer::A))
        );
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.timers.len(), 1);
    }

    #[test]
    fn capacity_rejects_without_leaking_partial_transaction_state() {
        let start = Instant::now();
        let mut runtime = TransactionRuntime::new(1);
        runtime
            .start_client(
                TransactionKind::NonInvite,
                Reliability::Unreliable,
                TimerConfig::default(),
                start,
            )
            .expect("first transaction");

        assert_eq!(
            runtime.start_client(
                TransactionKind::Invite,
                Reliability::Reliable,
                TimerConfig::default(),
                start,
            ),
            Err(Error::TransactionCapacity)
        );
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.timers.len(), 2);
    }

    #[test]
    fn close_releases_transactions_and_prevents_future_work() {
        let start = Instant::now();
        let mut runtime = TransactionRuntime::new(2);
        runtime
            .start_client(
                TransactionKind::Invite,
                Reliability::Unreliable,
                TimerConfig::default(),
                start,
            )
            .expect("client transaction");

        assert_eq!(runtime.close(), 1);
        assert!(runtime.is_closed());
        assert_eq!(runtime.len(), 0);
        assert!(runtime.timers.is_empty());
        assert_eq!(runtime.poll_due(start), Err(Error::Closed));
    }
}
