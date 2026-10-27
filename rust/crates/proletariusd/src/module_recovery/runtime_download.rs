//! Provenance for newly created, data-only runtime download directories.
//! This never grants device I/O or settles a physical recovery obligation.
use super::Gate;
use crate::call_mode_runtime_manifest::{Artifact, RuntimeManifest, manifest};
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{Mode, fstatat, mkdirat};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const DIRECTORY: &str = "runtime-download-lineage-v1";
const RECEIPT: &str = "source.json";
const COMMITTED: &str = "committed";
const REVOKED: &str = "revoked";
const STAGED: &str = "runtime";
const FILES: &str = "files";
const RUNTIME: &str = "voice-runtime";
const PRISTINE: &str = ".call-mode-pristine-v1";
const LOCKS: [&str; 2] = ["call-mode-operation.lock", ".module-adb-interface.lock"];
const ABSENT: [&str; 6] = [
    super::LEGACY_EXECUTION_PENDING,
    "module-backups",
    "voice-control",
    "module-recovery-v1",
    crate::call_mode_restore_journal::ACTIVE_NAME,
    crate::call_mode_restore_journal::ARCHIVE_NAME,
];

fn invalid() -> io::Error {
    io::Error::other("runtime download provenance unavailable or changed")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: i64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
fn stamp(parent: &File, name: &str) -> io::Result<Option<Stamp>> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(s) => Ok(Some(Stamp {
            device: s.st_dev as u64,
            inode: s.st_ino as u64,
            mode: s.st_mode as u32,
            uid: s.st_uid,
            gid: s.st_gid,
            links: s.st_nlink as u64,
            size: s.st_size,
            mtime: (s.st_mtime, s.st_mtime_nsec),
            ctime: (s.st_ctime, s.st_ctime_nsec),
        })),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn held(file: &File) -> io::Result<Stamp> {
    stamp(file, ".")?.ok_or_else(invalid)
}
fn regular(s: &Stamp) -> bool {
    s.mode & nix::libc::S_IFMT as u32 == nix::libc::S_IFREG as u32
        && s.links == 1
        && s.uid == nix::unistd::geteuid().as_raw()
        && s.mode & 0o7022 == 0
}
fn empty(parent: &File, name: &str) -> io::Result<Stamp> {
    let s = stamp(parent, name)?.ok_or_else(invalid)?;
    if !regular(&s) || s.size != 0 || s.mode & 0o7133 != 0 {
        return Err(invalid());
    }
    Ok(s)
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
}
fn directory_identity(file: &File) -> io::Result<DirectoryIdentity> {
    let m = file.metadata()?;
    if !m.is_dir() || m.uid() != nix::unistd::geteuid().as_raw() {
        return Err(invalid());
    }
    Ok(DirectoryIdentity {
        device: m.dev(),
        inode: m.ino(),
        mode: m.mode(),
        uid: m.uid(),
    })
}
fn open_dir(parent: &File, name: &str) -> io::Result<File> {
    let before = stamp(parent, name)?.ok_or_else(invalid)?;
    if before.mode & nix::libc::S_IFMT as u32 != nix::libc::S_IFDIR as u32
        || before.mode & 0o7777 != 0o700
        || before.uid != nix::unistd::geteuid().as_raw()
    {
        return Err(invalid());
    }
    let file = File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    if held(&file)? != before || stamp(parent, name)? != Some(before) {
        return Err(invalid());
    }
    Ok(file)
}
fn create_dir(parent: &File, name: &str) -> io::Result<File> {
    match mkdirat(parent, name, Mode::from_bits_truncate(0o700)) {
        Ok(()) => parent.sync_all()?,
        Err(nix::errno::Errno::EEXIST) => {}
        Err(error) => return Err(error.into()),
    }
    open_dir(parent, name)
}
fn write_new(parent: &File, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::from(openat(
        parent,
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    file.write_all(bytes)?;
    file.sync_all()?;
    parent.sync_all()
}
fn names(file: &File, permitted: &[&str]) -> io::Result<()> {
    let before = held(file)?;
    for name in proletarius_platform_fs::directory_names(file, permitted.len())? {
        if !permitted.iter().any(|p| name == *p) {
            return Err(invalid());
        }
    }
    if held(file)? != before {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    root: DirectoryIdentity,
    locks: [Stamp; 2],
    pristine: Option<Stamp>,
    legacy: bool,
    legacy_namespace: Option<DirectoryIdentity>,
    legacy_receipt: Option<Stamp>,
    legacy_commit: Option<Stamp>,
}
fn source(root: &File, allow_runtime: bool) -> io::Result<Source> {
    for name in ABSENT {
        if stamp(root, name)?.is_some() {
            return Err(invalid());
        }
    }
    if !allow_runtime && stamp(root, RUNTIME)?.is_some() {
        return Err(invalid());
    }
    let pristine = stamp(root, PRISTINE)?;
    let legacy = if pristine.is_some() {
        empty(root, PRISTINE)?;
        // A pristine marker cannot hide revoked/damaged legacy provenance.
        if stamp(root, super::legacy_locks::DIRECTORY)?.is_some() {
            return Err(invalid());
        }
        false
    } else {
        if !super::legacy_locks::covered_download_source(root, allow_runtime)? {
            return Err(invalid());
        }
        true
    };
    let (legacy_namespace, legacy_receipt, legacy_commit) = if legacy {
        let store = open_dir(root, super::legacy_locks::DIRECTORY)?;
        (
            Some(directory_identity(&store)?),
            Some(stamp(&store, "provenance.json")?.ok_or_else(invalid)?),
            Some(empty(&store, "committed")?),
        )
    } else {
        (None, None, None)
    };
    Ok(Source {
        root: directory_identity(root)?,
        locks: [empty(root, LOCKS[0])?, empty(root, LOCKS[1])?],
        pristine,
        legacy,
        legacy_namespace,
        legacy_receipt,
        legacy_commit,
    })
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactIdentity {
    name: String,
    size: u64,
    sha256: String,
    mode: u32,
}
fn artifacts(files: &[Artifact]) -> Vec<ArtifactIdentity> {
    files
        .iter()
        .map(|a| ArtifactIdentity {
            name: a.name.clone(),
            size: a.size,
            sha256: a.sha256.clone(),
            mode: a.mode,
        })
        .collect()
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    policy: String,
    source: Source,
    runtime: DirectoryIdentity,
    version: DirectoryIdentity,
    version_name: String,
    artifacts: Vec<ArtifactIdentity>,
    download_lock: Stamp,
    files: DirectoryIdentity,
}
fn read_receipt(store: &File) -> io::Result<Receipt> {
    empty(store, COMMITTED)?;
    let before = stamp(store, RECEIPT)?.ok_or_else(invalid)?;
    if !regular(&before) || before.mode & 0o7777 != 0o600 || !(1..=16384).contains(&before.size) {
        return Err(invalid());
    }
    let file = File::from(openat(
        store,
        RECEIPT,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let m = file.metadata()?;
    if m.dev() != before.device || m.ino() != before.inode {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 || stamp(store, RECEIPT)? != Some(before) {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
fn verify_artifacts(version: &File, files: &[Artifact]) -> io::Result<()> {
    let before = held(version)?;
    for entry in proletarius_platform_fs::directory_names(version, files.len())? {
        let artifact = files
            .iter()
            .find(|a| entry == a.name.as_str())
            .ok_or_else(invalid)?;
        let original = stamp(version, &artifact.name)?.ok_or_else(invalid)?;
        if !regular(&original)
            || original.mode & 0o7777 != artifact.mode
            || original.size != artifact.size as i64
        {
            return Err(invalid());
        }
        let file = File::from(openat(
            version,
            artifact.name.as_str(),
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        let m = file.metadata()?;
        if m.dev() != original.device || m.ino() != original.inode {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(artifact.size + 1).read_to_end(&mut bytes)?;
        crate::call_mode_runtime::verify_artifact_bytes(artifact, &bytes).map_err(|_| invalid())?;
        if stamp(version, &artifact.name)? != Some(original) {
            return Err(invalid());
        }
    }
    if held(version)? != before {
        return Err(invalid());
    }
    Ok(())
}
fn verify(
    root: &File,
    store: &File,
    receipt: &Receipt,
    runtime_manifest: &RuntimeManifest,
) -> io::Result<(File, File, bool)> {
    if source(root, true)? != receipt.source || stamp(store, REVOKED)?.is_some() {
        return Err(invalid());
    }
    verify_layout(root, store, receipt, runtime_manifest)
}
fn verify_source_binding(root: &File, source: &Source) -> io::Result<()> {
    if directory_identity(root)? != source.root
        || [empty(root, LOCKS[0])?, empty(root, LOCKS[1])?] != source.locks
    {
        return Err(invalid());
    }
    if source.legacy {
        let legacy = open_dir(root, super::legacy_locks::DIRECTORY)?;
        names(&legacy, &["provenance.json", "committed", "revoked"])?;
        if stamp(&legacy, "revoked")?.is_some() {
            empty(&legacy, "revoked")?;
        }
        if Some(directory_identity(&legacy)?) != source.legacy_namespace
            || stamp(&legacy, "provenance.json")? != source.legacy_receipt
            || Some(empty(&legacy, "committed")?) != source.legacy_commit
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn verify_layout(
    root: &File,
    store: &File,
    receipt: &Receipt,
    runtime_manifest: &RuntimeManifest,
) -> io::Result<(File, File, bool)> {
    names(store, &[RECEIPT, COMMITTED, STAGED, FILES, REVOKED])?;
    if receipt.policy != "controlled-runtime-download-v1"
        || receipt.version_name != runtime_manifest.version
        || receipt.artifacts != artifacts(&runtime_manifest.files)
    {
        return Err(invalid());
    }
    let published = stamp(root, RUNTIME)?.is_some();
    if published && stamp(store, STAGED)?.is_some() {
        return Err(invalid());
    }
    let runtime = if published {
        open_dir(root, RUNTIME)?
    } else {
        open_dir(store, STAGED)?
    };
    let version = open_dir(&runtime, &receipt.version_name)?;
    let files = open_dir(store, FILES)?;
    if directory_identity(&runtime)? != receipt.runtime
        || directory_identity(&version)? != receipt.version
        || directory_identity(&files)? != receipt.files
        || empty(&runtime, ".download.lock")? != receipt.download_lock
    {
        return Err(invalid());
    }
    names(&runtime, &[&receipt.version_name, ".download.lock"])?;
    verify_artifacts(&version, &runtime_manifest.files)?;
    // Scratch bytes never appear in the runtime tree or establish permission.
    validate_scratch(&files, &runtime_manifest.files, false)?;
    Ok((runtime, files, published))
}
fn validate_scratch(directory: &File, artifacts: &[Artifact], remove: bool) -> io::Result<()> {
    let entries = proletarius_platform_fs::directory_names(directory, 32)?;
    for entry in entries {
        let name = entry.to_str().ok_or_else(invalid)?;
        let artifact = artifacts
            .iter()
            .find(|a| {
                name.strip_prefix(&format!(".{}-", a.name))
                    .is_some_and(|tail| {
                        let parts: Vec<_> = tail.split('-').collect();
                        parts.len() == 2
                            && parts[0].len() == 32
                            && parts[0]
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                            && parts[1].strip_suffix(".part").is_some_and(|suffix| {
                                suffix.len() == 16
                                    && suffix
                                        .bytes()
                                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                            })
                    })
            })
            .ok_or_else(invalid)?;
        let metadata = stamp(directory, name)?.ok_or_else(invalid)?;
        if !regular(&metadata)
            || metadata.size < 0
            || metadata.size > artifact.size as i64
            || !matches!(metadata.mode & 0o7777, 0o600 | 0o644 | 0o755)
        {
            return Err(invalid());
        }
        if remove {
            if stamp(directory, name)? != Some(metadata) {
                return Err(invalid());
            }
            nix::unistd::unlinkat(directory, name, nix::unistd::UnlinkatFlags::NoRemoveDir)?;
        }
    }
    if remove {
        directory.sync_all()?;
    }
    Ok(())
}

/// Read-only classification. Only the runtime slot and the exact pre-existing
/// legacy locks can be covered; other startup evidence retains its own policy.
pub(super) fn coverage(root: &File, artifacts: &[Artifact]) -> io::Result<Option<bool>> {
    let Some(before) = stamp(root, DIRECTORY)? else {
        return Ok(None);
    };
    let store = open_dir(root, DIRECTORY)?;
    if stamp(&store, COMMITTED)?.is_none() && stamp(root, RUNTIME)?.is_none() {
        // Interrupted preparation has published no executable namespace. It
        // may be retried only after the writer validates its original clean source.
        return Ok(None);
    }
    let receipt = read_receipt(&store)?;
    // Physical intent invalidates data-only lineage. Leave those slots to the
    // existing recovery classifier, including a genuine published reconciliation.
    if stamp(&store, REVOKED)?.is_some() {
        empty(&store, REVOKED)?;
        if receipt.source.pristine.is_some() && stamp(root, PRISTINE)?.is_some() {
            return Err(invalid());
        }
        return Ok(None);
    }
    if stamp(root, PRISTINE)? != receipt.source.pristine
        || ABSENT
            .iter()
            .any(|name| !matches!(stamp(root, name), Ok(None)))
        || (receipt.source.legacy && {
            let legacy = open_dir(root, super::legacy_locks::DIRECTORY)?;
            stamp(&legacy, "revoked")?.is_some()
        })
    {
        return Ok(None);
    }
    let current_manifest = RuntimeManifest {
        commit: manifest().commit.clone(),
        kernel: manifest().kernel.clone(),
        version: manifest().version.clone(),
        files: artifacts.to_vec(),
    };
    let (_, _, published) = verify(root, &store, &receipt, &current_manifest)?;
    if stamp(root, DIRECTORY)? != Some(before) {
        return Err(invalid());
    }
    Ok(published.then_some(receipt.source.legacy))
}

/// A durable physical-intent tombstone cannot disappear with the runtime or
/// original lock names. Only the existing sealed reconciliation may cover it.
pub(super) fn pending_intent(root: &File) -> io::Result<bool> {
    let Some(before) = stamp(root, DIRECTORY)? else {
        return Ok(false);
    };
    let store = open_dir(root, DIRECTORY)?;
    if stamp(&store, REVOKED)?.is_none() {
        return Ok(false);
    }
    empty(&store, REVOKED)?;
    let receipt = read_receipt(&store)?;
    if directory_identity(root)? != receipt.source.root || stamp(root, DIRECTORY)? != Some(before) {
        return Err(invalid());
    }
    Ok(true)
}

pub(crate) struct Directories {
    pub(crate) runtime: File,
    pub(crate) staging: File,
}

/// Called by the actual download worker before any network operation. Existing
/// unknown runtimes are never adopted. Inapplicable physical-history roots keep
/// their existing writer behavior without gaining a provenance receipt.
pub(crate) fn begin(root: &Path, gate: &Gate) -> io::Result<Option<Directories>> {
    if gate.is_admission_revoked() {
        return Err(invalid());
    }
    let proof = gate.root_proof_for(root).map_err(|_| invalid())?;
    proof
        .with_root(root, |directory| begin_in(directory, manifest(), true))
        .map_err(|_| invalid())?
}
fn begin_in(
    root: &File,
    runtime_manifest: &RuntimeManifest,
    admitted_retry: bool,
) -> io::Result<Option<Directories>> {
    let existing = stamp(root, DIRECTORY)?.is_some();
    if existing {
        let store = open_dir(root, DIRECTORY)?;
        if stamp(&store, COMMITTED)?.is_some() {
            let receipt = read_receipt(&store)?;
            if stamp(&store, REVOKED)?.is_some() {
                empty(&store, REVOKED)?;
                if !admitted_retry {
                    return Err(invalid());
                }
                verify_source_binding(root, &receipt.source)?;
                if receipt.source.pristine.is_some() && stamp(root, PRISTINE)?.is_some() {
                    return Err(invalid());
                }
                if receipt.source.legacy {
                    let legacy = open_dir(root, super::legacy_locks::DIRECTORY)?;
                    empty(&legacy, "revoked")?;
                }
                let (_, _, published) = verify_layout(root, &store, &receipt, runtime_manifest)?;
                if !published {
                    return Err(invalid());
                }
                // The original live Gate already authorized this ordinary job.
                // Retired provenance grants nothing and never becomes active again.
                return Ok(None);
            }
            let (runtime, files, published) = verify(root, &store, &receipt, runtime_manifest)?;
            if !published {
                proletarius_platform_fs::rename_directory_noreplace(&store, STAGED, root, RUNTIME)?;
                store.sync_all()?;
                root.sync_all()?;
            }
            validate_scratch(&files, &runtime_manifest.files, true)?;
            verify(root, &store, &receipt, runtime_manifest)?;
            return Ok(Some(Directories {
                runtime,
                staging: files,
            }));
        }
    }
    let captured = match source(root, false) {
        Ok(source) => source,
        Err(_) if !existing => return Ok(None),
        Err(error) => return Err(error),
    };
    let store = create_dir(root, DIRECTORY)?;
    names(&store, &[RECEIPT, STAGED, FILES])?;
    let runtime = create_dir(&store, STAGED)?;
    names(&runtime, &[&runtime_manifest.version, ".download.lock"])?;
    let version = create_dir(&runtime, &runtime_manifest.version)?;
    names(&version, &[])?;
    if stamp(&runtime, ".download.lock")?.is_none() {
        write_new(&runtime, ".download.lock", &[])?;
    }
    let files = create_dir(&store, FILES)?;
    names(&files, &[])?;
    if source(root, false)? != captured {
        return Err(invalid());
    }
    let receipt = Receipt {
        policy: "controlled-runtime-download-v1".into(),
        source: captured,
        runtime: directory_identity(&runtime)?,
        version: directory_identity(&version)?,
        version_name: runtime_manifest.version.clone(),
        artifacts: artifacts(&runtime_manifest.files),
        download_lock: empty(&runtime, ".download.lock")?,
        files: directory_identity(&files)?,
    };
    // A crashed source.json write is recoverable only before COMMITTED and
    // before runtime publication, while the original data-only source is clean.
    if let Some(partial) = stamp(&store, RECEIPT)? {
        if !regular(&partial)
            || partial.mode & 0o7777 != 0o600
            || !(0..=16384).contains(&partial.size)
        {
            return Err(invalid());
        }
        nix::unistd::unlinkat(&store, RECEIPT, nix::unistd::UnlinkatFlags::NoRemoveDir)?;
        store.sync_all()?;
    }
    write_new(
        &store,
        RECEIPT,
        &serde_json::to_vec(&receipt).map_err(|_| invalid())?,
    )?;
    runtime.sync_all()?;
    version.sync_all()?;
    files.sync_all()?;
    write_new(&store, COMMITTED, &[])?;
    verify(root, &store, &receipt, runtime_manifest)?;
    proletarius_platform_fs::rename_directory_noreplace(&store, STAGED, root, RUNTIME)?;
    store.sync_all()?;
    root.sync_all()?;
    verify(root, &store, &receipt, runtime_manifest)?;
    Ok(Some(Directories {
        runtime,
        staging: files,
    }))
}

/// Record actual physical intent before the pristine/legacy markers are revoked.
/// This tombstone cannot certify cleanup or restore ordinary startup admission.
pub(crate) fn invalidate(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let root = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(root)?;
    if stamp(&root, DIRECTORY)?.is_none() {
        return Ok(());
    }
    let store = open_dir(&root, DIRECTORY)?;
    if stamp(&store, COMMITTED)?.is_none() {
        // No runtime was published by an unfinished preparation transaction.
        return Ok(());
    }
    let receipt = read_receipt(&store)?;
    verify_source_binding(&root, &receipt.source)?;
    names(&store, &[RECEIPT, COMMITTED, STAGED, FILES, REVOKED])?;
    if stamp(&store, REVOKED)?.is_some() {
        empty(&store, REVOKED)?;
        return store.sync_all();
    }
    write_new(&store, REVOKED, &[])
}

#[cfg(test)]
pub(crate) fn begin_fixture(
    root: &Path,
    gate: &Gate,
    runtime: &RuntimeManifest,
) -> io::Result<Option<Directories>> {
    if gate.is_admission_revoked() {
        return Err(invalid());
    }
    gate.root_proof_for(root)
        .map_err(|_| invalid())?
        .with_root(root, |directory| begin_in(directory, runtime, true))
        .map_err(|_| invalid())?
}

#[cfg(test)]
mod tests;
