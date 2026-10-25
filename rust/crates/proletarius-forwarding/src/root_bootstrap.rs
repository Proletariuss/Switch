//! Fresh-root creation foundation. No production owner or migration capability
//! can be issued until every real daemon owner has been wired into the drain.
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Stage {
    ProductionGate,
    OwnerGate,
    ParentOpen,
    GuardOpen,
    GuardLock,
    GuardSync,
    GuardDirectorySync,
    Random,
    Mkdir,
    RootOpen,
    RootParentSync,
    SourceCreate,
    SourceWrite,
    SourceFileSync,
    SourceDirectorySync,
    RootCreate,
    RootWrite,
    RootFileSync,
    RootDirectorySync,
    WitnessCreate,
    WitnessWrite,
    WitnessFileSync,
    WitnessDirectorySync,
    FinalCheck,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum FailureKind {
    OwnerIntegrationUnproven,
    OwnersNotDrained,
    OwnerGateClosed,
    OwnerStateUnproven,
    InvalidPath,
    UnsafePermissions,
    Conflict,
    Busy,
    Io,
    RandomUnavailable,
    ChangedDuringCreation,
    Injected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BootstrapError {
    pub stage: Stage,
    pub kind: FailureKind,
}
impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "fresh-root bootstrap {:?}: {:?}", self.stage, self.kind)
    }
}
impl std::error::Error for BootstrapError {}
fn err(stage: Stage, kind: FailureKind) -> BootstrapError {
    BootstrapError { stage, kind }
}

/// Intentionally has no public constructor, deserializer, or fixture conversion.
/// It will require an audited daemon owner handoff in a later slice.
/// ```compile_fail
/// use proletarius_forwarding::root_bootstrap::ProductionBootstrap;
/// let _: ProductionBootstrap = serde_json::from_str("{}").unwrap();
/// ```
/// ```compile_fail
/// use proletarius_forwarding::root_bootstrap::ProductionBootstrap;
/// let _ = ProductionBootstrap { _private: () };
/// ```
pub struct ProductionBootstrap {
    _private: (),
}

/// Read-only rejection before opening files, creating guard locks or markers.
/// A caller-supplied path/JSON/bool and B3a preflight are not owner-drain proof.
pub fn bootstrap_production(_root: &Path) -> Result<ProductionBootstrap, BootstrapError> {
    Err(err(
        Stage::ProductionGate,
        FailureKind::OwnerIntegrationUnproven,
    ))
}

pub fn production_blockers() -> &'static [&'static str] {
    &[
        "all real producer/configuration/source/queue_io/provider owners must close and finish",
        "old processes and the launcher must observe a proven cross-process owner fence",
        "fresh origin and independent anti-rollback/controlled restore policy remain unproven",
        "SWF2 migration, durable progress ACK and legacy unknown frontier remain closed",
    ]
}

// The only usable creator is deliberately restricted to explicit synthetic
// builds/tests. Its receipt cannot be converted to ProductionBootstrap.
#[cfg(any(test, feature = "root-bootstrap-fault-tests"))]
pub mod fixture;
#[cfg(test)]
mod tests;
