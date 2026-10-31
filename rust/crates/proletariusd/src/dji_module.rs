//! Direct DJI module admission.
//!
//! The DJI adapter owns its own physical identity and generation. Historical
//! Switch call-mode recovery files are not an admission source for this path.
//! The shared module VoWiFi session still performs its own USB/SIM/AKA binding
//! and cleanup checks after the owner is opened.
use crate::daemon_lifetime::Lease;
use crate::module_recovery::Gate;

/// Current production mode for the migration branch.
pub(crate) const MODE: &str = "dji_native_direct";

/// Construct the module admission used by the direct DJI adapter.
///
/// The old metadata quarantine was designed for the retired Switch call-mode
/// recovery path. It cannot prove ownership of a DJI USB device and therefore
/// must not block direct device discovery. Physical ownership, IMEI/SIM
/// identity, USB composition, ADB state and cleanup are checked by the adapter
/// and the retained VoWiFi session before any module operation is admitted.
pub(crate) fn admission_gate(_lifetime: &Lease) -> Gate {
    Gate::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_dji_admission_does_not_inherit_legacy_quarantine() {
        let gate = Gate::default();
        assert!(!gate.is_quarantined());
        assert!(!gate.is_admission_revoked());
    }
}
