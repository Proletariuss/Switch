//! Pure, deliberately unreachable G8 daemon acceptance scaffold.
//!
//! This module contains no file, socket, process, environment, Keychain,
//! hardware, provider, or network access.  Shipping `main` does not call it;
//! acceptance candidates continue to fail closed through `acceptance_capsule`.
//! The types here make the reviewed lexical, DTO, outcome and checked-report
//! contracts executable without granting runtime admission.

use serde::Serialize;
use std::{collections::BTreeMap, ffi::OsString, os::unix::ffi::OsStrExt, path::PathBuf};

const LONG_PREFIX: &[u8] = b"--switch-acceptance";
const SHORT_PREFIX: &[u8] = b"-switch-acceptance";
const CAPSULE_PREFIX: &[u8] = b"--switch-acceptance-capsule=";
const MAX_FAULTS: usize = 16;
const MAX_RESOURCES: usize = 8;
const MAX_CLEANUP_STEPS: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EntryReject {
    InvalidShape,
    InvalidSocketPath,
    InvalidCapsulePath,
}

pub(crate) enum LexicalEntry {
    Production,
    Candidate(AcceptanceArgShape),
    Reject(EntryReject),
}

/// Fields intentionally stay private.  This value is not an admission token.
pub(crate) struct AcceptanceArgShape {
    socket_path: PathBuf,
    capsule_path: PathBuf,
}

impl AcceptanceArgShape {
    pub(crate) fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    pub(crate) fn capsule_path(&self) -> &std::path::Path {
        &self.capsule_path
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScaffoldShippingDecision {
    Production,
    Refuse78,
}

/// The scaffold cannot select a runtime.  A later independently reviewed
/// wiring patch must replace this fail-closed decision.
pub(crate) fn shipping_decision(entry: &LexicalEntry) -> ScaffoldShippingDecision {
    match entry {
        LexicalEntry::Production => ScaffoldShippingDecision::Production,
        LexicalEntry::Candidate(_) | LexicalEntry::Reject(_) => ScaffoldShippingDecision::Refuse78,
    }
}

pub(crate) fn classify_raw(arguments: &[OsString]) -> LexicalEntry {
    let acceptance_like = arguments.iter().any(|argument| {
        let bytes = argument.as_os_str().as_bytes();
        bytes.starts_with(LONG_PREFIX) || bytes.starts_with(SHORT_PREFIX)
    });
    if !acceptance_like {
        return LexicalEntry::Production;
    }
    if arguments.len() != 4 {
        return LexicalEntry::Reject(EntryReject::InvalidShape);
    }
    let raw: Vec<&[u8]> = arguments
        .iter()
        .map(|argument| argument.as_os_str().as_bytes())
        .collect();
    if raw[0] != b"-listen" || raw[2] != b"-backend=at" {
        return LexicalEntry::Reject(EntryReject::InvalidShape);
    }
    let Some(socket) = raw[1].strip_prefix(b"unix:") else {
        return LexicalEntry::Reject(EntryReject::InvalidShape);
    };
    let socket = match canonical_utf8_path(socket) {
        Some(path) if path.ends_with("/switch.sock") => path,
        _ => return LexicalEntry::Reject(EntryReject::InvalidSocketPath),
    };
    let Some(data_root) = socket.strip_suffix("/switch.sock") else {
        return LexicalEntry::Reject(EntryReject::InvalidSocketPath);
    };
    if !canonical_path(data_root) {
        return LexicalEntry::Reject(EntryReject::InvalidSocketPath);
    }
    let Some(capsule) = raw[3].strip_prefix(CAPSULE_PREFIX) else {
        return LexicalEntry::Reject(EntryReject::InvalidShape);
    };
    let Some(capsule) = canonical_utf8_path(capsule) else {
        return LexicalEntry::Reject(EntryReject::InvalidCapsulePath);
    };
    LexicalEntry::Candidate(AcceptanceArgShape {
        socket_path: PathBuf::from(socket),
        capsule_path: PathBuf::from(capsule),
    })
}

fn canonical_utf8_path(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|path| canonical_path(path))
}

fn canonical_path(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && !path.ends_with('/')
        && !path.contains('\0')
        && !path.contains("//")
        && !path.split('/').any(|part| part == "." || part == "..")
}

pub(crate) struct AcceptanceBuildMetadata {
    version: String,
    app_release_version: String,
    build_number: String,
    source_digest: String,
}

impl AcceptanceBuildMetadata {
    pub(crate) fn validated(
        version: String,
        app_release_version: String,
        build_number: String,
        source_digest: String,
    ) -> Result<Self, DtoFault> {
        if version.is_empty()
            || app_release_version.is_empty()
            || build_number.is_empty()
            || !lower_hex(&source_digest, 64)
        {
            return Err(DtoFault::InvalidBuildMetadata);
        }
        Ok(Self {
            version,
            app_release_version,
            build_number,
            source_digest,
        })
    }
}

pub(crate) struct AcceptanceRunIdentity {
    run_id: String,
    daemon_pid: u32,
    app_pid: u32,
}

impl AcceptanceRunIdentity {
    pub(crate) fn validated(
        run_id: String,
        daemon_pid: u32,
        app_pid: u32,
    ) -> Result<Self, DtoFault> {
        if !lower_hex(&run_id, 32) || daemon_pid == 0 || app_pid == 0 {
            return Err(DtoFault::InvalidRunIdentity);
        }
        Ok(Self {
            run_id,
            daemon_pid,
            app_pid,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DtoFault {
    InvalidBuildMetadata,
    InvalidRunIdentity,
    Encode,
}

#[derive(Serialize)]
struct AcceptanceRuntimeDto<'a> {
    schema_version: u8,
    ok: bool,
    mode: &'static str,
    backend: &'static str,
    version: &'a str,
    app_release_version: &'a str,
    build_number: &'a str,
    source_digest: &'a str,
    run_id: &'a str,
    daemon_pid: u32,
    app_pid: u32,
    hardware_owner: &'static str,
    hardware_ownership: bool,
    hardware_ready: bool,
    stores_opened: bool,
    providers_started: bool,
    external_network_enabled: bool,
}

#[derive(Serialize)]
struct AcceptanceHealthDto<'a> {
    schema_version: u8,
    ok: bool,
    mode: &'static str,
    run_id: &'a str,
    daemon_pid: u32,
    app_pid: u32,
    service_ready: bool,
    hardware_owner: &'static str,
    hardware_ownership: bool,
    hardware_ready: bool,
    stores_opened: bool,
    providers_started: bool,
    external_network_enabled: bool,
}

pub(crate) fn runtime_json(
    build: &AcceptanceBuildMetadata,
    run: &AcceptanceRunIdentity,
) -> Result<Vec<u8>, DtoFault> {
    serde_json::to_vec(&AcceptanceRuntimeDto {
        schema_version: 1,
        ok: true,
        mode: "no-device-acceptance",
        backend: "rust",
        version: &build.version,
        app_release_version: &build.app_release_version,
        build_number: &build.build_number,
        source_digest: &build.source_digest,
        run_id: &run.run_id,
        daemon_pid: run.daemon_pid,
        app_pid: run.app_pid,
        hardware_owner: "disabled_no_device_acceptance",
        hardware_ownership: false,
        hardware_ready: false,
        stores_opened: false,
        providers_started: false,
        external_network_enabled: false,
    })
    .map_err(|_| DtoFault::Encode)
}

pub(crate) fn health_json(run: &AcceptanceRunIdentity) -> Result<Vec<u8>, DtoFault> {
    serde_json::to_vec(&AcceptanceHealthDto {
        schema_version: 1,
        ok: true,
        mode: "no-device-acceptance",
        run_id: &run.run_id,
        daemon_pid: run.daemon_pid,
        app_pid: run.app_pid,
        service_ready: true,
        hardware_owner: "disabled_no_device_acceptance",
        hardware_ownership: false,
        hardware_ready: false,
        stores_opened: false,
        providers_started: false,
        external_network_enabled: false,
    })
    .map_err(|_| DtoFault::Encode)
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub(crate) enum ExpectedClientEnd {
    PeerEof,
    PeerReset,
    BrokenPipe,
    HeaderDeadline,
    ResponseIdleDeadline,
    ResponseAbsoluteDeadline,
    MalformedHttp,
    InvalidTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoPhase {
    Accept,
    HeaderRead,
    ResponseWrite,
    Retirement,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AcceptanceFault {
    InternalIo { phase: IoPhase, errno: Option<i32> },
    ProtocolInvariant,
    TransferInvariant,
    TaskPanic,
    UnexpectedCancellation,
    AcceptFailed,
    ContextDrift,
    ProofDeadline,
    MonitorLost,
    ReportOverflow,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ConnectionOutcome {
    Responded { status: u16 },
    ClientEnded(ExpectedClientEnd),
    Fault(AcceptanceFault),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitCause {
    RequestedTerm,
    RequestedInt,
    Fault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcquirePhase {
    Start,
    ParentOpened,
    ParentValidated,
    LockOpened,
    LockValidated,
    LockHeld,
    ClaimValidated,
    Committed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindPhase {
    ClaimHeld,
    PathAbsent,
    SocketFdOpened,
    SocketConfigured,
    BoundUncaptured,
    Captured,
    PermissionChecked,
    Listening,
    Registered,
    ContextChecked,
    Published,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LockDisposition {
    NotAttempted,
    NotHeld,
    Held,
    Uncertain,
    Released,
    FailedOrUncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseDisposition {
    NotOwned,
    Owned,
    Closed,
    FailedOrUncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceRole {
    Parent,
    Lock,
    Socket,
    ListenerRegistration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResourceId(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CleanupAction {
    Deregister,
    Close,
    Unlock,
    Stat,
    Unlink,
    Sync,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CleanupResult {
    NotOwned,
    Success,
    Fault,
    Uncertain,
    SkippedUnsafe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerDisposition {
    NotOwned,
    Owned,
    Closed,
    FailedOrUncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathDisposition {
    Absent,
    Removed,
    ReplacementPreserved,
    UncapturedBoundPathPreserved,
    UnknownPreserved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StableFault {
    code: &'static str,
}

impl StableFault {
    pub(crate) fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CleanupStep {
    resource: ResourceId,
    action: CleanupAction,
    result: CleanupResult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResourceState {
    id: ResourceId,
    role: ResourceRole,
    close: CloseDisposition,
}

#[derive(Default)]
pub(crate) struct ResourceLedger {
    next: u32,
    resources: Vec<ResourceState>,
}

impl ResourceLedger {
    pub(crate) fn register(&mut self, role: ResourceRole) -> Result<ResourceId, ReportFault> {
        if self.resources.len() == MAX_RESOURCES || self.next == u32::MAX {
            return Err(ReportFault::ResourceOverflow);
        }
        let id = ResourceId(self.next);
        self.next += 1;
        self.resources.push(ResourceState {
            id,
            role,
            close: CloseDisposition::Owned,
        });
        Ok(id)
    }

    pub(crate) fn close_once(
        &mut self,
        id: ResourceId,
        result: CloseDisposition,
    ) -> Result<(), ReportFault> {
        if !matches!(
            result,
            CloseDisposition::Closed | CloseDisposition::FailedOrUncertain
        ) {
            return Err(ReportFault::InvalidTransition);
        }
        let Some(resource) = self.resources.iter_mut().find(|resource| resource.id == id) else {
            return Err(ReportFault::UnknownResource);
        };
        if resource.close != CloseDisposition::Owned {
            return Err(ReportFault::DoubleDisposition);
        }
        resource.close = result;
        Ok(())
    }

    fn all_final(&self) -> bool {
        self.resources.iter().all(|resource| {
            matches!(
                resource.close,
                CloseDisposition::Closed | CloseDisposition::FailedOrUncertain
            )
        })
    }
}

#[derive(Default)]
pub(crate) struct BoundedCleanupSteps {
    steps: Vec<CleanupStep>,
    overflow: u64,
}

impl BoundedCleanupSteps {
    pub(crate) fn push(&mut self, step: CleanupStep) -> Result<(), ReportFault> {
        if self.steps.len() == MAX_CLEANUP_STEPS {
            self.overflow = self.overflow.saturating_add(1);
            return Err(ReportFault::CleanupOverflow);
        }
        self.steps.push(step);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReportFault {
    ResourceOverflow,
    CleanupOverflow,
    UnknownResource,
    DoubleDisposition,
    InvalidTransition,
}

pub(crate) struct AcquisitionReport {
    pub(crate) last_phase: AcquirePhase,
    pub(crate) resources: ResourceLedger,
    pub(crate) lock: LockDisposition,
    pub(crate) primary_fault: Option<StableFault>,
    pub(crate) cleanup: BoundedCleanupSteps,
    pub(crate) cleanup_complete: bool,
    pub(crate) unchecked_fallback_used: bool,
}

impl Default for AcquisitionReport {
    fn default() -> Self {
        Self {
            last_phase: AcquirePhase::Start,
            resources: ResourceLedger::default(),
            lock: LockDisposition::NotAttempted,
            primary_fault: None,
            cleanup: BoundedCleanupSteps::default(),
            cleanup_complete: false,
            unchecked_fallback_used: false,
        }
    }
}

impl AcquisitionReport {
    pub(crate) fn finalize(&mut self) {
        self.cleanup_complete = self.resources.all_final()
            && self.cleanup.overflow == 0
            && !self.unchecked_fallback_used
            && !matches!(
                self.lock,
                LockDisposition::Held
                    | LockDisposition::Uncertain
                    | LockDisposition::FailedOrUncertain
            )
            && self
                .resources
                .resources
                .iter()
                .all(|resource| resource.close != CloseDisposition::FailedOrUncertain)
            && self.cleanup.steps.iter().all(|step| {
                !matches!(
                    step.result,
                    CleanupResult::Fault | CleanupResult::Uncertain | CleanupResult::SkippedUnsafe
                )
            });
    }
}

pub(crate) struct BindReport {
    pub(crate) last_phase: BindPhase,
    pub(crate) acquisition: AcquisitionReport,
    pub(crate) listener: ListenerDisposition,
    pub(crate) captured_socket: bool,
    pub(crate) path_result: PathDisposition,
    pub(crate) primary_fault: Option<StableFault>,
    pub(crate) cleanup: BoundedCleanupSteps,
    pub(crate) cleanup_complete: bool,
    pub(crate) unchecked_fallback_used: bool,
}

impl BindReport {
    pub(crate) fn finalize(&mut self) {
        self.acquisition.finalize();
        self.cleanup_complete = self.acquisition.cleanup_complete
            // Before bind, an absent path needs no captured identity. Once bind
            // has happened, no clean result is possible without that identity.
            && (self.captured_socket
                || matches!(
                    self.last_phase,
                    BindPhase::ClaimHeld
                        | BindPhase::PathAbsent
                        | BindPhase::SocketFdOpened
                        | BindPhase::SocketConfigured
                ))
            // A removal is never justified by an uncaptured socket, even if a
            // caller supplies an inconsistent pre-bind phase.
            && (self.captured_socket || self.path_result != PathDisposition::Removed)
            && self.cleanup.overflow == 0
            && !self.unchecked_fallback_used
            && matches!(
                self.listener,
                ListenerDisposition::NotOwned | ListenerDisposition::Closed
            )
            && !matches!(
                self.path_result,
                PathDisposition::ReplacementPreserved
                    | PathDisposition::UncapturedBoundPathPreserved
                    | PathDisposition::UnknownPreserved
            )
            && self.cleanup.steps.iter().all(|step| {
                !matches!(
                    step.result,
                    CleanupResult::Fault | CleanupResult::Uncertain | CleanupResult::SkippedUnsafe
                )
            });
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExitReportFault {
    FaultOverflow,
    JoinOverflow,
    ReapWithoutOutstandingSpawn,
}

pub(crate) struct RetirementReport {
    pub(crate) cleanup_complete: bool,
    pub(crate) unchecked_fallback_used: bool,
    pub(crate) residual_or_uncertain: bool,
}

pub(crate) struct ExitReport {
    cause: ExitCause,
    expected_client_ends: BTreeMap<ExpectedClientEnd, u64>,
    faults: Vec<AcceptanceFault>,
    fault_overflow: u64,
    joins_spawned: u64,
    joins_reaped: u64,
    join_protocol_failed: bool,
    retirement: Option<RetirementReport>,
    final_context_ok: bool,
    proof_worker_joined: bool,
}

impl ExitReport {
    pub(crate) fn new(cause: ExitCause) -> Self {
        Self {
            cause,
            expected_client_ends: BTreeMap::new(),
            faults: Vec::new(),
            fault_overflow: 0,
            joins_spawned: 0,
            joins_reaped: 0,
            join_protocol_failed: false,
            retirement: None,
            final_context_ok: false,
            proof_worker_joined: false,
        }
    }

    pub(crate) fn spawned(&mut self) -> Result<(), ExitReportFault> {
        let Some(next) = self.joins_spawned.checked_add(1) else {
            self.join_protocol_failed = true;
            return Err(ExitReportFault::JoinOverflow);
        };
        self.joins_spawned = next;
        Ok(())
    }

    pub(crate) fn reaped(&mut self, outcome: ConnectionOutcome) -> Result<(), ExitReportFault> {
        if self.joins_reaped >= self.joins_spawned {
            self.join_protocol_failed = true;
            return Err(ExitReportFault::ReapWithoutOutstandingSpawn);
        }
        // The outstanding-spawn check proves this increment cannot overflow.
        self.joins_reaped += 1;
        match outcome {
            ConnectionOutcome::Responded { .. } => Ok(()),
            ConnectionOutcome::ClientEnded(reason) => {
                let count = self.expected_client_ends.entry(reason).or_default();
                *count = count.saturating_add(1);
                Ok(())
            }
            ConnectionOutcome::Fault(fault) => self.push_fault(fault),
        }
    }

    pub(crate) fn push_fault(&mut self, fault: AcceptanceFault) -> Result<(), ExitReportFault> {
        if self.faults.len() == MAX_FAULTS {
            self.fault_overflow = self.fault_overflow.saturating_add(1);
            return Err(ExitReportFault::FaultOverflow);
        }
        self.faults.push(fault);
        Ok(())
    }

    pub(crate) fn complete(
        &mut self,
        retirement: RetirementReport,
        final_context_ok: bool,
        proof_worker_joined: bool,
    ) {
        self.retirement = Some(retirement);
        self.final_context_ok = final_context_ok;
        self.proof_worker_joined = proof_worker_joined;
    }

    pub(crate) fn clean_exit(&self) -> bool {
        matches!(
            self.cause,
            ExitCause::RequestedTerm | ExitCause::RequestedInt
        ) && self.faults.is_empty()
            && self.fault_overflow == 0
            && !self.join_protocol_failed
            && self.joins_spawned == self.joins_reaped
            && self.final_context_ok
            && self.proof_worker_joined
            && self.retirement.as_ref().is_some_and(|retirement| {
                retirement.cleanup_complete
                    && !retirement.unchecked_fallback_used
                    && !retirement.residual_or_uncertain
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};
    use std::os::unix::ffi::OsStringExt;

    fn os(bytes: &[u8]) -> OsString {
        OsString::from_vec(bytes.to_vec())
    }

    fn exact() -> Vec<OsString> {
        vec![
            os(b"-listen"),
            os(b"unix:/private/run/switch/data/switch.sock"),
            os(b"-backend=at"),
            os(b"--switch-acceptance-capsule=/private/run/switch/data/daemon.json"),
        ]
    }

    #[test]
    fn raw_truth_table_accepts_only_exact_four_argument_shape() {
        let entry = classify_raw(&exact());
        let LexicalEntry::Candidate(shape) = entry else {
            panic!("exact shape was not a candidate")
        };
        assert_eq!(
            shape.socket_path(),
            std::path::Path::new("/private/run/switch/data/switch.sock")
        );
        assert_eq!(
            shape.capsule_path(),
            std::path::Path::new("/private/run/switch/data/daemon.json")
        );
        assert_eq!(
            shipping_decision(&LexicalEntry::Candidate(shape)),
            ScaffoldShippingDecision::Refuse78
        );
    }

    #[test]
    fn raw_truth_table_scans_every_os_argument_before_utf8_or_terminator() {
        for args in [
            vec![os(b"--"), os(b"--switch-acceptance")],
            vec![os(b"--switch-acceptance")],
            vec![os(b"-switch-acceptance")],
            vec![os(b"position"), os(b"--switch-acceptance-capsul")],
            vec![os(b"-switch-acceptance-capsule=/a")],
            vec![os(b"--switch-acceptance-unknown")],
            vec![os(b"--switch-acceptance\xff")],
        ] {
            assert!(matches!(classify_raw(&args), LexicalEntry::Reject(_)));
        }
        assert!(matches!(
            classify_raw(&[os(b"--switch-acceptanc")]),
            LexicalEntry::Production
        ));
        assert!(matches!(
            classify_raw(&[os(b"-switch-acceptanc")]),
            LexicalEntry::Production
        ));
    }

    #[test]
    fn raw_truth_table_rejects_duplicates_reordering_trailing_and_invalid_paths() {
        let mut cases = Vec::new();
        let mut duplicate = exact();
        duplicate.push(os(b"--switch-acceptance-capsule=/other"));
        cases.push(duplicate);
        let mut both_reserved_prefixes = exact();
        both_reserved_prefixes.push(os(b"-switch-acceptance"));
        cases.push(both_reserved_prefixes);
        let mut reordered = exact();
        reordered.swap(0, 2);
        cases.push(reordered);
        let mut invalid_utf8 = exact();
        invalid_utf8[3] = os(b"--switch-acceptance-capsule=/bad\xff");
        cases.push(invalid_utf8);
        let mut relative = exact();
        relative[3] = os(b"--switch-acceptance-capsule=relative");
        cases.push(relative);
        let mut dot = exact();
        dot[1] = os(b"unix:/private/run/../data/switch.sock");
        cases.push(dot);
        let mut qmi = exact();
        qmi[2] = os(b"-backend=qmi");
        cases.push(qmi);
        let mut trailing = exact();
        trailing.push(os(b"-native-vowifi=true"));
        cases.push(trailing);
        for args in cases {
            assert!(matches!(classify_raw(&args), LexicalEntry::Reject(_)));
        }
    }

    #[test]
    fn environment_cannot_select_acceptance_and_no_prefix_is_production() {
        // The classifier has no environment parameter and performs no reads.
        for args in [vec![], vec![os(b"-listen"), os(b"unix:/ordinary.sock")]] {
            let entry = classify_raw(&args);
            assert!(matches!(entry, LexicalEntry::Production));
            assert_eq!(
                shipping_decision(&entry),
                ScaffoldShippingDecision::Production
            );
        }
    }

    fn keys(bytes: &[u8]) -> Map<String, Value> {
        serde_json::from_slice::<Value>(bytes)
            .unwrap()
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn runtime_is_exact_17_and_health_is_exact_13() {
        let build = AcceptanceBuildMetadata::validated(
            "3.51".into(),
            "3.51".into(),
            "351".into(),
            "d".repeat(64),
        )
        .unwrap();
        let run = AcceptanceRunIdentity::validated("a".repeat(32), 41, 42).unwrap();
        let runtime = keys(&runtime_json(&build, &run).unwrap());
        let health = keys(&health_json(&run).unwrap());
        let runtime_expected = [
            "schema_version",
            "ok",
            "mode",
            "backend",
            "version",
            "app_release_version",
            "build_number",
            "source_digest",
            "run_id",
            "daemon_pid",
            "app_pid",
            "hardware_owner",
            "hardware_ownership",
            "hardware_ready",
            "stores_opened",
            "providers_started",
            "external_network_enabled",
        ];
        let health_expected = [
            "schema_version",
            "ok",
            "mode",
            "run_id",
            "daemon_pid",
            "app_pid",
            "service_ready",
            "hardware_owner",
            "hardware_ownership",
            "hardware_ready",
            "stores_opened",
            "providers_started",
            "external_network_enabled",
        ];
        assert_eq!(runtime.len(), 17);
        assert_eq!(health.len(), 13);
        for key in runtime_expected {
            assert!(runtime.contains_key(key), "missing runtime {key}");
        }
        for key in health_expected {
            assert!(health.contains_key(key), "missing health {key}");
        }
        for object in [&runtime, &health] {
            assert_eq!(object["schema_version"], 1);
            assert_eq!(object["ok"], true);
            assert_eq!(object["mode"], "no-device-acceptance");
            assert_eq!(object["run_id"], "a".repeat(32));
            assert_eq!(object["daemon_pid"], 41);
            assert_eq!(object["app_pid"], 42);
            assert_eq!(object["hardware_owner"], "disabled_no_device_acceptance");
            for key in [
                "hardware_ownership",
                "hardware_ready",
                "stores_opened",
                "providers_started",
                "external_network_enabled",
            ] {
                assert_eq!(object[key], false, "{key}");
            }
        }
        assert_eq!(runtime["backend"], "rust");
        assert_eq!(health["service_ready"], true);
        assert!(!health.contains_key("backend"));
        assert!(!health.contains_key("version"));
        assert!(!health.contains_key("source_digest"));
        assert!(!health.contains_key("build_number"));
    }

    #[test]
    fn dto_authority_rejects_invalid_identity_or_metadata() {
        assert!(AcceptanceRunIdentity::validated("A".repeat(32), 1, 1).is_err());
        assert!(AcceptanceRunIdentity::validated("a".repeat(32), 0, 1).is_err());
        assert!(AcceptanceRunIdentity::validated("a".repeat(32), 1, 0).is_err());
        assert!(
            AcceptanceBuildMetadata::validated("".into(), "v".into(), "1".into(), "a".repeat(64))
                .is_err()
        );
        assert!(
            AcceptanceBuildMetadata::validated("v".into(), "v".into(), "1".into(), "A".repeat(64))
                .is_err()
        );
    }

    #[test]
    fn exit_report_requires_requested_signal_full_drain_and_clean_retirement() {
        let mut report = ExitReport::new(ExitCause::RequestedTerm);
        report.spawned().unwrap();
        report
            .reaped(ConnectionOutcome::ClientEnded(ExpectedClientEnd::PeerEof))
            .unwrap();
        report.complete(
            RetirementReport {
                cleanup_complete: true,
                unchecked_fallback_used: false,
                residual_or_uncertain: false,
            },
            true,
            true,
        );
        assert!(report.clean_exit());
        report.push_fault(AcceptanceFault::ContextDrift).unwrap();
        assert!(!report.clean_exit());
    }

    #[test]
    fn exit_report_fault_capacity_overflow_is_terminal() {
        let mut report = ExitReport::new(ExitCause::RequestedInt);
        for _ in 0..MAX_FAULTS {
            report
                .push_fault(AcceptanceFault::ProtocolInvariant)
                .unwrap();
        }
        assert_eq!(
            report.push_fault(AcceptanceFault::ProtocolInvariant),
            Err(ExitReportFault::FaultOverflow)
        );
        report.complete(
            RetirementReport {
                cleanup_complete: true,
                unchecked_fallback_used: false,
                residual_or_uncertain: false,
            },
            true,
            true,
        );
        assert!(!report.clean_exit());
    }

    #[test]
    fn checked_report_scaffold_is_bounded_single_disposition_and_fail_closed() {
        let mut acquisition = AcquisitionReport::default();
        acquisition.last_phase = AcquirePhase::LockHeld;
        acquisition.lock = LockDisposition::Held;
        let parent = acquisition
            .resources
            .register(ResourceRole::Parent)
            .unwrap();
        let lock = acquisition.resources.register(ResourceRole::Lock).unwrap();
        acquisition
            .resources
            .close_once(lock, CloseDisposition::Closed)
            .unwrap();
        assert_eq!(
            acquisition
                .resources
                .close_once(lock, CloseDisposition::Closed),
            Err(ReportFault::DoubleDisposition)
        );
        acquisition
            .resources
            .close_once(parent, CloseDisposition::Closed)
            .unwrap();
        acquisition.lock = LockDisposition::Released;
        acquisition
            .cleanup
            .push(CleanupStep {
                resource: lock,
                action: CleanupAction::Unlock,
                result: CleanupResult::Success,
            })
            .unwrap();
        acquisition.finalize();
        assert!(acquisition.cleanup_complete);

        let mut bind = BindReport {
            last_phase: BindPhase::BoundUncaptured,
            acquisition,
            listener: ListenerDisposition::Closed,
            captured_socket: false,
            path_result: PathDisposition::UncapturedBoundPathPreserved,
            primary_fault: Some(StableFault::new("capture_failed")),
            cleanup: BoundedCleanupSteps::default(),
            cleanup_complete: false,
            unchecked_fallback_used: false,
        };
        bind.finalize();
        assert!(!bind.cleanup_complete);
    }

    #[test]
    fn checked_report_capacity_and_uncertainty_never_become_clean() {
        let mut ledger = ResourceLedger::default();
        for _ in 0..MAX_RESOURCES {
            ledger.register(ResourceRole::Parent).unwrap();
        }
        assert_eq!(
            ledger.register(ResourceRole::Parent),
            Err(ReportFault::ResourceOverflow)
        );
        let mut acquisition = AcquisitionReport::default();
        let id = acquisition
            .resources
            .register(ResourceRole::Parent)
            .unwrap();
        acquisition
            .resources
            .close_once(id, CloseDisposition::FailedOrUncertain)
            .unwrap();
        acquisition.unchecked_fallback_used = true;
        acquisition.finalize();
        assert!(!acquisition.cleanup_complete);
    }

    fn bind_report(
        last_phase: BindPhase,
        captured_socket: bool,
        path_result: PathDisposition,
    ) -> BindReport {
        BindReport {
            last_phase,
            acquisition: AcquisitionReport::default(),
            listener: ListenerDisposition::NotOwned,
            captured_socket,
            path_result,
            primary_fault: None,
            cleanup: BoundedCleanupSteps::default(),
            cleanup_complete: false,
            unchecked_fallback_used: false,
        }
    }

    #[test]
    fn uncaptured_socket_and_removed_path_must_not_finalize_clean() {
        // Exact first adversarial state from the independent review.
        let mut bind = bind_report(BindPhase::BoundUncaptured, false, PathDisposition::Removed);
        bind.finalize();
        assert!(bind.acquisition.cleanup_complete);
        assert!(!bind.cleanup_complete);
    }

    #[test]
    fn uncaptured_post_bind_cleanup_is_never_clean_for_any_path_disposition() {
        for phase in [
            BindPhase::BoundUncaptured,
            BindPhase::Captured,
            BindPhase::PermissionChecked,
            BindPhase::Listening,
            BindPhase::Registered,
            BindPhase::ContextChecked,
            BindPhase::Published,
        ] {
            for path in [
                PathDisposition::Absent,
                PathDisposition::Removed,
                PathDisposition::ReplacementPreserved,
                PathDisposition::UncapturedBoundPathPreserved,
                PathDisposition::UnknownPreserved,
            ] {
                let mut bind = bind_report(phase, false, path);
                bind.cleanup_complete = true; // Finalize must revoke stale claims.
                bind.finalize();
                assert!(!bind.cleanup_complete, "{phase:?}, {path:?}");
                assert_eq!(bind.path_result, path); // No unlink is performed here.
            }
        }
    }

    #[test]
    fn bind_cleanup_preserves_pre_bind_absence_and_captured_removal_boundaries() {
        for phase in [
            BindPhase::ClaimHeld,
            BindPhase::PathAbsent,
            BindPhase::SocketFdOpened,
            BindPhase::SocketConfigured,
        ] {
            let mut absent = bind_report(phase, false, PathDisposition::Absent);
            absent.finalize();
            assert!(absent.cleanup_complete, "{phase:?}");
            let mut removed = bind_report(phase, false, PathDisposition::Removed);
            removed.finalize();
            assert!(!removed.cleanup_complete, "{phase:?}");
        }
        for phase in [
            BindPhase::Captured,
            BindPhase::PermissionChecked,
            BindPhase::Listening,
            BindPhase::Registered,
            BindPhase::ContextChecked,
            BindPhase::Published,
        ] {
            let mut bind = bind_report(phase, true, PathDisposition::Removed);
            bind.listener = ListenerDisposition::Closed;
            bind.finalize();
            assert!(bind.cleanup_complete, "{phase:?}");
        }
    }

    fn complete_clean_retirement(report: &mut ExitReport) {
        report.complete(
            RetirementReport {
                cleanup_complete: true,
                unchecked_fallback_used: false,
                residual_or_uncertain: false,
            },
            true,
            true,
        );
    }

    #[test]
    fn reap_before_spawn_must_not_become_clean_when_counts_later_match() {
        // Exact second adversarial ordering, now rejected explicitly.
        let mut report = ExitReport::new(ExitCause::RequestedTerm);
        assert_eq!(
            report.reaped(ConnectionOutcome::Responded { status: 200 }),
            Err(ExitReportFault::ReapWithoutOutstandingSpawn)
        );
        assert_eq!((report.joins_spawned, report.joins_reaped), (0, 0));
        report.spawned().unwrap();
        complete_clean_retirement(&mut report);
        assert!(!report.clean_exit());
        report
            .reaped(ConnectionOutcome::Responded { status: 200 })
            .unwrap();
        assert_eq!(report.joins_spawned, report.joins_reaped);
        complete_clean_retirement(&mut report);
        assert!(!report.clean_exit());
    }

    #[test]
    fn double_reap_is_explicit_sticky_and_does_not_record_an_outcome() {
        let mut report = ExitReport::new(ExitCause::RequestedInt);
        report.spawned().unwrap();
        report
            .reaped(ConnectionOutcome::Responded { status: 200 })
            .unwrap();
        assert_eq!(
            report.reaped(ConnectionOutcome::ClientEnded(ExpectedClientEnd::PeerEof)),
            Err(ExitReportFault::ReapWithoutOutstandingSpawn)
        );
        assert!(report.expected_client_ends.is_empty());
        assert_eq!(report.joins_reaped, 1);
        report.spawned().unwrap();
        report
            .reaped(ConnectionOutcome::Responded { status: 200 })
            .unwrap();
        complete_clean_retirement(&mut report);
        assert_eq!(report.joins_spawned, report.joins_reaped);
        assert!(!report.clean_exit());
    }

    #[test]
    fn join_counter_limit_allows_final_reap_but_overflow_is_sticky() {
        let mut report = ExitReport::new(ExitCause::RequestedTerm);
        report.joins_spawned = u64::MAX;
        report.joins_reaped = u64::MAX - 1;
        report
            .reaped(ConnectionOutcome::Responded { status: 200 })
            .unwrap();
        complete_clean_retirement(&mut report);
        assert!(report.clean_exit());
        assert_eq!(report.spawned(), Err(ExitReportFault::JoinOverflow));
        assert_eq!(report.joins_spawned, u64::MAX);
        assert_eq!(report.joins_reaped, u64::MAX);
        complete_clean_retirement(&mut report);
        assert!(!report.clean_exit());
    }

    #[test]
    fn multiple_spawned_tasks_require_full_drain_and_preserve_expected_ends() {
        let mut report = ExitReport::new(ExitCause::RequestedTerm);
        report.spawned().unwrap();
        report.spawned().unwrap();
        report
            .reaped(ConnectionOutcome::Responded { status: 200 })
            .unwrap();
        complete_clean_retirement(&mut report);
        assert!(!report.clean_exit());
        report
            .reaped(ConnectionOutcome::ClientEnded(ExpectedClientEnd::PeerEof))
            .unwrap();
        assert_eq!(report.expected_client_ends[&ExpectedClientEnd::PeerEof], 1);
        assert!(report.clean_exit());
    }
}
