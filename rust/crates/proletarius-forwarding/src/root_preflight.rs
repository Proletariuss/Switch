//! Read-only consistency inspection. Never creates identities/witnesses, fsyncs
//! user data, migrates a queue, or yields an admission/ownership capability.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, Metadata, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
pub const ROOT_FILE: &str = ".switch-root-identity-v1.json";
pub const SOURCE_FILE: &str = ".switch-source-identity-v1.json";
pub const WITNESS_FILE: &str = ".switch-fresh-root-witness-v1.json";
const ARCHIVES: [&str; 3] = [
    "sms-forwarding/state.enc",
    "sms-forwarding-admission.json",
    "multistandby/store-v1.json",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightError {
    MissingEvidence,
    InvalidSchema,
    InvalidBinding,
    DigestMismatch,
    UnsafePath,
    UnsafePermissions,
    ChangedDuringRead,
    TooLarge,
    Io,
}
impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "root preflight {self:?}")
    }
}
impl std::error::Error for PreflightError {}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootIdentity {
    pub schema_version: u16,
    pub root_id: String,
    pub creation_nonce: String,
    pub source_identity_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceIdentity {
    pub schema_version: u16,
    pub root_id: String,
    pub source_id: String,
    pub creation_nonce: String,
    pub store_relative_path: String,
    pub contract: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FreshWitness {
    pub schema_version: u16,
    pub contract: String,
    pub phase: String,
    pub root_id: String,
    pub source_id: String,
    pub creation_nonce: String,
    pub root_identity_sha256: String,
    pub source_identity_sha256: String,
    pub inspected_archive_paths: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct ArchivePresence {
    pub relative_path: String,
    pub present: bool,
}
#[derive(Debug, Serialize)]
pub struct PreflightReport {
    pub root_id: String,
    pub source_id: String,
    pub root_identity_sha256: String,
    pub source_identity_sha256: String,
    pub witness_sha256: String,
    pub archive_presence: Vec<ArchivePresence>,
    pub fresh_intent_consistent: bool,
    pub production_authorized: bool,
    pub owner_drained: bool,
    pub creation_order_durability_proven: bool,
    pub hostile_namespace_safety_proven: bool,
    pub unresolved_preconditions: Vec<&'static str>,
}
struct Snapshot {
    path: PathBuf,
    metadata: Metadata,
    bytes: Vec<u8>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn root_metadata(root: &Path) -> Result<Metadata, PreflightError> {
    if !root.is_absolute()
        || fs::canonicalize(root).map_err(|_| PreflightError::UnsafePath)? != root
    {
        return Err(PreflightError::UnsafePath);
    }
    let m = fs::symlink_metadata(root).map_err(|_| PreflightError::UnsafePath)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(PreflightError::UnsafePath);
    }
    if m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o022 != 0 {
        return Err(PreflightError::UnsafePermissions);
    }
    Ok(m)
}
fn read_snapshot(path: PathBuf) -> Result<Snapshot, PreflightError> {
    let named = fs::symlink_metadata(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PreflightError::MissingEvidence
        } else {
            PreflightError::Io
        }
    })?;
    if !named.is_file() || named.file_type().is_symlink() || named.nlink() != 1 {
        return Err(PreflightError::UnsafePath);
    }
    if named.uid() != nix::unistd::geteuid().as_raw() || named.mode() & 0o077 != 0 {
        return Err(PreflightError::UnsafePermissions);
    }
    if named.len() > 4096 {
        return Err(PreflightError::TooLarge);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| PreflightError::Io)?;
    if !same(&named, &file.metadata().map_err(|_| PreflightError::Io)?) {
        return Err(PreflightError::ChangedDuringRead);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| PreflightError::Io)?;
    if bytes.len() > 4096 {
        return Err(PreflightError::TooLarge);
    }
    Ok(Snapshot {
        path,
        metadata: named,
        bytes,
    })
}
fn decode<T: serde::de::DeserializeOwned>(s: &Snapshot) -> Result<T, PreflightError> {
    crate::format_v2::strict_json(&s.bytes).map_err(|_| PreflightError::InvalidSchema)?;
    serde_json::from_slice(&s.bytes).map_err(|_| PreflightError::InvalidSchema)
}
fn recheck(s: &Snapshot) -> Result<(), PreflightError> {
    let again = read_snapshot(s.path.clone())?;
    if !same(&s.metadata, &again.metadata) || s.bytes != again.bytes {
        return Err(PreflightError::ChangedDuringRead);
    }
    Ok(())
}
fn archive_present(root: &Path, relative: &str) -> Result<bool, PreflightError> {
    let mut path = root.to_owned();
    for part in Path::new(relative).components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => return Err(PreflightError::UnsafePath),
            Ok(m)
                if m.is_dir()
                    && (m.uid() != nix::unistd::geteuid().as_raw() || m.mode() & 0o022 != 0) =>
            {
                return Err(PreflightError::UnsafePermissions);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(PreflightError::Io),
        }
    }
    Ok(true)
}
/// Existing data is reported as non-fresh. Consistent local marker bytes never
/// prove the creator's original order/fsync or that an older root was not lost.
pub fn inspect(root: &Path) -> Result<PreflightReport, PreflightError> {
    inspect_with_hook(root, || {})
}
pub fn inspect_expected(
    root: &Path,
    expected_root_id: &str,
    expected_source_id: &str,
) -> Result<PreflightReport, PreflightError> {
    let report = inspect(root)?;
    if report.root_id != expected_root_id || report.source_id != expected_source_id {
        return Err(PreflightError::InvalidBinding);
    }
    Ok(report)
}
fn inspect_with_hook(root: &Path, hook: impl FnOnce()) -> Result<PreflightReport, PreflightError> {
    let root_before = root_metadata(root)?;
    let r = read_snapshot(root.join(ROOT_FILE))?;
    let s = read_snapshot(root.join(SOURCE_FILE))?;
    let w = read_snapshot(root.join(WITNESS_FILE))?;
    let root_id: RootIdentity = decode(&r)?;
    let source: SourceIdentity = decode(&s)?;
    let witness: FreshWitness = decode(&w)?;
    if root_id.schema_version != 1
        || source.schema_version != 1
        || witness.schema_version != 1
        || source.contract != "nwait-source-identity-v1"
        || source.store_relative_path != "multistandby/store-v1.json"
        || witness.contract != "switch-fresh-root-intent-v1"
        || witness.phase != "fresh_intent"
        || witness.inspected_archive_paths != ARCHIVES
    {
        return Err(PreflightError::InvalidSchema);
    }
    if !valid_hex(&root_id.root_id, 32)
        || !valid_hex(&source.source_id, 32)
        || !valid_hex(&root_id.creation_nonce, 32)
        || root_id.root_id != source.root_id
        || root_id.root_id != witness.root_id
        || source.source_id != witness.source_id
        || root_id.creation_nonce != source.creation_nonce
        || root_id.creation_nonce != witness.creation_nonce
    {
        return Err(PreflightError::InvalidBinding);
    }
    if !valid_hex(&root_id.source_identity_sha256, 64)
        || root_id.source_identity_sha256 != hash(&s.bytes)
        || witness.source_identity_sha256 != hash(&s.bytes)
        || witness.root_identity_sha256 != hash(&r.bytes)
    {
        return Err(PreflightError::DigestMismatch);
    }
    let mut presence = Vec::new();
    for path in ARCHIVES {
        presence.push(ArchivePresence {
            relative_path: path.into(),
            present: archive_present(root, path)?,
        });
    }
    let mut extra = false;
    let mut entries = 0;
    for entry in fs::read_dir(root).map_err(|_| PreflightError::Io)? {
        entries += 1;
        if entries > 64 {
            return Err(PreflightError::TooLarge);
        }
        let entry = entry.map_err(|_| PreflightError::Io)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(PreflightError::UnsafePath)?;
        if ![
            ROOT_FILE,
            SOURCE_FILE,
            WITNESS_FILE,
            "sms-forwarding",
            "multistandby",
        ]
        .contains(&name)
        {
            extra = true;
        }
    }
    // Empty precreated known directories are allowed; any other content makes a
    // fresh claim unproven, without reading SMS bodies or provider credentials.
    let mut directory_snapshots = Vec::new();
    for directory in ["sms-forwarding", "multistandby"] {
        let path = root.join(directory);
        if path.exists() {
            let m = fs::symlink_metadata(&path).map_err(|_| PreflightError::Io)?;
            if !m.is_dir() || m.file_type().is_symlink() {
                return Err(PreflightError::UnsafePath);
            }
            directory_snapshots.push((path.clone(), m));
            if fs::read_dir(path)
                .map_err(|_| PreflightError::Io)?
                .next()
                .is_some()
            {
                extra = true;
            }
        }
    }
    hook();
    for (path, before) in directory_snapshots {
        let after = fs::symlink_metadata(path).map_err(|_| PreflightError::ChangedDuringRead)?;
        if !same(&before, &after) {
            return Err(PreflightError::ChangedDuringRead);
        }
    }
    for observed in &presence {
        if archive_present(root, &observed.relative_path)? != observed.present {
            return Err(PreflightError::ChangedDuringRead);
        }
    }
    recheck(&r)?;
    recheck(&s)?;
    recheck(&w)?;
    let root_after = root_metadata(root)?;
    if !same(&root_before, &root_after) {
        return Err(PreflightError::ChangedDuringRead);
    }
    Ok(PreflightReport {
        root_id: root_id.root_id,
        source_id: source.source_id,
        root_identity_sha256: hash(&r.bytes),
        source_identity_sha256: hash(&s.bytes),
        witness_sha256: hash(&w.bytes),
        fresh_intent_consistent: !extra && !presence.iter().any(|p| p.present),
        archive_presence: presence,
        production_authorized: false,
        owner_drained: false,
        creation_order_durability_proven: false,
        hostile_namespace_safety_proven: false,
        unresolved_preconditions: vec![
            "creator provenance and initial file/directory fsync order",
            "data-root/queue owner drain and migration lock",
            "supported old-reader/launcher downgrade boundary",
            "independent anti-rollback anchor or controlled restore protocol",
        ],
    })
}
#[cfg(test)]
mod tests;
