//! Current-runtime, last-observed recovery facts, independent of event retention.
//! Projection only: no filesystem/device probes and no admission decisions.
//! Closed enums deliberately exclude paths, identities, evidence hashes and text.
use serde::{Deserialize, Serialize};

use crate::module_recovery::{Gate, Reason};

// The enclosing bundle timestamp is the export time, never a new Gate probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ObservationKind {
    LastObserved,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    #[default]
    NotObserved,
    Open,
    LegacyRecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReasonCode {
    LegacyExecutionPending,
    RestoreJournalPending,
    RestoreJournalUnreadable,
    ReconciliationPending,
    ReconciliationUnreadable,
    LegacyCallModeLock,
    LegacyAdbInterfaceLock,
    LegacyLockProvenancePending,
    ModuleBackupPresent,
    LegacyVoiceRuntime,
    ManagedVoiceControlPending,
    InvalidPristineMarker,
    UnreadableEvidence,
    LifetimeAuthorityChanged,
}
impl From<Reason> for ReasonCode {
    fn from(reason: Reason) -> Self {
        match reason {
            Reason::LegacyExecutionPending => Self::LegacyExecutionPending,
            Reason::RestoreJournalPending => Self::RestoreJournalPending,
            Reason::RestoreJournalUnreadable => Self::RestoreJournalUnreadable,
            Reason::ReconciliationPending => Self::ReconciliationPending,
            Reason::ReconciliationUnreadable => Self::ReconciliationUnreadable,
            Reason::LegacyCallModeLock => Self::LegacyCallModeLock,
            Reason::LegacyAdbInterfaceLock => Self::LegacyAdbInterfaceLock,
            Reason::LegacyLockProvenancePending => Self::LegacyLockProvenancePending,
            Reason::ModuleBackupPresent => Self::ModuleBackupPresent,
            Reason::LegacyVoiceRuntime => Self::LegacyVoiceRuntime,
            Reason::ManagedVoiceControlPending => Self::ManagedVoiceControlPending,
            Reason::InvalidPristineMarker => Self::InvalidPristineMarker,
            Reason::UnreadableEvidence => Self::UnreadableEvidence,
            Reason::LifetimeAuthorityChanged => Self::LifetimeAuthorityChanged,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Metadata {
    #[default]
    NotObserved,
    NotNeeded,
    Inspected,
    Failed,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reconciliation {
    #[default]
    NotObserved,
    NotApplicable,
    Completed,
    CompletedAfterStop,
    Failed,
    PublicationUnconfirmed,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeInspection {
    #[default]
    NotObserved,
    NotApplicable,
    Inspected,
    Failed,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cleanup {
    #[default]
    NotObserved,
    Confirmed,
    Unconfirmed,
}
/// Retained last observation of the startup-only current-baseline branch.
/// No string supplied by a transport or error can enter this closed projection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LegacyCurrent {
    #[default]
    NotObserved,
    NotApplicable,
    Started,
    CurrentVerified,
    PublicationUnconfirmed,
    #[serde(rename = "legacy_current_ineligible")]
    Ineligible,
    #[serde(rename = "legacy_current_evidence_changed")]
    EvidenceChanged,
    #[serde(rename = "legacy_current_busy")]
    Busy,
    #[serde(rename = "legacy_current_cancelled")]
    Cancelled,
    #[serde(rename = "legacy_current_timeout")]
    Timeout,
    #[serde(rename = "legacy_current_device_unconfirmed")]
    DeviceUnconfirmed,
    #[serde(rename = "legacy_current_readback_unconfirmed")]
    ReadbackUnconfirmed,
    #[serde(rename = "legacy_current_helper_unconfirmed")]
    HelperUnconfirmed,
    #[serde(rename = "legacy_current_cleanup_unconfirmed")]
    CleanupUnconfirmed,
    #[serde(rename = "legacy_current_worker_failed")]
    WorkerFailed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Startup {
    metadata: Metadata,
    reconciliation: Reconciliation,
    native_inspection: NativeInspection,
    cleanup: Cleanup,
    #[serde(default)]
    legacy_current: LegacyCurrent,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    schema_version: u8,
    observation: ObservationKind,
    state: State,
    reason_codes: Vec<ReasonCode>,
    historical_recovery_confirmed: bool,
    startup: Startup,
}
#[derive(Default)]
pub(super) struct Observation {
    gate: Option<Gate>,
    startup: Startup,
}
impl Observation {
    pub(super) fn bind(&mut self, gate: &Gate) {
        self.gate = Some(gate.clone());
    }

    fn cleanup(&mut self, result: Cleanup) {
        // A later observation must not hide an earlier unconfirmed cleanup.
        if self.startup.cleanup != Cleanup::Unconfirmed {
            self.startup.cleanup = result;
        }
    }

    pub(super) fn legacy_current(&mut self, result: LegacyCurrent) {
        if matches!(result, LegacyCurrent::CleanupUnconfirmed | LegacyCurrent::WorkerFailed) {
            self.cleanup(Cleanup::Unconfirmed);
        }
        if !matches!(self.startup.legacy_current,
            LegacyCurrent::CleanupUnconfirmed | LegacyCurrent::WorkerFailed) {
            self.startup.legacy_current = result;
        }
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        let mut reason_codes = Vec::new();
        let state = match self.gate.as_ref() {
            None => State::NotObserved,
            Some(gate) => match gate.observed_status() {
                None => State::Open,
                Some(status) => {
                    for reason in &status.reason_codes {
                        let reason = ReasonCode::from(*reason);
                        if !reason_codes.contains(&reason) {
                            reason_codes.push(reason);
                        }
                    }
                    if reason_codes.is_empty() {
                        State::NotObserved
                    } else {
                        State::LegacyRecoveryRequired
                    }
                }
            },
        };
        Snapshot {
            schema_version: 1,
            observation: ObservationKind::LastObserved,
            state,
            reason_codes,
            // A new sealed compensation is not proof of old execution.
            historical_recovery_confirmed: false,
            startup: self.startup.clone(),
        }
    }
}

fn update(change: impl FnOnce(&mut Observation)) {
    if let Some(recorder) = super::RECORDER.get() {
        let mut observation = recorder
            .shared
            .recovery
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        change(&mut observation);
    }
}
pub(crate) fn bind(gate: &Gate) {
    update(|observation| observation.bind(gate));
}
pub(crate) fn metadata(result: Metadata) {
    update(|observation| observation.startup.metadata = result);
}
pub(crate) fn reconciliation(result: Reconciliation) {
    update(|observation| observation.startup.reconciliation = result);
}
pub(crate) fn native_inspection(result: NativeInspection) {
    update(|observation| observation.startup.native_inspection = result);
}
pub(crate) fn legacy_current(result: LegacyCurrent) {
    update(|observation| observation.legacy_current(result));
}
pub(crate) fn cleanup(result: Cleanup) {
    update(|observation| observation.cleanup(result));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_projection_is_current_runtime_and_never_certifies_history() {
        let gate = Gate::quarantined_for_tests();
        let mut observation = Observation::default();
        assert_eq!(observation.snapshot().state, State::NotObserved);
        observation.bind(&gate);
        observation.startup.metadata = Metadata::Failed;
        let blocked = observation.snapshot();
        assert_eq!(blocked.state, State::LegacyRecoveryRequired);
        assert_eq!(blocked.reason_codes, vec![ReasonCode::LegacyCallModeLock]);
        assert_eq!(blocked.startup.metadata, Metadata::Failed);
        assert!(!blocked.historical_recovery_confirmed);
        assert_eq!(blocked.startup.cleanup, Cleanup::NotObserved);
        assert_eq!(Observation::default().snapshot().state, State::NotObserved);

        let gate = Gate::default();
        observation.bind(&gate);
        assert_eq!(observation.snapshot().state, State::Open);
        assert!(observation.snapshot().reason_codes.is_empty());
        gate.revoke_for_tests();
        assert_eq!(observation.snapshot().state, State::LegacyRecoveryRequired);
        assert!(!observation.snapshot().historical_recovery_confirmed);
    }

    #[test]
    fn observation_kind_is_required_closed_and_never_claims_a_fresh_probe() {
        let mut observation = Observation::default();
        let unobserved = observation.snapshot();
        observation.bind(&Gate::default());
        let open = observation.snapshot();
        observation.bind(&Gate::quarantined_for_tests());
        let blocked = observation.snapshot();
        for snapshot in [unobserved, open, blocked] {
            let value = serde_json::to_value(&snapshot).unwrap();
            assert_eq!(value["observation"], "last_observed");
            assert_eq!(
                serde_json::from_value::<Snapshot>(value.clone()).unwrap(),
                snapshot
            );
            for unsupported in [
                serde_json::json!("current"),
                serde_json::json!("freshly_verified"),
                serde_json::json!("LastObserved"),
                serde_json::json!(true),
                serde_json::json!(null),
                serde_json::json!({"last_observed": true}),
            ] {
                let mut changed = value.clone();
                changed["observation"] = unsupported;
                assert!(serde_json::from_value::<Snapshot>(changed).is_err());
            }
            let mut missing = value;
            missing.as_object_mut().unwrap().remove("observation");
            assert!(serde_json::from_value::<Snapshot>(missing).is_err());
        }
    }

    #[test]
    fn completed_compensation_never_confirms_history_or_hides_cleanup_debt() {
        let mut observation = Observation::default();
        observation.startup.reconciliation = Reconciliation::Completed;
        observation.cleanup(Cleanup::Confirmed);
        assert_eq!(observation.snapshot().startup.cleanup, Cleanup::Confirmed);
        assert!(!observation.snapshot().historical_recovery_confirmed);
        observation.cleanup(Cleanup::Unconfirmed);
        observation.cleanup(Cleanup::Confirmed);
        assert_eq!(observation.snapshot().startup.cleanup, Cleanup::Unconfirmed);
    }

    #[test]
    fn closed_snapshot_rejects_unknown_fields_reasons_and_private_text() {
        let mut observation = Observation::default();
        observation.bind(&Gate::quarantined_for_tests());
        let snapshot = observation.snapshot();
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(serde_json::from_value::<Snapshot>(value.clone()).unwrap(), snapshot);
        for field in ["path", "imei", "imsi", "iccid", "backup", "journal", "command", "token", "error"] {
            let mut changed = value.clone();
            changed.as_object_mut().unwrap().insert(field.into(), "private".into());
            assert!(serde_json::from_value::<Snapshot>(changed).is_err());
            let mut changed = value.clone();
            changed["startup"].as_object_mut().unwrap().insert(field.into(), "private".into());
            assert!(serde_json::from_value::<Snapshot>(changed).is_err());
        }
        for field in ["state", "reason_codes"] {
            let mut changed = value.clone();
            changed[field] = if field == "reason_codes" {
                serde_json::json!(["/private/AT+CGSN"])
            } else {
                "/private/AT+CGSN".into()
            };
            assert!(serde_json::from_value::<Snapshot>(changed).is_err());
        }
        for field in ["metadata", "reconciliation", "native_inspection", "cleanup"] {
            let mut changed = value.clone();
            changed["startup"][field] = "private".into();
            assert!(serde_json::from_value::<Snapshot>(changed).is_err());
        }
        assert!(serde_json::to_vec(&snapshot).unwrap().len() < 4096);
    }

    #[test]
    fn every_gate_reason_has_a_closed_bounded_projection() {
        let reasons = [
            Reason::LegacyExecutionPending, Reason::RestoreJournalPending,
            Reason::RestoreJournalUnreadable, Reason::ReconciliationPending,
            Reason::ReconciliationUnreadable, Reason::LegacyCallModeLock,
            Reason::LegacyAdbInterfaceLock, Reason::LegacyLockProvenancePending,
            Reason::ModuleBackupPresent, Reason::LegacyVoiceRuntime,
            Reason::ManagedVoiceControlPending, Reason::InvalidPristineMarker,
            Reason::UnreadableEvidence, Reason::LifetimeAuthorityChanged,
        ];
        let mut value = Observation::default().snapshot();
        value.reason_codes = reasons.into_iter().map(ReasonCode::from).collect();
        value.state = State::LegacyRecoveryRequired;
        assert_eq!(value.reason_codes.len(), 14);
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(encoded.len() < 4096);
        assert_eq!(serde_json::from_slice::<Snapshot>(&encoded).unwrap(), value);
    }

    #[test]
    fn legacy_current_is_a_closed_optional_backward_compatible_projection() {
        let values = [
            LegacyCurrent::NotObserved, LegacyCurrent::NotApplicable, LegacyCurrent::Started,
            LegacyCurrent::CurrentVerified, LegacyCurrent::PublicationUnconfirmed,
            LegacyCurrent::Ineligible, LegacyCurrent::EvidenceChanged, LegacyCurrent::Busy,
            LegacyCurrent::Cancelled, LegacyCurrent::Timeout, LegacyCurrent::DeviceUnconfirmed,
            LegacyCurrent::ReadbackUnconfirmed, LegacyCurrent::HelperUnconfirmed,
            LegacyCurrent::CleanupUnconfirmed, LegacyCurrent::WorkerFailed,
        ];
        let names = [
            "not_observed", "not_applicable", "started", "current_verified", "publication_unconfirmed",
            "legacy_current_ineligible", "legacy_current_evidence_changed", "legacy_current_busy",
            "legacy_current_cancelled", "legacy_current_timeout", "legacy_current_device_unconfirmed",
            "legacy_current_readback_unconfirmed", "legacy_current_helper_unconfirmed",
            "legacy_current_cleanup_unconfirmed", "legacy_current_worker_failed",
        ];
        for (value, name) in values.into_iter().zip(names) {
            let mut observation = Observation::default();
            observation.legacy_current(value);
            let snapshot = observation.snapshot();
            let encoded = serde_json::to_value(&snapshot).unwrap();
            assert_eq!(encoded["startup"]["legacy_current"], name);
            assert_eq!(serde_json::from_value::<Snapshot>(encoded).unwrap(), snapshot);
            assert!(!snapshot.historical_recovery_confirmed);
            assert!(serde_json::to_vec(&snapshot).unwrap().len() < 4096);
        }
        let mut old = serde_json::to_value(Observation::default().snapshot()).unwrap();
        old["startup"].as_object_mut().unwrap().remove("legacy_current");
        assert_eq!(serde_json::from_value::<Snapshot>(old.clone()).unwrap().startup.legacy_current,
            LegacyCurrent::NotObserved);
        for unsupported in [serde_json::json!("legacy_current_raw_AT+CGSN"),
            serde_json::json!("CURRENT_VERIFIED"), serde_json::json!(null),
            serde_json::json!(false), serde_json::json!({"imei":"private"}), serde_json::json!(17)] {
            let mut changed = old.clone();
            changed["startup"]["legacy_current"] = unsupported;
            assert!(serde_json::from_value::<Snapshot>(changed).is_err());
        }
    }

    #[test]
    fn current_baseline_failure_is_retained_without_any_module_event_or_probe() {
        let mut observation = Observation::default();
        observation.bind(&Gate::quarantined_for_tests());
        observation.legacy_current(LegacyCurrent::Started);
        observation.legacy_current(LegacyCurrent::HelperUnconfirmed);
        for _ in 0..8192 {
            let snapshot = observation.snapshot();
            assert_eq!(snapshot.startup.legacy_current, LegacyCurrent::HelperUnconfirmed);
            assert_eq!(snapshot.state, State::LegacyRecoveryRequired);
            assert!(!snapshot.historical_recovery_confirmed);
        }
        for sticky in [LegacyCurrent::CleanupUnconfirmed, LegacyCurrent::WorkerFailed] {
            let mut observation = Observation::default();
            observation.legacy_current(sticky);
            for later in [LegacyCurrent::NotApplicable, LegacyCurrent::CurrentVerified,
                LegacyCurrent::DeviceUnconfirmed, LegacyCurrent::Started] {
                observation.legacy_current(later);
                assert_eq!(observation.snapshot().startup.legacy_current, sticky);
                assert_eq!(observation.snapshot().startup.cleanup, Cleanup::Unconfirmed);
            }
        }
    }

}
