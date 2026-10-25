//! Rust owns BLE hot-unplug polling, reader-generation fencing, presence
//! observation and absence confirmation. Native lifecycle owns physical cleanup.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const MAX_REQUIRED_MISSES: u32 = 64;
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_secs(1);
const REQUIRED_MISSES: u32 = 2;
#[derive(Default)]
pub(crate) struct Owner(Mutex<State>);

#[derive(Default)]
struct State {
    monitor_token: Option<String>,
    reader: Option<String>,
    generation: u64,
    last_observation_id: u64,
    misses: u32,
}

#[derive(Clone, Copy)]
enum Presence {
    Present,
    Absent,
    Indeterminate,
}

struct ObserveRequest {
    monitor_token: String,
    reader: String,
    generation: u64,
    observation_id: u64,
    presence: Presence,
    required_misses: u32,
    native_lease_active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Decision {
    accepted: bool,
    confirmed: bool,
    confirmation_count: u32,
}

impl Owner {
    fn open(&self) -> Result<String, &'static str> {
        let token = crate::multistandby_store::random_token().map_err(|_| "entropy_unavailable")?;
        let mut state = self.0.lock().map_err(|_| "reader_hotplug_state_failed")?;
        state.monitor_token = Some(token.clone());
        state.reader = None;
        state.generation = 0;
        state.last_observation_id = 0;
        state.misses = 0;
        Ok(token)
    }

    fn observe(&self, request: ObserveRequest) -> Decision {
        let Ok(mut state) = self.0.lock() else {
            return rejected();
        };
        if request.reader.trim().is_empty()
            || request.reader.len() > 256
            || request.generation == 0
            || request.observation_id == 0
            || request.required_misses == 0
            || request.required_misses > MAX_REQUIRED_MISSES
            || state.monitor_token.as_deref() != Some(request.monitor_token.as_str())
        {
            return rejected();
        }

        if request.generation < state.generation
            || (request.generation == state.generation
                && state
                    .reader
                    .as_deref()
                    .is_some_and(|reader| reader != request.reader))
            || request.observation_id <= state.last_observation_id
        {
            // A stale generation or duplicate/out-of-order sample is never
            // evidence. In particular, a replayed confirmation cannot cause
            // the lifecycle to execute teardown a second time.
            return rejected();
        }

        if request.generation > state.generation
            || state.reader.as_deref() != Some(request.reader.as_str())
        {
            state.reader = Some(request.reader.clone());
            state.generation = request.generation;
            state.misses = 0;
        }
        state.last_observation_id = request.observation_id;

        match request.presence {
            Presence::Present | Presence::Indeterminate => {
                state.misses = 0;
                Decision {
                    accepted: true,
                    confirmed: false,
                    confirmation_count: 0,
                }
            }
            Presence::Absent if request.native_lease_active => {
                // A native reader-session lease makes removal indeterminate;
                // release must be observed before absence can accrue again.
                state.misses = 0;
                Decision {
                    accepted: true,
                    confirmed: false,
                    confirmation_count: 0,
                }
            }
            Presence::Absent => {
                state.misses = state.misses.saturating_add(1).min(request.required_misses);
                let count = state.misses;
                let confirmed = count >= request.required_misses;
                if confirmed {
                    // The next unplug attempt requires a fresh pair of
                    // observations if guarded teardown cannot complete.
                    state.misses = 0;
                }
                Decision {
                    accepted: true,
                    confirmed,
                    confirmation_count: count,
                }
            }
        }
    }
}

fn rejected() -> Decision {
    Decision {
        accepted: false,
        confirmed: false,
        confirmation_count: 0,
    }
}

pub(crate) fn global() -> &'static Owner {
    static OWNER: OnceLock<Owner> = OnceLock::new();
    OWNER.get_or_init(Owner::default)
}

/// Rust owns the BLE hot-unplug cadence, directly samples the Swift
/// CoreBluetooth system bridge, and hands confirmed loss to the native owner.
pub(crate) async fn run(state: crate::ServerState) {
    let mut ticker =
        tokio::time::interval_at(tokio::time::Instant::now() + POLL_INTERVAL, POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut binding: Option<(String, u64, String, u64)> = None;
    loop {
        ticker.tick().await;
        let Some((reader, generation)) = state.reader_gate.selected_reader_generation() else {
            binding = None;
            continue;
        };
        if !crate::reader_ble_apdu::valid_ble_reader(&reader) {
            binding = None;
            continue;
        }
        if binding
            .as_ref()
            .is_none_or(|(current, current_generation, _, _)| {
                current != &reader || *current_generation != generation
            })
        {
            let Ok(token) = global().open() else {
                binding = None;
                continue;
            };
            binding = Some((reader.clone(), generation, token, 0));
        }
        let Some((bound_reader, bound_generation, token, observation_id)) = binding.as_mut() else {
            continue;
        };
        if bound_reader != &reader || *bound_generation != generation {
            binding = None;
            continue;
        }
        // A live native lease or any submitted reader work makes the sample
        // indeterminate. Do not probe Swift's serial BLE executor while a
        // card operation can be holding it.
        let native_lease_active = state.reader_gate.native_reader_session_active();
        let presence = if state.reader_gate.reader_work_active(&reader) {
            Presence::Indeterminate
        } else {
            let socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from);
            match tokio::time::timeout(
                POLL_TIMEOUT,
                crate::reader_ble_status::reader_status_facts(
                    socket.as_deref(),
                    state.app_token.as_deref(),
                    &reader,
                ),
            )
            .await
            {
                Ok(Ok((_, crate::reader_ble_status::ReaderPresence::Present))) => Presence::Present,
                Ok(Ok((_, crate::reader_ble_status::ReaderPresence::Absent))) => Presence::Absent,
                Ok(Ok((_, crate::reader_ble_status::ReaderPresence::Indeterminate)))
                | Ok(Err(_))
                | Err(_) => Presence::Indeterminate,
            }
        };
        // A delayed sample from a retired selection cannot affect the new
        // reader generation.
        if state.reader_gate.selected_reader_generation().as_ref()
            != Some(&(reader.clone(), generation))
        {
            binding = None;
            continue;
        }
        let Some(next_observation_id) = observation_id.checked_add(1) else {
            binding = None;
            continue;
        };
        *observation_id = next_observation_id;
        let decision = global().observe(ObserveRequest {
            monitor_token: token.clone(),
            reader: reader.clone(),
            generation,
            observation_id: *observation_id,
            presence,
            required_misses: REQUIRED_MISSES,
            native_lease_active,
        });
        if !decision.accepted {
            binding = None;
            continue;
        }
        if !decision.confirmed || decision.confirmation_count < REQUIRED_MISSES {
            continue;
        }
        let Ok(service) = state.reader_lifecycle() else {
            continue;
        };
        // Service retains its transition/physical worker if this monitor is
        // cancelled. A stale generation cannot tear down the new selection.
        if !matches!(
            service.confirmed_loss(&reader, generation, None).await,
            Ok(true)
        ) {
            continue;
        }
        binding = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{Decision, ObserveRequest, Owner, Presence};

    fn request(
        token: &str,
        reader: &str,
        generation: u64,
        id: u64,
        presence: Presence,
    ) -> ObserveRequest {
        ObserveRequest {
            monitor_token: token.to_owned(),
            reader: reader.to_owned(),
            generation,
            observation_id: id,
            presence,
            required_misses: 2,
            native_lease_active: false,
        }
    }

    #[test]
    fn confirms_only_after_two_distinct_absent_samples_and_resets_after_confirmation() {
        let owner = Owner::default();
        let token = owner.open().unwrap();
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-a", 4, 1, Presence::Absent)),
            Decision {
                accepted: true,
                confirmed: false,
                confirmation_count: 1
            }
        );
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-a", 4, 2, Presence::Absent)),
            Decision {
                accepted: true,
                confirmed: true,
                confirmation_count: 2
            }
        );
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-a", 4, 3, Presence::Absent)),
            Decision {
                accepted: true,
                confirmed: false,
                confirmation_count: 1
            }
        );
    }

    #[test]
    fn duplicate_and_old_generation_observations_never_accumulate() {
        let owner = Owner::default();
        let token = owner.open().unwrap();
        owner.observe(request(&token, "BLE:reader-a", 9, 1, Presence::Absent));
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-a", 9, 1, Presence::Absent)),
            Decision {
                accepted: false,
                confirmed: false,
                confirmation_count: 0
            }
        );
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-a", 8, 2, Presence::Absent)),
            Decision {
                accepted: false,
                confirmed: false,
                confirmation_count: 0
            }
        );
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-a", 9, 2, Presence::Absent)),
            Decision {
                accepted: true,
                confirmed: true,
                confirmation_count: 2
            }
        );
    }

    #[test]
    fn identity_generation_and_observation_order_fence_prior_tracker_state() {
        let owner = Owner::default();
        let token = owner.open().unwrap();
        owner.observe(request(&token, "BLE:reader-a", 2, 10, Presence::Absent));
        assert_eq!(
            owner
                .observe(request(&token, "BLE:reader-b", 2, 11, Presence::Absent))
                .accepted,
            false
        );
        assert_eq!(
            owner
                .observe(request(&token, "BLE:reader-a", 3, 9, Presence::Absent))
                .accepted,
            false
        );
        assert_eq!(
            owner.observe(request(&token, "BLE:reader-b", 3, 11, Presence::Absent)),
            Decision {
                accepted: true,
                confirmed: false,
                confirmation_count: 1
            }
        );
    }

    #[test]
    fn presence_indeterminate_and_native_lease_reset_absence_count() {
        let owner = Owner::default();
        let token = owner.open().unwrap();
        owner.observe(request(&token, "BLE:reader-a", 4, 1, Presence::Absent));
        assert_eq!(
            owner
                .observe(request(
                    &token,
                    "BLE:reader-a",
                    4,
                    2,
                    Presence::Indeterminate
                ))
                .confirmation_count,
            0
        );
        assert_eq!(
            owner
                .observe(request(&token, "BLE:reader-a", 4, 3, Presence::Absent))
                .confirmation_count,
            1
        );
        let mut leased = request(&token, "BLE:reader-a", 4, 4, Presence::Absent);
        leased.native_lease_active = true;
        assert_eq!(owner.observe(leased).confirmation_count, 0);
        assert_eq!(
            owner
                .observe(request(&token, "BLE:reader-a", 4, 5, Presence::Absent))
                .confirmation_count,
            1
        );
        assert_eq!(
            owner
                .observe(request(&token, "BLE:reader-a", 4, 6, Presence::Present))
                .confirmation_count,
            0
        );
        assert_eq!(
            owner
                .observe(request(&token, "BLE:reader-a", 4, 7, Presence::Absent))
                .confirmation_count,
            1
        );
    }

    #[test]
    fn superseded_monitor_token_is_rejected_and_threshold_is_bounded() {
        let owner = Owner::default();
        let old = owner.open().unwrap();
        let current = owner.open().unwrap();
        assert_eq!(
            owner
                .observe(request(&old, "BLE:reader-a", 1, 1, Presence::Absent))
                .accepted,
            false
        );
        let mut invalid = request(&current, "BLE:reader-a", 1, 1, Presence::Absent);
        invalid.required_misses = 65;
        assert_eq!(owner.observe(invalid).accepted, false);
    }
}
