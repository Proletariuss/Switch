//! Typed native manager access to the existing durable report/retry policy.
use super::*;
#[derive(Clone)]
pub(crate) struct Observation {
    pub(crate) profile_id: String,
    pub(crate) generation: u64,
    pub(crate) reason: String,
    pub(crate) desired: bool,
    pub(crate) tunnel_connected: bool,
    pub(crate) ims_registered: bool,
    pub(crate) messaging_ready: bool,
    pub(crate) sim_sms_route_ready: bool,
}
pub(crate) struct Plan {
    pub(crate) reason: String,
    pub(crate) delay_ms: u64,
    pub(crate) failures: u32,
}
pub(crate) struct Schedule {
    pub(crate) generation: u64,
    pub(crate) due_at_ms: Option<i64>,
}
impl Journal {
    pub(crate) fn observe_native(
        &mut self,
        input: Observation,
        now: i64,
    ) -> Result<(), &'static str> {
        let event = observation_event(
            ObservationInput {
                profile_id: input.profile_id,
                generation: input.generation,
                reason: input.reason,
                desired: input.desired,
                tunnel_connected: input.tunnel_connected,
                ims_registered: input.ims_registered,
                messaging_ready: input.messaging_ready,
                sim_sms_route_ready: input.sim_sms_route_ready,
            },
            now,
        )
        .ok_or("invalid_runtime_observation")?;
        self.record_at(event, now);
        if self.storage_issue.is_empty() {
            Ok(())
        } else {
            Err("runtime_journal_unavailable")
        }
    }
    pub(crate) fn recovery_native(
        &mut self,
        profile_id: String,
        reason: String,
        now: i64,
    ) -> Result<Plan, &'static str> {
        self.plan_recovery(RecoveryPlanRequest { profile_id, reason }, now)
            .map_err(|_| "runtime_journal_unavailable")?
            .map(|p| Plan {
                reason: p.reason,
                delay_ms: p.delay_ms,
                failures: p.failures,
            })
            .ok_or("invalid_runtime_recovery")
    }
    pub(crate) fn schedule_native_retry(
        &mut self,
        profile_id: String,
        owner_id: String,
        attempt_id: String,
        delay_ms: u64,
        now: i64,
    ) -> Result<Schedule, &'static str> {
        self.schedule_retry(
            RetryScheduleRequest {
                profile_id,
                owner_id,
                attempt_id,
                delay_ms,
            },
            now,
        )
        .map_err(|_| "runtime_journal_unavailable")?
        .map(|s| Schedule {
            generation: s.generation,
            due_at_ms: s.due_at_ms,
        })
        .ok_or("invalid_runtime_retry")
    }
}
