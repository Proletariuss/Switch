//! Read-only startup evidence, anchored to one actual held root directory.
use super::{LEGACY_EXECUTION_PENDING, Reason};
use crate::call_mode_runtime_manifest::{Artifact, MAX_ARTIFACT_BYTES, manifest};
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{FileStat, Mode, fstatat};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

const PRISTINE: &str = ".call-mode-pristine-v1";
const RUNTIME: &str = "voice-runtime";
const DOWNLOAD_LOCK: &str = ".download.lock";
const NAMES: [&str; 8] = [
    LEGACY_EXECUTION_PENDING,
    PRISTINE,
    "call-mode-operation.lock",
    ".module-adb-interface.lock",
    "module-backups",
    RUNTIME,
    crate::call_mode_restore_journal::ACTIVE_NAME,
    "voice-control",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: i128,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Stamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            size: i128::from(metadata.len()),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    fn from_stat(metadata: &FileStat) -> Self {
        Self {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
            mode: metadata.st_mode as u32,
            uid: metadata.st_uid,
            gid: metadata.st_gid,
            links: metadata.st_nlink as u64,
            size: i128::from(metadata.st_size),
            mtime: (metadata.st_mtime as i64, metadata.st_mtime_nsec as i64),
            ctime: (metadata.st_ctime as i64, metadata.st_ctime_nsec as i64),
        }
    }
    fn of(file: &File) -> io::Result<Self> {
        file.metadata()
            .map(|metadata| Self::from_metadata(&metadata))
    }
    fn kind(self, kind: u32) -> bool {
        self.mode & nix::libc::S_IFMT as u32 == kind
    }
    fn regular(self) -> bool {
        self.kind(nix::libc::S_IFREG as u32)
    }
    fn directory(self) -> bool {
        self.kind(nix::libc::S_IFDIR as u32)
    }
    fn marker(self) -> bool {
        // Keep the original marker format: no payload or new mode/uid rule.
        self.regular() && self.size == 0 && self.links == 1
    }
    fn safe_empty_lock(self) -> bool {
        self.marker() && self.uid == nix::unistd::geteuid().as_raw() && self.mode & 0o7133 == 0
    }
    fn artifact(self, artifact: &Artifact) -> bool {
        self.regular()
            && self.links == 1
            && self.size == i128::from(artifact.size)
            && self.mode & 0o7777 == artifact.mode
    }
}

type Entry = Result<Option<Stamp>, ()>;
type Snapshot = [Entry; NAMES.len()];

fn entry(directory: &File, name: &OsStr) -> Entry {
    match fstatat(directory, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(metadata) => Ok(Some(Stamp::from_stat(&metadata))),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(_) => Err(()),
    }
}
fn snapshot(directory: &File) -> Snapshot {
    NAMES.map(|name| entry(directory, OsStr::new(name)))
}
fn present(entry: Entry) -> bool {
    matches!(entry, Ok(Some(_)))
}
fn add(reasons: &mut Vec<Reason>, reason: Reason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

// Hooks only arrange actual filesystem interleavings in the same reader.
// They never supply metadata, bytes, a proof, or a successful verifier result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    RootCaptured,
    EvidenceCaptured,
    RuntimePinned,
    NamesCaptured,
    ArtifactPinned(usize),
    ArtifactRead(usize),
    BeforeFinalCheck,
}

pub(super) fn inspect(directory: &File, artifacts: &[Artifact]) -> Vec<Reason> {
    inspect_with_hook(directory, artifacts, |_| {})
}

fn inspect_with_hook(
    directory: &File,
    artifacts: &[Artifact],
    mut hook: impl FnMut(Phase),
) -> Vec<Reason> {
    let Ok(root_before) = Stamp::of(directory) else {
        return vec![Reason::UnreadableEvidence];
    };
    if !root_before.directory() {
        return vec![Reason::UnreadableEvidence];
    }
    hook(Phase::RootCaptured);
    let before = snapshot(directory);
    let legacy_locks = super::legacy_locks::covered(directory);
    let legacy_namespace = entry(directory, OsStr::new(super::legacy_locks::DIRECTORY));
    let reconciliation_before = entry(directory, OsStr::new("module-recovery-v1"));
    // The new ledger is a separate bounded mutable namespace. It never joins
    // the original eight-slot immutable tree or turns old bytes into permits.
    let reconciliation = crate::startup_recovery::reconcile::ledger::inspect_startup(directory);
    // A fresh data-only lifecycle can explain exactly the backup slot. It
    // cannot settle a historical physical attempt, a lock or any other slot.
    let fresh_backups = if matches!(
        &reconciliation,
        Ok(crate::startup_recovery::reconcile::ledger::RecoveryClassification::Absent)
    ) {
        crate::startup_recovery::reconcile::backup_lifecycle::verified_fresh_backup_slot(directory)
    } else {
        Ok(None)
    };
    hook(Phase::EvidenceCaptured);
    // Presence alone is unfinished-restore evidence. A bounded diagnostic
    // read can add an unreadable reason, never establish permission to replay
    // or remove evidence. Archived receipts do not certify legacy history.
    let journal_entry = entry(
        directory,
        OsStr::new(crate::call_mode_restore_journal::ACTIVE_NAME),
    );
    let journal = if before[6] == Ok(None) && journal_entry == Ok(None) {
        // An absent journal must not impose the writer's root-mode policy on
        // unrelated historical evidence formats.
        Ok(None)
    } else {
        crate::call_mode_restore_journal::diagnostic_summary(directory)
    };
    let journal_present = matches!(&journal, Ok(Some(_)));
    let journal_unreadable = journal.is_err();
    let mut runtime =
        if matches!(before[1], Ok(Some(stamp)) if stamp.marker()) && present(before[5]) {
            RuntimeEvidence::capture(directory, artifacts, &mut hook).ok()
        } else {
            None
        };
    let control = if present(before[7]) {
        ControlEvidence::capture(directory).ok()
    } else {
        None
    };
    hook(Phase::BeforeFinalCheck);
    let fresh_backups_valid = fresh_backups
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(|coverage| coverage.validate(directory).is_ok());
    let after = snapshot(directory);
    let reconciliation_after = entry(directory, OsStr::new("module-recovery-v1"));
    let runtime_valid = runtime
        .as_mut()
        .is_some_and(|runtime| runtime.validate(directory).is_ok());
    let control_valid = control
        .as_ref()
        .is_some_and(|control| control.validate(directory).is_ok());
    let root_stable = Stamp::of(directory).is_ok_and(|stamp| stamp == root_before);
    let pristine = before[1] == after[1] && matches!(before[1], Ok(Some(stamp)) if stamp.marker());
    let mut reasons = Vec::new();
    // A pending/revoked migration namespace cannot disappear merely because
    // its original locks were removed. Genuine published physical recovery
    // may supersede it only by covering both exact original lock slots.
    let locks_reconciled = reconciliation.as_ref().is_ok_and(|classification| {
        classification.covers_original_slot("call-mode-operation.lock")
            && classification.covers_original_slot(".module-adb-interface.lock")
    });
    if !locks_reconciled {
        if legacy_locks.is_err() || legacy_namespace.is_err() {
            add(&mut reasons, Reason::UnreadableEvidence);
        }
        if present(legacy_namespace) && !legacy_locks.as_ref().is_ok_and(|covered| *covered) {
            add(&mut reasons, Reason::LegacyLockProvenancePending);
        }
    }
    match &reconciliation {
        Ok(crate::startup_recovery::reconcile::ledger::RecoveryClassification::Absent) => {}
        Ok(crate::startup_recovery::reconcile::ledger::RecoveryClassification::Pending) => {
            add(&mut reasons, Reason::ReconciliationPending);
        }
        Ok(
            crate::startup_recovery::reconcile::ledger::RecoveryClassification::PublishedCoverage(
                _,
            ),
        ) => {}
        Err(_) => add(&mut reasons, Reason::ReconciliationUnreadable),
    }
    if fresh_backups.is_err()
        || (fresh_backups
            .as_ref()
            .is_ok_and(|coverage| coverage.is_some())
            && !fresh_backups_valid)
    {
        add(&mut reasons, Reason::ReconciliationUnreadable);
    }
    if reconciliation_before.is_err()
        || reconciliation_after.is_err()
        || reconciliation_before != reconciliation_after
    {
        add(&mut reasons, Reason::ReconciliationUnreadable);
    }
    if present(before[0]) || present(after[0]) {
        add(&mut reasons, Reason::LegacyExecutionPending);
    }
    if before[0].is_err() || after[0].is_err() {
        add(&mut reasons, Reason::UnreadableEvidence);
    }
    if present(before[6]) || present(journal_entry) || present(after[6]) || journal_present {
        add(&mut reasons, Reason::RestoreJournalPending);
    }
    if journal_unreadable
        || before[6].is_err()
        || journal_entry.is_err()
        || after[6].is_err()
        || before[6] != after[6]
    {
        add(&mut reasons, Reason::RestoreJournalUnreadable);
    }
    if [before[1], after[1]]
        .into_iter()
        .any(|value| matches!(value, Ok(Some(stamp)) if !stamp.marker()))
    {
        add(&mut reasons, Reason::InvalidPristineMarker);
    }
    if before[1].is_err() || after[1].is_err() {
        add(&mut reasons, Reason::UnreadableEvidence);
    }
    for (index, reason) in [
        (2, Reason::LegacyCallModeLock),
        (3, Reason::LegacyAdbInterfaceLock),
        (4, Reason::ModuleBackupPresent),
        (5, Reason::LegacyVoiceRuntime),
        // Even an empty or malformed control store may have an orphaned
        // remote supervisor. Startup must never infer a closed child from it.
        (7, Reason::ManagedVoiceControlPending),
    ] {
        let exists = present(before[index]) || present(after[index]);
        // Each disposition belongs to the exact validated slot, including all
        // original inode/byte identities and any authenticated normal-origin
        // extension. A reason count or enum-wide whitelist cannot waive a new
        // artifact. Unreadable evidence and an active journal remain pending.
        let covered = before[index] == after[index]
            && reconciliation
                .as_ref()
                .is_ok_and(|classification| classification.covers_original_slot(NAMES[index]));
        let allowed = covered
            || (matches!(index, 2 | 3)
                && before[index] == after[index]
                && legacy_locks.as_ref().is_ok_and(|covered| *covered)
                && super::legacy_locks::covered(directory).unwrap_or(false))
            || (index == 4 && before[4] == after[4] && fresh_backups_valid)
            || (pristine
                && before[index] == after[index]
                && match index {
                    2 | 3 => matches!(before[index], Ok(Some(stamp)) if stamp.safe_empty_lock()),
                    5 => runtime_valid,
                    7 => control_valid,
                    _ => false,
                });
        if exists && !allowed {
            add(&mut reasons, reason);
        }
        if before[index].is_err() || after[index].is_err() {
            add(&mut reasons, Reason::UnreadableEvidence);
        }
    }
    if !root_stable || before != after {
        add(&mut reasons, Reason::UnreadableEvidence);
    }
    reasons
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "startup evidence changed or has an unsafe shape",
    )
}

fn single_name(name: &OsStr) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn open_child(parent: &File, name: &OsStr, is_directory: bool) -> io::Result<(File, Stamp)> {
    if !single_name(name) {
        return Err(invalid());
    }
    let named = entry(parent, name)
        .map_err(|()| invalid())?
        .ok_or_else(invalid)?;
    if if is_directory {
        !named.directory()
    } else {
        !named.regular() || named.links != 1
    } {
        return Err(invalid());
    }
    let flags = OFlag::O_RDONLY
        | OFlag::O_NOFOLLOW
        | OFlag::O_CLOEXEC
        | OFlag::O_NONBLOCK
        | if is_directory {
            OFlag::O_DIRECTORY
        } else {
            OFlag::empty()
        };
    let file = File::from(openat(parent, name, flags, Mode::empty())?);
    let held = Stamp::of(&file)?;
    if held != named || !same_name(parent, name, held) {
        return Err(invalid());
    }
    Ok((file, held))
}

fn same_name(parent: &File, name: &OsStr, expected: Stamp) -> bool {
    entry(parent, name) == Ok(Some(expected))
}

fn names(directory: &File, expected: &BTreeSet<OsString>) -> io::Result<()> {
    let limit = expected.len().checked_add(1).ok_or_else(invalid)?;
    let entries = proletarius_platform_fs::directory_names(directory, limit)?;
    let count = entries.len();
    let actual: BTreeSet<_> = entries.into_iter().collect();
    if count != actual.len() || &actual != expected {
        return Err(invalid());
    }
    Ok(())
}

struct ArtifactEvidence {
    name: OsString,
    file: File,
    stamp: Stamp,
}

// This read-only bounded snapshot also verifies rolling-archive namespace
// bindings and exact residual-original identities. It never grants recovery
// authority or exempts any other startup evidence slot.
type ControlEvidence = crate::call_mode_adb::ControlStoreEvidence;

struct RuntimeEvidence {
    runtime: File,
    runtime_stamp: Stamp,
    version: File,
    version_stamp: Stamp,
    version_name: OsString,
    runtime_names: BTreeSet<OsString>,
    artifact_names: BTreeSet<OsString>,
    download_lock: Entry,
    artifacts: Vec<ArtifactEvidence>,
}
impl RuntimeEvidence {
    fn capture(
        root: &File,
        artifacts: &[Artifact],
        hook: &mut impl FnMut(Phase),
    ) -> io::Result<Self> {
        if artifacts.is_empty() {
            return Err(invalid());
        }
        let version_name = OsString::from(&manifest().version);
        let (runtime, runtime_stamp) = open_child(root, OsStr::new(RUNTIME), true)?;
        let (version, version_stamp) = open_child(&runtime, &version_name, true)?;
        hook(Phase::RuntimePinned);
        let download_lock = entry(&runtime, OsStr::new(DOWNLOAD_LOCK));
        let mut runtime_names = BTreeSet::from([version_name.clone()]);
        match download_lock {
            Ok(Some(stamp)) if stamp.safe_empty_lock() => {
                runtime_names.insert(OsString::from(DOWNLOAD_LOCK));
            }
            Ok(None) => {}
            _ => return Err(invalid()),
        }
        let artifact_names: BTreeSet<_> = artifacts
            .iter()
            .map(|artifact| OsString::from(&artifact.name))
            .collect();
        if artifact_names.len() != artifacts.len()
            || artifacts.iter().any(|artifact| {
                !single_name(OsStr::new(&artifact.name))
                    || artifact.size == 0
                    || artifact.size > MAX_ARTIFACT_BYTES
            })
        {
            return Err(invalid());
        }
        names(&runtime, &runtime_names)?;
        names(&version, &artifact_names)?;
        hook(Phase::NamesCaptured);
        let mut verified = Vec::with_capacity(artifacts.len());
        for (index, artifact) in artifacts.iter().enumerate() {
            let name = OsString::from(&artifact.name);
            let (mut file, stamp) = open_child(&version, &name, false)?;
            if !stamp.artifact(artifact) {
                return Err(invalid());
            }
            hook(Phase::ArtifactPinned(index));
            // Check again after any interleaving and before reading a byte.
            if Stamp::of(&file)? != stamp || !same_name(&version, &name, stamp) {
                return Err(invalid());
            }
            let mut payload = Vec::with_capacity(artifact.size as usize);
            (&mut file)
                .take(artifact.size.checked_add(1).ok_or_else(invalid)?)
                .read_to_end(&mut payload)?;
            crate::call_mode_runtime::verify_artifact_bytes(artifact, &payload)
                .map_err(|_| invalid())?;
            hook(Phase::ArtifactRead(index));
            if Stamp::of(&file)? != stamp || !same_name(&version, &name, stamp) {
                return Err(invalid());
            }
            verified.push(ArtifactEvidence { name, file, stamp });
        }
        let value = Self {
            runtime,
            runtime_stamp,
            version,
            version_stamp,
            version_name,
            runtime_names,
            artifact_names,
            download_lock,
            artifacts: verified,
        };
        value.validate(root)?;
        Ok(value)
    }
    fn validate(&self, root: &File) -> io::Result<()> {
        if Stamp::of(&self.runtime)? != self.runtime_stamp
            || Stamp::of(&self.version)? != self.version_stamp
            || !same_name(root, OsStr::new(RUNTIME), self.runtime_stamp)
            || !same_name(&self.runtime, &self.version_name, self.version_stamp)
            || entry(&self.runtime, OsStr::new(DOWNLOAD_LOCK)) != self.download_lock
        {
            return Err(invalid());
        }
        names(&self.runtime, &self.runtime_names)?;
        names(&self.version, &self.artifact_names)?;
        for artifact in &self.artifacts {
            if Stamp::of(&artifact.file)? != artifact.stamp
                || !same_name(&self.version, &artifact.name, artifact.stamp)
            {
                return Err(invalid());
            }
        }
        // Directory enumeration itself is an observation interval too.
        if Stamp::of(&self.runtime)? != self.runtime_stamp
            || Stamp::of(&self.version)? != self.version_stamp
            || !same_name(root, OsStr::new(RUNTIME), self.runtime_stamp)
            || !same_name(&self.runtime, &self.version_name, self.version_stamp)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
