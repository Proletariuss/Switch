// Copyright (c) 2026 iniwex5
// Modifications copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Caller-driven fan-out with a bounded mailbox per subscriber.
//!
//! A QMI manager and the modem pool publish indications to a small set of
//! caller threads. Both need the same shape - a blocking `sync_channel` on
//! the hot path, a bounded pending deque when a subscriber is busy, and a
//! criticality rule so a status storm cannot evict the indications that carry
//! messages, calls or card state - so the mechanism lives here once. The
//! publisher keeps its own event type and criticality predicate; only the
//! queue and back-pressure policy are shared.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

// Keep pressure reporting independent of the daemon and safe after its launcher exits.
fn write_diagnostic(arguments: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{arguments}");
}

/// Bounded fan-out of one event type to caller-owned subscriptions.
pub struct Fanout<E> {
    subscribers: Mutex<BTreeMap<u64, Subscriber<E>>>,
    next_id: AtomicU64,
    channel_capacity: usize,
    max_subscribers: usize,
    pending_limit: usize,
}

struct Subscriber<E> {
    sender: SyncSender<E>,
    pending: VecDeque<E>,
}

/// One caller's view of a [`Fanout`]. Dropping it removes the registration.
pub struct Subscription<E> {
    id: Option<u64>,
    fanout: Weak<Fanout<E>>,
    receiver: Receiver<E>,
}

/// What one publish delivered and discarded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PublishReport {
    /// Events handed to a subscriber channel by this publish without going
    /// through the backlog.
    pub forwarded: u64,
    /// Events dropped because a mailbox was full.
    pub dropped: u64,
}

/// Criticality rule and log label for one publisher.
pub struct Policy<E> {
    /// Prefix used in mailbox saturation logs, for example `QMI manager`.
    pub label: &'static str,
    /// Whether an event must survive a backlog overflow when possible.
    pub critical: fn(&E) -> bool,
}

impl<E> Fanout<E> {
    #[must_use]
    pub fn new(channel_capacity: usize, max_subscribers: usize, pending_limit: usize) -> Self {
        Self {
            subscribers: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(0),
            channel_capacity,
            max_subscribers,
            pending_limit,
        }
    }

    /// Register a subscriber with the fan-out's default queue capacity. The
    /// oldest registration is evicted once `max_subscribers` is reached, so a
    /// forgotten client cannot retain memory indefinitely.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>) -> Subscription<E> {
        self.subscribe_with_capacity(self.channel_capacity)
    }

    /// Register a subscriber with an explicit queue capacity.
    #[must_use]
    pub fn subscribe_with_capacity(self: &Arc<Self>, capacity: usize) -> Subscription<E> {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let mut subscribers = lock(&self.subscribers);
        if subscribers.len() >= self.max_subscribers
            && let Some(oldest) = subscribers.keys().next().copied()
        {
            subscribers.remove(&oldest);
        }
        let id = loop {
            let candidate = self
                .next_id
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1)
                .max(1);
            if !subscribers.contains_key(&candidate) {
                break candidate;
            }
        };
        subscribers.insert(
            id,
            Subscriber {
                sender,
                pending: VecDeque::new(),
            },
        );
        Subscription {
            id: Some(id),
            fanout: Arc::downgrade(self),
            receiver,
        }
    }

    /// Publish one event. The event is built only when a subscriber exists,
    /// moved into the final subscriber and cloned only for true fan-out, so
    /// the normal single-subscriber bridge avoids a deep clone entirely.
    pub fn publish(&self, make_event: impl FnOnce() -> E, policy: &Policy<E>) -> PublishReport
    where
        E: Clone,
    {
        let mut subscribers = lock(&self.subscribers);
        if subscribers.is_empty() {
            return PublishReport::default();
        }
        let owner = subscribers.keys().next_back().copied();
        let mut value = Some(make_event());
        let mut report = PublishReport::default();
        subscribers.retain(|id, subscriber| {
            while let Some(queued) = subscriber.pending.pop_front() {
                match subscriber.sender.try_send(queued) {
                    Ok(()) => {}
                    Err(TrySendError::Full(queued)) => {
                        subscriber.pending.push_front(queued);
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => return false,
                }
            }
            let payload = if Some(*id) == owner {
                let Some(payload) = value.take() else {
                    return true;
                };
                payload
            } else {
                let Some(payload) = value.as_ref() else {
                    return true;
                };
                payload.clone()
            };
            match subscriber.sender.try_send(payload) {
                Ok(()) => {
                    report.forwarded += 1;
                    true
                }
                Err(TrySendError::Full(payload)) => {
                    if subscriber.pending.len() >= self.pending_limit {
                        if let Some(index) = subscriber
                            .pending
                            .iter()
                            .position(|queued| !(policy.critical)(queued))
                        {
                            subscriber.pending.remove(index);
                            report.dropped += 1;
                        } else if !(policy.critical)(&payload) {
                            // Do not evict a retained critical indication
                            // merely to make room for a best-effort update:
                            // the source event is the only safe item to drop.
                            report.dropped += 1;
                            write_diagnostic(format_args!(
                                "{} subscriber mailbox full; dropping non-critical event",
                                policy.label
                            ));
                            return true;
                        } else {
                            subscriber.pending.pop_front();
                            report.dropped += 1;
                            write_diagnostic(format_args!(
                                "{} subscriber mailbox saturated; evicting critical event",
                                policy.label
                            ));
                        }
                    }
                    subscriber.pending.push_back(payload);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
        report
    }
}

impl<E> Subscription<E> {
    pub fn recv(&self) -> std::result::Result<E, mpsc::RecvError> {
        loop {
            self.flush_pending();
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(mpsc::RecvError),
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> std::result::Result<E, RecvTimeoutError> {
        let started = std::time::Instant::now();
        loop {
            self.flush_pending();
            match self.receiver.try_recv() {
                Ok(event) => return Ok(event),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(RecvTimeoutError::Disconnected);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            match self
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Timeout) if started.elapsed() >= timeout => {
                    return Err(RecvTimeoutError::Timeout);
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
            }
        }
    }

    pub fn try_recv(&self) -> std::result::Result<E, mpsc::TryRecvError> {
        self.flush_pending();
        self.receiver.try_recv()
    }

    fn flush_pending(&self) {
        let Some(fanout) = self.fanout.upgrade() else {
            return;
        };
        let mut subscribers = lock(&fanout.subscribers);
        let Some(subscriber) = subscribers.get_mut(&self.id.unwrap_or_default()) else {
            return;
        };
        while let Some(event) = subscriber.pending.pop_front() {
            match subscriber.sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    subscriber.pending.push_front(event);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    subscriber.pending.clear();
                    break;
                }
            }
        }
    }

    pub fn close(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Some(fanout) = self.fanout.upgrade() {
            lock(&fanout.subscribers).remove(&id);
        }
    }
}

impl<E> Drop for Subscription<E> {
    fn drop(&mut self) {
        self.close();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
