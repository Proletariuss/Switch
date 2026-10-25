//! Bounded evidence, not authority. All storage is reserved before any open.
use nix::errno::Errno;
use std::{cell::Cell, rc::Rc};

pub(super) const RESOURCES: usize = 2;
pub(super) const CLEANUP: usize = 3;
pub(super) const EVENTS: usize = 32;
pub(super) const FAULTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Parent,
    Lock,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Reserve,
    ContextBefore,
    ParentOpen,
    ParentStat,
    ParentNamed,
    LockOpen,
    LockStat,
    LockNamed,
    TryLock,
    PostLock,
    PostParentStat,
    PostParentNamed,
    PostLockStat,
    PostLockNamed,
    ContextAfter,
    Committed,
    Unlock,
    LockClose,
    ParentClose,
    Fallback,
    Ledger,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Io,
    Type,
    Uid,
    Mode,
    Identity,
    Links,
    Size,
    Context,
    Busy,
    Uncertain,
    Capacity,
    Invariant,
    Fallback,
    Panic,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fault {
    pub phase: Phase,
    pub role: Role,
    pub kind: Kind,
    pub errno: Option<Errno>,
}
impl Fault {
    pub(super) fn new(phase: Phase, role: Role, kind: Kind, errno: Option<Errno>) -> Self {
        Self {
            phase,
            role,
            kind,
            errno,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LockState {
    NotAttempted,
    NotHeld,
    Held,
    Uncertain,
    Released,
    ReleasedUncertain,
    ReleaseFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Disposition {
    ReservedUnused,
    Owned,
    Closed,
    CloseFailed,
    UncheckedClosed,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Resource {
    pub id: usize,
    pub role: Role,
    pub observed_fd: Option<i32>,
    pub disposition: Disposition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Unlock,
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Success,
    Error(Errno),
    Unchecked(Option<Errno>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cleanup {
    pub resource: usize,
    pub role: Role,
    pub phase: Phase,
    pub action: Action,
    pub outcome: Outcome,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Event {
    pub phase: Phase,
    pub role: Role,
}
pub(super) const SUCCESS_EVENTS: [Event; 15] = [
    Event {
        phase: Phase::ContextBefore,
        role: Role::Parent,
    },
    Event {
        phase: Phase::ParentOpen,
        role: Role::Parent,
    },
    Event {
        phase: Phase::ParentStat,
        role: Role::Parent,
    },
    Event {
        phase: Phase::ParentNamed,
        role: Role::Parent,
    },
    Event {
        phase: Phase::LockOpen,
        role: Role::Lock,
    },
    Event {
        phase: Phase::LockStat,
        role: Role::Lock,
    },
    Event {
        phase: Phase::LockNamed,
        role: Role::Lock,
    },
    Event {
        phase: Phase::TryLock,
        role: Role::Lock,
    },
    Event {
        phase: Phase::PostLock,
        role: Role::Lock,
    },
    Event {
        phase: Phase::PostParentStat,
        role: Role::Parent,
    },
    Event {
        phase: Phase::PostParentNamed,
        role: Role::Parent,
    },
    Event {
        phase: Phase::PostLockStat,
        role: Role::Lock,
    },
    Event {
        phase: Phase::PostLockNamed,
        role: Role::Lock,
    },
    Event {
        phase: Phase::ContextAfter,
        role: Role::Parent,
    },
    Event {
        phase: Phase::Committed,
        role: Role::Lock,
    },
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lifecycle {
    Fresh,
    Active,
    Sealed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub resources: [Resource; RESOURCES],
    pub cleanup: [Option<Cleanup>; CLEANUP],
    pub events: [Option<Event>; EVENTS],
    pub faults: [Option<Fault>; FAULTS],
    pub primary: Option<Fault>,
    pub lock: LockState,
    pub reserved: bool,
    pub ever_held: bool,
    pub ever_uncertain: bool,
    pub terminal: bool,
    pub unchecked: bool,
    pub overflow: bool,
    pub lifecycle: Lifecycle,
    pub poisoned: bool,
}
impl Snapshot {
    fn complete_events(&self) -> bool {
        self.events
            .iter()
            .enumerate()
            .all(|(index, actual)| *actual == SUCCESS_EVENTS.get(index).copied())
    }
    pub(super) fn checked_clean(&self) -> bool {
        let mut expected = [None; CLEANUP];
        let mut index = 0;
        if self.ever_held {
            expected[index] = Some((1, Action::Unlock));
            index += 1;
        }
        for resource in [1, 0] {
            if self.resources[resource].observed_fd.is_some() {
                if index >= CLEANUP {
                    return false;
                }
                expected[index] = Some((resource, Action::Close));
                index += 1;
            }
        }
        let unlocks = self
            .cleanup
            .iter()
            .flatten()
            .filter(|c| c.action == Action::Unlock)
            .count();
        self.cleanup
            .iter()
            .zip(expected)
            .all(|(actual, expected)| match (actual, expected) {
                (Some(actual), Some((resource, action))) => {
                    actual.resource == resource && actual.action == action
                }
                (None, None) => true,
                _ => false,
            })
            && self.reserved
            && self.terminal
            && self.lifecycle == Lifecycle::Sealed
            && !self.poisoned
            && self.complete_events()
            && self.ever_held
            && self.lock == LockState::Released
            && self
                .resources
                .iter()
                .all(|r| r.disposition == Disposition::Closed && r.observed_fd.is_some())
            && !self.unchecked
            && !self.overflow
            && self.primary.is_none()
            && self.faults.iter().all(Option::is_none)
            && !self.ever_uncertain
            && if self.ever_held {
                self.lock == LockState::Released && unlocks == 1
            } else {
                matches!(self.lock, LockState::NotAttempted | LockState::NotHeld) && unlocks == 0
            }
            && self.resources.iter().enumerate().all(|(index, r)| {
                let closes = self
                    .cleanup
                    .iter()
                    .flatten()
                    .filter(|c| c.resource == index && c.action == Action::Close)
                    .count();
                r.id == index
                    && r.role == if index == 0 { Role::Parent } else { Role::Lock }
                    && match r.disposition {
                        Disposition::ReservedUnused => r.observed_fd.is_none() && closes == 0,
                        Disposition::Closed => r.observed_fd.is_some() && closes == 1,
                        _ => false,
                    }
            })
            && self.cleanup.iter().flatten().all(|c| {
                c.resource < RESOURCES
                    && self.resources[c.resource].role == c.role
                    && self.resources[c.resource].observed_fd.is_some()
                    && c.outcome == Outcome::Success
                    && match (c.action, c.role) {
                        (Action::Unlock, Role::Lock) => c.resource == 1 && c.phase == Phase::Unlock,
                        (Action::Close, Role::Lock) => c.phase == Phase::LockClose,
                        (Action::Close, Role::Parent) => c.phase == Phase::ParentClose,
                        _ => false,
                    }
            })
    }
}
#[derive(Clone, Copy)]
struct Report {
    view: Snapshot,
    admitted: [bool; RESOURCES],
    reserved: bool,
    events: usize,
    cleanup: usize,
    faults: usize,
}
/// Copyable ledger indices never carry an FD or grant a checked claim.
#[derive(Clone, Copy)]
pub(super) struct Ticket {
    pub index: usize,
    pub role: Role,
}
/// The only completion right. No Clone/Copy/DTO constructor and no Sink method
/// can manufacture it. Production custody belongs exclusively to Transaction.
pub(super) struct FinishPermit {
    sink: Sink,
    receipted: Cell<[bool; RESOURCES]>,
}
pub(super) struct Reservation {
    pub tickets: [Ticket; RESOURCES],
    pub finish: FinishPermit,
}
impl std::ops::Index<usize> for Reservation {
    type Output = Ticket;
    fn index(&self, index: usize) -> &Ticket {
        &self.tickets[index]
    }
}
impl FinishPermit {
    /// Record physical custody even when a callback has requested stop. The
    /// unique owner is stored before this receipt; normal admission may fail.
    pub(super) fn adopt_pending(&self, ticket: Ticket, fd: i32) {
        let mut receipted = self.receipted.get();
        self.sink.update(|r| {
            if r.view.lifecycle == Lifecycle::Active
                && ticket.index < RESOURCES
                && !receipted[ticket.index]
                && r.view.resources[ticket.index].role == ticket.role
                && fd >= 0
            {
                if r.view.resources[ticket.index].disposition != Disposition::ReservedUnused {
                    self.sink.protocol(r);
                }
                let slot = &mut r.view.resources[ticket.index];
                slot.observed_fd = Some(fd);
                slot.disposition = Disposition::Owned;
                receipted[ticket.index] = true;
            } else {
                self.sink.protocol(r);
            }
        });
        self.receipted.set(receipted);
    }
    pub(super) fn seal(self) {
        self.sink.seal();
    }
}
/// Cell transactions contain no callbacks and cannot suffer a RefCell borrow
/// conflict. External syscall callbacks only receive a clonable Sink, never
/// the completion right. This keeps the bounded cleanup lane always writable.
#[derive(Clone)]
pub(super) struct Sink {
    report: Rc<Cell<Report>>,
    sticky: Rc<Cell<bool>>,
}
pub(super) struct CallGuard {
    sink: Sink,
    phase: Phase,
    role: Role,
    armed: bool,
}
impl CallGuard {
    pub(super) fn complete(&mut self) {
        self.armed = false;
    }
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.armed {
            self.sink.sticky.set(true);
            if matches!(self.phase, Phase::LockClose | Phase::ParentClose) {
                let index = if self.role == Role::Parent { 0 } else { 1 };
                self.sink.update(|r| {
                    if r.view.lifecycle == Lifecycle::Sealed {
                        return;
                    }
                    r.view.poisoned = true;
                    if r.view.resources[index].disposition == Disposition::Owned {
                        r.view.resources[index].disposition = Disposition::Unknown;
                    }
                });
                self.sink.cleanup(Cleanup {
                    resource: index,
                    role: self.role,
                    phase: self.phase,
                    action: Action::Close,
                    outcome: Outcome::Unchecked(None),
                });
            }
            if self.phase == Phase::Unlock {
                self.sink.cleanup(Cleanup {
                    resource: 1,
                    role: Role::Lock,
                    phase: Phase::Unlock,
                    action: Action::Unlock,
                    outcome: Outcome::Unchecked(None),
                });
                let _ = self.sink.lock(LockState::ReleaseFailed);
            }
            self.sink.primary(Fault::new(
                self.phase,
                self.role,
                if std::thread::panicking() {
                    Kind::Panic
                } else {
                    Kind::Uncertain
                },
                None,
            ));
        }
    }
}
impl Sink {
    pub(super) fn new() -> Self {
        Self {
            report: Rc::new(Cell::new(Report {
                view: Snapshot {
                    resources: [
                        Resource {
                            id: 0,
                            role: Role::Parent,
                            observed_fd: None,
                            disposition: Disposition::ReservedUnused,
                        },
                        Resource {
                            id: 1,
                            role: Role::Lock,
                            observed_fd: None,
                            disposition: Disposition::ReservedUnused,
                        },
                    ],
                    cleanup: [None; CLEANUP],
                    events: [None; EVENTS],
                    faults: [None; FAULTS],
                    primary: None,
                    lock: LockState::NotAttempted,
                    reserved: false,
                    ever_held: false,
                    ever_uncertain: false,
                    terminal: false,
                    unchecked: false,
                    overflow: false,
                    lifecycle: Lifecycle::Fresh,
                    poisoned: false,
                },
                reserved: false,
                admitted: [false; RESOURCES],
                events: 0,
                cleanup: 0,
                faults: 0,
            })),
            sticky: Rc::new(Cell::new(false)),
        }
    }
    fn update(&self, f: impl FnOnce(&mut Report)) {
        let mut r = self.report.get();
        f(&mut r);
        self.report.set(r);
    }
    fn pristine(&self, r: &Report) -> bool {
        !self.sticky.get()
            && !r.view.poisoned
            && !r.view.overflow
            && !r.view.terminal
            && r.view.primary.is_none()
            && r.view.faults.iter().all(Option::is_none)
    }
    fn protocol(&self, r: &mut Report) {
        self.sticky.set(true);
        if r.view.lifecycle != Lifecycle::Sealed {
            r.view.poisoned = true;
        }
    }
    pub(super) fn permit_operation(&self) -> Result<(), Fault> {
        let mut accepted = false;
        self.update(|r| {
            if r.view.lifecycle == Lifecycle::Active && r.reserved && self.pristine(r) {
                accepted = true;
            } else {
                self.protocol(r);
            }
        });
        if accepted {
            Ok(())
        } else {
            Err(Fault::new(
                Phase::Ledger,
                Role::Parent,
                Kind::Invariant,
                None,
            ))
        }
    }
    pub(super) fn authority_ready(&self) -> Result<(), Fault> {
        let mut accepted = false;
        self.update(|r| {
            if r.view.lifecycle == Lifecycle::Active
                && r.reserved
                && self.pristine(r)
                && r.view.complete_events()
                && r.view.lock == LockState::Held
                && r.view.ever_held
                && !r.view.ever_uncertain
                && r.view
                    .resources
                    .iter()
                    .all(|v| v.disposition == Disposition::Owned)
                && r.view.cleanup.iter().all(Option::is_none)
            {
                accepted = true;
            } else {
                self.protocol(r);
            }
        });
        if accepted {
            Ok(())
        } else {
            Err(Fault::new(
                Phase::Committed,
                Role::Lock,
                Kind::Invariant,
                None,
            ))
        }
    }
    pub(super) fn reserve(
        &self,
        resources: usize,
        cleanup: usize,
        events: usize,
        faults: usize,
    ) -> Result<Reservation, Fault> {
        let mut accepted = false;
        self.update(|r| {
            if r.view.lifecycle == Lifecycle::Fresh
                && self.pristine(r)
                && !r.reserved
                && resources == RESOURCES
                && cleanup == CLEANUP
                && events == EVENTS
                && faults == FAULTS
            {
                r.reserved = true;
                r.view.reserved = true;
                r.view.lifecycle = Lifecycle::Active;
                accepted = true;
            } else {
                if r.view.lifecycle != Lifecycle::Sealed {
                    r.view.overflow = true;
                    if r.view.lifecycle == Lifecycle::Fresh {
                        // No resource exists and no permit was issued. Record
                        // capacity failure and terminal state atomically.
                        r.view.primary = Some(Fault::new(
                            Phase::Reserve,
                            Role::Parent,
                            Kind::Capacity,
                            None,
                        ));
                        r.view.poisoned = true;
                        r.view.terminal = true;
                        r.view.lifecycle = Lifecycle::Sealed;
                    }
                }
                self.protocol(r);
            }
        });
        if !accepted {
            self.sticky.set(true);
            return Err(Fault::new(
                Phase::Reserve,
                Role::Parent,
                Kind::Capacity,
                None,
            ));
        }
        Ok(Reservation {
            tickets: [
                Ticket {
                    index: 0,
                    role: Role::Parent,
                },
                Ticket {
                    index: 1,
                    role: Role::Lock,
                },
            ],
            finish: FinishPermit {
                sink: self.clone(),
                receipted: Cell::new([false; RESOURCES]),
            },
        })
    }
    pub(super) fn step(&self, phase: Phase, role: Role) -> Result<(), Fault> {
        let mut recorded = false;
        let mut kind = Kind::Invariant;
        self.update(|r| {
            if r.view.lifecycle == Lifecycle::Active
                && r.reserved
                && self.pristine(r)
                && r.events < EVENTS
                && SUCCESS_EVENTS.get(r.events) == Some(&Event { phase, role })
            {
                r.view.events[r.events] = Some(Event { phase, role });
                r.events += 1;
                recorded = true;
            } else {
                if r.view.lifecycle != Lifecycle::Sealed && r.events >= EVENTS {
                    r.view.overflow = true;
                    kind = Kind::Capacity;
                }
                self.protocol(r);
            }
        });
        if recorded {
            Ok(())
        } else {
            self.sticky.set(true);
            Err(Fault::new(phase, role, kind, None))
        }
    }
    pub(super) fn acquired(&self, ticket: Ticket, fd: i32) -> Result<(), Fault> {
        let mut accepted = false;
        self.update(|r| {
            let expected = match ticket.role {
                Role::Parent => (2, Phase::ParentOpen),
                Role::Lock => (5, Phase::LockOpen),
            };
            if r.view.lifecycle == Lifecycle::Active
                && self.pristine(r)
                && r.reserved
                && fd >= 0
                && r.events == expected.0
                && r.view.events[expected.0 - 1]
                    == Some(Event {
                        phase: expected.1,
                        role: ticket.role,
                    })
                && ticket.index < RESOURCES
                && !r.admitted[ticket.index]
                && r.view.resources[ticket.index].role == ticket.role
                && matches!(
                    r.view.resources[ticket.index].disposition,
                    Disposition::ReservedUnused | Disposition::Owned
                )
                && r.view.resources[ticket.index]
                    .observed_fd
                    .is_none_or(|v| v == fd)
            {
                let slot = &mut r.view.resources[ticket.index];
                slot.observed_fd = Some(fd);
                slot.disposition = Disposition::Owned;
                r.admitted[ticket.index] = true;
                accepted = true;
            } else {
                self.protocol(r);
            }
        });
        if accepted {
            Ok(())
        } else {
            Err(Fault::new(
                Phase::Ledger,
                ticket.role,
                Kind::Invariant,
                None,
            ))
        }
    }
    pub(super) fn close(&self, ticket: Ticket, outcome: Outcome) {
        let mut active = false;
        self.update(|r| {
            if r.view.lifecycle != Lifecycle::Active {
                self.protocol(r);
                return;
            }
            if self.pristine(r) && !r.view.complete_events() {
                self.protocol(r);
            }
            active = true;
            if ticket.index < RESOURCES
                && r.view.resources[ticket.index].role == ticket.role
                && r.view.resources[ticket.index].disposition == Disposition::Owned
            {
                r.view.resources[ticket.index].disposition = match outcome {
                    Outcome::Success => Disposition::Closed,
                    Outcome::Error(_) => Disposition::CloseFailed,
                    Outcome::Unchecked(_) => Disposition::UncheckedClosed,
                };
            } else {
                self.protocol(r);
                if ticket.index < RESOURCES
                    && r.view.resources[ticket.index].disposition == Disposition::ReservedUnused
                {
                    r.view.resources[ticket.index].disposition = Disposition::Unknown;
                }
            }
            if outcome != Outcome::Success {
                r.view.poisoned = true;
            }
        });
        if !active {
            return;
        }
        self.cleanup(Cleanup {
            resource: ticket.index,
            role: ticket.role,
            phase: match ticket.role {
                Role::Parent => Phase::ParentClose,
                Role::Lock => Phase::LockClose,
            },
            action: Action::Close,
            outcome,
        });
    }
    pub(super) fn cleanup(&self, step: Cleanup) {
        self.update(|r| {
            if r.view.lifecycle != Lifecycle::Active {
                self.protocol(r);
                return;
            }
            if self.pristine(r) && !r.view.complete_events() {
                self.protocol(r);
            }
            if step.outcome != Outcome::Success {
                r.view.poisoned = true;
            }
            if r.cleanup < CLEANUP {
                r.view.cleanup[r.cleanup] = Some(step);
                r.cleanup += 1;
            } else {
                r.view.overflow = true;
                self.sticky.set(true);
            }
        });
    }
    pub(super) fn primary(&self, fault: Fault) {
        self.update(|r| {
            if r.view.lifecycle == Lifecycle::Sealed {
                self.sticky.set(true);
                return;
            }
            r.view.poisoned = true;
            if r.view.primary.is_none() {
                r.view.primary = Some(fault);
            } else if r.faults < FAULTS {
                r.view.faults[r.faults] = Some(fault);
                r.faults += 1;
            } else {
                r.view.overflow = true;
                self.sticky.set(true);
            }
        });
    }
    pub(super) fn lock(&self, state: LockState) -> Result<(), Fault> {
        let mut accepted = false;
        self.update(|r| {
            if r.view.lifecycle != Lifecycle::Active {
                self.protocol(r);
                return;
            }
            if state == LockState::NotAttempted {
                self.protocol(r);
                return;
            }
            if matches!(
                state,
                LockState::Held | LockState::NotHeld | LockState::Uncertain
            ) && (!self.pristine(r)
                || r.view.lock != LockState::NotAttempted
                || r.events != 8
                || r.view.events[7]
                    != Some(Event {
                        phase: Phase::TryLock,
                        role: Role::Lock,
                    }))
            {
                self.protocol(r);
                return;
            }
            if matches!(
                state,
                LockState::Released | LockState::ReleasedUncertain | LockState::ReleaseFailed
            ) && !r.view.poisoned
                && !self.sticky.get()
                && (!matches!(r.view.lock, LockState::Held | LockState::Uncertain)
                    || !r
                        .view
                        .cleanup
                        .iter()
                        .flatten()
                        .any(|c| c.resource == 1 && c.action == Action::Unlock))
            {
                self.protocol(r);
                return;
            }
            r.view.lock = state;
            r.view.ever_held |= state == LockState::Held;
            r.view.ever_uncertain |= state == LockState::Uncertain;
            accepted = true;
        });
        if accepted {
            Ok(())
        } else {
            Err(Fault::new(
                Phase::TryLock,
                Role::Lock,
                Kind::Invariant,
                None,
            ))
        }
    }
    /// A callback may request termination, but cannot freeze cleanup evidence.
    pub(super) fn request_stop(&self) {
        self.update(|r| self.protocol(r));
    }
    fn seal(&self) {
        self.update(|r| {
            if r.view.lifecycle == Lifecycle::Sealed {
                return;
            }
            if r.view.lifecycle == Lifecycle::Fresh
                || r.view
                    .resources
                    .iter()
                    .any(|v| v.disposition == Disposition::Owned)
                || (r.view.primary.is_none()
                    && (!r.view.complete_events()
                        || !r.view.ever_held
                        || r.view.lock != LockState::Released
                        || !r
                            .view
                            .resources
                            .iter()
                            .all(|v| v.disposition == Disposition::Closed)))
            {
                self.protocol(r);
            }
            r.view.terminal = true;
            r.view.lifecycle = Lifecycle::Sealed;
        });
    }
    pub(super) fn fallback(&self) {
        self.sticky.set(true);
        self.primary(Fault::new(
            Phase::Fallback,
            Role::Parent,
            if std::thread::panicking() {
                Kind::Panic
            } else {
                Kind::Fallback
            },
            None,
        ));
    }
    pub(super) fn call_guard(&self, phase: Phase, role: Role) -> CallGuard {
        CallGuard {
            sink: self.clone(),
            phase,
            role,
            armed: true,
        }
    }
    pub(super) fn snapshot(&self) -> Snapshot {
        let mut v = self.report.get().view;
        v.unchecked |= self.sticky.get();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r2_physical_event_capacity_plus_one_never_writes_past_fixed_storage() {
        let sink = Sink::new();
        sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        // Direct private counter injection isolates the physical 32-slot
        // boundary. Operational histories are stricter: only 15 are legal.
        {
            let mut report = sink.report.get();
            report.events = EVENTS;
            report.view.events = [Some(Event {
                phase: Phase::Ledger,
                role: Role::Parent,
            }); EVENTS];
            sink.report.set(report);
        }
        let before = sink.snapshot().events;
        assert!(sink.step(Phase::ContextBefore, Role::Parent).is_err());
        let view = sink.snapshot();
        assert_eq!(view.events, before);
        assert!(view.overflow && view.poisoned && view.unchecked);
        assert!(sink.permit_operation().is_err());
        assert!(sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).is_err());
    }
    #[test]
    fn r2_report_borrow_failure_and_fault_cleanup_boundaries_never_reopen() {
        let sink = Sink::new();
        // Value snapshots cannot block stop, receipt, or cleanup recording.
        let held = sink.report.get();
        sink.request_stop();
        assert_eq!(held.view.lifecycle, Lifecycle::Fresh);
        assert!(sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).is_err());
        assert!(sink.snapshot().unchecked);
        let sink = Sink::new();
        sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        let fault = Fault::new(Phase::Ledger, Role::Parent, Kind::Invariant, None);
        sink.primary(fault);
        for _ in 0..FAULTS {
            sink.primary(fault);
        }
        assert!(!sink.snapshot().overflow);
        sink.primary(fault);
        assert!(sink.snapshot().overflow);
        let sink = Sink::new();
        sink.reserve(RESOURCES, CLEANUP, EVENTS, FAULTS).unwrap();
        sink.primary(fault);
        let step = Cleanup {
            resource: 0,
            role: Role::Parent,
            phase: Phase::ParentClose,
            action: Action::Close,
            outcome: Outcome::Error(Errno::EIO),
        };
        for _ in 0..CLEANUP {
            sink.cleanup(step);
        }
        assert!(!sink.snapshot().overflow);
        sink.cleanup(step);
        assert!(sink.snapshot().overflow);
        assert!(sink.permit_operation().is_err());
    }
}
