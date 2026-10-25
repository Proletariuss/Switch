//! Per-reader/SIM lifecycle core for the future native VoWiFi owner.
//!
//! This module intentionally has no transport, PC/SC, DNS, proxy, or task
//! creation dependency. It establishes the ownership invariants before a live
//! IKE/IMS worker is connected: a reader/SIM pair has one generation at a
//! time; cancellation is scoped to that generation; and a late worker result
//! cannot clear a replacement session.
//!
//! The live owner must keep this order when it consumes a stop signal:
//!
//! `quiesce new work -> RegisteredImsSession::shutdown() -> signal IKE stop
//! -> await this exact generation -> worker_finished`.
//!
//! Signalling IKE before the registered IMS session is shut down is unsafe:
//! its security-lease cleanup is serviced by the still-running encrypted
//! network owner.

#![allow(
    dead_code,
    reason = "The reducer is intentionally dormant until the native IKE/IMS runtime is gated on; it is fully unit-tested but has no public route yet."
)]

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

use tokio::sync::watch;
use zeroize::Zeroizing;

const MAX_RECOVERY_CONCURRENCY: usize = 8;

/// Non-serializable, non-debuggable key for one physical reader plus the card
/// identity observed during preparation. `sim_id` accepts ICCID when present
/// and IMSI only as the existing read-path fallback; callers must never obtain
/// it from an HTTP request.
pub(crate) struct ReaderSessionKey {
    reader: String,
    sim_id: Zeroizing<String>,
}

impl ReaderSessionKey {
    pub(crate) fn new(reader: &str, sim_id: &str) -> Result<Self, SessionKeyError> {
        let reader = reader.trim();
        let sim_id = sim_id.trim();
        if reader.is_empty()
            || reader.len() > 512
            || reader.bytes().any(|byte| byte.is_ascii_control())
            || !(5..=22).contains(&sim_id.len())
            || !sim_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(SessionKeyError::Invalid);
        }
        Ok(Self {
            reader: reader.to_owned(),
            sim_id: Zeroizing::new(sim_id.to_owned()),
        })
    }
}

impl Clone for ReaderSessionKey {
    fn clone(&self) -> Self {
        Self {
            reader: self.reader.clone(),
            sim_id: Zeroizing::new(self.sim_id.as_str().to_owned()),
        }
    }
}

impl PartialEq for ReaderSessionKey {
    fn eq(&self, other: &Self) -> bool {
        self.reader == other.reader && self.sim_id.as_str() == other.sim_id.as_str()
    }
}

impl Eq for ReaderSessionKey {}

impl Hash for ReaderSessionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.reader.hash(state);
        self.sim_id.as_str().hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionKeyError {
    Invalid,
}

/// Opaque monotonically allocated worker generation. It is safe to expose in
/// non-subscriber diagnostics, but never substitutes for the reader/SIM key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SessionGeneration(u64);

impl SessionGeneration {
    pub(crate) const fn value(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_value(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeSessionPhase {
    Prepared,
    Running,
    Stopping,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeSessionSnapshot {
    pub(crate) generation: SessionGeneration,
    pub(crate) phase: NativeSessionPhase,
}

/// Held only by the future worker that owns the matching immutable session
/// context. It carries the scoped cancellation receiver, never a mutable
/// global "active reader" slot.
pub(crate) struct NativeSessionPermit {
    key: ReaderSessionKey,
    generation: SessionGeneration,
    stop: watch::Receiver<bool>,
    cancel: watch::Sender<bool>,
}

impl NativeSessionPermit {
    pub(crate) fn matches_key(&self, key: &ReaderSessionKey) -> bool {
        self.key == *key
    }

    pub(crate) const fn generation(&self) -> SessionGeneration {
        self.generation
    }

    pub(crate) fn cancellation_sender(&self) -> watch::Sender<bool> {
        self.cancel.clone()
    }

    pub(crate) fn stop_signal(&self) -> watch::Receiver<bool> {
        self.stop.clone()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReserveError {
    Active,
    GenerationExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartError {
    Missing,
    StaleGeneration,
    NotPrepared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopDisposition {
    Signalled,
    AlreadyStopping,
    Missing,
    StaleGeneration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionDisposition {
    Removed,
    Missing,
    StaleGeneration,
}

struct SessionEntry {
    generation: SessionGeneration,
    phase: NativeSessionPhase,
    stop: watch::Sender<bool>,
    recovery: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
    recovery_claimed: bool,
    last_recovery_attempt: u64,
    recovery_failures: u32,
    recovery_after: Option<std::time::Instant>,
}

/// Pure ownership registry. It does not spawn workers and cannot start IKE,
/// read a card, resolve a host, or alter proxy state. The future actor inserts
/// one immutable `NativeSessionContext` alongside the returned permit.
#[derive(Default)]
pub(crate) struct NativeVoWiFiSupervisor {
    next_generation: u64,
    next_recovery_attempt: u64,
    sessions: HashMap<ReaderSessionKey, SessionEntry>,
}

impl Drop for NativeVoWiFiSupervisor {
    fn drop(&mut self) {
        for entry in self.sessions.values() {
            entry.stop.send_replace(true);
        }
    }
}

impl NativeVoWiFiSupervisor {
    pub(crate) fn reserve(
        &mut self,
        key: ReaderSessionKey,
    ) -> Result<NativeSessionPermit, ReserveError> {
        if self.sessions.contains_key(&key) {
            return Err(ReserveError::Active);
        }
        let generation = self.allocate_generation()?;
        let (stop, stop_receiver) = watch::channel(false);
        self.sessions.insert(
            key.clone(),
            SessionEntry {
                generation,
                phase: NativeSessionPhase::Prepared,
                stop: stop.clone(),
                recovery: None,
                recovery_claimed: false,
                last_recovery_attempt: 0,
                recovery_failures: 0,
                recovery_after: None,
            },
        );
        Ok(NativeSessionPermit {
            key,
            generation,
            stop: stop_receiver,
            cancel: stop,
        })
    }

    /// Retain an opaque cleanup resource without adding transport dependencies.
    pub(crate) fn retain_recovery<T: Send + Sync + 'static>(
        &mut self,
        permit: &NativeSessionPermit,
        resource: std::sync::Arc<T>,
    ) -> bool {
        let Some(entry) = self.sessions.get_mut(&permit.key) else {
            return false;
        };
        if entry.generation != permit.generation || entry.recovery.is_some() {
            return false;
        }
        entry.recovery = Some(resource);
        true
    }

    pub(crate) fn recovery<T: Send + Sync + 'static>(
        &self,
        permit: &NativeSessionPermit,
    ) -> Option<std::sync::Arc<T>> {
        let entry = self.sessions.get(&permit.key)?;
        if entry.generation != permit.generation {
            return None;
        }
        entry.recovery.as_ref()?.clone().downcast().ok()
    }

    /// Snapshot a bounded batch without exposing resource details or claiming IO.
    pub(crate) fn recovery_candidates<T: Send + Sync + 'static>(
        &self,
        limit: usize,
        eligible: impl Fn(&T) -> bool,
    ) -> Vec<(ReaderSessionKey, SessionGeneration)> {
        let mut candidates: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(key, entry)| {
                if entry.recovery_claimed
                    || !*entry.stop.borrow()
                    || entry
                        .recovery_after
                        .is_some_and(|deadline| std::time::Instant::now() < deadline)
                {
                    return None;
                }
                let resource = entry.recovery.as_ref()?.as_ref().downcast_ref::<T>()?;
                eligible(resource)
                    .then(|| (key.clone(), entry.generation, entry.last_recovery_attempt))
            })
            .collect();
        candidates.sort_by_key(|(_, generation, attempt)| (*attempt, generation.0));
        candidates
            .into_iter()
            .take(limit.min(MAX_RECOVERY_CONCURRENCY))
            .map(|(key, generation, _)| (key, generation))
            .collect()
    }

    /// Recreate only a stopped, retained generation; never use the current key alone.
    pub(crate) fn recovery_permit(
        &self,
        key: &ReaderSessionKey,
        generation: SessionGeneration,
    ) -> Option<NativeSessionPermit> {
        let entry = self.sessions.get(key)?;
        if entry.generation != generation
            || entry.recovery.is_none()
            || entry.recovery_claimed
            || !*entry.stop.borrow()
            || entry
                .recovery_after
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
            return None;
        }
        Some(NativeSessionPermit {
            key: key.clone(),
            generation,
            stop: entry.stop.subscribe(),
            cancel: entry.stop.clone(),
        })
    }

    pub(crate) fn claim_recovery(&mut self, permit: &NativeSessionPermit) -> bool {
        if self
            .sessions
            .values()
            .filter(|entry| entry.recovery_claimed)
            .count()
            >= MAX_RECOVERY_CONCURRENCY
        {
            return false;
        }
        let Some(entry) = self.sessions.get_mut(&permit.key) else {
            return false;
        };
        if entry.generation != permit.generation
            || entry.recovery.is_none()
            || entry.recovery_claimed
            || entry
                .recovery_after
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
            return false;
        }
        let Some(attempt) = self.next_recovery_attempt.checked_add(1) else {
            return false;
        };
        self.next_recovery_attempt = attempt;
        entry.last_recovery_attempt = attempt;
        entry.recovery_claimed = true;
        entry.phase = NativeSessionPhase::Stopping;
        entry.stop.send_replace(true);
        true
    }

    pub(crate) fn recovery_failed(&mut self, permit: &NativeSessionPermit) {
        self.recovery_failed_at(permit, std::time::Instant::now());
    }

    fn recovery_failed_at(&mut self, permit: &NativeSessionPermit, now: std::time::Instant) {
        if let Some(entry) = self.sessions.get_mut(&permit.key)
            && entry.generation == permit.generation
            && entry.recovery_claimed
        {
            entry.recovery_claimed = false;
            entry.recovery_failures = entry.recovery_failures.saturating_add(1);
            let seconds = (1_u64 << entry.recovery_failures.saturating_sub(1).min(6)).min(60);
            entry.recovery_after = now.checked_add(std::time::Duration::from_secs(seconds));
        }
    }

    pub(crate) fn release_recovery_claim(&mut self, permit: &NativeSessionPermit) {
        if let Some(entry) = self.sessions.get_mut(&permit.key)
            && entry.generation == permit.generation
        {
            entry.recovery_claimed = false;
        }
    }

    pub(crate) fn mark_running(&mut self, permit: &NativeSessionPermit) -> Result<(), StartError> {
        let entry = self
            .sessions
            .get_mut(&permit.key)
            .ok_or(StartError::Missing)?;
        if entry.generation != permit.generation {
            return Err(StartError::StaleGeneration);
        }
        if entry.phase != NativeSessionPhase::Prepared {
            return Err(StartError::NotPrepared);
        }
        entry.phase = NativeSessionPhase::Running;
        Ok(())
    }

    /// Requests shutdown for exactly the matching worker. The worker performs
    /// IMS/TCP/security cleanup before it observes this IKE stop receiver.
    pub(crate) fn request_stop(&mut self, permit: &NativeSessionPermit) -> StopDisposition {
        let Some(entry) = self.sessions.get_mut(&permit.key) else {
            return StopDisposition::Missing;
        };
        if entry.generation != permit.generation {
            return StopDisposition::StaleGeneration;
        }
        if entry.phase == NativeSessionPhase::Stopping {
            return StopDisposition::AlreadyStopping;
        }
        entry.phase = NativeSessionPhase::Stopping;
        entry.stop.send_replace(true);
        StopDisposition::Signalled
    }

    /// Stop by the opaque daemon-issued generation returned to the caller.
    /// The generation is unique across this supervisor lifetime, so callers
    /// never need to send a reader's ICCID/IMSI back to identify a session.
    pub(crate) fn request_stop_generation(
        &mut self,
        generation: SessionGeneration,
    ) -> StopDisposition {
        let Some(entry) = self
            .sessions
            .values_mut()
            .find(|entry| entry.generation == generation)
        else {
            return StopDisposition::Missing;
        };
        if entry.phase == NativeSessionPhase::Stopping {
            return StopDisposition::AlreadyStopping;
        }
        entry.phase = NativeSessionPhase::Stopping;
        entry.stop.send_replace(true);
        StopDisposition::Signalled
    }

    pub(crate) fn snapshot_generation(
        &self,
        generation: SessionGeneration,
    ) -> Option<NativeSessionSnapshot> {
        self.sessions
            .values()
            .find(|entry| entry.generation == generation)
            .map(|entry| NativeSessionSnapshot {
                generation: entry.generation,
                phase: entry.phase,
            })
    }

    /// Requests stopping every current session. It intentionally does not
    /// remove an entry; the matching worker must finish and report back before
    /// a replacement may reserve the same reader/SIM key.
    pub(crate) fn request_stop_all(&mut self) -> usize {
        let mut signalled = 0;
        for entry in self.sessions.values_mut() {
            if entry.phase != NativeSessionPhase::Stopping {
                entry.phase = NativeSessionPhase::Stopping;
                entry.stop.send_replace(true);
                signalled += 1;
            }
        }
        signalled
    }

    /// Removes only an exact completed worker generation. Late completions
    /// cannot erase a replacement session that reused the same reader/SIM.
    pub(crate) fn worker_finished(
        &mut self,
        permit: &NativeSessionPermit,
    ) -> CompletionDisposition {
        match self.sessions.get(&permit.key) {
            None => CompletionDisposition::Missing,
            Some(entry) if entry.generation != permit.generation => {
                CompletionDisposition::StaleGeneration
            }
            Some(_) => {
                self.sessions.remove(&permit.key);
                CompletionDisposition::Removed
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub(crate) fn snapshot(&self, key: &ReaderSessionKey) -> Option<NativeSessionSnapshot> {
        self.sessions.get(key).map(|entry| NativeSessionSnapshot {
            generation: entry.generation,
            phase: entry.phase,
        })
    }

    fn allocate_generation(&mut self) -> Result<SessionGeneration, ReserveError> {
        let next = self
            .next_generation
            .checked_add(1)
            .ok_or(ReserveError::GenerationExhausted)?;
        self.next_generation = next;
        Ok(SessionGeneration(next))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_recovery_delays_all_admission_paths_and_caps_backoff() {
        let mut owner = NativeVoWiFiSupervisor::default();
        let key = key("backoff-reader", "8901234567890123456");
        let permit = owner.reserve(key.clone()).unwrap();
        owner.retain_recovery(&permit, std::sync::Arc::new(1_u32));
        assert!(owner.claim_recovery(&permit));
        let now = std::time::Instant::now();
        owner.recovery_failed_at(&permit, now);
        assert!(!owner.claim_recovery(&permit));
        assert!(owner.recovery_permit(&key, permit.generation()).is_none());
        assert!(owner.recovery_candidates::<u32>(8, |_| true).is_empty());
        assert_eq!(
            owner.sessions.get(&key).unwrap().recovery_after,
            Some(now + std::time::Duration::from_secs(1))
        );
        for expected in [2, 4, 8, 16, 32, 60, 60] {
            owner.sessions.get_mut(&key).unwrap().recovery_after = None;
            assert!(owner.claim_recovery(&permit));
            owner.recovery_failed_at(&permit, now);
            assert_eq!(
                owner.sessions.get(&key).unwrap().recovery_after,
                Some(now + std::time::Duration::from_secs(expected))
            );
        }
        assert!(owner.reserve(key).is_err());
    }

    #[test]
    fn recovery_failure_rotation_prioritizes_never_attempted_sessions() {
        let mut owner = NativeVoWiFiSupervisor::default();
        let mut permits = Vec::new();
        for index in 0..10 {
            let permit = owner
                .reserve(key(&format!("fair-reader-{index}"), "8901234567890123456"))
                .unwrap();
            owner.retain_recovery(&permit, std::sync::Arc::new(index as u32));
            owner.request_stop(&permit);
            permits.push(permit);
        }
        let first = owner.recovery_candidates::<u32>(8, |_| true);
        assert_eq!(
            first
                .iter()
                .map(|(_, generation)| *generation)
                .collect::<Vec<_>>(),
            permits[..8]
                .iter()
                .map(|p| p.generation())
                .collect::<Vec<_>>()
        );
        for permit in &permits[..8] {
            assert!(owner.claim_recovery(permit));
        }
        for permit in &permits[..8] {
            owner.release_recovery_claim(permit);
        }
        let next = owner.recovery_candidates::<u32>(2, |_| true);
        assert_eq!(
            next.iter()
                .map(|(_, generation)| *generation)
                .collect::<Vec<_>>(),
            permits[8..]
                .iter()
                .map(|p| p.generation())
                .collect::<Vec<_>>()
        );
        for permit in &permits[8..] {
            assert!(owner.claim_recovery(permit));
            owner.release_recovery_claim(permit);
        }
        assert_eq!(
            owner.recovery_candidates::<u32>(1, |_| true)[0].1,
            permits[0].generation()
        );
        owner.next_recovery_attempt = u64::MAX;
        assert!(!owner.claim_recovery(&permits[0]));
        assert!(
            owner
                .recovery_permit(&first[0].0, permits[0].generation())
                .is_some()
        );
    }

    #[test]
    fn recovery_claim_limit_applies_across_batches_and_reopens_only_after_completion() {
        let mut owner = NativeVoWiFiSupervisor::default();
        let mut permits = Vec::new();
        for index in 0..10 {
            let permit = owner
                .reserve(key(
                    &format!("global-recovery-{index}"),
                    "8901234567890123456",
                ))
                .unwrap();
            assert!(owner.retain_recovery(&permit, std::sync::Arc::new(index)));
            permits.push(permit);
        }
        for permit in &permits[..MAX_RECOVERY_CONCURRENCY] {
            assert!(owner.claim_recovery(permit));
        }
        assert!(!owner.claim_recovery(&permits[8]));
        assert!(!owner.claim_recovery(&permits[9]));
        owner.release_recovery_claim(&permits[0]);
        assert!(owner.claim_recovery(&permits[8]));
        assert!(!owner.claim_recovery(&permits[9]));
        owner.worker_finished(&permits[1]);
        assert!(owner.claim_recovery(&permits[9]));
    }

    #[test]
    fn recovery_batch_filters_before_bounding_and_excludes_claimed_resources() {
        let mut owner = NativeVoWiFiSupervisor::default();
        let mut permits = Vec::new();
        for index in 0..12 {
            let permit = owner
                .reserve(key(&format!("batch-reader-{index}"), "8901234567890123456"))
                .unwrap();
            assert!(owner.retain_recovery(&permit, std::sync::Arc::new(index as u32)));
            owner.request_stop(&permit);
            permits.push(permit);
        }
        assert!(owner.recovery_candidates::<u32>(0, |_| true).is_empty());
        assert_eq!(owner.recovery_candidates::<u32>(100, |_| true).len(), 8);
        let filtered = owner.recovery_candidates::<u32>(8, |value| *value == 11);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1, permits[11].generation());
        assert!(owner.claim_recovery(&permits[11]));
        assert!(
            owner
                .recovery_candidates::<u32>(8, |value| *value == 11)
                .is_empty()
        );
        assert!(owner.recovery_candidates::<String>(8, |_| true).is_empty());
    }

    #[test]
    fn abandoned_permit_can_be_recovered_only_for_stopped_original_generation() {
        let mut owner = NativeVoWiFiSupervisor::default();
        let key = key("abandoned-reader", "8901234567890123456");
        let original = owner.reserve(key.clone()).unwrap();
        let generation = original.generation();
        assert!(owner.retain_recovery(&original, std::sync::Arc::new(42_u32)));
        assert!(owner.recovery_permit(&key, generation).is_none());
        owner.request_stop(&original);
        drop(original);
        let recovered = owner.recovery_permit(&key, generation).unwrap();
        assert!(recovered.matches_key(&key));
        assert_eq!(recovered.generation(), generation);
        assert!(*recovered.stop_signal().borrow());
        assert_eq!(*owner.recovery::<u32>(&recovered).unwrap(), 42);
        assert!(owner.claim_recovery(&recovered));
        assert!(owner.recovery_permit(&key, generation).is_none());
        owner.release_recovery_claim(&recovered);
        assert!(owner.recovery_permit(&key, generation).is_some());
        owner.worker_finished(&recovered);
        let next = owner.reserve(key.clone()).unwrap();
        assert!(owner.retain_recovery(&next, std::sync::Arc::new(7_u32)));
        owner.request_stop(&next);
        assert!(owner.recovery_permit(&key, generation).is_none());
        assert!(owner.recovery_permit(&key, next.generation()).is_some());
    }

    #[test]
    fn recovery_claim_is_exclusive_and_old_claim_cannot_unlock_replacement() {
        let mut owner = NativeVoWiFiSupervisor::default();
        let key = key("claim-reader", "8901234567890123456");
        let permit = owner.reserve(key.clone()).unwrap();
        assert!(!owner.claim_recovery(&permit));
        assert!(owner.retain_recovery(&permit, std::sync::Arc::new(1_u32)));
        assert!(owner.claim_recovery(&permit));
        assert!(!owner.claim_recovery(&permit));
        owner.release_recovery_claim(&permit);
        assert!(owner.claim_recovery(&permit));
        owner.worker_finished(&permit);
        let next = owner.reserve(key).unwrap();
        assert!(owner.retain_recovery(&next, std::sync::Arc::new(2_u32)));
        assert!(owner.claim_recovery(&next));
        owner.release_recovery_claim(&permit);
        assert!(!owner.claim_recovery(&next));
    }

    #[test]
    fn recovery_resource_is_retained_until_exact_generation_completion() {
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let key = key("recovery-reader", "8901234567890123456");
        let permit = supervisor.reserve(key.clone()).unwrap();
        let resource = std::sync::Arc::new(42_u32);
        let weak = std::sync::Arc::downgrade(&resource);
        assert!(supervisor.retain_recovery(&permit, resource.clone()));
        assert!(!supervisor.retain_recovery(&permit, std::sync::Arc::new(7_u32)));
        drop(resource);
        assert_eq!(*supervisor.recovery::<u32>(&permit).unwrap(), 42);
        assert!(weak.upgrade().is_some());
        supervisor.worker_finished(&permit);
        assert!(weak.upgrade().is_none());
        let replacement = supervisor.reserve(key).unwrap();
        assert!(!supervisor.retain_recovery(&permit, std::sync::Arc::new(9_u32)));
        assert!(supervisor.recovery::<u32>(&permit).is_none());
        assert!(supervisor.retain_recovery(&replacement, std::sync::Arc::new(10_u32)));
        supervisor.worker_finished(&permit);
        assert_eq!(*supervisor.recovery::<u32>(&replacement).unwrap(), 10);
    }

    use super::*;

    fn key(reader: &str, sim: &str) -> ReaderSessionKey {
        ReaderSessionKey::new(reader, sim).unwrap()
    }

    #[test]
    fn key_rejects_control_characters_and_unusable_card_identity() {
        for (reader, sim) in [
            ("", "8901234567890123456"),
            ("Reader\nA", "8901234567890123456"),
            ("Reader A", "1234"),
            ("Reader A", "89012345678901234x"),
        ] {
            assert!(matches!(
                ReaderSessionKey::new(reader, sim),
                Err(SessionKeyError::Invalid)
            ));
        }
    }

    #[test]
    fn duplicate_reservation_is_rejected_but_independent_reader_sim_pairs_isolate() {
        let first_key = key("Reader A", "8901234567890123456");
        let second_key = key("Reader B", "8901234567890123457");
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let first = supervisor.reserve(first_key.clone()).unwrap();
        assert!(matches!(
            supervisor.reserve(first_key),
            Err(ReserveError::Active)
        ));
        let second = supervisor.reserve(second_key.clone()).unwrap();
        assert_ne!(first.generation(), second.generation());
        assert_eq!(
            supervisor.snapshot(&second_key).unwrap().phase,
            NativeSessionPhase::Prepared
        );
    }

    #[test]
    fn only_the_matching_session_receives_stop_and_duplicate_start_is_rejected() {
        let first_key = key("Reader A", "8901234567890123456");
        let second_key = key("Reader B", "8901234567890123457");
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let first = supervisor.reserve(first_key.clone()).unwrap();
        let second = supervisor.reserve(second_key.clone()).unwrap();
        let first_stop = first.stop_signal();
        let second_stop = second.stop_signal();

        supervisor.mark_running(&first).unwrap();
        assert_eq!(
            supervisor.mark_running(&first),
            Err(StartError::NotPrepared)
        );
        assert_eq!(supervisor.request_stop(&first), StopDisposition::Signalled);
        assert!(*first_stop.borrow());
        assert!(!*second_stop.borrow());
        assert_eq!(
            supervisor.snapshot(&first_key).unwrap().phase,
            NativeSessionPhase::Stopping
        );
        assert_eq!(
            supervisor.snapshot(&second_key).unwrap().phase,
            NativeSessionPhase::Prepared
        );
    }

    #[test]
    fn stale_completion_cannot_remove_a_replacement_generation() {
        let session_key = key("Reader A", "8901234567890123456");
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let first = supervisor.reserve(session_key.clone()).unwrap();
        supervisor.mark_running(&first).unwrap();
        assert_eq!(supervisor.request_stop(&first), StopDisposition::Signalled);
        assert_eq!(
            supervisor.worker_finished(&first),
            CompletionDisposition::Removed
        );

        let replacement = supervisor.reserve(session_key.clone()).unwrap();
        supervisor.mark_running(&replacement).unwrap();
        assert_eq!(
            supervisor.worker_finished(&first),
            CompletionDisposition::StaleGeneration
        );
        assert_eq!(
            supervisor.snapshot(&session_key).unwrap(),
            NativeSessionSnapshot {
                generation: replacement.generation(),
                phase: NativeSessionPhase::Running,
            }
        );
    }

    #[test]
    fn shutdown_all_is_idempotent_and_retains_sessions_until_exact_worker_completion() {
        let first_key = key("Reader A", "8901234567890123456");
        let second_key = key("Reader B", "8901234567890123457");
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let first = supervisor.reserve(first_key.clone()).unwrap();
        let second = supervisor.reserve(second_key.clone()).unwrap();
        let first_stop = first.stop_signal();
        let second_stop = second.stop_signal();
        supervisor.mark_running(&first).unwrap();
        supervisor.mark_running(&second).unwrap();

        assert_eq!(supervisor.request_stop_all(), 2);
        assert_eq!(supervisor.request_stop_all(), 0);
        assert!(*first_stop.borrow());
        assert!(*second_stop.borrow());
        assert!(supervisor.snapshot(&first_key).is_some());
        assert_eq!(
            supervisor.worker_finished(&first),
            CompletionDisposition::Removed
        );
        assert!(supervisor.snapshot(&first_key).is_none());
        assert!(supervisor.snapshot(&second_key).is_some());
        assert_eq!(
            supervisor.worker_finished(&second),
            CompletionDisposition::Removed
        );
    }

    #[test]
    fn stop_during_preparation_prevents_late_worker_start() {
        let session_key = key("Reader A", "8901234567890123456");
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(session_key.clone()).unwrap();
        let stop = permit.stop_signal();

        assert_eq!(supervisor.request_stop(&permit), StopDisposition::Signalled);
        assert!(*stop.borrow());
        assert_eq!(
            supervisor.mark_running(&permit),
            Err(StartError::NotPrepared)
        );
        assert_eq!(
            supervisor.snapshot(&session_key).unwrap().phase,
            NativeSessionPhase::Stopping
        );
        assert_eq!(
            supervisor.worker_finished(&permit),
            CompletionDisposition::Removed
        );
    }

    #[test]
    fn generation_controls_status_and_stop_without_exposing_sim_identity() {
        let session_key = key("reader-fixture", "8901234567890123456");
        let mut supervisor = NativeVoWiFiSupervisor::default();
        let permit = supervisor.reserve(session_key).unwrap();
        let generation = SessionGeneration::from_value(permit.generation().value()).unwrap();
        let stopped = permit.stop_signal();

        assert_eq!(
            supervisor.snapshot_generation(generation),
            Some(NativeSessionSnapshot {
                generation,
                phase: NativeSessionPhase::Prepared,
            })
        );
        assert_eq!(
            supervisor.request_stop_generation(generation),
            StopDisposition::Signalled
        );
        assert!(*stopped.borrow());
        assert_eq!(
            supervisor.request_stop_generation(generation),
            StopDisposition::AlreadyStopping
        );
        assert_eq!(
            supervisor.snapshot_generation(
                SessionGeneration::from_value(generation.value() + 1).unwrap()
            ),
            None
        );
        assert_eq!(
            supervisor.worker_finished(&permit),
            CompletionDisposition::Removed
        );
        assert_eq!(supervisor.snapshot_generation(generation), None);
    }
}
