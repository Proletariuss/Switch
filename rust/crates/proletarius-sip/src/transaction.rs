// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Allocation-free RFC 3261/6026 transaction state transitions.
//!
//! The state machines own no socket and create no timers. They return a small,
//! fixed-capacity action list for the runtime to execute, keeping transport,
//! scheduling and transaction ownership separate.

use std::time::Duration;

const MAX_ACTIONS: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reliability {
    Reliable,
    Unreliable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionKind {
    Invite,
    NonInvite,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TransactionTimer {
    A,
    B,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerConfig {
    pub t1: Duration,
    pub t2: Duration,
    pub t4: Duration,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            t1: Duration::from_millis(500),
            t2: Duration::from_secs(4),
            t4: Duration::from_secs(5),
        }
    }
}

impl TimerConfig {
    #[must_use]
    pub fn invite_retransmit_delay(self, attempt: u8) -> Duration {
        exponential_delay(self.t1, attempt, self.duration_64_t1())
    }

    #[must_use]
    pub fn non_invite_retransmit_delay(self, attempt: u8, proceeding: bool) -> Duration {
        if proceeding {
            self.t2
        } else {
            exponential_delay(self.t1, attempt, self.t2)
        }
    }

    #[must_use]
    pub fn server_retransmit_delay(self, attempt: u8) -> Duration {
        exponential_delay(self.t1, attempt, self.t2)
    }

    #[must_use]
    pub fn duration_64_t1(self) -> Duration {
        self.t1.saturating_mul(64)
    }

    #[must_use]
    pub fn completion_delay(self, timer: TransactionTimer, reliability: Reliability) -> Duration {
        match timer {
            TransactionTimer::B
            | TransactionTimer::F
            | TransactionTimer::H
            | TransactionTimer::L
            | TransactionTimer::M => self.duration_64_t1(),
            TransactionTimer::D => {
                if reliability == Reliability::Reliable {
                    Duration::ZERO
                } else {
                    self.duration_64_t1().max(Duration::from_secs(32))
                }
            }
            TransactionTimer::I | TransactionTimer::K => {
                if reliability == Reliability::Reliable {
                    Duration::ZERO
                } else {
                    self.t4
                }
            }
            TransactionTimer::J => {
                if reliability == Reliability::Reliable {
                    Duration::ZERO
                } else {
                    self.duration_64_t1()
                }
            }
            TransactionTimer::A | TransactionTimer::E | TransactionTimer::G => self.t1,
        }
    }
}

fn exponential_delay(initial: Duration, attempt: u8, maximum: Duration) -> Duration {
    let multiplier = 1_u32
        .checked_shl(u32::from(attempt.min(31)))
        .unwrap_or(u32::MAX);
    initial.saturating_mul(multiplier).min(maximum)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionAction {
    TransmitRequest,
    TransmitResponse,
    RetransmitResponse,
    TransmitAck,
    DeliverResponse,
    DeliverAck,
    DeliverCancel,
    Schedule(TransactionTimer, Duration),
    Cancel(TransactionTimer),
    Terminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionActions {
    items: [Option<TransactionAction>; MAX_ACTIONS],
    len: u8,
}

impl Default for TransactionActions {
    fn default() -> Self {
        Self {
            items: [None; MAX_ACTIONS],
            len: 0,
        }
    }
}

impl TransactionActions {
    fn push(&mut self, action: TransactionAction) {
        let index = usize::from(self.len);
        let inserted = self.items.get_mut(index).is_some_and(|slot| {
            *slot = Some(action);
            true
        });
        debug_assert!(inserted, "SIP transaction action capacity exceeded");
        if inserted {
            self.len = self.len.saturating_add(1);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = TransactionAction> + '_ {
        self.items[..self.len()].iter().flatten().copied()
    }

    #[must_use]
    pub fn contains(&self, expected: TransactionAction) -> bool {
        self.iter().any(|action| action == expected)
    }

    #[must_use]
    pub fn has_timer(&self, expected: TransactionTimer) -> bool {
        self.iter().any(
            |action| matches!(action, TransactionAction::Schedule(timer, _) if timer == expected),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionTransition<State> {
    pub state: State,
    pub actions: TransactionActions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientState {
    Calling,
    Trying,
    Proceeding,
    Accepted,
    Completed,
    Terminated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientEvent {
    Provisional,
    Success,
    Failure,
    Timer(TransactionTimer),
    TransportError,
    Terminate,
}

#[derive(Clone)]
pub struct ClientTransaction {
    kind: TransactionKind,
    state: ClientState,
    reliability: Reliability,
    timers: TimerConfig,
    retransmit_attempt: u8,
    started: bool,
}

impl ClientTransaction {
    #[must_use]
    pub fn new(kind: TransactionKind, reliability: Reliability, timers: TimerConfig) -> Self {
        let state = match kind {
            TransactionKind::Invite => ClientState::Calling,
            TransactionKind::NonInvite => ClientState::Trying,
        };
        Self {
            kind,
            state,
            reliability,
            timers,
            retransmit_attempt: 0,
            started: false,
        }
    }

    #[must_use]
    pub const fn state(&self) -> ClientState {
        self.state
    }

    pub fn start(&mut self) -> TransactionActions {
        let mut actions = TransactionActions::default();
        if self.started || self.state == ClientState::Terminated {
            return actions;
        }
        self.started = true;
        actions.push(TransactionAction::TransmitRequest);
        match self.kind {
            TransactionKind::Invite => {
                if self.reliability == Reliability::Unreliable {
                    actions.push(TransactionAction::Schedule(
                        TransactionTimer::A,
                        self.timers.t1,
                    ));
                }
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::B,
                    self.timers
                        .completion_delay(TransactionTimer::B, self.reliability),
                ));
            }
            TransactionKind::NonInvite => {
                if self.reliability == Reliability::Unreliable {
                    actions.push(TransactionAction::Schedule(
                        TransactionTimer::E,
                        self.timers.t1,
                    ));
                }
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::F,
                    self.timers
                        .completion_delay(TransactionTimer::F, self.reliability),
                ));
            }
        }
        actions
    }

    pub fn on_event(&mut self, event: ClientEvent) -> TransactionTransition<ClientState> {
        let mut actions = TransactionActions::default();
        if self.state == ClientState::Terminated {
            return TransactionTransition {
                state: self.state,
                actions,
            };
        }
        if matches!(event, ClientEvent::TransportError | ClientEvent::Terminate) {
            self.terminate(&mut actions);
            return TransactionTransition {
                state: self.state,
                actions,
            };
        }
        match self.kind {
            TransactionKind::Invite => self.invite_event(event, &mut actions),
            TransactionKind::NonInvite => self.non_invite_event(event, &mut actions),
        }
        TransactionTransition {
            state: self.state,
            actions,
        }
    }

    fn invite_event(&mut self, event: ClientEvent, actions: &mut TransactionActions) {
        match (self.state, event) {
            (ClientState::Calling, ClientEvent::Timer(TransactionTimer::A)) => {
                actions.push(TransactionAction::TransmitRequest);
                self.retransmit_attempt = self.retransmit_attempt.saturating_add(1);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::A,
                    self.timers.invite_retransmit_delay(self.retransmit_attempt),
                ));
            }
            (ClientState::Calling, ClientEvent::Provisional) => {
                self.state = ClientState::Proceeding;
                actions.push(TransactionAction::Cancel(TransactionTimer::A));
                actions.push(TransactionAction::DeliverResponse);
            }
            (ClientState::Calling | ClientState::Proceeding, ClientEvent::Success) => {
                self.state = ClientState::Accepted;
                actions.push(TransactionAction::Cancel(TransactionTimer::A));
                actions.push(TransactionAction::Cancel(TransactionTimer::B));
                actions.push(TransactionAction::DeliverResponse);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::M,
                    self.timers
                        .completion_delay(TransactionTimer::M, self.reliability),
                ));
            }
            (ClientState::Calling | ClientState::Proceeding, ClientEvent::Failure) => {
                self.state = ClientState::Completed;
                actions.push(TransactionAction::Cancel(TransactionTimer::A));
                actions.push(TransactionAction::Cancel(TransactionTimer::B));
                actions.push(TransactionAction::TransmitAck);
                actions.push(TransactionAction::DeliverResponse);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::D,
                    self.timers
                        .completion_delay(TransactionTimer::D, self.reliability),
                ));
            }
            (ClientState::Proceeding, ClientEvent::Provisional)
            | (ClientState::Accepted, ClientEvent::Success) => {
                actions.push(TransactionAction::DeliverResponse);
            }
            (ClientState::Completed, ClientEvent::Failure) => {
                actions.push(TransactionAction::TransmitAck);
            }
            (
                ClientState::Calling | ClientState::Proceeding,
                ClientEvent::Timer(TransactionTimer::B),
            )
            | (ClientState::Accepted, ClientEvent::Timer(TransactionTimer::M))
            | (ClientState::Completed, ClientEvent::Timer(TransactionTimer::D)) => {
                self.terminate(actions);
            }
            _ => {}
        }
    }

    fn non_invite_event(&mut self, event: ClientEvent, actions: &mut TransactionActions) {
        match (self.state, event) {
            (
                ClientState::Trying | ClientState::Proceeding,
                ClientEvent::Timer(TransactionTimer::E),
            ) => {
                actions.push(TransactionAction::TransmitRequest);
                self.retransmit_attempt = self.retransmit_attempt.saturating_add(1);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::E,
                    self.timers.non_invite_retransmit_delay(
                        self.retransmit_attempt,
                        self.state == ClientState::Proceeding,
                    ),
                ));
            }
            (ClientState::Trying, ClientEvent::Provisional) => {
                self.state = ClientState::Proceeding;
                actions.push(TransactionAction::DeliverResponse);
            }
            (ClientState::Proceeding, ClientEvent::Provisional) => {
                actions.push(TransactionAction::DeliverResponse);
            }
            (
                ClientState::Trying | ClientState::Proceeding,
                ClientEvent::Success | ClientEvent::Failure,
            ) => {
                self.state = ClientState::Completed;
                actions.push(TransactionAction::Cancel(TransactionTimer::E));
                actions.push(TransactionAction::Cancel(TransactionTimer::F));
                actions.push(TransactionAction::DeliverResponse);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::K,
                    self.timers
                        .completion_delay(TransactionTimer::K, self.reliability),
                ));
            }
            (
                ClientState::Trying | ClientState::Proceeding,
                ClientEvent::Timer(TransactionTimer::F),
            )
            | (ClientState::Completed, ClientEvent::Timer(TransactionTimer::K)) => {
                self.terminate(actions);
            }
            _ => {}
        }
    }

    fn terminate(&mut self, actions: &mut TransactionActions) {
        self.state = ClientState::Terminated;
        for timer in [
            TransactionTimer::A,
            TransactionTimer::B,
            TransactionTimer::D,
            TransactionTimer::E,
            TransactionTimer::F,
            TransactionTimer::K,
            TransactionTimer::M,
        ] {
            actions.push(TransactionAction::Cancel(timer));
        }
        actions.push(TransactionAction::Terminate);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerState {
    Trying,
    Proceeding,
    Accepted,
    Completed,
    Confirmed,
    Terminated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerEvent {
    Request,
    Ack,
    Cancel,
    UserProvisional,
    UserSuccess,
    UserFailure,
    Timer(TransactionTimer),
    TransportError,
    Terminate,
}

#[derive(Clone)]
pub struct ServerTransaction {
    kind: TransactionKind,
    state: ServerState,
    reliability: Reliability,
    timers: TimerConfig,
    retransmit_attempt: u8,
}

impl ServerTransaction {
    #[must_use]
    pub fn new(kind: TransactionKind, reliability: Reliability, timers: TimerConfig) -> Self {
        let state = match kind {
            TransactionKind::Invite => ServerState::Proceeding,
            TransactionKind::NonInvite => ServerState::Trying,
        };
        Self {
            kind,
            state,
            reliability,
            timers,
            retransmit_attempt: 0,
        }
    }

    #[must_use]
    pub const fn state(&self) -> ServerState {
        self.state
    }

    pub fn on_event(&mut self, event: ServerEvent) -> TransactionTransition<ServerState> {
        let mut actions = TransactionActions::default();
        if self.state == ServerState::Terminated {
            return TransactionTransition {
                state: self.state,
                actions,
            };
        }
        if matches!(event, ServerEvent::TransportError | ServerEvent::Terminate) {
            self.terminate(&mut actions);
            return TransactionTransition {
                state: self.state,
                actions,
            };
        }
        match self.kind {
            TransactionKind::Invite => self.invite_event(event, &mut actions),
            TransactionKind::NonInvite => self.non_invite_event(event, &mut actions),
        }
        TransactionTransition {
            state: self.state,
            actions,
        }
    }

    fn invite_event(&mut self, event: ServerEvent, actions: &mut TransactionActions) {
        match (self.state, event) {
            (ServerState::Proceeding, ServerEvent::Request) => {
                actions.push(TransactionAction::RetransmitResponse);
            }
            (ServerState::Proceeding, ServerEvent::Cancel) => {
                actions.push(TransactionAction::DeliverCancel);
            }
            (ServerState::Proceeding, ServerEvent::UserProvisional) => {
                actions.push(TransactionAction::TransmitResponse);
            }
            (ServerState::Proceeding, ServerEvent::UserSuccess) => {
                self.state = ServerState::Accepted;
                actions.push(TransactionAction::TransmitResponse);
                if self.reliability == Reliability::Unreliable {
                    actions.push(TransactionAction::Schedule(
                        TransactionTimer::G,
                        self.timers.t1,
                    ));
                }
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::L,
                    self.timers
                        .completion_delay(TransactionTimer::L, self.reliability),
                ));
            }
            (ServerState::Proceeding, ServerEvent::UserFailure) => {
                self.state = ServerState::Completed;
                actions.push(TransactionAction::TransmitResponse);
                if self.reliability == Reliability::Unreliable {
                    actions.push(TransactionAction::Schedule(
                        TransactionTimer::G,
                        self.timers.t1,
                    ));
                }
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::H,
                    self.timers
                        .completion_delay(TransactionTimer::H, self.reliability),
                ));
            }
            (ServerState::Accepted, ServerEvent::Ack) => {
                actions.push(TransactionAction::Cancel(TransactionTimer::G));
                actions.push(TransactionAction::DeliverAck);
            }
            (ServerState::Accepted, ServerEvent::UserSuccess) => {
                actions.push(TransactionAction::TransmitResponse);
            }
            (ServerState::Accepted, ServerEvent::Timer(TransactionTimer::G)) => {
                actions.push(TransactionAction::RetransmitResponse);
                self.retransmit_attempt = self.retransmit_attempt.saturating_add(1);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::G,
                    self.timers.server_retransmit_delay(self.retransmit_attempt),
                ));
            }
            (ServerState::Accepted, ServerEvent::Timer(TransactionTimer::L)) => {
                self.terminate(actions);
            }
            (ServerState::Completed, ServerEvent::Request) => {
                actions.push(TransactionAction::RetransmitResponse);
            }
            (ServerState::Completed, ServerEvent::Ack) => {
                self.state = ServerState::Confirmed;
                actions.push(TransactionAction::Cancel(TransactionTimer::G));
                actions.push(TransactionAction::Cancel(TransactionTimer::H));
                actions.push(TransactionAction::DeliverAck);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::I,
                    self.timers
                        .completion_delay(TransactionTimer::I, self.reliability),
                ));
            }
            (ServerState::Completed, ServerEvent::Timer(TransactionTimer::G)) => {
                actions.push(TransactionAction::RetransmitResponse);
                self.retransmit_attempt = self.retransmit_attempt.saturating_add(1);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::G,
                    self.timers.server_retransmit_delay(self.retransmit_attempt),
                ));
            }
            (ServerState::Completed, ServerEvent::Timer(TransactionTimer::H))
            | (ServerState::Confirmed, ServerEvent::Timer(TransactionTimer::I)) => {
                self.terminate(actions);
            }
            _ => {}
        }
    }

    fn non_invite_event(&mut self, event: ServerEvent, actions: &mut TransactionActions) {
        match (self.state, event) {
            (ServerState::Trying, ServerEvent::Request) => {}
            (ServerState::Trying, ServerEvent::UserProvisional) => {
                self.state = ServerState::Proceeding;
                actions.push(TransactionAction::TransmitResponse);
            }
            (ServerState::Proceeding, ServerEvent::Request) => {
                actions.push(TransactionAction::RetransmitResponse);
            }
            (ServerState::Proceeding, ServerEvent::UserProvisional) => {
                actions.push(TransactionAction::TransmitResponse);
            }
            (
                ServerState::Trying | ServerState::Proceeding,
                ServerEvent::UserSuccess | ServerEvent::UserFailure,
            ) => {
                self.state = ServerState::Completed;
                actions.push(TransactionAction::TransmitResponse);
                actions.push(TransactionAction::Schedule(
                    TransactionTimer::J,
                    self.timers
                        .completion_delay(TransactionTimer::J, self.reliability),
                ));
            }
            (ServerState::Completed, ServerEvent::Request) => {
                actions.push(TransactionAction::RetransmitResponse);
            }
            (ServerState::Completed, ServerEvent::Timer(TransactionTimer::J)) => {
                self.terminate(actions);
            }
            _ => {}
        }
    }

    fn terminate(&mut self, actions: &mut TransactionActions) {
        self.state = ServerState::Terminated;
        for timer in [
            TransactionTimer::G,
            TransactionTimer::H,
            TransactionTimer::I,
            TransactionTimer::J,
            TransactionTimer::L,
        ] {
            actions.push(TransactionAction::Cancel(timer));
        }
        actions.push(TransactionAction::Terminate);
    }
}
