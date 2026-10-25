//! Public presence is a bounded read-only observation, never a card session.
//! UI polling cannot select a reader, mutate retained metadata, connect/reset a
//! card, or lend a stale scanner result to a newer selection/metadata commit.
use super::*;
use proletarius_reader::{CardPresence, ReaderCardStatus};
use std::future::Future;
use std::time::Instant;

const CACHE_TTL: Duration = Duration::from_millis(250);
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CardState {
    Present,
    Absent,
    Unknown,
    Unavailable,
}
impl CardState {
    fn text(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Sample {
    state: CardState,
    muted: Option<bool>,
    unpowered: Option<bool>,
    source: &'static str,
    error: Option<&'static str>,
    metadata_confirmed: bool,
    event_count: Option<u32>,
}
impl Sample {
    pub(super) fn unknown(source: &'static str, error: Option<&'static str>) -> Self {
        Self {
            state: CardState::Unknown,
            muted: None,
            unpowered: None,
            source,
            error,
            metadata_confirmed: false,
            event_count: None,
        }
    }
    pub(super) fn pcsc(reader: &str, statuses: &[ReaderCardStatus]) -> Self {
        let mut selected = statuses
            .iter()
            .filter(|status| status.reader.name == reader);
        let Some(status) = selected.next() else {
            return Self {
                state: CardState::Unavailable,
                ..Self::unknown("pcsc", None)
            };
        };
        if selected.next().is_some() {
            return Self::unknown("pcsc", Some("ambiguous_reader"));
        }
        Self {
            state: match status.reader.card {
                CardPresence::Present => CardState::Present,
                CardPresence::Absent => CardState::Absent,
                CardPresence::Unknown => CardState::Unknown,
                CardPresence::Unavailable => CardState::Unavailable,
            },
            muted: Some(status.reader.muted),
            unpowered: Some(status.unpowered),
            source: "pcsc",
            error: None,
            metadata_confirmed: status.reader.card == CardPresence::Present,
            event_count: Some(status.event_count),
        }
    }
    fn ble(presence: crate::reader_ble_status::ReaderPresence) -> Self {
        // The BLE status contract confirms a reader link, not a card slot.
        // A connected bridge must not manufacture a PRESENT card observation.
        Self {
            state: match presence {
                crate::reader_ble_status::ReaderPresence::Absent => CardState::Unavailable,
                _ => CardState::Unknown,
            },
            metadata_confirmed: presence == crate::reader_ble_status::ReaderPresence::Present,
            ..Self::unknown("ble-bridge", None)
        }
    }
}

struct Cached {
    selection: SelectionSnapshot,
    metadata_revision: u64,
    at: Instant,
    sample: Sample,
}
struct CardEvent {
    selection: SelectionSnapshot,
    metadata_revision: u64,
    count: u32,
}
struct Invalidated {
    selection: SelectionSnapshot,
    metadata_revision: u64,
}
#[derive(Default)]
pub(super) struct Cache {
    sample: Option<Cached>,
    invalidated: Option<Invalidated>,
    last_card_event: Option<CardEvent>,
}
impl Cache {
    fn cached(&self, selection: &SelectionSnapshot, revision: u64) -> Option<Sample> {
        self.sample
            .as_ref()
            .filter(|cached| {
                &cached.selection == selection
                    && cached.metadata_revision == revision
                    && cached.at.elapsed() < CACHE_TTL
            })
            .map(|cached| cached.sample)
    }
    fn apply(&mut self, mut snapshot: Snapshot, revision: u64, sample: Sample) -> Snapshot {
        if self
            .invalidated
            .as_ref()
            .is_some_and(|old| old.selection != snapshot.selection)
        {
            self.invalidated = None;
        }
        let card_changed = sample.event_count.is_some_and(|count| {
            self.last_card_event.as_ref().is_some_and(|old| {
                old.selection == snapshot.selection
                    && old.metadata_revision == revision
                    && old.count != count
            })
        });
        if let Some(count) = sample.event_count {
            self.last_card_event = Some(CardEvent {
                selection: snapshot.selection.clone(),
                metadata_revision: revision,
                count,
            });
        }
        if matches!(sample.state, CardState::Absent | CardState::Unavailable)
            || sample.muted == Some(true)
            || card_changed
        {
            self.invalidated = Some(Invalidated {
                selection: snapshot.selection.clone(),
                metadata_revision: revision,
            });
        }
        let latched = self.invalidated.as_ref().is_some_and(|old| {
            // Saturation cannot accidentally revalidate a previously removed
            // card. Only a successful explicit metadata commit clears a latch.
            revision <= old.metadata_revision || revision == u64::MAX
        });
        if !latched {
            self.invalidated = None;
        }
        // Unconfirmed observations hide identity for this response only. They
        // do not assert removal or destroy the known capability skeleton.
        if latched || !sample.metadata_confirmed {
            snapshot.usim_info = None;
        }
        if latched {
            // Public copy only. Retained LPA catalogs, physical capabilities,
            // metadata and workers remain owned by the lifecycle service.
            snapshot.esim_profile_name.clear();
            snapshot.esim_manager_available = false;
            snapshot.notification_aids.clear();
            snapshot.status["card_type"] = json!("unknown");
            snapshot.status["has_esim"] = json!(false);
        }
        snapshot.status["has_card"] = json!(sample.state == CardState::Present);
        snapshot.status["card_state"] = json!(sample.state.text());
        snapshot.status["card_muted"] = json!(sample.muted);
        snapshot.status["card_unpowered"] = json!(sample.unpowered);
        // OS usability only; this does not promise a successful APDU or AKA.
        snapshot.status["card_ready"] = json!(
            sample.state == CardState::Present
                && sample.muted == Some(false)
                && sample.unpowered == Some(false)
        );
        snapshot.status["presence_source"] = json!(sample.source);
        snapshot.status["presence_error"] = json!(sample.error);
        snapshot.status["metadata_ready"] = json!(snapshot.usim_info.is_some());
        snapshot.status["has_usim_info"] = json!(snapshot.usim_info.is_some());
        snapshot
    }
}

fn same_target(
    before: &Snapshot,
    after: &Snapshot,
    before_revision: u64,
    after_revision: u64,
) -> bool {
    !before.selection.pending
        && !after.selection.pending
        && before.selection == after.selection
        && before.pcsc_session == after.pcsc_session
        && before.usim_info == after.usim_info
        && before_revision == after_revision
}

impl Service {
    pub(crate) async fn observed_snapshot(&self) -> Result<Snapshot, Failure> {
        self.observed_snapshot_with(|selection| async move {
            if selection.reader.starts_with("BLE:") {
                return match crate::reader_ble_status::reader_status_facts(
                    self.0.ble_socket.as_deref(),
                    self.0.app_token.as_deref(),
                    &selection.reader,
                )
                .await
                {
                    Ok((_, presence)) => Sample::ble(presence),
                    Err(_) => Sample::unknown("ble-bridge", Some("bridge_unavailable")),
                };
            }
            match crate::reader_inventory::scan_card_status().await {
                Ok(Ok(statuses)) => Sample::pcsc(&selection.reader, &statuses),
                Ok(Err(_)) => Sample::unknown("pcsc", Some("scan_failed")),
                Err(_) => Sample::unknown("pcsc", Some("scan_task_failed")),
            }
        })
        .await
    }

    pub(super) async fn observed_snapshot_with<F, Fut>(
        &self,
        observe: F,
    ) -> Result<Snapshot, Failure>
    where
        F: FnOnce(SelectionSnapshot) -> Fut,
        Fut: Future<Output = Sample>,
    {
        self.observed_snapshot_until(observe, tokio::time::Instant::now() + OBSERVATION_TIMEOUT)
            .await
    }

    pub(super) async fn observed_snapshot_until<F, Fut>(
        &self,
        observe: F,
        deadline: tokio::time::Instant,
    ) -> Result<Snapshot, Failure>
    where
        F: FnOnce(SelectionSnapshot) -> Fut,
        Fut: Future<Output = Sample>,
    {
        // One total budget includes single-flight queueing and native scanner
        // capacity. Dropping a scan future does not release its running OS
        // worker's BlockingPool permit; actual worker completion still owns it.
        let mut cache = tokio::time::timeout_at(deadline, self.0.presence.lock())
            .await
            .map_err(|_| Failure::gate("reader_observation_timeout"))?;
        if tokio::time::Instant::now() >= deadline {
            // No cache/latch write or scanner dispatch after an expired wait.
            return Err(Failure::gate("reader_observation_timeout"));
        }
        let (before, revision) = self.snapshot_with_epoch();
        if before.selection.pending {
            return Err(Failure::gate("reader_observation_stale"));
        }
        if before.selection.reader.is_empty() {
            cache.sample = None;
            cache.invalidated = None;
            cache.last_card_event = None;
            return Ok(before);
        }
        let (sample, newly_observed) = if let Some(sample) =
            cache.cached(&before.selection, revision)
        {
            (sample, false)
        } else {
            let sample =
                match tokio::time::timeout_at(deadline, observe(before.selection.clone())).await {
                    Ok(sample) => sample,
                    Err(_) => {
                        if before.selection.reader.starts_with("BLE:") {
                            Sample::unknown("ble-bridge", Some("bridge_timeout"))
                        } else {
                            Sample::unknown("pcsc", Some("scan_timeout"))
                        }
                    }
                };
            (sample, true)
        };
        let (after, after_revision) = self.snapshot_with_epoch();
        if !same_target(&before, &after, revision, after_revision) {
            return Err(Failure::gate("reader_observation_stale"));
        }
        if newly_observed {
            cache.sample = Some(Cached {
                selection: after.selection.clone(),
                metadata_revision: revision,
                at: Instant::now(),
                sample,
            });
        }
        Ok(cache.apply(after, revision, sample))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_reader::ReaderObservation;

    fn snapshot() -> Snapshot {
        Snapshot::from_facts(
            SelectionSnapshot {
                reader: "Reader".into(),
                generation: 7,
                ..Default::default()
            },
            Some(NativeReaderFacts {
                metadata: Some(json!({"iccid":"8986000000000000000"})),
                has_esim: true,
                ..Default::default()
            }),
            None,
        )
    }
    fn sample(card: CardPresence, muted: bool, unpowered: bool) -> Sample {
        Sample::pcsc(
            "Reader",
            &[ReaderCardStatus {
                reader: ReaderObservation {
                    name: "Reader".into(),
                    card,
                    in_use: false,
                    muted,
                    atr: vec![],
                },
                unpowered,
                event_count: 0,
            }],
        )
    }
    #[test]
    fn logical_selection_and_metadata_never_manufacture_presence() {
        let snapshot = snapshot();
        assert_eq!(snapshot.status["connected"], true);
        assert_eq!(snapshot.status["has_card"], false);
        assert_eq!(snapshot.status["card_state"], "unknown");
        assert_eq!(snapshot.status["metadata_ready"], true);
    }
    #[test]
    fn empty_reader_hides_public_identity_but_preserves_private_facts() {
        let private = snapshot();
        let public = Cache::default().apply(
            private.clone(),
            1,
            sample(CardPresence::Absent, false, false),
        );
        assert_eq!(public.status["connected"], true);
        assert_eq!(public.status["has_card"], false);
        assert_eq!(public.status["card_state"], "absent");
        assert!(public.usim_info.is_none());
        assert_eq!(public.status["metadata_ready"], false);
        assert!(private.usim_info.is_some());
        assert!(private.esim_manager_available);
    }
    #[test]
    fn present_card_does_not_require_sim_metadata() {
        let mut snapshot = snapshot();
        snapshot.usim_info = None;
        let public =
            Cache::default().apply(snapshot, 1, sample(CardPresence::Present, false, false));
        assert_eq!(public.status["has_card"], true);
        assert_eq!(public.status["metadata_ready"], false);
        assert_eq!(public.status["card_ready"], true);
    }
    #[test]
    fn mute_and_unpowered_are_physical_presence_without_usability() {
        for (muted, unpowered) in [(true, false), (false, true), (true, true)] {
            let public = Cache::default().apply(
                snapshot(),
                1,
                sample(CardPresence::Present, muted, unpowered),
            );
            assert_eq!(public.status["has_card"], true);
            assert_eq!(public.status["card_ready"], false);
            assert_eq!(public.status["card_muted"], muted);
            assert_eq!(public.status["card_unpowered"], unpowered);
            if muted {
                assert!(public.usim_info.is_none());
            }
        }
    }
    #[test]
    fn remove_reinsert_same_slot_does_not_resurrect_old_metadata() {
        let mut cache = Cache::default();
        let old = snapshot();
        cache.apply(old.clone(), 4, sample(CardPresence::Absent, false, false));
        let reinserted = cache.apply(old.clone(), 4, sample(CardPresence::Present, false, false));
        assert_eq!(reinserted.status["has_card"], true);
        assert!(reinserted.usim_info.is_none());
        let refreshed = cache.apply(old, 5, sample(CardPresence::Present, false, false));
        assert!(refreshed.usim_info.is_some());
        assert_eq!(refreshed.status["metadata_ready"], true);
    }
    #[test]
    fn scan_error_is_unknown_and_never_reuses_present_or_erases_private_facts() {
        let mut cache = Cache::default();
        let original = snapshot();
        cache.apply(
            original.clone(),
            1,
            sample(CardPresence::Present, false, false),
        );
        let unknown = cache.apply(
            original.clone(),
            1,
            Sample::unknown("pcsc", Some("scan_failed")),
        );
        assert_eq!(unknown.status["has_card"], false);
        assert_eq!(unknown.status["card_state"], "unknown");
        assert!(unknown.status["card_unpowered"].is_null());
        assert!(unknown.usim_info.is_none());
        assert!(original.usim_info.is_some());
        assert!(unknown.esim_manager_available);
        let recovered = cache.apply(
            original.clone(),
            1,
            sample(CardPresence::Present, false, false),
        );
        assert_eq!(recovered.usim_info, original.usim_info);
        cache.apply(
            original.clone(),
            1,
            sample(CardPresence::Absent, false, false),
        );
        assert!(
            cache
                .apply(original, 1, Sample::unknown("pcsc", Some("scan_failed")))
                .usim_info
                .is_none()
        );
    }
    #[test]
    fn stale_selection_or_same_generation_refresh_rejects_sample() {
        let before = snapshot();
        let mut next = before.clone();
        next.selection.generation += 1;
        assert!(!same_target(&before, &next, 1, 1));
        assert!(!same_target(&before, &before, 1, 2));
        next = before.clone();
        next.usim_info = Some(json!({"iccid":"8986000000000000001"}));
        assert!(!same_target(&before, &next, 1, 1));
        next = before.clone();
        next.selection.pending = true;
        assert!(!same_target(&before, &next, 1, 1));
    }
    #[test]
    fn unavailable_invalidates_while_unknown_does_not_and_ble_link_is_not_card_presence() {
        let mut cache = Cache::default();
        let unavailable = cache.apply(snapshot(), 1, Sample::pcsc("Reader", &[]));
        assert_eq!(unavailable.status["card_state"], "unavailable");
        assert!(unavailable.usim_info.is_none());
        let ble = Cache::default().apply(
            snapshot(),
            1,
            Sample::ble(crate::reader_ble_status::ReaderPresence::Present),
        );
        assert_eq!(ble.status["card_state"], "unknown");
        assert_eq!(ble.status["has_card"], false);
        assert!(ble.usim_info.is_some());
        assert!(ble.status["card_muted"].is_null());
    }
    #[test]
    fn same_slot_present_samples_with_changed_event_count_cannot_reuse_identity() {
        let mut cache = Cache::default();
        let original = snapshot();
        let mut present = sample(CardPresence::Present, false, false);
        present.event_count = Some(4);
        assert!(
            cache
                .apply(original.clone(), 9, present)
                .usim_info
                .is_some()
        );
        // Removal/insertion occurred entirely between polls; card state and
        // ATR could be identical. The OS event counter is not identity proof.
        present.event_count = Some(6);
        assert!(
            cache
                .apply(original.clone(), 9, present)
                .usim_info
                .is_none()
        );
        assert!(
            cache
                .apply(original.clone(), 9, present)
                .usim_info
                .is_none()
        );
        assert!(cache.apply(original, 10, present).usim_info.is_some());
    }
    #[test]
    fn mute_latches_identity_until_new_facts_and_unknown_does_not_revalidate_it() {
        let mut cache = Cache::default();
        let original = snapshot();
        let muted = cache.apply(
            original.clone(),
            2,
            sample(CardPresence::Present, true, false),
        );
        assert!(muted.usim_info.is_none());
        let unmuted = cache.apply(
            original.clone(),
            2,
            sample(CardPresence::Present, false, false),
        );
        assert!(unmuted.usim_info.is_none());
        let refreshed = cache.apply(original, 3, sample(CardPresence::Present, false, false));
        assert!(refreshed.usim_info.is_some());
    }
    #[test]
    fn suppressed_identity_cannot_feed_phone_notes_or_carrier_projection() {
        let public =
            Cache::default().apply(snapshot(), 1, sample(CardPresence::Absent, false, false));
        let response =
            crate::reader_info_projection::project_native(public, &std::env::temp_dir()).unwrap();
        assert!(response.get("usim_info").is_none());
        assert!(response.get("epdg_fqdn").is_none());
        assert!(response.get("carrier_config_error").is_none());
    }
    #[test]
    fn ble_unknown_is_transient_but_known_disconnect_requires_new_facts() {
        use crate::reader_ble_status::ReaderPresence;
        let original = snapshot();
        let mut cache = Cache::default();
        let transitioning = cache.apply(
            original.clone(),
            3,
            Sample::ble(ReaderPresence::Indeterminate),
        );
        assert!(transitioning.usim_info.is_none());
        assert!(transitioning.esim_manager_available);
        let connected = cache.apply(original.clone(), 3, Sample::ble(ReaderPresence::Present));
        assert_eq!(connected.usim_info, original.usim_info);
        cache.apply(original.clone(), 3, Sample::ble(ReaderPresence::Absent));
        assert!(
            cache
                .apply(original.clone(), 3, Sample::ble(ReaderPresence::Present))
                .usim_info
                .is_none()
        );
        assert!(
            cache
                .apply(original, 4, Sample::ble(ReaderPresence::Present))
                .usim_info
                .is_some()
        );
    }
    #[test]
    fn cached_samples_are_revision_fenced_and_expire() {
        let original = snapshot();
        let mut cache = Cache::default();
        cache.sample = Some(Cached {
            selection: original.selection.clone(),
            metadata_revision: 3,
            at: Instant::now(),
            sample: sample(CardPresence::Present, false, false),
        });
        assert!(cache.cached(&original.selection, 3).is_some());
        assert!(cache.cached(&original.selection, 4).is_none());
        cache.sample.as_mut().unwrap().at = Instant::now() - CACHE_TTL;
        assert!(cache.cached(&original.selection, 3).is_none());
    }

    fn gate_facts(iccid: &str) -> NativeReaderFacts {
        NativeReaderFacts {
            metadata: Some(json!({"iccid":iccid,"imsi":"454030000000000","mcc":"454","mnc":"03"})),
            ..Default::default()
        }
    }
    fn gate_fixture() -> (
        Arc<Gate>,
        LifecycleBinding,
        crate::reader_reservation::NativeTransition,
    ) {
        let gate = Arc::new(Gate::default());
        gate.initialize_native_reader_producer("native");
        gate.initialize_business_producer("go");
        gate.initialize_native_lifecycle("native").unwrap();
        let physical = LifecycleBinding {
            producer: "native".into(),
            reader: "Reader".into(),
            session_id: "session".into(),
            session_sequence: 1,
        };
        let transition = gate
            .prepare_native_transition(
                "native",
                &SelectionSnapshot::default(),
                Kind::Connect,
                "Reader",
                NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(
            &transition,
            Some(&physical),
            gate_facts("8986000000000000000"),
        )
        .unwrap();
        (gate, physical, transition)
    }
    fn gate_snapshot(gate: &Gate) -> (Snapshot, u64) {
        let (selection, facts, session, epoch) = gate.native_reader_snapshot_with_epoch();
        (Snapshot::from_facts(selection, facts, session), epoch)
    }
    #[test]
    fn phone_enrichment_and_exact_receipt_replay_cannot_unlock_presence_latch() {
        let (gate, physical, receipt) = gate_fixture();
        let (before, epoch) = gate_snapshot(&gate);
        let mut cache = Cache::default();
        cache.apply(
            before.clone(),
            epoch,
            sample(CardPresence::Absent, false, false),
        );
        assert!(gate.native_phone_update("Reader", before.selection.generation, "+85290000001"));
        assert!(gate.native_registered_phone_update(
            "Reader",
            before.selection.generation,
            "8986000000000000000",
            "454030000000000",
            "+85290000002"
        ));
        let (enriched, after_phone) = gate_snapshot(&gate);
        assert_eq!(after_phone, epoch);
        assert!(
            cache
                .apply(
                    enriched,
                    after_phone,
                    sample(CardPresence::Present, false, false)
                )
                .usim_info
                .is_none()
        );
        // Replaying a previously successful commit is not another physical read.
        assert_eq!(
            gate.commit_native_transition(
                &receipt,
                Some(&physical),
                gate_facts("8986000000000000000")
            )
            .unwrap(),
            before.selection
        );
        let (replayed, after_replay) = gate_snapshot(&gate);
        assert_eq!(after_replay, epoch);
        assert!(
            cache
                .apply(
                    replayed,
                    after_replay,
                    sample(CardPresence::Present, false, false)
                )
                .usim_info
                .is_none()
        );
        let refresh = gate
            .prepare_native_transition(
                "native",
                &before.selection,
                Kind::Identity,
                "Reader",
                NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(&refresh, Some(&physical), gate_facts("8986000000000000000"))
            .unwrap();
        let (fresh, after_read) = gate_snapshot(&gate);
        assert_eq!(fresh.selection, before.selection);
        assert!(after_read > epoch);
        assert!(
            cache
                .apply(
                    fresh,
                    after_read,
                    sample(CardPresence::Present, false, false)
                )
                .usim_info
                .is_some()
        );
    }
    #[test]
    fn multistandby_clear_is_not_read_proof_but_fresh_publication_unlocks_latch() {
        let (gate, _, _) = gate_fixture();
        let (before, epoch) = gate_snapshot(&gate);
        let mut cache = Cache::default();
        cache.apply(
            before.clone(),
            epoch,
            sample(CardPresence::Absent, false, false),
        );
        gate.test_business_command(json!({"action":"acquire","producer_id":"go",
            "token":"business-token-0000000000000000000000000000","sequence":1,
            "role":"multistandby","reader":"Reader","reader_generation":before.selection.generation}),false).unwrap();
        let binding = json!({"producer_id":"go","operation_token":"operation-token-000000000000000000000000000",
            "operation_sequence":1,"reader":"Reader","reader_generation":before.selection.generation,
            "purpose":"card","authority":{"kind":"business","token":"business-token-0000000000000000000000000000","sequence":1,"role":"multistandby"}});
        gate.test_operation_command("acquire", binding.clone())
            .unwrap();
        let transaction: CardTransaction =
            serde_json::from_value(json!({"kind":"operation","binding":binding})).unwrap();
        let worker = gate.track_card_io(&transaction, "go", "Reader").unwrap();
        // workflow::enable publishes None before ENABLE can affect the card.
        gate.publish_multistandby_metadata(
            &transaction,
            "Reader",
            before.selection.generation,
            NativeReaderFacts::default(),
        )
        .unwrap();
        let (cleared, after_clear) = gate_snapshot(&gate);
        assert_eq!(after_clear, epoch);
        assert!(
            cache
                .apply(
                    cleared,
                    after_clear,
                    sample(CardPresence::Present, false, false)
                )
                .usim_info
                .is_none()
        );
        // A fresh identity read is published by workflow::publish afterwards.
        gate.publish_multistandby_metadata(
            &transaction,
            "Reader",
            before.selection.generation,
            gate_facts("8986000000000000001"),
        )
        .unwrap();
        let (fresh, after_read) = gate_snapshot(&gate);
        assert_eq!(fresh.selection, before.selection);
        assert!(after_read > epoch);
        let public = cache.apply(
            fresh,
            after_read,
            sample(CardPresence::Present, false, false),
        );
        assert_eq!(public.usim_info.unwrap()["iccid"], "8986000000000000001");
        drop(worker);
    }
    #[test]
    fn unpowered_alone_preserves_confirmed_identity_and_is_not_replacement_proof() {
        let original = snapshot();
        let mut cache = Cache::default();
        let unpowered = cache.apply(
            original.clone(),
            3,
            sample(CardPresence::Present, false, true),
        );
        assert_eq!(unpowered.status["has_card"], true);
        assert_eq!(unpowered.status["card_unpowered"], true);
        assert_eq!(unpowered.status["card_ready"], false);
        assert_eq!(unpowered.usim_info, original.usim_info);
        let powered = cache.apply(
            original.clone(),
            3,
            sample(CardPresence::Present, false, false),
        );
        assert_eq!(powered.usim_info, original.usim_info);
        assert_eq!(powered.status["card_ready"], true);
    }
    #[test]
    fn saturated_physical_epoch_never_revalidates_a_latched_identity() {
        let mut cache = Cache::default();
        cache.apply(
            snapshot(),
            u64::MAX,
            sample(CardPresence::Absent, false, false),
        );
        assert!(
            cache
                .apply(
                    snapshot(),
                    u64::MAX,
                    sample(CardPresence::Present, false, false)
                )
                .usim_info
                .is_none()
        );
    }
}
