//! Startup quarantine and shared, monotonic new-operation revocation are
//! deliberately separate. Neither is a physical restore receipt. Accepted
//! finite owners retain settlement rights until actual cleanup completes.
//! A pristine marker only describes controlled new-version read-only lineage;
//! an unwrapped old-version downgrade can invalidate that assumption without
//! removing the marker. This preflight is not full downgrade acceptance.
use serde::Serialize;
mod evidence;
pub(crate) mod legacy_locks;
pub(crate) mod runtime_download;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

mod reconciliation;
pub(crate) use reconciliation::{ReconciliationTicket, LegacyCurrentClaim};

pub(crate) const CODE: &str = "legacy_recovery_required";
pub(crate) const MESSAGE: &str = "模块存在未确认的旧版恢复状态；读卡器功能可继续使用";
pub(crate) const LEGACY_EXECUTION_PENDING: &str = ".legacy-execution-pending-v1.json";

pub(crate) fn rejection() -> hyper::Response<http_body_util::Full<bytes::Bytes>> {
    crate::json_response(
        hyper::StatusCode::CONFLICT,
        &serde_json::json!({
            "code": CODE, "error": MESSAGE, "retryable": false,
            "recoverable": false, "submitted": false, "uncertain": false,
            "historical_recovery_confirmed": false
        }),
    )
}

pub(crate) fn call_mode_status(root: &Path) -> crate::call_mode_state::Status {
    // State's normalization is pure metadata projection (manifest/version/
    // paths), not the lifecycle owner that initializes lock evidence.
    let mut status = crate::call_mode_state::State::new(root).snapshot();
    status.state = CODE.into();
    status.summary = MESSAGE.into();
    status
}

/// Module-only endpoints. Shared SMS and ordinary reader IMS controls must
/// resolve their captured owner before applying the module admission fence.
pub(crate) fn owns_public(method: &hyper::Method, path: &str) -> bool {
    use hyper::Method;
    matches!(
        (method, path),
        (
            &Method::POST,
            "/api/at"
                | "/api/module/flight-mode"
                | "/api/esim/phonebook/probe"
                | "/api/esim/switch"
                | "/api/esim/download"
                | "/api/esim/download/confirm"
                | "/api/network/reboot-module"
                | "/api/network/usbnet"
                | "/api/call-mode/download"
                | "/api/call-mode/enable"
                | "/api/call-mode/restore"
                | "/api/call-mode/retry"
                | "/api/call-mode/backups/import"
                | "/api/call-mode/backups/delete"
                | "/api/call/audio/start"
                | "/api/call/audio/stop"
                | "/api/sms/refresh"
                | "/api/sms/adopt"
        ) | (
            &Method::GET,
            "/api/module/flight-mode"
                | "/api/network"
                | "/api/esim/module-notes"
                | "/api/esim"
                | "/api/esim/health"
                | "/api/sms/storage"
        ) | (&Method::PUT, "/api/esim/module-notes")
            | (&Method::PATCH | &Method::DELETE, "/api/esim/profile")
    ) || matches!(
        (method.as_str(), path),
        ("GET" | "POST", "/api/vowifi") | ("GET", "/api/vowifi/result")
    )
}

pub(crate) fn owns_internal(path: &str) -> bool {
    path != crate::modem_actor::PROJECT_CALL_STATE_PATH
        && [
            crate::usb_at::PREFIX,
            crate::module_radio::PREFIX,
            crate::usb_sms_receive::INTERNAL_PREFIX,
            crate::modem_actor::PREFIX,
            "/api/internal/call-mode/",
            crate::qmi_enhancement::PREFIX,
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Status {
    pub(crate) state: &'static str,
    pub(crate) reason_codes: Vec<Reason>,
    pub(crate) historical_recovery_confirmed: bool,
}

type LiveProbe = Arc<dyn Fn() -> Result<bool, Reason> + Send + Sync>;

#[derive(Default)]
struct Authority {
    startup: Option<Status>,
    revoked: OnceLock<Status>,
    admission: Mutex<()>,
    restore_in_progress: AtomicBool,
    probe: Option<LiveProbe>,
    root_proof: Option<crate::daemon_lifetime::RootProof>,
    root_path: Option<PathBuf>,
    reconciliation_started: AtomicBool,
    reconciliation_epoch: AtomicU64,
    startup_reconciled: OnceLock<()>,
}
impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleAuthority")
            .field("startup_quarantined", &self.startup.is_some())
            .field("ordinary_admission_revoked", &self.revoked.get().is_some())
            .field(
                "restore_in_progress",
                &self.restore_in_progress.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Gate(Arc<Authority>);

/// Sealed metadata-only lineage for the separate startup inspector. It is
/// neither an ordinary operation right nor a physical recovery permit, and
/// retains no lifetime lease or Gate ownership.
pub(crate) struct RecoveryAuthority {
    authority: std::sync::Weak<Authority>,
    proof: crate::daemon_lifetime::RootProof,
    root: PathBuf,
}
impl RecoveryAuthority {
    pub(crate) fn validate(&self, gate: &Gate) -> Result<(), ()> {
        let authority = self.authority.upgrade().ok_or(())?;
        if !Arc::ptr_eq(&authority, &gate.0)
            || !authority
                .root_proof
                .as_ref()
                .is_some_and(|proof| proof.same_authority(&self.proof))
        {
            return Err(());
        }
        self.proof.with_root(&self.root, |_| ()).map_err(|_| ())
    }

    pub(crate) fn root_proof(&self) -> &crate::daemon_lifetime::RootProof {
        &self.proof
    }
}

/// Lineage for the distinct finite startup native-read owner. This is not an
/// executable scope: that owner must independently pin the original locks,
/// immutable journal/evidence and its versioned read-only policy. Metadata
/// inspection rights cannot be converted into this type or a native scope.
pub(crate) struct PhysicalReadLineage {
    authority: std::sync::Weak<Authority>,
    proof: crate::daemon_lifetime::RootProof,
    root: PathBuf,
}
impl PhysicalReadLineage {
    pub(crate) fn validate(&self, gate: &Gate) -> Result<(), ()> {
        let authority = self.authority.upgrade().ok_or(())?;
        if !Arc::ptr_eq(&authority, &gate.0)
            || authority.startup.is_none()
            || authority.startup_reconciled.get().is_some()
            || !authority
                .root_proof
                .as_ref()
                .is_some_and(|proof| proof.same_authority(&self.proof))
        {
            return Err(());
        }
        self.proof.with_root(&self.root, |_| ()).map_err(|_| ())
    }

    pub(crate) fn root_proof(&self) -> &crate::daemon_lifetime::RootProof {
        &self.proof
    }
}

/// The existing finite restore keeps its own operation right. This guard
/// blocks only newly minted rights while its exact durable journal is active.
/// Unfinished Drop becomes monotonic quarantine; it cannot end cleanup rights.
#[must_use]
pub(crate) struct RestoreJournalAdmission {
    authority: Arc<Authority>,
    journal: crate::call_mode_restore_journal::Journal,
    completed: bool,
}
impl RestoreJournalAdmission {
    pub(crate) fn complete(&mut self) -> Result<(), ()> {
        if self.completed {
            return Ok(());
        }
        let _admission = self
            .authority
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.authority.restore_in_progress.load(Ordering::Acquire) {
            return Err(());
        }
        let proof = self.authority.root_proof.as_ref().ok_or(())?;
        self.journal.completed_for_root(proof).map_err(|_| ())?;
        self.authority
            .restore_in_progress
            .store(false, Ordering::Release);
        self.completed = true;
        Ok(())
    }
}
impl Drop for RestoreJournalAdmission {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let _admission = self
            .authority
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = self.authority.revoked.set(Status {
            state: CODE,
            reason_codes: vec![Reason::RestoreJournalPending],
            historical_recovery_confirmed: false,
        });
        self.authority
            .restore_in_progress
            .store(false, Ordering::Release);
    }
}

/// Non-clone owner of one finite admitted operation, not a session permission.
/// Cancellation must leave this guard in the retained physical worker. Rights
/// alone cannot extend the operation beyond its actual owner's lifetime.
pub(crate) struct OperationGuard {
    right: OperationRight,
}
#[derive(Clone)]
pub(crate) struct OperationRight(Arc<OperationIdentity>);
struct OperationIdentity {
    authority: Arc<Authority>,
    producer: String,
    active: AtomicBool,
}
impl OperationGuard {
    pub(crate) fn right(&self) -> OperationRight {
        self.right.clone()
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.right.0.active.store(false, Ordering::Release);
    }
}
impl OperationRight {
    /// This is logical operation authority only. Callers must still validate
    /// their captured physical binding, producer/epoch/session and job scope.
    pub(crate) fn is_active_for(&self, gate: &Gate, producer: &str) -> bool {
        Arc::ptr_eq(&self.0.authority, &gate.0)
            && self.0.producer == producer
            && self.0.active.load(Ordering::Acquire)
    }
}
impl std::fmt::Debug for OperationRight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationRight")
            .field("active", &self.0.active.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for OperationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OperationGuard").field(&self.right).finish()
    }
}

impl Gate {
    /// Only the exact metadata-only upgraded lock pair can explain an idle
    /// shutdown. This is not a physical restoration receipt.
    pub(crate) fn has_clean_legacy_lock_provenance(&self, root: &Path) -> bool {
        let Ok(_admission) = self.0.admission.lock() else {
            return false;
        };
        if self.refresh_locked() {
            return false;
        }
        self.0.root_proof.as_ref().is_some_and(|proof| {
            proof
                .with_root(root, |directory| {
                    if legacy_locks::covered(directory)? {
                        return Ok(true);
                    }
                    runtime_download::coverage(
                        directory,
                        &crate::call_mode_runtime_manifest::manifest().files,
                    ).map(|coverage| coverage == Some(true))
                })
                .is_ok_and(|result| result.unwrap_or(false))
        })
    }
    /// Capture only the same quarantined authority and real root lifetime.
    /// This neither admits I/O nor clears any startup/revocation reason.
    pub(crate) fn capture_physical_read_lineage(
        &self,
        root: &Path,
    ) -> Result<PhysicalReadLineage, ()> {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_quarantined() {
            return Err(());
        }
        let proof = self.0.root_proof.as_ref().ok_or(())?;
        proof.with_root(root, |_| ()).map_err(|_| ())?;
        Ok(PhysicalReadLineage {
            authority: Arc::downgrade(&self.0),
            proof: proof.clone(),
            root: root.to_owned(),
        })
    }

    /// Startup metadata inspection has its own sealed authority. Quarantine
    /// deliberately remains in force; this method never refreshes, clears or
    /// converts ordinary admission and cannot mint an OperationRight.
    pub(crate) fn capture_recovery_authority(&self, root: &Path) -> Result<RecoveryAuthority, ()> {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_quarantined() {
            return Err(());
        }
        let proof = self.0.root_proof.as_ref().ok_or(())?;
        proof.with_root(root, |_| ()).map_err(|_| ())?;
        Ok(RecoveryAuthority {
            authority: Arc::downgrade(&self.0),
            proof: proof.clone(),
            root: root.to_owned(),
        })
    }

    pub(crate) fn inspect(root: &Path) -> Self {
        Self::inspect_with_runtime(root, &crate::call_mode_runtime_manifest::manifest().files)
    }

    pub(crate) fn inspect_with_lifetime(lease: &crate::daemon_lifetime::Lease) -> Self {
        Self::inspect_with_lifetime_and_artifacts(
            lease, &crate::call_mode_runtime_manifest::manifest().files,
        )
    }

    #[cfg(test)]
    pub(crate) fn inspect_download_fixture_with_lifetime(
        lease: &crate::daemon_lifetime::Lease,
        artifacts: &[crate::call_mode_runtime_manifest::Artifact],
    ) -> Self {
        Self::inspect_with_lifetime_and_artifacts(lease, artifacts)
    }

    fn inspect_with_lifetime_and_artifacts(
        lease: &crate::daemon_lifetime::Lease,
        artifacts: &[crate::call_mode_runtime_manifest::Artifact],
    ) -> Self {
        let root = lease.root().to_path_buf();
        let root_proof = lease.root_proof();
        // Initial inspection and every later probe belong to the same actual
        // lease. Keeping a path, or retaining a proof after its owner exits,
        // cannot manufacture a new root authority.
        let initial = root_proof
            .with_root(&root, |directory| {
                Self::from_reasons(evidence::inspect(directory, artifacts))
            })
            .unwrap_or_else(|_| {
                Self(Arc::new(Authority {
                    startup: Some(Status {
                        state: CODE,
                        reason_codes: vec![Reason::LifetimeAuthorityChanged],
                        historical_recovery_confirmed: false,
                    }),
                    ..Authority::default()
                }))
            });
        let live_proof = root_proof.clone();
        let root_path = root.clone();
        Self(Arc::new(Authority {
            startup: initial.0.startup.clone(),
            probe: Some(Arc::new(move || {
                // The current finite restore writes its own active journal.
                // Only startup inspection treats that journal as unfinished
                // predecessor evidence; observing it live must not revoke its
                // retained operation. The legacy downgrade fence is separate.
                live_proof
                    .with_root(&root, |directory| {
                        match nix::sys::stat::fstatat(
                            directory,
                            LEGACY_EXECUTION_PENDING,
                            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
                        ) {
                            Ok(_) => Ok(true),
                            Err(nix::errno::Errno::ENOENT) => Ok(false),
                            Err(_) => Err(Reason::LifetimeAuthorityChanged),
                        }
                    })
                    .map_err(|_| Reason::LifetimeAuthorityChanged)
                    .and_then(|result| result)
            })),
            root_proof: Some(root_proof),
            root_path: Some(root_path),
            ..Authority::default()
        }))
    }

    /// A filesystem ownership proof only; this never admits ordinary work,
    /// clears startup quarantine, or authorizes physical recovery.
    pub(crate) fn root_proof_for(
        &self,
        root: &Path,
    ) -> Result<crate::daemon_lifetime::RootProof, ()> {
        let proof = self.0.root_proof.as_ref().ok_or(())?;
        proof.with_root(root, |_| ()).map_err(|_| ())?;
        Ok(proof.clone())
    }

    /// A current data-only observation for the existing shutdown decision.
    /// This neither admits an operation nor settles any physical obligation.
    pub(crate) fn has_clean_fresh_backup_data(&self, root: &Path) -> bool {
        let Ok(_admission) = self.0.admission.lock() else {
            return false;
        };
        if self.refresh_locked() {
            return false;
        }
        let Some(proof) = self.0.root_proof.as_ref() else {
            return false;
        };
        let clean = proof.with_root(root, |directory| -> std::io::Result<bool> {
            let Some(coverage) = crate::startup_recovery::reconcile::backup_lifecycle::verified_fresh_backup_slot(directory)? else {
                return Ok(false);
            };
            if !evidence::inspect(directory, &crate::call_mode_runtime_manifest::manifest().files).is_empty() {
                return Ok(false);
            }
            coverage.validate(directory)?;
            Ok(true)
        }).is_ok_and(|result| result.unwrap_or(false));
        clean && !self.refresh_locked()
    }

    fn inspect_with_runtime(
        root: &Path,
        artifacts: &[crate::call_mode_runtime_manifest::Artifact],
    ) -> Self {
        use std::os::unix::fs::OpenOptionsExt;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                nix::libc::O_DIRECTORY
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NONBLOCK,
            )
            .open(root);
        match directory {
            Ok(directory) => Self::from_reasons(evidence::inspect(&directory, artifacts)),
            // Keep the historical path-only convenience's missing-root case.
            // It has no RootProof and cannot manufacture root/recovery authority.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self::from_reasons(vec![Reason::UnreadableEvidence]),
        }
    }

    fn from_reasons(reasons: Vec<Reason>) -> Self {
        if reasons.is_empty() {
            Self::default()
        } else {
            Self(Arc::new(Authority {
                startup: Some(Status {
                    state: CODE,
                    reason_codes: reasons,
                    historical_recovery_confirmed: false,
                }),
                ..Authority::default()
            }))
        }
    }
    pub(crate) fn is_quarantined(&self) -> bool {
        // Existing startup projections/drain assertions cannot be reused for
        // live revocation: those owners may already have real obligations.
        self.0.startup.is_some() && self.0.startup_reconciled.get().is_none()
    }
    pub(crate) fn status(&self) -> Option<&Status> {
        self.is_admission_revoked();
        self.observed_status()
    }

    /// Last observed state only. Support export must never probe evidence or
    /// turn a diagnostic read into a new admission/revocation decision.
    pub(crate) fn observed_status(&self) -> Option<&Status> {
        if self.is_quarantined() {
            self.0.startup.as_ref()
        } else {
            self.0.revoked.get()
        }
    }
    fn refresh_locked(&self) -> bool {
        if self.is_quarantined() || self.0.revoked.get().is_some() {
            return true;
        }
        if let Some(probe) = &self.0.probe {
            let reason = match probe() {
                Ok(false) => None,
                Ok(true) => Some(Reason::LegacyExecutionPending),
                Err(reason) => Some(reason),
            };
            if let Some(reason) = reason {
                let _ = self.0.revoked.set(Status {
                    state: CODE,
                    reason_codes: vec![reason],
                    historical_recovery_confirmed: false,
                });
            }
        }
        self.0.revoked.get().is_some()
    }
    pub(crate) fn is_admission_revoked(&self) -> bool {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        self.refresh_locked()
    }
    pub(crate) fn admit_operation(&self, producer: &str) -> Result<OperationGuard, ()> {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        if producer.is_empty()
            || producer.len() > 128
            || self.refresh_locked()
            || self.0.restore_in_progress.load(Ordering::Acquire)
        {
            return Err(());
        }
        Ok(OperationGuard {
            right: OperationRight(Arc::new(OperationIdentity {
                authority: self.0.clone(),
                producer: producer.to_owned(),
                active: AtomicBool::new(true),
            })),
        })
    }
    pub(crate) fn start_restore_journal(
        &self,
        journal: &crate::call_mode_restore_journal::Journal,
    ) -> Result<RestoreJournalAdmission, ()> {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        if self.refresh_locked() || self.0.restore_in_progress.load(Ordering::Acquire) {
            return Err(());
        }
        let proof = self.0.root_proof.as_ref().ok_or(())?;
        journal.validate_root(proof).map_err(|_| ())?;
        self.0.restore_in_progress.store(true, Ordering::Release);
        Ok(RestoreJournalAdmission {
            authority: self.0.clone(),
            journal: journal.clone(),
            completed: false,
        })
    }
    /// Explicit failure-path observation after an untracked begin/attach
    /// failure. Normal owned journals are deliberately absent from live probe.
    pub(crate) fn observe_unowned_restore_journal(&self) {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        let reason = match (&self.0.root_proof, &self.0.root_path) {
            (Some(proof), Some(root)) => match proof.with_root(root, |held| {
                nix::sys::stat::fstatat(
                    held,
                    crate::call_mode_restore_journal::ACTIVE_NAME,
                    nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
                )
            }) {
                Ok(Ok(_)) => Some(Reason::RestoreJournalPending),
                Ok(Err(nix::errno::Errno::ENOENT)) => None,
                Ok(Err(_)) => Some(Reason::RestoreJournalUnreadable),
                Err(_) => Some(Reason::LifetimeAuthorityChanged),
            },
            _ => Some(Reason::LifetimeAuthorityChanged),
        };
        if let Some(reason) = reason {
            let _ = self.0.revoked.set(Status {
                state: CODE,
                reason_codes: vec![reason],
                historical_recovery_confirmed: false,
            });
        }
    }
    #[cfg(test)]
    pub(crate) fn revoke_for_tests(&self) {
        let _admission = self.0.admission.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.0.revoked.set(Status {
            state: CODE,
            reason_codes: vec![Reason::LegacyExecutionPending],
            historical_recovery_confirmed: false,
        });
    }

    #[cfg(test)]
    pub(crate) fn quarantined_for_tests() -> Self {
        Self(Arc::new(Authority {
            startup: Some(Status {
                state: CODE,
                reason_codes: vec![Reason::LegacyCallModeLock],
                historical_recovery_confirmed: false,
            }),
            ..Authority::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::PathBuf;

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap()
                .join("build/runtime/p556-preflight")
                .join(crate::snapshot::new_instance_id());
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, body: &[u8]) {
            fs::write(self.0.join(name), body).unwrap();
        }
        fn reasons(&self) -> Vec<Reason> {
            Gate::inspect(&self.0)
                .status()
                .map_or_else(Vec::new, |s| s.reason_codes.clone())
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn live_gate(root: &Path) -> (crate::daemon_lifetime::Lease, Gate) {
        let lease = crate::daemon_lifetime::Lease::acquire(root).unwrap();
        let gate = Gate::inspect_with_lifetime(&lease);
        (lease, gate)
    }

    #[test]
    fn support_observation_does_not_probe_or_revoke_the_gate() {
        let root = Root::new();
        let (_lease, gate) = live_gate(&root.0);
        assert!(gate.observed_status().is_none());
        root.write(LEGACY_EXECUTION_PENDING, b"private evidence");
        // Filesystem changes are not permission for a diagnostic projection to
        // perform the live probe. The existing status path retains that job.
        assert!(gate.observed_status().is_none());
        assert!(!gate.is_quarantined());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::LegacyExecutionPending]
        );
        assert_eq!(
            gate.observed_status().unwrap().reason_codes,
            vec![Reason::LegacyExecutionPending]
        );
    }

    #[test]
    fn fresh_backup_cleanup_query_rejects_other_evidence_even_with_a_stale_open_gate() {
        use crate::startup_recovery::reconcile::backup_lifecycle::{
            BackupWriteKind, prepare_create, verified_fresh_backup_slot,
        };
        use std::os::unix::fs::PermissionsExt;
        for artifact in ["invalid_pristine", "unsafe_lock", "active_journal"] {
            let root = Root::new();
            let (lease, gate) = live_gate(&root.0);
            let proof = lease.root_proof();
            prepare_create(
                &root.0,
                &proof,
                "usb-imported-cleanup-query.json",
                BackupWriteKind::Import,
                b"{}",
            )
            .unwrap()
            .commit()
            .unwrap();
            assert!(gate.has_clean_fresh_backup_data(&root.0));
            match artifact {
                "invalid_pristine" => root.write(".call-mode-pristine-v1", b"invalid marker"),
                "unsafe_lock" => {
                    root.write("call-mode-operation.lock", b"");
                    fs::set_permissions(
                        root.0.join("call-mode-operation.lock"),
                        fs::Permissions::from_mode(0o666),
                    )
                    .unwrap();
                }
                "active_journal" => {
                    root.write(crate::call_mode_restore_journal::ACTIVE_NAME, b"unfinished")
                }
                _ => unreachable!(),
            }
            assert!(
                proof
                    .with_root(&root.0, verified_fresh_backup_slot)
                    .unwrap()
                    .unwrap()
                    .is_some(),
                "the negative must retain genuine fresh backup provenance"
            );
            assert!(
                !gate.is_quarantined() && !gate.is_admission_revoked(),
                "the negative must exercise a stale formerly open Gate"
            );
            assert!(
                !gate.has_clean_fresh_backup_data(&root.0),
                "backup provenance cannot waive another evidence slot"
            );
        }
    }

    #[test]
    fn fresh_backup_cleanup_query_requires_actual_lifetime_and_fresh_null_genesis() {
        use crate::startup_recovery::reconcile::backup_lifecycle::{
            BackupWriteKind, prepare_create,
        };
        let make_fresh = |root: &Root| {
            let (lease, gate) = live_gate(&root.0);
            prepare_create(
                &root.0,
                &lease.root_proof(),
                "usb-imported-cleanup-query.json",
                BackupWriteKind::Import,
                b"{}",
            )
            .unwrap()
            .commit()
            .unwrap();
            assert!(gate.has_clean_fresh_backup_data(&root.0));
            (lease, gate)
        };
        let root = Root::new();
        let (lease, gate) = make_fresh(&root);
        let foreign = Root::new();
        let (_foreign_lease, foreign_gate) = make_fresh(&foreign);
        assert!(!gate.has_clean_fresh_backup_data(&foreign.0));
        assert!(!foreign_gate.has_clean_fresh_backup_data(&root.0));
        assert!(!Gate::default().has_clean_fresh_backup_data(&root.0));
        assert!(
            !Gate::inspect(&root.0).has_clean_fresh_backup_data(&root.0),
            "a path-only reader cannot replace the actual lifetime proof"
        );
        gate.revoke_for_tests();
        assert!(!gate.has_clean_fresh_backup_data(&root.0));
        drop(lease);
        let ended = Root::new();
        let (ended_lease, ended_gate) = make_fresh(&ended);
        drop(ended_lease);
        assert!(!ended_gate.has_clean_fresh_backup_data(&ended.0));

        let legacy = Root::new();
        let (legacy_lease, legacy_gate) = live_gate(&legacy.0);
        fs::create_dir(legacy.0.join("module-backups")).unwrap();
        fs::write(
            legacy.0.join("module-backups/usb-imported-orphan.json"),
            b"{}",
        )
        .unwrap();
        prepare_create(
            &legacy.0,
            &legacy_lease.root_proof(),
            "usb-imported-known-new.json",
            BackupWriteKind::Import,
            b"{}",
        )
        .unwrap()
        .commit()
        .unwrap();
        assert!(!legacy_gate.is_quarantined() && !legacy_gate.is_admission_revoked());
        assert!(
            !legacy_gate.has_clean_fresh_backup_data(&legacy.0),
            "a completed transaction over a legacy baseline is not fresh provenance"
        );
    }

    async fn actual_restore_journal(
        root: &Root,
        lease: &crate::daemon_lifetime::Lease,
    ) -> crate::call_mode_restore_journal::Journal {
        use crate::module_radio_transport::{
            CallModeModuleIdentity, UsbComposition, VoiceConfiguration,
        };
        let usb = UsbComposition {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [1, 0, 1, 0, 1, 0, 0],
        };
        let voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let proof = lease.root_proof();
        let id = crate::call_mode_backups::create_native_backup(
            &root.0,
            CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: "admission fixture".into(),
            },
            usb,
            voice,
            false,
            &proof,
        )
        .await
        .unwrap();
        let captured = crate::call_mode_backups::capture_native_backup(&root.0, &id, &proof)
            .await
            .unwrap();
        crate::call_mode_restore_journal::Journal::begin(
            &root.0,
            &proof,
            &captured,
            crate::call_mode_restore_journal::Intent {
                producer_id: "finite-restore".into(),
                reservation_token: "original-reservation".into(),
                reservation_sequence: 1,
                operation_id: "original-restore".into(),
                module_imei: "123456789012345".into(),
                current_usb: usb,
                current_voice: voice,
                target_usb: usb,
                target_voice: Some(voice),
                recovery_binding: None,
                voice_recovery_binding: None,
            },
        )
        .await
        .unwrap()
    }
    fn complete_actual_equal_journal(journal: &crate::call_mode_restore_journal::Journal) {
        let mut receipt = crate::call_mode_restore_journal::Receipt {
            restored_at: String::new(),
            changed: false,
            module_imei: "123456789012345".into(),
            usb: crate::module_radio_transport::UsbComposition {
                vendor_id: 0x2c7c,
                product_id: 0x2400,
                flags: [1, 0, 1, 0, 1, 0, 0],
            },
            voice: Some(crate::module_radio_transport::VoiceConfiguration {
                ims: 1,
                volte_capability: 1,
                volte_disabled: 0,
            }),
        };
        // This is persistence/admission evidence for an equal-target journal,
        // not a physical readback fixture or a hardware restore claim.
        journal.target_verified(receipt.clone()).unwrap();
        journal.released().unwrap();
        receipt.restored_at = "2026-10-19T00:00:00Z".into();
        journal.finish_after_release(receipt).unwrap();
    }

    #[tokio::test]
    async fn tracked_restore_blocks_new_rights_until_actual_archive_without_self_revoking() {
        let root = Root::new();
        let (lease, gate) = live_gate(&root.0);
        let owner = gate.admit_operation("finite-restore").unwrap();
        let journal = actual_restore_journal(&root, &lease).await;
        let mut pending = gate.start_restore_journal(&journal).unwrap();
        assert!(!gate.is_admission_revoked() && !gate.is_quarantined());
        assert!(owner.right().is_active_for(&gate, "finite-restore"));
        assert!(gate.admit_operation("new-business").is_err());
        assert!(gate.start_restore_journal(&journal).is_err());
        assert!(
            pending.complete().is_err(),
            "an active intent is not an archived receipt"
        );
        assert!(gate.admit_operation("new-business").is_err());
        complete_actual_equal_journal(&journal);
        assert!(
            gate.admit_operation("new-business").is_err(),
            "archive alone cannot drop the guard's fence"
        );
        pending.complete().unwrap();
        drop(pending);
        assert!(!gate.is_admission_revoked());
        assert!(gate.admit_operation("new-business").is_ok());
        assert!(owner.right().is_active_for(&gate, "finite-restore"));
    }

    #[tokio::test]
    async fn failed_restore_drop_monotonically_denies_new_rights_but_keeps_exact_cleanup_right() {
        let root = Root::new();
        let (lease, gate) = live_gate(&root.0);
        let owner = gate.admit_operation("finite-restore").unwrap();
        let right = owner.right();
        let journal = actual_restore_journal(&root, &lease).await;
        let pending = gate.start_restore_journal(&journal).unwrap();
        drop(pending);
        assert!(gate.is_admission_revoked() && !gate.is_quarantined());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::RestoreJournalPending]
        );
        assert!(gate.admit_operation("new-business").is_err());
        assert!(right.is_active_for(&gate, "finite-restore"));
        fs::rename(
            root.0.join(crate::call_mode_restore_journal::ACTIVE_NAME),
            root.0.join("retained-original-journal"),
        )
        .unwrap();
        assert!(
            gate.admit_operation("new-business").is_err(),
            "removing a name cannot reverse failed-owner revocation"
        );
        assert!(right.is_active_for(&gate, "finite-restore"));
        drop(owner);
        assert!(!right.is_active_for(&gate, "finite-restore"));
    }

    #[tokio::test]
    async fn foreign_journal_or_ended_root_never_reopens_admission() {
        let root = Root::new();
        let (lease, gate) = live_gate(&root.0);
        let foreign = Root::new();
        let (foreign_lease, foreign_gate) = live_gate(&foreign.0);
        let foreign_journal = actual_restore_journal(&foreign, &foreign_lease).await;
        assert!(gate.start_restore_journal(&foreign_journal).is_err());
        assert!(
            Gate::default()
                .start_restore_journal(&foreign_journal)
                .is_err()
        );
        assert!(gate.admit_operation("ordinary").is_ok());
        assert!(!foreign_gate.is_admission_revoked());
        let journal = actual_restore_journal(&root, &lease).await;
        let mut pending = gate.start_restore_journal(&journal).unwrap();
        complete_actual_equal_journal(&journal);
        drop(lease);
        assert!(
            pending.complete().is_err(),
            "a completed archive cannot replace an ended root authority"
        );
        drop(pending);
        assert!(gate.admit_operation("ordinary").is_err());
    }

    #[tokio::test]
    async fn untracked_real_intent_is_observed_before_release_without_replaying_or_losing_cleanup()
    {
        let root = Root::new();
        let (lease, gate) = live_gate(&root.0);
        gate.observe_unowned_restore_journal();
        assert!(gate.admit_operation("ordinary").is_ok());
        let cleanup = gate.admit_operation("finite-restore").unwrap();
        let _journal = actual_restore_journal(&root, &lease).await;
        // Models Intent persisted but begin/attach never installed a guard.
        gate.observe_unowned_restore_journal();
        assert!(gate.is_admission_revoked() && !gate.is_quarantined());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::RestoreJournalPending]
        );
        assert!(gate.admit_operation("ordinary").is_err());
        assert!(cleanup.right().is_active_for(&gate, "finite-restore"));
        assert!(
            root.0
                .join(crate::call_mode_restore_journal::ACTIVE_NAME)
                .exists()
        );
    }

    #[test]
    fn untracked_observer_rejects_changed_root_without_adopting_its_absent_journal() {
        let root = Root::new();
        let (_lease, gate) = live_gate(&root.0);
        let held = root.0.with_extension("retained");
        fs::rename(&root.0, &held).unwrap();
        fs::create_dir(&root.0).unwrap();
        gate.observe_unowned_restore_journal();
        assert!(gate.is_admission_revoked());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::LifetimeAuthorityChanged]
        );
        fs::remove_dir(&root.0).unwrap();
        fs::rename(&held, &root.0).unwrap();
        assert!(gate.admit_operation("ordinary").is_err());
    }

    #[test]
    fn live_root_proof_requires_the_actual_owner_and_cannot_adopt_another_root() {
        let root = Root::new();
        let foreign = Root::new();
        let (lease, gate) = live_gate(&root.0);
        let proof = gate.root_proof_for(&root.0).unwrap();
        assert!(gate.root_proof_for(&foreign.0).is_err());
        assert!(Gate::default().root_proof_for(&root.0).is_err());
        assert!(!gate.is_admission_revoked());
        drop(lease);
        assert!(proof.with_root(&root.0, |_| ()).is_err());
        assert!(gate.root_proof_for(&root.0).is_err());
        assert!(gate.is_admission_revoked());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::LifetimeAuthorityChanged]
        );
        let replacement = crate::daemon_lifetime::Lease::acquire(&root.0).unwrap();
        assert!(!proof.same_authority(&replacement.root_proof()));
        assert!(gate.root_proof_for(&root.0).is_err());
    }

    #[test]
    fn filesystem_root_proof_never_grants_startup_recovery_or_normal_operation_rights() {
        let root = Root::new();
        root.write(LEGACY_EXECUTION_PENDING, b"preserved legacy evidence");
        let (_lease, gate) = live_gate(&root.0);
        let original = fs::read(root.0.join(LEGACY_EXECUTION_PENDING)).unwrap();
        gate.root_proof_for(&root.0)
            .unwrap()
            .with_root(&root.0, |_| ())
            .unwrap();
        assert!(gate.is_quarantined() && gate.is_admission_revoked());
        assert!(gate.admit_operation("ordinary-producer").is_err());
        assert!(!gate.status().unwrap().historical_recovery_confirmed);
        assert_eq!(
            fs::read(root.0.join(LEGACY_EXECUTION_PENDING)).unwrap(),
            original
        );
        fs::remove_file(root.0.join(LEGACY_EXECUTION_PENDING)).unwrap();
        assert!(gate.is_quarantined() && gate.is_admission_revoked());
        assert!(gate.admit_operation("ordinary-producer").is_err());
    }

    #[test]
    fn physical_read_lineage_requires_same_quarantined_gate_and_actual_lifetime() {
        let ordinary = Root::new();
        let (_ordinary_lease, ordinary_gate) = live_gate(&ordinary.0);
        assert!(!ordinary_gate.is_quarantined());
        assert!(
            ordinary_gate
                .capture_physical_read_lineage(&ordinary.0)
                .is_err()
        );
        let root = Root::new();
        let foreign = Root::new();
        root.write(LEGACY_EXECUTION_PENDING, b"retained unknown execution");
        let (_lease, gate) = live_gate(&root.0);
        let original = fs::read(root.0.join(LEGACY_EXECUTION_PENDING)).unwrap();
        assert!(
            Gate::default()
                .capture_physical_read_lineage(&root.0)
                .is_err()
        );
        assert!(
            Gate::inspect(&root.0)
                .capture_physical_read_lineage(&root.0)
                .is_err()
        );
        assert!(gate.capture_physical_read_lineage(&foreign.0).is_err());
        let lineage = gate.capture_physical_read_lineage(&root.0).unwrap();
        lineage.validate(&gate.clone()).unwrap();
        let same_root_different_gate = Gate::inspect_with_lifetime(&_lease);
        assert!(lineage.validate(&same_root_different_gate).is_err());
        assert!(gate.admit_operation("ordinary").is_err());
        assert!(gate.is_quarantined() && gate.is_admission_revoked());
        assert!(!gate.status().unwrap().historical_recovery_confirmed);
        assert_eq!(
            fs::read(root.0.join(LEGACY_EXECUTION_PENDING)).unwrap(),
            original
        );
    }

    #[test]
    fn physical_read_lineage_cannot_outlive_or_rebind_its_root_owner() {
        let root = Root::new();
        root.write(LEGACY_EXECUTION_PENDING, b"retained unknown execution");
        let (lease, gate) = live_gate(&root.0);
        let lineage = gate.capture_physical_read_lineage(&root.0).unwrap();
        let held = root.0.with_extension("physical-read-retained");
        fs::rename(&root.0, &held).unwrap();
        fs::create_dir(&root.0).unwrap();
        assert!(lineage.validate(&gate).is_err());
        fs::remove_dir(&root.0).unwrap();
        fs::rename(&held, &root.0).unwrap();
        lineage.validate(&gate).unwrap();
        drop(lease);
        assert!(lineage.validate(&gate).is_err());
        let next = crate::daemon_lifetime::Lease::acquire(&root.0).unwrap();
        let next_gate = Gate::inspect_with_lifetime(&next);
        assert!(lineage.validate(&gate).is_err());
        assert!(lineage.validate(&next_gate).is_err());
        drop(gate);
        assert!(lineage.authority.upgrade().is_none());
        assert!(lineage.validate(&next_gate).is_err());
    }

    #[test]
    fn pristine_marker_cannot_legitimize_unsafe_existing_legacy_lock_permissions() {
        use std::os::unix::fs::PermissionsExt;
        for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
            for mode in [0o622, 0o666, 0o700, 0o1600, 0o2600, 0o4600] {
                let root = Root::new();
                root.write(".call-mode-pristine-v1", b"");
                root.write(name, b"");
                fs::set_permissions(root.0.join(name), fs::Permissions::from_mode(mode)).unwrap();
                let before = fs::symlink_metadata(root.0.join(name)).unwrap();
                let observed = before.mode() & 0o7777;
                assert!(
                    observed == mode || observed == mode & !0o6000,
                    "requested {mode:o}, unexpected effective mode {observed:o}"
                );
                let gate = Gate::inspect(&root.0);
                if observed & 0o7133 == 0 {
                    assert!(
                        !gate.is_quarantined(),
                        "safe effective {name} mode {observed:o}"
                    );
                    assert!(!gate.is_admission_revoked());
                    assert!(gate.status().is_none());
                    drop(gate.admit_operation("effective-safe-mode-fixture").unwrap());
                } else {
                    assert!(
                        gate.is_quarantined(),
                        "unsafe {name} effective mode {observed:o}"
                    );
                    assert!(gate.is_admission_revoked());
                    assert!(gate.admit_operation("unsafe-mode-fixture").is_err());
                    assert!(!gate.status().unwrap().historical_recovery_confirmed);
                }
                let after = fs::symlink_metadata(root.0.join(name)).unwrap();
                assert_eq!(
                    (
                        before.ino(),
                        before.mode(),
                        before.ctime(),
                        before.ctime_nsec()
                    ),
                    (after.ino(), after.mode(), after.ctime(), after.ctime_nsec())
                );
            }
        }
    }

    #[test]
    fn live_pending_is_shared_and_monotonic_without_changing_startup_cleanup_branch() {
        let root = Root::new();
        let (_lease, gate) = live_gate(&root.0);
        let clone = gate.clone();
        let accepted = gate.admit_operation("producer").unwrap();
        root.write(LEGACY_EXECUTION_PENDING, b"not parsed");
        assert!(clone.is_admission_revoked());
        fs::remove_file(root.0.join(LEGACY_EXECUTION_PENDING)).unwrap();
        assert!(gate.is_admission_revoked());
        assert!(!gate.is_quarantined());
        assert!(accepted.right().is_active_for(&clone, "producer"));
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::LegacyExecutionPending]
        );
        assert!(gate.admit_operation("producer").is_err());
    }

    #[test]
    fn own_active_journal_does_not_self_revoke_but_is_quarantined_at_restart() {
        let root = Root::new();
        let (lease, gate) = live_gate(&root.0);
        let accepted = gate.admit_operation("finite-restore").unwrap();
        root.write(
            crate::call_mode_restore_journal::ACTIVE_NAME,
            b"unfinished active journal",
        );
        assert!(!gate.is_admission_revoked() && !gate.is_quarantined());
        assert!(accepted.right().is_active_for(&gate, "finite-restore"));
        assert!(gate.admit_operation("another-ordinary-operation").is_ok());
        drop(accepted);
        drop(lease);
        let replacement = crate::daemon_lifetime::Lease::acquire(&root.0).unwrap();
        let restarted = Gate::inspect_with_lifetime(&replacement);
        assert!(restarted.is_quarantined() && restarted.is_admission_revoked());
        assert!(
            restarted
                .status()
                .unwrap()
                .reason_codes
                .contains(&Reason::RestoreJournalPending)
        );
        assert!(restarted.admit_operation("ordinary").is_err());
        assert_eq!(
            fs::read(root.0.join(crate::call_mode_restore_journal::ACTIVE_NAME)).unwrap(),
            b"unfinished active journal"
        );
    }

    #[test]
    fn operation_right_cannot_be_reused_by_foreign_authority_or_after_real_owner_ends() {
        let gate = Gate::default();
        let owner = gate.admit_operation("producer").unwrap();
        let right = owner.right();
        assert!(!right.is_active_for(&Gate::default(), "producer"));
        assert!(!right.is_active_for(&gate, "replacement"));
        gate.revoke_for_tests();
        assert!(right.is_active_for(&gate, "producer"));
        drop(owner);
        assert!(!right.is_active_for(&gate, "producer"));
        assert!(!right.clone().is_active_for(&gate, "producer"));
    }

    #[test]
    fn live_probe_rejects_replaced_root_even_when_new_namespace_has_no_pending_entry() {
        let root = Root::new();
        let path = root.0.join("data");
        let (_lease, gate) = live_gate(&path);
        fs::rename(&path, root.0.join("retained")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(gate.is_admission_revoked());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::LifetimeAuthorityChanged]
        );
        assert!(!gate.is_quarantined());
    }

    #[test]
    fn live_probe_rejects_lifetime_lock_replacement_and_does_not_self_revoke_on_pristine_changes() {
        let root = Root::new();
        let (_lease, gate) = live_gate(&root.0);
        root.write(".call-mode-pristine-v1", b"");
        crate::call_mode_lifecycle::invalidate_pristine_marker(&root.0).unwrap();
        assert!(!gate.is_admission_revoked());
        fs::rename(
            root.0.join(".switchd-lifetime.lock"),
            root.0.join("saved-lock"),
        )
        .unwrap();
        root.write(".switchd-lifetime.lock", b"");
        assert!(gate.is_admission_revoked());
        assert_eq!(
            gate.status().unwrap().reason_codes,
            vec![Reason::LifetimeAuthorityChanged]
        );
    }

    #[test]
    fn live_probe_counts_pending_links_as_evidence_without_reading_the_target() {
        let root = Root::new();
        let (_lease, gate) = live_gate(&root.0);
        let target = root.0.join("private-target");
        fs::write(&target, b"unchanged").unwrap();
        symlink(&target, root.0.join(LEGACY_EXECUTION_PENDING)).unwrap();
        assert!(gate.is_admission_revoked());
        assert_eq!(fs::read(target).unwrap(), b"unchanged");
        assert!(
            fs::symlink_metadata(root.0.join(LEGACY_EXECUTION_PENDING))
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn all_clones_refuse_operations_after_revocation_linearization() {
        let gate = Gate::default();
        let owner = gate.admit_operation("producer").unwrap();
        let clones: Vec<_> = (0..8).map(|_| gate.clone()).collect();
        gate.revoke_for_tests();
        let threads: Vec<_> = clones
            .into_iter()
            .map(|clone| {
                std::thread::spawn(move || {
                    for _ in 0..32 {
                        assert!(clone.admit_operation("producer").is_err());
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(owner.right().is_active_for(&gate, "producer"));
    }

    #[test]
    fn missing_and_clean_roots_do_not_create_a_pristine_marker() {
        let root = Root::new();
        assert!(!Gate::inspect(&root.0).is_quarantined());
        assert!(!Gate::inspect(&root.0.join("absent")).is_quarantined());
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[test]
    fn either_empty_legacy_lock_quarantines_without_becoming_clean_proof() {
        for (name, reason) in [
            ("call-mode-operation.lock", Reason::LegacyCallModeLock),
            (".module-adb-interface.lock", Reason::LegacyAdbInterfaceLock),
        ] {
            let root = Root::new();
            root.write(name, b"");
            assert_eq!(root.reasons(), vec![reason]);
            let gate = Gate::inspect(&root.0);
            let status = gate.status().unwrap();
            assert_eq!(status.state, CODE);
            assert!(!status.historical_recovery_confirmed);
            assert!(!root.0.join(".call-mode-pristine-v1").exists());
        }
    }

    #[test]
    fn valid_pristine_read_only_locks_remain_usable() {
        let root = Root::new();
        for name in [
            ".call-mode-pristine-v1",
            "call-mode-operation.lock",
            ".module-adb-interface.lock",
        ] {
            root.write(name, b"");
        }
        assert!(!Gate::inspect(&root.0).is_quarantined());
    }

    #[test]
    fn legacy_pending_of_any_shape_overrides_pristine_without_rewriting_evidence() {
        use std::os::unix::fs::PermissionsExt;
        for kind in [
            "valid",
            "invalid",
            "empty",
            "unreadable",
            "directory",
            "symlink",
            "dangling",
            "hardlink",
        ] {
            let root = Root::new();
            root.write(".call-mode-pristine-v1", b"");
            root.write("call-mode-operation.lock", b"");
            root.write(".module-adb-interface.lock", b"");
            let pending = root.0.join(LEGACY_EXECUTION_PENDING);
            match kind {
                "valid" => root.write(
                    LEGACY_EXECUTION_PENDING,
                    br#"{"schema":"legacy_execution_pending_v1","phase":"spawn_pending"}"#,
                ),
                "invalid" => root.write(LEGACY_EXECUTION_PENDING, b"invalid"),
                "empty" => root.write(LEGACY_EXECUTION_PENDING, b""),
                "unreadable" => {
                    root.write(LEGACY_EXECUTION_PENDING, b"unknown");
                    fs::set_permissions(&pending, fs::Permissions::from_mode(0o000)).unwrap();
                }
                "directory" => fs::create_dir(&pending).unwrap(),
                "symlink" | "dangling" | "hardlink" => {
                    root.write("pending-target", b"preserve-target");
                    if kind == "hardlink" {
                        fs::hard_link(root.0.join("pending-target"), &pending).unwrap();
                    } else {
                        symlink(
                            if kind == "dangling" {
                                "absent"
                            } else {
                                "pending-target"
                            },
                            &pending,
                        )
                        .unwrap();
                    }
                }
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&pending).unwrap();
            let gate = Gate::inspect(&root.0);
            let status = gate.status().unwrap();
            assert_eq!(
                status.reason_codes,
                vec![Reason::LegacyExecutionPending],
                "{kind}"
            );
            assert!(!status.historical_recovery_confirmed);
            let after = fs::symlink_metadata(&pending).unwrap();
            assert_eq!(
                (before.ino(), before.len(), before.mtime(), before.ctime()),
                (after.ino(), after.len(), after.mtime(), after.ctime()),
                "{kind}"
            );
            assert_eq!(
                fs::read(root.0.join(".call-mode-pristine-v1")).unwrap(),
                b""
            );
        }
    }

    #[test]
    fn unreadable_pending_lookup_is_not_absent_evidence() {
        let root = Root::new();
        root.write("not-a-directory", b"unknown");
        let gate = Gate::inspect(&root.0.join("not-a-directory"));
        assert!(gate.is_quarantined());
        assert!(
            gate.status()
                .unwrap()
                .reason_codes
                .contains(&Reason::UnreadableEvidence)
        );
        assert!(!gate.status().unwrap().historical_recovery_confirmed);
    }

    #[test]
    fn module_backups_override_even_valid_pristine_evidence() {
        let root = Root::new();
        root.write(".call-mode-pristine-v1", b"");
        fs::create_dir(root.0.join("module-backups")).unwrap();
        assert_eq!(root.reasons(), vec![Reason::ModuleBackupPresent]);
    }

    #[test]
    fn unknown_voice_runtime_quarantines_even_with_pristine_marker() {
        let root = Root::new();
        fs::create_dir(root.0.join("voice-runtime")).unwrap();
        assert_eq!(root.reasons(), vec![Reason::LegacyVoiceRuntime]);
        root.write(".call-mode-pristine-v1", b"");
        assert_eq!(root.reasons(), vec![Reason::LegacyVoiceRuntime]);
    }

    #[test]
    fn managed_control_store_is_startup_evidence_even_without_runtime_or_locks() {
        for shape in ["empty", "directory", "symlink", "hardlink", "fifo"] {
            let root = Root::new();
            root.write(".call-mode-pristine-v1", b"");
            let path = root.0.join("voice-control");
            match shape {
                "empty" => root.write("voice-control", b""),
                "directory" => fs::create_dir(&path).unwrap(),
                "symlink" => symlink("absent", &path).unwrap(),
                "hardlink" => fs::hard_link(root.0.join(".call-mode-pristine-v1"), &path).unwrap(),
                "fifo" => {
                    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600))
                        .unwrap()
                }
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&path).unwrap();
            let gate = Gate::inspect(&root.0);
            assert!(gate.is_quarantined(), "{shape}");
            assert!(
                gate.status()
                    .unwrap()
                    .reason_codes
                    .contains(&Reason::ManagedVoiceControlPending)
            );
            assert!(gate.admit_operation("ordinary").is_err());
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                (before.ino(), before.mode(), before.len()),
                (after.ino(), after.mode(), after.len()),
                "{shape}"
            );
        }
    }

    #[test]
    fn own_managed_control_record_does_not_live_revoke_but_is_pending_at_restart() {
        let root = Root::new();
        let (lease, gate) = live_gate(&root.0);
        let accepted = gate.admit_operation("managed-voice").unwrap();
        fs::create_dir(root.0.join("voice-control")).unwrap();
        root.write("voice-control/session.json", b"private session evidence");
        assert!(!gate.is_admission_revoked() && !gate.is_quarantined());
        assert!(accepted.right().is_active_for(&gate, "managed-voice"));
        drop(accepted);
        drop(lease);
        let next = crate::daemon_lifetime::Lease::acquire(&root.0).unwrap();
        let restarted = Gate::inspect_with_lifetime(&next);
        assert!(restarted.is_quarantined());
        assert!(
            restarted
                .status()
                .unwrap()
                .reason_codes
                .contains(&Reason::ManagedVoiceControlPending)
        );
    }

    #[test]
    fn pristine_never_authorizes_nonempty_directory_symlink_or_hardlinked_locks() {
        for (name, reason) in [
            ("call-mode-operation.lock", Reason::LegacyCallModeLock),
            (".module-adb-interface.lock", Reason::LegacyAdbInterfaceLock),
        ] {
            for kind in ["nonempty", "directory", "symlink", "hardlink"] {
                let root = Root::new();
                root.write(".call-mode-pristine-v1", b"");
                match kind {
                    "nonempty" => root.write(name, b"unknown-work"),
                    "directory" => fs::create_dir(root.0.join(name)).unwrap(),
                    "symlink" => {
                        root.write("target", b"");
                        symlink("target", root.0.join(name)).unwrap();
                    }
                    "hardlink" => {
                        root.write("target", b"");
                        fs::hard_link(root.0.join("target"), root.0.join(name)).unwrap();
                    }
                    _ => unreachable!(),
                }
                let before = fs::symlink_metadata(root.0.join(name)).unwrap();
                assert_eq!(root.reasons(), vec![reason], "{name}: {kind}");
                let after = fs::symlink_metadata(root.0.join(name)).unwrap();
                assert_eq!(
                    (
                        before.ino(),
                        before.len(),
                        before.nlink(),
                        before.mtime(),
                        before.ctime()
                    ),
                    (
                        after.ino(),
                        after.len(),
                        after.nlink(),
                        after.mtime(),
                        after.ctime()
                    )
                );
            }
        }
    }

    #[test]
    fn pristine_never_authorizes_unknown_voice_runtime_shapes() {
        for kind in [
            "nonempty-file",
            "directory",
            "symlink",
            "hardlink",
            "unknown-version",
            "unknown-current-files",
        ] {
            let root = Root::new();
            root.write(".call-mode-pristine-v1", b"");
            match kind {
                "nonempty-file" => root.write("voice-runtime", b"unknown"),
                "directory" => fs::create_dir(root.0.join("voice-runtime")).unwrap(),
                "symlink" => {
                    fs::create_dir(root.0.join("target")).unwrap();
                    symlink("target", root.0.join("voice-runtime")).unwrap();
                }
                "hardlink" => {
                    root.write("target", b"");
                    fs::hard_link(root.0.join("target"), root.0.join("voice-runtime")).unwrap();
                }
                "unknown-version" => {
                    fs::create_dir_all(root.0.join("voice-runtime/old-runtime")).unwrap()
                }
                "unknown-current-files" => {
                    let version = root
                        .0
                        .join("voice-runtime")
                        .join(&crate::call_mode_runtime_manifest::manifest().version);
                    fs::create_dir_all(&version).unwrap();
                    fs::write(version.join("unexpected"), b"unknown").unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(root.reasons(), vec![Reason::LegacyVoiceRuntime], "{kind}");
        }
    }

    fn synthetic_download() -> (
        Root,
        Vec<crate::call_mode_runtime_manifest::Artifact>,
        PathBuf,
    ) {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let root = Root::new();
        for name in [
            ".call-mode-pristine-v1",
            "call-mode-operation.lock",
            ".module-adb-interface.lock",
        ] {
            root.write(name, b"");
        }
        let directory = root
            .0
            .join("voice-runtime")
            .join(&crate::call_mode_runtime_manifest::manifest().version);
        fs::create_dir_all(&directory).unwrap();
        let bytes = b"synthetic verified download";
        let artifact = crate::call_mode_runtime_manifest::Artifact {
            name: "synthetic-runtime".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            mode: 0o644,
            deploy: true,
        };
        fs::write(directory.join(&artifact.name), bytes).unwrap();
        fs::set_permissions(
            directory.join(&artifact.name),
            fs::Permissions::from_mode(artifact.mode),
        )
        .unwrap();
        (root, vec![artifact], directory)
    }

    #[test]
    fn pristine_allows_only_complete_current_hash_verified_download_with_safe_read_only_locks() {
        let (root, artifacts, _) = synthetic_download();
        assert!(!Gate::inspect_with_runtime(&root.0, &artifacts).is_quarantined());
        root.write("voice-runtime/.download.lock", b"");
        assert!(!Gate::inspect_with_runtime(&root.0, &artifacts).is_quarantined());
        // Production still requires the actual embedded manifest, not our
        // synthetic artifact list. This fixture cannot bless arbitrary files.
        assert_eq!(root.reasons(), vec![Reason::LegacyVoiceRuntime]);
    }

    #[test]
    fn pristine_download_rejects_partial_extra_corrupt_linked_and_wrong_mode_evidence() {
        use std::os::unix::fs::PermissionsExt;
        for kind in [
            "missing",
            "corrupt",
            "extra-file",
            "extra-directory",
            "old-version",
            "symlink",
            "hardlink",
            "directory",
            "mode",
            "nonempty-download-lock",
            "symlink-download-lock",
            "hardlink-download-lock",
            "version-symlink",
        ] {
            let (root, artifacts, directory) = synthetic_download();
            let artifact = directory.join(&artifacts[0].name);
            match kind {
                "missing" => fs::remove_file(&artifact).unwrap(),
                "corrupt" => fs::write(&artifact, vec![b'x'; artifacts[0].size as usize]).unwrap(),
                "extra-file" => fs::write(directory.join("unexpected"), b"unknown").unwrap(),
                "extra-directory" => fs::create_dir(directory.join("unexpected")).unwrap(),
                "old-version" => fs::create_dir(root.0.join("voice-runtime/old-version")).unwrap(),
                "symlink" | "hardlink" => {
                    let target = root.0.join("target");
                    fs::rename(&artifact, &target).unwrap();
                    if kind == "symlink" {
                        symlink(target, &artifact).unwrap();
                    } else {
                        fs::hard_link(target, &artifact).unwrap();
                    }
                }
                "directory" => {
                    fs::remove_file(&artifact).unwrap();
                    fs::create_dir(&artifact).unwrap();
                }
                "mode" => {
                    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o600)).unwrap()
                }
                "nonempty-download-lock" => root.write("voice-runtime/.download.lock", b"unknown"),
                "symlink-download-lock" | "hardlink-download-lock" => {
                    root.write("target", b"");
                    let lock = root.0.join("voice-runtime/.download.lock");
                    if kind == "symlink-download-lock" {
                        symlink(root.0.join("target"), lock).unwrap();
                    } else {
                        fs::hard_link(root.0.join("target"), lock).unwrap();
                    }
                }
                "version-symlink" => {
                    fs::rename(&directory, root.0.join("target")).unwrap();
                    symlink(root.0.join("target"), &directory).unwrap();
                }
                _ => unreachable!(),
            }
            let gate = Gate::inspect_with_runtime(&root.0, &artifacts);
            assert_eq!(
                gate.status().unwrap().reason_codes,
                vec![Reason::LegacyVoiceRuntime],
                "{kind}"
            );
        }
    }

    #[test]
    fn malformed_directory_symlink_and_hardlink_markers_never_authorize_startup() {
        for kind in ["nonempty", "directory", "symlink", "hardlink"] {
            let root = Root::new();
            match kind {
                "nonempty" => root.write(".call-mode-pristine-v1", b"not-proof"),
                "directory" => fs::create_dir(root.0.join(".call-mode-pristine-v1")).unwrap(),
                "symlink" => {
                    root.write("target", b"");
                    symlink("target", root.0.join(".call-mode-pristine-v1")).unwrap();
                }
                "hardlink" => {
                    root.write("target", b"");
                    fs::hard_link(root.0.join("target"), root.0.join(".call-mode-pristine-v1"))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(
                root.reasons(),
                vec![Reason::InvalidPristineMarker],
                "{kind}"
            );
        }
    }

    #[test]
    fn evidence_metadata_errors_fail_closed() {
        let root = Root::new();
        root.write("not-directory", b"synthetic");
        let gate = Gate::inspect(&root.0.join("not-directory/child"));
        assert!(gate.is_quarantined());
        assert!(
            gate.status()
                .unwrap()
                .reason_codes
                .contains(&Reason::UnreadableEvidence)
        );
    }

    #[test]
    fn preflight_preserves_all_evidence_bytes_and_metadata() {
        let root = Root::new();
        for name in ["call-mode-operation.lock", ".module-adb-interface.lock"] {
            root.write(name, b"synthetic-evidence");
        }
        let before: Vec<_> = fs::read_dir(&root.0)
            .unwrap()
            .map(|p| {
                let p = p.unwrap().path();
                let s = p.symlink_metadata().unwrap();
                (
                    p.clone(),
                    fs::read(&p).unwrap(),
                    s.ino(),
                    s.mtime_nsec(),
                    s.ctime_nsec(),
                    s.nlink(),
                )
            })
            .collect();
        let gate = Gate::inspect(&root.0);
        assert!(gate.is_quarantined());
        for (path, bytes, ino, mtime, ctime, links) in before {
            let s = path.symlink_metadata().unwrap();
            assert_eq!(
                (
                    fs::read(&path).unwrap(),
                    s.ino(),
                    s.mtime_nsec(),
                    s.ctime_nsec(),
                    s.nlink()
                ),
                (bytes, ino, mtime, ctime, links)
            );
        }
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
    }

    #[test]
    fn startup_decision_is_immutable_when_files_later_change() {
        let root = Root::new();
        root.write("call-mode-operation.lock", b"");
        let gate = Gate::inspect(&root.0);
        fs::remove_file(root.0.join("call-mode-operation.lock")).unwrap();
        root.write(".call-mode-pristine-v1", b"");
        assert!(gate.is_quarantined());
        assert!(!Gate::inspect(&root.0).is_quarantined());
    }

    #[test]
    fn admitted_native_lifetime_does_not_become_quarantined_after_marker_revocation() {
        let root = Root::new();
        root.write(".call-mode-pristine-v1", b"");
        let gate = Gate::inspect(&root.0);
        fs::remove_file(root.0.join(".call-mode-pristine-v1")).unwrap();
        root.write("call-mode-operation.lock", b"");
        assert!(
            !gate.is_quarantined(),
            "runtime mutation must retain normal cleanup fences"
        );
        assert!(
            Gate::inspect(&root.0).is_quarantined(),
            "next startup must reconcile that evidence"
        );
    }
}
