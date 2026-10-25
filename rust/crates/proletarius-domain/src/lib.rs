#![forbid(unsafe_code)]

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationStage {
    Foundation,
    Gateway,
    ReadShadow,
    SwiftShadow,
    ReplayGate,
    SupervisorGate,
    RecoveryGate,
    VoWiFiProxyGate,
    VoWiFiSnapshotGate,
    ReaderDiscoveryGate,
    SimProtocolGate,
    IkeWireGate,
    ImsProtocolGate,
    SmsProtocolGate,
    CallProtocolGate,
    OwnershipManifestGate,
    CallArchiveCutover,
    MessageArchiveCutover,
}

impl MigrationStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Foundation => "foundation",
            Self::Gateway => "gateway",
            Self::ReadShadow => "read-shadow",
            Self::SwiftShadow => "swift-shadow",
            Self::ReplayGate => "replay-gate",
            Self::SupervisorGate => "supervisor-gate",
            Self::RecoveryGate => "recovery-gate",
            Self::VoWiFiProxyGate => "vowifi-proxy-gate",
            Self::VoWiFiSnapshotGate => "vowifi-snapshot-gate",
            Self::ReaderDiscoveryGate => "reader-discovery-gate",
            Self::SimProtocolGate => "sim-protocol-gate",
            Self::IkeWireGate => "ike-wire-gate",
            Self::ImsProtocolGate => "ims-protocol-gate",
            Self::SmsProtocolGate => "sms-protocol-gate",
            Self::CallProtocolGate => "call-protocol-gate",
            Self::OwnershipManifestGate => "ownership-manifest-gate",
            Self::CallArchiveCutover => "call-archive-cutover",
            Self::MessageArchiveCutover => "message-archive-cutover",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendFailure {
    InvalidListenAddress,
    ExistingPathIsNotSocket,
    GoBackendMissing,
    GoBackendExited(String),
    GoBackendUnavailable(String),
    ReaderAdmissionRejected { code: &'static str },
    Io(String),
}

impl fmt::Display for BackendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidListenAddress => {
                formatter.write_str("listen address must use unix:<path>")
            }
            Self::ExistingPathIsNotSocket => {
                formatter.write_str("refusing to replace a non-socket listen path")
            }
            Self::GoBackendMissing => formatter.write_str("bundled Go backend is missing"),
            Self::GoBackendExited(status) => {
                write!(formatter, "Go backend exited unexpectedly: {status}")
            }
            Self::GoBackendUnavailable(message) => {
                write!(formatter, "Go backend unavailable: {message}")
            }
            Self::ReaderAdmissionRejected { code } => {
                write!(formatter, "reader admission rejected: {code}")
            }
            Self::Io(message) => write!(formatter, "I/O failure: {message}"),
        }
    }
}

impl std::error::Error for BackendFailure {}

#[cfg(test)]
mod tests {
    use super::MigrationStage;

    #[test]
    fn migration_contract_names_are_stable() {
        assert_eq!(MigrationStage::Foundation.as_str(), "foundation");
        assert_eq!(MigrationStage::Gateway.as_str(), "gateway");
        assert_eq!(MigrationStage::ReadShadow.as_str(), "read-shadow");
        assert_eq!(MigrationStage::SwiftShadow.as_str(), "swift-shadow");
        assert_eq!(MigrationStage::ReplayGate.as_str(), "replay-gate");
        assert_eq!(MigrationStage::SupervisorGate.as_str(), "supervisor-gate");
        assert_eq!(MigrationStage::RecoveryGate.as_str(), "recovery-gate");
        assert_eq!(
            MigrationStage::VoWiFiProxyGate.as_str(),
            "vowifi-proxy-gate"
        );
        assert_eq!(
            MigrationStage::VoWiFiSnapshotGate.as_str(),
            "vowifi-snapshot-gate"
        );
        assert_eq!(
            MigrationStage::ReaderDiscoveryGate.as_str(),
            "reader-discovery-gate"
        );
        assert_eq!(
            MigrationStage::SimProtocolGate.as_str(),
            "sim-protocol-gate"
        );
        assert_eq!(MigrationStage::IkeWireGate.as_str(), "ike-wire-gate");
        assert_eq!(
            MigrationStage::ImsProtocolGate.as_str(),
            "ims-protocol-gate"
        );
        assert_eq!(
            MigrationStage::SmsProtocolGate.as_str(),
            "sms-protocol-gate"
        );
        assert_eq!(
            MigrationStage::CallProtocolGate.as_str(),
            "call-protocol-gate"
        );
        assert_eq!(
            MigrationStage::OwnershipManifestGate.as_str(),
            "ownership-manifest-gate"
        );
        assert_eq!(
            MigrationStage::CallArchiveCutover.as_str(),
            "call-archive-cutover"
        );
        assert_eq!(
            MigrationStage::MessageArchiveCutover.as_str(),
            "message-archive-cutover"
        );
    }
}
